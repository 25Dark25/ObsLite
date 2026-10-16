//! UI and recording lifecycle.
//!
//! The layout follows OBS Studio (preview on top; Sources, Audio Mixer and Controls
//! docks below; status bar; Settings dialog) and uses its Yami palette and Spanish
//! wording, so OBS users find everything where they expect it.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Button, Color32, ComboBox, CornerRadius, FontId, Layout, Margin, Rect, RichText, Sense,
    Slider, Stroke, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::audio::{self, Device, Input, InputKind, Mixer};
use crate::capture::{self, RecordConfig, Session, SessionOptions, Source, SourceId};
use crate::config::{Config, Quality, Resolution};
use crate::i18n::{self, Language, tr};

const PREVIEW_FPS: u32 = 5;
const RECORD_KEY: &str = "Ctrl+Alt+R";
const PAUSE_KEY: &str = "Ctrl+Alt+P";
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
}

enum RecState {
    Idle,
    Recording { session: Session, mixer: Option<Mixer>, path: PathBuf },
    Saving(Receiver<Result<PathBuf, String>>),
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
    meters: [f32; 2],
    texture: Option<TextureHandle>,
    texture_seq: Option<u64>,
    status: Option<Status>,
    settings: Option<SettingsDialog>,
    cpu: CpuMeter,
    actions: Arc<Mutex<Vec<Action>>>,
    _hotkeys: Option<GlobalHotKeyManager>,
    /// The main window, for excluding it from capture.
    hwnd: Option<isize>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let config = Config::load();
        i18n::set(config.language.unwrap_or_else(Language::system));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let (hotkeys, hotkey_error) = match register_hotkeys(&cc.egui_ctx, actions.clone()) {
            Ok(manager) => (Some(manager), None),
            Err(e) => (None, Some(e)),
        };

