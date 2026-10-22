use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex};

use super::encoder::{Encoder, Outputs};
use crate::capture::{AudioApp, PreviewFrame, SessionOptions, Source};
use crate::media::Clock;

type Size = (u32, u32);

pub fn list_sources() -> Vec<Source> {
    Vec::new()
}

pub fn list_audio_apps() -> Vec<AudioApp> {
    Vec::new()
}

/// State shared between the capture thread, the audio mixer and the UI (as on Windows).
pub struct Shared {
    pub preview: Mutex<Option<PreviewFrame>>,
    pub preview_width: AtomicUsize,
    pub frames: AtomicU64,
    pub clock: Arc<Clock>,
    pub encoder_ready: Arc<AtomicBool>,
    pub encoder: Mutex<Option<Encoder>>,
    pub outputs: Arc<Mutex<Outputs>>,
    pub sizes: Mutex<Option<(Size, Size)>>,
}

pub struct Session {
    pub shared: Arc<Shared>,
}

impl Session {
    pub fn start(_opts: SessionOptions, _repaint: Option<eframe::egui::Context>) -> Result<Self, String> {
        Err(super::not_yet())
    }

    pub fn is_finished(&self) -> bool {
        true
    }

    pub fn source_size(&self) -> Option<(u32, u32)> {
        None
    }

    pub fn snapshot(&self) -> Result<(u32, u32, Vec<u8>), String> {
        Err(super::not_yet())
    }

    pub fn stop(self) -> (Option<Encoder>, Option<String>) {
        (None, None)
    }
}
