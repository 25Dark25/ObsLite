//! Small Windows system calls the rest of the app needs.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// The local date and time: year, month, day, hour, minute, second.
pub fn local_time() -> [u32; 6] {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    [t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond].map(u32::from)
}

/// The Windows display language.
pub fn system_language() -> Language {
    Language::from_lang_id(unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() })
}

/// A system font with the characters of Chinese or Japanese, as the file and the face in it.
pub fn cjk_font(language: Language) -> Option<(Vec<u8>, u32)> {
    // Each language's own UI font first: the same characters are drawn differently in each.
    let files: &[&str] = match language {
        Language::Japanese => &["YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"],
        Language::ChineseSimplified => &["msyh.ttc", "simsun.ttc"],
        Language::ChineseTraditional => &["msjh.ttc", "mingliu.ttc"],
        _ => return None,
    };
    let fonts = PathBuf::from(std::env::var_os("WINDIR")?).join("Fonts");
    files.iter().find_map(|file| std::fs::read(fonts.join(file)).ok()).map(|bytes| (bytes, 0))
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

/// Opens `dir` in Explorer.
pub fn open_folder(dir: &Path) {
    let _ = explorer().raw_arg(quoted(dir)).spawn();
}

/// Opens the folder that holds `file` in Explorer, with the file selected.
pub fn show_in_folder(file: &Path) {
    let mut arg = OsString::from("/select,");
    arg.push(quoted(file));
    let _ = explorer().raw_arg(arg).spawn();
}

/// Opens a web page in the default browser.
pub fn open_url(url: &str) {
    let _ = explorer().arg(url).spawn();
}

/// Explorer from the Windows folder. Found by name, it would be looked for first in obs-lite's
/// own folder, which for the portable build may be a downloads folder.
fn explorer() -> Command {
    use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
    let mut dir = [0u16; 260];
    let len = unsafe { GetSystemWindowsDirectoryW(Some(&mut dir)) } as usize;
    match dir.get(..len).filter(|d| !d.is_empty()) {
        Some(dir) => Command::new(PathBuf::from(OsString::from_wide(dir)).join("explorer.exe")),
        None => Command::new("explorer"),
    }
}

/// `path` in quotes: Explorer splits its arguments at commas, which a path can hold, and a path
/// cannot hold a quote.
fn quoted(path: &Path) -> OsString {
    let mut arg = OsString::from("\"");
    arg.push(path);
    arg.push("\"");
    arg
}

/// The path as the user should see it; on Windows, the path itself.
pub fn display_path(path: &Path) -> PathBuf {
    path.to_owned()
}

/// Files are never kept only in memory here (that is the Linux sandbox's case).
pub fn volatile(_path: &Path) -> bool {
    false
}

/// Starts COM on this thread (multithreaded) unless it already runs here, as it must before
/// calling Windows Runtime APIs, and keeps the multithreaded apartment up for good. windows-rs
/// caches the objects those APIs are reached through, for the whole process; once every thread
/// that had started COM ended, Windows unloaded the code behind them, and the next call crashed.
/// On the UI thread, which winit started as single-threaded, the first part changes nothing.
pub fn init_com() {
    use std::sync::Once;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoIncrementMTAUsage, CoInitializeEx};
    static KEEP_MTA: Once = Once::new();
    // The cookie would only give the usage back, which is never wanted.
    KEEP_MTA.call_once(|| {
        let _ = unsafe { CoIncrementMTAUsage() };
    });
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

/// An HTTP client for GitHub, for a thread of its own.
fn http_client() -> windows::core::Result<windows::Web::Http::HttpClient> {
    use windows::core::HSTRING;
    init_com();
    let client = windows::Web::Http::HttpClient::new()?;
    // GitHub's API rejects requests without a User-Agent.
    let agent = concat!("obs-lite/", env!("CARGO_PKG_VERSION"));
    client.DefaultRequestHeaders()?.TryAppendWithoutValidation(&HSTRING::from("User-Agent"), &HSTRING::from(agent))?;
    Ok(client)
}

fn uri(url: &str) -> windows::core::Result<windows::Foundation::Uri> {
    windows::Foundation::Uri::CreateUri(&windows::core::HSTRING::from(url))
}

/// GET `url` and return the body as text.
pub fn http_get(url: &str) -> Result<String, String> {
    let get = || -> windows::core::Result<String> { Ok(http_client()?.GetStringAsync(&uri(url)?)?.join()?.to_string()) };
    get().map_err(|e| e.to_string())
}

/// GET `url`, following redirects, and return the body.
pub fn http_get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let get = || -> windows::core::Result<Vec<u8>> {
        let buffer = http_client()?.GetBufferAsync(&uri(url)?)?.join()?;
        let mut bytes = vec![0; buffer.Length()? as usize];
        windows::Storage::Streams::DataReader::FromBuffer(&buffer)?.ReadBytes(&mut bytes)?;
        Ok(bytes)
    };
    get().map_err(|e| e.to_string())
}

