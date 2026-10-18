# obs-lite

Lightweight screen recorder for Windows: a single ~7 MB `.exe`, with no bundled runtime or browser.
The interface follows OBS Studio, so OBS users find everything where they expect it.

| | Idle (preview + audio) | Recording 1080p60 + audio |
|---|---|---|
| Private memory | ~100 MB | ~270 MB |
| CPU (16 threads) | ~0.5 % | ~1.5 % |

Measured on Windows 11 on a laptop with an RTX 4060 GPU, encoding on its Intel integrated GPU
(Quick Sync), without a webcam; your numbers will vary. About 40 MB of the recording figure is
the queue of converted frames that keeps the encoder from dropping frames when it briefly stalls.

> **Status: early (0.2.1).** obs-lite has been tested on a single machine
> (Windows 11, laptop with Intel and NVIDIA GPUs); see [docs/TESTING.md](docs/TESTING.md) for what was and was not
> verified. It has not had any security audit or independent code review. Most of the code was
> written with an AI coding assistant (Claude Code) and then built and tested as described there.
> It is not affiliated with the OBS Project and contains no OBS code.

## Requirements

- Windows 11. Windows 10 has not been tested.
- A GPU with a working driver (NVIDIA, Intel or AMD). Its hardware H.264 encoder is used; without
  one, the Windows software encoder takes over (more CPU). Tested: Intel Quick Sync, and the
  software encoder.

## Quick start

