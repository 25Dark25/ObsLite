//! H.264 + AAC encoding with GStreamer, producing constant-frame-rate packets for the shared
//! outputs (`media`).
//!
//! PipeWire only delivers frames when the picture changes, so captured frames (and the webcam)
//! are drawn into a canvas in memory; like on Windows, a producer thread hands the canvas to
//! the encoder every 1/fps of recording time. The best available H.264 encoder is used:
//! VA-API (Intel/AMD), NVENC, then x264 or OpenH264 in software.

use std::path::Path;
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
}

/// A captured picture: BGRx rows `stride` bytes apart.
pub struct FrameRef<'a> {
    pub data: &'a [u8],
    pub size: (u32, u32),
    pub stride: usize,
}

struct Inner {
    /// The picture being recorded, BGRx at the input size.
    canvas: Mutex<Vec<u8>>,
    /// The webcam picture, already scaled, and where it goes.
    overlay: Mutex<Option<(Vec<u8>, PixelRect)>>,
    ready: AtomicBool,
    clock: Arc<Clock>,
    encoded: AtomicU64,
    lagged: AtomicU64,
    stop: AtomicBool,
    fps: u64,
    input: (u32, u32),
    video_src: gst_app::AppSrc,
    /// The H.264 encoder element, to ask it for keyframes.
    h264: gst::Element,
    output: Arc<Mutex<Outputs>>,
}

pub struct Encoder {
    inner: Arc<Inner>,
    producer: Option<JoinHandle<()>>,
    video: gst::Pipeline,
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
        let (name, format, h264) = h264_encoder(s.bitrate, fps)?;
        let (iw, ih) = s.input;
        let (ow, oh) = s.output;
        let frame_bytes = iw as u64 * ih as u64 * 4;
        let video = launch(&format!(
            "appsrc name=src is-live=true format=time block=false max-bytes={max} \
               caps=video/x-raw,format=BGRx,width={iw},height={ih},framerate={fps}/1 \
             ! videoconvert n-threads=0 ! videoscale \
             ! video/x-raw,format={format},width={ow},height={oh},colorimetry=bt709 \
             ! {h264} ! h264parse ! video/x-h264,stream-format=avc,alignment=au \
             ! appsink name=sink sync=false",
            max = frame_bytes * (QUEUED_FRAMES + 2),
        ))?;
        let video_src = by_name::<gst_app::AppSrc>(&video, "src")?;
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
            canvas: Mutex::new(vec![0; frame_bytes as usize]),
            overlay: Mutex::new(None),
            ready: AtomicBool::new(false),
            clock,
            encoded: AtomicU64::new(0),
            lagged: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            fps: fps as u64,
            input: s.input,
            video_src,
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

