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
| `app::mute_silences_regardless_of_volume` | Mute wins over any volume. |
| `app::volume_uses_a_squared_curve` | The volume slider maps to gain as `volume²`. |

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

Check each MP4's duration, resolution, frame rate and audio track in its *Properties › Details*
tab or with `ffprobe`.

### Last results

2026-10-16, commit `96413f0`, Windows 11, RTX 4060 Laptop GPU, Rust 1.99:

| Step | Result |
|---|---|
| 1. Self-test, 6 s and 15 s at 1080p60, screen in motion | 60 fps exactly, 0 dropped frames, AAC stereo 48 kHz |
| 1. Self-test with pause, 720p | 3.65 s, 1280×720, 60 fps |
| 2. Interface flow | Valid file; status bar shows the saved path |
| 4. Hotkeys: record, pause 2 s, resume, stop | 5.63 s file (pause excluded) |
| 5. Window shrunk, grown, then closed | Recording stopped on close; valid 6.52 s file, 0 dropped frames |
| 6. Every desktop and microphone device (2 + 2) | No errors; signal on the active devices |
| 7. Exit while recording | File finalized; app exits in 0.2 s |
| 8. Settings *Aceptar* / *Cancelar* | **Not tested** (dialog checked visually only) |
| Resource use | Idle ~85 MB private / 0.3 % CPU; recording 1080p60 ~180 MB / 1.1 % CPU |

Not tested at all: Intel and AMD encoders, Windows 10, multiple monitors, HDR displays.
