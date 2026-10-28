//! H.264 + AAC encoding with GStreamer, producing constant-frame-rate packets for the shared
//! outputs (`media`).
//!
//! PipeWire may deliver frames only when the picture changes, so each captured frame is drawn
//! once into a picture in memory; like on Windows, a producer thread hands the newest picture
//! (with the webcam on top) to the encoder every 1/fps of recording time. The best available H.264 encoder is used:
//! VA-API (Intel/AMD), NVENC, then x264 or OpenH264 in software. Frames on the GPU (DMA-BUFs) are
//! instead converted there once each, by VA-API, and the newest surface is what is handed over.

use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use crate::audio::{CHANNELS, SAMPLE_RATE};
use crate::camera::{self, CameraSize, Corner, PixelRect};
use crate::i18n::tr;
use crate::media::{self, Clock, Muxer, Packet};

pub type Outputs = media::Outputs<Mp4>;
pub type PendingFile = media::PendingFile<Mp4>;

/// Queued frames the encoder may fall behind by before frames are dropped (and counted).
const QUEUED_FRAMES: u64 = 8;
/// Pictures drawn from captured frames: the queued ones, those being encoded and the newest.
const MAX_PICTURES: usize = QUEUED_FRAMES as usize + 4;

/// A picture shared with GStreamer, which reads it in place.
struct Shared(Arc<Vec<u8>>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
/// Packet times are in 100 ns units.
const NS_PER_TICK: u64 = 100;

pub struct EncoderSettings {
    pub input: (u32, u32),
    pub output: (u32, u32),
    pub fps: u32,
    pub bitrate: u32,
    pub audio: bool,
    /// Where the webcam goes, if there is one.
    pub overlay: Option<(Corner, CameraSize)>,
    /// The pictures hold red first (RGBx, as some desktops send them) instead of blue (BGRx).
    pub red_first: bool,
    /// The first frame, if frames come on the GPU (DMA-BUFs, see `push_gpu_frame`).
    pub gpu: Option<gst::Sample>,
}

/// The newest picture: drawn in memory, or a VA-API surface converted from a frame on the GPU.
#[derive(Clone)]
enum Picture {
    Memory(Arc<Vec<u8>>),
    Surface(gst::Buffer),
}

/// A captured picture: rows of 4-byte pixels (BGRx, or RGBx) `stride` bytes apart.
pub struct FrameRef<'a> {
    pub data: &'a [u8],
    pub size: (u32, u32),
    pub stride: usize,
}

struct Inner {
    /// The picture being recorded, BGRx at the input size.
    /// Pictures drawn from captured frames (`input` sized), drawn on again once nothing reads
    /// them; the newest is what the producer hands over, without copying it.
    pictures: Mutex<Vec<Arc<Vec<u8>>>>,
    latest: Mutex<Option<Picture>>,
    /// The webcam picture, already scaled, and where it goes.
    overlay: Mutex<Option<(Vec<u8>, PixelRect)>>,
    red_first: bool,
    ready: AtomicBool,
    clock: Arc<Clock>,
    encoded: AtomicU64,
    lagged: AtomicU64,
    stop: AtomicBool,
    fps: u64,
    input: (u32, u32),
    output_size: (u32, u32),
    video_src: gst_app::AppSrc,
    /// The video pipeline, to notice when it fails.
    video: gst::Pipeline,
    /// The H.264 encoder element, to ask it for keyframes.
    h264: gst::Element,
    output: Arc<Mutex<Outputs>>,
}

pub struct Encoder {
    inner: Arc<Inner>,
    producer: Option<JoinHandle<()>>,
    video: gst::Pipeline,
    /// Where frames on the GPU go to be converted, if they come so.
    frames: Option<gst_app::AppSrc>,
    audio: Option<(gst::Pipeline, gst_app::AppSrc)>,
    audio_frames: Mutex<u64>,
    overlay: Option<(Corner, CameraSize)>,
    name: String,
}

