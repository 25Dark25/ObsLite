//! Audio capture through WASAPI, in shared mode with automatic conversion to 48 kHz stereo f32.

use std::collections::VecDeque;
use std::error::Error;
use std::thread;
use std::time::Duration;

use wasapi::{AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

use crate::audio::{CHANNELS, Device, InputKind, InputShared, SAMPLE_RATE};

fn direction(kind: InputKind) -> Direction {
    match kind {
        // Loopback capture opens the render (output) endpoint.
        InputKind::System | InputKind::SystemExcept(_) | InputKind::Application(_) => Direction::Render,
        InputKind::Microphone => Direction::Capture,
    }
}

/// Active endpoints for an input kind.
pub fn list_devices(kind: InputKind) -> Vec<Device> {
    let list = move || -> Result<Vec<Device>, Box<dyn Error>> {
        wasapi::initialize_mta().ok()?;
        let collection = DeviceEnumerator::new()?.get_device_collection(&direction(kind))?;
        let mut out = Vec::new();
        for device in &collection {
            let device = device?;
            out.push(Device { id: device.get_id()?, name: device.get_friendlyname()? });
        }
        Ok(out)
    };
    // COM is initialized per thread; keep it off the UI thread.
    thread::spawn(move || list().map_err(|e| e.to_string())).join().ok().and_then(Result::ok).unwrap_or_default()
}

pub fn capture(kind: InputKind, device_id: Option<&str>, shared: &InputShared) -> Result<(), Box<dyn Error>> {
    wasapi::initialize_mta().ok()?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, CHANNELS, None);
    let (mut client, mode) = match kind {
        // Process loopback (Windows 10 2004+) only works event-driven. It includes the process
        // tree, or excludes it and takes everything else that plays.
        InputKind::Application(pid) | InputKind::SystemExcept(pid) => (
            AudioClient::new_application_loopback_client(pid, matches!(kind, InputKind::Application(_)))?,
            StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 },
        ),
        _ => {
            let enumerator = DeviceEnumerator::new()?;
            let device = match device_id {
                Some(id) => enumerator.get_device(id)?,
                None => enumerator.get_default_device(&direction(kind))?,
            };
            (device.get_iaudioclient()?, StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: 1_000_000 })
        }
    };
    client.initialize_client(&format, &Direction::Capture, &mode)?;
    let event = match mode {
        StreamMode::EventsShared { .. } => Some(client.set_get_eventhandle()?),
        _ => None,
    };
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;

    let mut raw: VecDeque<u8> = VecDeque::new();
    while !shared.stopped() {
        capture.read_from_device_to_deque(&mut raw)?;
        let usable = raw.len() / 4 * 4;
        if usable > 0 {
            let bytes: Vec<u8> = raw.drain(..usable).collect();
            shared.deliver(bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect());
        } else {
            shared.silent();
        }
        match &event {
            // Times out while the application is silent; that is fine.
            Some(event) => {
                let _ = event.wait_for_event(20);
            }
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    client.stop_stream()?;
    Ok(())
}
