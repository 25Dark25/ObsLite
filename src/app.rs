//! UI and recording lifecycle.
//!
//! The layout follows OBS Studio (preview on top; Sources, Audio Mixer and Controls
//! docks below; status bar; Settings dialog) and uses its Yami palette and Spanish
//! wording, so OBS users find everything where they expect it.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Button, Color32, ComboBox, CornerRadius, FontId, Layout, Margin, Rect, RichText, Sense,
    Slider, Stroke, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};

use crate::audio::{self, Device, Input, InputKind, METER_FLOOR, Mixer};
use crate::camera::{self, Camera, CameraConfig, CameraDevice, CameraFrame, CameraSize, Corner};
use crate::capture::{self, RecordConfig, Session, SessionOptions, Source, SourceId};
use crate::platform::encoder::{Encoder, PendingFile};
use crate::config::{Config, Hotkeys, Quality, Region, Resolution};
use crate::hotkeys::{self, Binding, GlobalHotkeys};
use crate::i18n::{self, Language, tr};
use crate::tray::{Tray, TrayCommand, TrayState};
use crate::update::{self, Release};

const PREVIEW_FPS: u32 = 5;
const DOCK_HEIGHT: f32 = 236.0;

// OBS "Yami" theme palette.
const GREY1: Color32 = Color32::from_rgb(0x5B, 0x62, 0x73);
const GREY3: Color32 = Color32::from_rgb(0x46, 0x4B, 0x59);
const GREY4: Color32 = Color32::from_rgb(0x3C, 0x40, 0x4D);
const GREY6: Color32 = Color32::from_rgb(0x27, 0x2A, 0x33);
const GREY7: Color32 = Color32::from_rgb(0x1D, 0x1F, 0x26);
const GREY8: Color32 = Color32::from_rgb(0x13, 0x14, 0x1A);
const PRIMARY: Color32 = Color32::from_rgb(0x28, 0x4C, 0xB8);
const PRIMARY_LIGHT: Color32 = Color32::from_rgb(0x47, 0x6B, 0xD7);
const TEXT_MUTED: Color32 = Color32::from_rgb(0x96, 0x96, 0x96);
const RED: Color32 = Color32::from_rgb(0xE3, 0x3B, 0x57);
const YELLOW: Color32 = Color32::from_rgb(0xE5, 0xAF, 0x24);
const GREEN: Color32 = Color32::from_rgb(0x37, 0xD2, 0x47);
const RED_DIM: Color32 = Color32::from_rgb(0x7D, 0x12, 0x24);
const YELLOW_DIM: Color32 = Color32::from_rgb(0x6E, 0x52, 0x0D);
const GREEN_DIM: Color32 = Color32::from_rgb(0x17, 0x64, 0x1E);

#[derive(Clone, Copy)]
enum Action {
    ToggleRecording,
    TogglePause,
    ToggleReplay,
    SaveReplay,
    Screenshot,
    ToggleMicMute,
}

enum RecState {
    Idle,
    /// Capturing and encoding, for a recording, the replay buffer or both.
    Active { session: Session, mixer: Option<Mixer>, recording: Option<Recording>, replay: bool },
    /// The last output was stopped and the recording is being written out.
    Saving(Receiver<Result<PathBuf, String>>),
}

/// A recording in progress: where it starts on the session clock, which may have been running
/// for the replay buffer.
struct Recording {
    since: Duration,
}

struct Status {
    text: String,
    file: Option<PathBuf>,
    error: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum SettingsPage {
    General,
    Output,
    Audio,
    Video,
    Hotkeys,
}

/// The Settings dialog edits a copy; tr!("Aceptar", "OK") applies it, tr!("Cancelar", "Cancel") discards it.
struct SettingsDialog {
    draft: Config,
    page: SettingsPage,
    /// The hotkey row waiting for a key combination.
    capturing: Option<usize>,
}

/// Process CPU usage as a share of all cores, like the OBS status bar.
struct CpuMeter {
    last: Option<(Instant, u64)>,
    percent: f64,
}

pub struct App {
    config: Config,
    sources: Vec<Source>,
    selected: Option<SourceId>,
    system_devices: Vec<Device>,
    mic_devices: Vec<Device>,
    preview: Option<Session>,
    rec: RecState,
    /// A screenshot being written to disk.
    screenshot: Option<Receiver<Result<PathBuf, String>>>,
    system: Option<Input>,
    mic: Option<Input>,
    /// Application Audio Capture, when its application is running.
    app_audio: Option<Input>,
    audio_apps: Vec<capture::AudioApp>,
    cameras: Vec<CameraDevice>,
    camera: Option<Camera>,
    camera_texture: Option<TextureHandle>,
    camera_seq: Option<u64>,
    meters: [f32; 3],
    /// For audio inputs to wake the UI when they start to make sound.
    ctx: egui::Context,
    /// When the window was last drawn, to tell when it is not shown (see `preview_hidden`).
    last_drawn: Instant,
    texture: Option<TextureHandle>,
    texture_seq: Option<u64>,
    /// Choosing a region on the preview, and the drag in progress (start, end).
    picking_region: bool,
    region_drag: Option<(egui::Pos2, egui::Pos2)>,
    status: Option<Status>,
    settings: Option<SettingsDialog>,
    cpu: CpuMeter,
    actions: Arc<Mutex<Vec<Action>>>,
    hotkeys: Option<GlobalHotkeys>,
    /// The action of each bound hotkey, by index, shared with the hotkey handler.
    bound_actions: Arc<Mutex<Vec<Action>>>,
    /// The main window, for excluding it from capture.
    native_window: Option<isize>,
    tray: Option<Tray>,
    tray_commands: Arc<Mutex<Vec<TrayCommand>>>,
    /// Minimized to the tray.
    hidden: bool,
    /// A newer release, once the start-up check has answered.
    update: Option<Release>,
    update_check: Option<Receiver<Option<Release>>>,
    /// The dialog telling of `update` is open.
    update_dialog: bool,
    /// Files being written in the background (a stopped recording, a replay).
    jobs: Vec<JoinHandle<Status>>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let config = Config::load();
        i18n::set(config.language.unwrap_or_else(Language::system));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let bound_actions: Arc<Mutex<Vec<Action>>> = Arc::default();
        let on_press = {
            let (actions, bound, ctx) = (actions.clone(), bound_actions.clone(), cc.egui_ctx.clone());
            move |index: usize| {
                if let Some(action) = bound.lock().unwrap().get(index).copied() {
                    actions.lock().unwrap().push(action);
                    ctx.request_repaint();
                }
            }
        };
        let (hotkeys, hotkey_error) = match GlobalHotkeys::new(on_press) {
            Ok(hotkeys) => (Some(hotkeys), None),
            Err(e) => (None, Some(e)),
        };

        let mut app = Self {
            system: None,
            mic: None,
            app_audio: None,
            audio_apps: Vec::new(),
            cameras: Vec::new(),
            camera: None,
            camera_texture: None,
            camera_seq: None,
            config,
            sources: Vec::new(),
            selected: None,
            system_devices: Vec::new(),
            mic_devices: Vec::new(),
            preview: None,
            rec: RecState::Idle,
            screenshot: None,
            meters: [0.0; 3],
            ctx: cc.egui_ctx.clone(),
            last_drawn: Instant::now(),
            texture: None,
            texture_seq: None,
            picking_region: false,
            region_drag: None,
            status: None,
            settings: None,
            cpu: CpuMeter { last: None, percent: 0.0 },
            actions,
            hotkeys,
            bound_actions,
            native_window: crate::platform::sys::window_handle(cc),
            tray: None,
            tray_commands: Arc::new(Mutex::new(Vec::new())),
            hidden: false,
            update: None,
            update_check: None,
            update_dialog: false,
            jobs: Vec::new(),
        };
        // The Flatpak has no network access, for privacy; Linux users follow the Releases page.
        if cfg!(windows) && app.config.check_updates {
            let (tx, rx) = mpsc::channel();
            let ctx = cc.egui_ctx.clone();
            thread::spawn(move || {
                // Offline or rate-limited: say nothing.
                let _ = tx.send(update::newer_release().ok().flatten());
                ctx.request_repaint();
            });
            app.update_check = Some(rx);
        }
        let commands = app.tray_commands.clone();
        match Tray::new(&cc.egui_ctx, crate::ICON_RGBA, 64, move |c| commands.lock().unwrap().push(c)) {
            Ok(tray) => app.tray = Some(tray),
            Err(e) => app.set_status(tr!("Icono de la bandeja no disponible: {}", "Tray icon unavailable: {}", e), None, true),
        }
        app.apply_capture_visibility();
        app.refresh_devices();
        // The application first: desktop audio leaves it out.
        app.restart_app_audio();
        app.restart_input(InputKind::System);
        app.restart_input(InputKind::Microphone);
        app.refresh_sources();
        app.restart_preview(&cc.egui_ctx);
        app.restart_camera(&cc.egui_ctx);
        app.bind_hotkeys();
        if let Some(e) = hotkey_error {
            app.set_status(tr!("Atajos globales no disponibles: {}", "Global hotkeys unavailable: {}", e), None, true);
        }
        app.recover_last_recording();
        app
    }

    /// A recording obs-lite was writing when it stopped (a crash, a power cut) is still a
    /// fragmented MP4: make it a regular one. A finished recording is left as it is.
    fn recover_last_recording(&mut self) {
        let Some(path) = self.config.last_recording.take() else { return };
        if let Ok(true) = crate::mp4::repair(&path) {
            let shown = crate::platform::sys::display_path(&path);
            self.set_status(tr!("Se recuperó la grabación interrumpida '{}'", "Recovered the interrupted recording '{}'", shown.display()), Some(path), false);
        }
    }

    fn is_recording(&self) -> bool {
        matches!(self.rec, RecState::Active { recording: Some(_), .. })
    }

    fn is_replaying(&self) -> bool {
        matches!(self.rec, RecState::Active { replay: true, .. })
    }

    /// The capture session of a recording or the replay buffer.
    fn active_session(&self) -> Option<&Session> {
        match &self.rec {
            RecState::Active { session, .. } => Some(session),
            _ => None,
        }
    }

    fn is_paused(&self) -> bool {
        self.is_recording() && self.active_session().is_some_and(|s| s.shared.clock.is_paused())
    }

    /// Recording and the encoder clock is running (pausing before that would be lost).
    fn is_live(&self) -> bool {
        self.is_recording() && self.active_session().is_some_and(|s| s.shared.encoder_ready.load(Ordering::Acquire))
    }

    fn refresh_sources(&mut self) {
        self.sources = capture::list_sources();
        let still_there = self.selected.is_some_and(|id| self.sources.iter().any(|s| s.id == id));
        if !still_there {
            let wanted = self.config.source_label.as_deref();
            self.selected = self
                .sources
                .iter()
                .find(|s| Some(s.label.as_str()) == wanted)
                .or_else(|| self.sources.first())
                .map(|s| s.id);
        }
    }

    fn refresh_devices(&mut self) {
        self.system_devices = audio::list_devices(InputKind::System);
        self.mic_devices = audio::list_devices(InputKind::Microphone);
        // A saved device that disappeared falls back to the Windows default.
        let keep = |id: &mut Option<String>, devices: &[Device]| {
            if id.as_ref().is_some_and(|id| !devices.iter().any(|d| &d.id == id)) {
                *id = None;
            }
        };
        keep(&mut self.config.system_device, &self.system_devices);
        keep(&mut self.config.mic_device, &self.mic_devices);
    }

    fn selected_source(&self) -> Option<&Source> {
        self.selected.and_then(|id| self.sources.iter().find(|s| s.id == id))
    }

    fn select_source(&mut self, ctx: &egui::Context, id: SourceId) {
        self.selected = Some(id);
        // A region belongs to the source it was drawn on.
        self.config.region = None;
        self.config.source_label = self.selected_source().map(|s| s.label.clone());
        self.restart_preview(ctx);
    }

    fn set_status(&mut self, text: impl Into<String>, file: Option<PathBuf>, error: bool) {
        self.status = Some(Status { text: text.into(), file, error });
    }

