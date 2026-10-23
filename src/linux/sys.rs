//! Small system calls the rest of the app needs, on Linux.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::thread;

use ashpd::desktop::open_uri::{OpenDirectoryRequest, OpenFileRequest};
use ashpd::documents::{DocumentID, Documents};

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

/// Opens `dir` in the file manager, through the OpenURI portal (which also works in the sandbox).
pub fn open_folder(dir: &Path) {
    open_with_portal(dir.to_owned(), false);
}

/// Opens the folder that holds `file` in the file manager, with the file selected.
pub fn show_in_folder(file: &Path) {
    open_with_portal(file.to_owned(), true);
}

fn open_with_portal(path: PathBuf, select: bool) {
    // The desktop may take a moment or ask which application to use; not on the UI thread.
    thread::spawn(move || {
        let Ok(file) = File::open(&path) else { return };
        let _ = pollster::block_on(async {
            if select {
                OpenDirectoryRequest::default().send(&file).await
            } else {
                OpenFileRequest::default().send_file(&file).await
            }
        });
    });
}

/// The path as the user knows it. A folder chosen in the folder picker reaches the sandbox as
/// /run/user/UID/doc/ID/NAME, which means nothing to them; the document portal knows where it
/// really is. Answers are kept, as this is called while drawing.
pub fn display_path(path: &Path) -> PathBuf {
    static HOSTS: LazyLock<Mutex<HashMap<String, Option<PathBuf>>>> = LazyLock::new(Mutex::default);
    let Some((id, rest)) = document_parts(path) else { return path.to_owned() };
    let host = HOSTS.lock().unwrap().entry(id.clone()).or_insert_with(|| host_path(&id)).clone();
    host.map_or_else(|| path.to_owned(), |host| inside(host, &rest))
}

fn host_path(id: &str) -> Option<PathBuf> {
    pollster::block_on(async {
        let id = DocumentID::from(id);
        let mut paths = Documents::new().await.ok()?.host_paths(std::slice::from_ref(&id)).await.ok()?;
        paths.remove(&id).map(|p| p.as_ref().to_owned())
    })
}

/// `rest` under `host`; nothing under it is `host` itself, without the trailing slash `join`
/// would add.
fn inside(host: PathBuf, rest: &Path) -> PathBuf {
    if rest.as_os_str().is_empty() { host } else { host.join(rest) }
}

/// For a path in the document portal (/run/user/UID/doc/ID/NAME/...), the document id and what
/// follows the exported file or folder (NAME).
fn document_parts(path: &Path) -> Option<(String, PathBuf)> {
    let mut parts = path.strip_prefix("/run/user").ok()?.components();
    parts.next()?; // the user id
    if parts.next()?.as_os_str() != "doc" {
        return None;
    }
    let id = parts.next()?.as_os_str().to_str()?.to_owned();
    parts.next()?;
    Some((id, parts.as_path().to_owned()))
}

/// Linux programs keep their terminal; nothing to do.
pub fn attach_console() {}

pub fn http_get(_url: &str) -> Result<String, String> {
    Err(super::not_yet())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_portal_paths_are_recognised() {
        let parts = document_parts(Path::new("/run/user/1000/doc/c674746b/Descargas/clip.mp4"));
        assert_eq!(parts, Some(("c674746b".to_owned(), PathBuf::from("clip.mp4"))));
        let parts = document_parts(Path::new("/run/user/1000/doc/c674746b/Descargas"));
        assert_eq!(parts, Some(("c674746b".to_owned(), PathBuf::new())));
        assert_eq!(document_parts(Path::new("/home/dark/Vídeos/clip.mp4")), None);
        assert_eq!(document_parts(Path::new("/run/user/1000/bus")), None);
        assert_eq!(display_path(Path::new("/home/dark/Vídeos")), PathBuf::from("/home/dark/Vídeos"));
    }

    #[test]
    fn a_granted_folder_shows_without_a_trailing_slash() {
        // PathBuf comparison ignores a trailing slash; the text shown does not.
        let shown = |rest: &str| inside(PathBuf::from("/home/dark/Descargas"), Path::new(rest)).display().to_string();
        assert_eq!(shown(""), "/home/dark/Descargas");
        assert_eq!(shown("clip.mp4"), "/home/dark/Descargas/clip.mp4");
    }
}
