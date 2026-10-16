//! Screen/window capture through Windows Graphics Capture.

use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings,
    SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::config::{Quality, Resolution};
use crate::encoder::{Clock, Encoder, EncoderSettings};
use crate::i18n::tr;

type BoxError = Box<dyn Error + Send + Sync>;
type Size = (u32, u32);

const PREVIEW_MAX_WIDTH: usize = 640;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceId {
    Monitor(isize),
    Window(isize),
}

#[derive(Clone, Debug)]
pub struct Source {
    pub id: SourceId,
    pub label: String,
}

/// Lists capturable monitors and top-level windows (excluding our own).
pub fn list_sources() -> Vec<Source> {
    let mut out = Vec::new();
    for (i, m) in Monitor::enumerate().unwrap_or_default().into_iter().enumerate() {
        let (w, h) = (m.width().unwrap_or(0), m.height().unwrap_or(0));
        let name = m.device_string().unwrap_or_default();
        out.push(Source {
            id: SourceId::Monitor(m.as_raw_hmonitor() as isize),
            label: tr!("Pantalla {} ({}×{}) {}", "Display {} ({}×{}) {}", i + 1, w, h, name).trim_end().to_string(),
        });
    }
    let own_pid = std::process::id();
    for w in Window::enumerate().unwrap_or_default() {
        let Ok(title) = w.title() else { continue };
        if title.trim().is_empty() || w.process_id().ok() == Some(own_pid) {
            continue;
        }
        let process = w.process_name().unwrap_or_default();
        out.push(Source { id: SourceId::Window(w.as_raw_hwnd() as isize), label: format!("{title} — {process}") });
    }
    out
}

#[derive(Clone)]
pub struct RecordConfig {
    pub path: PathBuf,
    pub quality: Quality,
    pub resolution: Resolution,
    pub audio: bool,
}

pub struct PreviewFrame {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
    pub seq: u64,
}

/// State shared between the capture thread, the audio mixer and the UI.
pub struct Shared {
    pub preview: Mutex<Option<PreviewFrame>>,
    pub frames: AtomicU64,
    /// Drives video and audio timestamps; pausing it pauses the recording.
    pub clock: Arc<Clock>,
    /// Set once the encoder is pulling samples and the clock has started.
    pub encoder_ready: Arc<AtomicBool>,
    pub encoder: Mutex<Option<Encoder>>,
    /// Capture size and encoded size, known once the encoder exists.
    pub sizes: Mutex<Option<(Size, Size)>>,
    repaint: Option<eframe::egui::Context>,
}

struct Flags {
    shared: Arc<Shared>,
    record: Option<RecordConfig>,
    fps: u32,
    preview_interval: Option<Duration>,
}

struct Handler {
    flags: Flags,
    last_preview: Option<Instant>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { flags: ctx.flags, last_preview: None })
    }

    fn on_frame_arrived(&mut self, frame: &mut Frame, _control: InternalCaptureControl) -> Result<(), Self::Error> {
        let shared = &self.flags.shared;
        shared.frames.fetch_add(1, Ordering::Relaxed);

        if let Some(rec) = &self.flags.record {
            let mut encoder = shared.encoder.lock().unwrap();
            if encoder.is_none() {
                let input = (frame.width() & !1, frame.height() & !1);
                let output = rec.resolution.apply(input);
                let fps = self.flags.fps;
                let created = Encoder::new(
                    frame.device(),
                    EncoderSettings {
                        path: rec.path.clone(),
                        input,
                        output,
                        fps,
                        bitrate: rec.quality.bitrate(output.0, output.1, fps),
                        audio: rec.audio,
                    },
                    shared.clock.clone(),
                    shared.encoder_ready.clone(),
                )?;
                *encoder = Some(created);
                *shared.sizes.lock().unwrap() = Some((input, output));
            }
            if let Some(enc) = encoder.as_ref() {
                enc.push_frame(frame);
            }
        }

        if let Some(interval) = self.flags.preview_interval
            && self.last_preview.is_none_or(|t| t.elapsed() >= interval)
        {
            self.last_preview = Some(Instant::now());
            self.update_preview(frame)?;
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        if let Some(ctx) = &self.flags.shared.repaint {
            ctx.request_repaint();
        }
        Ok(())
    }
}

