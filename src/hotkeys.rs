//! Global hotkeys as the text shown in Settings and stored in settings.json ("Ctrl+Alt+R").

use eframe::egui::{Key, Modifiers};
use global_hotkey::hotkey::{self, Code, HotKey};

/// Parses "Ctrl+Alt+R" into a hotkey that can be registered.
pub fn parse(text: &str) -> Option<HotKey> {
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

/// The text for a combination pressed in Settings, or `None` if it cannot be a global hotkey.
/// Without Ctrl or Alt, only F-keys are allowed: a global "R" or "Shift+R" would stop that
/// key from typing in every other application.
pub fn format(mods: Modifiers, key: Key) -> Option<String> {
    use Key::{AltLeft, AltRight, ControlLeft, ControlRight, ShiftLeft, ShiftRight, SuperLeft, SuperRight};
    if matches!(key, ShiftLeft | ShiftRight | ControlLeft | ControlRight | AltLeft | AltRight | SuperLeft | SuperRight) {
        return None; // still waiting for the actual key
    }
    code(key)?;
    let function_key = key.name().len() > 1 && key.name().starts_with('F') && key.name()[1..].parse::<u8>().is_ok();
    if !(mods.ctrl || mods.alt || function_key) {
        return None;
    }
    let mut text = String::new();
    for (on, name) in [(mods.ctrl, "Ctrl+"), (mods.alt, "Alt+"), (mods.shift, "Shift+")] {
        if on {
            text.push_str(name);
        }
    }
    text.push_str(key.name());
    Some(text)
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

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_ALT: Modifiers = Modifiers { alt: true, ctrl: true, shift: false, mac_cmd: false, command: true };

    #[test]
    fn text_round_trips_to_a_registrable_hotkey() {
        assert_eq!(format(CTRL_ALT, Key::R).as_deref(), Some("Ctrl+Alt+R"));
        let expected = HotKey::new(Some(hotkey::Modifiers::CONTROL | hotkey::Modifiers::ALT), Code::KeyR);
        assert_eq!(parse("Ctrl+Alt+R"), Some(expected));
        assert_eq!(parse("Ctrl+Shift+5"), Some(HotKey::new(Some(hotkey::Modifiers::CONTROL | hotkey::Modifiers::SHIFT), Code::Digit5)));
        assert_eq!(parse("F9"), Some(HotKey::new(None, Code::F9)));
    }

    #[test]
    fn keys_that_would_break_typing_are_rejected() {
        assert_eq!(format(Modifiers::NONE, Key::R), None);
        assert_eq!(format(Modifiers::SHIFT, Key::R), None);
        assert_eq!(format(Modifiers::NONE, Key::F9).as_deref(), Some("F9"));
    }

    #[test]
    fn a_modifier_alone_is_not_a_hotkey() {
        // egui reports the modifier keys themselves as key presses too.
        assert_eq!(format(CTRL_ALT, Key::ControlLeft), None);
        assert_eq!(format(CTRL_ALT, Key::AltRight), None);
        assert_eq!(format(Modifiers::SHIFT, Key::ShiftLeft), None);
    }

    #[test]
    fn invalid_text_is_rejected() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("Ctrl+Alt+"), None);
        assert_eq!(parse("Hyper+R"), None);
    }
}
