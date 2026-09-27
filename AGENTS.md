# Agent instructions

Instructions for AI coding agents working in this repository. Read
[README.md](README.md), [CONTRIBUTING.md](CONTRIBUTING.md) and the
[roadmap](docs/roadmap.md) first.

## Role

The maintainer writes the Rust implementation. By default, act as a reviewer
and pair: explain trade-offs and compiler errors, review changes, suggest
small next steps and help debug. Write or change implementation code only
when asked to for a specific task. Repository tooling, documentation and CI
may be written when an issue asks for them.

## Workflow

- Work from an issue. If there is none, propose one (context, scope
  checklist, acceptance criteria) before starting.
- Branch from `main` as `<type>/<issue>-<slug>`. Never commit or push to
  `main` directly.
- Commit with Conventional Commits, as in CONTRIBUTING.md.
- Open a pull request with the template, link the issue (`Closes #<n>`) and
  state exactly what was tested: host, CI, device.
- Do not merge, tag, publish releases or change repository settings unless
  asked to.

## Attribution

- Add `Co-Authored-By:` for the agent only to commits where it wrote part of
  the change. Commits written by the maintainer carry no agent trailer.
- No "generated with" lines or similar footers in commits, pull requests,
  issues or code.

## Engineering rules

- Playback lives in a service independent of the UI. Leaving the UI either
  detaches (music continues) or explicitly stops playback.
- Control interfaces bind locally (Unix socket). Never expose a playback or
  control API on a network interface by default.
- Never log, print, commit or request account tokens, credentials, pairing
  codes, session files or unredacted logs.
- Track the processes you own. Never use global `killall`, never treat a PID
  file alone as proof of identity, never touch another application's files
  or markers.
- Do not modify firmware, install boot services or rewrite global audio or
  network configuration. Prefer scoped, reversible changes that are restored
  on exit, and document any broader change.
- Pin dependencies, the toolchain and upstream revisions. Keep licence
  notices and source-distribution obligations intact.
- Keep firmware-specific behaviour (paths, audio, input, display, launcher,
  power) behind the platform profile.
- Host tests use the mock playback engine and injected clocks, not the
  network or real time.
- Claims about sound, controls, suspend, battery, performance or running
  alongside games need a device test report for that build. Cross-compiling
  or passing CI is not device validation.

## Scope

PocketSpot is an unofficial Spotify client. Official offline downloads, DRM
circumvention and audio from other services are out of scope.
