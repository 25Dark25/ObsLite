//! Draws the app icon and embeds it in the executable.
//!
//! The same drawing is written as a multi-size .ico (for Explorer and the taskbar)
//! and as raw RGBA (for the window icon), so there is a single source of truth.

use std::env;
use std::fs;
use std::path::PathBuf;

/// Renders the icon at `size`×`size` as straight-alpha RGBA, with 4×4 supersampling.
fn render(size: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let s = size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0f32; 4]; // premultiplied
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = (x as f32 + (sx as f32 + 0.5) / SS as f32) / s;
                    let py = (y as f32 + (sy as f32 + 0.5) / SS as f32) / s;
                    let [r, g, b, a] = shade(px, py);
                    acc[0] += r * a;
                    acc[1] += g * a;
                    acc[2] += b * a;
                    acc[3] += a;
                }
            }
            let n = (SS * SS) as f32;
            let a = acc[3] / n;
            let i = ((y * size + x) * 4) as usize;
            if a > 0.0 {
                for c in 0..3 {
                    out[i + c] = ((acc[c] / n / a).clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
            out[i + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}

/// Colour at normalized coordinates: a dark rounded tile with a white ring and a red dot.
fn shade(x: f32, y: f32) -> [f32; 4] {
    let (dx, dy) = (x - 0.5, y - 0.5);
    let d = (dx * dx + dy * dy).sqrt();
    if d < 0.2 {
        return [0.90, 0.28, 0.30, 1.0];
    }
    if (0.27..0.33).contains(&d) {
        return [0.95, 0.95, 0.96, 1.0];
    }
    // Rounded square with corner radius 0.22 inside a 0.04 margin.
    let (m, r) = (0.04, 0.22);
    let qx = (dx.abs() - (0.5 - m - r)).max(0.0);
    let qy = (dy.abs() - (0.5 - m - r)).max(0.0);
    if (qx * qx + qy * qy).sqrt() <= r {
        let t = y; // subtle vertical gradient
        return [0.13 - 0.04 * t, 0.14 - 0.04 * t, 0.17 - 0.04 * t, 1.0];
    }
    [0.0, 0.0, 0.0, 0.0]
}

/// Encodes RGBA images as an .ico with 32-bit BMP entries.
fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| bmp_entry(size, &render(size))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, data) in sizes.iter().zip(&images) {
        let dim = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for data in images {
        out.extend_from_slice(&data);
    }
    out
}

fn bmp_entry(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&(size as i32 * 2).to_le_bytes()); // colour + mask
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    let mask_row = size.div_ceil(32) * 4;
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out.join("icon64.rgba"), render(64)).unwrap();
    let ico_path = out.join("icon.ico");
    fs::write(&ico_path, ico(&[16, 24, 32, 48, 64, 256])).unwrap();

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().unwrap());
        res.set("FileDescription", "obs-lite");
        res.set("ProductName", "obs-lite");
        res.compile().expect("embed Windows resources");
    }
}