    fn channel(&self, kind: InputKind) -> (bool, Option<String>, f32) {
        let c = &self.config;
        match kind {
            InputKind::System | InputKind::SystemExcept(_) => {
                (c.system_audio, c.system_device.clone(), channel_gain(c.system_volume, c.system_muted))
            }
            InputKind::Microphone => (c.mic, c.mic_device.clone(), channel_gain(c.mic_volume, c.mic_muted)),
            InputKind::Application(_) => (c.app_audio.is_some(), None, channel_gain(c.app_volume, c.app_muted)),
        }
    }

    /// Captures the configured application's audio, if that application is running now.
    fn start_app_audio(&mut self) {
        self.app_audio = None;
        let Some(exe) = &self.config.app_audio else { return };
        let Some(app) = capture::list_audio_apps().into_iter().find(|a| a.exe.eq_ignore_ascii_case(exe)) else { return };
        let (_, _, gain) = self.channel(InputKind::Application(app.pid));
        self.app_audio = Some(Input::start(InputKind::Application(app.pid), None, gain, Some(self.ctx.clone())));
    }

    /// Restarts the application's capture and, when that changes what desktop audio has to
    /// leave out, desktop audio too.
    fn restart_app_audio(&mut self) {
        self.start_app_audio();
        let wanted = system_kind(self.app_audio.as_ref().map(|a| a.kind), self.config.system_device.as_deref());
        if self.system.as_ref().is_some_and(|s| s.kind != wanted) {
            self.restart_input(InputKind::System);
        }
    }

    fn restart_input(&mut self, kind: InputKind) {
        let kind = match kind {
            InputKind::System | InputKind::SystemExcept(_) => {
                system_kind(self.app_audio.as_ref().map(|a| a.kind), self.config.system_device.as_deref())
            }
            other => other,
        };
        let (enabled, device, gain) = self.channel(kind);
        let slot = match kind {
            InputKind::System | InputKind::SystemExcept(_) => &mut self.system,
            InputKind::Microphone => &mut self.mic,
            InputKind::Application(_) => &mut self.app_audio,
        };
        *slot = None; // release the device before reopening it
        *slot = enabled.then(|| Input::start(kind, device, gain, Some(self.ctx.clone())));
        if kind == InputKind::Microphone
            && let Some(mic) = &self.mic
        {
            mic.shared.set_denoise(self.config.mic_denoise);
        }
    }

    fn restart_camera(&mut self, ctx: &egui::Context) {
        (self.camera, self.camera_texture, self.camera_seq) = (None, None, None);
        if let Some(c) = &self.config.camera {
            self.camera = Some(Camera::start(c.device.clone(), Some(ctx.clone())));
        }
    }

    /// The camera overlay, when a camera is configured and running.
    fn overlay(&self) -> Option<(Corner, CameraSize)> {
        self.camera.as_ref().and(self.config.camera.as_ref()).map(|c| (c.corner, c.size))
    }

    fn restart_preview(&mut self, ctx: &egui::Context) {
        self.preview = None;
        self.texture_seq = None;
        if self.active_session().is_some() {
            return;
        }
        let Some(source) = self.selected else { return };
        let opts = SessionOptions {
            source,
            fps: PREVIEW_FPS,
            cursor: self.config.cursor,
            preview_fps: PREVIEW_FPS,
            record: None,
        };
        match Session::start(opts, Some(ctx.clone())) {
            Ok(s) => self.preview = Some(s),
            Err(e) => self.source_lost(tr!("No se pudo capturar la fuente: {}", "Could not capture the source: {}", e)),
        }
    }

    /// The selected source cannot be captured (e.g. its window was closed): stop showing
    /// it as the source and drop its last frame from the preview.
    fn source_lost(&mut self, message: String) {
        self.preview = None;
        self.selected = None;
        self.texture = None;
        self.texture_seq = None;
        self.set_status(message, None, true);
    }