        let mut app = Self {
            system: None,
            mic: None,
            config,
            sources: Vec::new(),
            selected: None,
            system_devices: Vec::new(),
            mic_devices: Vec::new(),
            preview: None,
            rec: RecState::Idle,
            screenshot: None,
            meters: [0.0; 2],
            texture: None,
            texture_seq: None,
            status: None,
            settings: None,
            cpu: CpuMeter { last: None, percent: 0.0 },
            actions,
            _hotkeys: hotkeys,
            hwnd: main_window(cc),
        };
        app.apply_capture_visibility();
        app.refresh_devices();
        app.restart_input(InputKind::System);
        app.restart_input(InputKind::Microphone);
        app.refresh_sources();
        app.restart_preview(&cc.egui_ctx);
        if let Some(e) = hotkey_error {
            app.set_status(tr!("Atajos globales no disponibles: {}", "Global hotkeys unavailable: {}", e), None, true);
        }
        app
    }

    fn is_recording(&self) -> bool {
        matches!(self.rec, RecState::Recording { .. })
    }

    fn recording_session(&self) -> Option<&Session> {
        match &self.rec {
            RecState::Recording { session, .. } => Some(session),
            _ => None,
        }
    }

    fn is_paused(&self) -> bool {
        self.recording_session().is_some_and(|s| s.shared.clock.is_paused())
    }

    /// Recording and the encoder clock is running (pausing before that would be lost).
    fn is_live(&self) -> bool {
        self.recording_session().is_some_and(|s| s.shared.encoder_ready.load(Ordering::Acquire))
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
        self.config.source_label = self.selected_source().map(|s| s.label.clone());
        self.restart_preview(ctx);
    }

    fn set_status(&mut self, text: impl Into<String>, file: Option<PathBuf>, error: bool) {
        self.status = Some(Status { text: text.into(), file, error });
    }

    fn channel(&self, kind: InputKind) -> (bool, Option<String>, f32) {
        let c = &self.config;
        match kind {
            InputKind::System => (c.system_audio, c.system_device.clone(), channel_gain(c.system_volume, c.system_muted)),
            InputKind::Microphone => (c.mic, c.mic_device.clone(), channel_gain(c.mic_volume, c.mic_muted)),
        }
    }

    fn restart_input(&mut self, kind: InputKind) {
        let (enabled, device, gain) = self.channel(kind);
        let slot = match kind {
            InputKind::System => &mut self.system,
            InputKind::Microphone => &mut self.mic,
        };
        *slot = None; // release the device before reopening it
        *slot = enabled.then(|| Input::start(kind, device, gain));
    }

    fn restart_preview(&mut self, ctx: &egui::Context) {
        self.preview = None;
        self.texture_seq = None;
        if self.is_recording() {
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
                RecState::Idle => self.start_recording(ctx),
                RecState::Recording { .. } => self.stop_recording(ctx, None),
                RecState::Saving(_) => {}
            },
            Action::TogglePause => {
                if self.is_live()
                    && let Some(session) = self.recording_session()
                {
                    let clock = &session.shared.clock;
                    clock.set_paused(!clock.is_paused());
                }
            }
        }
    }

    fn start_recording(&mut self, ctx: &egui::Context) {
        let Some(source) = self.selected else {
            return self.set_status(tr!("Añade una fuente con el botón + del panel Fuentes.", "Add a source with the + button in the Sources dock."), None, true);
        };
        if let Err(e) = std::fs::create_dir_all(&self.config.output_dir) {
            return self.set_status(tr!("No se puede usar la ruta de grabación: {}", "Cannot use the recording path: {}", e), None, true);
        }
        // One capture session at a time: the recording session also feeds the preview.
        self.preview = None;

        let name = format!("{}.mp4", chrono::Local::now().format("%Y-%m-%d %H-%M-%S"));
        let path = self.config.output_dir.join(name);
        let inputs: Vec<_> = [&self.system, &self.mic].into_iter().flatten().map(|i| i.shared.clone()).collect();
        let opts = SessionOptions {
            source,
            fps: self.config.fps,
            cursor: self.config.cursor,
            preview_fps: PREVIEW_FPS,
            record: Some(RecordConfig {
                path: path.clone(),
                quality: self.config.quality,
                resolution: self.config.resolution,
                audio: !inputs.is_empty(),
            }),
        };
        let session = match Session::start(opts, Some(ctx.clone())) {
            Ok(s) => s,
            Err(e) => {
                self.restart_preview(ctx);
                return self.set_status(tr!("No se pudo iniciar la grabación: {}", "Could not start recording: {}", e), None, true);
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
        self.rec = RecState::Recording { session, mixer, path };
        self.status = None;
    }

    fn stop_recording(&mut self, ctx: &egui::Context, reason: Option<String>) {
        let RecState::Recording { session, mixer, path } = std::mem::replace(&mut self.rec, RecState::Idle) else {
            return;
        };
        drop(mixer);
        let (encoder, stop_error) = session.stop();
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        // Finalizing the MP4 can take a moment; keep the UI responsive.
        thread::spawn(move || {
            let result = match encoder {
                Some(enc) => enc.finish().map(|()| path),
                None => Err(stop_error.unwrap_or_else(|| tr!("no se capturó ningún fotograma", "no frames were captured").into())),
            };
            let _ = tx.send(result);
            ctx2.request_repaint();
        });
        self.rec = RecState::Saving(rx);
        self.set_status(reason.unwrap_or_else(|| tr!("Guardando grabación…", "Saving recording…").into()), None, false);
    }

    /// Saves the newest frame of the current source as a PNG in the recordings folder.
    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let Some(session) = self.recording_session().or(self.preview.as_ref()) else {
            return self.set_status(tr!("Añade una fuente con el botón + del panel Fuentes.", "Add a source with the + button in the Sources dock."), None, true);
        };
        let (width, height, rgba) = match session.snapshot() {
            Ok(image) => image,
            Err(e) => return self.set_status(tr!("No se pudo hacer la captura: {}", "Could not take the screenshot: {}", e), None, true),
        };
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

        if let RecState::Saving(rx) = &self.rec
            && let Ok(result) = rx.try_recv()
        {
            self.rec = RecState::Idle;
            // Before the status: if the source is gone, its error must not hide where the file went.
            self.restart_preview(ctx);
            match result {
                Ok(path) => self.set_status(tr!("Grabación guardada en '{}'", "Recording saved to '{}'", path.display()), Some(path), false),
                Err(e) => self.set_status(tr!("Error al guardar la grabación: {}", "Error saving the recording: {}", e), None, true),
            }
        }

        if let Some(rx) = &self.screenshot
            && let Ok(result) = rx.try_recv()
        {
            self.screenshot = None;
            match result {
                Ok(path) => self.set_status(tr!("Captura guardada en '{}'", "Screenshot saved to '{}'", path.display()), Some(path), false),
                Err(e) => self.set_status(tr!("No se pudo guardar la captura: {}", "Could not save the screenshot: {}", e), None, true),
            }
        }

        if self.recording_session().is_some_and(Session::is_finished) {
            return self.stop_recording(ctx, Some(tr!("La fuente dejó de estar disponible; guardando…", "The source is no longer available; saving…").into()));
        }
        if self.preview.as_ref().is_some_and(Session::is_finished) {
            self.source_lost(tr!("La fuente ya no está disponible. Añade otra con el botón +.", "The source is no longer available. Add another one with the + button.").into());
        }
        self.cpu.update();
    }

    fn update_texture(&mut self, ctx: &egui::Context) {
        let Some(session) = self.recording_session().or(self.preview.as_ref()) else { return };
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
        self.settings = Some(SettingsDialog { draft: self.config.clone(), page: SettingsPage::General });
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
        self.apply_capture_visibility();
        self.config.save();
    }

    fn apply_capture_visibility(&self) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
        let Some(hwnd) = self.hwnd else { return };
        let affinity = if self.config.hide_from_capture { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        // Fails before Windows 10 2004; the window then simply stays capturable.
        let _ = unsafe { SetWindowDisplayAffinity(HWND(hwnd as *mut _), affinity) };
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
                    let _ = std::process::Command::new("explorer").arg(&self.config.output_dir).spawn();
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
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click());
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
        let recording = self.is_recording();
        let ctx = ui.ctx().clone();
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_enabled_ui(!recording, |ui| {
                ui.horizontal(|ui| {
                    let add = ui.menu_button(RichText::new(" + ").strong(), |ui| {
                        let mut chosen = None;
                        ui.menu_button(tr!("Captura de pantalla", "Display Capture"), |ui| {
                            for s in self.sources.iter().filter(|s| matches!(s.id, SourceId::Monitor(_))) {
                                if ui.button(&s.label).clicked() {
                                    chosen = Some(s.id);
                                }
                            }
                        });
                        ui.menu_button(tr!("Captura de ventana", "Window Capture"), |ui| {
                            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                                for s in self.sources.iter().filter(|s| matches!(s.id, SourceId::Window(_))) {
                                    if ui.button(&s.label).clicked() {
                                        chosen = Some(s.id);
                                    }
                                }
                            });
                        });
                        chosen
                    });
                    // Windows come and go; refresh the list each time the menu opens.
                    if add.response.clicked() {
                        self.refresh_sources();
                    }
                    if let Some(Some(id)) = add.inner {
                        self.select_source(&ctx, id);
                        ui.close();
                    }
                    add.response.on_hover_text(tr!("Añadir fuente", "Add source"));
                    ui.menu_button(tr!("Propiedades", "Properties"), |ui| {
                        if ui.checkbox(&mut self.config.cursor, tr!("Capturar el cursor", "Capture Cursor")).changed() {
                            self.restart_preview(&ctx);
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
                    };
                    list_row(ui, &format!("{kind}  ·  {}", source.label));
                }
                None => {
                    ui.label(RichText::new(tr!("No hay ninguna fuente. Pulsa + para añadir una.", "No sources. Press + to add one.")).color(TEXT_MUTED));
                }
            });
        });
    }

    fn mixer_dock(&mut self, ui: &mut egui::Ui) {
        let rows = [(InputKind::System, tr!("Audio del escritorio", "Desktop Audio")), (InputKind::Microphone, "Mic/Aux")];
        let mut any = false;
        for (i, (kind, label)) in rows.into_iter().enumerate() {
            let (volume, muted, input) = match kind {
                InputKind::System => (&mut self.config.system_volume, &mut self.config.system_muted, &self.system),
                InputKind::Microphone => (&mut self.config.mic_volume, &mut self.config.mic_muted, &self.mic),
            };
            // Disabled devices are not shown, as in OBS.
            let Some(input) = input else { continue };
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
                RecState::Idle => {
                    if ui.add_sized([full, height], Button::new(tr!("Iniciar Grabación", "Start Recording"))).on_hover_text(RECORD_KEY).clicked() {
                        self.handle(&ctx, Action::ToggleRecording);
                    }
                }
                RecState::Recording { .. } => {
                    let stop = Button::new(RichText::new(tr!("Detener Grabación", "Stop Recording")).color(Color32::WHITE)).fill(PRIMARY);
                    if ui.add_sized([full - height - 4.0, height], stop).on_hover_text(RECORD_KEY).clicked() {
                        self.handle(&ctx, Action::ToggleRecording);
                    }
                    let paused = self.is_paused();
                    let tip = if paused { tr!("Reanudar Grabación", "Resume Recording") } else { tr!("Pausar Grabación", "Pause Recording") };
                    if icon_button(ui, if paused { Icon::Play } else { Icon::Pause }, height, self.is_live(), paused)
                        .on_hover_text(format!("{tip} ({PAUSE_KEY})"))
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
                    let _ = std::process::Command::new("explorer").arg("/select,").arg(file).spawn();
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                self.recording_indicator(ui);
                ui.label(RichText::new(format!("CPU: {:.1}%", self.cpu.percent)).small().color(TEXT_MUTED));
            });
        });
    }

    fn recording_indicator(&self, ui: &mut egui::Ui) {
        let Some(session) = self.recording_session() else {
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
        let secs = shared.clock.elapsed().as_secs();
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
        let recording = matches!(self.rec, RecState::Recording { .. });
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
                                dialog.page = page;
                            }
                        }
                    });
                    let (line, _) = ui.allocate_exact_size(vec2(9.0, body), Sense::hover());
                    ui.painter().vline(line.center().x, line.y_range(), Stroke::new(1.0, GREY4));
                    ui.allocate_ui_with_layout(vec2(ui.available_width(), body), Layout::top_down(Align::Min), |ui| {
                        ui.set_min_height(body);
                        if recording && dialog.page != SettingsPage::Hotkeys {
                            ui.label(RichText::new(tr!("Detén la grabación para cambiar estos ajustes.", "Stop recording to change these settings.")).color(YELLOW));
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
        if response.should_close() && decision.is_none() {
            decision = Some(false);
        }
        match decision {
            Some(true) => {
                let draft = self.settings.take().expect("dialog is open").draft;
                self.apply_settings(draft);
            }
            Some(false) => self.settings = None,
            None => {}
        }
    }
}

fn settings_page(ui: &mut egui::Ui, dialog: &mut SettingsDialog, system: &[Device], mic: &[Device]) {
    let c = &mut dialog.draft;
    let grid = |id: &str| egui::Grid::new(id).num_columns(2).spacing([16.0, 10.0]);
    match dialog.page {
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
                ui.label(tr!("Captura", "Capture"));
                ui.checkbox(&mut c.hide_from_capture, tr!("Ocultar obs-lite de la captura de pantalla", "Hide obs-lite from screen capture"));
                ui.end_row();
            });
        }
        SettingsPage::Output => {
            group_title(ui, tr!("Grabación", "Recording"));
            grid("output").show(ui, |ui| {
                ui.label(tr!("Ruta de grabación", "Recording Path"));
                ui.horizontal(|ui| {
                    let mut path = c.output_dir.display().to_string();
                    if ui.add(egui::TextEdit::singleline(&mut path).desired_width(290.0)).changed() {
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
                ui.label("Hardware (H.264)");
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
            grid("hotkeys").show(ui, |ui| {
                ui.label(tr!("Iniciar / Detener Grabación", "Start / Stop Recording"));
                ui.label(RichText::new(RECORD_KEY).monospace());
                ui.end_row();
                ui.label(tr!("Pausar / Reanudar Grabación", "Pause / Resume Recording"));
                ui.label(RichText::new(PAUSE_KEY).monospace());
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.label(RichText::new(tr!("Funcionan aunque obs-lite esté minimizado o en segundo plano.", "They work even when obs-lite is minimized or in the background.")).small().color(TEXT_MUTED));
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        // Keep the timer, CPU and audio meters moving; nothing else needs continuous repaints.
        if self.is_recording() || self.system.is_some() || self.mic.is_some() {
            ctx.request_repaint_after(Duration::from_millis(66));
        } else {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.update_texture(&ctx);
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
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.config.save();
        finish_on_exit(std::mem::replace(&mut self.rec, RecState::Idle));
    }
}

/// Never leaves the MP4 half-written when the app closes mid-recording or mid-save.
fn finish_on_exit(rec: RecState) {
    match rec {
        RecState::Recording { session, mixer, .. } => {
            drop(mixer);
            if let (Some(enc), _) = session.stop() {
                let _ = enc.finish();
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
        let Some(cpu) = process_cpu_time() else { return };
        if let Some((at, before)) = self.last {
            let cores = thread::available_parallelism().map_or(1, |n| n.get()) as f64;
            let wall = now.duration_since(at).as_secs_f64() * 1e7; // 100 ns units
            self.percent = (cpu.saturating_sub(before)) as f64 / wall / cores * 100.0;
        }
        self.last = Some((now, cpu));
    }
}

/// Kernel + user CPU time of this process, in 100 ns units.
fn process_cpu_time() -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    Some(ticks(kernel) + ticks(user))
}

/// Slider position (0..1) to linear gain; squared for a more natural loudness curve.
fn channel_gain(volume: f32, muted: bool) -> f32 {
    if muted { 0.0 } else { volume * volume }
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
fn list_row(ui: &mut egui::Ui, text: &str) {
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
    response.on_hover_text(text);
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

/// The native handle of the main window, if eframe exposes it.
fn main_window(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

fn register_hotkeys(ctx: &egui::Context, actions: Arc<Mutex<Vec<Action>>>) -> Result<GlobalHotKeyManager, String> {
    let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
    let record = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyR);
    let pause = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyP);
    manager.register(record).map_err(|e| format!("{RECORD_KEY}: {e}"))?;
    manager.register(pause).map_err(|e| format!("{PAUSE_KEY}: {e}"))?;
    let (record_id, pause_id) = (record.id(), pause.id());
    let ctx = ctx.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state != HotKeyState::Pressed {
            return;
        }
        let action = if event.id == record_id {
            Action::ToggleRecording
        } else if event.id == pause_id {
            Action::TogglePause
        } else {
            return;
        };
        actions.lock().unwrap().push(action);
        ctx.request_repaint();
    }));
    Ok(manager)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

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
            meters: [0.0; 2],
            texture: None,
            texture_seq: None,
            status: None,
            settings: None,
            cpu: CpuMeter { last: None, percent: 0.0 },
            actions: Arc::new(Mutex::new(Vec::new())),
            _hotkeys: None,
            hwnd: None,
        }
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
    fn mute_silences_regardless_of_volume() {
        assert_eq!(channel_gain(1.0, true), 0.0);
        assert_eq!(channel_gain(0.5, true), 0.0);
    }

    #[test]
    fn volume_uses_a_squared_curve() {
        assert_eq!(channel_gain(1.0, false), 1.0);
        assert_eq!(channel_gain(0.5, false), 0.25);
        assert_eq!(channel_gain(0.0, false), 0.0);
    }
}
