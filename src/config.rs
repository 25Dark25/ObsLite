//! Persistent user settings stored as JSON in %APPDATA%\obs-lite\settings.json, or in a
//! settings.json next to the executable when there is one (a portable copy).

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
    pub replay: Option<String>,
    pub save_replay: Option<String>,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            record: Some("Ctrl+Alt+R".into()),
            pause: Some("Ctrl+Alt+P".into()),
            screenshot: None,
            mute_mic: None,
            replay: None,
            save_replay: None,
        }
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
    /// Release whose start-up dialog the user asked not to see again.
    pub skipped_version: Option<String>,
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
    /// Executable whose audio is captured on its own (Application Audio Capture).
    pub app_audio: Option<String>,
    pub app_volume: f32,
    pub app_muted: bool,
    /// WASAPI endpoint IDs; `None` follows the Windows default device.
    pub system_device: Option<String>,
    pub mic_device: Option<String>,
    pub source_label: Option<String>,
    /// Record only this part of the source.
    pub region: Option<Region>,
    /// Webcam overlaid on the recording.
    pub camera: Option<CameraConfig>,
    pub hotkeys: Hotkeys,
    /// Seconds the replay buffer keeps.
    pub replay_seconds: u32,
    /// The last recording started, repaired at start-up in case obs-lite stopped while writing it.
    pub last_recording: Option<PathBuf>,
    /// `last_recording` as this obs-lite last read or wrote it (see `save_to`).
    #[serde(skip)]
    last_recording_on_disk: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: None,
            hide_from_capture: true,
            minimize_to_tray: false,
            check_updates: true,
            skipped_version: None,
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
            app_audio: None,
            app_volume: 1.0,
            app_muted: false,
            system_device: None,
            mic_device: None,
            source_label: None,
            region: None,
            camera: None,
            hotkeys: Hotkeys::default(),
            replay_seconds: 20,
            last_recording: None,
            last_recording_on_disk: None,
        }
    }
}

/// Where the settings are, decided once at start: a `settings.json` next to obs-lite makes it
/// portable, its settings kept with it; otherwise the user's config folder.
fn path() -> Option<&'static Path> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        // Tests never touch the user's settings.
        if cfg!(test) {
            return Some(std::env::temp_dir().join(format!("obs-lite-test-{}", std::process::id())).join("settings.json"));
        }
        std::env::current_exe().ok().and_then(|exe| portable(&exe)).or_else(usual_path)
    })
    .as_deref()
}

/// The settings file in the user's config folder.
fn usual_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("obs-lite").join("settings.json"))
}

/// The `settings.json` next to the executable `exe`, if there is one.
fn portable(exe: &Path) -> Option<PathBuf> {
    let path = exe.with_file_name("settings.json");
    path.is_file().then_some(path)
}