    fn handle(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::ToggleRecording => match self.rec {
                RecState::Saving(_) => {}
                RecState::Active { recording: Some(_), .. } => self.stop_recording(ctx),
                _ => self.start_recording(ctx),
            },
            Action::ToggleReplay => match self.rec {
                RecState::Saving(_) => {}
                RecState::Active { replay: true, .. } => self.stop_replay(ctx),
                _ => self.start_replay(ctx),
            },
            Action::SaveReplay => self.save_replay(ctx),
            Action::TogglePause => {
                if self.is_live()
                    && let Some(session) = self.active_session()
                {
                    let clock = &session.shared.clock;
                    clock.set_paused(!clock.is_paused());
                }
            }
            Action::Screenshot => self.take_screenshot(ctx),
            Action::ToggleMicMute => {
                self.config.mic_muted = !self.config.mic_muted;
                if let Some(mic) = &self.mic {
                    mic.shared.set_gain(channel_gain(self.config.mic_volume, self.config.mic_muted));
                }
            }
        }
    }

    fn start_recording(&mut self, ctx: &egui::Context) {
        if let Err(e) = std::fs::create_dir_all(&self.config.output_dir) {
            return self.set_status(tr!("No se puede usar la ruta de grabación: {}", "Cannot use the recording path: {}", e), None, true);
        }
        if !self.start_session(ctx) {
            return;
        }
        let RecState::Active { session, recording, .. } = &mut self.rec else { return };
        let shared = &session.shared;
        // The clock starts with the encoder; if the replay buffer started it, count from now.
        let since = if shared.encoder_ready.load(Ordering::Acquire) { shared.clock.elapsed() } else { Duration::ZERO };
        let path = self.config.output_dir.join(format!("{}.mp4", chrono::Local::now().format("%Y-%m-%d %H-%M-%S")));
        shared.outputs.lock().unwrap().start_file(path.clone());
        *recording = Some(Recording { since });
        self.status = None;
        // Saved now, so the next start can repair the file if obs-lite stops while writing it.
        self.config.last_recording = Some(path);
        self.config.save();
    }

    fn start_replay(&mut self, ctx: &egui::Context) {
        if !self.start_session(ctx) {
            return;
        }
        let RecState::Active { session, replay, .. } = &mut self.rec else { return };
        session.shared.outputs.lock().unwrap().set_replay(Some(self.config.replay_seconds));
        *replay = true;
    }

    /// Saves the replay buffer as a new file, in the background.
    fn save_replay(&mut self, ctx: &egui::Context) {
        let Some(session) = self.active_session().filter(|_| self.is_replaying()) else {
            return self.set_status(tr!("Inicia el búfer de repetición para poder guardar repeticiones.", "Start the replay buffer to save replays."), None, true);
        };
        let replay = session.shared.outputs.lock().unwrap().replay();
        let replay = match replay {
            Ok(replay) => replay,
            Err(e) => return self.set_status(tr!("No se pudo guardar la repetición: {}", "Could not save the replay: {}", e), None, true),
        };
        let name = format!("{} {}.mp4", tr!("Repetición", "Replay"), chrono::Local::now().format("%Y-%m-%d %H-%M-%S"));
        let path = self.config.output_dir.join(name);
        self.spawn_job(ctx, move || match std::fs::create_dir_all(path.parent().unwrap_or(&path)).map_err(|e| e.to_string()).and_then(|()| replay.save(path)) {
            Ok(path) => saved(tr!("Repetición guardada en '{}'", "Replay saved to '{}'", crate::platform::sys::display_path(&path).display()), path),
            Err(e) => failed(tr!("No se pudo guardar la repetición: {}", "Could not save the replay: {}", e)),
        });
    }

    /// Runs `job` on another thread; its status is shown when it ends, and exit waits for it.
    fn spawn_job(&mut self, ctx: &egui::Context, job: impl FnOnce() -> Status + Send + 'static) {
        let ctx = ctx.clone();
        self.jobs.push(thread::spawn(move || {
            let status = job();
            ctx.request_repaint();
            status
        }));
    }

    /// Starts capturing and encoding for a recording or the replay buffer, unless already
    /// doing so. Returns whether the session is running.
    fn start_session(&mut self, ctx: &egui::Context) -> bool {
        match self.rec {
            RecState::Active { .. } => return true,
            RecState::Saving(_) => return false,
            RecState::Idle => {}
        }
        let Some(source) = self.selected else {
            self.set_status(tr!("Añade una fuente con el botón + del panel Fuentes.", "Add a source with the + button in the Sources dock."), None, true);
            return false;
        };
        // One capture session at a time: the recording session also feeds the preview.
        self.preview = None;

        // The application may have started after obs-lite.
        if self.app_audio.is_none() {
            self.restart_app_audio();
        }
        let inputs: Vec<_> =
            [&self.system, &self.mic, &self.app_audio].into_iter().flatten().map(|i| i.shared.clone()).collect();
        let opts = SessionOptions {
            source,
            fps: self.config.fps,
            cursor: self.config.cursor,
            preview_fps: PREVIEW_FPS,
            record: Some(RecordConfig {
                quality: self.config.quality,
                resolution: self.config.resolution,
                audio: !inputs.is_empty(),
                region: self.config.region,
                overlay: self.overlay(),
            }),
        };
        let session = match Session::start(opts, Some(ctx.clone())) {
            Ok(s) => s,
            Err(e) => {
                self.restart_preview(ctx);
                self.set_status(tr!("No se pudo iniciar la grabación: {}", "Could not start recording: {}", e), None, true);
                return false;
            }
        };
        let mixer = (!inputs.is_empty()).then(|| {
            let shared = session.shared.clone();
            Mixer::start(inputs, shared.encoder_ready.clone(), shared.clock.clone(), move |pcm| {
                if let Some(enc) = shared.encoder.lock().unwrap().as_ref() {
                    enc.push_audio(pcm);
                }
            })
        });
        if let Some(camera) = &self.camera {
            let shared = session.shared.clone();
            *camera.shared.sink.lock().unwrap() = Some(Box::new(move |frame: &CameraFrame| {
                // Scale outside the encoder lock: audio and screen frames wait on it, and audio
                // arriving late made the encoder emit black frames.
                let size = (frame.width, frame.height);
                let Some(rect) = shared.encoder.lock().unwrap().as_ref().and_then(|e| e.overlay_rect(size)) else { return };
                let pixels = camera::resize(&frame.bgra, size, (rect.2, rect.3));
                if let Some(enc) = shared.encoder.lock().unwrap().as_ref() {
                    enc.push_overlay(rect, &pixels);
                }
            }));
        }
        self.rec = RecState::Active { session, mixer, recording: None, replay: false };
        true
    }

    fn stop_recording(&mut self, ctx: &egui::Context) {
        let RecState::Active { session, recording, replay, .. } = &mut self.rec else { return };
        if recording.is_none() {
            return;
        }
        if !*replay {
            return self.stop_session(ctx, None);
        }
        *recording = None;
        // The replay buffer keeps the session (and its clock) going: only the file ends.
        session.shared.clock.set_paused(false);
        let file = session.shared.outputs.lock().unwrap().take_file();
        self.set_status(tr!("Guardando grabación…", "Saving recording…"), None, false);
        self.spawn_job(ctx, move || match file.map(PendingFile::finish) {
            Some(Ok(path)) => saved(tr!("Grabación guardada en '{}'", "Recording saved to '{}'", crate::platform::sys::display_path(&path).display()), path),
            Some(Err(e)) => failed(tr!("Error al guardar la grabación: {}", "Error saving the recording: {}", e)),
            None => failed(tr!("no se capturó ningún fotograma", "no frames were captured").into()),
        });
    }

    fn stop_replay(&mut self, ctx: &egui::Context) {
        let RecState::Active { session, recording, replay, .. } = &mut self.rec else { return };
        *replay = false;
        if recording.is_none() {
            return self.stop_session(ctx, None);
        }
        session.shared.outputs.lock().unwrap().set_replay(None);
    }

    /// Ends capture and encoding, writing out the recording if there is one.
    fn stop_session(&mut self, ctx: &egui::Context, reason: Option<String>) {
        let RecState::Active { session, mixer, recording, .. } = std::mem::replace(&mut self.rec, RecState::Idle) else {
            return;
        };
        drop(mixer);
        if let Some(camera) = &self.camera {
            *camera.shared.sink.lock().unwrap() = None;
        }
        let (encoder, stop_error) = session.stop();
        if recording.is_none() {
            // Only the replay buffer: nothing to write; let the encoder wind down elsewhere.
            if let Some(encoder) = encoder {
                thread::spawn(move || encoder.finish().map(PendingFile::finish));
            }
            self.restart_preview(ctx);
            if let Some(reason) = reason {
                self.set_status(reason, None, true);
            }
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        // Finalizing the MP4 can take a moment; keep the UI responsive.
        thread::spawn(move || {
            let result = match encoder.map(Encoder::finish) {
                Some(Some(file)) => file.finish(),
                _ => Err(stop_error.unwrap_or_else(|| tr!("no se capturó ningún fotograma", "no frames were captured").into())),
            };
            let _ = tx.send(result);
            ctx2.request_repaint();
        });
        self.rec = RecState::Saving(rx);
        self.set_status(reason.unwrap_or_else(|| tr!("Guardando grabación…", "Saving recording…").into()), None, false);
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.hidden) {
            self.set_taskbar_button(true);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn set_taskbar_button(&self, visible: bool) {
        if let Some(window) = self.native_window {
            crate::platform::sys::set_taskbar_button(window, visible);
        }
    }

    /// Saves the newest frame of the current source as a PNG in the recordings folder.
    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let Some(session) = self.active_session().or(self.preview.as_ref()) else {
            return self.set_status(tr!("Añade una fuente con el botón + del panel Fuentes.", "Add a source with the + button in the Sources dock."), None, true);
        };
        let (mut width, mut height, mut rgba) = match session.snapshot() {
            Ok(image) => image,
            Err(e) => return self.set_status(tr!("No se pudo hacer la captura: {}", "Could not take the screenshot: {}", e), None, true),
        };
        if let Some(region) = self.config.region.and_then(|r| r.fit((width, height))) {
            rgba = capture::crop_rgba(&rgba, width, region);
            (width, height) = (region.width, region.height);
        }
        if let (Some((corner, size)), Some(camera)) = (self.overlay(), self.camera.as_ref())
            && let Some(frame) = camera.shared.latest.lock().unwrap().clone()
        {
            paste_camera(&mut rgba, width, height, &frame, corner, size);
        }
        let name = format!("{} {}.png", tr!("Captura", "Screenshot"), chrono::Local::now().format("%Y-%m-%d %H-%M-%S"));
        let path = self.config.output_dir.join(name);
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        // PNG encoding of a large frame takes a moment; keep the UI responsive.
        thread::spawn(move || {
            let result = std::fs::create_dir_all(path.parent().unwrap_or(&path))
                .map_err(|e| e.to_string())
                .and_then(|()| capture::write_png(&path, width, height, &rgba))
                .map(|()| path);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.screenshot = Some(rx);
    }

    /// Background bookkeeping; runs even while the window is minimized.
    fn poll(&mut self, ctx: &egui::Context) {
        let actions: Vec<Action> = self.actions.lock().unwrap().drain(..).collect();
        for action in actions {
            self.handle(ctx, action);
        }
        if let Some(e) = self.hotkeys.as_ref().and_then(GlobalHotkeys::take_error) {
            self.set_status(tr!("Atajos globales no disponibles: {}", "Global hotkeys unavailable: {}", e), None, true);
        }
        if let Some(rx) = &self.update_check
            && let Ok(release) = rx.try_recv()
        {
            self.update_dialog = release.as_ref().is_some_and(|r| self.config.skipped_version.as_ref() != Some(&r.version));
            (self.update, self.update_check) = (release, None);
        }
        let commands: Vec<TrayCommand> = self.tray_commands.lock().unwrap().drain(..).collect();
        for command in commands {
            match command {
                TrayCommand::ToggleRecording => self.handle(ctx, Action::ToggleRecording),
                TrayCommand::TogglePause => self.handle(ctx, Action::TogglePause),
                TrayCommand::ToggleReplay => self.handle(ctx, Action::ToggleReplay),
                TrayCommand::SaveReplay => self.handle(ctx, Action::SaveReplay),
                TrayCommand::Screenshot => self.handle(ctx, Action::Screenshot),
                TrayCommand::Show => self.show_window(ctx),
                TrayCommand::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
        // Minimize to tray: the window stays minimized (a hidden window would stop eframe from
        // handling hotkeys and tray clicks) and only loses its taskbar button.
        let minimized = ctx.input(|i| i.viewport().minimized);
        if self.config.minimize_to_tray && !self.hidden && minimized == Some(true) {
            self.set_taskbar_button(false);
            self.hidden = true;
        } else if self.hidden && minimized == Some(false) {
            // Restored some other way, e.g. Alt+Tab.
            self.set_taskbar_button(true);
            self.hidden = false;
        }
        let state = TrayState {
            recording: self.is_recording(),
            paused: self.is_paused(),
            replay: self.is_replaying(),
            english: i18n::english(),
        };
        if let Some(tray) = &mut self.tray {
            tray.update(state);
        }

        if let RecState::Saving(rx) = &self.rec
            && let Ok(result) = rx.try_recv()
        {
            self.rec = RecState::Idle;
            // Before the status: if the source is gone, its error must not hide where the file went.
            self.restart_preview(ctx);
            match result {
                Ok(path) => self.set_status(tr!("Grabación guardada en '{}'", "Recording saved to '{}'", crate::platform::sys::display_path(&path).display()), Some(path), false),
                Err(e) => self.set_status(tr!("Error al guardar la grabación: {}", "Error saving the recording: {}", e), None, true),
            }
        }

        if let Some(rx) = &self.screenshot
            && let Ok(result) = rx.try_recv()
        {
            self.screenshot = None;
            match result {
                Ok(path) => self.set_status(tr!("Captura guardada en '{}'", "Screenshot saved to '{}'", crate::platform::sys::display_path(&path).display()), Some(path), false),
                Err(e) => self.set_status(tr!("No se pudo guardar la captura: {}", "Could not save the screenshot: {}", e), None, true),
            }
        }

        let (done, running) = std::mem::take(&mut self.jobs).into_iter().partition(JoinHandle::is_finished);
        self.jobs = running;
        for job in done {
            if let Ok(status) = job.join() {
                self.status = Some(status);
            }
        }

        // A recording that cannot be written (its folder, a full disk, a failed encoder) is stopped
        // now, not when the user stops it, so the error shows right away.
        if let RecState::Active { session, recording: Some(_), .. } = &self.rec
            && session.shared.outputs.lock().unwrap().failed()
        {
            return self.stop_recording(ctx);
        }
        if self.active_session().is_some_and(Session::is_finished) {
            let reason = if self.is_recording() {
                tr!("La fuente dejó de estar disponible; guardando…", "The source is no longer available; saving…")
            } else {
                tr!("La fuente dejó de estar disponible; búfer de repetición detenido.", "The source is no longer available; replay buffer stopped.")
            };
            return self.stop_session(ctx, Some(reason.into()));
        }
        if self.preview.as_ref().is_some_and(Session::is_finished) {
            self.source_lost(tr!("La fuente ya no está disponible. Añade otra con el botón +.", "The source is no longer available. Add another one with the + button.").into());
        }
        self.cpu.update();
    }

    fn update_camera_texture(&mut self, ctx: &egui::Context) {
        let Some(camera) = &self.camera else { return };
        let seq = camera.shared.seq.load(Ordering::Relaxed);
        if self.camera_seq == Some(seq) {
            return;
        }
        let Some(frame) = camera.shared.latest.lock().unwrap().clone() else { return };
        self.camera_seq = Some(seq);
        let rgba: Vec<u8> = frame.bgra.as_chunks::<4>().0.iter().flat_map(|&[b, g, r, a]| [r, g, b, a]).collect();
        let image = egui::ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &rgba);
        match &mut self.camera_texture {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => self.camera_texture = Some(ctx.load_texture("camera", image, TextureOptions::LINEAR)),
        }
    }

    fn update_texture(&mut self, ctx: &egui::Context) {
        let Some(session) = self.active_session().or(self.preview.as_ref()) else { return };
        let (seq, image) = {
            let guard = session.shared.preview.lock().unwrap();
            let Some(frame) = guard.as_ref() else { return };
            if self.texture_seq == Some(frame.seq) && self.texture.is_some() {
                return;
            }
            (frame.seq, egui::ColorImage::from_rgba_unmultiplied(frame.size, &frame.rgba))
        };
        self.texture_seq = Some(seq);
        match &mut self.texture {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("preview", image, TextureOptions::LINEAR)),
        }
    }

    fn open_settings(&mut self) {
        self.refresh_devices();
        self.settings = Some(SettingsDialog { draft: self.config.clone(), page: SettingsPage::General, capturing: None });
        // Registered combinations never reach the window, so they could not be typed again.
        self.unbind_hotkeys();
    }

    fn apply_settings(&mut self, new: Config) {
        let old = std::mem::replace(&mut self.config, new);
        let c = &self.config;
        let system_changed = (old.system_audio, &old.system_device) != (c.system_audio, &c.system_device);
        let mic_changed = (old.mic, &old.mic_device) != (c.mic, &c.mic_device);
        if old.language != c.language {
            i18n::set(c.language.unwrap_or_else(Language::system));
            // Source names are translated ("Pantalla 1" / "Display 1").
            self.refresh_sources();
            self.config.source_label = self.selected_source().map(|s| s.label.clone());
        }
        if system_changed {
            self.restart_input(InputKind::System);
        }
        if mic_changed {
            self.restart_input(InputKind::Microphone);
        }
        if let Some(mic) = &self.mic {
            mic.shared.set_denoise(self.config.mic_denoise);
        }
        self.apply_capture_visibility();
        self.config.save();
    }

    /// Registers the configured hotkeys, reporting the ones another application already uses.
    fn bind_hotkeys(&mut self) {
        let Some(hotkeys) = &mut self.hotkeys else { return };
        let Hotkeys { record, pause, screenshot, mute_mic, replay, save_replay } = &self.config.hotkeys;
        let wanted = [
            (record, Action::ToggleRecording, "record", tr!("Iniciar / Detener Grabación", "Start / Stop Recording")),
            (pause, Action::TogglePause, "pause", tr!("Pausar / Reanudar Grabación", "Pause / Resume Recording")),
            (screenshot, Action::Screenshot, "screenshot", tr!("Hacer captura de pantalla", "Take Screenshot")),
            (mute_mic, Action::ToggleMicMute, "mute-mic", tr!("Silenciar / Activar Mic/Aux", "Mute / Unmute Mic/Aux")),
            (replay, Action::ToggleReplay, "replay", tr!("Iniciar / Detener búfer de repetición", "Start / Stop Replay Buffer")),
            (save_replay, Action::SaveReplay, "save-replay", tr!("Guardar repetición", "Save Replay")),
        ];
        let (bindings, actions): (Vec<Binding>, Vec<Action>) = wanted
            .into_iter()
            .filter_map(|(text, action, id, description)| {
                Some((Binding { id, description: description.to_owned(), text: text.clone()? }, action))
            })
            .unzip();
        *self.bound_actions.lock().unwrap() = actions;
        let failed = hotkeys.bind(&bindings);
        if !failed.is_empty() {
            let list = failed.join(", ");
            self.set_status(tr!("Atajos no disponibles (¿los usa otra aplicación?): {}", "Hotkeys unavailable (used by another application?): {}", list), None, true);
        }
    }

    fn unbind_hotkeys(&mut self) {
        if let Some(hotkeys) = &mut self.hotkeys {
            hotkeys.unbind();
        }
    }

    fn apply_capture_visibility(&self) {
        if let Some(window) = self.native_window {
            crate::platform::sys::exclude_from_capture(window, self.config.hide_from_capture);
        }
    }

    // ------------------------------------------------------------------ main window

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(tr!("Archivo", "File"), |ui| {
                if ui.button(tr!("Hacer captura de pantalla", "Take Screenshot")).clicked() {
                    self.take_screenshot(ui.ctx());
                    ui.close();
                }
                if ui.button(tr!("Mostrar grabaciones", "Show Recordings")).clicked() {
                    crate::platform::sys::open_folder(&self.config.output_dir);
                    ui.close();
                }
                if ui.button(tr!("Ajustes", "Settings")).clicked() {
                    self.open_settings();
                    ui.close();
                }
                ui.separator();
                if ui.button(tr!("Salir", "Exit")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }

    fn preview_area(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let source = self.active_session().or(self.preview.as_ref()).and_then(Session::source_size);
        let mut screenshot = false;
        response.context_menu(|ui| {
            if ui.button(tr!("Hacer captura de pantalla", "Take Screenshot")).clicked() {
                screenshot = true;
                ui.close();
            }
        });
        if screenshot {
            self.take_screenshot(ui.ctx());
        }
        let painter = ui.painter_at(rect);
        let aspect = self.texture.as_ref().map_or(16.0 / 9.0, |t| t.size_vec2().x / t.size_vec2().y);
        let size = if rect.width() / rect.height() > aspect {
            vec2(rect.height() * aspect, rect.height())
        } else {
            vec2(rect.width(), rect.width() / aspect)
        };
        let canvas = Rect::from_center_size(rect.center(), size);
        // Ask for a preview as sharp as it is drawn on screen (but never larger than the source).
        if let Some(session) = self.active_session().or(self.preview.as_ref()) {
            let pixels = (canvas.width() * ui.ctx().pixels_per_point()).round() as usize;
            session.shared.preview_width.store(pixels, Ordering::Relaxed);
        }
        painter.rect_filled(canvas, 0.0, Color32::BLACK);
        match &self.texture {
            Some(texture) => {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.image(texture.id(), canvas, uv, Color32::WHITE);
            }
            None => {
                let msg = if self.selected.is_none() { tr!("Añade una fuente con el botón +", "Add a source with the + button") } else { "" };
                painter.text(canvas.center(), Align2::CENTER_CENTER, msg, FontId::proportional(15.0), TEXT_MUTED);
            }
        }
        let Some(source) = source.filter(|_| self.texture.is_some()) else { return };
        let area = self.config.region.and_then(|r| r.fit(source));
        if let (Some((corner, size)), Some(texture)) = (self.overlay(), &self.camera_texture)
            && !self.picking_region
        {
            // The overlay sits in the recorded area: the region, or the whole source.
            let area = area.unwrap_or(Region { x: 0, y: 0, width: source.0, height: source.1 });
            let (x, y, w, h) = camera::overlay_rect((area.width, area.height), (texture.size()[0] as u32, texture.size()[1] as u32), corner, size);
            let rect = region_rect(Region { x: area.x + x, y: area.y + y, width: w, height: h }, canvas, source);
            painter.image(texture.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        if self.picking_region {
            self.pick_region(ui, &response, &painter, canvas, source);
        } else if let Some(region) = self.config.region.and_then(|r| r.fit(source)) {
            outline_region(&painter, canvas, region_rect(region, canvas, source));
        }
    }

    /// Drag on the preview to choose the region to record; Escape cancels.
    fn pick_region(&mut self, ui: &egui::Ui, response: &egui::Response, painter: &egui::Painter, canvas: Rect, source: (u32, u32)) {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            (self.picking_region, self.region_drag) = (false, None);
            return;
        }
        let pointer = response.interact_pointer_pos();
        if response.drag_started()
            && let Some(p) = pointer
        {
            self.region_drag = Some((p, p));
        }
        if let (Some(drag), Some(p)) = (&mut self.region_drag, pointer) {
            drag.1 = p;
        }
        match self.region_drag {
            Some((start, end)) => {
                let selection = Rect::from_two_pos(start, end).intersect(canvas);
                outline_region(painter, canvas, selection);
                if response.drag_stopped() {
                    self.config.region = region_from_rect(selection, canvas, source);
                    (self.picking_region, self.region_drag) = (false, None);
                }
            }
            None => {
                painter.rect_filled(canvas, 0.0, Color32::from_black_alpha(90));
                let hint = tr!("Arrastra para elegir la región que se grabará · Esc para cancelar", "Drag to choose the region to record · Esc to cancel");
                painter.text(canvas.center(), Align2::CENTER_CENTER, hint, FontId::proportional(15.0), Color32::WHITE);
            }
        }
    }

    fn docks(&mut self, ui: &mut egui::Ui) {
        let gap = 4.0;
        let width = ui.available_width() - gap * 2.0;
        let height = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            dock(ui, tr!("Fuentes", "Sources"), vec2(width * 0.30, height), |ui| self.sources_dock(ui));
            dock(ui, tr!("Mezclador de audio", "Audio Mixer"), vec2(width * 0.44, height), |ui| self.mixer_dock(ui));
            dock(ui, tr!("Controles", "Controls"), vec2(width * 0.26, height), |ui| self.controls_dock(ui));
        });
    }

    fn sources_dock(&mut self, ui: &mut egui::Ui) {
        let recording = matches!(self.rec, RecState::Active { .. });
        let ctx = ui.ctx().clone();
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_enabled_ui(!recording, |ui| {
                ui.horizontal(|ui| {
                    let mut camera_choice = None;
                    #[cfg_attr(not(windows), allow(unused_mut))]
                    let mut app_choice: Option<String> = None;
                    let add = ui.menu_button(RichText::new(" + ").strong(), |ui| {
                        let mut chosen = None;
                        // The portal's dialog lets the user pick the screen or window.
                        #[cfg(target_os = "linux")]
                        if ui.button(tr!("Captura de pantalla o ventana…", "Screen or Window Capture…")).clicked() {
                            capture::forget_choice();
                            chosen = Some(SourceId::Portal);
                        }
                        #[cfg(windows)]
                        ui.menu_button(tr!("Captura de pantalla", "Display Capture"), |ui| {
                            for s in self.sources.iter().filter(|s| matches!(s.id, SourceId::Monitor(_))) {
                                if ui.button(&s.label).clicked() {
                                    chosen = Some(s.id);
                                }
                            }
                        });
                        #[cfg(windows)]
                        ui.menu_button(tr!("Captura de ventana", "Window Capture"), |ui| {
                            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                                for s in self.sources.iter().filter(|s| matches!(s.id, SourceId::Window(_))) {
                                    if ui.button(&s.label).clicked() {
                                        chosen = Some(s.id);
                                    }
                                }
                            });
                        });
                        #[cfg(windows)]
                        ui.menu_button(tr!("Captura de audio de aplicación", "Application Audio Capture"), |ui| {
                            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                                for app in &self.audio_apps {
                                    if ui.button(format!("{}  ·  {}", app.exe, app.title)).clicked() {
                                        app_choice = Some(app.exe.clone());
                                    }
                                }
                            });
                        });
                        ui.menu_button(tr!("Dispositivo de captura de vídeo", "Video Capture Device"), |ui| {
                            if self.cameras.is_empty() {
                                ui.label(RichText::new(tr!("No se ha encontrado ninguna cámara", "No camera found")).color(TEXT_MUTED));
                            }
                            for device in &self.cameras {
                                if ui.button(&device.name).clicked() {
                                    camera_choice = Some(device.clone());
                                }
                            }
                        });
                        chosen
                    });
                    // Windows come and go; refresh the list each time the menu opens.
                    if add.response.clicked() {
                        self.refresh_sources();
                        self.cameras = camera::list_devices();
                        self.audio_apps = capture::list_audio_apps();
                    }
                    if let Some(Some(id)) = add.inner {
                        self.select_source(&ctx, id);
                        ui.close();
                    }
                    add.response.on_hover_text(tr!("Añadir fuente", "Add source"));
                    if let Some(exe) = app_choice {
                        self.config.app_audio = Some(exe);
                        self.restart_app_audio();
                        ui.close();
                    }
                    if let Some(device) = camera_choice {
                        let (corner, size) = self.config.camera.as_ref().map_or((Corner::BottomRight, CameraSize::Medium), |c| (c.corner, c.size));
                        self.config.camera = Some(CameraConfig { device: device.id, name: device.name, corner, size });
                        self.restart_camera(&ctx);
                        ui.close();
                    }
                    ui.menu_button(tr!("Propiedades", "Properties"), |ui| {
                        if ui.checkbox(&mut self.config.cursor, tr!("Capturar el cursor", "Capture Cursor")).changed() {
                            self.restart_preview(&ctx);
                        }
                        ui.separator();
                        if ui.add_enabled(self.selected.is_some(), Button::new(tr!("Seleccionar región…", "Select Region…"))).clicked() {
                            self.picking_region = true;
                            ui.close();
                        }
                        let whole = Button::new(tr!("Grabar toda la fuente", "Record Whole Source"));
                        if ui.add_enabled(self.config.region.is_some(), whole).clicked() {
                            self.config.region = None;
                            ui.close();
                        }
                        if self.config.app_audio.is_some() {
                            ui.separator();
                            if ui.button(tr!("Quitar audio de aplicación", "Remove Application Audio")).clicked() {
                                self.config.app_audio = None;
                                self.restart_app_audio();
                                ui.close();
                            }
                        }
                        let mut remove_camera = false;
                        if let Some(cam) = &mut self.config.camera {
                            ui.separator();
                            ui.label(RichText::new(tr!("Cámara", "Camera")).strong());
                            for corner in Corner::ALL {
                                ui.radio_value(&mut cam.corner, corner, corner.label());
                            }
                            ui.separator();
                            for size in CameraSize::ALL {
                                ui.radio_value(&mut cam.size, size, size.label());
                            }
                            ui.separator();
                            remove_camera = ui.button(tr!("Quitar cámara", "Remove Camera")).clicked();
                        }
                        if remove_camera {
                            self.config.camera = None;
                            self.restart_camera(&ctx);
                            ui.close();
                        }
                    });
                });
            });
            ui.add_space(2.0);
            ui.with_layout(Layout::top_down(Align::Min), |ui| match self.selected_source() {
                Some(source) => {
                    let kind = match source.id {
                        SourceId::Monitor(_) => tr!("Captura de pantalla", "Display Capture"),
                        SourceId::Window(_) => tr!("Captura de ventana", "Window Capture"),
                        #[cfg(target_os = "linux")]
                        SourceId::Portal => tr!("Captura de pantalla o ventana", "Screen or Window Capture"),
                    };
                    // Before the label, which is often too long to show in full.
                    let region = self.config.region.map_or(String::new(), |r| {
                        format!(" ({} {}×{})", tr!("región", "region"), r.width, r.height)
                    });
                    list_row(ui, &format!("{kind}{region}  ·  {}", source.label));
                    if let Some(exe) = &self.config.app_audio {
                        ui.add_space(2.0);
                        let state = if self.app_audio.is_some() { String::new() } else { tr!(" (no se está ejecutando)", " (not running)").to_owned() };
                        list_row(ui, &format!("{}  ·  {exe}{state}", tr!("Captura de audio de aplicación", "Application Audio Capture")));
                    }
                    if let Some(cam) = &self.config.camera {
                        ui.add_space(2.0);
                        let error = self.camera.as_ref().and_then(|c| c.shared.error.lock().unwrap().clone());
                        let text = format!("{}  ·  {}", tr!("Dispositivo de captura de vídeo", "Video Capture Device"), cam.name);
                        let row = list_row(ui, &text);
                        if let Some(e) = error {
                            row.on_hover_text(tr!("Error de la cámara: {}", "Camera error: {}", e));
                        }
                    }
                }
                None => {
                    ui.label(RichText::new(tr!("No hay ninguna fuente. Pulsa + para añadir una.", "No sources. Press + to add one.")).color(TEXT_MUTED));
                }
            });
        });
    }

    fn mixer_dock(&mut self, ui: &mut egui::Ui) {
        // Three channels do not always fit the dock's height.
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.mixer_channels(ui));
    }

    fn mixer_channels(&mut self, ui: &mut egui::Ui) {
        let app_label = self.config.app_audio.clone().unwrap_or_default();
        let mut any = false;
        for i in 0..3 {
            let c = &mut self.config;
            let (label, volume, muted, input) = match i {
                0 => (tr!("Audio del escritorio", "Desktop Audio").to_owned(), &mut c.system_volume, &mut c.system_muted, &self.system),
                1 => ("Mic/Aux".to_owned(), &mut c.mic_volume, &mut c.mic_muted, &self.mic),
                _ => (app_label.clone(), &mut c.app_volume, &mut c.app_muted, &self.app_audio),
            };
            // Disabled devices are not shown, as in OBS.
            let Some(input) = input else {
                self.meters[i] = 0.0;
                continue;
            };
            any = true;
            let peak = input.shared.peak() * channel_gain(*volume, *muted);
            // Fast attack, slow release.
            self.meters[i] = if peak > self.meters[i] { peak } else { self.meters[i] * 0.85 };
            let db = 20.0 * self.meters[i].max(1e-6).log10();

            ui.horizontal(|ui| {
                ui.label(label);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let text = if db <= -60.0 { "-inf dB".to_owned() } else { format!("{db:.1} dB") };
                    ui.label(RichText::new(text).monospace().small().color(TEXT_MUTED));
                    if let Some(e) = input.shared.error() {
                        ui.label(RichText::new("Error").small().color(RED)).on_hover_text(e);
                    }
                });
            });
            meter(ui, db);
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 40.0).max(60.0);
                let changed = ui.add(Slider::new(volume, 0.0..=1.0).show_value(false)).changed();
                let tip = if *muted { tr!("Quitar silencio", "Unmute") } else { tr!("Silenciar", "Mute") };
                let toggled = mute_button(ui, *muted).on_hover_text(tip).clicked();
                if toggled {
                    *muted = !*muted;
                }
                if changed || toggled {
                    input.shared.set_gain(channel_gain(*volume, *muted));
                }
            });
            ui.add_space(8.0);
        }
        let limiting = matches!(&self.rec, RecState::Active { mixer: Some(mixer), .. } if mixer.limiting());
        if limiting {
            ui.label(RichText::new(tr!("Mezcla al límite: baja algún volumen", "Mix at its limit: lower a volume")).small().color(RED)).on_hover_text(tr!(
                "Los canales juntos superan el máximo. Para que no se distorsione, la grabación baja el volumen en esos momentos.",
                "The channels add up to more than the maximum. To keep it from distorting, the recording lowers the volume at those moments."
            ));
        }
        if !any {
            ui.label(RichText::new(tr!("No hay dispositivos de audio activos.\nActívalos en Ajustes › Audio.", "No active audio devices.\nEnable them in Settings › Audio.")).color(TEXT_MUTED));
        }
    }

    fn controls_dock(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let full = ui.available_width();
        let height = 30.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            match &self.rec {
                RecState::Idle | RecState::Active { recording: None, .. } => {
                    if ui.add_sized([full, height], Button::new(tr!("Iniciar Grabación", "Start Recording"))).on_hover_text(hotkey_hint(&self.config.hotkeys.record)).clicked() {
                        self.handle(&ctx, Action::ToggleRecording);
                    }
                }
                RecState::Active { .. } => {
                    let stop = Button::new(RichText::new(tr!("Detener Grabación", "Stop Recording")).color(Color32::WHITE)).fill(PRIMARY);
                    if ui.add_sized([full - height - 4.0, height], stop).on_hover_text(hotkey_hint(&self.config.hotkeys.record)).clicked() {
                        self.handle(&ctx, Action::ToggleRecording);
                    }
                    let paused = self.is_paused();
                    let tip = if paused { tr!("Reanudar Grabación", "Resume Recording") } else { tr!("Pausar Grabación", "Pause Recording") };
                    if icon_button(ui, if paused { Icon::Play } else { Icon::Pause }, height, self.is_live(), paused)
                        .on_hover_text(format!("{tip} {}", hotkey_hint(&self.config.hotkeys.pause)))
                        .clicked()
                    {
                        self.handle(&ctx, Action::TogglePause);
                    }
                }
                RecState::Saving(_) => {
                    ui.add_enabled(false, Button::new(tr!("Guardando…", "Saving…")).min_size(vec2(full, height)));
                }
            }
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let hint = hotkey_hint(&self.config.hotkeys.replay);
            let saving = matches!(self.rec, RecState::Saving(_));
            if self.is_replaying() {
                let stop = Button::new(RichText::new(tr!("Detener búfer de repetición", "Stop Replay Buffer")).color(Color32::WHITE)).fill(PRIMARY);
                if ui.add_sized([full - height - 4.0, height], stop).on_hover_text(hint).clicked() {
                    self.handle(&ctx, Action::ToggleReplay);
                }
                let tip = format!("{} {}", tr!("Guardar repetición", "Save Replay"), hotkey_hint(&self.config.hotkeys.save_replay));
                if icon_button(ui, Icon::Save, height, true, false).on_hover_text(tip).clicked() {
                    self.handle(&ctx, Action::SaveReplay);
                }
            } else {
                let start = Button::new(tr!("Iniciar búfer de repetición", "Start Replay Buffer"));
                let tip = tr!("Guarda en memoria los últimos {} s, para guardarlos cuando quieras. {}", "Keeps the last {} s in memory, to save whenever you want. {}", self.config.replay_seconds, hint);
                if ui.add_enabled_ui(!saving, |ui| ui.add_sized([full, height], start)).inner.on_hover_text(tip).clicked() {
                    self.handle(&ctx, Action::ToggleReplay);
                }
            }
        });
        ui.add_space(2.0);
        if ui.add_sized([full, height], Button::new(tr!("Ajustes", "Settings"))).clicked() {
            self.open_settings();
        }
        ui.add_space(2.0);
        if ui.add_sized([full, height], Button::new(tr!("Salir", "Exit"))).clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(status) = &self.status {
                let color = if status.error { RED } else { TEXT_MUTED };
                let label = ui.add(
                    egui::Label::new(RichText::new(&status.text).small().color(color)).truncate().sense(Sense::click()),
                );
                if let Some(file) = &status.file
                    && label.on_hover_text(tr!("Mostrar en la carpeta", "Show in folder")).clicked()
                {
                    crate::platform::sys::show_in_folder(file);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                self.recording_indicator(ui);
                ui.label(RichText::new(format!("CPU: {:.1}%", self.cpu.percent)).small().color(TEXT_MUTED));
                if let Some(release) = &self.update {
                    let text = tr!("Nueva versión {} disponible", "New version {} available", release.version);
                    let link = ui.add(egui::Label::new(RichText::new(text).small().color(PRIMARY_LIGHT).underline()).sense(Sense::click()));
                    if link.on_hover_text(&release.url).clicked() {
                        open_url(&release.url);
                    }
                }
            });
        });
    }

    fn recording_indicator(&self, ui: &mut egui::Ui) {
        if self.is_replaying() {
            ui.label(RichText::new(tr!("Búfer de repetición activo", "Replay buffer active")).small().color(PRIMARY_LIGHT));
        }
        let (Some(session), RecState::Active { recording: Some(recording), .. }) = (self.active_session(), &self.rec) else {
            status_icon(ui, Icon::Record, GREY3, "00:00:00");
            return;
        };
        let shared = &session.shared;
        if !shared.encoder_ready.load(Ordering::Acquire) {
            // Hardware encoder start-up takes 0.2–1 s (more when cold); nothing is recorded yet.
            ui.label(RichText::new(tr!("Preparando…", "Starting…")).small().color(TEXT_MUTED));
            ui.spinner();
            return;
        }
        let secs = shared.clock.elapsed().saturating_sub(recording.since).as_secs();
        let time = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
        if shared.clock.is_paused() {
            status_icon(ui, Icon::Pause, YELLOW, &time);
        } else {
            status_icon(ui, Icon::Record, RED, &time);
        }
        if let Some((encoded, lagged)) = shared.encoder.lock().unwrap().as_ref().map(|e| e.stats())
            && lagged > 0
        {
            let pct = lagged as f64 * 100.0 / (encoded + lagged).max(1) as f64;
            ui.label(RichText::new(tr!("Fotogramas perdidos {} ({:.1}%)", "Dropped frames {} ({:.1}%)", lagged, pct)).small().color(YELLOW))
                .on_hover_text(tr!("El equipo no da abasto: prueba a bajar la resolución o los FPS en Ajustes › Vídeo", "This computer can't keep up: try a lower resolution or FPS in Settings › Video"));
        }
    }

    // ------------------------------------------------------------------ settings dialog

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.settings else { return };
        let recording = matches!(self.rec, RecState::Active { .. });
        // Read before the dialog runs: Escape there ends the wait for a hotkey, and must not then
        // also close the dialog.
        let capturing = dialog.capturing.is_some();
        let mut decision = None;
        let response = egui::Modal::new(egui::Id::unique("settings"))
            .frame(egui::Frame::new().fill(GREY7).stroke(Stroke::new(1.0, GREY4)).corner_radius(6).inner_margin(12))
            .show(ctx, |ui| {
                ui.set_width(660.0);
                ui.heading(tr!("Ajustes", "Settings"));
                ui.add_space(8.0);
                // Fixed body size: a modal has unbounded height, so nothing may grow to fill it.
                let body = 300.0;
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(vec2(130.0, body), Layout::top_down(Align::Min), |ui| {
                        for (page, name) in [
                            (SettingsPage::General, "General"),
                            (SettingsPage::Output, tr!("Salida", "Output")),
                            (SettingsPage::Audio, "Audio"),
                            (SettingsPage::Video, tr!("Vídeo", "Video")),
                            (SettingsPage::Hotkeys, tr!("Atajos", "Hotkeys")),
                        ] {
                            let selected = dialog.page == page;
                            let button = Button::new(name)
                                .fill(if selected { PRIMARY } else { Color32::TRANSPARENT })
                                .min_size(vec2(130.0, 28.0));
                            if ui.add(button).clicked() {
                                // A hotkey wait only makes sense on its own page.
                                (dialog.page, dialog.capturing) = (page, None);
                            }
                        }
                    });
                    let (line, _) = ui.allocate_exact_size(vec2(9.0, body), Sense::hover());
                    ui.painter().vline(line.center().x, line.y_range(), Stroke::new(1.0, GREY4));
                    ui.allocate_ui_with_layout(vec2(ui.available_width(), body), Layout::top_down(Align::Min), |ui| {
                        ui.set_min_height(body);
                        if recording {
                            ui.label(RichText::new(tr!("Detén la grabación y el búfer de repetición para cambiar estos ajustes.", "Stop recording and the replay buffer to change these settings.")).color(YELLOW));
                            ui.add_space(6.0);
                        }
                        ui.add_enabled_ui(!recording, |ui| {
                            settings_page(ui, dialog, &self.system_devices, &self.mic_devices);
                        });
                    });
                });
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new(tr!("Cancelar", "Cancel")).min_size(vec2(90.0, 26.0))).clicked() {
                        decision = Some(false);
                    }
                    let ok = Button::new(RichText::new(tr!("Aceptar", "OK")).color(Color32::WHITE)).fill(PRIMARY);
                    if ui.add(ok.min_size(vec2(90.0, 26.0))).clicked() {
                        decision = Some(true);
                    }
                });
            });
        if response.should_close() && decision.is_none() && !capturing {
            decision = Some(false);
        }
        match decision {
            Some(true) => {
                let draft = self.settings.take().expect("dialog is open").draft;
                self.apply_settings(draft);
                self.bind_hotkeys();
            }
            Some(false) => {
                self.settings = None;
                self.bind_hotkeys();
            }
            None => {}
        }
    }

    /// Waits for the settings dialog to close: OK there would save its older copy of the config.
    fn update_dialog(&mut self, ctx: &egui::Context) {
        let Some(release) = self.update.as_ref().filter(|_| self.update_dialog && self.settings.is_none()) else { return };
        let mut skip = false;
        let response = egui::Modal::new(egui::Id::unique("update"))
            .frame(egui::Frame::new().fill(GREY7).stroke(Stroke::new(1.0, GREY4)).corner_radius(6).inner_margin(16))
            .show(ctx, |ui| {
                ui.heading(tr!("Nueva versión disponible", "New version available"));
                ui.add_space(8.0);
                ui.label(tr!(
                    "Ya está disponible obs-lite {}. Tienes la {}.",
                    "obs-lite {} is available. You have {}.",
                    release.version,
                    env!("CARGO_PKG_VERSION")
                ));
                ui.add_space(14.0);
                // The buttons, the widest row, set the dialog's width (it differs by language).
                ui.horizontal(|ui| {
                    if ui.add(Button::new(tr!("No recordar esta versión", "Don't remind me of this version")).min_size(vec2(0.0, 26.0))).clicked() {
                        skip = true;
                        ui.close();
                    }
                    if ui.add(Button::new(tr!("Cerrar", "Close")).min_size(vec2(90.0, 26.0))).clicked() {
                        ui.close();
                    }
                    let download = Button::new(RichText::new(tr!("Descargar", "Download")).color(Color32::WHITE)).fill(PRIMARY);
                    if ui.add(download.min_size(vec2(90.0, 26.0))).on_hover_text(&release.url).clicked() {
                        open_url(&release.url);
                        ui.close();
                    }
                });
            });
        if skip {
            self.config.skipped_version = Some(release.version.clone());
            self.config.save();
        }
        if response.should_close() {
            self.update_dialog = false;
        }
    }
}

