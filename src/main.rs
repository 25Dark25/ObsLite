#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod i18n;

mod app;
mod audio;
mod camera;
mod capture;
mod config;
mod media;
mod mp4;
mod hotkeys;
mod tray;
mod update;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as platform;

use std::path::PathBuf;
use std::time::Duration;

const ICON_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon64.rgba"));

fn main() -> eframe::Result {
    #[cfg(target_os = "linux")]
    platform::sys::register_app();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--selftest") {
        platform::sys::attach_console();
        std::process::exit(match selftest(&args[2..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("selftest failed: {e}");
                1
            }
        });
    }

    let icon = eframe::egui::IconData { rgba: ICON_RGBA.to_vec(), width: 64, height: 64 };
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title("obs-lite")
        .with_icon(icon)
        .with_inner_size([1180.0, 800.0])
        .with_min_inner_size([900.0, 620.0]);
    // The desktop matches the window to the registered app by this id; GNOME only lets the
    // focused app ask for permissions (camera access).
    #[cfg(target_os = "linux")]
    let viewport = viewport.with_app_id(platform::APP_ID);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("obs-lite", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

/// Records the first monitor plus system audio without UI, using the same pipeline as the app.
///
/// Usage: `--selftest [seconds] [out.mp4] [pause] [720] [replay]`. With `pause`, the middle third
/// of the recording is paused, so the file should be one third shorter. With `replay`, the last
/// 3 s are also saved from the replay buffer, next to `out.mp4` as `out-replay.mp4`.
fn selftest(args: &[String]) -> Result<(), String> {
    let seconds: f64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let out = args.get(1).map_or_else(|| PathBuf::from("selftest.mp4"), PathBuf::from);
    let pause = args.iter().any(|a| a == "pause");
    let replay = args.iter().any(|a| a == "replay");
    let resolution = if args.iter().any(|a| a == "720") { config::Resolution::P720 } else { config::Resolution::Native };

    let source = capture::list_sources().into_iter().next().ok_or("no capture sources")?;
    let system = audio::Input::start(audio::InputKind::System, None, 1.0, None);
    let session = capture::Session::start(
        capture::SessionOptions {
            source: source.id,
            fps: 60,
            cursor: true,
            preview_fps: 0,
            record: Some(capture::RecordConfig {
                quality: config::Quality::Medium,
                resolution,
                audio: true,
                region: None,
                overlay: None,
            }),
        },
        None,
    )?;
    {
        let mut outputs = session.shared.outputs.lock().unwrap();
        outputs.start_file(out.clone());
        outputs.set_replay(replay.then_some(3));
    }
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
    let replay = replay.then(|| session.shared.outputs.lock().unwrap().replay());
    drop(mixer);

    let captured = session.shared.frames.load(std::sync::atomic::Ordering::Relaxed);
    let sizes = *session.shared.sizes.lock().unwrap();
    let (encoder, err) = session.stop();
    let encoder = encoder.ok_or_else(|| err.unwrap_or_else(|| "no frames captured".into()))?;
    let (encoded, lagged) = encoder.stats();
    let encoder_name = encoder.name().to_owned();
    encoder.finish().ok_or("nothing was recorded")?.finish()?;
    if let Some(replay) = replay {
        let path = out.with_file_name(format!("{}-replay.mp4", out.file_stem().unwrap_or_default().display()));
        println!("replay: {}", replay?.save(path)?.display());
    }
    println!("source: {}", source.label);
    println!("sizes (capture, output): {sizes:?}");
    println!("encoder: {encoder_name}");
    println!("captured frames: {captured}");
    println!("encoded: {encoded}  lagged: {lagged}");
    println!("file: {}", out.display());
    if let Some(e) = system.shared.error() {
        println!("system audio error: {e}");
    }
    Ok(())
}