1. Download from [Releases](https://github.com/25Dark25/obs-lite/releases) and
   [verify](#verifying-a-release) either:
   - `obs-lite-X.Y.Z-windows-x64.msi`, the installer: it needs no administrator rights, installs
     to `%LOCALAPPDATA%\Programs\obs-lite`, adds *Obs Lite* to the Start menu and, if you tick the
     box, to the desktop. A newer `.msi` upgrades it in place; uninstall it from *Settings › Apps*.
   - `obs-lite-X.Y.Z-windows-x64.zip`, the portable version: unzip it anywhere and run
     `obs-lite.exe`. There is nothing to install.
2. Press *Iniciar Grabación* (or `Ctrl+Alt+R`). By default it records the first display Windows
   reports and desktop audio at 60 fps to your *Videos* folder.

## Interface

Same layout as OBS Studio and its dark *Yami* palette, with OBS's wording in Spanish or English
(it follows the Windows display language; change it in Settings › General):

- **Preview** on top and, below it, the **Sources**, **Audio Mixer** and **Controls** docks.
  Right-click the preview to take a screenshot.
- **Sources › +**: *Display Capture*, *Window Capture*, *Application Audio Capture* (the sound of
  one application, with its own mixer channel) or *Video Capture Device* (a webcam shown in a
  corner of the recording). **Properties**: capture the cursor, *Select Region…* to record only part
  of the source, and the webcam's corner and size.
- **Audio Mixer**: desktop audio and *Mic/Aux*, each with a meter, volume slider and mute button
  (mute also works while recording).
- **Controls**: *Start Recording* (with a pause button next to it while recording), *Start Replay
  Buffer* (with a *Save Replay* button next to it while it runs), *Settings* and *Exit*.
- **Settings**: *General* (language, minimize to tray, hide obs-lite from capture), *Output*
  (recording path and quality, maximum replay time), *Audio* (devices, microphone noise suppression), *Video* (scaled
  output resolution and FPS) and *Hotkeys* (click a field and press the combination). *OK* applies,
  *Cancel* discards.
- **Status bar**: messages, CPU usage, dropped frames and the recording indicator with elapsed time.
- **File**: *Take Screenshot* and *Show Recordings*.
- **System tray icon**: grey when idle, in colour while recording or while the replay buffer runs.
  Its menu starts/stops and pauses recording, starts/stops the replay buffer, saves a replay, takes
  a screenshot, shows the window or exits; a left click shows the window.

## Features

- **Display or window capture** through Windows Graphics Capture (the API OBS uses), with or
  without the cursor.
- **Desktop audio + microphone** mixed together, with volume, mute, meters and device selection.
- **Application audio**: the sound of a single application (and its child processes), e.g. a game
  without Discord, as its own mixer channel. Needs Windows 10 version 2004 or later.
- **H.264 + AAC** to MP4, which plays everywhere: the GPU's hardware encoder (NVENC / Quick Sync /
  AMF through Media Foundation), or the Windows software encoder if the GPU has none. A keyframe
  every 2 s; BT.709 colours, signalled in the file.
- **Constant frame rate** (30 or 60 fps), suitable for video editors. When the screen does not
  change the last frame is repeated; when the machine falls behind, slots are skipped and reported
  as dropped frames.
- **Pause and resume** without gaps: paused time never reaches the file, in video or audio.
- **Replay buffer**, like OBS's: keeps the last 20 s (5–600, in Settings › Output) in memory, already
  encoded, and *Save Replay* writes them to a new MP4 (`Replay 2026-10-18 17-43-13.mp4`, or
  `Repetición …` in Spanish). It can run on its own or together with a recording; a recording
  started while it runs begins at once, on a forced keyframe. Memory: what the quality preset
  encodes in that time, about 30 MB for 20 s at 1080p60 medium.
- **Hardware scaling** to 1440p, 1080p or 720p (downscale only, aspect ratio preserved).
- **Region recording**: drag a rectangle on the preview to record only that part (cropped on the GPU).
- **Webcam overlay** (picture-in-picture) in any corner, in three sizes.
- **Screenshots** as full-resolution PNG, with the region and webcam applied.
- **Microphone noise suppression** with RNNoise, like OBS's filter.
- **Configurable global hotkeys** for start/stop, pause/resume, the replay buffer, saving a replay,
  screenshot and muting Mic/Aux, from any application. Defaults: `Ctrl+Alt+R` and `Ctrl+Alt+P`.
- **Hidden from capture**: like OBS's option, the obs-lite window never appears in recordings or
  in its own preview.
- **System tray icon**, with an option to minimize to the tray.
- **Spanish and English** interface.
- **Update notice**: at start-up it asks GitHub for the latest release and shows a link if there
  is a newer one. Nothing is downloaded; Settings › General turns the check off.
- Sharp preview at 5 fps: frames are scaled down to the size the preview is drawn at, averaging
  every pixel, so text stays legible.

## Configuration

Everything is set from the interface (*Ajustes* and the docks) and saved in
`%APPDATA%\obs-lite\settings.json`. Delete that file to go back to the defaults: first display,
cursor captured, desktop audio on, microphone off, 60 fps, native resolution, medium quality,
output to the *Videos* folder, hotkeys `Ctrl+Alt+R` and `Ctrl+Alt+P`, obs-lite hidden from capture,
update check on, 20 s replay buffer.
Recordings are named after their start time (`2026-10-16 13-45-02.mp4`) and screenshots likewise
(`Screenshot 2026-10-16 13-45-02.png`, or `Captura …` in Spanish).

## Known limitations

- The hardware encoder takes 0.2–1 s to start; that span is not recorded (the status bar shows
  *Preparando…*).
- An MP4 cannot be recovered if the process dies mid-recording.
- While the Windows session is locked, Windows delivers no new frames.
- If a window is resized while recording, the image is cropped or padded with black.
- On a laptop with two GPUs the image is captured, and so encoded, on the GPU driving the display
  (usually the integrated one).
- A PC without a GPU driver (for example a virtual machine without graphics acceleration) cannot
  record: colour conversion needs the GPU's video processor.
- One display or window source plus an optional webcam. No streaming, scenes, video filters or other
  codecs: obs-lite only records.
- Minimize to tray keeps the window minimized and removes its taskbar button (a hidden window would
  stop handling hotkeys); it still appears in Alt+Tab.
- Noise suppression downmixes the microphone to mono.
- Application audio is also part of desktop audio: with both on, that application is heard twice.
  It is chosen among applications with a window and found again by executable name; if the
  application starts after obs-lite, its audio is picked up when the recording starts.
- Hotkeys need Ctrl or Alt (F-keys excepted), and are off while Settings is open.
- Neither the executable nor the installer has an Authenticode signature (they are signed with
  OpenPGP instead), so Windows SmartScreen may warn before running them the first time and the
  installer shows an unknown publisher.
- The installer is per user: each Windows account that wants obs-lite installs it. Uninstalling
  keeps the settings in `%APPDATA%\obs-lite`.
- Pausing a recording also pauses the replay buffer: both share one encoder and clock.
- A replay starts at a keyframe (one every 2 s), so it is up to 2 s longer than the maximum
  replay time.

## Verifying a release

Each release zip and installer has a detached signature (`.asc`) made with this OpenPGP key:

```
A7FC EDEA D7E6 C343 7312  284A 442D A687 E0FF 73EE
```

1. Import the key and check that the fingerprint matches the one above **and** the one published
   in at least one other independent place (for example <https://github.com/25Dark25.gpg>):

   ```sh
   curl -sL https://github.com/25Dark25.gpg | gpg --import
   gpg --fingerprint A7FCEDEAD7E6C3437312284A442DA687E0FF73EE
   ```

2. Verify the signature of the zip (or, the same way, of the `.msi`):

   ```sh
   gpg --verify obs-lite-0.2.1-windows-x64.zip.asc obs-lite-0.2.1-windows-x64.zip
   ```

   The output must say `Good signature` and show the fingerprint above.

3. Optionally, compare the file's SHA-256 with the one in the release notes:
   `sha256sum obs-lite-0.2.1-windows-x64.zip`, or on Windows
   `Get-FileHash obs-lite-0.2.1-windows-x64.zip -Algorithm SHA256`.

`gpg` ships with [Gpg4win](https://gpg4win.org/) and with Git for Windows.

## Development

Requires Rust 1.95 or later and the MSVC Build Tools with the Windows SDK (used to embed the icon).

```powershell
cargo build --release
.\target\release\obs-lite.exe
```

Checks run by CI on every pull request (Windows, Rust 1.95 and stable):

```powershell
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
cargo deny --locked check   # needs cargo-deny; licenses, RustSec advisories, sources
```

The capture and encoding pipeline needs a GPU and a desktop session, so CI cannot exercise it.
`--selftest` records the primary display plus desktop audio through the same pipeline as the app:

```powershell
.\target\release\obs-lite.exe --selftest 6 test.mp4            # 6 s at 1080p60
.\target\release\obs-lite.exe --selftest 6 test.mp4 pause 720  # pauses the middle third, 720p output
.\target\release\obs-lite.exe --selftest 6 test.mp4 replay     # also saves the last 3 s as test-replay.mp4
```

It prints captured, encoded and dropped frame counts. The installer is built from a release build
with [WiX](https://wixtoolset.org/) 6.0.2 (`installer/build.ps1`), and `installer/test.ps1`
installs, upgrades and uninstalls it, as CI does. [docs/TESTING.md](docs/TESTING.md) has the
full manual test protocol and [docs/RELEASING.md](docs/RELEASING.md) the release steps.

### How it works

```
WGC (only when the image changes) ─┐
webcam (Media Foundation) ─────────┴─► GPU canvas (region crop + webcam overlay)
                                                │
recording clock (excludes pauses) ─► video thread converts the canvas every 1/fps
                                     to NV12 (GPU video processor, scaling) into a queue
WASAPI desktop + mic + app ─► mixer paced by the same clock (fills silence)
                                                │
      H.264 encoder (hardware, or software) + AAC encoder ─► packets ─┬─► MP4 writer
                                                    replay buffer (in memory) ◄─┘ ─► MP4 on save
```

- `src/capture.rs`: capture sessions, source list and preview.
- `src/encoder.rs`: clock, canvas, frame queue, constant-frame-rate encoder, recording file and
  replay buffer.
- `src/mf.rs`: Media Foundation pieces: GPU colour conversion, H.264 and AAC encoders, MP4 writer.
- `src/camera.rs`: webcam capture and overlay placement.
- `src/audio.rs`: WASAPI capture, devices and mixer.
- `src/app.rs`: user interface (egui) and recording lifecycle.
- `src/config.rs`: settings and their defaults.
- `build.rs`: draws the icon and embeds it in the executable.

## Security

Please report vulnerabilities privately; see [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE). Each release also ships `THIRD-PARTY-LICENSES.html` with the licenses of the
crates compiled into `obs-lite.exe`, generated with [cargo-about](https://github.com/EmbarkStudios/cargo-about)
(`cargo about generate --locked --fail about.hbs -o THIRD-PARTY-LICENSES.html`).
