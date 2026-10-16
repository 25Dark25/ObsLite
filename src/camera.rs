//! Webcam capture through Media Foundation, and the picture-in-picture overlay maths.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFMediaSource, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFCreateAttributes,
    MFCreateMediaType, MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video, MFSTARTUP_FULL,
    MFShutdown, MFStartup, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{GUID, Interface, PWSTR};

use crate::i18n::tr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Self; 4] = [Self::TopLeft, Self::TopRight, Self::BottomLeft, Self::BottomRight];

    pub fn label(self) -> &'static str {
        match self {
            Self::TopLeft => tr!("Arriba a la izquierda", "Top left"),
            Self::TopRight => tr!("Arriba a la derecha", "Top right"),
            Self::BottomLeft => tr!("Abajo a la izquierda", "Bottom left"),
            Self::BottomRight => tr!("Abajo a la derecha", "Bottom right"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CameraSize {
    Small,
    Medium,
    Large,
}

impl CameraSize {
    pub const ALL: [Self; 3] = [Self::Small, Self::Medium, Self::Large];

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => tr!("Pequeña", "Small"),
            Self::Medium => tr!("Mediana", "Medium"),
            Self::Large => tr!("Grande", "Large"),
        }
    }

    /// Overlay width as a share of the picture width.
    fn fraction(self) -> f32 {
        match self {
            Self::Small => 0.2,
            Self::Medium => 0.3,
            Self::Large => 0.4,
        }
    }
}

/// The webcam source: which device, and where its overlay goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraConfig {
    /// Media Foundation symbolic link of the device.
    pub device: String,
    pub name: String,
    pub corner: Corner,
    pub size: CameraSize,
}

/// `(x, y, width, height)` in pixels.
pub type PixelRect = (u32, u32, u32, u32);

/// Overlay rectangle `(x, y, width, height)` for a `camera`-sized image on a `picture`.
/// Keeps the camera's aspect ratio and even sides, with a small margin from the edges.
pub fn overlay_rect(picture: (u32, u32), camera: (u32, u32), corner: Corner, size: CameraSize) -> PixelRect {
    let margin = (picture.0.min(picture.1) as f32 * 0.02) as u32;
    let mut w = (picture.0 as f32 * size.fraction()) as u32;
    let mut h = (w as u64 * camera.1 as u64 / camera.0.max(1) as u64) as u32;
    let max_h = picture.1.saturating_sub(2 * margin);
    if h > max_h {
        w = (w as u64 * max_h as u64 / h.max(1) as u64) as u32;
        h = max_h;
    }
    let (w, h) = ((w & !1).max(2), (h & !1).max(2));
    let right = picture.0.saturating_sub(w + margin);
    let bottom = picture.1.saturating_sub(h + margin);
    let (x, y) = match corner {
        Corner::TopLeft => (margin, margin),
        Corner::TopRight => (right, margin),
        Corner::BottomLeft => (margin, bottom),
        Corner::BottomRight => (right, bottom),
    };
    (x, y, w, h)
}

/// Bilinear resize of a packed 4-bytes-per-pixel image.
pub fn resize(src: &[u8], (sw, sh): (u32, u32), (dw, dh): (u32, u32)) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = Vec::with_capacity(dw * dh * 4);
    let fx = sw as f32 / dw as f32;
    let fy = sh as f32 / dh as f32;
    for y in 0..dh {
        let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (sy as usize, sy.fract());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..dw {
            let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (sx as usize, sx.fract());
            let x1 = (x0 + 1).min(sw - 1);
            for c in 0..4 {
                let p = |x: usize, y: usize| src[(y * sw + x) * 4 + c] as f32;
                let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * tx;
                let bottom = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * tx;
                out.push((top + (bottom - top) * ty).round() as u8);
            }
        }
    }
    out
}

/// One camera picture, top-down BGRA.
pub struct CameraFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

type Sink = Box<dyn Fn(&CameraFrame) + Send>;

pub struct CameraShared {
    pub latest: Mutex<Option<Arc<CameraFrame>>>,
    pub seq: AtomicU64,
    pub error: Mutex<Option<String>>,
    /// Receives every frame while recording.
    pub sink: Mutex<Option<Sink>>,
    stop: AtomicBool,
}

pub struct Camera {
    pub shared: Arc<CameraShared>,
    thread: Option<JoinHandle<()>>,
}

