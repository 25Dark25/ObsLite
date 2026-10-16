//! System tray icon: recording state at a glance and the main actions in its menu.

use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::i18n::tr;

/// What the user picked in the tray.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayCommand {
    ToggleRecording,
    TogglePause,
    Screenshot,
    Show,
    Exit,
}

const ITEMS: [(&str, TrayCommand); 5] = [
    ("record", TrayCommand::ToggleRecording),
    ("pause", TrayCommand::TogglePause),
    ("screenshot", TrayCommand::Screenshot),
    ("show", TrayCommand::Show),
    ("exit", TrayCommand::Exit),
];

/// What the tray shows; it is only updated when this changes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TrayState {
    pub recording: bool,
    pub paused: bool,
    pub english: bool,
}

pub struct Tray {
    icon: TrayIcon,
    record: MenuItem,
    pause: MenuItem,
    screenshot: MenuItem,
    show: MenuItem,
    exit: MenuItem,
    idle_icon: Icon,
    recording_icon: Icon,
    shown: Option<TrayState>,
}

impl Tray {
    /// Creates the icon; menu clicks and left clicks on the icon go to `on_command`.
    pub fn new(
        ctx: &egui::Context,
        rgba: &[u8],
        size: u32,
        on_command: impl Fn(TrayCommand) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let item = |id: &str| MenuItem::with_id(id, "", true, None);
        let (record, pause, screenshot, show, exit) =
            (item("record"), item("pause"), item("screenshot"), item("show"), item("exit"));
        let menu = Menu::new();
        let separator = PredefinedMenuItem::separator();
        menu.append_items(&[&record, &pause, &screenshot, &separator, &show, &exit]).map_err(|e| e.to_string())?;

        let recording_icon = Icon::from_rgba(rgba.to_vec(), size, size).map_err(|e| e.to_string())?;
        let idle_icon = Icon::from_rgba(desaturate(rgba), size, size).map_err(|e| e.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_icon(idle_icon.clone())
            .build()
            .map_err(|e| e.to_string())?;

        let on_command = std::sync::Arc::new(on_command);
        let repaint = ctx.clone();
        let on_menu = on_command.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, command)) = ITEMS.iter().find(|(id, _)| event.id == MenuId::new(id)) {
                on_menu(*command);
                repaint.request_repaint();
            }
        }));
        let repaint = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                on_command(TrayCommand::Show);
                repaint.request_repaint();
            }
        }));

        Ok(Self { icon, record, pause, screenshot, show, exit, idle_icon, recording_icon, shown: None })
    }

    /// Updates icon, tooltip and menu texts when the state or the language changed.
    pub fn update(&mut self, state: TrayState) {
        if self.shown == Some(state) {
            return;
        }
        self.shown = Some(state);
        let record = if state.recording { tr!("Detener Grabación", "Stop Recording") } else { tr!("Iniciar Grabación", "Start Recording") };
        let pause = if state.paused { tr!("Reanudar Grabación", "Resume Recording") } else { tr!("Pausar Grabación", "Pause Recording") };
        self.record.set_text(record);
        self.pause.set_text(pause);
        self.pause.set_enabled(state.recording);
        self.screenshot.set_text(tr!("Hacer captura de pantalla", "Take Screenshot"));
        self.show.set_text(tr!("Mostrar obs-lite", "Show obs-lite"));
        self.exit.set_text(tr!("Salir", "Exit"));
        let tooltip = match (state.recording, state.paused) {
            (true, true) => tr!("obs-lite · En pausa", "obs-lite · Paused"),
            (true, false) => tr!("obs-lite · Grabando", "obs-lite · Recording"),
            _ => "obs-lite",
        };
        let _ = self.icon.set_tooltip(Some(tooltip));
        let icon = if state.recording { &self.recording_icon } else { &self.idle_icon };
        let _ = self.icon.set_icon(Some(icon.clone()));
    }
}

/// Grey version of the icon for when nothing is being recorded.
fn desaturate(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| {
            let l = ((r as u32 * 3 + g as u32 * 6 + b as u32) / 10) as u8;
            [l, l, l, a]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_icon_is_grey_with_the_same_alpha() {
        assert_eq!(desaturate(&[255, 0, 0, 200, 10, 10, 10, 0]), [76, 76, 76, 200, 10, 10, 10, 0]);
    }
}
