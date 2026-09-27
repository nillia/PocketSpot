//! How the player's events change the published state. Pure: librespot's
//! events are first converted to [`Event`] (in `player.rs`), so these rules
//! are tested without librespot.

use crate::protocol::{ActiveDevice, PlayState, PlaybackContext, Snapshot, Track};

/// A player event, reduced to what the snapshot shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    TrackChanged(Track),
    Loading {
        position_ms: u32,
    },
    Playing {
        position_ms: u32,
    },
    Paused {
        position_ms: u32,
    },
    Stopped,
    /// A position update while playing, or after a seek.
    Position {
        position_ms: u32,
    },
    Volume {
        percent: u8,
    },
    Shuffle {
        enabled: bool,
    },
    /// The playback context (playlist, album, Liked Songs) changed.
    Context {
        uri: String,
    },
}

/// Apply `event` at `now_ms`. A context takes its name from the library
/// when the library lists it.
pub fn apply(state: &mut Snapshot, event: Event, now_ms: u64) {
    let playback = &mut state.playback;
    let sample = |playback: &mut crate::protocol::Playback, position_ms: u32| {
        playback.position_ms = position_ms;
        playback.position_at_ms = now_ms;
    };
    match event {
        Event::TrackChanged(track) => playback.track = Some(track),
        Event::Loading { position_ms } => {
            playback.state = PlayState::Buffering;
            sample(playback, position_ms);
            state.device = ActiveDevice::Local;
        }
        Event::Playing { position_ms } => {
            playback.state = PlayState::Playing;
            sample(playback, position_ms);
            state.device = ActiveDevice::Local;
        }
        Event::Paused { position_ms } => {
            playback.state = PlayState::Paused;
            sample(playback, position_ms);
        }
        Event::Stopped => {
            playback.state = PlayState::Stopped;
            sample(playback, 0);
            if state.device == ActiveDevice::Local {
                state.device = ActiveDevice::None;
            }
        }
        Event::Position { position_ms } => {
            if playback.state == PlayState::Buffering {
                playback.state = PlayState::Playing;
            }
            sample(playback, position_ms);
        }
        Event::Volume { percent } => playback.volume = percent,
        Event::Shuffle { enabled } => playback.shuffle = enabled,
        Event::Context { uri } => {
            if uri.is_empty() {
                playback.context = None;
            } else if playback.context.as_ref().is_none_or(|c| c.uri != uri) {
                let name = context_name(state, &uri);
                state.playback.context = Some(PlaybackContext { uri, name });
            }
        }
    }
}

/// The name the library gives `uri`, if it lists it.
pub fn context_name(state: &Snapshot, uri: &str) -> Option<String> {
    state
        .library
        .items
        .iter()
        .find(|item| item.uri == uri)
        .map(|item| item.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_then_playing_then_pausing_updates_state_position_and_device() {
        let mut state = Snapshot::default();
        apply(&mut state, Event::Loading { position_ms: 0 }, 1_000);
        assert_eq!(state.playback.state, PlayState::Buffering);
        assert_eq!(state.device, ActiveDevice::Local);
        apply(&mut state, Event::Playing { position_ms: 0 }, 1_500);
        assert_eq!(state.playback.state, PlayState::Playing);
        assert_eq!(state.playback.position_at(4_500), 3_000);
        apply(&mut state, Event::Paused { position_ms: 3_000 }, 4_500);
        assert_eq!(state.playback.state, PlayState::Paused);
        assert_eq!(state.playback.position_at(60_000), 3_000);
    }

    #[test]
    fn a_position_update_ends_buffering_and_stopping_frees_the_device() {
        let mut state = Snapshot::default();
        apply(&mut state, Event::Loading { position_ms: 0 }, 0);
        apply(&mut state, Event::Position { position_ms: 1_000 }, 1_000);
        assert_eq!(state.playback.state, PlayState::Playing);
        apply(&mut state, Event::Stopped, 2_000);
        assert_eq!(state.playback.state, PlayState::Stopped);
        assert_eq!(state.device, ActiveDevice::None);
    }

    #[test]
    fn the_context_takes_its_name_from_the_library_once() {
        let mut state = Snapshot::default();
        state.library.items.push(crate::protocol::LibraryItem {
            uri: "spotify:playlist:a".into(),
            name: "Focus".into(),
            ..Default::default()
        });
        let context = |uri: &str| Event::Context { uri: uri.into() };
        apply(&mut state, context("spotify:playlist:a"), 0);
        assert_eq!(
            state.playback.context.as_ref().unwrap().name.as_deref(),
            Some("Focus")
        );
        apply(&mut state, context("spotify:album:b"), 0);
        assert_eq!(state.playback.context.as_ref().unwrap().name, None);
        apply(&mut state, context(""), 0);
        assert_eq!(state.playback.context, None);
    }

    #[test]
    fn volume_shuffle_and_tracks_are_mirrored() {
        let mut state = Snapshot::default();
        apply(&mut state, Event::Volume { percent: 70 }, 0);
        apply(&mut state, Event::Shuffle { enabled: true }, 0);
        let track = Track {
            title: "Northbound".into(),
            ..Track::default()
        };
        apply(&mut state, Event::TrackChanged(track.clone()), 0);
        assert_eq!(state.playback.volume, 70);
        assert!(state.playback.shuffle);
        assert_eq!(state.playback.track, Some(track));
    }
}
