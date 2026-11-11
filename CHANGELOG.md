# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The window, the tray and the messages call the app Obs Lite, as the Start menu and the
  applications menu already did. File names and the settings folder stay `obs-lite`.
- Linux: the applications menu shows the description in the new languages too.

### Fixed

- A setting that cannot be read (a value from a newer version after going back, a hand edit of
  the wrong type) only loses itself. Every setting was reset, and the file overwritten on exit,
  the recording to repair included.
- A mis-click while choosing a region to record keeps the region there was; it was silently
  dropped, and the next recording took the whole source.

## [0.7.0] - 2026-11-08

### Added

- The interface is also in German, French, Italian, Polish, Russian, Japanese and Chinese
  (Simplified and Traditional), following the system language or chosen in Settings › General.
  These are machine translations; corrections are welcome. Japanese and Chinese use the system's
  font for those characters; without one, obs-lite shows in English and says why.

## [0.6.0] - 2026-11-07

### Changed

- Windows: a GPU that cannot convert the picture for the encoder now stops the recording with
  that error; the recording was green, with nothing in it.
- Windows: the C runtime is inside obs-lite.exe. The build needed VCRUNTIME140.dll, which a fresh
  Windows does not have (other programs bring it), and did not start there.

- The project moved to https://github.com/25Dark25/ObsLite (the old address redirects there), so
  that the Flatpak's id matches it, as Flathub requires. The Linux metainfo has a screenshot,
  brand colours, the smallest screen it fits and notes for the last release.

### Fixed

- Pausing a recording no longer pauses the replay buffer: the encoder goes on, and only the file
  leaves the pause out, going on at a keyframe asked for on resuming. The timer shows the file's
  length.
- Linux: recording with VA-API (Intel, AMD) keeps the desktop's frames on the GPU, as on Windows:
  they are cropped to the region, converted, given the webcam on top and encoded there, and only
  the preview (small) and screenshots are brought into memory. Recording 1080p60 on Intel went
  from about 40 % to 11 % of a CPU core, and from 165 to 140 MB of memory; with a region, the
  webcam and the preview, from about 42 % to 22 % (measured in the same session). Where the
  desktop cannot send its frames so, or the GPU cannot encode them, they are taken in memory as
  before.
- Linux: recording takes less CPU again: the desktop's picture is no longer converted twice (it
  is taken in the format the desktop sends when obs-lite can use it), and the whole picture is no
  longer copied for every frame. Recording 1080p60 with VA-API on Intel went from about 50 % to
  39 % of a CPU core.
- Linux: recording takes less CPU: the conversion for the encoder no longer runs on every thread
  (which cost more than it saved), and NVENC converts the picture on the GPU itself. Recording
  1080p60 with VA-API on Intel went from about 61 % to 49 % of a CPU core.
- Linux: obs-lite no longer processes every frame a desktop sends when it sends more than needed;
  on COSMIC, which sent about 80 a second for a still screen, the idle preview went from about
  20 % to 4 % of a CPU core.
- Recordings, replays and screenshots made in the same second no longer write over each other: a
  name already in use gets " (2)", " (3)" and so on.
- A second obs-lite started while the first is recording no longer "repairs" the recording the
  first is still writing, which corrupted it.
- A recording path typed empty or relative in Settings is not taken; recordings went to whatever
  folder obs-lite was started from.
- The settings file is replaced whole, so a crash or a full disk while saving no longer resets
  every setting.
- Windows: when the hardware encoder fails mid-recording (a GPU reset, a driver error), the
  recording stops with the error instead of hanging on "Saving…", or hanging obs-lite on exit.
- Windows: desktop audio and the microphone follow the default device: switching to headphones
  after obs-lite started left them on the old device, and desktop audio was silent in every
  recording until a restart. An unplugged default device is opened again once there is one.
- Windows: application audio no longer builds up delay after the system is briefly busy, and
  packets Windows marks as silence are silent.
- Windows: the window no longer redraws about 100 times a second while there is sound.
- Windows: while the encoder was falling behind, a frame could be drawn over before the encoder
  had read it, showing frames out of order or half drawn.
