# obs-lite

Lightweight screen recorder for Windows: a single ~5.8 MB `.exe`, with no bundled runtime or browser.
The interface follows OBS Studio, so OBS users find everything where they expect it.

| | Idle (preview + audio) | Recording 1080p60 + audio |
|---|---|---|
| Private memory | ~85 MB | ~180 MB |
| CPU (16 threads) | ~0.3 % | ~1.1 % |

Measured on Windows 11 with an RTX 4060 Laptop GPU; your numbers will vary.

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
- Settings saved in `%APPDATA%\obs-lite\settings.json`.

## Building

Requires Rust (edition 2024; developed with 1.99) and the MSVC Build Tools with the Windows SDK
(used to embed the icon).

```powershell
cargo build --release
.\target\release\obs-lite.exe
```

## Headless self-test

Records the primary display plus desktop audio through the same pipeline as the app:

```powershell
.\target\release\obs-lite.exe --selftest 6 test.mp4            # 6 s at 1080p60
.\target\release\obs-lite.exe --selftest 6 test.mp4 pause 720  # pauses the middle third, 720p output
```

It prints captured, encoded and dropped frame counts.

## How it works

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
- `build.rs`: draws the icon and embeds it in the executable.

## Known limitations

- The hardware encoder takes 0.2–1 s to start; that span is not recorded (the status bar shows
  *Preparando…*).
- An MP4 cannot be recovered if the process dies mid-recording.
- While the Windows session is locked, Windows delivers no new frames.
- If a window is resized while recording, the image is cropped or padded with black.

## License

[MIT](LICENSE)
