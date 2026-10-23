# Testing

## Automated

Run on every pull request by CI (Windows and Ubuntu, Rust 1.95 and stable), together with clippy,
a release build and `cargo deny`. `mf::software_encoder_encodes_the_canvas` is Windows-only. CI also builds the MSI, validates it (ICE) and runs `installer/test.ps1`,
which installs it for the runner's user and checks the files, the *Obs Lite* Start menu shortcut,
that there is no desktop shortcut by default, the *Installed apps* entry and its icon, a clean
uninstall, then an install with the desktop shortcut and an upgrade that keeps it:

```powershell
cargo test --locked
```

| Test | Checks |
|---|---|
| `media::clock_excludes_paused_time` | The recording clock stops while paused. |
| `media::replay_keeps_the_last_seconds_from_a_keyframe` | The replay buffer keeps at least its length, from a keyframe, at most one keyframe interval more, and audio from the first frame on. |
| `media::replay_shorter_than_its_length_keeps_everything` | A replay buffer that has run less than its length keeps every packet. |
| `media::replay_starts_at_the_first_keyframe` | Packets before the first keyframe are not kept (they would not play). |
| `media::a_recording_starts_at_the_next_keyframe_with_the_audio_since_then` | A recording started while encoding asks for a keyframe once, starts on it and keeps only the audio from then on. |
| `media::a_recording_without_frames_is_an_error` | Stopping a recording before any frame reports an error instead of an empty file. |
| `media::a_saved_replay_is_interleaved_by_time` | A saved replay writes video and audio in time order, relative to its first keyframe, from another thread. |
| `mf::software_encoder_encodes_the_canvas` | Without a hardware encoder, the canvas still goes through the GPU colour conversion and the Windows software H.264 encoder (one packet per frame, keyframe first). Skipped where there is no GPU video processor, such as CI. |
| `audio::mixes_with_gain_and_pads_silence` | Inputs are mixed with their gain; a short input is padded with silence. |
| `audio::inputs_adding_up_past_full_scale_are_limited_not_clipped` | Two inputs that add up to 1.4 are brought within full scale with one steady gain, so the wave keeps its shape (a 50 Hz tone: the gain must not wobble from cycle to cycle). |
| `audio::a_mix_within_full_scale_is_untouched` | The limiter leaves a mix that fits exactly as it is. |
| `audio::drops_oldest_samples_beyond_latency_bound` | A backed-up input drops its oldest samples instead of drifting. |
| `config::bitrate_scales_with_pixel_rate` | Quality presets scale with resolution and frame rate, within limits. |
| `config::resolution_scales_down_only_and_keeps_aspect` | Output scaling never upscales and keeps even dimensions and aspect ratio. |
| `config::missing_fields_fall_back_to_defaults` | An old or partial `settings.json` still loads. |
| `config::settings_saved_with_a_utf8_bom_still_load` | A `settings.json` saved with a UTF-8 BOM (e.g. by Windows PowerShell) still loads. |
| `app::mute_silences_regardless_of_volume` | Mute wins over any volume. |
| `app::volume_uses_a_squared_curve` | The volume slider maps to gain as `volume²`. |
| `app::no_preview_pictures_for_a_window_that_is_not_shown` | No preview pictures are made while the window is minimized (Windows says so) or has not been drawn for 3 s (Wayland only stops asking for frames); a shown idle window draws once a second, so it never counts as hidden. |
| `app::a_silent_idle_window_repaints_rarely` | The window draws once a second when idle and silent, and at 15 fps while a meter moves or a recording runs. |
| `app::exit_waits_for_the_recording_being_saved` | Closing the app while a recording is being saved waits for the MP4 to be finalized. |
| `i18n::every_spanish_variant_is_spanish` | The UI is Spanish for every Spanish Windows locale and English otherwise. |
| `i18n::every_spanish_locale_is_spanish` | The same for POSIX locales (`es_ES.UTF-8`, `es`…), as read on Linux. |
| `linux::encoder::a_region_is_copied_and_what_is_missing_is_black` | Linux only: a region (or a shrunk window) is copied into the canvas, padded with black. |
| `linux::encoder::the_webcam_is_pasted_in_place` | Linux only: the webcam picture lands in its rectangle. |
| `linux::encoder::packet_times_discount_the_encoders_offset` | Linux only: packet times ignore the 1000-hour offset GStreamer video encoders add, so audio is not dropped from files. |
| `linux::encoder::aac_codec_data_describes_lc_48k_stereo` | Linux only: the AAC stream description given to the muxer before any audio is AAC-LC, 48 kHz, stereo. |
| `hotkeys::a_pressed_combination_becomes_text` | A combination pressed in the settings becomes "Ctrl+Alt+R". |
| `hotkeys::keys_that_would_break_typing_are_rejected` | A hotkey needs Ctrl or Alt unless it is an F-key. |
| `hotkeys::a_modifier_alone_is_not_a_hotkey` | Pressing Ctrl or Alt alone does not end hotkey capture. |
| `win::hotkeys::text_round_trips_to_a_registrable_hotkey` | Windows only: "Ctrl+Alt+R" in the settings becomes the right registered hotkey. |
| `win::hotkeys::invalid_text_is_rejected` | Windows only: malformed hotkeys in the settings are ignored. |
| `linux::hotkeys::hotkeys_become_portal_triggers` | Linux only: "Ctrl+Alt+R" becomes the shortcut text the desktop expects ("CTRL+ALT+r"); unknown modifiers are rejected. |
| `capture::bgra_rows_with_padding_become_packed_rgba` | Screenshot read-back handles padded GPU rows. |
| `capture::png_round_trips` | Screenshots are valid PNG with the right pixels. |
| `capture::crop_keeps_only_the_region` | Screenshots are cropped to the region. |
| `capture::preview_downscale_averages_every_pixel` | The preview averages every source pixel when scaling down, and is an exact copy at full size. |
| `capture::preview_downscale_matches_the_straightforward_average` | The faster preview scaling gives exactly the same pixels as adding each source pixel one by one, at odd sizes and with padded rows. |
| `config::region_is_clipped_to_the_source_with_even_sides` | A region survives the source changing size and keeps even sides for H.264. |
| `app::a_selection_on_the_preview_maps_to_source_pixels` | A rectangle dragged on the preview becomes the right region, and a click selects nothing. |
| `audio::denoiser_keeps_the_stream_length_in_whole_frames` | Noise suppression neither loses nor invents samples. |
| `audio::denoiser_keeps_silence_silent` | Noise suppression adds nothing to silence. |
| `audio::only_sound_after_silence_wakes_the_ui` | An input wakes the window only when it goes from silence (below −60 dB) to sound. |
| `camera::overlay_sits_in_the_corner_with_the_camera_aspect` | The webcam overlay's position and size. |
| `camera::resize_keeps_flat_colour_and_identity` | The webcam scaling. |
| `linux::camera::denied_access_says_how_to_allow_it` | Linux only: when the desktop gives no camera access, the error says to choose the camera again or allow it in the privacy settings, instead of "Portal request was cancelled". |
| `linux::camera::row_padding_is_dropped` | Linux only: camera pictures with padded rows are packed, and short buffers are rejected. |
| `linux::sys::document_portal_paths_are_recognised` | Linux only: a folder granted through the folder picker (`/run/user/UID/doc/ID/NAME/…`) is recognised, so its real path can be shown; other paths are left alone. |
| `linux::sys::a_granted_folder_shows_without_a_trailing_slash` | Linux only: that real path is shown as `/home/…/Descargas`, not `/home/…/Descargas/`. |
| `tray::idle_icon_is_grey_with_the_same_alpha` | The idle tray icon. |
| `update::versions_compare_numerically` | Only a strictly newer plain version counts as an update. |
| `update::release_is_read_from_the_github_response` | The tag and page are read from GitHub's response; an error response gives nothing. |
| `app::saved_message_survives_a_source_that_is_gone` | When the recorded window is closed, the "saved" message and file link stay, and the source is deselected. |

