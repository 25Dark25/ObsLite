//! Global hotkeys through RegisterHotKey (the global-hotkey crate).

use std::sync::{Arc, Mutex};

use eframe::egui::Key;
use global_hotkey::hotkey::{self, Code, HotKey};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::hotkeys::Binding;

/// Whether `key` can be part of a global hotkey.
pub fn supports(key: Key) -> bool {
    code(key).is_some()
}

/// Parses "Ctrl+Alt+R" into a hotkey that can be registered.
fn parse(text: &str) -> Option<HotKey> {
    let mut parts: Vec<&str> = text.split('+').collect();
    let key = Key::from_name(parts.pop()?)?;
    let mut mods = hotkey::Modifiers::empty();
    for part in parts {
        mods |= match part {
            "Ctrl" => hotkey::Modifiers::CONTROL,
            "Alt" => hotkey::Modifiers::ALT,
            "Shift" => hotkey::Modifiers::SHIFT,
            _ => return None,
        };
    }
    Some(HotKey::new((!mods.is_empty()).then_some(mods), code(key)?))
}

fn code(key: Key) -> Option<Code> {
    let name = key.name();
    let code = match name.as_bytes() {
        [c] if c.is_ascii_uppercase() => format!("Key{name}"),
        [c] if c.is_ascii_digit() => format!("Digit{name}"),
        _ => name.to_owned(),
    };
    code.parse().ok()
}

pub struct GlobalHotkeys {
    manager: GlobalHotKeyManager,
    /// Registered hotkeys and the index of their binding, shared with the event handler.
    bound: Arc<Mutex<Vec<(HotKey, usize)>>>,
}

impl GlobalHotkeys {
    /// Starts listening; `on_press` gets the index (in the last `bind`) of a pressed hotkey.
    pub fn new(on_press: impl Fn(usize) + Send + Sync + 'static) -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        let bound: Arc<Mutex<Vec<(HotKey, usize)>>> = Arc::default();
        let handler_bound = bound.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state != HotKeyState::Pressed {
                return;
            }
            let index = handler_bound.lock().unwrap().iter().find(|(hotkey, _)| hotkey.id() == event.id).map(|(_, i)| *i);
            if let Some(index) = index {
                on_press(index);
            }
        }));
        Ok(Self { manager, bound })
    }

    /// Replaces the registered hotkeys; returns the ones that could not be registered.
    pub fn bind(&mut self, bindings: &[Binding]) -> Vec<String> {
        self.unbind();
        let mut bound = Vec::new();
        let mut failed = Vec::new();
        for (index, binding) in bindings.iter().enumerate() {
            match parse(&binding.text) {
                Some(hotkey) if self.manager.register(hotkey).is_ok() => bound.push((hotkey, index)),
                _ => failed.push(binding.text.clone()),
            }
        }
        *self.bound.lock().unwrap() = bound;
        failed
    }

    /// Registration happens in `bind`, which reports failures itself.
    pub fn take_error(&self) -> Option<String> {
        None
    }

    /// Releases every hotkey, so the combinations reach the focused window again.
    pub fn unbind(&mut self) {
        let bound = std::mem::take(&mut *self.bound.lock().unwrap());
        let hotkeys: Vec<HotKey> = bound.iter().map(|(hotkey, _)| *hotkey).collect();
        let _ = self.manager.unregister_all(&hotkeys);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips_to_a_registrable_hotkey() {
        let expected = HotKey::new(Some(hotkey::Modifiers::CONTROL | hotkey::Modifiers::ALT), Code::KeyR);
        assert_eq!(parse("Ctrl+Alt+R"), Some(expected));
        assert_eq!(parse("Ctrl+Shift+5"), Some(HotKey::new(Some(hotkey::Modifiers::CONTROL | hotkey::Modifiers::SHIFT), Code::Digit5)));
        assert_eq!(parse("F9"), Some(HotKey::new(None, Code::F9)));
    }

    #[test]
    fn invalid_text_is_rejected() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("Ctrl+Alt+"), None);
        assert_eq!(parse("Hyper+R"), None);
    }
}
