# Roadmap

PocketSpot is built in releases that are each usable on their own. A release
is done when its issues are closed, CI is green, and a device test report for
the release build confirms its promises on real hardware. CI never counts as
device evidence.

First target: TrimUI Brick running NextUI. Everything that differs between
firmware (paths, audio, input, display, launcher, power hooks) lives behind a
platform profile from the first release, so further firmware is a new
profile, not a rewrite.

## v0.1 — Playback service

The foundation: playback runs as a service, independent of any UI, and is
driven from the command line.

- Playback service with the platform profile (NextUI implementation)
- Device-code pairing shown in the terminal; the saved login survives restarts
- Local, versioned control protocol (Unix socket, never on the network)
- Command-line client: status, play a playlist or Liked Songs, pause,
  previous/next, volume, stop
- Spotify Connect: the Brick appears as a device the Spotify app can control
- Mock playback engine for tests without an account, network or device
- Release pipeline: tagged commits build and publish the binaries

Device evidence, on the Brick over ssh: pair, play through the speaker,
control from the phone, close the terminal and keep listening, stop.

## v0.2 — Play on the Brick

The handheld experience, as a client of the service.

- UI with framebuffer rendering and controller input
- NextUI package, launched from Tools
- Pairing screen with a QR code
- Library: your playlists and Liked Songs
- Now Playing: play/pause, previous/next, volume, shuffle
- Leave with "Return to games" (music continues) or "Stop music and exit"
- The UI restarts a service that died; logout

Device evidence: install, pair, play, detach and reattach, stop and relaunch
at once.

## v0.3 — Browse, search, Connect

- Playlist and album details; start from any track in context
- Search
- Local pins file
- Spotify Connect in the UI: show what plays on another device, move it here
- Streaming quality setting

Device evidence: browse, search and transfer on the device; installing v0.3
replaces a running v0.2 service (the upgrade path).

## v0.4 — Robust audio and network

- Speaker, headphone jack and USB DAC, with hot-plug switching while playing
- Reconnect with backoff after network loss; resume if it was playing
- Wi-Fi power saving handled while playing, always restored
- Clear error and recovery states in the UI

## v0.5 — Gaming

- Music while a game runs: audio coexistence, per game and emulator
- The service survives starting, playing and leaving games
- Power profile: CPU, memory, wake-ups and battery drain measured with and
  without music, idle and while gaming, published as the baseline

Device evidence: a tested list of games and emulators, and the measurement
method and results.

## v0.6 — Power and performance

- Suspend and resume: playback, network and the saved session afterwards
- Improvements measured against the v0.5 power profile
- Updated measurements published with the release

## v1.0 — Stable on NextUI

v0.6 after a stretch of daily use with no open critical issues. The first
release recommended to everyone with a Brick on NextUI.

## v1.1 — More firmware on the Brick

- Knulli and muOS profiles: launchers, state locations, input mapping
- Audio through the system sound server where one runs
- One release, one package per firmware, one device test report each

## Later

- Other handhelds (for example 640×480 devices) through display profiles
- Distribution through PortMaster

## Engineering principles

These shape every release:

- One task owns playback state; everything else sends it messages and
  reads published snapshots.
- UI state and screens are pure and testable; hardware and I/O live in
  adapters at the edges.
- Decisions (routing, reconnects, resume) are pure functions or state
  machines, tested on the host with a controllable clock.
- A mock playback engine lets the UI and service be tested without an
  account, a network or the device.
- Nothing claims sound, controls, suspend or gaming behaviour without a
  device test report.