        for pipeline in std::iter::once(&video).chain(audio.as_ref().map(|(p, _)| p)) {
            pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
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
        Ok(Self { inner, producer: Some(producer), video, audio, audio_frames: Mutex::new(0), overlay: s.overlay, name })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Draws the part of a captured frame starting at `origin` onto the canvas.
    pub fn push_frame(&self, frame: &FrameRef, origin: (u32, u32)) {
        let mut canvas = self.inner.canvas.lock().unwrap();
        copy_region(frame, origin, &mut canvas, self.inner.input);
        self.inner.ready.store(true, Ordering::Release);
    }

    /// Where a `camera`-sized webcam picture goes on the recording, if there is an overlay.
    pub fn overlay_rect(&self, camera: (u32, u32)) -> Option<PixelRect> {
        self.overlay.map(|(corner, size)| camera::overlay_rect(self.inner.input, camera, corner, size))
    }

    /// Takes a webcam picture already scaled to `rect` (see `overlay_rect`).
    pub fn push_overlay(&self, rect: PixelRect, pixels: &[u8]) {
        *self.inner.overlay.lock().unwrap() = Some((pixels.to_vec(), rect));
    }

    /// Interleaved 16-bit stereo PCM from the mixer.
    pub fn push_audio(&self, pcm: Vec<u8>) {
        let Some((_, src)) = &self.audio else { return };
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
        if let Some((_, src)) = &self.audio {
            let _ = src.end_of_stream();
        }
        for pipeline in std::iter::once(&self.video).chain(self.audio.as_ref().map(|(p, _)| p)) {
            wait_for_eos(pipeline);
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
    let limit = inner.canvas.lock().unwrap().len() as u64 * QUEUED_FRAMES;
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
        if inner.video_src.current_level_bytes() > limit {
            inner.lagged.fetch_add(1, Ordering::Relaxed);
            n += 1;
            continue;
        }
        let mut picture = inner.canvas.lock().unwrap().clone();
        if let Some((pixels, rect)) = inner.overlay.lock().unwrap().as_ref() {
            paste(&mut picture, inner.input.0, *rect, pixels);
        }
        if inner.output.lock().unwrap().take_keyframe_request() {
            inner.h264.send_event(gst_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build());
        }
        let mut buffer = gst::Buffer::from_mut_slice(picture);
        {
            let buffer = buffer.get_mut().expect("new buffer");
            buffer.set_pts(gst::ClockTime::from_nseconds(n * 1_000_000_000 / fps));
            buffer.set_duration(gst::ClockTime::from_nseconds(frame_ns));
        }
        if let Err(e) = inner.video_src.push_buffer(buffer) {
            inner.output.lock().unwrap().record_error(e.to_string());
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

/// H.264 encoders, hardware first: the factory, the raw format it takes (VA-API and NVENC only
/// take NV12, OpenH264 only I420) and a launch fragment named `enc`.
fn h264_candidates(bitrate: u32, fps: u32) -> [(&'static str, &'static str, String); 5] {
    let kbps = (bitrate / 1000).max(1);
    // A keyframe every 2 s: where a replay or a new recording can start.
    let gop = fps * 2;
    [
        ("vah264enc", "NV12", format!("vah264enc name=enc bitrate={kbps} key-int-max={gop} b-frames=0")),
        ("vah264lpenc", "NV12", format!("vah264lpenc name=enc bitrate={kbps} key-int-max={gop} b-frames=0")),
        ("nvh264enc", "NV12", format!("nvh264enc name=enc bitrate={kbps} gop-size={gop} bframes=0")),
        ("x264enc", "I420", format!("x264enc name=enc bitrate={kbps} key-int-max={gop} tune=zerolatency speed-preset=veryfast")),
        ("openh264enc", "I420", format!("openh264enc name=enc bitrate={bitrate} gop-size={gop} complexity=low")),
    ]
}

/// The first available H.264 encoder: its factory, raw format and launch fragment.
fn h264_encoder(bitrate: u32, fps: u32) -> Result<(String, &'static str, String), String> {
    h264_candidates(bitrate, fps)
        .into_iter()
        .find(|(factory, _, _)| gst::ElementFactory::find(factory).is_some())
        .map(|(factory, format, fragment)| (factory.to_owned(), format, fragment))
        .ok_or_else(|| tr!("no hay ningún codificador H.264 de GStreamer instalado", "no GStreamer H.264 encoder is installed").to_owned())
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

/// Waits (at most 10 s) for a pipeline to drain after end-of-stream.
fn wait_for_eos(pipeline: &gst::Pipeline) {
    if let Some(bus) = pipeline.bus() {
        let _ = bus.timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Eos, gst::MessageType::Error]);
    }
}

/// The GStreamer MP4 muxer, fed already encoded packets.
pub struct Mp4 {
    pipeline: gst::Pipeline,
    path: std::path::PathBuf,
    video: gst_app::AppSrc,
    audio: Option<gst_app::AppSrc>,
    start: i64,
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
        by_name::<gst::Element>(&pipeline, "file")?.set_property("location", path.to_string_lossy().as_ref());
        pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
        Ok(Self { pipeline, path: path.to_owned(), video: video_src, audio: audio_src, start })
    }

    fn write(&self, video: bool, packet: &Packet) -> Result<(), String> {
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
}
