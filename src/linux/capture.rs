//! Screen/window capture: the ScreenCast portal gives a PipeWire stream, which GStreamer turns
//! into BGRx frames for the preview, screenshots and the encoder.

use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::encoder::{Encoder, EncoderSettings, FrameRef, Outputs};
use super::portal;
use crate::capture::{
    AudioApp, PREVIEW_DEFAULT_WIDTH, PreviewFrame, RecordConfig, SessionOptions, Source, SourceId, bgra_to_rgba, downscale,
};
use crate::i18n::tr;
use crate::media::Clock;

type Size = (u32, u32);

/// The portal lets the user choose the screen or window, so there is a single source.
pub fn list_sources() -> Vec<Source> {
    vec![Source { id: SourceId::Portal, label: tr!("Pantalla o ventana", "Screen or window").to_owned() }]
}

/// Capturing one application's audio is not available on Linux.
pub fn list_audio_apps() -> Vec<AudioApp> {
    Vec::new()
}

/// Forgets the screen or window chosen in the portal, so the next session asks again.
pub fn forget_choice() {
    portal::forget();
}

/// State shared between the capture thread, the audio mixer and the UI (as on Windows).
pub struct Shared {
    pub preview: Mutex<Option<PreviewFrame>>,
    /// Width in screen pixels the preview is drawn at; frames are scaled down to it, not further.
    pub preview_width: AtomicUsize,
    /// Set while the window is not shown (minimized, or covered where the system says so):
    /// nothing would draw the preview, so no preview pictures are made.
    pub preview_hidden: AtomicBool,
    pub frames: AtomicU64,
    /// Drives video and audio timestamps; pausing it pauses the recording.
    pub clock: Arc<Clock>,
    /// Set once the encoder is running and the clock has started.
    pub encoder_ready: Arc<AtomicBool>,
    pub encoder: Mutex<Option<Encoder>>,
    /// The recording file and replay buffer; they can be turned on before the encoder exists.
    pub outputs: Arc<Mutex<Outputs>>,
    /// Capture size and encoded size, known once the encoder exists.
    pub sizes: Mutex<Option<(Size, Size)>>,
    /// The newest frame, for screenshots: PipeWire only sends frames when something changes.
    last_frame: Mutex<Option<gst::Sample>>,
    finished: AtomicBool,
    error: Mutex<Option<String>>,
    repaint: Option<eframe::egui::Context>,
}

/// What the capture pipeline does with each frame.
struct Handler {
    shared: Arc<Shared>,
    record: Option<RecordConfig>,
    fps: u32,
    preview_interval: Option<Duration>,
    last_preview: Option<Instant>,
    /// Top-left corner of the recorded region, fixed when the encoder is created.
    origin: (u32, u32),
}

impl Handler {
    fn on_frame(&mut self, sample: &gst::Sample) -> Result<(), String> {
        let shared = self.shared.clone();
        shared.frames.fetch_add(1, Ordering::Relaxed);
        *shared.last_frame.lock().unwrap() = Some(sample.clone());
        let (Some(buffer), Some(size)) = (sample.buffer(), frame_size(sample)) else { return Ok(()) };
        let map = buffer.map_readable().map_err(|e| e.to_string())?;
        let frame = FrameRef { data: map.as_slice(), size, stride: map.size() / size.1.max(1) as usize };

        if let Some(rec) = &self.record {
            let mut encoder = shared.encoder.lock().unwrap();
            if encoder.is_none() {
                let input = match rec.region.and_then(|r| r.fit(size)) {
                    Some(r) => {
                        self.origin = (r.x, r.y);
                        (r.width, r.height)
                    }
                    None => (size.0 & !1, size.1 & !1),
                };
                let output = rec.resolution.apply(input);
                let settings = EncoderSettings {
                    input,
                    output,
                    fps: self.fps,
                    bitrate: rec.quality.bitrate(output.0, output.1, self.fps),
                    audio: rec.audio,
                    overlay: rec.overlay,
                };
                *encoder = Some(Encoder::new(settings, shared.clock.clone(), shared.encoder_ready.clone(), shared.outputs.clone())?);
                *shared.sizes.lock().unwrap() = Some((input, output));
            }
            if let Some(enc) = encoder.as_ref() {
                enc.push_frame(&frame, self.origin);
            }
        }

        if let Some(interval) = self.preview_interval
            && !shared.preview_hidden.load(Ordering::Relaxed)
            && self.last_preview.is_none_or(|t| t.elapsed() >= interval)
        {
            self.last_preview = Some(Instant::now());
            let (w, h) = (size.0 as usize, size.1 as usize);
            let pw = shared.preview_width.load(Ordering::Relaxed).clamp(64.min(w), w);
            let ph = (h * pw / w).max(1);
            let rgba = downscale(frame.data, (w, h), frame.stride, (pw, ph));
            let mut preview = shared.preview.lock().unwrap();
            let seq = preview.as_ref().map_or(0, |p| p.seq + 1);
            *preview = Some(PreviewFrame { size: [pw, ph], rgba, seq });
            drop(preview);
            if let Some(ctx) = &shared.repaint {
                crate::capture::repaint_for_picture(ctx);
            }
        }
        Ok(())
    }
}

fn frame_size(sample: &gst::Sample) -> Option<Size> {
    let s = sample.caps()?.structure(0)?;
    Some((s.get::<i32>("width").ok()? as u32, s.get::<i32>("height").ok()? as u32))
}

