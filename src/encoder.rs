//! Hardware H.264 encoder producing constant-frame-rate MP4.
//!
//! Windows Graphics Capture only delivers frames when the screen changes, so the capture side
//! (and the webcam) just draw into a GPU "canvas" texture. Like OBS's video thread, a producer
//! thread converts the canvas to NV12 at the output size every 1/fps of recording time and
//! queues it; the hardware encoder of the same GPU takes frames from the queue, and the
//! encoded packets are written to the MP4 (see `mf`). Pausing stops the recording clock, so
//! paused time never reaches the file.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext, ID3D11Query,
    ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{IMFMediaType, MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::BOOL;
use windows_capture::frame::Frame;

use crate::camera::{self, CameraSize, Corner, PixelRect};
use crate::i18n::tr;
use crate::mf::{Aac, Converter, H264, Mp4, Packet, TICKS_PER_SECOND, VideoEvent};

/// Queued NV12 frames (200 ms at 60 fps, about 3 MB each at 1080p). If the encoder falls
/// further behind, the oldest queued frame is dropped and counted as lagged.
const FRAME_TEXTURES: usize = 12;
/// Frames kept out of reuse after the encoder takes them: it reads them on another GPU engine
/// and does not report when it is done.
const IN_FLIGHT: usize = 4;

/// Recording clock that excludes paused intervals.
pub struct Clock(Mutex<ClockState>);

struct ClockState {
    origin: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
}

impl Clock {
    pub fn new() -> Self {
        Self(Mutex::new(ClockState { origin: Instant::now(), paused_at: None, paused_total: Duration::ZERO }))
    }

    pub fn restart(&self) {
        *self.0.lock().unwrap() = ClockState { origin: Instant::now(), paused_at: None, paused_total: Duration::ZERO };
    }

    /// Active (unpaused) time since the last restart.
    pub fn elapsed(&self) -> Duration {
        let s = self.0.lock().unwrap();
        let end = s.paused_at.unwrap_or_else(Instant::now);
        end.saturating_duration_since(s.origin).saturating_sub(s.paused_total)
    }

    pub fn set_paused(&self, paused: bool) {
        let mut s = self.0.lock().unwrap();
        match (paused, s.paused_at) {
            (true, None) => s.paused_at = Some(Instant::now()),
            (false, Some(at)) => {
                s.paused_total += at.elapsed();
                s.paused_at = None;
            }
            _ => {}
        }
    }

    pub fn is_paused(&self) -> bool {
        self.0.lock().unwrap().paused_at.is_some()
    }
}

pub struct EncoderSettings {
    pub path: PathBuf,
    pub input: (u32, u32),
    pub output: (u32, u32),
    pub fps: u32,
    pub bitrate: u32,
    pub audio: bool,
    /// Where the webcam goes, if there is one.
    pub overlay: Option<(Corner, CameraSize)>,
}

/// COM pointer used from several threads; D3D11 devices are free-threaded and the
/// immediate context is multithread-protected (see `capture::keep_last_frame`).
pub struct Com<T>(pub T);
unsafe impl<T> Send for Com<T> {}
unsafe impl<T> Sync for Com<T> {}

/// The picture being recorded: capture plus webcam, at the capture (or region) size.
struct Canvas {
    texture: Com<ID3D11Texture2D>,
    target: Com<ID3D11RenderTargetView>,
    context: Com<ID3D11DeviceContext>,
    converter: Com<Converter>,
    /// Signals when the GPU has finished a conversion.
    converted: Com<ID3D11Query>,
    /// Set once the first frame has been drawn.
    ready: AtomicBool,
    /// Keeps each group of GPU commands (a frame with its overlay, or a conversion) together.
    lock: Mutex<()>,
}

impl Canvas {
    /// Converts the canvas into NV12 frame `slot` and waits until the GPU has done it: the
    /// hardware encoder reads the frame on another GPU engine, without waiting for this
    /// context, so handing it over earlier showed up as half-drawn frames.
    fn snapshot(&self, slot: usize) {
        let ctx = &self.context.0;
        unsafe {
            {
                let _gpu = self.lock.lock().unwrap();
                let _ = self.converter.0.convert(slot);
                ctx.End(&self.converted.0);
                ctx.Flush();
            }
            for _ in 0..50 {
                let mut done = BOOL(0);
                let data = Some((&raw mut done).cast());
                if ctx.GetData(&self.converted.0, data, size_of::<BOOL>() as u32, 0).is_ok() && done.as_bool() {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// Where encoded packets go: the MP4 file, created at the first keyframe.
struct Output {
    path: PathBuf,
    file: Option<Com<Mp4>>,
    audio_type: Option<Com<IMFMediaType>>,
    /// Audio encoded before the first keyframe.
    pending_audio: Vec<Packet>,
    error: Option<String>,
}

impl Output {
    fn video(&mut self, packet: &Packet, video_type: impl FnOnce() -> windows::core::Result<IMFMediaType>) {
        if self.file.is_none() {
            if !packet.key {
                return;
            }
            let created = video_type().and_then(|video| {
                Mp4::create(&self.path, &video, self.audio_type.as_ref().map(|t| &t.0), packet.time)
            });
            match created {
                Ok(file) => {
                    for audio in self.pending_audio.drain(..) {
                        let _ = file.write(false, &audio);
                    }
                    self.file = Some(Com(file));
                }
                Err(e) => {
                    self.error.get_or_insert(e.to_string());
                    return;
                }
            }
        }
        if let Some(file) = &self.file
            && let Err(e) = file.0.write(true, packet)
        {
            self.error.get_or_insert(e.to_string());
        }
    }

    fn audio(&mut self, packets: Vec<Packet>) {
        match &self.file {
            Some(file) => {
                for packet in &packets {
                    if let Err(e) = file.0.write(false, packet) {
                        self.error.get_or_insert(e.to_string());
                    }
                }
            }
            None => self.pending_audio.extend(packets),
        }
    }

    fn finish(&mut self) -> Result<(), String> {
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        match self.file.take() {
            Some(file) => file.0.finish().map_err(|e| e.to_string()),
            None => Err(tr!("no se codificó ningún fotograma", "no frame was encoded").into()),
        }
    }
}

struct Inner {
    canvas: Canvas,
    clock: Arc<Clock>,
    /// Set when the encoder is running; the clock starts then.
    started: Arc<AtomicBool>,
    /// Frames handed to the encoder.
    encoded: AtomicU64,
    /// Frame slots skipped because the machine or the encoder fell behind real time.
    lagged: AtomicU64,
    stop: AtomicBool,
    fps: u64,
    /// Frames waiting for the encoder: (frame number, NV12 slot).
    queue: Mutex<VecDeque<(u64, usize)>>,
    queued: Condvar,
    output: Mutex<Output>,
}

pub struct Encoder {
    inner: Arc<Inner>,
    audio_tx: Option<Sender<Vec<u8>>>,
    threads: Vec<JoinHandle<()>>,
    input: (u32, u32),
    device: Com<ID3D11Device>,
    overlay: Option<(Corner, CameraSize)>,
    /// The newest webcam picture, scaled, and where it goes: (x, y, width, height).
    overlay_texture: Mutex<Option<(Com<ID3D11Texture2D>, PixelRect)>>,
    /// Name of the hardware encoder in use, for diagnostics.
    pub encoder_name: String,
}

impl Encoder {
    /// Creates the encoder and starts `clock` (setting `started`); from then on the clock
    /// drives both video and audio.
    pub fn new(
        device: &ID3D11Device,
        s: EncoderSettings,
        clock: Arc<Clock>,
        started: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| e.to_string())?;
        }
        let created = Self::create(device, s, clock, started);
        if created.is_err() {
            let _ = unsafe { MFShutdown() };
        }
        created
    }

    fn create(device: &ID3D11Device, s: EncoderSettings, clock: Arc<Clock>, started: Arc<AtomicBool>) -> Result<Self, String> {
        let fps = s.fps.max(1);
        let h264 = H264::new(device, s.output, fps, s.bitrate)
            .map_err(|e| tr!("no se pudo iniciar el codificador H.264: {}", "could not start the H.264 encoder: {}", e))?;
        let encoder_name = h264.name.clone();
        let aac = if s.audio { Some(Aac::new().map_err(|e| e.to_string())?) } else { None };
        let audio_type = match &aac {
            Some(aac) => Some(Com(aac.output_type().map_err(|e| e.to_string())?)),
            None => None,
        };
        let canvas = create_canvas(device, s.input, s.output).map_err(|e| e.to_string())?;
        let inner = Arc::new(Inner {
            canvas,
            clock,
            started,
            encoded: AtomicU64::new(0),
            lagged: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            fps: fps as u64,
            queue: Mutex::new(VecDeque::new()),
            queued: Condvar::new(),
            output: Mutex::new(Output { path: s.path, file: None, audio_type, pending_audio: Vec::new(), error: None }),
        });

        let spawn = |name: &str, work: Box<dyn FnOnce() + Send>| {
            thread::Builder::new().name(name.into()).spawn(work).map_err(|e| e.to_string())
        };
        let mut threads = Vec::new();
        let h264 = Com(h264);
        threads.push(spawn("video-encoder", Box::new({
            let inner = inner.clone();
            move || encode_video(&inner, h264)
        }))?);
        threads.push(spawn("video-frames", Box::new({
            let inner = inner.clone();
            move || produce_frames(&inner)
        }))?);
        let (audio_tx, audio_rx) = mpsc::channel();
        if let Some(aac) = aac {
            let aac = Com(aac);
            threads.push(spawn("audio-encoder", Box::new({
                let inner = inner.clone();
                move || encode_audio(&inner, aac, audio_rx)
            }))?);
        }

        Ok(Self {
            inner,
            audio_tx: s.audio.then_some(audio_tx),
            threads,
            input: s.input,
            device: Com(device.clone()),
            overlay: s.overlay,
            overlay_texture: Mutex::new(None),
            encoder_name,
        })
    }

    /// Draws the part of a captured frame starting at `origin` onto the canvas, with the
    /// webcam on top (capture thread only).
    pub fn push_frame(&self, frame: &Frame, origin: (u32, u32)) {
        let canvas = &self.inner.canvas;
        let (ctx, dst, source) = (&canvas.context.0, &canvas.texture.0, frame.as_raw_texture());
        let (w, h) = (frame.width(), frame.height());
        let _gpu = canvas.lock.lock().unwrap();
        unsafe {
            if origin == (0, 0) && (w, h) == self.input {
                ctx.CopyResource(dst, source);
            } else {
                // A region, or a resized window: crop, padding with black what is missing.
                let (left, top) = origin;
                let (right, bottom) = ((left + self.input.0).min(w), (top + self.input.1).min(h));
                if (right.saturating_sub(left), bottom.saturating_sub(top)) != self.input {
                    ctx.ClearRenderTargetView(&canvas.target.0, &[0.0, 0.0, 0.0, 1.0]);
                }
                if right > left && bottom > top {
                    let region = D3D11_BOX { left, top, front: 0, right, bottom, back: 1 };
                    ctx.CopySubresourceRegion(dst, 0, 0, 0, 0, source, 0, Some(&region));
                }
            }
            if let Some((texture, (x, y, _, _))) = self.overlay_texture.lock().unwrap().as_ref() {
                ctx.CopySubresourceRegion(dst, 0, *x, *y, 0, &texture.0, 0, None);
            }
            ctx.Flush();
        }
        canvas.ready.store(true, Ordering::Release);
    }

    /// Where a `camera`-sized webcam picture goes on the recording, if there is an overlay.
    pub fn overlay_rect(&self, camera: (u32, u32)) -> Option<PixelRect> {
        self.overlay.map(|(corner, size)| camera::overlay_rect(self.input, camera, corner, size))
    }

    /// Takes a webcam picture already scaled to `rect` (see `overlay_rect`) and draws it onto
    /// the canvas right away, so the camera keeps moving while the screen does not.
    pub fn push_overlay(&self, rect: PixelRect, pixels: &[u8]) {
        let (x, y, w, h) = rect;
        let canvas = &self.inner.canvas;
        let ctx = &canvas.context.0;
        let mut overlay = self.overlay_texture.lock().unwrap();
        let _gpu = canvas.lock.lock().unwrap();
        unsafe {
            if overlay.as_ref().is_none_or(|(_, r)| *r != rect) {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w,
                    Height: h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    ..Default::default()
                };
                let mut texture = None;
                if self.device.0.CreateTexture2D(&desc, None, Some(&mut texture)).is_err() {
                    return;
                }
                *overlay = Some((Com(texture.expect("CreateTexture2D returned no texture")), rect));
            }
            let texture = &overlay.as_ref().expect("set above").0.0;
            ctx.UpdateSubresource(texture, 0, None, pixels.as_ptr().cast(), w * 4, 0);
            if canvas.ready.load(Ordering::Acquire) {
                ctx.CopySubresourceRegion(&canvas.texture.0, 0, x, y, 0, texture, 0, None);
            }
            ctx.Flush();
        }
    }

    pub fn push_audio(&self, pcm: Vec<u8>) {
        if let Some(tx) = &self.audio_tx {
            let _ = tx.send(pcm);
        }
    }

    /// (frames encoded, frame slots skipped because the machine fell behind).
    pub fn stats(&self) -> (u64, u64) {
        (self.inner.encoded.load(Ordering::Relaxed), self.inner.lagged.load(Ordering::Relaxed))
    }

    /// Ends both streams and waits for the MP4 to be finalized.
    pub fn finish(mut self) -> Result<(), String> {
        self.stop_threads();
        self.inner.output.lock().unwrap().finish()
    }

    fn stop_threads(&mut self) {
        self.audio_tx = None;
        self.inner.stop.store(true, Ordering::Release);
        self.inner.queued.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if !self.threads.is_empty() {
            self.stop_threads();
            let _ = self.inner.output.lock().unwrap().finish();
        }
        let _ = unsafe { MFShutdown() };
    }
}

fn create_canvas(device: &ID3D11Device, (w, h): (u32, u32), output: (u32, u32)) -> windows::core::Result<Canvas> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    let mut target = None;
    let mut converted = None;
    unsafe {
        device.CreateTexture2D(&desc, None, Some(&mut texture))?;
        let texture = texture.as_ref().expect("CreateTexture2D returned no texture");
        device.CreateRenderTargetView(texture, None, Some(&mut target))?;
        device.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut converted))?;
    }
    let texture = texture.expect("CreateTexture2D returned no texture");
    let converter = Converter::new(device, &texture, (w, h), output, FRAME_TEXTURES)?;
    Ok(Canvas {
        texture: Com(texture),
        target: Com(target.expect("CreateRenderTargetView returned no view")),
        context: Com(unsafe { device.GetImmediateContext()? }),
        converter: Com(converter),
        converted: Com(converted.expect("CreateQuery returned no query")),
        ready: AtomicBool::new(false),
        lock: Mutex::new(()),
    })
}

