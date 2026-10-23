//! Capture sources, recording settings and picture helpers shared by every platform. The
//! capture session itself is platform code (`platform::capture`).

use crate::camera::{CameraSize, Corner};
use crate::config::{Quality, Region, Resolution};

pub use crate::platform::capture::{Session, list_audio_apps, list_sources};
#[cfg(target_os = "linux")]
pub use crate::platform::capture::forget_choice;

/// Asks for one repaint, for a new picture to show. egui answers an immediate request with two
/// repaints (to let layout settle), which a picture does not need; pictures arrive several times
/// a second, so the second one would be most of the idle drawing.
pub fn repaint_for_picture(ctx: &eframe::egui::Context) {
    ctx.request_repaint_after(std::time::Duration::from_millis(1));
}

/// Preview width until the UI reports how wide the preview is drawn.
pub(crate) const PREVIEW_DEFAULT_WIDTH: usize = 640;

// Windows lists displays and windows itself; on Linux the portal picks them.
#[cfg_attr(target_os = "linux", allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceId {
    Monitor(isize),
    Window(isize),
    /// Chosen by the user in the ScreenCast portal's dialog.
    #[cfg(target_os = "linux")]
    Portal,
}

#[derive(Clone, Debug)]
pub struct Source {
    pub id: SourceId,
    pub label: String,
}

/// An application whose audio can be captured on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioApp {
    pub pid: u32,
    /// Executable name, e.g. "Spotify.exe": what the settings remember.
    pub exe: String,
    pub title: String,
}

#[derive(Clone)]
pub struct RecordConfig {
    pub quality: Quality,
    pub resolution: Resolution,
    pub audio: bool,
    pub region: Option<Region>,
    pub overlay: Option<(Corner, CameraSize)>,
}

pub struct PreviewFrame {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
    pub seq: u64,
}

pub struct SessionOptions {
    pub source: SourceId,
    pub fps: u32,
    pub cursor: bool,
    /// Preview updates per second; 0 disables the preview.
    pub preview_fps: u32,
    pub record: Option<RecordConfig>,
}

/// Area-average downscale of BGRA rows (`pitch` bytes apart) to `size` opaque RGBA. Every
/// source pixel counts, so text stays legible; picking one pixel per block did not.
pub(crate) fn downscale(bgra: &[u8], (w, h): (usize, usize), pitch: usize, size: (usize, usize)) -> Vec<u8> {
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
pub(crate) fn bgra_to_rgba(bgra: &[u8], width: usize, height: usize, pitch: usize) -> Vec<u8> {
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
