use std::path::Path;

use crate::camera::PixelRect;
use crate::media::{self, Muxer, Packet};

/// The MP4 writer. There is none yet, so it cannot be created.
pub enum Mp4 {}

impl Muxer for Mp4 {
    type Format = ();

    fn create(_path: &Path, _video: &(), _audio: Option<&()>, _start: i64) -> Result<Self, String> {
        Err(super::not_yet())
    }

    fn write(&self, _video: bool, _packet: &Packet) -> Result<(), String> {
        match *self {}
    }

    fn finish(self) -> Result<(), String> {
        match self {}
    }
}

pub type Outputs = media::Outputs<Mp4>;
pub type PendingFile = media::PendingFile<Mp4>;

/// The encoder. There is none yet, so it cannot be created.
pub enum Encoder {}

impl Encoder {
    pub fn name(&self) -> &str {
        match *self {}
    }

    pub fn overlay_rect(&self, _camera: (u32, u32)) -> Option<PixelRect> {
        match *self {}
    }

    pub fn push_overlay(&self, _rect: PixelRect, _pixels: &[u8]) {
        match *self {}
    }

    pub fn push_audio(&self, _pcm: Vec<u8>) {
        match *self {}
    }

    pub fn stats(&self) -> (u64, u64) {
        match *self {}
    }

    pub fn finish(self) -> Option<PendingFile> {
        match self {}
    }
}
