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
use windows::Win32::Media::MediaFoundation::IMFMediaType;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::BOOL;
use windows_capture::frame::Frame;

use crate::camera::{self, CameraSize, Corner, PixelRect};
use crate::i18n::tr;
use crate::mf::{Aac, Converter, H264, Media, Mp4, Packet, TICKS_PER_SECOND, VideoEvent};

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

/// Where encoded packets go: the recording file and the replay buffer, each optional. Shared
/// with the app, which turns them on and off while the encoder runs.
#[derive(Default)]
pub struct Outputs {
    /// The recording asked for, and its file once a keyframe has arrived.
    path: Option<PathBuf>,
    file: Option<Com<Mp4>>,
    /// Audio not yet in the file: from the last second, or since the recording was asked for.
    pending_audio: VecDeque<Packet>,
    replay: Option<Ring>,
    video_type: Option<Com<IMFMediaType>>,
    audio_type: Option<Com<IMFMediaType>>,
    /// The encoder should make its next frame a keyframe, for a recording to start on.
    keyframe_wanted: bool,
    error: Option<String>,
}

impl Outputs {
    /// Starts recording to `path` at the next keyframe, which is asked for right away.
    pub fn start_file(&mut self, path: PathBuf) {
        (self.path, self.file, self.error, self.keyframe_wanted) = (Some(path), None, None, true);
    }

    /// Stops recording; the returned file must still be finished (it can take a moment).
    pub fn take_file(&mut self) -> Option<PendingFile> {
        let path = self.path.take()?;
        Some(PendingFile { path, file: self.file.take(), error: self.error.take() })
    }

    /// Keeps the last `seconds` of encoded video and audio in memory, or stops doing so.
    pub fn set_replay(&mut self, seconds: Option<u32>) {
        self.replay = seconds.map(|s| Ring::new(s as i64 * TICKS_PER_SECOND));
    }

    /// A copy of the replay buffer, to be saved.
    pub fn replay(&self) -> Result<Replay, String> {
        let empty = || tr!("el búfer de repetición todavía está vacío", "the replay buffer is still empty").to_owned();
        let ring = self.replay.as_ref().filter(|r| !r.video.is_empty()).ok_or_else(empty)?;
        let video_type = self.video_type.as_ref().ok_or_else(empty)?;
        Ok(Replay {
            video_type: Com(video_type.0.clone()),
            audio_type: self.audio_type.as_ref().map(|t| Com(t.0.clone())),
            video: ring.video.iter().cloned().collect(),
            audio: ring.audio.iter().cloned().collect(),
        })
    }

    fn take_keyframe_request(&mut self) -> bool {
        std::mem::take(&mut self.keyframe_wanted)
    }

    fn video(&mut self, packet: Packet, video_type: impl FnOnce() -> windows::core::Result<IMFMediaType>) {
        if packet.key && self.video_type.is_none() {
            match video_type() {
                Ok(t) => self.video_type = Some(Com(t)),
                Err(e) => {
                    self.error.get_or_insert(e.to_string());
                }
            }
        }
        if let (Some(path), None, true, Some(video)) = (&self.path, &self.file, packet.key, &self.video_type) {
            match Mp4::create(path, &video.0, self.audio_type.as_ref().map(|t| &t.0), packet.time) {
                Ok(file) => {
                    for audio in self.pending_audio.drain(..) {
                        let _ = file.write(false, &audio);
                    }
                    self.file = Some(Com(file));
                }
                Err(e) => {
                    self.error.get_or_insert(e.to_string());
                }
            }
        }
        if let Some(file) = &self.file
            && let Err(e) = file.0.write(true, &packet)
        {
            self.error.get_or_insert(e.to_string());
        }
        if let Some(ring) = &mut self.replay {
            ring.push_video(packet);
        }
    }

