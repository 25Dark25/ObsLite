//! Hardware H.264 encoder producing constant-frame-rate MP4.
//!
//! Windows Graphics Capture only delivers frames when the screen changes, so the
//! capture side (and the webcam) just draw into a GPU "canvas" texture. Like OBS's
//! video thread, a producer thread copies the canvas every 1/fps of recording time
//! into a queue that the encoder drains at its own pace: a short stall in the
//! encoder delays frames instead of losing or shifting them. Pausing stops the recording
//! clock, so paused time never reaches the file.
//!
//! The MediaStreamSource + MediaTranscoder pipeline follows the approach used by
//! the windows-capture crate (MIT). The transcoder also scales from the capture
//! size to the output size in hardware.

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
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
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext, ID3D11Query,
    ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11SurfaceFromDXGISurface;
use windows::core::{BOOL, HSTRING, Interface};
use windows_capture::frame::Frame;

use crate::audio::{CHANNELS, SAMPLE_RATE};
use crate::camera::{self, CameraSize, Corner, PixelRect};
use crate::i18n::tr;

/// Queued copies of the canvas (200 ms at 60 fps). If the encoder falls further behind,
/// the oldest queued frame is dropped and counted as lagged.
const SAMPLE_TEXTURES: usize = 12;
/// Copies kept out of reuse after the encoder takes them, because Media Foundation never
/// reports when it is done with a sample.
const IN_FLIGHT: usize = 4;
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
    /// Where the webcam goes, if there is one.
    pub overlay: Option<(Corner, CameraSize)>,
}

/// COM pointer used from several threads; D3D11 devices are free-threaded and the
/// immediate context is multithread-protected (see `capture::keep_last_frame`).
pub struct Com<T>(pub T);
unsafe impl<T> Send for Com<T> {}
unsafe impl<T> Sync for Com<T> {}

/// The picture being recorded (capture plus webcam) and the copies handed to the encoder.
struct Canvas {
    texture: Com<ID3D11Texture2D>,
    target: Com<ID3D11RenderTargetView>,
    context: Com<ID3D11DeviceContext>,
    copies: Vec<(Com<ID3D11Texture2D>, Com<IDirect3DSurface>)>,
    /// Signals when the GPU has finished a copy.
    copied: Com<ID3D11Query>,
    /// Set once the first frame has been drawn.
    ready: AtomicBool,
    /// Keeps each group of GPU commands (a frame with its overlay, or a copy) together.
    lock: Mutex<()>,
}