/// Opens a web page in the default browser.
fn open_url(url: &str) {
    let _ = std::process::Command::new("explorer").arg(url).spawn();
}

fn settings_page(ui: &mut egui::Ui, dialog: &mut SettingsDialog, system: &[Device], mic: &[Device]) {
    let SettingsDialog { draft: c, page, capturing } = dialog;
    let grid = |id: &str| egui::Grid::new(id).num_columns(2).spacing([16.0, 10.0]);
    match page {
        SettingsPage::General => {
            group_title(ui, "General");
            grid("general").show(ui, |ui| {
                ui.label(tr!("Idioma", "Language"));
                let options = [None, Some(Language::Spanish), Some(Language::English)];
                let name = |l: Option<Language>| {
                    l.map_or(tr!("Automático (idioma del sistema)", "Automatic (system language)"), Language::name).to_owned()
                };
                combo(ui, "language", &mut c.language, &options, name, 260.0);
                ui.end_row();
                ui.label(tr!("Bandeja del sistema", "System Tray"));
                ui.checkbox(&mut c.minimize_to_tray, tr!("Minimizar a la bandeja del sistema", "Minimize to the system tray"));
                ui.end_row();
                // Neither applies on Linux: the Flatpak has no network access, and Wayland
                // cannot keep a window out of screen captures.
                if cfg!(windows) {
                    ui.label(tr!("Actualizaciones", "Updates"));
                    ui.checkbox(&mut c.check_updates, tr!("Buscar versiones nuevas al iniciar", "Check for new versions at startup"));
                    ui.end_row();
                    ui.label(tr!("Captura", "Capture"));
                    ui.checkbox(&mut c.hide_from_capture, tr!("Ocultar obs-lite de la captura de pantalla", "Hide obs-lite from screen capture"));
                    ui.end_row();
                }
            });
        }
        SettingsPage::Output => {
            group_title(ui, tr!("Grabación", "Recording"));
            grid("output").show(ui, |ui| {
                ui.label(tr!("Ruta de grabación", "Recording Path"));
                ui.horizontal(|ui| {
                    let shown = crate::platform::sys::display_path(&c.output_dir);
                    let mut path = shown.display().to_string();
                    // A folder granted through the folder picker (in the Linux sandbox) shows
                    // where it really is, which cannot be typed back in: only Browse changes it.
                    let editable = shown == c.output_dir;
                    let field = egui::TextEdit::singleline(&mut path).desired_width(290.0).interactive(editable);
                    if ui.add(field).changed() {
                        c.output_dir = PathBuf::from(path);
                    }
                    if ui.button(tr!("Examinar", "Browse")).clicked()
                        && let Some(dir) = rfd::FileDialog::new().set_directory(&c.output_dir).pick_folder()
                    {
                        c.output_dir = dir;
                    }
                });
                ui.end_row();
                ui.label(tr!("Calidad de grabación", "Recording Quality"));
                combo(ui, "quality", &mut c.quality, &Quality::ALL, |q| q.label().into(), 360.0);
                ui.end_row();
                ui.label(tr!("Formato de grabación", "Recording Format"));
                ui.label("MP4");
                ui.end_row();
                ui.label(tr!("Codificador", "Encoder"));
                ui.label(tr!("H.264 por hardware (por software si la GPU no tiene)", "Hardware H.264 (software if the GPU has none)"));
                ui.end_row();
            });
            ui.add_space(8.0);
            group_title(ui, tr!("Búfer de repetición", "Replay Buffer"));
            grid("replay").show(ui, |ui| {
                ui.label(tr!("Tiempo máximo de repetición", "Maximum Replay Time"));
                ui.add(egui::DragValue::new(&mut c.replay_seconds).range(5..=600).suffix(" s"));
                ui.end_row();
            });
        }
        SettingsPage::Audio => {
            group_title(ui, tr!("Dispositivos de audio globales", "Global Audio Devices"));
            grid("audio").show(ui, |ui| {
                ui.label(tr!("Audio del escritorio", "Desktop Audio"));
                device_combo(ui, "sysdev", &mut c.system_audio, &mut c.system_device, system);
                ui.end_row();
                ui.label(tr!("Dispositivo de audio Mic/auxiliar", "Mic/Auxiliary Audio"));
                device_combo(ui, "micdev", &mut c.mic, &mut c.mic_device, mic);
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut c.mic_denoise, tr!("Supresión de ruido del micrófono (RNNoise)", "Microphone noise suppression (RNNoise)"));
                ui.end_row();
            });
        }
        SettingsPage::Video => {
            group_title(ui, tr!("Vídeo", "Video"));
            grid("video").show(ui, |ui| {
                ui.label(tr!("Resolución de salida (Escalada)", "Output (Scaled) Resolution"));
                combo(ui, "res", &mut c.resolution, &Resolution::ALL, |r| r.label().into(), 200.0);
                ui.end_row();
                ui.label(tr!("Valores comunes de FPS", "Common FPS Values"));
                combo(ui, "fps", &mut c.fps, &[30, 60], |f| f.to_string(), 200.0);
                ui.end_row();
            });
        }
        SettingsPage::Hotkeys => {
            group_title(ui, tr!("Atajos globales", "Global Hotkeys"));
            let Hotkeys { record, pause, screenshot, mute_mic, replay, save_replay } = &mut c.hotkeys;
            let rows = [
                (tr!("Iniciar / Detener Grabación", "Start / Stop Recording"), record),
                (tr!("Pausar / Reanudar Grabación", "Pause / Resume Recording"), pause),
                (tr!("Iniciar / Detener búfer de repetición", "Start / Stop Replay Buffer"), replay),
                (tr!("Guardar repetición", "Save Replay"), save_replay),
                (tr!("Hacer captura de pantalla", "Take Screenshot"), screenshot),
                (tr!("Silenciar / Activar Mic/Aux", "Mute / Unmute Mic/Aux"), mute_mic),
            ];
            grid("hotkeys").show(ui, |ui| {
                let mut slots = Vec::new();
                let mut set = None;
                for (i, (label, slot)) in rows.into_iter().enumerate() {
                    ui.label(label);
                    if hotkey_field(ui, i, slot, capturing) {
                        set = Some(i);
                    }
                    slots.push(slot);
                    ui.end_row();
                }
                if let Some(row) = set {
                    take_from_others(&mut slots, row);
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new(tr!("Funcionan aunque obs-lite esté minimizado o en segundo plano.", "They work even when obs-lite is minimized or in the background.")).small().color(TEXT_MUTED));
            #[cfg(target_os = "linux")]
            ui.label(
                RichText::new(tr!(
                    "El escritorio guarda los atajos: obs-lite le propone estos, y se pueden cambiar en la configuración del sistema.",
                    "The desktop keeps the shortcuts: obs-lite proposes these, and they can be changed in the system settings."
                ))
                .small()
                .color(TEXT_MUTED),
            );
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        let hidden = preview_hidden(ctx.input(|i| i.viewport().visible()), self.last_drawn.elapsed());
        if let Some(session) = self.active_session().or(self.preview.as_ref()) {
            session.shared.preview_hidden.store(hidden, Ordering::Relaxed);
        }
        ctx.request_repaint_after(repaint_interval(self.active_session().is_some(), &self.meters));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.last_drawn = Instant::now();
        let ctx = ui.ctx().clone();
        self.update_texture(&ctx);
        self.update_camera_texture(&ctx);
        let bar = egui::Frame::new().fill(GREY7).inner_margin(Margin::symmetric(8, 3));
        egui::Panel::top("menu").frame(bar).show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").frame(bar.inner_margin(Margin::symmetric(10, 4))).show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom("docks")
            .frame(egui::Frame::new().fill(GREY7).inner_margin(Margin::symmetric(4, 4)))
            .resizable(false)
            .exact_size(DOCK_HEIGHT)
            .show(ui, |ui| self.docks(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(GREY8).inner_margin(Margin::same(6)))
            .show(ui, |ui| self.preview_area(ui));
        self.settings_dialog(&ctx);
        self.update_dialog(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.config.save();
        finish_on_exit(std::mem::replace(&mut self.rec, RecState::Idle));
        for job in self.jobs.drain(..) {
            let _ = job.join();
        }
    }
}

fn saved(text: String, file: PathBuf) -> Status {
    Status { text, file: Some(file), error: false }
}

fn failed(text: String) -> Status {
    Status { text, file: None, error: true }
}

/// Never leaves the MP4 half-written when the app closes mid-recording or mid-save.
fn finish_on_exit(rec: RecState) {
    match rec {
        RecState::Active { session, mixer, .. } => {
            drop(mixer);
            if let (Some(enc), _) = session.stop()
                && let Some(file) = enc.finish()
            {
                let _ = file.finish();
            }
        }
        RecState::Saving(rx) => {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
        RecState::Idle => {}
    }
}

impl CpuMeter {
    fn update(&mut self) {
        let now = Instant::now();
        if self.last.is_some_and(|(at, _)| now.duration_since(at) < Duration::from_secs(1)) {
            return;
        }
        let Some(cpu) = crate::platform::sys::process_cpu_time() else { return };
        if let Some((at, before)) = self.last {
            let cores = thread::available_parallelism().map_or(1, |n| n.get()) as f64;
            let wall = now.duration_since(at).as_secs_f64() * 1e7; // 100 ns units
            self.percent = (cpu.saturating_sub(before)) as f64 / wall / cores * 100.0;
        }
        self.last = Some((now, cpu));
    }
}

/// Whether the window is not shown, so the capture need not make preview pictures. Windows says
/// when the window is minimized (`visible`); Wayland never does, but there the compositor stops
/// asking for frames, so the window is not drawn. A shown window draws at least once a second
/// (`repaint_interval`).
fn preview_hidden(visible: Option<bool>, since_drawn: Duration) -> bool {
    visible == Some(false) || since_drawn > Duration::from_secs(3)
}

/// How soon to draw again. Meters move at 15 fps while there is sound, and the timer while
/// recording; a silent, idle window only needs the CPU figure, once a second (an input that
/// starts to make sound wakes it at once, and the preview and camera wake it for new pictures).
fn repaint_interval(active: bool, meters: &[f32]) -> Duration {
    if active || meters.iter().any(|m| *m > METER_FLOOR) { Duration::from_millis(66) } else { Duration::from_secs(1) }
}

/// Slider position (0..1) to linear gain; squared for a more natural loudness curve.
fn channel_gain(volume: f32, muted: bool) -> f32 {
    if muted { 0.0 } else { volume * volume }
}

/// Desktop audio leaves out the application captured on its own (`app`), which it would
/// otherwise repeat. Leaving a process out is not tied to an output device, so with a chosen
/// one (`system_device`) desktop audio stays that device's whole output.
fn system_kind(app: Option<InputKind>, system_device: Option<&str>) -> InputKind {
    match (app, system_device) {
        (Some(InputKind::Application(pid)), None) => InputKind::SystemExcept(pid),
        _ => InputKind::System,
    }
}

/// Where `region` of a `source`-sized image falls on the preview `canvas`.
fn region_rect(region: Region, canvas: Rect, source: (u32, u32)) -> Rect {
    let scale = vec2(canvas.width() / source.0 as f32, canvas.height() / source.1 as f32);
    let min = canvas.min + vec2(region.x as f32, region.y as f32) * scale;
    Rect::from_min_size(min, vec2(region.width as f32, region.height as f32) * scale)
}

/// The source region under `selection` on the preview `canvas`.
fn region_from_rect(selection: Rect, canvas: Rect, source: (u32, u32)) -> Option<Region> {
    let scale = vec2(source.0 as f32 / canvas.width(), source.1 as f32 / canvas.height());
    let min = (selection.min - canvas.min) * scale;
    let size = selection.size() * scale;
    let px = |v: f32| v.round().max(0.0) as u32;
    Region { x: px(min.x), y: px(min.y), width: px(size.x), height: px(size.y) }.fit(source)
}

/// Dims the preview outside `inner` and outlines it.
fn outline_region(painter: &egui::Painter, canvas: Rect, inner: Rect) {
    let shade = Color32::from_black_alpha(140);
    for part in [
        Rect::from_min_max(canvas.min, pos2(canvas.max.x, inner.min.y)),
        Rect::from_min_max(pos2(canvas.min.x, inner.max.y), canvas.max),
        Rect::from_min_max(pos2(canvas.min.x, inner.min.y), pos2(inner.min.x, inner.max.y)),
        Rect::from_min_max(pos2(inner.max.x, inner.min.y), pos2(canvas.max.x, inner.max.y)),
    ] {
        if part.is_positive() {
            painter.rect_filled(part, 0.0, shade);
        }
    }
    painter.rect_stroke(inner, 0.0, Stroke::new(1.5, PRIMARY_LIGHT), egui::StrokeKind::Inside);
}

/// Draws the webcam onto an RGBA screenshot where the recording would show it.
fn paste_camera(rgba: &mut [u8], width: u32, height: u32, frame: &CameraFrame, corner: Corner, size: CameraSize) {
    let (x, y, w, h) = camera::overlay_rect((width, height), (frame.width, frame.height), corner, size);
    let scaled = camera::resize(&frame.bgra, (frame.width, frame.height), (w, h));
    for (row, src) in scaled.chunks(w as usize * 4).enumerate() {
        let start = ((y as usize + row) * width as usize + x as usize) * 4;
        for (dst, &[b, g, r, a]) in rgba[start..start + src.len()].as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
            *dst = [r, g, b, a];
        }
    }
}

/// An OBS-style dock: titled panel with a border.
fn dock(ui: &mut egui::Ui, title: &str, size: Vec2, content: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
        egui::Frame::new().fill(GREY6).stroke(Stroke::new(1.0, GREY4)).corner_radius(4).show(ui, |ui| {
            ui.set_min_size(size - vec2(2.0, 2.0));
            ui.set_max_width(size.x - 2.0);
            egui::Frame::new().fill(GREY7).inner_margin(Margin::symmetric(8, 4)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(title).strong());
            });
            egui::Frame::new().inner_margin(Margin::same(8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(ui.available_height());
                content(ui);
            });
        });
    });
}