/// The video thread: every 1/fps of recording time, converts the canvas into the queue.
fn produce_frames(inner: &Inner) {
    let fps = inner.fps;
    let mut n = 0u64;
    while !inner.stop.load(Ordering::Acquire) {
        let now = inner.clock.elapsed();
        let due = Duration::from_nanos(n * 1_000_000_000 / fps);
        if !inner.started.load(Ordering::Acquire) || !inner.canvas.ready.load(Ordering::Acquire) || now < due {
            thread::sleep(due.saturating_sub(now).clamp(Duration::from_millis(1), Duration::from_millis(5)));
            continue;
        }
        // Descheduled for more than a frame: skip ahead rather than drift from real time.
        let current = (now.as_nanos() * fps as u128 / 1_000_000_000) as u64;
        if current > n + 1 {
            inner.lagged.fetch_add(current - n, Ordering::Relaxed);
            n = current;
        }
        {
            // The slot is about to be overwritten: it must not still be queued or in flight.
            let mut queue = inner.queue.lock().unwrap();
            while queue.len() > FRAME_TEXTURES - IN_FLIGHT {
                queue.pop_front();
                inner.lagged.fetch_add(1, Ordering::Relaxed);
            }
        }
        let slot = n as usize % FRAME_TEXTURES;
        inner.canvas.snapshot(slot);
        inner.queue.lock().unwrap().push_back((n, slot));
        inner.queued.notify_one();
        n += 1;
    }
    inner.queued.notify_all();
}

