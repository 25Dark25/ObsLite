use std::error::Error;

use crate::audio::{Device, InputKind, InputShared};

pub fn list_devices(_kind: InputKind) -> Vec<Device> {
    Vec::new()
}

pub fn capture(_kind: InputKind, _device_id: Option<&str>, _shared: &InputShared) -> Result<(), Box<dyn Error>> {
    Err(super::not_yet().into())
}
