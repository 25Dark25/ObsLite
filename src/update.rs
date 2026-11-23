//! Looks up the latest obs-lite release on GitHub once at start-up. Settings › General can turn
//! it off. Nothing is downloaded or installed: the user is only told, with a link to the website.

use std::path::Path;

const LATEST: &str = "https://api.github.com/repos/25Dark25/ObsLite/releases/latest";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
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
    let installed = match (std::env::current_exe(), std::env::var_os("LOCALAPPDATA")) {
        (Ok(exe), Some(local)) => is_installed(&exe, Path::new(&local)),
        _ => false,
    };
    if installed {
        "https://25dark25.github.io/ObsLite/?file=msi#download"
    } else {
        "https://25dark25.github.io/ObsLite/?file=zip#download"
    }
}

/// Whether `exe` is where the installer puts it: `%LOCALAPPDATA%\Programs\obs-lite`.
fn is_installed(exe: &Path, local_app_data: &Path) -> bool {
    let folder = local_app_data.join("Programs").join("obs-lite");
    exe.parent().is_some_and(|dir| dir.as_os_str().eq_ignore_ascii_case(folder.as_os_str()))
}

/// Release tag from the GitHub API response.
fn parse(json: &str) -> Option<Release> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = value.get("tag_name")?.as_str()?;
    Some(Release { version: tag.trim_start_matches('v').to_owned() })
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
        assert_eq!(parse(json), Some(Release { version: "0.3.0".into() }));
        assert_eq!(parse(r#"{"message": "Not Found"}"#), None);
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
