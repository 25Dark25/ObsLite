//! Looks up the latest obs-lite release on GitHub once at start-up. Settings › General can turn
//! it off. The user is told, with a link to the website; a copy installed with the installer can
//! also update itself, when the user asks: the new installer is downloaded, checked against its
//! SHA-256 from GitHub and run once Obs Lite has closed.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

const LATEST: &str = "https://api.github.com/repos/25Dark25/ObsLite/releases/latest";
/// Where every release file is downloaded from: an installer link is taken only if it is one.
const DOWNLOADS: &str = "https://github.com/25Dark25/ObsLite/releases/download/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The release's installer, if it has one with a SHA-256.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub installer: Option<Installer>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))] // the Flatpak has no network access
pub struct Installer {
    pub url: String,
    /// Lowercase hexadecimal.
    pub sha256: String,
}

/// The latest release, if it is newer than this build. Errors (no network, rate limit…) are
/// returned so the caller can ignore them.
pub fn newer_release() -> Result<Option<Release>, String> {
    let json = crate::platform::sys::http_get(LATEST)?;
    let release = parse(&json).ok_or("unexpected response")?;
    Ok(is_newer(&release.version, env!("CARGO_PKG_VERSION")).then_some(release))
}

/// The website's download section, offering the file this copy came from: the installer, or the
/// portable zip. The website always offers the latest release.
pub fn download_page() -> &'static str {
    if installed() {
        "https://25dark25.github.io/ObsLite/?file=msi#download"
    } else {
        "https://25dark25.github.io/ObsLite/?file=zip#download"
    }
}

/// Whether this copy runs from where the installer puts it, so an installer can update it.
pub fn installed() -> bool {
    match (std::env::current_exe(), std::env::var_os("LOCALAPPDATA")) {
        (Ok(exe), Some(local)) => is_installed(&exe, Path::new(&local)),
        _ => false,
    }
}

/// Whether `exe` is where the installer puts it: `%LOCALAPPDATA%\Programs\obs-lite`.
fn is_installed(exe: &Path, local_app_data: &Path) -> bool {
    let folder = local_app_data.join("Programs").join("obs-lite");
    exe.parent().is_some_and(|dir| dir.as_os_str().eq_ignore_ascii_case(folder.as_os_str()))
}

/// Where a downloaded installer waits to be run. Removed at the next start.
#[cfg(windows)]
pub fn installer_path() -> PathBuf {
    std::env::temp_dir().join("obs-lite-update.msi")
}

/// Downloads `installer` to [`installer_path`] and checks it against its SHA-256.
#[cfg(windows)]
pub fn download(installer: &Installer) -> Result<PathBuf, String> {
    use crate::platform::sys;
    let bytes = sys::http_get_bytes(&installer.url)?;
    if hex(&sys::sha256(&bytes)?) != installer.sha256 {
        return Err(crate::i18n::tr!("el archivo no coincide con su SHA-256", "the file does not match its SHA-256").to_owned());
    }
    let path = installer_path();
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The installer to run once the window has closed, set by [`install_on_exit`].
#[cfg(windows)]
static PENDING: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(windows)]
pub fn install_on_exit(msi: PathBuf) {
    *PENDING.lock().unwrap() = Some(msi);
}

/// Runs the installer set by [`install_on_exit`], if any, and Obs Lite again after it. Called
/// last thing before exiting, so that nothing of this copy is in use any more.
#[cfg(windows)]
pub fn run_pending_install() {
    if let Some(msi) = PENDING.lock().unwrap().take()
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = crate::platform::sys::install_and_restart(&msi, &exe);
    }
}

#[cfg(windows)]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Release tag and installer from the GitHub API response.
fn parse(json: &str) -> Option<Release> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let version = value.get("tag_name")?.as_str()?.trim_start_matches('v').to_owned();
    let name = format!("obs-lite-{version}-windows-x64.msi");
    let installer = value.get("assets").and_then(|a| a.as_array()).and_then(|assets| {
        let asset = assets.iter().find(|a| a.get("name").and_then(|n| n.as_str()) == Some(name.as_str()))?;
        let url = asset.get("browser_download_url")?.as_str().filter(|u| u.starts_with(DOWNLOADS))?;
        let sha256 = asset.get("digest")?.as_str()?.strip_prefix("sha256:")?;
        let valid = sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        valid.then(|| Installer { url: url.to_owned(), sha256: sha256.to_owned() })
    });
    Some(Release { version, installer })
}

