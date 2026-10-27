//! Audio capture through WASAPI, in shared mode with automatic conversion to 48 kHz stereo f32.

use std::collections::VecDeque;
use std::error::Error;
use std::thread;
use std::time::{Duration, Instant};

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

/// Captures until asked to stop. An input on the default device follows it: when Windows
/// switches to another (headphones plugged in) or the one in use goes away, the new default is
/// opened. A chosen device that goes away, or a stream that never opened, ends with an error.
pub fn capture(kind: InputKind, device_id: Option<&str>, shared: &InputShared) -> Result<(), Box<dyn Error>> {
    wasapi::initialize_mta().ok()?;
    let follows_default = device_id.is_none() && matches!(kind, InputKind::System | InputKind::Microphone);
    let mut opened = false;
    while !shared.stopped() {
        match stream(kind, device_id, shared, follows_default, &mut opened) {
            Ok(()) => {}
            Err(_) if follows_default && opened => {
                // No default device for now (the only one was unplugged): try again shortly, still
                // stopping at once when asked.
                shared.silent();
                for _ in 0..20 {
                    if shared.stopped() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// One stream, until asked to stop or, when following the default device, until that changes.
fn stream(kind: InputKind, device_id: Option<&str>, shared: &InputShared, follows_default: bool, opened: &mut bool) -> Result<(), Box<dyn Error>> {
    let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, CHANNELS, None);
    let enumerator = DeviceEnumerator::new()?;
    let mut device_in_use = None;
    let (mut client, mode) = match kind {
        // Process loopback (Windows 10 2004+) only works event-driven. It includes the process
        // tree, or excludes it and takes everything else that plays.
        InputKind::Application(pid) | InputKind::SystemExcept(pid) => (
            AudioClient::new_application_loopback_client(pid, matches!(kind, InputKind::Application(_)))?,
            StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 },
        ),
        _ => {
            let device = match device_id {
                Some(id) => enumerator.get_device(id)?,
                None => enumerator.get_default_device(&direction(kind))?,
            };
            device_in_use = Some(device.get_id()?);
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
    *opened = true;

    let mut raw: VecDeque<u8> = VecDeque::new();
    let mut checked = Instant::now();
    while !shared.stopped() {
        // Every packet waiting, not one per wake-up: otherwise a delay is never caught up.
        loop {
            let before = raw.len();
            capture.read_from_device_to_deque(&mut raw)?;
            if raw.len() == before {
                break;
            }
        }
        let usable = raw.len() / 4 * 4;
        if usable > 0 {
            let bytes: Vec<u8> = raw.drain(..usable).collect();
            shared.deliver(bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect());
        } else {
            shared.silent();
        }
        if follows_default && checked.elapsed() >= Duration::from_secs(1) {
            checked = Instant::now();
            let default = enumerator.get_default_device(&direction(kind)).and_then(|d| d.get_id());
            if default.ok() != device_in_use {
                break;
            }
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