impl Encoder {
    /// Creates the encoder and starts `clock` (setting `started`); from then on the clock
    /// drives both video and audio.
    pub fn new(s: EncoderSettings, clock: Arc<Clock>, started: Arc<AtomicBool>, outputs: Arc<Mutex<Outputs>>) -> Result<Self, String> {
        gst::init().map_err(|e| e.to_string())?;
        let fps = s.fps.max(1);
        let (iw, ih) = s.input;
        let (name, video) = match &s.gpu {
            Some(frame) => {
                // VA-API converts the frames, so only its encoders can take them as they are.
                let va = h264_candidates(s.bitrate, fps).into_iter().filter(|(factory, ..)| factory.starts_with("va"));
                let (name, _, h264) = h264_encoder(va, |_, fragment| encodes_on_gpu(frame, fragment, s.output))?;
                // Each frame is converted once, as it comes (`frames` to `surfaces`); the newest surface
                // is what the producer hands the encoder (`src`).
                let surfaces = surface_caps(s.output);
                let video = launch(&format!(
                    "appsrc name=frames is-live=true format=time block=false max-bytes=0 max-buffers=2 leaky-type=downstream \
                     ! vapostproc add-borders=true ! {surfaces} ! appsink name=surfaces max-buffers=1 drop=true sync=false async=false \
                     appsrc name=src is-live=true format=time block=false \
                       caps=\"{surfaces},framerate={fps}/1\" \
                     ! {h264} ! h264parse ! video/x-h264,stream-format=avc,alignment=au \
                     ! appsink name=sink sync=false async=false"
                ))?;
                super::gpu::share_display(&video);
                (format!("{name}, frames on the GPU"), video)
            }
            None => {
                let (name, format, h264) = h264_encoder(h264_candidates(s.bitrate, fps), |format, fragment| encodes(format, fragment, s.output))?;
                // Two threads: one leaves 4K60 too little time per frame, and every thread there is costs
                // much more CPU (80 % more at 4K) for the work of sharing it.
                let caps = raw_caps(format, s.output);
                // The canvas is in the order the desktop sent (BGRx or RGBx), so only the encoder's own
                // conversion is left. An encoder that takes BGRA or RGBA gets it as such, its fourth byte
                // unused, so nothing is converted.
                let canvas = match (format == "BGRA", s.red_first) {
                    (true, false) => "BGRA",
                    (true, true) => "RGBA",
                    (false, false) => "BGRx",
                    (false, true) => "RGBx",
                };
                let video = launch(&format!(
                    "appsrc name=src is-live=true format=time block=false max-bytes={max} \
                       caps=video/x-raw,format={canvas},width={iw},height={ih},framerate={fps}/1 \
                     ! videoconvert n-threads=2 ! videoscale ! {caps} \
                     ! {h264} ! h264parse ! video/x-h264,stream-format=avc,alignment=au \
                     ! appsink name=sink sync=false",
                    max = iw as u64 * ih as u64 * 4 * (QUEUED_FRAMES + 2),
                ))?;
                (name, video)
            }
        };
        let video_src = by_name::<gst_app::AppSrc>(&video, "src")?;
        let frames = s.gpu.as_ref().map(|_| by_name::<gst_app::AppSrc>(&video, "frames")).transpose()?;
        let h264_element = by_name::<gst::Element>(&video, "enc")?;

        let audio = if s.audio {
            let aac = aac_encoder()?;
            let pipeline = launch(&format!(
                "appsrc name=src is-live=true format=time block=false \
                   caps=audio/x-raw,format=S16LE,layout=interleaved,rate={SAMPLE_RATE},channels={CHANNELS} \
                 ! audioconvert ! {aac} ! aacparse ! audio/mpeg,mpegversion=4,stream-format=raw \
                 ! appsink name=sink sync=false"
            ))?;
            let src = by_name::<gst_app::AppSrc>(&pipeline, "src")?;
            outputs.lock().unwrap().set_audio_format(Some(aac_caps()));
            Some((pipeline, src))
        } else {
            None
        };

        let inner = Arc::new(Inner {
            pictures: Mutex::new(Vec::new()),
            latest: Mutex::new(None),
            overlay: Mutex::new(None),
            red_first: s.red_first,
            ready: AtomicBool::new(false),
            clock,
            encoded: AtomicU64::new(0),
            lagged: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            fps: fps as u64,
            input: s.input,
            output_size: s.output,
            video_src,
            video: video.clone(),
            h264: h264_element,
            output: outputs,
        });

        let sink = by_name::<gst_app::AppSink>(&video, "sink")?;
        let out = inner.output.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(packet) = to_packet(&sample) {
                        let caps = sample.caps().map(|c| c.to_owned());
                        out.lock().unwrap().video(packet, || caps.ok_or_else(|| "no H.264 caps".to_owned()));
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        if frames.is_some() {
            // The callback must not keep the encoder alive: it lives as long as the pipeline.
            let weak = Arc::downgrade(&inner);
            by_name::<gst_app::AppSink>(&video, "surfaces")?.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let (Some(inner), Some(surface)) = (weak.upgrade(), sample.buffer_owned()) else { return Ok(gst::FlowSuccess::Ok) };
                        *inner.latest.lock().unwrap() = Some(Picture::Surface(surface));
                        inner.ready.store(true, Ordering::Release);
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }
        if let Some((pipeline, _)) = &audio {
            let sink = by_name::<gst_app::AppSink>(pipeline, "sink")?;
            let out = inner.output.clone();
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        if let Some(packet) = to_packet(&sample) {
                            out.lock().unwrap().audio(vec![Packet { key: true, ..packet }]);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }

        let pipelines = || std::iter::once(&video).chain(audio.as_ref().map(|(p, _)| p));
        if let Err(e) = pipelines().try_for_each(play) {
            for pipeline in pipelines() {
                let _ = pipeline.set_state(gst::State::Null);
            }
            return Err(e);
        }
        // Time starts now, so encoder start-up is not counted as lag.
        inner.clock.restart();
        started.store(true, Ordering::Release);

        let producer = thread::Builder::new()
            .name("video-frames".into())
            .spawn({
                let inner = inner.clone();
                move || produce_frames(&inner)
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { inner, producer: Some(producer), video, frames, audio, audio_frames: Mutex::new(0), overlay: s.overlay, name })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether frames come on the GPU (`push_gpu_frame`) rather than in memory (`push_frame`).
    pub fn on_gpu(&self) -> bool {
        self.frames.is_some()
    }

    /// The newest picture, if it is a VA-API surface (frames on the GPU).
    pub fn newest_surface(&self) -> Option<gst::Sample> {
        let Some(Picture::Surface(surface)) = self.inner.latest.lock().unwrap().clone() else { return None };
        let caps = gst::Caps::from_str(&surface_caps(self.inner.output_size)).ok()?;
        Some(gst::Sample::builder().buffer(&surface).caps(&caps).build())
    }

    /// Draws the part of a captured frame starting at `origin` onto a picture nothing reads,
    /// which becomes the newest. With none free (the encoder far behind) the frame is skipped.
    pub fn push_frame(&self, frame: &FrameRef, origin: (u32, u32)) {
        let size = (self.inner.input.0 as usize * self.inner.input.1 as usize * 4, self.inner.input);
        let mut pictures = self.inner.pictures.lock().unwrap();
        let Some(free) = free_picture(&mut pictures, size.0) else { return };
        let picture = Arc::get_mut(&mut pictures[free]).expect("nothing else holds a free picture");
        copy_region(frame, origin, picture, size.1);
        *self.inner.latest.lock().unwrap() = Some(Picture::Memory(pictures[free].clone()));
        self.inner.ready.store(true, Ordering::Release);
    }

    /// Converts a `size` frame on the GPU (a DMA-BUF) into the newest picture, there. The PipeWire
    /// buffer is let go once converted, so the desktop can draw into it again.
    pub fn push_gpu_frame(&self, frame: &gst::Sample, size: (u32, u32)) {
        let Some(frames) = &self.frames else { return };
        let (w, h) = (size.0 & !1, size.1 & !1);
        if (w, h) == size {
            let _ = frames.push_sample(frame);
            return;
        }
        // An odd width or height is cropped to the even one recorded, as in memory, not scaled to it.
        let (Some(mut buffer), Some(caps)) = (frame.buffer_owned(), frame.caps_owned()) else { return };
        gst_video::VideoCropMeta::add(buffer.make_mut(), (0, 0, w, h));
        let _ = frames.push_sample(&gst::Sample::builder().buffer(&buffer).caps(&caps).build());
    }

    /// Where a `camera`-sized webcam picture goes on the recording, if there is an overlay.
    pub fn overlay_rect(&self, camera: (u32, u32)) -> Option<PixelRect> {
        self.overlay.map(|(corner, size)| camera::overlay_rect(self.inner.input, camera, corner, size))
    }

    /// Takes a webcam picture already scaled to `rect` (see `overlay_rect`).
    pub fn push_overlay(&self, rect: PixelRect, pixels: &[u8]) {
        // Webcam pictures are BGRA; the canvas may hold red first.
        let mut pixels = pixels.to_vec();
        if self.inner.red_first {
            swap_red_blue(&mut pixels);
        }
        *self.inner.overlay.lock().unwrap() = Some((pixels, rect));
    }

    /// Interleaved 16-bit stereo PCM from the mixer.
    pub fn push_audio(&self, pcm: Vec<u8>) {
        let Some((pipeline, src)) = &self.audio else { return };
        if let Some(e) = posted_error(pipeline) {
            self.inner.output.lock().unwrap().record_error(e);
        }
        let frames = (pcm.len() / (CHANNELS * 2)) as u64;
        let mut done = self.audio_frames.lock().unwrap();
        let mut buffer = gst::Buffer::from_mut_slice(pcm);
        {
            let buffer = buffer.get_mut().expect("new buffer");
            buffer.set_pts(gst::ClockTime::from_nseconds(*done * 1_000_000_000 / SAMPLE_RATE as u64));
            buffer.set_duration(gst::ClockTime::from_nseconds(frames * 1_000_000_000 / SAMPLE_RATE as u64));
        }
        *done += frames;
        let _ = src.push_buffer(buffer);
    }

    /// (frames encoded, frame slots skipped because the machine fell behind).
    pub fn stats(&self) -> (u64, u64) {
        (self.inner.encoded.load(Ordering::Relaxed), self.inner.lagged.load(Ordering::Relaxed))
    }

    /// Ends both streams, waiting for the encoders to hand over their last packets; what
    /// was being recorded still has to be finished.
    pub fn finish(mut self) -> Option<PendingFile> {
        self.stop();
        self.inner.output.lock().unwrap().take_file()
    }

    fn stop(&mut self) {
        let Some(producer) = self.producer.take() else { return };
        self.inner.stop.store(true, Ordering::Release);
        let _ = producer.join();
        let _ = self.inner.video_src.end_of_stream();
        if let Some(frames) = &self.frames {
            let _ = frames.end_of_stream();
        }
        if let Some((_, src)) = &self.audio {
            let _ = src.end_of_stream();
        }
        for pipeline in std::iter::once(&self.video).chain(self.audio.as_ref().map(|(p, _)| p)) {
            if let Some(e) = wait_for_eos(pipeline) {
                self.inner.output.lock().unwrap().record_error(e);
            }
            let _ = pipeline.set_state(gst::State::Null);
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.stop();
        if let Some(file) = self.inner.output.lock().unwrap().take_file() {
            let _ = file.finish();
        }
    }
}

fn produce_frames(inner: &Inner) {
    let fps = inner.fps;
    let mut n = 0u64;
    let frame_ns = 1_000_000_000 / fps;
    while !inner.stop.load(Ordering::Acquire) {
        let now = inner.clock.elapsed();
        let due = Duration::from_nanos(n * 1_000_000_000 / fps);
        if !inner.ready.load(Ordering::Acquire) || now < due {
            thread::sleep(due.saturating_sub(now).clamp(Duration::from_millis(1), Duration::from_millis(5)));
            continue;
        }
        // Descheduled for more than a frame: skip ahead rather than drift from real time.
        let current = (now.as_nanos() * fps as u128 / 1_000_000_000) as u64;
        if current > n + 1 {
            inner.lagged.fetch_add(current - n, Ordering::Relaxed);
            n = current;
        }
        // The encoder is falling behind: drop this slot rather than queue without bound.
        if inner.video_src.property::<u64>("current-level-buffers") > QUEUED_FRAMES {
            inner.lagged.fetch_add(1, Ordering::Relaxed);
            n += 1;
            continue;
        }
        let Some(picture) = inner.latest.lock().unwrap().clone() else { continue };
        // The same picture goes out as long as nothing new is captured, shared, not copied; only
        // a webcam on top needs a picture of its own.
        let mut buffer = match (picture, inner.overlay.lock().unwrap().as_ref()) {
            (Picture::Surface(surface), _) => surface.copy(),
            (Picture::Memory(picture), Some((pixels, rect))) => {
                let mut composed = picture.as_ref().clone();
                paste(&mut composed, inner.input.0, *rect, pixels);
                gst::Buffer::from_mut_slice(composed)
            }
            (Picture::Memory(picture), None) => gst::Buffer::from_slice(Shared(picture)),
        };
        if inner.output.lock().unwrap().take_keyframe_request() {
            inner.h264.send_event(gst_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build());
        }
        {
            let buffer = buffer.get_mut().expect("new buffer");
            buffer.set_pts(gst::ClockTime::from_nseconds(n * 1_000_000_000 / fps));
            buffer.set_duration(gst::ClockTime::from_nseconds(frame_ns));
        }
        if let Err(e) = inner.video_src.push_buffer(buffer) {
            inner.output.lock().unwrap().record_error(e.to_string());
            break;
        }
        if let Some(e) = posted_error(&inner.video) {
            inner.output.lock().unwrap().record_error(e);
            break;
        }
        inner.encoded.fetch_add(1, Ordering::Relaxed);
        n += 1;
    }
}

/// Copies the part of `frame` starting at `origin` into `canvas` (`size`, BGRx), padding with
/// black what the frame does not cover (a region past its edge, or a window that shrank).
fn copy_region(frame: &FrameRef, origin: (u32, u32), canvas: &mut [u8], size: (u32, u32)) {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let (ox, oy) = (origin.0 as usize, origin.1 as usize);
    let (fw, fh) = (frame.size.0 as usize, frame.size.1 as usize);
    let width = fw.saturating_sub(ox).min(w);
    for y in 0..h {
        let row = &mut canvas[y * w * 4..(y + 1) * w * 4];
        let src_y = oy + y;
        if src_y < fh && width > 0 {
            let start = src_y * frame.stride + ox * 4;
            row[..width * 4].copy_from_slice(&frame.data[start..start + width * 4]);
            row[width * 4..].fill(0);
        } else {
            row.fill(0);
        }
    }
}

/// A picture of `bytes` nothing else holds (not the newest, not one the encoder still has),
/// made if there are fewer than `MAX_PICTURES`.
fn free_picture(pictures: &mut Vec<Arc<Vec<u8>>>, bytes: usize) -> Option<usize> {
    if let Some(i) = pictures.iter().position(|p| Arc::strong_count(p) == 1) {
        return Some(i);
    }
    (pictures.len() < MAX_PICTURES).then(|| {
        pictures.push(Arc::new(vec![0; bytes]));
        pictures.len() - 1
    })
}

/// Turns BGRA pixels into RGBA, or the other way.
pub(super) fn swap_red_blue(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
}

/// Draws `pixels` (packed, `rect` sized) onto a `width`-pixel-wide picture.
fn paste(picture: &mut [u8], width: u32, rect: PixelRect, pixels: &[u8]) {
    let (x, y, w, h) = (rect.0 as usize, rect.1 as usize, rect.2 as usize, rect.3 as usize);
    let width = width as usize;
    for row in 0..h {
        let start = ((y + row) * width + x) * 4;
        let Some(dst) = picture.get_mut(start..start + w * 4) else { break };
        dst.copy_from_slice(&pixels[row * w * 4..(row + 1) * w * 4]);
    }
}

/// The encoded buffer of `sample` as a packet (times in 100 ns units). Times are taken as
/// running time: GStreamer video encoders shift their timestamps by 1000 hours (so decoding
/// times stay positive) and their segment by the same amount.
fn to_packet(sample: &gst::Sample) -> Option<Packet> {
    let buffer = sample.buffer()?;
    let segment = sample.segment()?.downcast_ref::<gst::ClockTime>()?;
    let time = segment.to_running_time(buffer.pts()?)?;
    let map = buffer.map_readable().ok()?;
    Some(Packet {
        data: map.as_slice().to_vec(),
        time: (time.nseconds() / NS_PER_TICK) as i64,
        duration: buffer.duration().map_or(0, |d| (d.nseconds() / NS_PER_TICK) as i64),
        key: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
    })
}

/// H.264 encoders, hardware first: the factory, the raw format it is given (VA-API only takes
/// NV12, OpenH264 only I420) and a launch fragment named `enc`.
fn h264_candidates(bitrate: u32, fps: u32) -> [(&'static str, &'static str, String); 5] {
    let kbps = (bitrate / 1000).max(1);
    // A keyframe every 2 s: where a replay or a new recording can start.
    let gop = fps * 2;
    [
        ("vah264enc", "NV12", format!("vah264enc name=enc bitrate={kbps} key-int-max={gop} b-frames=0")),
        ("vah264lpenc", "NV12", format!("vah264lpenc name=enc bitrate={kbps} key-int-max={gop} b-frames=0")),
        // NVENC converts RGB itself, on the GPU: no conversion on the CPU at all.
        ("nvh264enc", "BGRA", format!("nvh264enc name=enc bitrate={kbps} gop-size={gop} bframes=0")),
        ("x264enc", "I420", format!("x264enc name=enc bitrate={kbps} key-int-max={gop} tune=zerolatency speed-preset=veryfast")),
        ("openh264enc", "I420", format!("openh264enc name=enc bitrate={bitrate} gop-size={gop} complexity=low")),
    ]
}

/// The first H.264 encoder that is installed and starts: its factory, raw format and launch
/// fragment. Some are installed but fail at their first picture (a driver that does not match
/// the GStreamer version, a size the GPU does not support), so each is tried on one picture of
/// the recording's size.
fn h264_encoder(
    candidates: impl IntoIterator<Item = (&'static str, &'static str, String)>,
    works: impl Fn(&str, &str) -> Result<(), String>,
) -> Result<(String, &'static str, String), String> {
    let mut failed = None;
    for (factory, format, fragment) in candidates {
        if gst::ElementFactory::find(factory).is_none() {
            continue;
        }
        match works(format, &fragment) {
            Ok(()) => return Ok((factory.to_owned(), format, fragment)),
            Err(e) => failed = Some(format!("{factory}: {e}")),
        }
    }
    Err(match failed {
        Some(e) => format!("{}: {e}", tr!("ningún codificador H.264 de GStreamer funciona", "no GStreamer H.264 encoder works")),
        None => tr!("no hay ningún codificador H.264 de GStreamer instalado", "no GStreamer H.264 encoder is installed").to_owned(),
    })
}

/// Raw `format` pictures of `size`. YUV is BT.709, as players expect of HD video; RGB has no
/// matrix to state.
fn raw_caps(format: &str, (width, height): (u32, u32)) -> String {
    let colorimetry = if format.starts_with("BGR") || format.starts_with("RGB") { "" } else { ",colorimetry=bt709" };
    format!("video/x-raw,format={format},width={width},height={height}{colorimetry}")
}

/// VA-API surfaces of `size`, as the GPU path converts frames to.
fn surface_caps((width, height): (u32, u32)) -> String {
    format!("video/x-raw(memory:VAMemory),format=NV12,width={width},height={height},colorimetry=bt709")
}

/// Encodes one `size` test picture with `fragment`, given `format` pictures.
fn encodes(format: &str, fragment: &str, (width, height): (u32, u32)) -> Result<(), String> {
    let pipeline = launch(&format!(
        "videotestsrc num-buffers=1 ! video/x-raw,width={width},height={height} \
         ! videoconvert ! {caps} ! {fragment} ! fakesink",
        caps = raw_caps(format, (width, height)),
    ))?;
    play(&pipeline)?;
    finishes(&pipeline)
}

/// Encodes `frame`, on the GPU, with `fragment` at `size`, converting it as recording does: the
/// GPU may be unable to read the desktop's frames or to encode them.
fn encodes_on_gpu(frame: &gst::Sample, fragment: &str, size: (u32, u32)) -> Result<(), String> {
    let pipeline = launch(&format!("appsrc name=src format=time ! vapostproc ! {} ! {fragment} ! fakesink", surface_caps(size)))?;
    super::gpu::share_display(&pipeline);
    let src = by_name::<gst_app::AppSrc>(&pipeline, "src")?;
    play(&pipeline)?;
    let _ = src.push_sample(frame);
    let _ = src.end_of_stream();
    finishes(&pipeline)
}

/// Waits (at most 5 s) for a pipeline given one picture to end; the error if it did not.
fn finishes(pipeline: &gst::Pipeline) -> Result<(), String> {
    let message = pipeline
        .bus()
        .and_then(|bus| bus.timed_pop_filtered(gst::ClockTime::from_seconds(5), &[gst::MessageType::Eos, gst::MessageType::Error]));
    let _ = pipeline.set_state(gst::State::Null);
    match message.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => Ok(()),
        Some(gst::MessageView::Error(e)) => Err(e.error().to_string()),
        _ => Err(tr!("no terminó en 5 s", "did not finish in 5 s").to_owned()),
    }
}

/// The first available AAC encoder, as a launch fragment (192 kbit/s).
fn aac_encoder() -> Result<String, String> {
    ["fdkaacenc", "avenc_aac", "voaacenc"]
        .into_iter()
        .find(|factory| gst::ElementFactory::find(factory).is_some())
        .map(|factory| format!("{factory} bitrate=192000"))
        .ok_or_else(|| tr!("no hay ningún codificador AAC de GStreamer instalado", "no GStreamer AAC encoder is installed").to_owned())
}

/// AAC-LC, 48 kHz stereo, raw: `codec_data` is the AudioSpecificConfig for exactly that, so the
/// file's audio stream can be declared before the first packet arrives.
fn aac_caps() -> gst::Caps {
    gst::Caps::builder("audio/mpeg")
        .field("mpegversion", 4i32)
        .field("stream-format", "raw")
        .field("channels", CHANNELS as i32)
        .field("rate", SAMPLE_RATE as i32)
        .field("codec_data", gst::Buffer::from_slice([0x11u8, 0x90]))
        .build()
}

fn launch(description: &str) -> Result<gst::Pipeline, String> {
    gst::parse::launch(description)
        .map_err(|e| e.to_string())?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline".to_owned())
}

fn by_name<T: IsA<gst::Element>>(pipeline: &gst::Pipeline, name: &str) -> Result<T, String> {
    pipeline.by_name(name).and_then(|e| e.downcast::<T>().ok()).ok_or_else(|| format!("no element {name}"))
}

/// Starts `pipeline`. If it cannot start, it goes back to NULL (a pipeline must not be dropped
/// in another state), and the error is the element's own message, such as a file that cannot
/// be opened, rather than "failed to change its state".
pub(super) fn play(pipeline: &gst::Pipeline) -> Result<(), String> {
    let Err(e) = pipeline.set_state(gst::State::Playing) else { return Ok(()) };
    let message = posted_error(pipeline);
    let _ = pipeline.set_state(gst::State::Null);
    Err(message.unwrap_or_else(|| e.to_string()))
}

/// The error a running pipeline has posted, if any, without waiting: a pipeline that fails
/// keeps taking buffers without complaint, so this is how the failure is noticed.
pub(super) fn posted_error(pipeline: &gst::Pipeline) -> Option<String> {
    let message = pipeline.bus()?.pop_filtered(&[gst::MessageType::Error])?;
    match message.view() {
        gst::MessageView::Error(e) => Some(e.error().to_string()),
        _ => None,
    }
}

/// Waits (at most 10 s) for a pipeline to drain after end-of-stream; the error if it failed.
fn wait_for_eos(pipeline: &gst::Pipeline) -> Option<String> {
    let bus = pipeline.bus()?;
    let message = bus.timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Eos, gst::MessageType::Error])?;
    match message.view() {
        gst::MessageView::Error(e) => Some(e.error().to_string()),
        _ => None,
    }
}

/// The GStreamer MP4 muxer, fed already encoded packets.
pub struct Mp4 {
    pipeline: gst::Pipeline,
    path: std::path::PathBuf,
    video: gst_app::AppSrc,
    audio: Option<gst_app::AppSrc>,
    start: i64,
    /// Why the pipeline failed, once it has: it will not reach end-of-stream.
    failed: Mutex<Option<String>>,
}

impl Muxer for Mp4 {
    type Format = gst::Caps;

    fn create(path: &Path, video: &gst::Caps, audio: Option<&gst::Caps>, start: i64) -> Result<Self, String> {
        gst::init().map_err(|e| e.to_string())?;
        // The parsers adapt the encoders' caps to what mp4mux accepts. The file is written in
        // half-second fragments, so it plays up to the last one if the process dies; `finish`
        // makes it a regular MP4. (With mp4mux's own `first-moov-then-finalise` mode, a recording
        // cut short did not open.)
        let audio_branch = if audio.is_some() { "appsrc name=a format=time ! aacparse ! queue ! mux." } else { "" };
        let pipeline = launch(&format!(
            "appsrc name=v format=time ! h264parse ! queue \
             ! mp4mux name=mux fragment-duration=500 \
             ! filesink name=file {audio_branch}"
        ))?;
        let video_src = by_name::<gst_app::AppSrc>(&pipeline, "v")?;
        video_src.set_caps(Some(video));
        let audio_src = match audio {
            Some(caps) => {
                let src = by_name::<gst_app::AppSrc>(&pipeline, "a")?;
                src.set_caps(Some(caps));
                Some(src)
            }
            None => None,
        };
        for src in std::iter::once(&video_src).chain(audio_src.as_ref()) {
            // Never block the encoder: the muxer waits for both streams to interleave them.
            src.set_max_bytes(0);
        }
        // filesink takes text: a path that is not valid UTF-8 would be written somewhere else.
        let location = path.to_str().ok_or_else(|| tr!("la ruta de grabación no es UTF-8 válido", "the recording path is not valid UTF-8").to_owned())?;
        by_name::<gst::Element>(&pipeline, "file")?.set_property("location", location);
        play(&pipeline)?;
        Ok(Self { pipeline, path: path.to_owned(), video: video_src, audio: audio_src, start, failed: Mutex::new(None) })
    }

    fn write(&self, video: bool, packet: &Packet) -> Result<(), String> {
        // A failed write (a full disk) stops the muxer, while the packets would keep queueing.
        let mut failed = self.failed.lock().unwrap();
        if let Some(e) = failed.as_ref().cloned().or_else(|| posted_error(&self.pipeline)) {
            return Err(failed.insert(e).clone());
        }
        drop(failed);
        let time = packet.time - self.start;
        let src = if video { &self.video } else { self.audio.as_ref().ok_or("no audio stream")? };
        if time < 0 {
            return Ok(());
        }
        let mut buffer = gst::Buffer::from_slice(packet.data.clone());
        {
            let buffer = buffer.get_mut().expect("new buffer");
            let at = gst::ClockTime::from_nseconds(time as u64 * NS_PER_TICK);
            buffer.set_pts(at);
            buffer.set_dts(at);
            buffer.set_duration(gst::ClockTime::from_nseconds(packet.duration.max(0) as u64 * NS_PER_TICK));
            if !packet.key {
                buffer.set_flags(gst::BufferFlags::DELTA_UNIT);
            }
        }
        src.push_buffer(buffer).map(|_| ()).map_err(|e| e.to_string())
    }

    fn finish(self) -> Result<(), String> {
        if let Some(e) = self.failed.into_inner().unwrap() {
            let _ = self.pipeline.set_state(gst::State::Null);
            return Err(e);
        }
        let _ = self.video.end_of_stream();
        if let Some(audio) = &self.audio {
            let _ = audio.end_of_stream();
        }
        let bus = self.pipeline.bus().ok_or("no bus")?;
        let message = bus.timed_pop_filtered(gst::ClockTime::from_seconds(60), &[gst::MessageType::Eos, gst::MessageType::Error]);
        let _ = self.pipeline.set_state(gst::State::Null);
        match message.as_ref().map(|m| m.view()) {
            Some(gst::MessageView::Eos(_)) => {
                // If this fails the file stays fragmented, which still plays.
                let _ = crate::mp4::defragment(&self.path);
                Ok(())
            }
            Some(gst::MessageView::Error(e)) => Err(e.error().to_string()),
            _ => Err(tr!("el archivo MP4 no terminó de escribirse", "the MP4 file did not finish writing").to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_is_copied_and_what_is_missing_is_black() {
        // A 4×2 frame with 4-byte padding per row; each pixel's bytes are its index.
        let stride = 4 * 4 + 4;
        let mut data = vec![0xEE; stride * 2];
        for y in 0..2 {
            for x in 0..4 {
                data[y * stride + x * 4..y * stride + x * 4 + 4].fill((y * 4 + x) as u8 + 1);
            }
        }
        let frame = FrameRef { data: &data, size: (4, 2), stride };
        // A 3×3 canvas from (2, 1): one row and two columns of it are past the frame.
        let mut canvas = vec![0x55; 3 * 3 * 4];
        copy_region(&frame, (2, 1), &mut canvas, (3, 3));
        let pixel = |x: usize, y: usize| canvas[(y * 3 + x) * 4];
        assert_eq!([pixel(0, 0), pixel(1, 0), pixel(2, 0)], [7, 8, 0]);
        assert_eq!([pixel(0, 1), pixel(1, 1), pixel(2, 1)], [0, 0, 0]);
        assert_eq!([pixel(0, 2), pixel(1, 2), pixel(2, 2)], [0, 0, 0]);
    }

    #[test]
    fn a_picture_is_drawn_on_again_only_once_nothing_reads_it() {
        let mut pictures = Vec::new();
        // Each one drawn on is held (as the newest, then by the encoder) before the next.
        let mut held = Vec::new();
        for _ in 0..MAX_PICTURES {
            let i = free_picture(&mut pictures, 4).unwrap();
            held.push(pictures[i].clone());
        }
        assert_eq!(pictures.len(), MAX_PICTURES);
        assert_eq!(free_picture(&mut pictures, 4), None, "all in use: the frame is skipped");
        drop(held);
        assert_eq!(free_picture(&mut pictures, 4), Some(0), "drawn on again, not made anew");
        assert_eq!(pictures.len(), MAX_PICTURES);
    }

    #[test]
    fn the_webcam_is_pasted_in_place() {
        let mut picture = vec![0u8; 4 * 3 * 4];
        let webcam = vec![9u8; 2 * 2 * 4];
        paste(&mut picture, 4, (1, 1, 2, 2), &webcam);
        let painted: Vec<(usize, usize)> =
            (0..3).flat_map(|y| (0..4).map(move |x| (x, y))).filter(|&(x, y)| picture[(y * 4 + x) * 4] == 9).collect();
        assert_eq!(painted, [(1, 1), (2, 1), (1, 2), (2, 2)]);
    }

    #[test]
    fn packet_times_discount_the_encoders_offset() {
        gst::init().unwrap();
        // GStreamer video encoders shift timestamps and segment by 1000 hours.
        let offset = gst::ClockTime::from_seconds(1000 * 3600);
        let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
        segment.set_start(offset);
        let mut buffer = gst::Buffer::from_slice([1u8, 2, 3]);
        buffer.get_mut().unwrap().set_pts(offset + gst::ClockTime::from_mseconds(500));
        let sample = gst::Sample::builder().buffer(&buffer).segment(&segment).build();
        let packet = to_packet(&sample).unwrap();
        assert_eq!(packet.time, 5_000_000, "0.5 s in 100 ns units");
        assert_eq!(packet.data, [1, 2, 3]);
        assert!(packet.key, "a buffer without DELTA_UNIT is a keyframe");
    }

    #[test]
    fn aac_codec_data_describes_lc_48k_stereo() {
        gst::init().unwrap();
        let caps = aac_caps();
        let s = caps.structure(0).unwrap();
        let data = s.get::<gst::Buffer>("codec_data").unwrap();
        let bytes = data.map_readable().unwrap();
        // object type 2 (LC, 5 bits) | frequency index 3 (48 kHz, 4 bits) | 2 channels (4 bits)
        let config = u16::from_be_bytes([bytes[0], bytes[1]]);
        assert_eq!(config >> 11, 2);
        assert_eq!((config >> 7) & 0xF, 3);
        assert_eq!((config >> 3) & 0xF, CHANNELS as u16);
    }

    #[test]
    fn every_installed_h264_encoder_takes_its_format() {
        gst::init().unwrap();
        for (factory, format, _) in h264_candidates(2_000_000, 30) {
            let Some(found) = gst::ElementFactory::find(factory) else { continue };
            let caps = gst::Caps::builder("video/x-raw").field("format", format).build();
            assert!(found.can_sink_any_caps(&caps), "{factory} does not take {format}");
        }
    }

    #[test]
    fn an_encoder_that_fails_at_its_first_picture_is_skipped() {
        gst::init().unwrap();
        // Core elements stand in for encoders: one rejects the pictures once they flow, one
        // takes them.
        let candidates = [
            ("capsfilter", "NV12", "capsfilter name=enc caps=video/x-raw,format=RGB".to_owned()),
            ("identity", "NV12", "identity name=enc".to_owned()),
        ];
        let (factory, _, _) = h264_encoder(candidates, |format, fragment| encodes(format, fragment, (64, 48))).unwrap();
        assert_eq!(factory, "identity");
    }

    /// `count` BGRA frames of `size` on the GPU, made by VA-API, and the pipeline that made them,
    /// to stop once they are used (GStreamer 1.24 drops its surfaces when it stops); `None` where
    /// VA-API cannot record them (CI). They are VA-API's own memory rather than DMA-BUFs, which it
    /// gives only to elements taking their video meta (appsink does not), but are converted the same way.
    fn gpu_frames(size: (u32, u32), count: usize) -> Option<(gst::Pipeline, Vec<gst::Sample>)> {
        super::super::gpu::formats()?;
        let pipeline = launch(&format!(
            "videotestsrc num-buffers={count} ! video/x-raw,format=BGRA,width={},height={},framerate=30/1 ! vapostproc \
             ! video/x-raw(memory:VAMemory),format=BGRA ! appsink name=sink sync=false",
            size.0, size.1,
        ))
        .unwrap();
        // On the display the recording uses, as the desktop's frames are imported into it.
        super::super::gpu::share_display(&pipeline);
        let sink = by_name::<gst_app::AppSink>(&pipeline, "sink").unwrap();
        play(&pipeline).unwrap();
        let frames = std::iter::from_fn(|| sink.pull_sample().ok()).collect();
        assert_eq!(posted_error(&pipeline), None);
        Some((pipeline, frames))
    }

    #[test]
    fn frames_on_the_gpu_are_recorded() {
        gst::init().unwrap();
        // An odd size is cropped to the even one recorded.
        for (size, input) in [((320, 240), (320, 240)), ((321, 241), (320, 240))] {
            let Some((maker, frames)) = gpu_frames(size, 30) else { return };
            assert_eq!(frames.len(), 30);
            let first = &frames[0];
            let outputs = Arc::new(Mutex::new(Outputs::default()));
            let path = std::env::temp_dir().join(format!("obs-lite-gpu-{}-{}.mp4", std::process::id(), size.0));
            outputs.lock().unwrap().start_file(path.clone());
            let settings = EncoderSettings {
                input,
                output: input,
                fps: 30,
                bitrate: 2_000_000,
                audio: false,
                overlay: None,
                red_first: false,
                gpu: Some(first.clone()),
            };
            let encoder = Encoder::new(settings, Arc::new(Clock::new()), Arc::default(), outputs).unwrap();
            assert!(encoder.on_gpu());
            for frame in &frames {
                encoder.push_gpu_frame(frame, size);
                std::thread::sleep(Duration::from_millis(20));
            }
            let finished = encoder.finish().expect("a recording").finish();
            drop(frames);
            let _ = maker.set_state(gst::State::Null);
            let _ = std::fs::remove_file(&path);
            finished.unwrap_or_else(|e| panic!("{size:?}: {e}"));
        }
    }

    #[test]
    fn a_muxer_that_cannot_write_says_so_while_recording() {
        gst::init().unwrap();
        // Real H.264 from an installed encoder; where there is none (CI), there is nothing to test.
        let Ok((_, format, fragment)) = h264_encoder(h264_candidates(8_000_000, 30), |format, fragment| encodes(format, fragment, (640, 360)))
        else {
            return;
        };
        if ["mp4mux", "h264parse"].iter().any(|f| gst::ElementFactory::find(f).is_none()) {
            return;
        }
        let source = launch(&format!(
            "videotestsrc num-buffers=60 ! video/x-raw,width=640,height=360,framerate=30/1 ! videoconvert \
             ! {caps} ! {fragment} ! h264parse \
             ! video/x-h264,stream-format=avc,alignment=au ! appsink name=sink sync=false",
            caps = raw_caps(format, (640, 360)),
        ))
        .unwrap();
        let sink = by_name::<gst_app::AppSink>(&source, "sink").unwrap();
        play(&source).unwrap();
        let (mut packets, mut caps) = (Vec::new(), None);
        while let Ok(sample) = sink.pull_sample() {
            caps = sample.caps().map(|c| c.to_owned());
            packets.extend(to_packet(&sample));
        }
        let _ = source.set_state(gst::State::Null);
        // Every write to /dev/full fails as a full disk does (once filesink's 64 KB buffer fills).
        let mp4 = Mp4::create(Path::new("/dev/full"), &caps.unwrap(), None, packets[0].time).unwrap();
        for packet in &packets {
            let _ = mp4.write(true, packet);
        }
        std::thread::sleep(Duration::from_millis(500));
        assert!(mp4.write(true, &packets[0]).is_err(), "the failed write must reach the recording");
        let finishing = std::time::Instant::now();
        assert!(mp4.finish().is_err());
        assert!(finishing.elapsed() < Duration::from_secs(5), "a failed muxer does not wait for its end of stream");
    }
}