- A recording that cannot be written (a folder that cannot be written to, a full disk, an encoder
  that fails) now stops at once and says why. Before, the timer kept running and the problem only
  showed at Stop, audio for the file piled up in memory, and the file was left unfinished even
  when most of it had been written. Now it is finished, keeping what was recorded until then, and
  the message says where. A replay that fails to save is finished too.
- Turning a recording into a regular MP4 now puts its new index on the disk before the old ones
  become padding: a power cut right after stopping could leave a file with no index at all.
- A recording whose conversion was interrupted is finished at the next start with one index, not
  two (the second one missing samples).
- Linux: a failing encoder or a full disk is noticed while recording; before, every later packet
  queued in memory and the recording was reported as saved, cut short. If the encoder cannot be
  started, the error is shown instead of sharing the screen again and again.
- Windows: capture works again on Windows 10 and on Windows 11 before 24H2. obs-lite asked
  Windows for a minimum time between frames, which only 24H2 and later support, so elsewhere no
  source could be previewed, recorded or captured.
- Windows: the arrow keys and `=` `[` `]` `` ` `` can be global hotkeys.
- Windows: the webcam overlay picks a mode of at least 25 fps when the camera has one; many
  webcams offer 720p uncompressed at only 10 fps before 720p compressed at 30.
- Windows: a camera that changes its picture size mid-stream no longer makes obs-lite read past
  the end of a picture, and having no camera at all is handled cleanly.
- Windows: installing a rebuilt package of the version already installed replaces it instead of
  adding a second Obs Lite to the list of apps.
- Windows: a window with an odd width or height is recorded sharp, losing its last column or row,
  instead of being scaled down on every frame with a black line at the edge.
- Flatpak: a recording folder typed by hand that is not shared with obs-lite is refused, with a
  message to choose it with Browse; recordings, replays and screenshots saved there were in the
  sandbox's memory and lost when obs-lite closed.
- Linux: obs-lite's language follows LANGUAGE, as other applications do.
- Linux: clearing every hotkey releases its combinations; the desktop kept taking them from other
  applications until obs-lite closed.
- Linux: an unplugged camera ends with a message instead of leaving its last picture frozen.
- Linux: a recording path that is not valid UTF-8 is refused instead of the file going elsewhere.
- Windows: a failed preview picture, or a failed copy kept for screenshots, no longer stops a
  recording.
- An audio input that stalled for a moment (a busy system) no longer stays late against the video
  for the rest of the recording; a device a little faster than the clock no longer drifts up to
  100 ms late either.
- Windows: windows the desktop hides (suspended Store apps, windows on other virtual desktops) are
  no longer offered as sources or for application audio; capturing them gave no picture.
- Linux: an H.264 encoder that is installed but fails at its first picture no longer makes the
  recording fail: each one is tried on one picture of the recording's size, and the next is used
  if it fails. For example, Ubuntu 24.04's NVENC encoder (GStreamer 1.24) fails with NVIDIA's
  595 driver; obs-lite then uses the next encoder (software if no other works).
- `--selftest` with a negative or endless length no longer aborts mid-recording.
- Linux: when the desktop's screen-sharing portal crashes, the capture now carries on: obs-lite
  starts sharing again from the remembered choice, without the dialog, and a recording keeps going
  with the picture frozen for under a second. Before, the preview was lost and a recording
  stopped. COSMIC's portal (Pop!_OS 24.04) crashes every few seconds to minutes. Stopping sharing
  from the desktop, or closing a shared window, still ends the capture.
- Linux: recording failed at once wherever a hardware H.264 encoder was found: the hardware H.264
  encoders (VA-API and NVENC) only take NV12 pictures, and obs-lite gave them I420. Software
  encoding (x264), used where there is no GPU, was not affected.
- Escape while setting a hotkey cancels only that, not the whole Settings dialog with its changes.
- Setting a hotkey that another action has takes it from that action; before, the second one
  failed and the message blamed another application.
- While Settings is open, the tray does not start a recording or the replay buffer (it says to
  close Settings first): OK there then undid part of the new recording's state.
- An audio device that failed (unplugged, then plugged back in) is tried again when a session
  starts, or on OK in Settings.
- A screenshot asked for while a recording is being saved says to wait, instead of asking for a
  source.
- The Hotkeys page now says why it is greyed out during a session, as the other pages do.
- Menus close when a session starts, so an open one can no longer remove the camera or the
  application audio from a running recording.
- A minimized window no longer wakes 15 times a second after loud audio.
- Windows: the update check and starting a capture no longer lean on COM started by other
  threads. Once those threads had ended, Windows unloaded code that was still in use, and the
  next call crashed the process. The tests crashed so about once in 20 runs; it was not seen in
  the app, whose window keeps COM running.
- Pausing a recording, resuming and pausing again before the file went on took the first pause
  out twice: the rest of the recording got times from before what was already in it, or was
  dropped, and the timer went back.
- Saving a replay no longer copies the whole replay buffer while the encoder waits for it: with a
  long buffer (minutes, high quality) the copy took up to about half a second and took memory
  again, the window stopped for that time and a recording running at the same time lost frames.
- An encoder that fails while only the replay buffer runs (a GPU reset, a driver error) now
  stops it and says why. Before, the replay buffer went on saying it was on but kept nothing new,
  and a recording started afterwards got no frame and said only "no frame was encoded".
- A capture that ends with an error (the encoder could not be started, the screen-sharing
  dialog was cancelled, a missing plugin on Linux) shows that error, instead of "The source is
  no longer available".
- A power cut (or a crash of the system) while a recording was being made regular could leave a
  file that the repair at the next start made unplayable, taking its only full index for padding,
  or that players read as empty. The initial index is now made padding first and put on the
  disk before the fragments' indexes, and the repair takes only an index before the fragments
  for the initial one.
- Starting obs-lite after a recording no longer reads the whole box list of that recording
  again, looking for something to repair, before the window shows; on a hard disk or a network
  folder after a restart this could take seconds or more. Only a recording that was not finished
  whole is checked.
- Linux: an obs-lite started while another one was recording (after the first one was closed)
  could take that recording for one cut short and "repair" it while it was still being written,
  ruining it. A recording is now locked while it is written, and the repair leaves a locked file
  alone (and checks it again at the next start).
- Windows: a computer that slept while recording no longer fills the recording with the time
  asleep. obs-lite made up for it at once on waking: all that silence was allocated (gigabytes
  after a night, enough to crash it), encoded and written. The time asleep is now left out of
  the recording, as on Linux.
- When the computer fell behind and frames were skipped, the video lost that time and ran ahead
  of the audio, more with every skip (on Windows about half the skipped time was lost). Each
  frame now lasts until the next one, in recordings and saved replays.
- Opening the + menu after the recorded window was closed no longer selects the first display by
  itself: it went on to record that display (with the region drawn on the window) while the
  preview stayed black. The source changes only when one is chosen; a saved region is dropped at
  start-up too when the saved source is not there.
- Windows: Application Audio Capture follows an application that was closed and opened again
  while obs-lite stayed open. It stayed bound to the old process: the application's channel
  recorded nothing, and desktop audio no longer left the application out.
- Windows: a webcam whose picture rows are padded (picture widths such as 424 or 1366) no
  longer stays blank: every such picture was skipped.
- Windows: Application Audio Capture lists Store apps (Calculator, Media Player…) by their own
  names and records them. They were all one entry, "ApplicationFrameHost.exe", which recorded
  silence.
- A webcam that stops (unplugged, access taken away, its stream ended) no longer leaves its last
  picture frozen in the preview, screenshots and recordings. The status bar says so, a recording
  going on stops showing it, and the camera is tried again when the next recording or replay
  buffer starts.
- A webcam no longer wakes obs-lite for every picture (about 30 times a second) while the window
  is minimized or in the tray, as the screen preview already did not.
- Linux: removing or changing the webcam, or closing obs-lite, while the desktop is still asking
  whether obs-lite may use the cameras no longer freezes the window until that is answered.
- OK in Settings during a recording no longer restarts an audio device that had failed: its meter
  moved again, but the recording stayed silent until it was stopped. The device is tried again
  when the next recording or replay buffer starts, as before.
- With two obs-lite open, closing one no longer makes the other's recording go unrepaired if
  that one then stops partway (a crash, a power cut): saving its settings wrote over the
  recording the other one had just started.
- Volumes, mutes, the region, the webcam and the other choices made in the docks are saved
  within a second, not only when obs-lite closes: signing out or shutting down Windows with
  obs-lite open (in the tray, say) ends it without closing it, and they were lost.
- Linux: an empty or cut-short picture from the desktop (PipeWire before 1.4 sends empty ones;
  GNOME does when only the pointer moved) no longer closes obs-lite at once: it is skipped.
- Linux: recording a region on the GPU (VA-API) on GNOME records that region again; the whole
  screen or window was recorded, scaled down to the region's size.
- Linux: Settings › Audio lists the audio devices again when GStreamer's PipeWire plugin is
  installed (a native build): it offered only "Default", and a device chosen before was reset.
- Linux: every recording pipeline uses one VA-API display, as was meant: the sharing never took
  place, so each opened its own.
- Linux: listing the webcams no longer leaves the closed connection to the desktop's camera
  service on GStreamer's PipeWire device provider, which is shared by the whole process.

## [0.5.0] - 2026-10-25

### Added

- Recordings survive obs-lite or the PC stopping mid-recording (a crash, a power cut): the MP4 is
  written in fragments and keeps all but the last fraction of a second (about 0.3 s on Windows, up
  to 0.5 s on Linux). The next start turns such a recording into a regular MP4 and says so in the
  status bar. A normal stop still leaves a regular MP4, as before.

### Changed

- Windows: a window resized while recording is scaled down to fit the recording and centred,
  with black around it, instead of cropped or left in the corner. A smaller window is not
  enlarged, so it stays sharp.

### Fixed

- Windows: every recording left about 120 MB of memory behind, so obs-lite grew with each
  one (over 600 MB after five). The hardware encoder kept the GPU device of the recording, with
  its textures, until obs-lite closed; it is now shut down when the recording ends.
- With application audio on, desktop audio no longer includes that application as well: it is
  heard once instead of twice, and muting its channel silences it. Not when desktop audio is set
  to a specific output device.

## [0.4.1] - 2026-10-24

### Changed

- A newer version is announced in a dialog at start-up, with a button to its download page,
  instead of only a small link in the status bar (which stays). "Don't remind me of this version"
  keeps the dialog away until the next release.

## [0.4.0] - 2026-10-23

### Added

- Linux version, as a Flatpak (`obs-lite-0.4.0-linux-x86_64.flatpak`): the same interface,
  recording, pause, region, replay buffer, webcam, screenshots, tray icon and global hotkeys. The
  screen or window is chosen in the desktop's sharing dialog (PipeWire, so Wayland works), global
  hotkeys and the webcam go through the desktop, and encoding uses GStreamer (VA-API, NVENC or
  x264). The Flatpak has no network access. Not on Linux: application audio, hiding obs-lite from
  capture and the update notice.

### Changed

- Less CPU while idle: a silent window draws once a second instead of 15 times (it wakes as soon
  as there is sound), each new preview picture draws once instead of twice, and the preview is
  scaled about 40 % faster. Idle CPU fell from 7.4 % to about 4 % of one core on the test laptop.
- While the window is minimized, no preview pictures are made at all.

### Fixed

- Channels that together went over full scale (desktop audio and microphone both loud) were
  clipped, distorting the recording, while each meter looked fine. A limiter now lowers the
  volume at those moments, and the mixer shows *Mix at its limit: lower a volume*.

## [0.3.0] - 2026-10-18

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

[Unreleased]: https://github.com/25Dark25/ObsLite/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/25Dark25/ObsLite/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/25Dark25/ObsLite/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/25Dark25/ObsLite/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/25Dark25/ObsLite/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/25Dark25/ObsLite/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/25Dark25/ObsLite/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/25Dark25/ObsLite/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/25Dark25/ObsLite/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/25Dark25/ObsLite/releases/tag/v0.1.0