pub struct Session {
    pub shared: Arc<Shared>,
    /// Asks the portal and then runs the pipeline; the portal may show a dialog, so this
    /// does not run on the UI thread.
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl Session {
    pub fn start(opts: SessionOptions, repaint: Option<eframe::egui::Context>) -> Result<Self, String> {
        // Only the portal's source exists here (a Windows source cannot be captured).
        if opts.source != SourceId::Portal {
            return Err(tr!("esta fuente no está disponible", "this source is not available").to_owned());
        }
        gst::init().map_err(|e| e.to_string())?;
        let shared = Arc::new(Shared {
            preview: Mutex::new(None),
            preview_width: AtomicUsize::new(PREVIEW_DEFAULT_WIDTH),
            preview_hidden: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            clock: Arc::new(Clock::new()),
            encoder_ready: Arc::new(AtomicBool::new(false)),
            encoder: Mutex::new(None),
            outputs: Arc::default(),
            sizes: Mutex::new(None),
            last_frame: Mutex::new(None),
            finished: AtomicBool::new(false),
            error: Mutex::new(None),
            repaint,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("capture".into())
            .spawn({
                let (shared, stop) = (shared.clone(), stop.clone());
                move || {
                    if let Err(e) = run(opts, &shared, &stop) {
                        *shared.error.lock().unwrap() = Some(e);
                    }
                    shared.finished.store(true, Ordering::Release);
                    if let Some(ctx) = &shared.repaint {
                        ctx.request_repaint();
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { shared, thread: Some(thread), stop })
    }

    /// The capture ended: the source went away, sharing was stopped, or the portal failed.
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }

    /// Size of the source, once a frame has arrived.
    pub fn source_size(&self) -> Option<(u32, u32)> {
        self.shared.last_frame.lock().unwrap().as_ref().and_then(frame_size)
    }

    /// The newest captured frame as RGBA pixels: (width, height, pixels).
    pub fn snapshot(&self) -> Result<(u32, u32, Vec<u8>), String> {
        let last = self.shared.last_frame.lock().unwrap();
        let none = || tr!("todavía no hay imagen de la fuente", "there is no image from the source yet").to_owned();
        let sample = last.as_ref().ok_or_else(none)?;
        let (w, h) = frame_size(sample).ok_or_else(none)?;
        let map = sample.buffer().ok_or_else(none)?.map_readable().map_err(|e| e.to_string())?;
        Ok((w, h, bgra_to_rgba(map.as_slice(), w as usize, h as usize, map.size() / h.max(1) as usize)))
    }

    /// Stops capturing and hands back the encoder (if recording) so it can be finalized.
    /// The error is whatever made the capture end, if anything.
    pub fn stop(mut self) -> (Option<Encoder>, Option<String>) {
        self.halt();
        let error = self.shared.error.lock().unwrap().take();
        (self.shared.encoder.lock().unwrap().take(), error)
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Portal, then pipeline, until asked to stop or the stream ends. A portal that crashes ends the
/// stream without closing the session (COSMIC's did, every few seconds to minutes): then a new
/// session is started, from the remembered choice so without the dialog, and capture goes on.
fn run(opts: SessionOptions, shared: &Arc<Shared>, stop: &AtomicBool) -> Result<(), String> {
    let handler = Arc::new(Mutex::new(Handler {
        shared: shared.clone(),
        record: opts.record,
        fps: opts.fps,
        preview_interval: (opts.preview_fps > 0).then(|| Duration::from_secs_f64(1.0 / opts.preview_fps as f64)),
        last_preview: None,
        origin: (0, 0),
    }));
    loop {
        let mut cast = portal::start(opts.cursor)?;
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let frames = shared.frames.load(Ordering::Relaxed);
        let result = stream(&cast, &handler, stop);
        if stop.load(Ordering::Acquire) {
            return result;
        }
        // The desktop's `Closed` can come just after the end of the stream. A session that sent
        // no frames is not started again, so a source that cannot work does not loop.
        thread::sleep(Duration::from_millis(500));
        if cast.was_closed() || shared.frames.load(Ordering::Relaxed) == frames {
            return result;
        }
    }
}

/// Runs the pipeline for one portal session until asked to stop or the stream ends.
fn stream(cast: &portal::Cast, handler: &Arc<Mutex<Handler>>, stop: &AtomicBool) -> Result<(), String> {
    let pipeline = gst::parse::launch(&format!(
        "pipewiresrc fd={fd} path={node} do-timestamp=true keepalive-time=1000 \
         ! videoconvert ! video/x-raw,format=BGRx ! appsink name=sink max-buffers=2 drop=true sync=false",
        fd = cast.fd.as_raw_fd(),
        node = cast.node,
    ))
    .map_err(|e| e.to_string())?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "not a pipeline".to_owned())?;
    let sink = pipeline.by_name("sink").and_then(|e| e.downcast::<gst_app::AppSink>().ok()).ok_or("no sink")?;
    let failed = Arc::new(Mutex::new(None::<String>));
    let handler_failed = failed.clone();
    let handler = handler.clone();
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Err(e) = handler.lock().unwrap().on_frame(&sample) {
                    *handler_failed.lock().unwrap() = Some(e);
                    return Err(gst::FlowError::Error);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
    let bus = pipeline.bus().ok_or("no bus")?;
    let mut result = Ok(());
    while !stop.load(Ordering::Acquire) {
        let Some(message) = bus.timed_pop_filtered(gst::ClockTime::from_mseconds(100), &[gst::MessageType::Eos, gst::MessageType::Error]) else {
            continue;
        };
        if let gst::MessageView::Error(e) = message.view() {
            result = Err(failed.lock().unwrap().take().unwrap_or_else(|| e.error().to_string()));
        }
        // End of stream: the window was closed, sharing was stopped from the desktop, or the
        // portal went away.
        break;
    }
    let _ = pipeline.set_state(gst::State::Null);
    result
}