    fn audio(&mut self, packets: Vec<Packet>) {
        for packet in packets {
            match &self.file {
                Some(file) => {
                    if let Err(e) = file.0.write(false, &packet) {
                        self.error.get_or_insert(e.to_string());
                    }
                }
                None => {
                    self.pending_audio.push_back(packet.clone());
                    // Without a recording waiting for its keyframe, a second is plenty.
                    if self.path.is_none() {
                        while self.pending_audio.front().is_some_and(|p| p.time < packet.time - TICKS_PER_SECOND) {
                            self.pending_audio.pop_front();
                        }
                    }
                }
            }
            if let Some(ring) = &mut self.replay {
                ring.push_audio(packet);
            }
        }
    }
}

/// A recording that was stopped, still to be written out.
pub struct PendingFile {
    path: PathBuf,
    file: Option<Com<Mp4>>,
    error: Option<String>,
}

impl PendingFile {
    pub fn finish(self) -> Result<PathBuf, String> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let file = self.file.ok_or_else(|| tr!("no se codificó ningún fotograma", "no frame was encoded").to_owned())?;
        file.0.finish().map_err(|e| e.to_string())?;
        Ok(self.path)
    }
}

/// The last seconds of encoded packets, starting at a video keyframe so they play on their own.
struct Ring {
    /// 100 ns units.
    length: i64,
    video: VecDeque<Packet>,
    audio: VecDeque<Packet>,
}

impl Ring {
    fn new(length: i64) -> Self {
        Self { length, video: VecDeque::new(), audio: VecDeque::new() }
    }

    fn push_video(&mut self, packet: Packet) {
        if self.video.is_empty() && !packet.key {
            return;
        }
        let oldest_needed = packet.time - self.length;
        self.video.push_back(packet);
        // Drop whole keyframe intervals while the next keyframe still covers `length`.
        while let Some(next) = self.video.iter().skip(1).position(|p| p.key).map(|i| i + 1)
            && self.video[next].time <= oldest_needed
        {
            self.video.drain(..next);
        }
        self.trim_audio();
    }

    fn push_audio(&mut self, packet: Packet) {
        self.audio.push_back(packet);
        self.trim_audio();
    }

    /// Audio from before the first video frame is of no use, nor is audio older than `length`.
    fn trim_audio(&mut self) {
        let start = match (self.video.front(), self.audio.back()) {
            (Some(video), _) => video.time,
            (None, Some(audio)) => audio.time - self.length,
            (None, None) => return,
        };
        while self.audio.front().is_some_and(|p| p.time < start) {
            self.audio.pop_front();
        }
    }
}

/// A copy of the replay buffer.
pub struct Replay {
    video_type: Com<IMFMediaType>,
    audio_type: Option<Com<IMFMediaType>>,
    video: Vec<Packet>,
    audio: Vec<Packet>,
}

impl Replay {
    pub fn save(self, path: PathBuf) -> Result<PathBuf, String> {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let start = self.video[0].time;
        let audio_type = self.audio_type.as_ref().map(|t| &t.0);
        let file = Mp4::create(&path, &self.video_type.0, audio_type, start).map_err(|e| e.to_string())?;
        // Interleaved by time, the order a player reads them in.
        let (mut audio, mut video) = (self.audio.iter().peekable(), self.video.iter().peekable());
        loop {
            let is_video = match (audio.peek(), video.peek()) {
                (Some(a), Some(v)) => v.time <= a.time,
                (Some(_), None) => false,
                (None, Some(_)) => true,
                (None, None) => break,
            };
            let packet = if is_video { video.next() } else { audio.next() };
            file.write(is_video, packet.expect("peeked")).map_err(|e| e.to_string())?;
        }
        file.finish().map_err(|e| e.to_string())?;
        Ok(path)
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
    output: Arc<Mutex<Outputs>>,
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
    /// Name of the H.264 encoder in use, for diagnostics.
    pub encoder_name: String,
    /// Dropped last, after everything that uses Media Foundation.
    _media: Media,
}

impl Encoder {
    /// Creates the encoder and starts `clock` (setting `started`); from then on the clock
    /// drives both video and audio.
    pub fn new(
        device: &ID3D11Device,
        s: EncoderSettings,
        clock: Arc<Clock>,
        started: Arc<AtomicBool>,
        outputs: Arc<Mutex<Outputs>>,
    ) -> Result<Self, String> {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let media = Media::start().map_err(|e| e.to_string())?;
        Self::create(device, s, clock, started, outputs, media)
    }

