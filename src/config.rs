//! Persistent user settings stored as JSON in %APPDATA%\obs-lite\settings.json.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::camera::CameraConfig;
use crate::i18n::{Language, tr};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Quality {
    Low,
    Medium,
    High,
}

impl Quality {
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => tr!("Calidad estándar, tamaño de archivo pequeño", "Standard Quality, Small File Size"),
            Self::Medium => tr!("Alta calidad, tamaño de archivo medio", "High Quality, Medium File Size"),
            Self::High => tr!("Calidad indistinguible, tamaño de archivo grande", "Indistinguishable Quality, Large File Size"),
        }
    }

    /// Bitrate scaled from a 1080p60 reference by pixel rate.
    pub fn bitrate(self, width: u32, height: u32, fps: u32) -> u32 {
        let reference = match self {
            Self::Low => 6_000_000.0,
            Self::Medium => 12_000_000.0,
            Self::High => 24_000_000.0,
        };
        let pixel_rate = width as f64 * height as f64 * fps as f64 / (1920.0 * 1080.0 * 60.0);
        (reference * pixel_rate).clamp(1_500_000.0, 80_000_000.0) as u32
    }
}

/// Output height; the capture is only ever scaled down.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Resolution {
    Native,
    P1440,
    P1080,
    P720,
}

impl Resolution {
    pub const ALL: [Self; 4] = [Self::Native, Self::P1440, Self::P1080, Self::P720];

    pub fn label(self) -> &'static str {
        match self {
            Self::Native => tr!("Nativa", "Native"),
            Self::P1440 => "1440p",
            Self::P1080 => "1080p",
            Self::P720 => "720p",
        }
    }

    /// Output size for a capture of `(w, h)`, keeping the aspect ratio and even dimensions.
    pub fn apply(self, (w, h): (u32, u32)) -> (u32, u32) {
        let target = match self {
            Self::Native => return (w & !1, h & !1),
            Self::P1440 => 1440,
            Self::P1080 => 1080,
            Self::P720 => 720,
        };
        if h <= target {
            return (w & !1, h & !1);
        }
        let scaled_w = (w as u64 * target as u64 / h as u64) as u32;
        (scaled_w.max(2) & !1, target)
    }
}

/// Part of the source to record, in source pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// Smaller regions are almost certainly a mis-click.
    const MIN: u32 = 16;

    /// The region clipped to a source of `(w, h)`, with the even dimensions H.264 needs;
    /// `None` if little or nothing of it is left.
    pub fn fit(self, (w, h): (u32, u32)) -> Option<Self> {
        if self.x >= w || self.y >= h {
            return None;
        }
        let width = self.width.min(w - self.x) & !1;
        let height = self.height.min(h - self.y) & !1;
        (width >= Self::MIN && height >= Self::MIN).then_some(Self { width, height, ..self })
    }
}

/// Global hotkeys as text ("Ctrl+Alt+R"); `None` means unassigned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub record: Option<String>,
    pub pause: Option<String>,
    pub screenshot: Option<String>,
    pub mute_mic: Option<String>,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self { record: Some("Ctrl+Alt+R".into()), pause: Some("Ctrl+Alt+P".into()), screenshot: None, mute_mic: None }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `None` follows the Windows display language.
    pub language: Option<Language>,
    /// Keep the obs-lite window out of every capture, including its own preview.
    pub hide_from_capture: bool,
    /// Minimizing hides the window; the tray icon brings it back.
    pub minimize_to_tray: bool,
    /// Ask GitHub for a newer release at start-up.
    pub check_updates: bool,
    pub output_dir: PathBuf,
    pub fps: u32,
    pub resolution: Resolution,
    pub quality: Quality,
    pub cursor: bool,
    pub system_audio: bool,
    pub system_volume: f32,
    pub system_muted: bool,
    pub mic: bool,
    pub mic_volume: f32,
    pub mic_muted: bool,
    /// RNNoise noise suppression on the microphone.
    pub mic_denoise: bool,
    /// WASAPI endpoint IDs; `None` follows the Windows default device.
    pub system_device: Option<String>,
    pub mic_device: Option<String>,
    pub source_label: Option<String>,
    /// Record only this part of the source.
    pub region: Option<Region>,
    /// Webcam overlaid on the recording.
    pub camera: Option<CameraConfig>,
    pub hotkeys: Hotkeys,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: None,
            hide_from_capture: true,
            minimize_to_tray: false,
            check_updates: true,
            output_dir: dirs::video_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")),
            fps: 60,
            resolution: Resolution::Native,
            quality: Quality::Medium,
            cursor: true,
            system_audio: true,
            system_volume: 1.0,
            system_muted: false,
            mic: false,
            mic_volume: 1.0,
            mic_muted: false,
            mic_denoise: false,
            system_device: None,
            mic_device: None,
            source_label: None,
            region: None,
            camera: None,
            hotkeys: Hotkeys::default(),
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("obs-lite").join("settings.json"))
}

impl Config {
    pub fn load() -> Self {
        path().and_then(|p| fs::read_to_string(p).ok()).and_then(|s| Self::parse(&s)).unwrap_or_default()
    }

    /// Editors such as Windows PowerShell save UTF-8 with a BOM, which serde_json rejects;
    /// without this the app would start with defaults and overwrite the file on exit.
    fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(p, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_scales_with_pixel_rate() {
        assert_eq!(Quality::Medium.bitrate(1920, 1080, 60), 12_000_000);
        assert_eq!(Quality::Medium.bitrate(1920, 1080, 30), 6_000_000);
        assert_eq!(Quality::High.bitrate(3840, 2160, 60), 80_000_000); // clamped
        assert_eq!(Quality::Low.bitrate(640, 360, 30), 1_500_000); // clamped
    }

    #[test]
    fn resolution_scales_down_only_and_keeps_aspect() {
        assert_eq!(Resolution::P1080.apply((3840, 2160)), (1920, 1080));
        assert_eq!(Resolution::P720.apply((2560, 1440)), (1280, 720));
        assert_eq!(Resolution::P1080.apply((1280, 720)), (1280, 720));
        assert_eq!(Resolution::P720.apply((3440, 1440)), (1720, 720));
        assert_eq!(Resolution::Native.apply((1001, 777)), (1000, 776));
    }

    #[test]
    fn region_is_clipped_to_the_source_with_even_sides() {
        let r = Region { x: 100, y: 50, width: 301, height: 2000 };
        assert_eq!(r.fit((1920, 1080)), Some(Region { x: 100, y: 50, width: 300, height: 1030 }));
        assert_eq!(r.fit((300, 1080)), Some(Region { x: 100, y: 50, width: 200, height: 1030 }));
        assert_eq!(r.fit((100, 1080)), None); // starts outside a source that shrank
        assert_eq!(Region { x: 0, y: 0, width: 10, height: 500 }.fit((1920, 1080)), None);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let c: Config = serde_json::from_str(r#"{"fps": 30}"#).unwrap();
        assert_eq!(c.fps, 30);
        assert_eq!(c.quality, Quality::Medium);
    }

    #[test]
    fn settings_saved_with_a_utf8_bom_still_load() {
        let c = Config::parse("\u{feff}{\"fps\": 30}").expect("settings with a BOM are rejected");
        assert_eq!(c.fps, 30);
    }
}
