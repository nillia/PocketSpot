# Control protocol

Clients (the command-line client, the UI) control the PocketSpot service
through a Unix socket. This document describes protocol version **1**.

## Transport

- Socket: `control.sock` in the service's runtime directory, which is
  private to the user (0700). Nothing listens on a network interface.
- One request per connection: the client connects, writes one request
  line, reads one reply line and closes.
- Each message is a single line of JSON, at most 256 KiB, terminated by
  `\n`.
- A client has 2 seconds to send its request and read the reply once it is
  written. Slow or silent clients never delay others.

## Envelope

```json
{"protocol": 1, "request": {"type": "snapshot", "since": 1790518244035000}}
{"protocol": 1, "response": {"type": "unchanged", "revision": 1790518244035000}}
```

A request in another protocol version is answered with
`{"type": "version_mismatch", "supported": 1}`, except for the stable
handshake below.

## Stable handshake

`identify` and `shutdown` keep exactly this shape in every protocol version,
and the service answers them whatever `protocol` the envelope carries. A
newer client can therefore always recognise an older service after an
upgrade and stop it.

```json
{"protocol": 1, "request": {"type": "identify"}}
{"protocol": 1, "response": {"type": "identity", "server": "pocketspotd", "version": "0.1.0", "pid": 4242, "protocol": 1}}

{"protocol": 1, "request": {"type": "shutdown"}}
{"protocol": 1, "response": {"type": "accepted"}}
```

## Requests

| Request | Reply | Service answers within |
|---|---|---|
| `identify` | `identity` | 2 s |
| `snapshot` (optional `since`) | `snapshot`, or `unchanged` when `since` equals the current revision | 2 s |
| `command` | `accepted` or `rejected` | 2 s |
| `pair` | `accepted`, or `rejected` when already signed in | 2 s |
| `logout` | `accepted` or `rejected` | 6 s |
| `shutdown` | `accepted` | 2 s |

Clients wait 2 seconds longer than the service's bound, so a slow success is
never reported as a failure.

`pair` asks for a new pairing code (for example after one expired or was
declined) or retries signing in immediately after a network failure.

### Commands

```json
{"type": "command", "command": {"type": "play", "context_uri": "spotify:playlist:…", "track_uri": null}}
```

| Command | Fields |
|---|---|
| `play` | `context_uri`, optional `track_uri` (start at that track in the context) |
| `pause`, `resume`, `next`, `previous`, `stop` | none |
| `set_volume` | `percent` (0–100) |
| `set_shuffle` | `enabled` |

### Rejections

```json
{"type": "rejected", "reason": "not_ready", "message": "not signed in"}
```

`reason` is one of `busy`, `not_ready`, `invalid`, `unavailable`,
`malformed`; `message` is for people, not programs.

## Snapshot

The complete state a client needs to show. Every field has a default, so a
client decodes snapshots that miss fields it knows.

| Field | Meaning |
|---|---|
| `revision` | Changes whenever anything else changes. Starts at the service's start time in ms × 1000, so a restarted service never repeats a revision |
| `session` | `starting`, `pairing` (`url`, `code`, `expires_at_ms`), `connecting`, `ready`, `failed` (`kind`, `message`, `retry_at_ms`) |
| `playback` | `state` (`stopped`, `buffering`, `playing`, `paused`), `track`, `context`, `position_ms` sampled at `position_at_ms`, `volume`, `shuffle` |
| `device` | `none`, `local`, or `remote` with the Connect device `name` |

Clients poll with `since` set to the last revision they received; an
unchanged service answers with a few bytes.

## Versioning

Any incompatible change bumps the protocol version. Adding a field with a
default, or a new request, command or reason, is compatible.
