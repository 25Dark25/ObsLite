//! Screen/window capture through Windows Graphics Capture.

use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
};
use windows::core::Interface;
use windows_capture::window::Window;

use crate::camera::{CameraSize, Corner};
use crate::config::{Quality, Region, Resolution};
use crate::encoder::{Clock, Com, Encoder, EncoderSettings, Outputs};
use crate::i18n::tr;

type BoxError = Box<dyn Error + Send + Sync>;
type Size = (u32, u32);

/// Preview width until the UI reports how wide the preview is drawn.
const PREVIEW_DEFAULT_WIDTH: usize = 640;

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

/// An application whose audio can be captured on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioApp {
    pub pid: u32,
    /// Executable name, e.g. "Spotify.exe": what the settings remember.
    pub exe: String,
    pub title: String,
}

/// Applications with a visible window (one entry per executable), excluding obs-lite.
pub fn list_audio_apps() -> Vec<AudioApp> {
    let own_pid = std::process::id();
    let mut apps: Vec<AudioApp> = Vec::new();
    for w in Window::enumerate().unwrap_or_default() {
        let (Ok(title), Ok(pid), Ok(exe)) = (w.title(), w.process_id(), w.process_name()) else { continue };
        if title.trim().is_empty() || pid == own_pid || apps.iter().any(|a| a.exe.eq_ignore_ascii_case(&exe)) {
            continue;
        }
        apps.push(AudioApp { pid, exe, title });
    }
    apps.sort_by_key(|a| a.exe.to_lowercase());
    apps
}

#[derive(Clone)]
pub struct RecordConfig {
    pub quality: Quality,
    pub resolution: Resolution,
    pub audio: bool,
    pub region: Option<Region>,
    pub overlay: Option<(Corner, CameraSize)>,
}

/// GPU copy of the newest frame. Windows Graphics Capture only delivers frames when
/// something changes, so a screenshot reads this instead of waiting for the next one.
pub struct LastFrame {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
    size: Size,
}

pub struct PreviewFrame {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
    pub seq: u64,
}

