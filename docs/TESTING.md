# Testing

## Automated

Run on every pull request by CI (Windows, Rust 1.95 and stable), together with clippy, a release
build and `cargo deny`:

```powershell
cargo test --locked
```

| Test | Checks |
|---|---|
| `encoder::clock_excludes_paused_time` | The recording clock stops while paused. |
| `audio::mixes_with_gain_and_pads_silence` | Inputs are mixed with their gain; a short input is padded with silence. |
| `audio::drops_oldest_samples_beyond_latency_bound` | A backed-up input drops its oldest samples instead of drifting. |
| `config::bitrate_scales_with_pixel_rate` | Quality presets scale with resolution and frame rate, within limits. |
| `config::resolution_scales_down_only_and_keeps_aspect` | Output scaling never upscales and keeps even dimensions and aspect ratio. |
| `config::missing_fields_fall_back_to_defaults` | An old or partial `settings.json` still loads. |
| `config::settings_saved_with_a_utf8_bom_still_load` | A `settings.json` saved with a UTF-8 BOM (e.g. by Windows PowerShell) still loads. |
| `app::mute_silences_regardless_of_volume` | Mute wins over any volume. |
| `app::volume_uses_a_squared_curve` | The volume slider maps to gain as `volume²`. |
| `app::exit_waits_for_the_recording_being_saved` | Closing the app while a recording is being saved waits for the MP4 to be finalized. |
| `i18n::every_spanish_variant_is_spanish` | The UI is Spanish for every Spanish Windows locale and English otherwise. |
| `hotkeys::text_round_trips_to_a_registrable_hotkey` | "Ctrl+Alt+R" in the settings becomes the right registered hotkey and back. |
| `hotkeys::keys_that_would_break_typing_are_rejected` | A hotkey needs Ctrl or Alt unless it is an F-key. |
| `hotkeys::a_modifier_alone_is_not_a_hotkey` | Pressing Ctrl or Alt alone does not end hotkey capture. |
| `hotkeys::invalid_text_is_rejected` | Malformed hotkeys in the settings are ignored. |
| `capture::bgra_rows_with_padding_become_packed_rgba` | Screenshot read-back handles padded GPU rows. |
| `capture::png_round_trips` | Screenshots are valid PNG with the right pixels. |
| `capture::crop_keeps_only_the_region` | Screenshots are cropped to the region. |
| `config::region_is_clipped_to_the_source_with_even_sides` | A region survives the source changing size and keeps even sides for H.264. |
| `app::a_selection_on_the_preview_maps_to_source_pixels` | A rectangle dragged on the preview becomes the right region, and a click selects nothing. |
| `audio::denoiser_keeps_the_stream_length_in_whole_frames` | Noise suppression neither loses nor invents samples. |
| `audio::denoiser_keeps_silence_silent` | Noise suppression adds nothing to silence. |
| `camera::overlay_sits_in_the_corner_with_the_camera_aspect` | The webcam overlay's position and size. |
| `camera::resize_keeps_flat_colour_and_identity` | The webcam scaling. |
| `tray::idle_icon_is_grey_with_the_same_alpha` | The idle tray icon. |
| `app::saved_message_survives_a_source_that_is_gone` | When the recorded window is closed, the "saved" message and file link stay, and the source is deselected. |

## Manual

Capture and encoding need a GPU and an interactive desktop session, which CI runners do not
provide, so the pipeline is tested by hand before each release. Build with
`cargo build --release`, then:

1. **Self-test**: `obs-lite.exe --selftest 6 a.mp4` and `obs-lite.exe --selftest 6 b.mp4 pause 720`.
   Move something on screen and play audio while they run. Expect 0 dropped frames, 60 fps and,
   for `b.mp4`, about 4 s of 1280×720 video.
2. **Interface flow**: start, stop; the status bar shows the saved path; click it to open the folder.
3. **Pause**: record, pause about 2 s with the button, resume, stop; the pause must not be in the file.
4. **Hotkeys**: the same with `Ctrl+Alt+R` and `Ctrl+Alt+P` while another application has focus.
5. **Window capture**: record a window, resize it smaller and larger, then close it; the recording
   must stop by itself and the file must play.