/// Whether `candidate` ("0.3.0") is a later version than `current`.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Option<Vec<u64>> { v.split('.').map(|p| p.parse().ok()).collect() };
    match (parts(candidate), parts(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.3.0", "0.2.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.0", "0.2.1"));
        assert!(!is_newer("0.3.0-beta", "0.2.1")); // not a plain version: ignored
    }

    #[test]
    fn release_is_read_from_the_github_response() {
        let json = r#"{"tag_name": "v0.3.0", "html_url": "https://github.com/25Dark25/ObsLite/releases/tag/v0.3.0", "assets": []}"#;
        assert_eq!(parse(json), Some(Release { version: "0.3.0".into(), installer: None }));
        assert_eq!(parse(r#"{"message": "Not Found"}"#), None);
    }

    const SHA: &str = "96fe2c45db29a4a758f426757d07f2d4b4025a63f5066c53dd473f1c80db23ae";

    fn with_asset(name: &str, url: &str, digest: &str) -> String {
        let zip = serde_json::json!({"name": "obs-lite-0.3.0-portable-windows-x64.zip", "browser_download_url": "https://github.com/25Dark25/ObsLite/releases/download/v0.3.0/obs-lite-0.3.0-portable-windows-x64.zip", "digest": format!("sha256:{SHA}")});
        let asset = serde_json::json!({"name": name, "browser_download_url": url, "digest": digest});
        serde_json::json!({"tag_name": "v0.3.0", "assets": [zip, asset]}).to_string()
    }

    #[test]
    fn the_installer_and_its_sha256_are_read_from_the_github_response() {
        let url = "https://github.com/25Dark25/ObsLite/releases/download/v0.3.0/obs-lite-0.3.0-windows-x64.msi";
        let release = parse(&with_asset("obs-lite-0.3.0-windows-x64.msi", url, &format!("sha256:{SHA}"))).unwrap();
        assert_eq!(release.installer, Some(Installer { url: url.into(), sha256: SHA.into() }));
    }

    #[test]
    fn an_installer_is_offered_only_from_this_repository_with_a_valid_sha256() {
        let url = "https://github.com/25Dark25/ObsLite/releases/download/v0.3.0/obs-lite-0.3.0-windows-x64.msi";
        let name = "obs-lite-0.3.0-windows-x64.msi";
        for (name, url, digest) in [
            ("obs-lite-0.2.0-windows-x64.msi", url, format!("sha256:{SHA}")), // another version's
            (name, "https://example.com/obs-lite-0.3.0-windows-x64.msi", format!("sha256:{SHA}")),
            (name, "http://github.com/25Dark25/ObsLite/releases/download/v0.3.0/obs-lite-0.3.0-windows-x64.msi", format!("sha256:{SHA}")),
            (name, url, String::new()),
            (name, url, format!("sha512:{SHA}")),
            (name, url, format!("sha256:{}", &SHA[1..])),
            (name, url, format!("sha256:{}", SHA.to_uppercase())),
        ] {
            assert_eq!(parse(&with_asset(name, url, &digest)).unwrap().installer, None, "{name} {url} {digest}");
        }
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "network, run by hand: cargo test the_latest_installer -- --ignored"]
    fn the_latest_installer_is_downloaded_and_checked() {
        let release = parse(&crate::platform::sys::http_get(LATEST).unwrap()).unwrap();
        let mut installer = release.installer.expect("an installer with a SHA-256");
        let path = download(&installer).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 1_000_000);
        std::fs::remove_file(&path).unwrap();
        // A wrong hash is refused and nothing is left to run.
        installer.sha256 = "0".repeat(64);
        assert!(download(&installer).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn installed_copies_are_told_apart_from_portable_ones() {
        // Built with join so the separators are right on every platform the tests run on.
        let local = Path::new("Users").join("Ana").join("AppData").join("Local");
        let under = |parts: &[&str]| parts.iter().fold(local.clone(), |path, part| path.join(part));
        assert!(is_installed(&under(&["Programs", "obs-lite", "obs-lite.exe"]), &local));
        assert!(is_installed(&under(&["programs", "OBS-LITE", "obs-lite.exe"]), &local), "Windows paths ignore case");
        assert!(!is_installed(&under(&["Programs", "obs-lite", "sub", "obs-lite.exe"]), &local));
        assert!(!is_installed(&Path::new("Users").join("Ana").join("Downloads").join("obs-lite.exe"), &local));
    }
}
