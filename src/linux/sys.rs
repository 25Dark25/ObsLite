//! Small system calls the rest of the app needs, on Linux.

use crate::i18n::Language;

/// No native handle is needed on Linux yet.
pub fn window_handle(_cc: &eframe::CreationContext<'_>) -> Option<isize> {
    None
}

pub fn set_taskbar_button(_window: isize, _visible: bool) {}

pub fn exclude_from_capture(_window: isize, _exclude: bool) {}

/// CPU time used by this process so far, in 100 ns units.
pub fn process_cpu_time() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    let micros = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    Some((micros(usage.ru_utime) + micros(usage.ru_stime)) * 10)
}

/// The language of the user's locale (the first of LC_ALL, LC_MESSAGES and LANG that is set).
pub fn system_language() -> Language {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()));
    Language::from_locale(locale.as_deref().unwrap_or(""))
}

/// Tells the desktop portals who we are. Outside a sandbox they cannot tell, and some (global
/// shortcuts, for one) need to know. Inside Flatpak this fails harmlessly.
pub fn register_app() {
    if let Ok(id) = ashpd::AppID::try_from(super::APP_ID) {
        let _ = pollster::block_on(ashpd::register_host_app(id));
    }
}

/// Linux programs keep their terminal; nothing to do.
pub fn attach_console() {}

pub fn http_get(_url: &str) -> Result<String, String> {
    Err(super::not_yet())
}