/// A selected list item, as in the OBS Sources list.
fn list_row(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().rect_filled(rect, 3.0, PRIMARY);
    let clip = rect.shrink2(vec2(8.0, 0.0));
    ui.painter().with_clip_rect(clip).text(
        pos2(clip.left(), rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.5),
        Color32::WHITE,
    );
    response.on_hover_text(text)
}

fn group_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).strong().size(14.0));
    ui.add_space(4.0);
}

/// OBS-style volume meter: dim colour zones, lit up to the current level, with a dB scale.
fn meter(ui: &mut egui::Ui, db: f32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
    let bar = Rect::from_min_size(rect.min, vec2(width, 7.0));
    let painter = ui.painter();
    let x_of = |db: f32| bar.left() + bar.width() * ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let level = x_of(db);
    for (from, to, dim, lit) in [(-60.0, -20.0, GREEN_DIM, GREEN), (-20.0, -9.0, YELLOW_DIM, YELLOW), (-9.0, 0.0, RED_DIM, RED)] {
        let zone = Rect::from_x_y_ranges(x_of(from)..=x_of(to), bar.y_range());
        painter.rect_filled(zone, 0.0, dim);
        if level > zone.left() {
            painter.rect_filled(Rect::from_x_y_ranges(zone.left()..=level.min(zone.right()), bar.y_range()), 0.0, lit);
        }
    }
    for tick in [-60, -50, -40, -30, -20, -10, 0] {
        let x = x_of(tick as f32);
        painter.line_segment([pos2(x, bar.bottom() + 1.0), pos2(x, bar.bottom() + 3.0)], Stroke::new(1.0, GREY1));
        let align = match tick {
            -60 => Align2::LEFT_TOP,
            0 => Align2::RIGHT_TOP,
            _ => Align2::CENTER_TOP,
        };
        painter.text(pos2(x, bar.bottom() + 3.0), align, tick.to_string(), FontId::proportional(9.0), TEXT_MUTED);
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Record,
    Pause,
    Play,
    Save,
}

/// Draws an icon centred on `c`; drawn with shapes because the bundled fonts lack these glyphs.
fn paint_icon(painter: &egui::Painter, icon: Icon, c: egui::Pos2, s: f32, color: Color32) {
    match icon {
        Icon::Record => {
            painter.circle_filled(c, s * 0.8, color);
        }
        Icon::Pause => {
            let bar = vec2(s * 0.55, s * 1.8);
            painter.rect_filled(Rect::from_center_size(c - vec2(s * 0.45, 0.0), bar), 1.0, color);
            painter.rect_filled(Rect::from_center_size(c + vec2(s * 0.45, 0.0), bar), 1.0, color);
        }
        Icon::Play => {
            let pts = vec![c + vec2(-s * 0.7, -s), c + vec2(-s * 0.7, s), c + vec2(s, 0.0)];
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Icon::Save => {
            // A downward arrow onto a tray.
            let stroke = Stroke::new(1.6, color);
            painter.line_segment([c + vec2(0.0, -s), c + vec2(0.0, s * 0.35)], stroke);
            let head = vec![c + vec2(-s * 0.55, -s * 0.05), c + vec2(s * 0.55, -s * 0.05), c + vec2(0.0, s * 0.55)];
            painter.add(egui::Shape::convex_polygon(head, color, Stroke::NONE));
            painter.line_segment([c + vec2(-s, s), c + vec2(s, s)], stroke);
        }
    }
}

/// Square button with a drawn icon (the pause button next to tr!("Detener Grabación", "Stop Recording")).
fn icon_button(ui: &mut egui::Ui, icon: Icon, size: f32, enabled: bool, checked: bool) -> egui::Response {
    let button = Button::new("").min_size(vec2(size, size)).fill(if checked { PRIMARY } else { GREY4 });
    let response = ui.add_enabled(enabled, button);
    let color = if enabled { Color32::WHITE } else { TEXT_MUTED };
    paint_icon(ui.painter(), icon, response.rect.center(), 6.0, color);
    response
}

/// Speaker button that turns red with a cross when muted.
fn mute_button(ui: &mut egui::Ui, muted: bool) -> egui::Response {
    let response = ui.add(Button::new("").min_size(vec2(28.0, 22.0)).fill(if muted { RED_DIM } else { GREY4 }));
    let c = response.rect.center() - vec2(3.0, 0.0);
    let color = if muted { RED } else { Color32::WHITE };
    let painter = ui.painter();
    painter.rect_filled(Rect::from_min_max(c + vec2(-6.0, -3.0), c + vec2(-3.0, 3.0)), 0.0, color);
    let cone = vec![c + vec2(-3.0, -3.0), c + vec2(2.0, -7.0), c + vec2(2.0, 7.0), c + vec2(-3.0, 3.0)];
    painter.add(egui::Shape::convex_polygon(cone, color, Stroke::NONE));
    let stroke = Stroke::new(1.5, color);
    if muted {
        painter.line_segment([c + vec2(5.0, -3.0), c + vec2(11.0, 3.0)], stroke);
        painter.line_segment([c + vec2(11.0, -3.0), c + vec2(5.0, 3.0)], stroke);
    } else {
        painter.line_segment([c + vec2(5.0, -3.0), c + vec2(7.0, 0.0)], stroke);
        painter.line_segment([c + vec2(7.0, 0.0), c + vec2(5.0, 3.0)], stroke);
        painter.line_segment([c + vec2(8.0, -6.0), c + vec2(11.0, 0.0)], stroke);
        painter.line_segment([c + vec2(11.0, 0.0), c + vec2(8.0, 6.0)], stroke);
    }
    response
}

fn status_icon(ui: &mut egui::Ui, icon: Icon, color: Color32, text: &str) {
    ui.label(RichText::new(text).small().monospace().color(if color == GREY3 { TEXT_MUTED } else { Color32::WHITE }));
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    paint_icon(ui.painter(), icon, rect.center(), 5.0, color);
}

fn combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut T,
    options: &[T],
    label: impl Fn(T) -> String,
    width: f32,
) {
    ComboBox::from_id_salt(id).width(width).selected_text(label(*value)).show_ui(ui, |ui| {
        for &option in options {
            ui.selectable_value(value, option, label(option));
        }
    });
}

/// OBS-style device picker: "(Por defecto)", "Deshabilitado" or a specific device.
fn device_combo(ui: &mut egui::Ui, id: &str, enabled: &mut bool, device: &mut Option<String>, devices: &[Device]) {
    let default = tr!("(Por defecto)", "Default");
    let disabled = tr!("Deshabilitado", "Disabled");
    let current = if !*enabled {
        disabled.to_owned()
    } else {
        device
            .as_ref()
            .and_then(|id| devices.iter().find(|d| &d.id == id))
            .map_or(default.to_owned(), |d| d.name.clone())
    };
    ComboBox::from_id_salt(id).width(330.0).truncate().selected_text(current).show_ui(ui, |ui| {
        if ui.selectable_label(*enabled && device.is_none(), default).clicked() {
            (*enabled, *device) = (true, None);
        }
        if ui.selectable_label(!*enabled, disabled).clicked() {
            *enabled = false;
        }
        for d in devices {
            let selected = *enabled && device.as_deref() == Some(d.id.as_str());
            if ui.selectable_label(selected, &d.name).clicked() {
                (*enabled, *device) = (true, Some(d.id.clone()));
            }
        }
    });
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = GREY7;
    v.window_fill = GREY7;
    v.window_stroke = Stroke::new(1.0, GREY4);
    v.extreme_bg_color = GREY8;
    v.faint_bg_color = GREY6;
    v.selection.bg_fill = PRIMARY;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = PRIMARY_LIGHT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, GREY4);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    for (w, fill) in [
        (&mut v.widgets.inactive, GREY4),
        (&mut v.widgets.hovered, GREY3),
        (&mut v.widgets.active, GREY7),
        (&mut v.widgets.open, GREY3),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = CornerRadius::same(4);
        w.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    }
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, GREY1);
    v.widgets.active.bg_stroke = Stroke::new(1.0, PRIMARY);
    ctx.set_visuals(v);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = vec2(6.0, 4.0);
        style.spacing.button_padding = vec2(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
    });
    ctx.set_theme(egui::ThemePreference::Dark);
}

