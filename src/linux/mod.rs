//! Linux implementation: the ScreenCast portal and PipeWire for capture, GStreamer for audio,
//! encoding and MP4. The webcam is not available yet.

pub mod audio;
pub mod camera;
pub mod capture;
pub mod encoder;
mod portal;
pub mod sys;

use crate::i18n::tr;

/// Error for what the Linux version cannot do yet.
fn not_yet() -> String {
    tr!("todavía no disponible en Linux", "not available on Linux yet").to_owned()
}