/// The next queued frame; once stopped and empty, `None`.
fn next_frame(inner: &Inner) -> Option<(u64, usize)> {
    let mut queue = inner.queue.lock().unwrap();
    loop {
        if let Some(frame) = queue.pop_front() {
            return Some(frame);
        }
        if inner.stop.load(Ordering::Acquire) {
            return None;
        }
        queue = inner.queued.wait_timeout(queue, Duration::from_millis(20)).unwrap().0;
    }
}

/// Feeds queued frames to the hardware encoder and passes its packets on.
fn encode_video(inner: &Inner, h264: Com<H264>) {
    let h264 = h264.0;
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // Time starts now, so encoder start-up is not counted as lag.
    inner.clock.restart();
    inner.started.store(true, Ordering::Release);
    let fps = inner.fps as i64;
    let mut draining = false;
    loop {
        let event = match h264.next_event() {
            Ok(event) => event,
            Err(e) => {
                inner.output.lock().unwrap().error.get_or_insert(e.to_string());
                break;
            }
        };
        match event {
            VideoEvent::NeedInput if !draining => match next_frame(inner) {
                Some((n, slot)) => {
                    let texture = inner.canvas.converter.0.texture(slot);
                    if let Err(e) = h264.push(texture, n as i64 * TICKS_PER_SECOND / fps, TICKS_PER_SECOND / fps) {
                        inner.output.lock().unwrap().error.get_or_insert(e.to_string());
                    }
                    inner.encoded.fetch_add(1, Ordering::Relaxed);
                }
                None => {
                    draining = true;
                    if h264.drain().is_err() {
                        break;
                    }
                }
            },
            VideoEvent::HaveOutput => match h264.pull() {
                Ok(Some(packet)) => inner.output.lock().unwrap().video(&packet, || h264.output_type()),
                Ok(None) => {}
                Err(e) => {
                    inner.output.lock().unwrap().error.get_or_insert(e.to_string());
                }
            },
            VideoEvent::Drained => break,
            _ => {}
        }
    }
}

/// Encodes the mixer's PCM to AAC until the encoder is finished.
fn encode_audio(inner: &Inner, aac: Com<Aac>, rx: Receiver<Vec<u8>>) {
    let mut aac = aac.0;
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    for pcm in rx {
        match aac.encode(&pcm) {
            Ok(packets) => inner.output.lock().unwrap().audio(packets),
            Err(e) => {
                inner.output.lock().unwrap().error.get_or_insert(e.to_string());
            }
        }
    }
    if let Ok(packets) = aac.drain() {
        inner.output.lock().unwrap().audio(packets);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_excludes_paused_time() {
        let clock = Clock::new();
        thread::sleep(Duration::from_millis(30));
        clock.set_paused(true);
        let at_pause = clock.elapsed();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(clock.elapsed(), at_pause);
        assert!(clock.is_paused());
        clock.set_paused(false);
        thread::sleep(Duration::from_millis(20));
        let after = clock.elapsed();
        assert!(after >= at_pause + Duration::from_millis(15));
        assert!(after < at_pause + Duration::from_millis(45), "paused time leaked: {after:?}");
    }
}