/// State shared between the capture thread, the audio mixer and the UI.
pub struct Shared {
    pub preview: Mutex<Option<PreviewFrame>>,
    /// Width in screen pixels the preview is drawn at; frames are scaled down to it, not further.
    pub preview_width: AtomicUsize,
    pub frames: AtomicU64,
    /// Drives video and audio timestamps; pausing it pauses the recording.
    pub clock: Arc<Clock>,
    /// Set once the encoder is pulling samples and the clock has started.
    pub encoder_ready: Arc<AtomicBool>,
    pub encoder: Mutex<Option<Encoder>>,
    /// The recording file and replay buffer; they can be turned on before the encoder exists.
    pub outputs: Arc<Mutex<Outputs>>,
    /// Capture size and encoded size, known once the encoder exists.
    pub sizes: Mutex<Option<(Size, Size)>>,
    last_frame: Mutex<Option<Com<LastFrame>>>,
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
    /// Top-left corner of the recorded region, fixed when the encoder is created.
    origin: (u32, u32),
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { flags: ctx.flags, last_preview: None, origin: (0, 0) })
    }

    fn on_frame_arrived(&mut self, frame: &mut Frame, _control: InternalCaptureControl) -> Result<(), Self::Error> {
        let shared = self.flags.shared.clone();
        shared.frames.fetch_add(1, Ordering::Relaxed);
        keep_last_frame(&shared, frame)?;

        if let Some(rec) = &self.flags.record {
            let mut encoder = shared.encoder.lock().unwrap();
            if encoder.is_none() {
                let full = (frame.width(), frame.height());
                let input = match rec.region.and_then(|r| r.fit(full)) {
                    Some(r) => {
                        self.origin = (r.x, r.y);
                        (r.width, r.height)
                    }
                    None => (full.0 & !1, full.1 & !1),
                };
                let output = rec.resolution.apply(input);
                let fps = self.flags.fps;
                let created = Encoder::new(
                    frame.device(),
                    EncoderSettings {
                        input,
                        output,
                        fps,
                        bitrate: rec.quality.bitrate(output.0, output.1, fps),
                        audio: rec.audio,
                        overlay: rec.overlay,
                    },
                    shared.clock.clone(),
                    shared.encoder_ready.clone(),
                    shared.outputs.clone(),
                )?;
                *encoder = Some(created);
                *shared.sizes.lock().unwrap() = Some((input, output));
            }
            if let Some(enc) = encoder.as_ref() {
                enc.push_frame(frame, self.origin);
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
    /// Scales the frame down to the size the preview is drawn at.
    fn update_preview(&mut self, frame: &mut Frame) -> Result<(), BoxError> {
        let mut buffer = frame.buffer()?;
        let (w, h, pitch) = (buffer.width() as usize, buffer.height() as usize, buffer.row_pitch() as usize);
        if w == 0 || h == 0 {
            return Ok(());
        }
        let shared = &self.flags.shared;
        let pw = shared.preview_width.load(Ordering::Relaxed).clamp(64.min(w), w);
        let ph = (h * pw / w).max(1);
        let rgba = downscale(buffer.as_raw_buffer(), (w, h), pitch, (pw, ph));
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
            preview_width: AtomicUsize::new(PREVIEW_DEFAULT_WIDTH),
            frames: AtomicU64::new(0),
            clock: Arc::new(Clock::new()),
            encoder_ready: Arc::new(AtomicBool::new(false)),
            encoder: Mutex::new(None),
            outputs: Arc::default(),
            sizes: Mutex::new(None),
            last_frame: Mutex::new(None),
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

    /// Size of the source, once a frame has arrived.
    pub fn source_size(&self) -> Option<(u32, u32)> {
        self.shared.last_frame.lock().unwrap().as_ref().map(|l| l.0.size)
    }

    /// The newest captured frame as RGBA pixels: (width, height, pixels).
    pub fn snapshot(&self) -> Result<(u32, u32, Vec<u8>), String> {
        let last = self.shared.last_frame.lock().unwrap();
        let Some(Com(last)) = last.as_ref() else {
            return Err(tr!("todavía no hay imagen de la fuente", "there is no image from the source yet").into());
        };
        read_back(last).map_err(|e| e.to_string())
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

fn keep_last_frame(shared: &Shared, frame: &Frame) -> windows::core::Result<()> {
    let source = frame.as_raw_texture();
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { source.GetDesc(&mut desc) };
    let mut last = shared.last_frame.lock().unwrap();
    if last.as_ref().is_none_or(|l| l.0.size != (desc.Width, desc.Height)) {
        let size = (desc.Width, desc.Height);
        desc.Usage = D3D11_USAGE_DEFAULT;
        (desc.BindFlags, desc.CPUAccessFlags, desc.MiscFlags) = (0, 0, 0);
        let mut texture = None;
        unsafe {
            frame.device().CreateTexture2D(&desc, None, Some(&mut texture))?;
            // The UI thread reads the copy back through the same immediate context.
            let _ = frame.device_context().cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
        }
        let texture = texture.expect("CreateTexture2D returned no texture");
        let (device, context) = (frame.device().clone(), frame.device_context().clone());
        *last = Some(Com(LastFrame { device, context, texture, size }));
    }
    let last = &last.as_ref().expect("set above").0;
    unsafe { last.context.CopyResource(&last.texture, source) };
    Ok(())
}

fn read_back(last: &LastFrame) -> windows::core::Result<(u32, u32, Vec<u8>)> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { last.texture.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    (desc.BindFlags, desc.MiscFlags) = (0, 0);
    let mut staging = None;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        last.device.CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.expect("CreateTexture2D returned no texture");
        last.context.CopyResource(&staging, &last.texture);
        last.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let (w, h, pitch) = (desc.Width as usize, desc.Height as usize, mapped.RowPitch as usize);
        let bgra = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * h);
        let rgba = bgra_to_rgba(bgra, w, h, pitch);
        last.context.Unmap(&staging, 0);
        Ok((desc.Width, desc.Height, rgba))
    }
}

/// Area-average downscale of BGRA rows (`pitch` bytes apart) to `size` opaque RGBA. Every
/// source pixel counts, so text stays legible; picking one pixel per block did not.
fn downscale(bgra: &[u8], (w, h): (usize, usize), pitch: usize, size: (usize, usize)) -> Vec<u8> {
    let (pw, ph) = size;
    let column: Vec<usize> = (0..w).map(|x| x * pw / w).collect();
    let mut sums = vec![0u32; pw * 3];
    let mut counts = vec![0u32; pw];
    let mut rgba = Vec::with_capacity(pw * ph * 4);
    for y in 0..ph {
        let (top, bottom) = (y * h / ph, ((y + 1) * h / ph).max(y * h / ph + 1));
        sums.fill(0);
        counts.fill(0);
        for sy in top..bottom {
            let row = &bgra[sy * pitch..sy * pitch + w * 4];
            for (&c, &[b, g, r, _]) in column.iter().zip(row.as_chunks::<4>().0) {
                sums[c * 3] += r as u32;
                sums[c * 3 + 1] += g as u32;
                sums[c * 3 + 2] += b as u32;
                counts[c] += 1;
            }
        }
        for (rgb, &n) in sums.as_chunks::<3>().0.iter().zip(&counts) {
            let n = n.max(1);
            rgba.extend_from_slice(&[(rgb[0] / n) as u8, (rgb[1] / n) as u8, (rgb[2] / n) as u8, 255]);
        }
    }
    rgba
}

/// Converts BGRA rows (`pitch` bytes apart) into tightly packed opaque RGBA.
fn bgra_to_rgba(bgra: &[u8], width: usize, height: usize, pitch: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in bgra.chunks(pitch).take(height) {
        for px in row[..width * 4].as_chunks::<4>().0 {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }
    rgba
}

/// Cuts `region` out of a packed RGBA image that is `width` pixels wide.
pub fn crop_rgba(rgba: &[u8], width: u32, region: Region) -> Vec<u8> {
    let (x, w) = (region.x as usize * 4, region.width as usize * 4);
    let rows = rgba.chunks(width as usize * 4).skip(region.y as usize).take(region.height as usize);
    rows.flat_map(|row| &row[x..x + w]).copied().collect()
}

/// Writes an RGBA image as PNG.
pub fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_rows_with_padding_become_packed_rgba() {
        // 2×2 image, rows padded to 12 bytes.
        let bgra = [1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0];
        assert_eq!(
            bgra_to_rgba(&bgra, 2, 2, 12),
            [3, 2, 1, 255, 7, 6, 5, 255, 11, 10, 9, 255, 15, 14, 13, 255]
        );
    }

    #[test]
    fn preview_downscale_averages_every_pixel() {
        // 4×2 BGRA, rows padded to 20 bytes: left half black/white columns, right half grey.
        let mut bgra = Vec::new();
        for _ in 0..2 {
            bgra.extend_from_slice(&[0, 0, 0, 255, 255, 255, 255, 255, 100, 100, 100, 255, 100, 100, 100, 255, 9, 9, 9, 9]);
        }
        // Half size: each output pixel is the mean of a 2×2 block.
        assert_eq!(downscale(&bgra, (4, 2), 20, (2, 1)), [127, 127, 127, 255, 100, 100, 100, 255]);
        // Same size: a plain BGRA to RGBA copy.
        let same = downscale(&bgra, (4, 2), 20, (4, 2));
        assert_eq!(same, bgra_to_rgba(&bgra, 4, 2, 20));
    }

    #[test]
    fn crop_keeps_only_the_region() {
        // 3×2 image whose pixels are numbered 0..6 in the red channel.
        let rgba: Vec<u8> = (0..6).flat_map(|i| [i, 0, 0, 255]).collect();
        let region = Region { x: 1, y: 0, width: 2, height: 2 };
        assert_eq!(crop_rgba(&rgba, 3, region), [1, 0, 0, 255, 2, 0, 0, 255, 4, 0, 0, 255, 5, 0, 0, 255]);
    }

    #[test]
    fn png_round_trips() {
        let path = std::env::temp_dir().join(format!("obs-lite-test-{}.png", std::process::id()));
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
        write_png(&path, 2, 2, &rgba).unwrap();
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut out = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut out).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(out, rgba);
    }
}
