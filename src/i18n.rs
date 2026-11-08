//! UI text. The language follows the system unless chosen in Settings › General.
//!
//! Spanish and English stay inline as pairs (`tr!`); the other languages are tables in
//! `src/i18n/` that translate the English text. A text missing from a table shows in English.

use std::collections::HashMap;
use std::fmt::{Display, Write};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

mod de;
mod fr;
mod it;
mod ja;
mod pl;
mod ru;
mod zh_hans;
mod zh_hant;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Language {
    Spanish,
    English,
    German,
    French,
    Italian,
    Polish,
    Russian,
    Japanese,
    ChineseSimplified,
    ChineseTraditional,
}

/// The current language, as its discriminant.
static CURRENT: AtomicU8 = AtomicU8::new(Language::Spanish as u8);

/// Tests that change the language hold this, so they do not change it under each other.
#[cfg(test)]
pub static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Language {
    /// In the order Settings lists them.
    pub const ALL: [Self; 10] = [
        Self::German,
        Self::English,
        Self::Spanish,
        Self::French,
        Self::Italian,
        Self::Polish,
        Self::Russian,
        Self::Japanese,
        Self::ChineseSimplified,
        Self::ChineseTraditional,
    ];

    /// The system display language; English when Obs Lite does not have it.
    pub fn system() -> Self {
        crate::platform::sys::system_language()
    }

    /// From a POSIX locale such as "es_ES.UTF-8" or "zh_TW".
    #[cfg_attr(windows, allow(dead_code))]
    pub fn from_locale(locale: &str) -> Self {
        let locale = locale.split(['.', '@']).next().unwrap_or_default();
        let (language, region) = locale.split_once(['_', '-']).unwrap_or((locale, ""));
        match language {
            "es" => Self::Spanish,
            "de" => Self::German,
            "fr" => Self::French,
            "it" => Self::Italian,
            "pl" => Self::Polish,
            "ru" => Self::Russian,
            "ja" => Self::Japanese,
            "zh" if ["TW", "HK", "MO", "Hant"].iter().any(|r| region.starts_with(r)) => Self::ChineseTraditional,
            "zh" => Self::ChineseSimplified,
            _ => Self::English,
        }
    }

    /// From a Windows language id.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn from_lang_id(id: u16) -> Self {
        // zh-TW, zh-HK, zh-MO and zh-Hant; every other Chinese is simplified.
        const TRADITIONAL: [u16; 4] = [0x0404, 0x0C04, 0x1404, 0x7C04];
        match id & 0x3FF {
            0x0A => Self::Spanish,
            0x07 => Self::German,
            0x0C => Self::French,
            0x10 => Self::Italian,
            0x15 => Self::Polish,
            0x19 => Self::Russian,
            0x11 => Self::Japanese,
            0x04 if TRADITIONAL.contains(&id) => Self::ChineseTraditional,
            0x04 => Self::ChineseSimplified,
            _ => Self::English,
        }
    }

    /// The language's own name, so it is recognizable whatever the current language.
    pub fn name(self) -> &'static str {
        match self {
            Self::Spanish => "Español",
            Self::English => "English",
            Self::German => "Deutsch",
            Self::French => "Français",
            Self::Italian => "Italiano",
            Self::Polish => "Polski",
            Self::Russian => "Русский",
            Self::Japanese => "日本語",
            Self::ChineseSimplified => "简体中文",
            Self::ChineseTraditional => "繁體中文",
        }
    }

    /// The name in English, for when the fonts cannot draw its own.
    pub fn english_name(self) -> &'static str {
        match self {
            Self::Japanese => "Japanese",
            Self::ChineseSimplified => "Chinese (Simplified)",
            Self::ChineseTraditional => "Chinese (Traditional)",
            _ => self.name(),
        }
    }

    /// Whether its text needs a font with Chinese characters, which Obs Lite does not bundle.
    pub fn needs_cjk_font(self) -> bool {
        matches!(self, Self::Japanese | Self::ChineseSimplified | Self::ChineseTraditional)
    }

    /// English text and its translation; `None` for the languages written inline.
    fn table(self) -> Option<&'static [(&'static str, &'static str)]> {
        match self {
            Self::Spanish | Self::English => None,
            Self::German => Some(de::TEXT),
            Self::French => Some(fr::TEXT),
            Self::Italian => Some(it::TEXT),
            Self::Polish => Some(pl::TEXT),
            Self::Russian => Some(ru::TEXT),
            Self::Japanese => Some(ja::TEXT),
            Self::ChineseSimplified => Some(zh_hans::TEXT),
            Self::ChineseTraditional => Some(zh_hant::TEXT),
        }
    }
}