/// "(Ctrl+Alt+R)" for tooltips, or nothing when unassigned.
fn hotkey_hint(hotkey: &Option<String>) -> String {
    hotkey.as_ref().map_or(String::new(), |h| format!("({h})"))
}

/// OBS-style hotkey field: click it, then press the combination; Escape cancels, × clears.
/// One hotkey's button; true when a new combination was just set.
fn hotkey_field(ui: &mut egui::Ui, row: usize, slot: &mut Option<String>, capturing: &mut Option<usize>) -> bool {
    let mut set = false;
    ui.horizontal(|ui| {
        let waiting = *capturing == Some(row);
        let text = if waiting {
            tr!("Pulsa una combinación…", "Press a combination…").to_owned()
        } else {
            slot.clone().unwrap_or_else(|| tr!("(ninguno)", "(none)").to_owned())
        };
        let button = Button::new(RichText::new(text).monospace()).min_size(vec2(220.0, 24.0));
        if ui.add(button.fill(if waiting { PRIMARY } else { GREY4 })).clicked() {
            *capturing = Some(row);
        }
        if ui.add_enabled(slot.is_some(), Button::new("×")).on_hover_text(tr!("Quitar", "Clear")).clicked() {
            *slot = None;
        }
        if waiting {
            // Ctrl, Alt and Shift arrive as key presses of their own before the actual key.
            let pressed = ui.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => Some(None),
                    egui::Event::Key { key, pressed: true, modifiers, .. } => hotkeys::format(*modifiers, *key).map(Some),
                    _ => None,
                })
            });
            if let Some(text) = pressed {
                if text.is_some() {
                    *slot = text;
                    set = true;
                }
                *capturing = None;
            }
        }
    });
    set
}