impl Camera {
    pub fn start(device: String, repaint: Option<eframe::egui::Context>) -> Self {
        let shared = Arc::new(CameraShared {
            latest: Mutex::new(None),
            seq: AtomicU64::new(0),
            error: Mutex::new(None),
            sink: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let thread = thread::Builder::new()
            .name("camera".into())
            .spawn({
                let shared = shared.clone();
                move || {
                    if let Err(e) = with_media_foundation(|| capture_loop(&device, &shared, repaint.as_ref())) {
                        *shared.error.lock().unwrap() = Some(e.to_string());
                    }
                }
            })
            .expect("spawn camera thread");
        Self { shared, thread: Some(thread) }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Video capture devices, from a thread of its own (COM and Media Foundation are per thread).
pub fn list_devices() -> Vec<CameraDevice> {
    let list = || {
        with_media_foundation(|| {
            Ok(devices()?.iter().filter_map(|a| Some(CameraDevice { id: link(a)?, name: name(a)? })).collect())
        })
    };
    thread::spawn(list).join().ok().and_then(Result::ok).unwrap_or_default()
}

fn with_media_foundation<T>(f: impl FnOnce() -> windows::core::Result<T>) -> windows::core::Result<T> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    let result = f();
    let _ = unsafe { MFShutdown() };
    result
}

fn devices() -> windows::core::Result<Vec<IMFActivate>> {
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.expect("MFCreateAttributes returned nothing");
        attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        let mut array = std::ptr::null_mut();
        let mut count = 0;
        MFEnumDeviceSources(&attributes, &mut array, &mut count)?;
        let activates = std::slice::from_raw_parts_mut(array, count as usize).iter_mut().filter_map(Option::take).collect();
        CoTaskMemFree(Some(array as *const _));
        Ok(activates)
    }
}

fn string(activate: &IMFActivate, key: &GUID) -> Option<String> {
    unsafe {
        let mut value = PWSTR::null();
        let mut len = 0;
        activate.GetAllocatedString(key, &mut value, &mut len).ok()?;
        let text = value.to_string().ok();
        CoTaskMemFree(Some(value.0 as *const _));
        text
    }
}

fn name(activate: &IMFActivate) -> Option<String> {
    string(activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
}

fn link(activate: &IMFActivate) -> Option<String> {
    string(activate, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)
}

fn capture_loop(device: &str, shared: &CameraShared, repaint: Option<&eframe::egui::Context>) -> windows::core::Result<()> {
    let not_found = || windows::core::Error::new(windows::core::HRESULT(-1), tr!("cámara no encontrada", "camera not found"));
    let activate = devices()?.into_iter().find(|a| link(a).as_deref() == Some(device)).ok_or_else(not_found)?;
    unsafe {
        let source: IMFMediaSource = activate.ActivateObject()?;
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.expect("MFCreateAttributes returned nothing");
        // Lets the reader convert whatever the camera delivers (YUY2, NV12, MJPG…) to RGB32.
        attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromMediaSource(&source, &attributes)?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

        // The largest native mode up to 1280 wide is plenty for an overlay.
        let mut best: Option<(u64, _)> = None;
        for i in 0.. {
            let Ok(native) = reader.GetNativeMediaType(stream, i) else { break };
            let Ok(size) = native.GetUINT64(&MF_MT_FRAME_SIZE) else { continue };
            let (w, h) = ((size >> 32) as u32, size as u32);
            let area = w as u64 * h as u64;
            if w <= 1280 && best.as_ref().is_none_or(|(a, _)| area > *a) {
                best = Some((area, native));
            }
        }
        if let Some((_, native)) = &best {
            reader.SetCurrentMediaType(stream, None, native)?;
        }
        let rgb = MFCreateMediaType()?;
        rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        reader.SetCurrentMediaType(stream, None, &rgb)?;
        let current = reader.GetCurrentMediaType(stream)?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (width, height) = ((size >> 32) as u32, size as u32);
        // Uncompressed RGB is bottom-up (negative stride) unless the type says otherwise.
        let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or(-(width as i32 * 4), |s| s as i32);

        while !shared.stop.load(Ordering::Relaxed) {
            let mut flags = 0u32;
            let mut sample = None;
            reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else { continue };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut bgra = Vec::with_capacity(width as usize * height as usize * 4);
            // Rows are `pitch` bytes apart starting from the top row; negative for bottom-up images.
            let mut copy = |top: *const u8, pitch: isize| {
                for y in 0..height as isize {
                    let row = std::slice::from_raw_parts(top.offset(y * pitch), width as usize * 4);
                    bgra.extend(row.as_chunks::<4>().0.iter().flat_map(|&[b, g, r, _]| [b, g, r, 255]));
                }
            };
            if let Ok(buffer) = buffer.cast::<IMF2DBuffer>() {
                let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
                buffer.Lock2D(&mut scan0, &mut pitch)?;
                copy(scan0, pitch as isize);
                buffer.Unlock2D()?;
            } else {
                let mut data = std::ptr::null_mut();
                buffer.Lock(&mut data, None, None)?;
                let top = if stride < 0 { data.offset(-stride as isize * (height as isize - 1)) } else { data };
                copy(top, stride as isize);
                buffer.Unlock()?;
            }
            let frame = CameraFrame { width, height, bgra };
            if let Some(sink) = shared.sink.lock().unwrap().as_ref() {
                sink(&frame);
            }
            *shared.latest.lock().unwrap() = Some(Arc::new(frame));
            shared.seq.fetch_add(1, Ordering::Relaxed);
            if let Some(ctx) = repaint {
                ctx.request_repaint();
            }
        }
        // Releases the camera (its light goes off).
        let _ = source.Shutdown();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_sits_in_the_corner_with_the_camera_aspect() {
        let pic = (1920, 1080);
        assert_eq!(overlay_rect(pic, (640, 480), Corner::BottomRight, CameraSize::Medium), (1323, 627, 576, 432));
        assert_eq!(overlay_rect(pic, (1280, 720), Corner::TopLeft, CameraSize::Small), (21, 21, 384, 216));
        // A tall picture: the overlay is limited by the height.
        let (_, _, w, h) = overlay_rect((400, 200), (640, 480), Corner::TopLeft, CameraSize::Large);
        assert!(h <= 200 - 2 * 4 && w <= 160, "{w}×{h}");
    }

    #[test]
    fn resize_keeps_flat_colour_and_identity() {
        let flat = [10, 20, 30, 255].repeat(4 * 3);
        assert_eq!(resize(&flat, (4, 3), (7, 5)), [10, 20, 30, 255].repeat(7 * 5));
        let img: Vec<u8> = (0..16).collect();
        assert_eq!(resize(&img, (2, 2), (2, 2)), img);
    }
}
