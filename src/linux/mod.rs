//! Linux implementation. For now just enough for the app to build and show its interface;
//! capture and encoding (the ScreenCast portal, PipeWire and GStreamer) come next.

pub mod audio;
pub mod camera;
pub mod capture;
pub mod encoder;
pub mod sys;

use crate::i18n::tr;

/// Error for what the Linux version cannot do yet.
fn not_yet() -> String {
    tr!("todavía no disponible en Linux", "not available on Linux yet").to_owned()
}
