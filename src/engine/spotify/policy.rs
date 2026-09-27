//! Decisions the Spotify engine makes, free of librespot types so they are
//! tested on any host.

use crate::protocol::FailureKind;
use std::time::Duration;

/// How long a pairing code is shown. librespot does not report Spotify's
/// own expiry, so this is our bound; Spotify's codes last about as long.
pub const PAIRING_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// Delay before automatic retry number `attempt` (0-based): 2 s, 5 s, 15 s,
/// then every 30 s.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(match attempt {
        0 => 2,
        1 => 5,
        2 => 15,
        _ => 30,
    })
}

/// Why polling for a pairing token failed. librespot flattens the OAuth
/// error into text (`OAuthError::ExchangeDeviceCode { e: String }`), so the
/// text is all there is to go on. It is inspected here and never logged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingError {
    /// The user declined on Spotify's page.
    Declined,
    /// The code expired before it was approved.
    Expired,
    /// Anything else, most likely the network.
    Other,
}

pub fn classify_pairing_error(text: &str) -> PairingError {
    let text = text.to_ascii_lowercase();
    if text.contains("access_denied") || text.contains("denied") {
        PairingError::Declined
    } else if text.contains("expired_token") || text.contains("expire") {
        PairingError::Expired
    } else {
        PairingError::Other
    }
}

/// What happens when a pairing code expires: the first expiry of a round
/// shows a new code by itself; after that the user has to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterExpiry {
    NewCode,
    WaitForUser,
}

/// Tracks automatic code refreshes within one pairing round.
#[derive(Clone, Copy, Debug, Default)]
pub struct PairingRound {
    refreshed: bool,
}

impl PairingRound {
    pub fn expired(&mut self) -> AfterExpiry {
        if std::mem::replace(&mut self.refreshed, true) {
            AfterExpiry::WaitForUser
        } else {
            AfterExpiry::NewCode
        }
    }
}

/// How signing in with a login failed, from librespot's error kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectFailure {
    /// Spotify rejected the login: pairing is needed.
    LoginRejected,
    /// Worth retrying later with the same login.
    Retry(FailureKind),
}

/// Map librespot's error kind names (see `librespot_core::error::ErrorKind`)
/// to what the engine does next.
pub fn classify_connect_error(kind: librespot_core::error::ErrorKind) -> ConnectFailure {
    use librespot_core::error::ErrorKind;
    match kind {
        ErrorKind::PermissionDenied | ErrorKind::Unauthenticated => ConnectFailure::LoginRejected,
        ErrorKind::Unavailable
        | ErrorKind::DeadlineExceeded
        | ErrorKind::Aborted
        | ErrorKind::Cancelled
        | ErrorKind::ResourceExhausted => ConnectFailure::Retry(FailureKind::Network),
        _ => ConnectFailure::Retry(FailureKind::Spotify),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::error::ErrorKind;

    #[test]
    fn backoff_grows_then_settles() {
        let secs: Vec<u64> = (0..6).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [2, 5, 15, 30, 30, 30]);
    }

    #[test]
    fn pairing_errors_are_classified_by_category() {
        assert_eq!(
            classify_pairing_error("Server returned error response: access_denied"),
            PairingError::Declined
        );
        assert_eq!(
            classify_pairing_error("Server returned error response: expired_token"),
            PairingError::Expired
        );
        assert_eq!(
            classify_pairing_error("Request failed: error sending request"),
            PairingError::Other
        );
    }

    #[test]
    fn an_expired_code_is_replaced_once_per_round() {
        let mut round = PairingRound::default();
        assert_eq!(round.expired(), AfterExpiry::NewCode);
        assert_eq!(round.expired(), AfterExpiry::WaitForUser);
        assert_eq!(round.expired(), AfterExpiry::WaitForUser);
        let mut next = PairingRound::default();
        assert_eq!(next.expired(), AfterExpiry::NewCode);
    }

    #[test]
    fn a_rejected_login_needs_pairing_and_the_rest_is_retried() {
        for kind in [ErrorKind::PermissionDenied, ErrorKind::Unauthenticated] {
            assert_eq!(classify_connect_error(kind), ConnectFailure::LoginRejected);
        }
        assert_eq!(
            classify_connect_error(ErrorKind::Unavailable),
            ConnectFailure::Retry(FailureKind::Network)
        );
        assert_eq!(
            classify_connect_error(ErrorKind::Internal),
            ConnectFailure::Retry(FailureKind::Spotify)
        );
    }
}
