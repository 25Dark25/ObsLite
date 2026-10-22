//! Small Windows system calls the rest of the app needs.

use windows::Win32::Foundation::HWND;

use crate::i18n::Language;

/// The main window's handle, for the calls below.
pub fn window_handle(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Shows or removes the window's taskbar button (minimize to tray).
pub fn set_taskbar_button(window: isize, visible: bool) {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
    use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
    unsafe {
        // Already initialized as STA by winit on this thread; this only makes sure.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(list) = CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER) else { return };
        if list.HrInit().is_ok() {
            let hwnd = HWND(window as *mut _);
            let _ = if visible { list.AddTab(hwnd) } else { list.DeleteTab(hwnd) };
        }
    }
}

/// Keeps the window out of screen captures (ours and other programs').
pub fn exclude_from_capture(window: isize, exclude: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
    let affinity = if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
    // Fails before Windows 10 2004; the window then simply stays capturable.
    let _ = unsafe { SetWindowDisplayAffinity(HWND(window as *mut _), affinity) };
}

/// CPU time used by this process so far, in 100 ns units.
pub fn process_cpu_time() -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    Some(ticks(kernel) + ticks(user))
}

/// The Windows display language.
pub fn system_language() -> Language {
    Language::from_lang_id(unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() })
}

/// Release builds use the GUI subsystem; reattach to the parent console for selftest output.
pub fn attach_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    unsafe {
        AttachConsole(u32::MAX);
    }
}

/// GET `url` and return the body as text.
pub fn http_get(url: &str) -> Result<String, String> {
    use windows::Foundation::Uri;
    use windows::Web::Http::HttpClient;
    use windows::core::HSTRING;
    let get = || -> windows::core::Result<String> {
        let client = HttpClient::new()?;
        // GitHub's API rejects requests without a User-Agent.
        let agent = concat!("obs-lite/", env!("CARGO_PKG_VERSION"));
        client.DefaultRequestHeaders()?.TryAppendWithoutValidation(&HSTRING::from("User-Agent"), &HSTRING::from(agent))?;
        Ok(client.GetStringAsync(&Uri::CreateUri(&HSTRING::from(url))?)?.join()?.to_string())
    };
    get().map_err(|e| e.to_string())
}
