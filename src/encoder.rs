//! Hardware H.264 encoder producing constant-frame-rate MP4.
//!
//! Windows Graphics Capture only delivers frames when the screen changes, so the
//! capture side just copies each frame into a small ring of GPU textures. The
//! encoder pulls samples on its own fixed clock (like OBS's video thread): it
//! repeats the latest texture when nothing changed and skips ahead when it falls
//! behind. Pausing stops that clock, so paused time never reaches the file.
//!
//! The MediaStreamSource + MediaTranscoder pipeline follows the approach used by
//! the windows-capture crate (MIT). The transcoder also scales from the capture
//! size to the output size in hardware.

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::DirectX::Direct3D11::IDirect3DSurface;
use windows::Media::Core::{
    AudioStreamDescriptor, MediaStreamSample, MediaStreamSource, MediaStreamSourceSampleRequest,
    MediaStreamSourceSampleRequestedEventArgs, MediaStreamSourceStartingEventArgs, VideoStreamDescriptor,
};
use windows::Media::MediaProperties::{
    AudioEncodingProperties, ContainerEncodingProperties, MediaEncodingProfile, MediaEncodingSubtypes,
    VideoEncodingProperties,
};
use windows::Media::Transcoding::MediaTranscoder;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Storage::FileAccessMode;
use windows::Storage::Streams::FileRandomAccessStream;
use windows::System::Threading::{ThreadPool, WorkItemHandler, WorkItemOptions, WorkItemPriority};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    ID3D11Device, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11SurfaceFromDXGISurface;
use windows::core::{HSTRING, Interface};
use windows_capture::frame::Frame;

use crate::audio::{CHANNELS, SAMPLE_RATE};

/// A few textures so the capture never overwrites the one the encoder is reading.
const RING_SIZE: usize = 3;
const TICKS_PER_SECOND: u64 = 10_000_000;
const AUDIO_BLOCK_ALIGN: u64 = CHANNELS as u64 * 2; // 16-bit PCM

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
}

/// COM pointer used from several threads; D3D11 devices are free-threaded and the
/// immediate context is only touched from the capture thread.
struct Com<T>(T);
unsafe impl<T> Send for Com<T> {}
unsafe impl<T> Sync for Com<T> {}

struct Ring {
    textures: Vec<Com<ID3D11Texture2D>>,
    targets: Vec<Com<ID3D11RenderTargetView>>,
    surfaces: Vec<Com<IDirect3DSurface>>,
    /// Index of the newest complete texture, or `usize::MAX` before the first frame.
    latest: AtomicUsize,
}

struct Inner {
    ring: Ring,
    clock: Arc<Clock>,
    /// Set when the transcoder asks for its first video sample; the clock starts then.
    started: Arc<AtomicBool>,
    /// Video samples handed to the encoder.
    encoded: AtomicU64,
    /// Frame slots skipped because the encoder fell behind real time.
    lagged: AtomicU64,
    stop: AtomicBool,
    fps: u64,
    next_frame: Mutex<u64>,
    audio_rx: Mutex<Receiver<Vec<u8>>>,
    audio_frames_sent: AtomicU64,
}

pub struct Encoder {
    inner: Arc<Inner>,
    audio_tx: Option<Sender<Vec<u8>>>,
    source: Com<MediaStreamSource>,
    tokens: (i64, i64),
    transcode: Option<JoinHandle<Result<(), String>>>,
    input: (u32, u32),
}

impl Encoder {
    /// Creates the encoder. Once the transcoder is actually pulling samples it restarts
    /// `clock` and sets `started`; from then on the clock drives both video and audio.
    pub fn new(
        device: &ID3D11Device,
        s: EncoderSettings,
        clock: Arc<Clock>,
        started: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        Self::create(device, s, clock, started).map_err(|e| e.to_string())
    }