6. **Audio devices**: in *Ajustes › Audio*, pick each desktop and microphone device in turn; the
   meters must move and no error must appear.
7. **Exit while recording**: close the window during a recording; the file must play.
8. **Settings**: change values and press *Cancelar* (nothing changes), then *Aceptar* (they apply
   and survive a restart).
9. **Language**: switch between Español and English in *General*; the whole UI and the source names
   change at once.
10. **Hide from capture**: with the option on, a screenshot of the desktop must not show the window.
11. **Screenshot**: from the preview's context menu, the File menu and a hotkey; check the PNG.
12. **Hotkeys**: assign a combination to each action, press them with another application focused.
13. **Region**: select a region, record a few seconds and take a screenshot; both must be the
    region's size and content.
14. **Noise suppression**: compare the microphone's noise floor in a quiet room with it off and on.
15. **Tray**: minimize to tray, record with the hotkeys, start/stop from the tray menu, restore with
    a left click.
16. **Webcam**: add it, record a static screen for a few seconds (the camera must keep moving), try
    each corner and size, take a screenshot, remove it (the camera light must go off).

Check each MP4's duration, resolution, frame rate and audio track in its *Properties › Details*
tab or with `ffprobe`.

### Last results

2026-10-16, Windows 11, RTX 4060 Laptop GPU, Rust 1.99. Steps 1–4 and 6 on commit `96413f0`; steps 5, 7
and 8 on commit `b6cdad1` plus the settings BOM fix:

| Step | Result |
|---|---|
| 1. Self-test, 6 s and 15 s at 1080p60, screen in motion | 60 fps exactly, 0 dropped frames, AAC stereo 48 kHz |
| 1. Self-test with pause, 720p | 3.65 s, 1280×720, 60 fps |
| 2. Interface flow | Valid file; status bar shows the saved path |
| 4. Hotkeys: record, pause 2 s, resume, stop | 5.63 s file (pause excluded) |
| 5. Window shrunk, grown, then closed | Recording stopped on close; valid file (7.03 s, 626×392, 30 fps); status bar kept *Grabación guardada en …*; source deselected |
| 6. Every desktop and microphone device (2 + 2) | No errors; signal on the active devices |
| 7. Exit while recording, and right after *Detener* (while saving) | File finalized in both cases (3.67 s, 1080p30); app exits in 0.2 s |
| 8. Settings *Aceptar* / *Cancelar* | *Cancelar* kept 30 fps after picking 60; *Aceptar* wrote `fps: 30` to `settings.json` at once and the next recording was 30 fps |
| Release workflow artifact (`b6cdad1`), `--selftest 3` | Hashes match the run log; 2.73 s, 1080p60, 0 dropped frames |
| Resource use | Idle ~85 MB private / 0.3 % CPU; recording 1080p60 ~180 MB / 1.1 % CPU |

2026-10-16, Windows 11, RTX 4060 Laptop GPU, Rust 1.99, `features` branch (steps 9–16):

| Step | Result |
|---|---|
| 9. Language | English and Spanish switch live; source label "Pantalla 1" / "Display 1" follows |
| 10. Hide from capture | A GDI screenshot of the desktop shows what is behind the window |
| 11. Screenshot | Context menu and `Ctrl+Alt+S`: 1920×1080 PNG with correct colours |
| 12. Hotkeys | `Ctrl+Alt+S` and `Ctrl+Alt+M` assigned by pressing them; both worked; a stray "Ctrl+ControlLeft" capture was found and fixed (test added) |
| 13. Region | 840×504 region: recording 840×504 at 60 fps, screenshot 840×504 with the selected content |
| 14. Noise suppression | Built-in microphone, quiet room: noise floor about −76 dBFS off, −105 dBFS on |
| 15. Tray | Taskbar button removed when minimized; hotkey recording while minimized (3.82 s); tray menu start/stop (5.44 s); left click restores |
| 16. Webcam | Camera capture checked alone: 31 frames in 3 s at 1280×720, no error. **Not yet checked in the running app** (the session was locked) |

Not tested at all: Intel and AMD encoders, Windows 10, multiple monitors, HDR displays.
