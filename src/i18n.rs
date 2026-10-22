//! Spanish and English UI text. The language follows the system unless chosen in Ajustes › General.
//!
//! Strings stay inline as Spanish/English pairs (`tr!`), so there are no keys to keep in sync.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

static ENGLISH: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Language {
    Spanish,
    English,
}

impl Language {
    /// The system display language: Spanish for any Spanish variant, English otherwise.
    pub fn system() -> Self {
        crate::platform::sys::system_language()
    }

    /// From a Windows language id.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn from_lang_id(id: u16) -> Self {
        const LANG_SPANISH: u16 = 0x0A;
        if id & 0x3FF == LANG_SPANISH { Self::Spanish } else { Self::English }
    }

    /// The language's own name, so it is recognizable whatever the current language.
    pub fn name(self) -> &'static str {
        match self {
            Self::Spanish => "Español",
            Self::English => "English",
        }
    }
}

pub fn set(language: Language) {
    ENGLISH.store(language == Language::English, Ordering::Relaxed);
}

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// `tr!("es", "en")` picks a string; with arguments it formats the chosen one.
macro_rules! tr {
    ($es:literal, $en:literal $(,)?) => {
        if $crate::i18n::english() { $en } else { $es }
    };
    ($es:literal, $en:literal, $($arg:expr),+ $(,)?) => {
        if $crate::i18n::english() { format!($en, $($arg),+) } else { format!($es, $($arg),+) }
    };
}
pub(crate) use tr;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spanish_variant_is_spanish() {
        assert_eq!(Language::from_lang_id(0x0C0A), Language::Spanish); // es-ES
        assert_eq!(Language::from_lang_id(0x080A), Language::Spanish); // es-MX
        assert_eq!(Language::from_lang_id(0x0409), Language::English); // en-US
        assert_eq!(Language::from_lang_id(0x040C), Language::English); // fr-FR
    }
}