    fn create(
        device: &ID3D11Device,
        s: EncoderSettings,
        clock: Arc<Clock>,
        started: Arc<AtomicBool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let ring = create_ring(device, s.input)?;

        // Output profile: what ends up in the file.
        let video = VideoEncodingProperties::new()?;
        video.SetSubtype(&MediaEncodingSubtypes::H264()?)?;
        video.SetBitrate(s.bitrate)?;
        video.SetWidth(s.output.0)?;
        video.SetHeight(s.output.1)?;
        video.FrameRate()?.SetNumerator(s.fps)?;
        video.FrameRate()?.SetDenominator(1)?;
        video.PixelAspectRatio()?.SetNumerator(1)?;
        video.PixelAspectRatio()?.SetDenominator(1)?;
        let container = ContainerEncodingProperties::new()?;
        container.SetSubtype(&MediaEncodingSubtypes::Mpeg4()?)?;
        let profile = MediaEncodingProfile::new()?;
        profile.SetVideo(&video)?;
        profile.SetContainer(&container)?;

        // Input descriptors: what we feed in (BGRA at capture size, 16-bit PCM).
        let input = VideoEncodingProperties::CreateUncompressed(&MediaEncodingSubtypes::Bgra8()?, s.input.0, s.input.1)?;
        input.FrameRate()?.SetNumerator(s.fps)?;
        input.FrameRate()?.SetDenominator(1)?;
        let video_desc = VideoStreamDescriptor::Create(&input)?;
        let source = if s.audio {
            profile.SetAudio(&AudioEncodingProperties::CreateAac(SAMPLE_RATE, CHANNELS as u32, 192_000)?)?;
            let pcm = AudioEncodingProperties::CreatePcm(SAMPLE_RATE, CHANNELS as u32, 16)?;
            MediaStreamSource::CreateFromDescriptors(&video_desc, &AudioStreamDescriptor::Create(&pcm)?)?
        } else {
            MediaStreamSource::CreateFromDescriptor(&video_desc)?
        };
        // No read-ahead: with a buffer the pipeline stalls video requests while it fills
        // the audio buffer, which showed up as ~30% lagged frames at 1080p60.
        source.SetBufferTime(Duration::ZERO.into())?;

        let (audio_tx, audio_rx) = mpsc::channel();
        let inner = Arc::new(Inner {
            ring,
            clock,
            started,
            encoded: AtomicU64::new(0),
            lagged: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            fps: s.fps.max(1) as u64,
            next_frame: Mutex::new(0),
            audio_rx: Mutex::new(audio_rx),
            audio_frames_sent: AtomicU64::new(0),
        });

        let starting = source.Starting(&TypedEventHandler::<MediaStreamSource, MediaStreamSourceStartingEventArgs>::new(
            |_, args| {
                if let Some(args) = args.as_ref() {
                    args.Request()?.SetActualStartPosition(TimeSpan { Duration: 0 })?;
                }
                Ok(())
            },
        ))?;
        let requested = source.SampleRequested(&TypedEventHandler::<
            MediaStreamSource,
            MediaStreamSourceSampleRequestedEventArgs,
        >::new({
            let inner = inner.clone();
            move |_, args| {
                let Some(args) = args.as_ref() else { return Ok(()) };
                let request = args.Request()?;
                let is_audio = request.StreamDescriptor()?.cast::<AudioStreamDescriptor>().is_ok();
                let deferral = request.GetDeferral()?;
                let inner = inner.clone();
                // Never block the event thread; samples are produced on the thread pool.
                ThreadPool::RunWithPriorityAndOptionsAsync(
                    &WorkItemHandler::new(move |_| {
                        fulfil(&inner, &request, is_audio)?;
                        deferral.Complete()
                    }),
                    WorkItemPriority::Normal,
                    WorkItemOptions::None,
                )?;
                Ok(())
            }
        }))?;

        File::create(&s.path)?;
        let path = std::path::absolute(&s.path)?;
        let stream = FileRandomAccessStream::OpenAsync(&HSTRING::from(path.as_os_str()), FileAccessMode::ReadWrite)?
            .join()?;
        let transcoder = MediaTranscoder::new()?;
        transcoder.SetHardwareAccelerationEnabled(true)?;
        let prepared = transcoder.PrepareMediaStreamSourceTranscodeAsync(&source, &stream, &profile)?.join()?;
        if !prepared.CanTranscode()? {
            let reason = prepared.FailureReason()?;
            let _ = std::fs::remove_file(&s.path);
            return Err(format!("el sistema no puede codificar con esta configuración ({reason:?})").into());
        }
        // The transcoder and output stream must outlive the transcode operation.
        let job = Com((transcoder, stream, prepared));
        let transcode = thread::Builder::new().name("transcode".into()).spawn(move || {
            let job = job;
            job.0.2.TranscodeAsync().and_then(|op| op.join()).map_err(|e| e.to_string())
        })?;

        Ok(Self {
            inner,
            audio_tx: s.audio.then_some(audio_tx),
            source: Com(source),
            tokens: (starting, requested),
            transcode: Some(transcode),
            input: s.input,
        })
    }

    /// Copies a captured frame into the next ring texture (capture thread only).
    pub fn push_frame(&self, frame: &Frame) {
        let ring = &self.inner.ring;
        let latest = ring.latest.load(Ordering::Acquire);
        let idx = if latest == usize::MAX { 0 } else { (latest + 1) % RING_SIZE };
        let ctx = frame.device_context();
        let dst = &ring.textures[idx].0;
        let (w, h) = (frame.width(), frame.height());
        unsafe {
            if (w, h) == self.input {
                ctx.CopyResource(dst, frame.as_raw_texture());
            } else {
                // A resized window: crop or pad with black at the original size.
                ctx.ClearRenderTargetView(&ring.targets[idx].0, &[0.0, 0.0, 0.0, 1.0]);
                let (cw, ch) = (w.min(self.input.0), h.min(self.input.1));
                let region = D3D11_BOX { left: 0, top: 0, front: 0, right: cw, bottom: ch, back: 1 };
                ctx.CopySubresourceRegion(dst, 0, 0, 0, 0, frame.as_raw_texture(), 0, Some(&region));
            }
            ctx.Flush();
        }
        ring.latest.store(idx, Ordering::Release);
    }