static TABLES: LazyLock<HashMap<Language, HashMap<&'static str, &'static str>>> = LazyLock::new(|| {
    Language::ALL.iter().filter_map(|&language| Some((language, language.table()?.iter().copied().collect()))).collect()
});

pub fn set(language: Language) {
    CURRENT.store(language as u8, Ordering::Relaxed);
}

pub fn current() -> Language {
    let current = CURRENT.load(Ordering::Relaxed);
    Language::ALL.into_iter().find(|&l| l as u8 == current).unwrap_or(Language::Spanish)
}

/// The text in the current language.
pub fn pick(es: &'static str, en: &'static str) -> &'static str {
    match current() {
        Language::Spanish => es,
        Language::English => en,
        language => TABLES.get(&language).and_then(|t| t.get(en)).copied().unwrap_or(en),
    }
}

/// Puts `args` into the `{}` (or `{:.N}`) holes of `template`, in order: the translated
/// templates are only known at run time, so `format!` cannot read them.
pub fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut args = args.iter();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else { break };
        out.push_str(&rest[..start]);
        if let Some(arg) = args.next() {
            let _ = match rest[start + 1..start + len].strip_prefix(":.").and_then(|p| p.parse::<usize>().ok()) {
                Some(precision) => write!(out, "{arg:.precision$}"),
                None => write!(out, "{arg}"),
            };
        }
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