    fn create(
        device: &ID3D11Device,
        s: EncoderSettings,
        clock: Arc<Clock>,
        started: Arc<AtomicBool>,
        outputs: Arc<Mutex<Outputs>>,
        media: Media,
    ) -> Result<Self, String> {
        let fps = s.fps.max(1);
        let h264 = H264::new(device, s.output, fps, s.bitrate)
            .map_err(|e| tr!("no se pudo iniciar el codificador H.264: {}", "could not start the H.264 encoder: {}", e))?;
        let encoder_name = h264.name.clone();
        let aac = if s.audio { Some(Aac::new().map_err(|e| e.to_string())?) } else { None };
        outputs.lock().unwrap().audio_type = match &aac {
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
            output: outputs,
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
            _media: media,
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

    /// Ends both streams; what was being recorded still has to be finished.
    pub fn finish(mut self) -> Option<PendingFile> {
        self.stop_threads();
        self.inner.output.lock().unwrap().take_file()
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
            if let Some(file) = self.inner.output.lock().unwrap().take_file() {
                let _ = file.finish();
            }
        }
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
                    if inner.output.lock().unwrap().take_keyframe_request() {
                        h264.force_keyframe();
                    }
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
                Ok(Some(packet)) => inner.output.lock().unwrap().video(packet, || h264.output_type()),
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

    fn packet(time: i64, key: bool) -> Packet {
        Packet { data: vec![0; 4], time, duration: 0, key }
    }

    /// 30 fps video with a keyframe every 2 s, and audio every 1024 samples, for `seconds`.
    fn fill(ring: &mut Ring, seconds: i64) {
        let audio_step = 1024 * TICKS_PER_SECOND / 48_000;
        let mut audio = 0;
        for n in 0..seconds * 30 {
            let time = n * TICKS_PER_SECOND / 30;
            while audio <= time {
                ring.push_audio(packet(audio, true));
                audio += audio_step;
            }
            ring.push_video(packet(time, n % 60 == 0));
        }
    }

    #[test]
    fn replay_keeps_the_last_seconds_from_a_keyframe() {
        let mut ring = Ring::new(20 * TICKS_PER_SECOND);
        fill(&mut ring, 60);
        let (first, newest) = (&ring.video[0], ring.video.back().unwrap());
        assert!(first.key, "the replay must start on a keyframe");
        let covered = newest.time - first.time;
        assert!(covered >= 20 * TICKS_PER_SECOND, "only {covered} covered");
        assert!(covered < 22 * TICKS_PER_SECOND, "{covered} kept: more than one keyframe interval too many");
        assert!(ring.audio[0].time >= first.time, "audio from before the first frame was kept");
        assert!(ring.audio[0].time - first.time < TICKS_PER_SECOND / 40, "audio at the start is missing");
    }

    #[test]
    fn replay_shorter_than_its_length_keeps_everything() {
        let mut ring = Ring::new(20 * TICKS_PER_SECOND);
        fill(&mut ring, 5);
        assert_eq!(ring.video.len(), 150);
        assert_eq!(ring.video[0].time, 0);
    }

    #[test]
    fn replay_starts_at_the_first_keyframe() {
        let mut ring = Ring::new(20 * TICKS_PER_SECOND);
        ring.push_video(packet(0, false));
        ring.push_video(packet(1, false));
        assert!(ring.video.is_empty());
        ring.push_video(packet(2, true));
        assert_eq!(ring.video.len(), 1);
    }

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