    pub fn push_audio(&self, pcm: Vec<u8>) {
        if let Some(tx) = &self.audio_tx {
            let _ = tx.send(pcm);
        }
    }

    /// (samples encoded, frame slots skipped because the machine fell behind).
    pub fn stats(&self) -> (u64, u64) {
        (self.inner.encoded.load(Ordering::Relaxed), self.inner.lagged.load(Ordering::Relaxed))
    }

    /// Ends both streams and waits for the MP4 to be finalized.
    pub fn finish(mut self) -> Result<(), String> {
        self.audio_tx = None;
        self.inner.stop.store(true, Ordering::Release);
        let result = self.transcode.take().map_or(Ok(()), |t| t.join().unwrap_or(Err("transcode panicked".into())));
        let _ = self.source.0.RemoveStarting(self.tokens.0);
        let _ = self.source.0.RemoveSampleRequested(self.tokens.1);
        result
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.audio_tx = None;
        self.inner.stop.store(true, Ordering::Release);
        if let Some(t) = self.transcode.take() {
            let _ = t.join();
        }
    }
}

fn create_ring(device: &ID3D11Device, (w, h): (u32, u32)) -> windows::core::Result<Ring> {
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
    let mut ring = Ring {
        textures: Vec::new(),
        targets: Vec::new(),
        surfaces: Vec::new(),
        latest: AtomicUsize::new(usize::MAX),
    };
    for _ in 0..RING_SIZE {
        let mut texture = None;
        let mut target = None;
        unsafe {
            device.CreateTexture2D(&desc, None, Some(&mut texture))?;
            let texture = texture.as_ref().expect("CreateTexture2D returned no texture");
            device.CreateRenderTargetView(texture, None, Some(&mut target))?;
        }
        let texture = texture.expect("CreateTexture2D returned no texture");
        let surface: IDirect3DSurface =
            unsafe { CreateDirect3D11SurfaceFromDXGISurface(&texture.cast::<IDXGISurface>()?)? }.cast()?;
        ring.surfaces.push(Com(surface));
        ring.targets.push(Com(target.expect("CreateRenderTargetView returned no view")));
        ring.textures.push(Com(texture));
    }
    Ok(ring)
}

/// Answers one sample request; `None` (end of stream) once stopped.
fn fulfil(inner: &Inner, request: &MediaStreamSourceSampleRequest, is_audio: bool) -> windows::core::Result<()> {
    let sample = if is_audio { next_audio(inner)? } else { next_video(inner)? };
    request.SetSample(sample.as_ref())
}

fn next_video(inner: &Inner) -> windows::core::Result<Option<MediaStreamSample>> {
    let mut n = inner.next_frame.lock().unwrap();
    let fps = inner.fps;
    // The transcoder can take a while to spin up; time starts with its first request
    // so that start-up latency is not counted as lag.
    if !inner.started.load(Ordering::Acquire) {
        inner.clock.restart();
        inner.started.store(true, Ordering::Release);
    }
    loop {
        if inner.stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        let now = inner.clock.elapsed();
        let due = Duration::from_nanos(*n * 1_000_000_000 / fps);
        let latest = inner.ring.latest.load(Ordering::Acquire);
        if now >= due && latest != usize::MAX {
            // Fell behind by more than a frame: skip ahead rather than drift from real time.
            let current = (now.as_nanos() * fps as u128 / 1_000_000_000) as u64;
            if current > *n + 1 {
                inner.lagged.fetch_add(current - *n, Ordering::Relaxed);
                *n = current;
            }
            let sample = MediaStreamSample::CreateFromDirect3D11Surface(
                &inner.ring.surfaces[latest].0,
                TimeSpan { Duration: (*n * TICKS_PER_SECOND / fps) as i64 },
            )?;
            sample.SetDuration(TimeSpan { Duration: (TICKS_PER_SECOND / fps) as i64 })?;
            *n += 1;
            inner.encoded.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(sample));
        }
        let wait = due.saturating_sub(now).clamp(Duration::from_millis(1), Duration::from_millis(5));
        thread::sleep(wait);
    }
}

fn next_audio(inner: &Inner) -> windows::core::Result<Option<MediaStreamSample>> {
    let rx = inner.audio_rx.lock().unwrap();
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(pcm) => {
                let frames = pcm.len() as u64 / AUDIO_BLOCK_ALIGN;
                let start = inner.audio_frames_sent.fetch_add(frames, Ordering::Relaxed);
                let buffer = CryptographicBuffer::CreateFromByteArray(&pcm)?;
                let sample = MediaStreamSample::CreateFromBuffer(
                    &buffer,
                    TimeSpan { Duration: (start * TICKS_PER_SECOND / SAMPLE_RATE as u64) as i64 },
                )?;
                sample.SetDuration(TimeSpan { Duration: (frames * TICKS_PER_SECOND / SAMPLE_RATE as u64) as i64 })?;
                return Ok(Some(sample));
            }
            Err(RecvTimeoutError::Timeout) if !inner.stop.load(Ordering::Acquire) => {}
            Err(_) => return Ok(None),
        }
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
