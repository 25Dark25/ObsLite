//! Audio capture (WASAPI) and a clock-driven mixer.
//!
//! Each input runs on its own thread, polling WASAPI in shared mode with automatic
//! conversion to 48 kHz stereo f32. Loopback capture delivers nothing while the
//! system is silent, so the mixer is driven by the recording clock and pads gaps
//! with silence; this keeps the audio track aligned with the video timestamps.

use std::collections::VecDeque;
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

use crate::encoder::Clock;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Upper bound on buffered samples per input, so idle inputs never grow unbounded.
const MAX_BUFFERED: usize = SAMPLE_RATE as usize * CHANNELS / 2; // 500 ms
/// Latency above which the mixer drops the oldest input samples.
const MAX_LATENCY: usize = SAMPLE_RATE as usize * CHANNELS / 10; // 100 ms

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputKind {
    System,
    Microphone,
}

impl InputKind {
    fn direction(self) -> Direction {
        match self {
            // Loopback capture opens the render (output) endpoint.
            Self::System => Direction::Render,
            Self::Microphone => Direction::Capture,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

/// Active endpoints for an input kind.
pub fn list_devices(kind: InputKind) -> Vec<Device> {
    let list = move || -> Result<Vec<Device>, Box<dyn Error>> {
        wasapi::initialize_mta().ok()?;
        let collection = DeviceEnumerator::new()?.get_device_collection(&kind.direction())?;
        let mut out = Vec::new();
        for device in &collection {
            let device = device?;
            out.push(Device { id: device.get_id()?, name: device.get_friendlyname()? });
        }
        Ok(out)
    };
    // COM is initialized per thread; keep it off the UI thread.
    thread::spawn(move || list().map_err(|e| e.to_string())).join().ok().and_then(Result::ok).unwrap_or_default()
}

/// State shared between an input's capture thread, the mixer and the UI.
pub struct InputShared {
    gain: AtomicU32,
    peak: AtomicU32,
    stop: AtomicBool,
    buffer: Mutex<VecDeque<f32>>,
    error: Mutex<Option<String>>,
}

impl InputShared {
    fn new(gain: f32) -> Self {
        Self {
            gain: AtomicU32::new(gain.to_bits()),
            peak: AtomicU32::new(0),
            stop: AtomicBool::new(false),
            buffer: Mutex::new(VecDeque::with_capacity(MAX_BUFFERED)),
            error: Mutex::new(None),
        }
    }

    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    /// Peak amplitude (0..1) of the most recent captured block, before gain.
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
}

pub struct Input {
    pub shared: Arc<InputShared>,
    thread: Option<JoinHandle<()>>,
}

impl Input {
    /// Starts capturing from `device_id`, or the Windows default endpoint when `None`.
    pub fn start(kind: InputKind, device_id: Option<String>, gain: f32) -> Self {
        let shared = Arc::new(InputShared::new(gain));
        let thread = thread::Builder::new()
            .name(format!("audio-{kind:?}"))
            .spawn({
                let shared = shared.clone();
                move || {
                    if let Err(e) = capture_loop(kind, device_id.as_deref(), &shared) {
                        *shared.error.lock().unwrap() = Some(e.to_string());
                    }
                    shared.peak.store(0, Ordering::Relaxed);
                }
            })
            .expect("spawn audio thread");
        Self { shared, thread: Some(thread) }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn capture_loop(kind: InputKind, device_id: Option<&str>, shared: &InputShared) -> Result<(), Box<dyn Error>> {
    wasapi::initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = match device_id {
        Some(id) => enumerator.get_device(id)?,
        None => enumerator.get_default_device(&kind.direction())?,
    };
    let mut client = device.get_iaudioclient()?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, CHANNELS, None);
    let mode = StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: 1_000_000 };
    client.initialize_client(&format, &Direction::Capture, &mode)?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;

    let mut raw: VecDeque<u8> = VecDeque::new();
    while !shared.stop.load(Ordering::Relaxed) {
        capture.read_from_device_to_deque(&mut raw)?;
        let usable = raw.len() / 4 * 4;
        if usable > 0 {
            let bytes: Vec<u8> = raw.drain(..usable).collect();
            let mut peak = 0f32;
            let mut buf = shared.buffer.lock().unwrap();
            for chunk in bytes.as_chunks::<4>().0 {
                let s = f32::from_le_bytes(*chunk);
                peak = peak.max(s.abs());
                buf.push_back(s);
            }
            let excess = buf.len().saturating_sub(MAX_BUFFERED);
            buf.drain(..excess);
            shared.peak.store(peak.to_bits(), Ordering::Relaxed);
        } else {
            shared.peak.store(0, Ordering::Relaxed);
        }
        thread::sleep(Duration::from_millis(5));
    }
    client.stop_stream()?;
    Ok(())
}

/// Mixes the given inputs into 16-bit PCM following the recording clock.
pub struct Mixer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Mixer {
    /// Starts mixing once `ready` becomes true (the encoder has restarted `clock`),
    /// delivering PCM blocks to `sink`. Nothing is produced while the clock is paused.
    pub fn start(
        inputs: Vec<Arc<InputShared>>,
        ready: Arc<AtomicBool>,
        clock: Arc<Clock>,
        mut sink: impl FnMut(Vec<u8>) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("audio-mixer".into())
            .spawn({
                let stop = stop.clone();
                move || {
                    while !ready.load(Ordering::Acquire) {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    let clear = || inputs.iter().for_each(|i| i.buffer.lock().unwrap().clear());
                    clear();
                    let mut produced: u64 = 0;
                    let mut mix = Vec::new();
                    while !stop.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(10));
                        if clock.is_paused() {
                            // Sound made during a pause must not leak into the file on resume.
                            clear();
                            continue;
                        }
                        let due = (clock.elapsed().as_secs_f64() * SAMPLE_RATE as f64) as u64;
                        let frames = due.saturating_sub(produced) as usize;
                        if frames == 0 {
                            continue;
                        }
                        produced += frames as u64;
                        mix_block(&inputs, frames * CHANNELS, &mut mix);
                        let pcm =
                            mix.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
                        sink(pcm.collect());
                    }
                }
            })
            .expect("spawn mixer thread");
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for Mixer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn mix_block(inputs: &[Arc<InputShared>], samples: usize, out: &mut Vec<f32>) {
    out.clear();
    out.resize(samples, 0.0);
    for input in inputs {
        let gain = input.gain();
        let mut buf = input.buffer.lock().unwrap();
        // Bound latency if a device delivers faster than the clock.
        let excess = buf.len().saturating_sub(samples + MAX_LATENCY);
        buf.drain(..excess);
        let take = buf.len().min(samples);
        for (o, s) in out.iter_mut().zip(buf.drain(..take)) {
            *o += s * gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_with(samples: &[f32], gain: f32) -> Arc<InputShared> {
        let input = InputShared::new(gain);
        input.buffer.lock().unwrap().extend(samples);
        Arc::new(input)
    }

    #[test]
    fn mixes_with_gain_and_pads_silence() {
        let a = input_with(&[0.5, 0.5], 1.0);
        let b = input_with(&[0.2, 0.2, 0.2, 0.2], 0.5);
        let mut out = Vec::new();
        mix_block(&[a.clone(), b], 4, &mut out);
        assert_eq!(out, vec![0.6, 0.6, 0.1, 0.1]);
        assert!(a.buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn drops_oldest_samples_beyond_latency_bound() {
        let data: Vec<f32> = (0..(MAX_LATENCY + 10)).map(|i| i as f32).collect();
        let a = input_with(&data, 1.0);
        let mut out = Vec::new();
        mix_block(std::slice::from_ref(&a), 4, &mut out);
        // Six oldest samples dropped, then four consumed.
        assert_eq!(out[0], 6.0);
        assert_eq!(a.buffer.lock().unwrap().len(), MAX_LATENCY);
    }
}
