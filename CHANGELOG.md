# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Update notice: at start-up obs-lite asks GitHub for the latest release and shows a link in the
  status bar if there is a newer one (Settings › General can turn this off).
- Application Audio Capture source: records the sound of one application, with its own mixer
  channel.
- MSI installer, alongside the portable zip: installs for the current user without administrator
  rights, adds *Obs Lite* to the Start menu and, optionally, to the desktop, and upgrades in place.
- Replay buffer: keeps the last 20 s (configurable) in memory and saves them to an MP4 on demand,
  from the Controls dock, the tray menu or a hotkey. It can run with or without a recording.

### Changed

- New encoding engine: obs-lite drives the Media Foundation H.264 and AAC encoders and the MP4
  writer itself instead of going through MediaTranscoder. Colour conversion and scaling run on the
  GPU's video processor; colours are BT.709 and signalled in the file; a keyframe every 2 s.
  Recording 1080p60 uses about 80 MB less memory.
- Without a usable hardware H.264 encoder, the Windows software encoder is used.

## [0.2.1] - 2026-10-17

The 0.2.0 release was withdrawn before its files were attached; 0.2.1 includes all of its
changes.

### Changed

- The preview is scaled to the size it is drawn at (up to the source's resolution) with area
  averaging, instead of to 640 pixels by picking one pixel per block, so it is no longer pixelated.

## [0.2.0] - 2026-10-17 [YANKED]

### Added

- English interface, following the Windows display language or chosen in Settings › General.
- Option to hide obs-lite from screen capture, like OBS Studio's (on by default).
- PNG screenshots of the source at full resolution: File menu, preview context menu or hotkey.
- Configurable global hotkeys, including taking a screenshot and muting Mic/Aux.
- Recording a region of the source, chosen by dragging on the preview.
- Microphone noise suppression (RNNoise).
- System tray icon showing the recording state, with an option to minimize to the tray.
- Webcam overlay (Video Capture Device source) in a chosen corner and size.

### Fixed

- Recordings no longer contain occasional black frames (about one every 15 s in 0.1.0, more
  with a webcam active) or half-drawn frames.

## [0.1.0] - 2026-10-16

### Added

- Screen recorder for Windows with an OBS Studio-like interface (preview, Sources, Audio Mixer and
  Controls docks, status bar, Settings dialog) in Spanish.
- Display and window capture through Windows Graphics Capture, with optional cursor.
- Hardware H.264 encoding to MP4 at a constant 30 or 60 fps, with optional downscaling to 1440p,
  1080p or 720p and three quality presets.
- Desktop audio and microphone capture, mixed into AAC, with per-channel volume, mute, meters and
  device selection.
- Pause and resume without gaps in the recording.
- Global hotkeys: `Ctrl+Alt+R` to start/stop and `Ctrl+Alt+P` to pause/resume.
- Settings saved in `%APPDATA%\obs-lite\settings.json`.
- `--selftest` command-line mode that records through the full pipeline and reports frame counts.
- Minimum supported Rust version: 1.95.
- Releases ship `THIRD-PARTY-LICENSES.html` with the licenses of the bundled crates.

[Unreleased]: https://github.com/25Dark25/obs-lite/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/25Dark25/obs-lite/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/25Dark25/obs-lite/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/25Dark25/obs-lite/releases/tag/v0.1.0
