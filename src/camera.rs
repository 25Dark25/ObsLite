//! Webcam sources and the picture-in-picture overlay maths. Capturing from the device is
//! platform code (`platform::camera`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use crate::i18n::tr;

pub use crate::platform::camera::list_devices;

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
    /// Platform device id (on Windows, the Media Foundation symbolic link).
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
    /// Set while the window is not shown: pictures are kept but do not wake it.
    pub hidden: AtomicBool,
    /// The camera is open: stopping its thread takes a moment. Before (on Linux, while the user
    /// is asked for access) it could take any time.
    opened: AtomicBool,
    stop: AtomicBool,
}

pub struct Camera {
    pub shared: Arc<CameraShared>,
    thread: Option<JoinHandle<()>>,
}

impl Camera {
    pub fn start(device: String, repaint: Option<eframe::egui::Context>) -> Self {
        let opened = !crate::platform::camera::OPENING_ASKS_USER;
        Self::start_with(move |shared, repaint| crate::platform::camera::capture(&device, shared, repaint), opened, repaint)
    }

    /// Runs `capture` on a thread of its own: it delivers pictures until it is stopped.
    pub(crate) fn start_with(
        capture: impl FnOnce(&CameraShared, Option<&eframe::egui::Context>) -> Result<(), String> + Send + 'static,
        opened: bool,
        repaint: Option<eframe::egui::Context>,
    ) -> Self {
        let shared = Arc::new(CameraShared {
            latest: Mutex::new(None),
            seq: AtomicU64::new(0),
            error: Mutex::new(None),
            sink: Mutex::new(None),
            hidden: AtomicBool::new(false),
            opened: AtomicBool::new(opened),
            stop: AtomicBool::new(false),
        });
        let thread = thread::Builder::new()
            .name("camera".into())
            .spawn({
                let shared = shared.clone();
                move || {
                    let result = capture(&shared, repaint.as_ref());
                    if !shared.stopped() {
                        // Unplugged, access taken away, or the stream ended: no picture to keep.
                        let why = result.err().unwrap_or_else(|| tr!("la cámara dejó de enviar imágenes", "the camera stopped sending pictures").to_owned());
                        shared.end(why, repaint.as_ref());
                    }
                }
            })
            .expect("spawn camera thread");
        Self { shared, thread: Some(thread) }
    }
}

impl CameraShared {
    /// Whether the capture thread should stop.
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Called by the capture once the camera is open (the user has answered).
    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) fn set_opened(&self) {
        self.opened.store(true, Ordering::Relaxed);
    }

    /// The camera stopped by itself; `error` says why.
    pub fn ended(&self) -> bool {
        self.error.lock().unwrap().is_some()
    }

    fn end(&self, why: String, repaint: Option<&eframe::egui::Context>) {
        *self.error.lock().unwrap() = Some(why);
        *self.latest.lock().unwrap() = None;
        self.seq.fetch_add(1, Ordering::Relaxed);
        if let Some(ctx) = repaint {
            ctx.request_repaint();
        }
    }

    /// A captured picture: to the recording (if any), then to the preview.
    pub(crate) fn deliver(&self, frame: CameraFrame, repaint: Option<&eframe::egui::Context>) {
        if let Some(sink) = self.sink.lock().unwrap().as_ref() {
            sink(&frame);
        }
        *self.latest.lock().unwrap() = Some(Arc::new(frame));
        self.seq.fetch_add(1, Ordering::Relaxed);
        if let Some(ctx) = repaint.filter(|_| !self.hidden.load(Ordering::Relaxed)) {
            crate::capture::repaint_for_picture(ctx);
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        // A camera still waiting for the user's answer is left to end by itself once it comes:
        // waiting here froze the window until the user answered.
        if let Some(t) = self.thread.take()
            && self.shared.opened.load(Ordering::Relaxed)
        {
            let _ = t.join();
        }
    }
}