/// `tr!("es", "en")` picks a string; with arguments it formats the chosen one.
macro_rules! tr {
    ($es:literal, $en:literal $(,)?) => {
        $crate::i18n::pick($es, $en)
    };
    ($es:literal, $en:literal, $($arg:expr),+ $(,)?) => {
        match $crate::i18n::current() {
            $crate::i18n::Language::Spanish => format!($es, $($arg),+),
            $crate::i18n::Language::English => format!($en, $($arg),+),
            _ => $crate::i18n::fill(
                $crate::i18n::pick($es, $en),
                &[$(&$arg as &dyn ::std::fmt::Display),+],
            ),
        }
    };
}
pub(crate) use tr;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn every_language_variant_is_recognized() {
        assert_eq!(Language::from_lang_id(0x0C0A), Language::Spanish); // es-ES
        assert_eq!(Language::from_lang_id(0x080A), Language::Spanish); // es-MX
        assert_eq!(Language::from_lang_id(0x0409), Language::English); // en-US
        assert_eq!(Language::from_lang_id(0x040C), Language::French); // fr-FR
        assert_eq!(Language::from_lang_id(0x0C07), Language::German); // de-AT
        assert_eq!(Language::from_lang_id(0x0410), Language::Italian); // it-IT
        assert_eq!(Language::from_lang_id(0x0415), Language::Polish); // pl-PL
        assert_eq!(Language::from_lang_id(0x0419), Language::Russian); // ru-RU
        assert_eq!(Language::from_lang_id(0x0411), Language::Japanese); // ja-JP
        assert_eq!(Language::from_lang_id(0x0804), Language::ChineseSimplified); // zh-CN
        assert_eq!(Language::from_lang_id(0x1004), Language::ChineseSimplified); // zh-SG
        assert_eq!(Language::from_lang_id(0x0404), Language::ChineseTraditional); // zh-TW
        assert_eq!(Language::from_lang_id(0x0C04), Language::ChineseTraditional); // zh-HK
        assert_eq!(Language::from_lang_id(0x0416), Language::English); // pt-BR: not translated
    }

    #[test]
    fn every_locale_is_recognized() {
        for locale in ["es_ES.UTF-8", "es_MX", "es", "es.UTF-8", "es_ES@euro"] {
            assert_eq!(Language::from_locale(locale), Language::Spanish, "{locale}");
        }
        for locale in ["en_US.UTF-8", "C", "POSIX", "", "eu_ES.UTF-8", "esperanto", "pt_BR.UTF-8"] {
            assert_eq!(Language::from_locale(locale), Language::English, "{locale}");
        }
        assert_eq!(Language::from_locale("de_DE.UTF-8"), Language::German);
        assert_eq!(Language::from_locale("fr_CA"), Language::French);
        assert_eq!(Language::from_locale("it"), Language::Italian);
        assert_eq!(Language::from_locale("pl_PL.UTF-8"), Language::Polish);
        assert_eq!(Language::from_locale("ru_RU.UTF-8"), Language::Russian);
        assert_eq!(Language::from_locale("ja_JP.UTF-8"), Language::Japanese);
        assert_eq!(Language::from_locale("zh_CN.UTF-8"), Language::ChineseSimplified);
        assert_eq!(Language::from_locale("zh"), Language::ChineseSimplified);
        assert_eq!(Language::from_locale("zh_TW.UTF-8"), Language::ChineseTraditional);
        assert_eq!(Language::from_locale("zh_HK"), Language::ChineseTraditional);
    }

    #[test]
    fn fill_puts_the_arguments_in_order() {
        assert_eq!(fill("a {} b {} c", &[&1, &"x"]), "a 1 b x c");
        assert_eq!(fill("{} ({:.1}%)", &[&3, &2.345]), "3 (2.3%)");
        assert_eq!(fill("no holes", &[&1]), "no holes");
        assert_eq!(fill("{} {}", &[&1]), "1 ");
    }

    /// The English text of every `tr!` in the source, unescaped.
    fn english_texts() -> Vec<String> {
        fn literal(s: &str) -> Option<(String, &str)> {
            let s = s.trim_start().strip_prefix('"')?;
            let mut out = String::new();
            let mut chars = s.char_indices();
            while let Some((i, c)) = chars.next() {
                match c {
                    '"' => return Some((out, &s[i + 1..])),
                    '\\' => match chars.next()?.1 {
                        'n' => out.push('\n'),
                        c => out.push(c),
                    },
                    c => out.push(c),
                }
            }
            None
        }
        fn visit(dir: &Path, texts: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    if path.file_name() != Some("i18n".as_ref()) {
                        visit(&path, texts);
                    }
                } else if path.extension() == Some("rs".as_ref()) && path.file_name() != Some("i18n.rs".as_ref()) {
                    let source = std::fs::read_to_string(&path).unwrap();
                    for (_, call) in source.match_indices("tr!(").map(|(i, _)| ((), &source[i + 4..])) {
                        let (_, rest) = literal(call).unwrap_or_else(|| panic!("tr! without a Spanish literal in {path:?}"));
                        let rest = rest.trim_start().strip_prefix(',').expect("tr! with two literals");
                        let (english, _) = literal(rest).unwrap_or_else(|| panic!("tr! without an English literal in {path:?}"));
                        if !texts.contains(&english) {
                            texts.push(english);
                        }
                    }
                }
            }
        }
        let mut texts = Vec::new();
        visit(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut texts);
        texts
    }

    fn holes(text: &str) -> Vec<&str> {
        text.match_indices('{').filter_map(|(i, _)| Some(&text[i..i + text[i..].find('}')? + 1])).collect()
    }

    #[test]
    fn every_table_translates_every_text_with_the_same_holes() {
        let texts = english_texts();
        assert!(texts.len() > 100, "found only {} texts", texts.len());
        for language in Language::ALL {
            let Some(table) = language.table() else { continue };
            let map = &TABLES[&language];
            assert_eq!(map.len(), table.len(), "{language:?}: a text appears twice");
            for text in &texts {
                let translation = map.get(text.as_str()).unwrap_or_else(|| panic!("{language:?}: missing {text:?}"));
                assert_eq!(holes(translation), holes(text), "{language:?}: {text:?} → {translation:?}");
                assert!(!translation.trim().is_empty(), "{language:?}: {text:?} is empty");
            }
            for (english, _) in table {
                assert!(texts.iter().any(|t| t == english), "{language:?}: {english:?} is no longer used");
            }
        }
    }

    #[test]
    fn a_translated_format_fills_its_holes() {
        let _language = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set(Language::German);
        let text = tr!("Fotogramas perdidos {} ({:.1}%)", "Dropped frames {} ({:.1}%)", 12, 3.456);
        set(Language::Spanish);
        assert_eq!(text, "Verlorene Frames 12 (3.5%)");
    }
}