/// Where *Export Settings* first opens: the folder of a portable copy, where a `settings.json`
/// makes it keep its settings there. `None` for an installed copy and on Linux (the Flatpak's
/// folder is read-only), which leave it to the system.
pub fn export_folder() -> Option<PathBuf> {
    if cfg!(windows) && !crate::update::installed() {
        std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

impl Config {
    pub fn load() -> Self {
        path().map_or_else(Self::default, Self::load_from)
    }

    /// Writes the settings in use to `target`, for a backup or another copy of Obs Lite (as the
    /// `settings.json` next to a portable `obs-lite.exe`). A recording to repair stays with this
    /// copy, unless `target` is its own settings file.
    pub fn export_to(&mut self, target: &Path) -> std::io::Result<()> {
        if path() == Some(target) {
            self.save();
            return Ok(());
        }
        let json = serde_json::to_string_pretty(&Self { last_recording: None, ..self.clone() }).map_err(std::io::Error::other)?;
        write_replacing(target, json.as_bytes())
    }

    fn load_from(path: &Path) -> Self {
        let read = fs::read(path).ok().map(|bytes| String::from_utf8(bytes).ok().and_then(|s| Self::parse(&s)));
        let mut config = match read {
            Some(Some(config)) => config.within_range(),
            // Not a settings file any more (a hand edit gone wrong, another encoding): kept aside
            // rather than replaced by the defaults at the next save.
            Some(None) => {
                let _ = fs::rename(path, path.with_extension("json.bad"));
                Self::default()
            }
            None => Self::default(),
        };
        config.last_recording_on_disk = config.last_recording.clone();
        config
    }

    /// Settings edited by hand brought within limits: a replay buffer of a day would fill
    /// the memory, and a relative folder would put recordings wherever obs-lite was started from.
    fn within_range(mut self) -> Self {
        self.replay_seconds = self.replay_seconds.clamp(5, 600);
        self.fps = self.fps.clamp(1, 240);
        for volume in [&mut self.system_volume, &mut self.mic_volume, &mut self.app_volume] {
            *volume = volume.clamp(0.0, 1.0);
        }
        if !self.output_dir.is_absolute() {
            self.output_dir = Self::default().output_dir;
        }
        self
    }

    /// Editors such as Windows PowerShell save UTF-8 with a BOM, which serde_json rejects; and a
    /// setting that cannot be read (a value from a newer version, a hand edit of the wrong type)
    /// only loses itself. Otherwise the app would start with defaults and overwrite the file on
    /// exit, the recording to repair included.
    fn parse(json: &str) -> Option<Self> {
        let serde_json::Value::Object(fields) = serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()? else { return None };
        let reads = |key: &String, value: &serde_json::Value| {
            let alone = serde_json::Map::from_iter([(key.clone(), value.clone())]);
            serde_json::from_value::<Self>(serde_json::Value::Object(alone)).is_ok()
        };
        let readable = fields.into_iter().filter(|(key, value)| reads(key, value));
        serde_json::from_value(serde_json::Value::Object(readable.collect())).ok()
    }

    pub fn save(&mut self) {
        if let Some(p) = path() {
            self.save_to(p);
        }
    }

    /// Another obs-lite may run with the same settings and have started a recording since this
    /// one read them: its `last_recording` is kept, unless this one started a recording since
    /// (the newer one), or cleared the very recording still saved there.
    fn save_to(&mut self, path: &Path) {
        let on_disk = Self::load_from(path).last_recording;
        let changed = self.last_recording != self.last_recording_on_disk;
        let ours = changed && (self.last_recording.is_some() || on_disk == self.last_recording_on_disk);
        if !ours {
            self.last_recording = on_disk;
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = write_replacing(path, json.as_bytes());
        }
        self.last_recording_on_disk = self.last_recording.clone();
    }
}

/// Writes `path` through a new file next to it, so a crash or a full disk midway leaves the old
/// contents rather than a cut-short file (which would load as the defaults).
fn write_replacing(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let new = new_file(path);
    let written = File::create(&new).and_then(|mut f| f.write_all(data).and_then(|()| f.sync_all()));
    match written.and_then(|()| fs::rename(&new, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&new);
            Err(e)
        }
    }
}

/// The new file `write_replacing` writes first: one per process, as another obs-lite saving at the
/// same moment would otherwise write into the same one and leave a mix of both.
fn new_file(path: &Path) -> PathBuf {
    path.with_extension(format!("{}.new", std::process::id()))
}

/// Whether no other obs-lite runs with these settings: the first holds a lock on a file next to
/// them until it exits. Only that one repairs a recording cut short, which another may still be
/// writing.
pub fn first_instance() -> bool {
    static LOCK: OnceLock<Option<File>> = OnceLock::new();
    let lock = LOCK.get_or_init(|| path().and_then(|p| try_lock(&p.with_file_name("instance.lock"))));
    lock.is_some()
}

/// The file at `path`, locked for as long as it is open; `None` if another process holds it.
fn try_lock(path: &Path) -> Option<File> {
    fs::create_dir_all(path.parent()?).ok()?;
    let file = File::options().create(true).truncate(false).write(true).open(path).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_instance_does_not_lose_the_recording_to_repair() {
        let dir = std::env::temp_dir().join(format!("obs-lite-two-{}", std::process::id()));
        let file = dir.join("settings.json");
        Config::default().save_to(&file);
        let (mut first, mut second) = (Config::load_from(&file), Config::load_from(&file));
        // The second instance records; the first one is then closed, saving its settings.
        second.last_recording = Some(PathBuf::from("rec.mp4"));
        second.save_to(&file);
        first.mic_volume = 0.5;
        first.save_to(&file);
        let saved = Config::load_from(&file);
        std::fs::remove_dir_all(&dir).unwrap();
        // If the second one now stops partway, its recording must still be repaired.
        assert_eq!(saved.last_recording, Some(PathBuf::from("rec.mp4")));
        assert_eq!(saved.mic_volume, 0.5);
    }

    #[test]
    fn the_newest_recording_and_a_repair_are_saved() {
        let dir = std::env::temp_dir().join(format!("obs-lite-repair-{}", std::process::id()));
        let file = dir.join("settings.json");
        let mut config = Config { last_recording: Some(PathBuf::from("cut.mp4")), ..Config::default() };
        config.save_to(&file);
        // The first obs-lite repairs the recording cut short; another one records meanwhile.
        let (mut first, mut second) = (Config::load_from(&file), Config::load_from(&file));
        first.last_recording = None;
        second.last_recording = Some(PathBuf::from("new.mp4"));
        second.save_to(&file);
        first.save_to(&file);
        assert_eq!(Config::load_from(&file).last_recording, Some(PathBuf::from("new.mp4")), "the repair cleared the new one");
        // With nobody else, the repair is saved; and a recording started here is saved over another's.
        let mut alone = Config::load_from(&file);
        alone.last_recording = None;
        alone.save_to(&file);
        assert_eq!(Config::load_from(&file).last_recording, None);
        first.last_recording = Some(PathBuf::from("mine.mp4"));
        first.save_to(&file);
        let saved = Config::load_from(&file).last_recording;
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(saved, Some(PathBuf::from("mine.mp4")));
    }

    #[test]
    fn only_one_process_holds_the_instance_lock() {
        let path = std::env::temp_dir().join(format!("obs-lite-lock-{}", std::process::id()));
        let held = try_lock(&path).expect("free at first");
        // A second open file is what a second process would have.
        assert!(try_lock(&path).is_none());
        drop(held);
        assert!(try_lock(&path).is_some(), "released when the holder exits");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn settings_are_replaced_whole() {
        let dir = std::env::temp_dir().join(format!("obs-lite-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        write_replacing(&path, b"old").unwrap();
        write_replacing(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "no temporary file left");
        // The new contents cannot be written (a folder is in the way): the old ones stay.
        fs::create_dir(new_file(&path)).unwrap();
        assert!(write_replacing(&path, b"lost").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_dir_all(&dir).unwrap();
    }

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
    fn an_unreadable_settings_file_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("obs-lite-bad-{}", std::process::id()));
        let path = dir.join("settings.json");
        // A hand edit that left a missing quote.
        write_replacing(&path, br#"{"fps": 30, "output_dir": "D:\Videos}"#).unwrap();
        let loaded = Config::load_from(&path);
        let kept = fs::read(dir.join("settings.json.bad"));
        let replaced = path.exists();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(loaded.fps, Config::default().fps);
        assert_eq!(kept.unwrap(), br#"{"fps": 30, "output_dir": "D:\Videos}"#, "the user's file would be overwritten with the defaults");
        assert!(!replaced);
    }

    #[test]
    fn exported_settings_next_to_the_executable_make_it_portable() {
        let dir = std::env::temp_dir().join(format!("obs-lite-portable-{}", std::process::id()));
        let exe = dir.join("obs-lite.exe");
        fs::create_dir_all(&dir).unwrap();
        let without = portable(&exe);
        // Exported there: the same settings, without the recording this copy has to repair.
        let mut config = Config { fps: 30, last_recording: Some(PathBuf::from("cut.mp4")), ..Config::default() };
        let exported = config.export_to(&dir.join("settings.json"));
        let found = portable(&exe);
        let loaded = found.as_deref().map(Config::load_from);
        fs::remove_dir_all(&dir).unwrap();
        assert!(exported.is_ok());
        assert_eq!(without, None);
        assert_eq!(found, Some(dir.join("settings.json")));
        let loaded = loaded.unwrap();
        assert_eq!((loaded.fps, loaded.last_recording), (30, None));
        assert_eq!(config.last_recording, Some(PathBuf::from("cut.mp4")), "taken from the copy in use");
    }

    #[test]
    fn exporting_to_a_folder_that_cannot_be_written_says_so() {
        let dir = std::env::temp_dir().join(format!("obs-lite-export-{}", std::process::id()));
        // A folder where the file should go: it cannot be replaced by a file.
        fs::create_dir_all(dir.join("settings.json")).unwrap();
        let exported = Config::default().export_to(&dir.join("settings.json"));
        fs::remove_dir_all(&dir).unwrap();
        assert!(exported.is_err());
    }

    #[test]
    fn settings_edited_by_hand_stay_within_limits() {
        let dir = std::env::temp_dir().join(format!("obs-lite-range-{}", std::process::id()));
        let path = dir.join("settings.json");
        let absolute = std::env::temp_dir().join("recordings");
        let load = |json: String| {
            write_replacing(&path, json.as_bytes()).unwrap();
            Config::load_from(&path)
        };
        let wild = load(r#"{"replay_seconds": 86400, "fps": 0, "mic_volume": 5.0, "output_dir": ""}"#.to_owned());
        let huge = load(r#"{"replay_seconds": 1, "fps": 3000000000, "system_volume": -1.0, "output_dir": "videos"}"#.to_owned());
        let fine = load(serde_json::json!({"replay_seconds": 90, "fps": 30, "mic_volume": 0.5, "output_dir": absolute}).to_string());
        fs::remove_dir_all(&dir).unwrap();
        // A day of video in memory, and recordings wherever obs-lite was started from.
        assert_eq!((wild.replay_seconds, wild.fps, wild.mic_volume), (600, 1, 1.0));
        assert_eq!(wild.output_dir, Config::default().output_dir);
        assert_eq!((huge.replay_seconds, huge.fps, huge.system_volume), (5, 240, 0.0));
        assert_eq!(huge.output_dir, Config::default().output_dir);
        assert_eq!((fine.replay_seconds, fine.fps, fine.mic_volume, fine.output_dir), (90, 30, 0.5, absolute));
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

    #[test]
    fn a_setting_that_cannot_be_read_only_loses_itself() {
        // A value from a newer version, after going back to this one, and a hand edit of the
        // wrong type: every other setting stays.
        let json = r#"{"fps": 30, "resolution": "P2160", "cursor": "yes", "last_recording": "cut.mp4"}"#;
        let c = Config::parse(json).expect("every setting was lost");
        assert_eq!((c.fps, c.resolution, c.cursor), (30, Resolution::Native, true));
        assert_eq!(c.last_recording, Some(PathBuf::from("cut.mp4")), "the recording to repair");
    }
}