/// Which camera mode to use, of `modes` as (width, height, frames per second): one at least 25
/// fps if there is one (many webcams offer 720p uncompressed at only 10 fps, and compressed at
/// 30), then the largest up to 1280 wide, which is plenty for an overlay, then the fastest.
#[cfg(any(windows, test))]
pub(crate) fn best_mode(modes: impl IntoIterator<Item = (u32, u32, f64)>) -> Option<usize> {
    let rank = |(w, h, fps): (u32, u32, f64)| (fps >= 25.0, u64::from(w) * u64::from(h), (fps * 1000.0) as u64);
    let mut best: Option<(usize, (bool, u64, u64))> = None;
    for (i, mode) in modes.into_iter().enumerate() {
        if mode.0 <= 1280 && best.is_none_or(|(_, b)| rank(mode) > b) {
            best = Some((i, rank(mode)));
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn picture() -> CameraFrame {
        CameraFrame { width: 2, height: 2, bgra: vec![9; 16] }
    }

    /// Waits up to a second for `done`.
    fn wait_for(done: impl Fn() -> bool) {
        let start = Instant::now();
        while !done() && start.elapsed() < Duration::from_secs(1) {
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn removing_a_camera_still_asking_for_access_does_not_wait_for_the_answer() {
        // Linux asks the user for the cameras: the answer may take any time.
        let (answer, answered) = std::sync::mpsc::channel::<()>();
        let camera = Camera::start_with(
            move |shared, _| {
                let _ = answered.recv_timeout(Duration::from_secs(2));
                shared.set_opened();
                Ok(())
            },
            false,
            None,
        );
        let removed = Instant::now();
        drop(camera);
        assert!(removed.elapsed() < Duration::from_millis(500), "the window froze for {:?}", removed.elapsed());
        drop(answer);
    }

    #[test]
    fn a_hidden_window_is_not_woken_for_every_camera_picture() {
        for hidden in [false, true] {
            // A camera that runs until it is removed.
            let camera = Camera::start_with(
                |shared, _| {
                    while !shared.stopped() {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Ok(())
                },
                true,
                None,
            );
            camera.shared.hidden.store(hidden, Ordering::Relaxed);
            let ctx = eframe::egui::Context::default();
            let woken = Arc::new(AtomicU64::new(0));
            ctx.set_request_repaint_callback({
                let woken = woken.clone();
                move |_| {
                    woken.fetch_add(1, Ordering::Relaxed);
                }
            });
            camera.shared.deliver(picture(), Some(&ctx));
            // Minimized or in the tray, about 30 wake-ups a second for nothing.
            assert_eq!(woken.load(Ordering::Relaxed) > 0, !hidden, "hidden: {hidden}");
            assert!(camera.shared.latest.lock().unwrap().is_some(), "kept for when the window shows");
        }
    }

    #[test]
    fn a_camera_that_stops_leaves_no_frozen_picture() {
        // Unplugged (an error), or its stream ended (no error).
        for result in [Err("unplugged".to_owned()), Ok(())] {
            let camera = Camera::start_with(
                move |shared, repaint| {
                    shared.deliver(picture(), repaint);
                    result
                },
                true,
                None,
            );
            wait_for(|| camera.shared.ended());
            assert!(camera.shared.latest.lock().unwrap().is_none(), "its last picture would stay on screen and in recordings");
            assert!(camera.shared.error.lock().unwrap().is_some(), "the user is not told");
            assert_eq!(camera.shared.seq.load(Ordering::Relaxed), 2, "the preview is not told");
        }
    }

    #[test]
    fn a_camera_mode_fast_enough_wins_over_a_slow_one() {
        // Uncompressed 720p at 10 fps listed before compressed 720p at 30.
        assert_eq!(best_mode([(1280, 720, 10.0), (1280, 720, 30.0), (640, 480, 30.0)]), Some(1));
        // Nothing fast enough: the largest.
        assert_eq!(best_mode([(640, 480, 15.0), (1280, 720, 10.0)]), Some(1));
        // Fast enough beats larger, and nothing wider than 1280.
        assert_eq!(best_mode([(1280, 720, 10.0), (640, 360, 30.0), (1920, 1080, 30.0)]), Some(1));
        assert_eq!(best_mode([(1920, 1080, 30.0)]), None);
    }

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
