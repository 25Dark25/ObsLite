#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod capture;
mod config;
mod encoder;

use std::path::PathBuf;
use std::time::Duration;

const ICON_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon64.rgba"));

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--selftest") {
        attach_console();
        std::process::exit(match selftest(&args[2..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("selftest failed: {e}");
                1
            }
        });
    }

    let icon = eframe::egui::IconData { rgba: ICON_RGBA.to_vec(), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("obs-lite")
            .with_icon(icon)
            .with_inner_size([1180.0, 800.0])
            .with_min_inner_size([900.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native("obs-lite", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

/// Release builds use the GUI subsystem; reattach to the parent console for selftest output.
fn attach_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    unsafe {
        AttachConsole(u32::MAX);
    }
}

/// Records the first monitor plus system audio without UI, using the same pipeline as the app.
///
/// Usage: `--selftest [seconds] [out.mp4] [pause] [720]`. With `pause`, the middle third of
/// the recording is paused, so the file should be one third shorter.
fn selftest(args: &[String]) -> Result<(), String> {
    let seconds: f64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let out = args.get(1).map_or_else(|| PathBuf::from("selftest.mp4"), PathBuf::from);
    let pause = args.iter().any(|a| a == "pause");
    let resolution = if args.iter().any(|a| a == "720") { config::Resolution::P720 } else { config::Resolution::Native };

    let source = capture::list_sources().into_iter().next().ok_or("no capture sources")?;
    let system = audio::Input::start(audio::InputKind::System, None, 1.0);
    let session = capture::Session::start(
        capture::SessionOptions {
            source: source.id,
            fps: 60,
            cursor: true,
            preview_fps: 0,
            record: Some(capture::RecordConfig {
                path: out.clone(),
                quality: config::Quality::Medium,
                resolution,
                audio: true,
            }),
        },
        None,
    )?;
    let shared = session.shared.clone();
    let mixer = audio::Mixer::start(
        vec![system.shared.clone()],
        shared.encoder_ready.clone(),
        shared.clock.clone(),
        move |pcm| {
            if let Some(enc) = shared.encoder.lock().unwrap().as_ref() {
                enc.push_audio(pcm);
            }
        },
    );
    let third = Duration::from_secs_f64(seconds / 3.0);
    std::thread::sleep(third);
    if pause {
        session.shared.clock.set_paused(true);
    }
    std::thread::sleep(third);
    session.shared.clock.set_paused(false);
    std::thread::sleep(third);
    drop(mixer);

    let captured = session.shared.frames.load(std::sync::atomic::Ordering::Relaxed);
    let sizes = *session.shared.sizes.lock().unwrap();
    let (encoder, err) = session.stop();
    let encoder = encoder.ok_or_else(|| err.unwrap_or_else(|| "no frames captured".into()))?;
    let (encoded, lagged) = encoder.stats();
    encoder.finish()?;
    println!("source: {}", source.label);
    println!("sizes (capture, output): {sizes:?}");
    println!("captured frames: {captured}");
    println!("encoded: {encoded}  lagged: {lagged}");
    println!("file: {}", out.display());
    if let Some(e) = system.shared.error() {
        println!("system audio error: {e}");
    }
    Ok(())
}