impl Handler {
    /// Nearest-neighbour downscale of the BGRA frame into a small RGBA preview.
    fn update_preview(&mut self, frame: &mut Frame) -> Result<(), BoxError> {
        let mut buffer = frame.buffer()?;
        let (w, h, pitch) = (buffer.width() as usize, buffer.height() as usize, buffer.row_pitch() as usize);
        if w == 0 || h == 0 {
            return Ok(());
        }
        let scale = (w as f32 / PREVIEW_MAX_WIDTH as f32).max(1.0);
        let (pw, ph) = (((w as f32 / scale) as usize).max(1), ((h as f32 / scale) as usize).max(1));
        let raw = buffer.as_raw_buffer();
        let mut rgba = vec![0u8; pw * ph * 4];
        for y in 0..ph {
            let row = &raw[((y as f32 * scale) as usize).min(h - 1) * pitch..];
            for x in 0..pw {
                let sx = ((x as f32 * scale) as usize).min(w - 1) * 4;
                let d = (y * pw + x) * 4;
                rgba[d..d + 4].copy_from_slice(&[row[sx + 2], row[sx + 1], row[sx], 255]);
            }
        }
        let shared = &self.flags.shared;
        let mut preview = shared.preview.lock().unwrap();
        let seq = preview.as_ref().map_or(0, |p| p.seq + 1);
        *preview = Some(PreviewFrame { size: [pw, ph], rgba, seq });
        drop(preview);
        if let Some(ctx) = &shared.repaint {
            ctx.request_repaint();
        }
        Ok(())
    }
}

pub struct SessionOptions {
    pub source: SourceId,
    pub fps: u32,
    pub cursor: bool,
    /// Preview updates per second; 0 disables the preview.
    pub preview_fps: u32,
    pub record: Option<RecordConfig>,
}

pub struct Session {
    control: Option<CaptureControl<Handler, BoxError>>,
    pub shared: Arc<Shared>,
}

impl Session {
    pub fn start(opts: SessionOptions, repaint: Option<eframe::egui::Context>) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            preview: Mutex::new(None),
            frames: AtomicU64::new(0),
            clock: Arc::new(Clock::new()),
            encoder_ready: Arc::new(AtomicBool::new(false)),
            encoder: Mutex::new(None),
            sizes: Mutex::new(None),
            repaint,
        });
        let control = match start_with_border(&opts, &shared, DrawBorderSettings::WithoutBorder) {
            Ok(c) => c,
            // Borderless capture needs Windows 11; fall back to the default border.
            Err(_) => start_with_border(&opts, &shared, DrawBorderSettings::Default)?,
        };
        Ok(Self { control: Some(control), shared })
    }

    /// The capture thread exited (source closed or handler error).
    pub fn is_finished(&self) -> bool {
        self.control.as_ref().is_none_or(CaptureControl::is_finished)
    }

    /// Stops capturing and hands back the encoder (if recording) so it can be finalized.
    /// The error is whatever made the capture thread exit, if anything.
    pub fn stop(mut self) -> (Option<Encoder>, Option<String>) {
        let mut error = None;
        if let Some(control) = self.control.take() {
            let finished = control.is_finished();
            let result = if finished { control.wait() } else { control.stop() };
            if let Err(e) = result {
                error = Some(e.to_string());
            }
        }
        (self.shared.encoder.lock().unwrap().take(), error)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }
    }
}

fn start_with_border(
    opts: &SessionOptions,
    shared: &Arc<Shared>,
    border: DrawBorderSettings,
) -> Result<CaptureControl<Handler, BoxError>, String> {
    let flags = Flags {
        shared: shared.clone(),
        record: opts.record.clone(),
        fps: opts.fps,
        preview_interval: (opts.preview_fps > 0).then(|| Duration::from_secs_f64(1.0 / opts.preview_fps as f64)),
    };
    let cursor = if opts.cursor { CursorCaptureSettings::WithCursor } else { CursorCaptureSettings::WithoutCursor };
    let interval = MinimumUpdateIntervalSettings::Custom(Duration::from_secs_f64(1.0 / opts.fps.max(1) as f64));
    let result = match opts.source {
        SourceId::Monitor(h) => Handler::start_free_threaded(Settings::new(
            Monitor::from_raw_hmonitor(h as *mut _),
            cursor,
            border,
            SecondaryWindowSettings::Default,
            interval,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        )),
        SourceId::Window(h) => Handler::start_free_threaded(Settings::new(
            Window::from_raw_hwnd(h as *mut _),
            cursor,
            border,
            SecondaryWindowSettings::Default,
            interval,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        )),
    };
    result.map_err(|e| e.to_string())
}
