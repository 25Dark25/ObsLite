use crate::camera::{CameraDevice, CameraShared};

pub fn list_devices() -> Vec<CameraDevice> {
    Vec::new()
}

pub fn capture(_device: &str, _shared: &CameraShared, _repaint: Option<&eframe::egui::Context>) -> Result<(), String> {
    Err(super::not_yet())
}