## Results

Capture and encoding need a GPU and a desktop session, which CI runners do not have, so they were
checked on real machines. What was checked, and what was not:

2026-10-16, Windows 11, RTX 4060 Laptop GPU, Rust 1.99, on commits `96413f0` and `b6cdad1` (with the
settings BOM fix):

| Step | Result |
|---|---|
| Self-test, 6 s and 15 s at 1080p60, screen in motion | 60 fps exactly, 0 dropped frames, AAC stereo 48 kHz |
| Self-test with pause, 720p | 3.65 s, 1280×720, 60 fps |
| Interface flow | Valid file; status bar shows the saved path |
| Hotkeys: record, pause 2 s, resume, stop | 5.63 s file (pause excluded) |
| Window shrunk, grown, then closed | Recording stopped on close; valid file (7.03 s, 626×392, 30 fps); status bar kept *Grabación guardada en …*; source deselected |
| Every desktop and microphone device (2 + 2) | No errors; signal on the active devices |
| Exit while recording, and right after *Detener* (while saving) | File finalized in both cases (3.67 s, 1080p30); app exits in 0.2 s |
| Settings *Aceptar* / *Cancelar* | *Cancelar* kept 30 fps after picking 60; *Aceptar* wrote `fps: 30` to `settings.json` at once and the next recording was 30 fps |
| Release workflow artifact (`b6cdad1`), `--selftest 3` | Hashes match the run log; 2.73 s, 1080p60, 0 dropped frames |
| Resource use | Idle ~85 MB private / 0.3 % CPU; recording 1080p60 ~180 MB / 1.1 % CPU |

