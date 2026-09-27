//! The local control protocol between clients and the service.
//!
//! A client connects to the service's Unix socket in the private runtime
//! directory, writes one request as a JSON line, reads one reply line and
//! closes. See `docs/protocol.md` for the full description.

pub mod client;
mod messages;
mod state;
mod wire;

pub use messages::{CLIENT_MARGIN, Command, Reject, Request, Response};
pub use state::{
    ActiveDevice, FailureKind, Library, LibraryItem, LibraryKind, LoadState, PlayState, Playback,
    PlaybackContext, Session, Snapshot, Track,
};
pub(crate) use wire::{Incoming, ResponseEnvelope, decode_request, read_message, write_message};

use std::time::Duration;

/// Bump on any incompatible change. `identify` and `shutdown` keep their
/// wire shape in every version (the stable handshake).
pub const PROTOCOL_VERSION: u32 = 1;
/// Upper bound for one request or reply line.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// The service answers every request except `logout` within this bound.
pub const REPLY_WITHIN: Duration = Duration::from_secs(2);
/// The service answers `logout` within this bound.
pub const LOGOUT_REPLY_WITHIN: Duration = Duration::from_secs(6);
/// Name of the control socket in the runtime directory.
pub const SOCKET_FILE: &str = "control.sock";
/// Name the service reports in its identity.
pub const SERVICE_NAME: &str = "pocketspotd";