/// SHA-256 of `data`, from Windows' own cryptography.
pub fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
    use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut hash = [0; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut hash) }.ok().map_err(|e| e.to_string())?;
    Ok(hash)
}

/// Starts the installer `msi` with no questions, only a progress bar, then `exe` again. Called as
/// the very last thing before exiting: Windows Installer takes seconds to get to replacing files,
/// so by then this process has ended. Each program is waited for with `start /wait`.
pub fn install_and_restart(msi: &Path, exe: &Path) -> Result<(), String> {
    // Inside the quotes cmd takes everything literally but % (environment variables).
    if [msi, exe].iter().any(|p| p.as_os_str().to_string_lossy().contains(['"', '%'])) {
        return Err("unexpected character in a path".into());
    }
    let system = system_dir().ok_or("no system folder")?;
    let mut line = OsString::from("/d /c start \"\" /wait ");
    line.push(quoted(&system.join("msiexec.exe")));
    line.push(" /i ");
    line.push(quoted(msi));
    line.push(" /passive & start \"\" ");
    line.push(quoted(exe));
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new(system.join("cmd.exe")).raw_arg(line).creation_flags(CREATE_NO_WINDOW).spawn().map(drop).map_err(|e| e.to_string())
}

/// System32, so that cmd and msiexec are never looked for in obs-lite's own folder first.
fn system_dir() -> Option<PathBuf> {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut dir = [0u16; 260];
    let len = unsafe { GetSystemDirectoryW(Some(&mut dir)) } as usize;
    dir.get(..len).filter(|d| !d.is_empty()).map(|d| PathBuf::from(OsString::from_wide(d)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_is_started_from_the_windows_folder_with_the_path_in_quotes() {
        let program = PathBuf::from(explorer().get_program());
        assert!(program.is_absolute() && program.ends_with("explorer.exe") && program.exists(), "{program:?}");
        // Unquoted, Explorer took the part before the comma for the folder.
        assert_eq!(quoted(Path::new(r"D:\Videos, games\a.mp4")), OsString::from(r#""D:\Videos, games\a.mp4""#));
    }

    #[test]
    fn sha256_matches_the_standard_test_vector() {
        let hex: String = sha256(b"abc").unwrap().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn the_installer_is_not_run_from_a_path_cmd_would_change() {
        let exe = Path::new(r"C:\Users\Ana\AppData\Local\Programs\obs-lite\obs-lite.exe");
        assert!(install_and_restart(Path::new(r"C:\Temp\100%TEMP%\obs-lite-update.msi"), exe).is_err());
        assert!(install_and_restart(Path::new(r#"C:\Temp\a"b.msi"#), exe).is_err());
        let system = system_dir().unwrap();
        assert!(system.join("msiexec.exe").exists() && system.join("cmd.exe").exists(), "{system:?}");
    }
}
