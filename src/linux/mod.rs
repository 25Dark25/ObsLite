//! Linux implementation: the ScreenCast and Camera portals and PipeWire for capture,
//! GStreamer for audio, encoding and MP4, and the GlobalShortcuts portal for hotkeys.

pub mod audio;
pub mod camera;
pub mod capture;
pub mod encoder;
mod gpu;
pub mod hotkeys;
mod portal;
pub mod sys;

use crate::i18n::tr;

/// The app id the desktop knows obs-lite by (the same as the Flatpak's).
pub const APP_ID: &str = "io.github._25Dark25.ObsLite";

/// Error for what the Linux version cannot do yet.
fn not_yet() -> String {
    tr!("todavía no disponible en Linux", "not available on Linux yet").to_owned()
}

/// Runs `work` on a thread of its own and waits for it `wait` at most; `None` if it takes longer
/// (it goes on, and its answer is dropped). For portal calls made from the window: they have no
/// time limit, and the portal may be asking the user something.
fn within<T: Send + 'static>(wait: std::time::Duration, work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = answer.send(work());
    });
    answered.recv_timeout(wait).ok()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_call_that_does_not_answer_is_waited_for_a_moment_only() {
        assert_eq!(within(Duration::from_secs(1), || 7), Some(7));
        let asked = Instant::now();
        assert_eq!(within(Duration::from_millis(200), || std::thread::sleep(Duration::from_secs(5))), None);
        assert!(asked.elapsed() < Duration::from_secs(1), "{:?}", asked.elapsed());
    }
}
