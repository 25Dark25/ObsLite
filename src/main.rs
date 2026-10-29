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
/// Usage: `--selftest [seconds] [out.mp4] [pause] [720] [replay] [preview] [shot]`. With `pause`,
/// the middle third of the recording is paused, so the file should be one third shorter. With
/// `replay`, the last 3 s are also saved from the replay buffer, next to `out.mp4` as
/// `out-replay.mp4`. With `preview`, preview pictures are made as in the app. With `shot`, a
/// screenshot is taken halfway, next to `out.mp4` as `out-shot.png`. With `region`, a 960×540
/// region from (480, 270) is recorded; with `webcam`, a magenta 320×240 picture stands for a
/// webcam in the bottom-right corner.
fn selftest(args: &[String]) -> Result<(), String> {
    let seconds = selftest_seconds(args.first().map(String::as_str));
    let out = args.get(1).map_or_else(|| PathBuf::from("selftest.mp4"), PathBuf::from);
    let pause = args.iter().any(|a| a == "pause");
    let replay = args.iter().any(|a| a == "replay");
    let preview = args.iter().any(|a| a == "preview");
    let shot = args.iter().any(|a| a == "shot");
    let region = args.iter().any(|a| a == "region").then_some(config::Region { x: 480, y: 270, width: 960, height: 540 });
    let webcam = args.iter().any(|a| a == "webcam");
    let resolution = if args.iter().any(|a| a == "720") { config::Resolution::P720 } else { config::Resolution::Native };

    let source = capture::list_sources().into_iter().next().ok_or("no capture sources")?;
    let system = audio::Input::start(audio::InputKind::System, None, 1.0, None);
    let session = capture::Session::start(
        capture::SessionOptions {
            source: source.id,
            fps: 60,
            cursor: true,
            preview_fps: if preview { 5 } else { 0 },
            record: Some(capture::RecordConfig {
                quality: config::Quality::Medium,
                resolution,
                audio: true,
                region,
                overlay: webcam.then_some((camera::Corner::BottomRight, camera::CameraSize::Medium)),
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
    if webcam {
        // Once, as the encoder keeps a webcam's last picture.
        std::thread::sleep(Duration::from_millis(500));
        if let Some(enc) = session.shared.encoder.lock().unwrap().as_ref()
            && let Some(rect) = enc.overlay_rect((320, 240))
        {
            let magenta = [255u8, 0, 255, 255].repeat(rect.2 as usize * rect.3 as usize);
            enc.push_overlay(rect, &magenta);
        }
    }
    std::thread::sleep(third);
    if pause {
        session.shared.clock.set_paused(true);
    }
    if shot {
        let (width, height, rgba) = session.snapshot()?;
        let path = out.with_file_name(format!("{}-shot.png", out.file_stem().unwrap_or_default().display()));
        capture::write_png(&path, width, height, &rgba)?;
        println!("screenshot: {} ({width}x{height})", path.display());
    }
    std::thread::sleep(third);
    session.shared.clock.set_paused(false);
    std::thread::sleep(third);
    let replay = replay.then(|| session.shared.outputs.lock().unwrap().replay());
    drop(mixer);

    let captured = session.shared.frames.load(std::sync::atomic::Ordering::Relaxed);
    let last_preview = session.shared.preview.lock().unwrap().take();
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
    if let Some(p) = last_preview {
        println!("preview: {} pictures, the last {}x{}", p.seq + 1, p.size[0], p.size[1]);
    }
    println!("file: {}", out.display());
    if let Some(e) = system.shared.error() {
        println!("system audio error: {e}");
    }
    Ok(())
}

/// The self-test's length: 5 s unless a positive number of seconds is given (a negative,
/// infinite or NaN one would make the sleeps panic, which aborts mid-recording).
fn selftest_seconds(arg: Option<&str>) -> f64 {
    arg.and_then(|s| s.parse::<f64>().ok()).filter(|s| s.is_finite() && *s > 0.0).unwrap_or(5.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_self_test_lasts_a_positive_time() {
        assert_eq!(selftest_seconds(Some("6")), 6.0);
        assert_eq!(selftest_seconds(Some("2.5")), 2.5);
        for bad in ["-5", "0", "nan", "inf", "x"] {
            assert_eq!(selftest_seconds(Some(bad)), 5.0, "{bad}");
        }
        assert_eq!(selftest_seconds(None), 5.0);
    }
}