2026-10-16, Windows 11, RTX 4060 Laptop GPU, Rust 1.99, `features` branch:

| Step | Result |
|---|---|
| Language | English and Spanish switch live; source label "Pantalla 1" / "Display 1" follows |
| Hide from capture | A GDI screenshot of the desktop shows what is behind the window |
| Screenshot | Context menu and `Ctrl+Alt+S`: 1920×1080 PNG with correct colours |
| Hotkeys | `Ctrl+Alt+S` and `Ctrl+Alt+M` assigned by pressing them; both worked; a stray "Ctrl+ControlLeft" capture was found and fixed (test added) |
| Region | 840×504 region: recording 840×504 at 60 fps, screenshot 840×504 with the selected content |
| Noise suppression | Built-in microphone, quiet room: noise floor about −76 dBFS off, −105 dBFS on |
| Tray | Taskbar button removed when minimized; hotkey recording while minimized (3.82 s); tray menu start/stop (5.44 s); left click restores |
| Webcam | Added from *+ › Video Capture Device*; overlay in the chosen corner of preview and recording. Static window recorded with a 400×300 region and the webcam: 400×300, 60 fps, the camera corner changes every frame while the rest does not |
| Black frames | Before the fix, counted with ffmpeg: 1 per 15 s on the screen without webcam (also on `main`), 5 per 15 s with it, up to 27 per 10 s in a debug build. After: 0 in 5 webcam recordings and 5 self-tests (6–15 s each), 0 lagged frames |
| Preview sharpness | Preview drawn at about 1170 px wide on a 1920×1080 display: text in it is legible (it was 640 px, one pixel per block). Idle CPU 4.2 → 7.5 % of one core (0.3 → 0.5 % of 16 threads), private memory 95 → 98 MB (105 MB maximized) |
| Update notice | A build labelled 0.2.0 queried the real GitHub API at start-up and showed "Nueva versión 0.2.1 disponible" in the status bar |
| Application audio | A PowerShell window looping a 440 Hz tone: captured on its own at −14 dBFS while VSCodium, captured at the same time, gave silence; recording with desktop audio off had the tone (mean −14.1 dB, ffmpeg volumedetect); mixer channel and source row shown |