/// A combination set for one action is taken from any other that had it: the system would
/// refuse the second registration, and blame another application.
fn take_from_others(slots: &mut [&mut Option<String>], row: usize) {
    let taken = slots[row].clone();
    for (i, slot) in slots.iter_mut().enumerate() {
        if i != row && **slot == taken {
            **slot = None;
        }
    }
}


#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    /// The interface language is global; tests that depend on it take this lock.
    static LANGUAGE: Mutex<()> = Mutex::new(());

    fn test_app() -> App {
        App {
            config: Config::default(),
            sources: Vec::new(),
            selected: None,
            system_devices: Vec::new(),
            mic_devices: Vec::new(),
            preview: None,
            rec: RecState::Idle,
            screenshot: None,
            system: None,
            mic: None,
            app_audio: None,
            audio_apps: Vec::new(),
            cameras: Vec::new(),
            camera: None,
            camera_texture: None,
            camera_seq: None,
            meters: [0.0; 3],
            ctx: egui::Context::default(),
            last_drawn: Instant::now(),
            texture: None,
            texture_seq: None,
            picking_region: false,
            region_drag: None,
            status: None,
            settings: None,
            cpu: CpuMeter { last: None, percent: 0.0 },
            actions: Arc::new(Mutex::new(Vec::new())),
            hotkeys: None,
            bound_actions: Arc::default(),
            native_window: None,
            tray: None,
            tray_commands: Arc::new(Mutex::new(Vec::new())),
            hidden: false,
            update: None,
            update_check: None,
            update_dialog: false,
            jobs: Vec::new(),
        }
    }

    #[test]
    fn a_selection_on_the_preview_maps_to_source_pixels() {
        // A 200×100 preview of a 1920×960 source.
        let canvas = Rect::from_min_size(pos2(100.0, 50.0), vec2(200.0, 100.0));
        let selection = Rect::from_min_max(pos2(150.0, 75.0), pos2(250.0, 125.0));
        let region = region_from_rect(selection, canvas, (1920, 960)).unwrap();
        assert_eq!(region, Region { x: 480, y: 240, width: 960, height: 480 });
        assert_eq!(region_rect(region, canvas, (1920, 960)), selection);
        // A click without a drag selects nothing.
        let click = Rect::from_min_max(pos2(150.0, 75.0), pos2(150.5, 75.5));
        assert_eq!(region_from_rect(click, canvas, (1920, 960)), None);
    }

    #[test]
    fn exit_waits_for_the_recording_being_saved() {
        let (tx, rx) = mpsc::channel();
        let saved = Arc::new(AtomicBool::new(false));
        let saver = thread::spawn({
            let saved = saved.clone();
            move || {
                thread::sleep(Duration::from_millis(100));
                saved.store(true, Ordering::SeqCst);
                let _ = tx.send(Ok(PathBuf::from("rec.mp4")));
            }
        });
        finish_on_exit(RecState::Saving(rx));
        assert!(saved.load(Ordering::SeqCst), "exited before the recording was finalized");
        saver.join().unwrap();
    }

    #[test]
    fn saved_message_survives_a_source_that_is_gone() {
        let _language = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
        let ctx = egui::Context::default();
        let mut app = test_app();
        // The recorded window was closed: its handle no longer exists.
        app.selected = Some(SourceId::Window(0));
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(PathBuf::from("rec.mp4"))).unwrap();
        app.rec = RecState::Saving(rx);
        app.poll(&ctx);
        let status = app.status.as_ref().expect("status");
        assert!(status.text.starts_with("Grabación guardada"), "status: {}", status.text);
        assert_eq!(status.file.as_deref(), Some(std::path::Path::new("rec.mp4")));
        assert!(app.selected.is_none(), "the closed window is still shown as the source");
    }

    #[test]
    fn a_new_release_opens_the_dialog_unless_skipped() {
        let ctx = egui::Context::default();
        let release = Release { version: "9.0.0".into(), url: "https://example.com".into() };
        for (skipped, dialog) in [(None, true), (Some("8.0.0"), true), (Some("9.0.0"), false)] {
            let mut app = test_app();
            app.config.skipped_version = skipped.map(str::to_owned);
            let (tx, rx) = mpsc::channel();
            tx.send(Some(release.clone())).unwrap();
            app.update_check = Some(rx);
            app.poll(&ctx);
            assert_eq!(app.update_dialog, dialog, "skipped version: {skipped:?}");
            assert_eq!(app.update.as_ref(), Some(&release), "the status bar link is always shown");
        }
        let mut app = test_app();
        let (tx, rx) = mpsc::channel();
        tx.send(None).unwrap();
        app.update_check = Some(rx);
        app.poll(&ctx);
        assert!(!app.update_dialog, "no newer release");
    }

    #[test]
    fn update_dialog_buttons_stay_inside_it_in_both_languages() {
        let _language = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
        for language in [Language::English, Language::Spanish] {
            i18n::set(language);
            let mut app = test_app();
            app.update = Some(Release { version: "10.10.10".into(), url: "https://example.com".into() });
            app.update_dialog = true;
            let ctx = egui::Context::default();
            let input = || egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1280.0, 720.0))),
                ..Default::default()
            };
            // The first passes size the dialog.
            let mut shapes = Vec::new();
            for _ in 0..3 {
                let mut output = ctx.run_ui(input(), |ui| app.update_dialog(ui.ctx()));
                output.textures_delta.clear(); // no renderer
                shapes = output.shapes;
            }
            let mut flat = Vec::new();
            let mut pending: Vec<egui::Shape> = shapes.into_iter().map(|c| c.shape).collect();
            while let Some(shape) = pending.pop() {
                match shape {
                    egui::Shape::Vec(inner) => pending.extend(inner),
                    shape => flat.push(shape),
                }
            }
            let title = tr!("Nueva versión disponible", "New version available");
            let title_pos = flat
                .iter()
                .find_map(|s| match s {
                    egui::Shape::Text(t) if t.galley.text() == title => Some(t.pos),
                    _ => None,
                })
                .expect("dialog title drawn");
            // All but the backdrop and the frame, the shapes around the title.
            let left = flat
                .iter()
                .map(egui::Shape::visual_bounding_rect)
                .filter(|r| r.is_positive() && !r.contains(title_pos))
                .map(|r| r.min.x)
                .fold(f32::INFINITY, f32::min);
            assert!(left >= title_pos.x - 0.5, "{language:?}: something starts at x = {left}, left of the title at {}", title_pos.x);
        }
        i18n::set(Language::Spanish);
    }

    #[test]
    fn a_recording_cut_short_is_repaired_at_start_up() {
        let _language = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("obs-lite-recover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cut.mp4");
        // Writing stopped inside the last fragment's data.
        let (mut file, _, _) = crate::mp4::tests::fragmented();
        file.truncate(file.len() - 42);
        std::fs::write(&path, file).unwrap();

        let mut app = test_app();
        app.config.last_recording = Some(path.clone());
        app.recover_last_recording();
        let status = app.status.as_ref().expect("a status");
        assert!(status.text.starts_with("Se recuperó la grabación interrumpida"), "status: {}", status.text);
        assert_eq!(status.file.as_deref(), Some(path.as_path()));
        assert!(app.config.last_recording.is_none(), "repaired once");
        assert!(!crate::mp4::repair(&path).unwrap(), "now a regular MP4");

        // A recording that finished (so is regular), or that is gone: nothing to say.
        for last in [path.clone(), dir.join("gone.mp4")] {
            let mut app = test_app();
            app.config.last_recording = Some(last);
            app.recover_last_recording();
            assert!(app.status.is_none());
            assert!(app.config.last_recording.is_none());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One pass of the settings dialog with `events` as input.
    fn settings_pass(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) {
        let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| app.settings_dialog(ui.ctx()));
        output.textures_delta.clear();
    }

    #[test]
    fn escape_while_waiting_for_a_hotkey_keeps_the_dialog_open() {
        let _language = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = test_app();
        app.settings = Some(SettingsDialog { draft: Config::default(), page: SettingsPage::Hotkeys, capturing: Some(0) });
        let escape = egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let ctx = egui::Context::default();
        settings_pass(&ctx, &mut app, Vec::new());
        settings_pass(&ctx, &mut app, vec![escape.clone()]);
        let dialog = app.settings.as_ref().expect("still open");
        assert!(dialog.capturing.is_none(), "the wait for a hotkey ended");
        // Another Escape, with no wait, closes it.
        settings_pass(&ctx, &mut app, vec![escape]);
        assert!(app.settings.is_none());
    }

    #[test]
    fn a_hotkey_set_for_one_action_is_taken_from_another() {
        // F9 was just set for the first action, and the second had it.
        let (mut a, mut b, mut c) = (Some("F9".to_owned()), Some("F9".to_owned()), None);
        take_from_others(&mut [&mut a, &mut b, &mut c], 0);
        assert_eq!((a.as_deref(), b, c), (Some("F9"), None, None));
    }

    #[test]
    fn mute_silences_regardless_of_volume() {
        assert_eq!(channel_gain(1.0, true), 0.0);
        assert_eq!(channel_gain(0.5, true), 0.0);
    }

    #[test]
    fn no_preview_pictures_for_a_window_that_is_not_shown() {
        let just = Duration::from_millis(200);
        assert!(!preview_hidden(Some(true), just));
        assert!(!preview_hidden(None, just), "Wayland: drawn recently, so shown");
        assert!(preview_hidden(Some(false), just), "minimized");
        assert!(preview_hidden(None, Duration::from_secs(4)), "Wayland: no frames asked for");
        assert!(!preview_hidden(None, Duration::from_millis(1500)), "an idle window draws once a second");
    }

    #[test]
    fn a_silent_idle_window_repaints_rarely() {
        let fast = Duration::from_millis(66);
        assert_eq!(repaint_interval(false, &[0.0, 0.0, 0.0]), Duration::from_secs(1));
        assert_eq!(repaint_interval(false, &[0.0, 0.2, 0.0]), fast, "a meter is moving");
        assert_eq!(repaint_interval(false, &[0.002, 0.0, 0.0]), fast, "a meter is still falling");
        assert_eq!(repaint_interval(true, &[0.0; 3]), fast, "the recording timer runs");
    }

    #[test]
    fn desktop_audio_leaves_out_the_application_captured_on_its_own() {
        assert_eq!(system_kind(Some(InputKind::Application(42)), None), InputKind::SystemExcept(42));
        assert_eq!(system_kind(None, None), InputKind::System, "no application captured");
        assert_eq!(system_kind(Some(InputKind::Application(42)), Some("speakers")), InputKind::System, "a chosen output device");
    }

    #[test]
    fn volume_uses_a_squared_curve() {
        assert_eq!(channel_gain(1.0, false), 1.0);
        assert_eq!(channel_gain(0.5, false), 0.25);
        assert_eq!(channel_gain(0.0, false), 0.0);
    }
}