impl Canvas {
    /// Copies the canvas into copy `slot` and waits until the GPU has done it: the hardware
    /// encoder reads the copy on another GPU engine, without waiting for this context, so
    /// handing it over earlier showed up as half-drawn frames.
    fn snapshot(&self, slot: usize) {
        let texture = &self.copies[slot].0;
        let ctx = &self.context.0;
        unsafe {
            {
                let _gpu = self.lock.lock().unwrap();
                ctx.CopyResource(&texture.0, &self.texture.0);
                ctx.End(&self.copied.0);
                ctx.Flush();
            }
            for _ in 0..50 {
                let mut done = BOOL(0);
                let data = Some((&raw mut done).cast());
                if ctx.GetData(&self.copied.0, data, size_of::<BOOL>() as u32, 0).is_ok() && done.as_bool() {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

struct Inner {
    canvas: Canvas,
    clock: Arc<Clock>,
    /// Set when the transcoder asks for its first video sample; the clock starts then.
    started: Arc<AtomicBool>,
    /// Video samples handed to the encoder.
    encoded: AtomicU64,
    /// Frame slots skipped because the encoder fell behind real time.
    lagged: AtomicU64,
    stop: AtomicBool,
    fps: u64,
    /// Frames waiting for the encoder: (frame number, copy slot).
    queue: Mutex<VecDeque<(u64, usize)>>,
    queued: Condvar,
    audio_rx: Mutex<Receiver<Vec<u8>>>,
    audio_frames_sent: AtomicU64,
}

pub struct Encoder {
    inner: Arc<Inner>,
    audio_tx: Option<Sender<Vec<u8>>>,
    source: Com<MediaStreamSource>,
    tokens: (i64, i64),
    transcode: Option<JoinHandle<Result<(), String>>>,
    producer: Option<JoinHandle<()>>,
    input: (u32, u32),
    device: Com<ID3D11Device>,
    overlay: Option<(Corner, CameraSize)>,
    /// The newest webcam picture, scaled, and where it goes: (x, y, width, height).
    overlay_texture: Mutex<Option<(Com<ID3D11Texture2D>, PixelRect)>>,
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
        let canvas = create_canvas(device, s.input)?;

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
        // Measured on an RTX 4060 (static and moving screens, with and without a webcam):
        // with no read-ahead the pipeline now and then encodes a black frame; with 30 ms it
        // stalls video requests for longer than the frame queue holds. 10 ms avoids both.
        source.SetBufferTime(Duration::from_millis(10).into())?;

        let (audio_tx, audio_rx) = mpsc::channel();
        let inner = Arc::new(Inner {
            canvas,
            clock,
            started,
            encoded: AtomicU64::new(0),
            lagged: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            fps: s.fps.max(1) as u64,
            queue: Mutex::new(VecDeque::new()),
            queued: Condvar::new(),
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
            return Err(tr!("el sistema no puede codificar con esta configuración ({:?})", "the system cannot encode with these settings ({:?})", reason).into());
        }
        // The transcoder and output stream must outlive the transcode operation.
        let job = Com((transcoder, stream, prepared));
        let producer = thread::Builder::new().name("video-frames".into()).spawn({
            let inner = inner.clone();
            move || produce_frames(&inner)
        })?;
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
            producer: Some(producer),
            input: s.input,
            device: Com(device.clone()),
            overlay: s.overlay,
            overlay_texture: Mutex::new(None),
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

    /// (samples encoded, frame slots skipped because the machine fell behind).
    pub fn stats(&self) -> (u64, u64) {
        (self.inner.encoded.load(Ordering::Relaxed), self.inner.lagged.load(Ordering::Relaxed))
    }

    /// Ends both streams and waits for the MP4 to be finalized.
    pub fn finish(mut self) -> Result<(), String> {
        self.audio_tx = None;
        self.inner.stop.store(true, Ordering::Release);
        if let Some(p) = self.producer.take() {
            let _ = p.join();
        }
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
        if let Some(p) = self.producer.take() {
            let _ = p.join();
        }
        if let Some(t) = self.transcode.take() {
            let _ = t.join();
        }
    }
}

fn create_canvas(device: &ID3D11Device, (w, h): (u32, u32)) -> windows::core::Result<Canvas> {
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
    let texture = || -> windows::core::Result<ID3D11Texture2D> {
        let mut texture = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
        Ok(texture.expect("CreateTexture2D returned no texture"))
    };
    let canvas = texture()?;
    let mut target = None;
    let mut copied = None;
    unsafe {
        device.CreateRenderTargetView(&canvas, None, Some(&mut target))?;
        device.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut copied))?;
    }
    let mut copies = Vec::with_capacity(SAMPLE_TEXTURES);
    for _ in 0..SAMPLE_TEXTURES {
        let copy = texture()?;
        let surface: IDirect3DSurface =
            unsafe { CreateDirect3D11SurfaceFromDXGISurface(&copy.cast::<IDXGISurface>()?)? }.cast()?;
        copies.push((Com(copy), Com(surface)));
    }
    Ok(Canvas {
        texture: Com(canvas),
        target: Com(target.expect("CreateRenderTargetView returned no view")),
        context: Com(unsafe { device.GetImmediateContext()? }),
        copies,
        copied: Com(copied.expect("CreateQuery returned no query")),
        ready: AtomicBool::new(false),
        lock: Mutex::new(()),
    })
}

/// Answers one sample request; `None` (end of stream) once stopped.
fn fulfil(inner: &Inner, request: &MediaStreamSourceSampleRequest, is_audio: bool) -> windows::core::Result<()> {
    let sample = if is_audio { next_audio(inner)? } else { next_video(inner)? };
    request.SetSample(sample.as_ref())
}

/// The video thread: every 1/fps of recording time, copies the canvas into the queue.
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
            while queue.len() > SAMPLE_TEXTURES - IN_FLIGHT {
                queue.pop_front();
                inner.lagged.fetch_add(1, Ordering::Relaxed);
            }
        }
        let slot = n as usize % SAMPLE_TEXTURES;
        inner.canvas.snapshot(slot);
        inner.queue.lock().unwrap().push_back((n, slot));
        inner.queued.notify_one();
        n += 1;
    }
    inner.queued.notify_all();
}

/// The next queued frame; once stopped, the remaining ones and then `None` (end of stream).
fn next_video(inner: &Inner) -> windows::core::Result<Option<MediaStreamSample>> {
    // The transcoder can take a while to spin up; time starts with its first request
    // so that start-up latency is not counted as lag.
    if !inner.started.load(Ordering::Acquire) {
        inner.clock.restart();
        inner.started.store(true, Ordering::Release);
    }
    let mut queue = inner.queue.lock().unwrap();
    let (n, slot) = loop {
        if let Some(frame) = queue.pop_front() {
            break frame;
        }
        if inner.stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        queue = inner.queued.wait_timeout(queue, Duration::from_millis(20)).unwrap().0;
    };
    drop(queue);
    let fps = inner.fps;
    let sample = MediaStreamSample::CreateFromDirect3D11Surface(
        &inner.canvas.copies[slot].1.0,
        TimeSpan { Duration: (n * TICKS_PER_SECOND / fps) as i64 },
    )?;
    sample.SetDuration(TimeSpan { Duration: (TICKS_PER_SECOND / fps) as i64 })?;
    inner.encoded.fetch_add(1, Ordering::Relaxed);
    Ok(Some(sample))
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
        let resumed = Instant::now();
        clock.set_paused(false);
        thread::sleep(Duration::from_millis(20));
        let after = clock.elapsed();
        assert!(after >= at_pause + Duration::from_millis(15));
        // Bounded by the real time since resuming, however long the sleep took: only the 50 ms
        // pause leaking into the clock can break it.
        assert!(after <= at_pause + resumed.elapsed(), "paused time leaked: {after:?}");
    }
}