2026-10-18, Windows 11, laptop with Intel UHD and RTX 4060 GPUs, new encoding engine (capture
and so encoding on the Intel GPU, *Intel Quick Sync Video H.264 Encoder MFT*):

| Step | Result |
|---|---|
| Self-test 6 s 1080p60 | 350 frames, 60 fps, H.264 High, AAC-LC 48 kHz stereo, keyframes at 0/2/4 s, BT.709 signalled, 0 black, 0 decode errors |
| Self-test with pause, 720p | 1280×720, 3.88 s, 233 frames, 0 black |
| Window grown and shrunk while recording | Cropped / padded with black as documented; 0 black frames, 0 decode errors |
| Region | Region past the right edge clipped to 520×360; 0 black frames |
| Black frames, 15 s with webcam | 890 frames, 0 black |
| Application audio with a region | Tone at −14.1 dB mean, desktop audio off |
| Audio/video sync, 10 min | 35 993 frames, 0 lagged; offset 0.211 s at the start and 0.207 s at the end (the same 0.2 s as 0.2.1, so it comes from the test signal); video 599.88 s, audio 599.87 s; private memory 225 → 229 MB |
| Software encoder | 1080p60: 359 frames, 0 lagged, about one CPU core; 720p with pause: 239 frames; correct colours |
| Resource use | Idle 98 MB / 0.5 % CPU; recording 1080p60 270 MB / 1.5 % CPU (0.2.1: ~350 MB / 1.3 %) |
| Replay buffer | Self-test with `replay`: 3 s asked, 4.83 s saved (from the keyframe before), audio −14.1 dB. In the app: two replays of 20.87 s and 20.90 s, first frame a keyframe, 0 decode errors; a recording started while it ran lasted 6.00 s for 6 s between hotkeys (forced keyframe), and another, paused 2 s, lasted 5.03 s with a 440 Hz tone at −14.1 dB; private memory 280 MB with both running. A bug where stopping a recording without the replay buffer left the MP4 unfinished was found here and fixed before commit. Not tested: the new tray menu items |

2026-10-18, installer (WiX 6.0.2), same machine:

| Step | Result |
|---|---|
| `installer/test.ps1` | All 23 checks passed (install, uninstall, desktop shortcut, upgrade 0.2.1 → 0.2.2 keeping it, clean uninstall) |
| Installer, interactive | Welcome, *Shortcuts* (box unticked), *Ready*, *Completed*; no UAC prompt; installed to `%LOCALAPPDATA%\Programs\obs-lite`; MSI 3.1 MB. Started from the Start menu shortcut, whose icon is the obs-lite one |
| `wix msi validate` | Passes; warnings ICE69 (shortcut and executable in different components of the same feature) and ICE91 (per-user folder), expected for a per-user package |

2026-10-18, release preparation for 0.3.0 (`main` plus the version bump), same machine:

| Step | Result |
|---|---|
| Automated tests and clippy | 33 passed; clippy clean |
| Self-test 6 s 1080p60, 440 Hz tone playing | 352 frames, 0 lagged, 5.87 s, audio −14.0 dB |
| Self-test with pause, 720p and `replay` | 1280×720, 3.87 s, 0 lagged, audio −14.1 dB; replay 3.85 s with audio |
| `installer/test.ps1` on the 0.3.0 MSI | All checks passed |

2026-10-22, Linux: Ubuntu 26.04.1 LTS (GNOME on Wayland, PipeWire, GStreamer 1.28.2) in a Hyper-V
VM without a GPU, so x264 in software; desktop audio through the VM's dummy output's monitor:

