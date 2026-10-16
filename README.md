# obs-lite

Lightweight screen recorder for Windows: a single ~5.8 MB `.exe`, with no bundled runtime or browser.
The interface follows OBS Studio, so OBS users find everything where they expect it.

| | Idle (preview + audio) | Recording 1080p60 + audio |
|---|---|---|
| Private memory | ~85 MB | ~180 MB |
| CPU (16 threads) | ~0.3 % | ~1.1 % |

Measured on Windows 11 with an RTX 4060 Laptop GPU; your numbers will vary.

> **Status: early (0.1.0, first release).** obs-lite has been tested on a single machine
> (Windows 11, NVIDIA GPU); see [docs/TESTING.md](docs/TESTING.md) for what was and was not
> verified. It has not had any security audit or independent code review. Most of the code was
> written with an AI coding assistant (Claude Code) and then built and tested as described there.
> It is not affiliated with the OBS Project and contains no OBS code.

## Requirements

- Windows 11. Windows 10 has not been tested.
- A GPU with a hardware H.264 encoder (NVIDIA, Intel or AMD). Only NVIDIA has been tested.

## Quick start

1. Download `obs-lite.exe` from [Releases](https://github.com/25Dark25/obs-lite/releases) and
   [verify it](#verifying-a-release).
2. Run it. There is nothing to install.
3. Press *Iniciar Grabación* (or `Ctrl+Alt+R`). By default it records the first display Windows
   reports and desktop audio at 60 fps to your *Videos* folder.

## Interface

Same layout as OBS Studio, its dark *Yami* palette and its Spanish wording:

- **Preview** on top and, below it, the **Fuentes** (Sources), **Mezclador de audio** (Audio Mixer)
  and **Controles** (Controls) docks.
- **Fuentes › +**: *Captura de pantalla* (display capture) or *Captura de ventana* (window capture).
  *Propiedades*: capture the cursor.
- **Mixer**: desktop audio and *Mic/Aux*, each with a meter, volume slider and mute button
  (mute also works while recording).
- **Controls**: *Iniciar Grabación* (with a pause button next to it while recording), *Ajustes*
  (Settings) and *Salir* (Exit).
- **Settings**: *Salida* (recording path and quality), *Audio* (global devices), *Vídeo*
  (scaled output resolution and FPS) and *Atajos* (hotkeys). *Aceptar* applies, *Cancelar* discards.
- **Status bar**: messages, CPU usage, dropped frames and the recording indicator with elapsed time.
- **Archivo › Mostrar grabaciones** opens the output folder.

## Features

- **Display or window capture** through Windows Graphics Capture (the API OBS uses), with or
  without the cursor.
- **Desktop audio + microphone** mixed together, with volume, mute, meters and device selection.
- **Hardware H.264** (NVENC / Quick Sync / AMF through Media Foundation) to MP4, which plays everywhere.
- **Constant frame rate** (30 or 60 fps), suitable for video editors. When the screen does not
  change the last frame is repeated; when the machine falls behind, slots are skipped and reported
  as dropped frames.
- **Pause and resume** without gaps: paused time never reaches the file, in video or audio.
- **Hardware scaling** to 1440p, 1080p or 720p (downscale only, aspect ratio preserved).
- **Global hotkeys**: `Ctrl+Alt+R` starts/stops recording and `Ctrl+Alt+P` pauses/resumes,
  from any application.
- Low-cost preview (5 fps).

## Configuration

Everything is set from the interface (*Ajustes* and the docks) and saved in
`%APPDATA%\obs-lite\settings.json`. Delete that file to go back to the defaults: first display,
cursor captured, desktop audio on, microphone off, 60 fps, native resolution, medium quality,
output to the *Videos* folder. Recordings are named after their start time
(`2026-10-16 13-45-02.mp4`).

## Known limitations

- The hardware encoder takes 0.2–1 s to start; that span is not recorded (the status bar shows
  *Preparando…*).
- An MP4 cannot be recovered if the process dies mid-recording.
- While the Windows session is locked, Windows delivers no new frames.
- If a window is resized while recording, the image is cropped or padded with black.
- No streaming, scenes, multiple sources at once, filters or other codecs: obs-lite only records.

## Verifying a release

Releases are signed with this OpenPGP key:

```
A7FC EDEA D7E6 C343 7312  284A 442D A687 E0FF 73EE
```

1. Import the key and check that the fingerprint matches the one above **and** the one published
   in at least one other independent place (for example <https://github.com/25Dark25.gpg>):

   ```sh
   curl -sL https://github.com/25Dark25.gpg | gpg --import
   gpg --fingerprint A7FCEDEAD7E6C3437312284A442DA687E0FF73EE
   ```

2. Verify the detached signature of each file you downloaded, then its hash against
   `SHA256SUMS` (also signed):

   ```sh
   gpg --verify obs-lite.exe.asc obs-lite.exe
   gpg --verify SHA256SUMS.asc SHA256SUMS
   sha256sum --check --ignore-missing SHA256SUMS
   ```

   On Windows without `sha256sum`, compare the output of
   `Get-FileHash obs-lite.exe -Algorithm SHA256` with the line in `SHA256SUMS`.

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
```

It prints captured, encoded and dropped frame counts. [docs/TESTING.md](docs/TESTING.md) has the
full manual test protocol and [docs/RELEASING.md](docs/RELEASING.md) the release steps.

### How it works

```
WGC (only when the image changes) ─► ring of 3 GPU textures
                                                │
recording clock (excludes pauses) ─► the encoder pulls one frame every 1/fps
                                     and receives the newest texture
WASAPI desktop + mic ─► mixer paced by the same clock (fills silence)
                                                │
                     MediaStreamSource ─► MediaTranscoder (HW, scaling) ─► MP4
```

- `src/capture.rs`: capture sessions, source list and preview.
- `src/encoder.rs`: clock, texture ring and constant-frame-rate encoder.
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
