//! The state snapshot the service publishes.
//!
//! Every field has a default, so a client can decode snapshots that are
//! missing fields it knows about.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// Changes whenever anything else changes. A service starts counting
    /// from its start time in milliseconds × 1000, so a restarted service
    /// never repeats a revision an earlier one reported.
    pub revision: u64,
    pub session: Session,
    pub playback: Playback,
    pub device: ActiveDevice,
}

/// The Spotify session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Session {
    #[default]
    Starting,
    /// Waiting for the user to approve this device on another screen.
    Pairing {
        url: String,
        code: String,
        /// Unix time in milliseconds when the code expires.
        expires_at_ms: u64,
    },
    Connecting,
    Ready,
    Failed {
        kind: FailureKind,
        message: String,
        /// Unix time in milliseconds of the next automatic retry, if any.
        #[serde(default)]
        retry_at_ms: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Network,
    Auth,
    Audio,
    Spotify,
    Internal,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playback {
    pub state: PlayState,
    pub track: Option<Track>,
    /// Where the track plays from, when known.
    pub context: Option<PlaybackContext>,
    pub position_ms: u32,
    /// Unix time in milliseconds when `position_ms` was sampled.
    pub position_at_ms: u64,
    /// 0 to 100.
    pub volume: u8,
    pub shuffle: bool,
}

impl Playback {
    /// The position at `now_ms`, advanced while playing and capped at the
    /// track length.
    pub fn position_at(&self, now_ms: u64) -> u32 {
        let mut position = u64::from(self.position_ms);
        if self.state == PlayState::Playing {
            position += now_ms.saturating_sub(self.position_at_ms);
        }
        if let Some(track) = &self.track
            && track.duration_ms > 0
        {
            position = position.min(u64::from(track.duration_ms));
        }
        u32::try_from(position).unwrap_or(u32::MAX)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayState {
    #[default]
    Stopped,
    /// Loading or waiting for data; about to play.
    Buffering,
    Playing,
    Paused,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    pub uri: String,
    pub title: String,
    pub artists: String,
    pub album: String,
    pub duration_ms: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackContext {
    pub uri: String,
    /// `None` until the name is known.
    pub name: Option<String>,
}

/// Which Spotify Connect device plays.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActiveDevice {
    #[default]
    None,
    /// This handheld.
    Local,
    /// Another device, by its Connect name.
    Remote { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_position_advances_only_while_playing_and_stops_at_the_end() {
        let mut playback = Playback {
            state: PlayState::Paused,
            track: Some(Track {
                duration_ms: 10_000,
                ..Track::default()
            }),
            position_ms: 1_000,
            position_at_ms: 5_000,
            ..Playback::default()
        };
        assert_eq!(playback.position_at(6_500), 1_000);
        playback.state = PlayState::Playing;
        assert_eq!(playback.position_at(6_500), 2_500);
        assert_eq!(playback.position_at(60_000), 10_000);
    }

    #[test]
    fn missing_fields_decode_to_defaults() {
        let snapshot: Snapshot = serde_json::from_str(r#"{"revision":7}"#).unwrap();
        assert_eq!(snapshot.revision, 7);
        assert_eq!(snapshot.session, Session::Starting);
        assert_eq!(snapshot.device, ActiveDevice::None);
    }
}