| Step | Result |
|---|---|
| Portal | The sharing dialog appears once ("Remember this decision" ticked); later sessions start without it (restore token), in under 0.1 s |
| Self-test 8 s with `replay`, test pattern moving and a 440 Hz tone | 1024×768, 60 fps, 479 frames, 0 lagged; H.264 High (x264) + AAC-LC; audio −33.2 dB; 0 decode errors; first frame a keyframe; replay 3.92 s with audio. About 3 s of CPU for 8 s of recording; 117 MB peak memory |
| Interface flow | The app starts with the preview of the shared screen, the desktop's sharing indicator and the tray icon (red while recording); *Start/Stop Recording* saved a 7.6 s file (456 frames at 60 fps, AAC) to `~/Vídeos` |
| Bug found and fixed | Audio was missing from files: video packet times carried GStreamer encoders' 1000-hour offset, so all audio fell before the file's start. Times are now running times (test added; it fails without the fix) |

Later the same day, hotkeys and webcam, same VM; a desktop entry for `io.github._25Dark25.ObsLite`
installed in `~/.local/share/applications`, as the Flatpak will have:

| Step | Result |
|---|---|
| Automated tests and clippy | 41 passed; clippy clean on 1.95 and stable (Linux and Windows); 38 passed on Windows |
| Hotkeys | GNOME's *Add Keyboard Shortcuts* dialog listed the proposed combinations; `Ctrl+Alt+R` started a recording with another window focused, `Ctrl+Alt+P` paused and resumed it, `Ctrl+Alt+R` stopped it: 24.65 s file (1479 frames, AAC, 0 decode errors), pauses excluded. Without the desktop entry the portal refuses ("An app id is required") and the status bar says so |
| Webcam | A virtual camera (`v4l2loopback` fed with a GStreamer test pattern). Before access is granted, *Video Capture Device* shows one "Camera" entry and nothing is asked; choosing it brings GNOME's *Allow Obs Lite to Use the Camera?* dialog; then the overlay shows in the preview and in an 8.45 s recording (507 frames, AAC) and the desktop's camera indicator is on. After a restart the camera comes back without asking and the menu lists it by name; choosing it by name works. Without the desktop entry GNOME asks generically ("Allow Apps…") and the camera works too |
| Bugs found and fixed | GNOME refused the camera dialog ("Only the focused app is allowed to show a system access dialog") because the window did not carry the app id; it does now. The camera list showed each camera twice; it is deduplicated |

Not verified on Linux: VA-API and NVENC encoders (no GPU in the VM), a real microphone (the VM has
none), a real webcam, removing the webcam (the camera indicator should go off), denying camera
access, X11 sessions, desktops other than GNOME.

