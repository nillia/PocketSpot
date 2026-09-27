//! Requests, replies and playback commands.

use super::{LOGOUT_REPLY_WITHIN, REPLY_WITHIN, Snapshot};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Extra time a client waits on top of the service's bound for a request.
pub const CLIENT_MARGIN: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Who is listening? Stable in every protocol version.
    Identify,
    /// The current state. With `since`, a service whose revision still
    /// equals it answers [`Response::Unchanged`].
    Snapshot {
        #[serde(default)]
        since: Option<u64>,
    },
    /// Control playback.
    Command { command: Command },
    /// Remove the saved login and return to pairing.
    Logout,
    /// Stop playback and the service. Stable in every protocol version.
    Shutdown,
}

impl Request {
    /// How long a client waits for the reply: the service's bound for this
    /// request plus [`CLIENT_MARGIN`], so a slow success is never reported
    /// as a failure.
    pub fn client_timeout(&self) -> Duration {
        let bound = match self {
            Request::Logout => LOGOUT_REPLY_WITHIN,
            _ => REPLY_WITHIN,
        };
        bound + CLIENT_MARGIN
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Start a playlist, album or Liked Songs; with `track_uri`, start at
    /// that track within the context.
    Play {
        context_uri: String,
        #[serde(default)]
        track_uri: Option<String>,
    },
    Pause,
    Resume,
    Next,
    Previous,
    /// Volume in percent, 0 to 100.
    SetVolume {
        percent: u8,
    },
    SetShuffle {
        enabled: bool,
    },
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Reply to [`Request::Identify`]. Stable in every protocol version.
    Identity {
        server: String,
        version: String,
        pid: u32,
        protocol: u32,
    },
    Snapshot {
        snapshot: Box<Snapshot>,
    },
    /// Reply to `Snapshot { since }` when nothing changed.
    Unchanged {
        revision: u64,
    },
    Accepted,
    Rejected {
        reason: Reject,
        message: String,
    },
    /// The request used a protocol version this service does not speak.
    VersionMismatch {
        supported: u32,
    },
}

impl Response {
    pub fn rejected(reason: Reject, message: impl Into<String>) -> Self {
        Response::Rejected {
            reason,
            message: message.into(),
        }
    }
}

/// Why a request was not carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reject {
    /// Too many requests at once; try again.
    Busy,
    /// Not signed in or not connected yet.
    NotReady,
    /// The request was understood but its content is invalid.
    Invalid,
    /// This service cannot do that (yet).
    Unavailable,
    /// The request could not be parsed.
    Malformed,
}