Mix limiter, same VM: a 440 Hz tone at 0.7 on the desktop (a null sink) and a 660 Hz tone at 0.7 on
a virtual microphone (a second null sink's monitor), so the mix reaches 1.4. Distortion is the
energy away from the two tones in seconds 1–5 of the file's audio:

| Step | Result |
|---|---|
| Automated tests and clippy | 43 passed on Linux, 40 on Windows; clippy clean on 1.95 and stable (both) |
| Before (`main`) | Both meters at −3.1 dB and no notice; distortion −15.3 dB, clipping harmonics at 220, 860, 1540 and 1740 Hz |
| After | *Mix at its limit* shown while recording; distortion −28.3 dB, no clipping harmonics: what is left sits next to the tones, the gain following peaks that change as the two captures drift in phase |
| Reference: each tone at 0.35 | No notice; distortion −34.6 dB (the floor of capture and AAC) |

2026-10-23, Flatpak, same VM: runtime `org.freedesktop.Platform` 26.08 (GStreamer 1.28.6, x264 from
its codecs-extra extension), built with `flatpak-builder` 1.4.8 and installed from the bundle:

| Step | Result |
|---|---|
| Build | 3 min; the app is 11.6 MB installed, the bundle 3.6 MB; `desktop-file-validate` and `appstreamcli validate` pass; permissions as listed in the manifest |
| Install and start | *Obs Lite* in the dock with its icon after the session reloads Flatpak's exports (only needed once, after installing Flatpak itself) |
| Sharing dialog | Shown the first time; the next starts restored the source without it |
| Recording | 8.7 s, 1024×768 H.264 High at 60 fps + AAC-LC, 0 decode errors, in `~/Vídeos` |
| Hotkeys | `Ctrl+Alt+R` started and stopped a recording with another window focused |
| Webcam | Overlay in the preview (camera access had already been granted to this app id, so no dialog) |
| Tray | Icon grey when idle and red while recording; its menu shows all actions |
| Browse | The folder picker (portal) opened; *Descargas* chosen; the next recording (4.6 s) landed in `~/Descargas`. The document portal maps the granted path to `/home/dark/Descargas` (checked from inside the sandbox) |
| Idle, preview and desktop audio | Native build: 24 % of one core, 38 MB anonymous memory. Flatpak: 33 %, 43 MB anonymous plus 84 MB of runtime libraries (file-backed, shared with other apps on the same runtime). Most of the CPU is Mesa drawing the window in software (no GPU in the VM) |
| Recording 1024×768 at 60 fps | Flatpak: 58 % of one core (x264 in software and the window), 186 MB PSS |
| Bugs found and fixed | The tray icon failed in the sandbox: `tray-icon` 0.26.0 asked for a D-Bus name Flatpak does not allow; 0.26.1 does not in a sandbox. *File › Show Recordings* and clicking the "saved" message did nothing on Linux (they ran Windows' `explorer`); they now go through the desktop's OpenURI portal. A folder chosen with *Browse* showed as `/run/user/1000/doc/…`; the setting and messages now show its real path |

Later, through the interface of the installed Flatpak, with *Descargas* chosen earlier:

| Step | Result |
|---|---|
| "Saved" message | Shows `/home/dark/Descargas/2026-10-23 16-35-32.mp4`, the real path |
| Clicking it | Files opens *Descargas* with the new recording selected |
| *File › Show Recordings* | Files opens *Descargas* |
| Recording Path setting | Shows the real path and cannot be typed into; *Browse* still changes it |
| Bug found and fixed | The setting showed `/home/dark/Descargas/`, with a trailing slash (test added; it fails without the fix) |

Not verified with the Flatpak: the camera permission dialog for a fresh install; a machine with a
GPU.

2026-10-23, idle CPU, with the preview of the screen and desktop audio on and nothing playing,
30 s per run after 12 s of start-up, `main` against the change that draws a silent, idle window
once a second and scales the preview faster:

| Machine | Before | After |
|---|---|---|
| Windows 11 laptop (the one above), 1080p display | 7.4 % of one core (two runs) | 3.9–4.3 % |
| Ubuntu VM without GPU (window drawn in software) | 37 % | 5.6 % |

Private memory did not change in the VM (55 MB) and rose 3–4 MB on Windows (99 to 102 MB). Most of
what is left is the preview: each picture is read back from the GPU (about 2.6 ms at 1080p) and
scaled down (5.4 ms before, 3.0 ms after, measured in a release build). A meter still reacts at
once when sound starts (checked with a tone after 10 s of silence) and falls back to −inf after.

2026-10-23, release preparation for 0.4.0 (`main` plus the version bump), Windows on the same
laptop, Linux in the same VM with the 0.4.0 Flatpak built from the release branch and installed from
its bundle:

| Step | Result |
|---|---|
| Automated tests and clippy | 43 passed on Windows, 49 on Linux (one timing test ignored on both); clippy clean on 1.95 and stable on both |
| Self-test 6 s 1080p60, 440 Hz tone playing (Windows) | Quick Sync; 352 frames, 0 lagged, 5.87 s, audio −14.1 dB, 0 decode errors |
| Self-test with pause, 720p and `replay` (Windows) | 1280×720, 3.88 s, 233 frames, 0 lagged; replay 3.85 s; audio −14.1 dB |
| Interface flow (Windows) | 5.48 s at 1080p60 with the tone at −14.1 dB; clicking the "saved" message opened Explorer with the file selected; *File › Show Recordings* opened *Videos* |
| Hotkeys (Windows) | `Ctrl+Alt+R`, `Ctrl+Alt+P` twice and `Ctrl+Alt+R` with VSCodium focused, 2 s apart: 4.67 s file, pause excluded |
| Replay buffer (Windows) | Started with the button; meter at −11 dB with the tone; saved replay 17.6 s, first frame a keyframe, audio −14.0 dB, 0 decode errors |
| Installer | Not run on the test machine: its installed 0.3.0 makes `installer/test.ps1` stop, and a local 0.4.0 MSI would stand in the way of the released one. The CI `installer` job (install, upgrade, uninstall) passed on `main` |
| Release workflow, run by hand on `main` (`c09b40a`) | `windows`, `flatpak` and `checksums` jobs passed; `sha256sum --check SHA256SUMS` passes for the zip, the MSI and the bundle; the CI-built bundle installs in the VM and has `LICENSE` and the Linux `THIRD-PARTY-LICENSES.html` (GStreamer included) |
| Flatpak version | `flatpak info` shows 0.4.0; `appstreamcli validate` passes |
| Self-test inside the Flatpak, 8 s with `replay`, tone playing | x264; 1280×720 at 60 fps, 479 frames, 0 lagged, audio −13.5 dB; replay 3.90 s; 0 decode errors |
| After `flatpak permission-reset` | The sharing dialog names the app ("Obs Lite quiere compartir su pantalla"). The camera, configured but no longer allowed, asked while that dialog had the focus and got "cancelled"; choosing it again showed *Camera* in the menu and *Allow Obs Lite to Use the Camera?*; *Deny* showed an error, and allowing it afterwards (as the privacy settings do) brought the overlay back |
| Bug found and fixed | That error read "Portal request was cancelled"; it now says to choose the camera again or allow it in the privacy settings (#26, test added) |
| Recording to the folder chosen earlier with *Browse* | 4.67 s, 1280×720, in `~/Descargas`, 0 decode errors |

Not repeated for 0.4.0: the Windows checks whose code did not change since 0.3.0 (the preview
scaling did change, but gives the same pixels and has a test); the mix limit on Windows (it needs
two sound inputs; checked in the VM); the tray menu on Windows (unchanged); the Flatpak on a machine
with a GPU.

2026-10-23, after 0.4.0's checks: dependency updates and no preview pictures while the window is not
shown. Idle CPU with the screen preview and desktop audio on, `main` against the change:

| Check | Result |
|---|---|
| Hotkeys with `global-hotkey` 0.9 (Windows) | `Ctrl+Alt+R`, `Ctrl+Alt+P` twice and `Ctrl+Alt+R` with VSCodium focused: 4.60 s file, pause excluded |
| Windows, minimized | 1.9–2.6 % of one core before, 1.1–1.6 % after (six runs) |
| Windows, shown | The same before and after (the screen was busy, so about 21 % in both) |
| Linux VM (Flatpak), minimized with Super+H | The preview stopped 3.3 s after minimizing and came back 0.4 s after the window was shown again. CPU 4.0 % minimized against 4.1 % before the change: on Linux the preview pictures cost little, and the window stops being drawn either way |
| Linux, frames reaching the capture | About 2 a second while idle, as GNOME sends a new one only when the screen changes; asking PipeWire for at most 5 a second gave 4.7 and more CPU, so it was not done |

Not tested at all: NVIDIA (with this engine) and AMD encoders, Windows 10, multiple monitors,
HDR displays.
