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

/// The Liked Songs URI of an account, if the username is safe to embed.
pub fn liked_songs_uri(username: &str) -> Option<String> {
    let safe = !username.is_empty()
        && username.len() <= 64
        && username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    safe.then(|| format!("spotify:user:{username}:collection"))
}

/// A Spotify id: 22 letters and digits.
fn spotify_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Check what `play` may start: a playlist, an album or the account's
/// Liked Songs, optionally from one of its tracks. Anything else is refused
/// before it reaches Spotify.
pub fn validate_play(
    context_uri: &str,
    track_uri: Option<&str>,
    liked_songs: Option<&str>,
) -> Result<(), &'static str> {
    let context_ok = liked_songs == Some(context_uri)
        || ["spotify:playlist:", "spotify:album:"]
            .iter()
            .any(|prefix| context_uri.strip_prefix(prefix).is_some_and(spotify_id));
    if !context_ok {
        return Err("not a playlist, album or Liked Songs");
    }
    match track_uri {
        Some(uri) if !uri.strip_prefix("spotify:track:").is_some_and(spotify_id) => {
            Err("not a track")
        }
        _ => Ok(()),
    }
}

/// librespot's 16-bit volume as a percentage, and back.
pub fn volume_to_percent(volume: u16) -> u8 {
    let percent = (u32::from(volume) * 100 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX);
    u8::try_from(percent.min(100)).unwrap_or(100)
}

pub fn percent_to_volume(percent: u8) -> u16 {
    let volume = u32::from(percent.min(100)) * u32::from(u16::MAX) / 100;
    u16::try_from(volume).unwrap_or(u16::MAX)
}

/// A playlist owner as shown to people: "Spotify" for Spotify's own
/// playlists, readable usernames as they are, opaque account ids hidden.
pub fn display_owner(owner: &str) -> Option<String> {
    let owner = owner.trim();
    if owner == "spotify" {
        return Some("Spotify".into());
    }
    let all_digits = !owner.is_empty() && owner.bytes().all(|b| b.is_ascii_digit());
    let opaque = owner.len() >= 20
        && owner.bytes().any(|b| b.is_ascii_digit())
        && owner
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    (!owner.is_empty() && !all_digits && !opaque).then(|| owner.chars().take(60).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::error::ErrorKind;

    const LIKED: &str = "spotify:user:nico:collection";

    #[test]
    fn only_playlists_albums_and_the_own_liked_songs_can_be_played() {
        let playlist = "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M";
        let track = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
        assert_eq!(validate_play(playlist, None, Some(LIKED)), Ok(()));
        assert_eq!(validate_play(playlist, Some(track), None), Ok(()));
        assert_eq!(
            validate_play("spotify:album:37i9dQZF1DXcBWIGoYBM5M", None, None),
            Ok(())
        );
        assert_eq!(validate_play(LIKED, None, Some(LIKED)), Ok(()));
        for context in [
            "spotify:user:other:collection",
            "spotify:show:37i9dQZF1DXcBWIGoYBM5M",
            "spotify:playlist:short",
            "spotify:playlist:$(rm -rf /)aaaaaaaaaaaaa",
        ] {
            assert!(
                validate_play(context, None, Some(LIKED)).is_err(),
                "{context}"
            );
        }
        assert!(
            validate_play(playlist, Some("spotify:album:37i9dQZF1DXcBWIGoYBM5M"), None).is_err()
        );
    }

    #[test]
    fn liked_songs_needs_a_plain_username() {
        assert_eq!(
            liked_songs_uri("nico.i"),
            Some("spotify:user:nico.i:collection".into())
        );
        assert_eq!(liked_songs_uri("a:b"), None);
        assert_eq!(liked_songs_uri(""), None);
    }

    #[test]
    fn volume_round_trips_through_percent() {
        for percent in [0, 1, 45, 99, 100] {
            assert_eq!(volume_to_percent(percent_to_volume(percent)), percent);
        }
        assert_eq!(volume_to_percent(u16::MAX), 100);
        assert_eq!(percent_to_volume(200), u16::MAX);
    }

    #[test]
    fn owners_hide_opaque_account_ids() {
        assert_eq!(display_owner("spotify").as_deref(), Some("Spotify"));
        assert_eq!(display_owner("vgallotti").as_deref(), Some("vgallotti"));
        assert_eq!(display_owner("tail398iotvplvxde1b2fvs6i"), None);
        assert_eq!(display_owner("1165108818"), None);
        assert_eq!(display_owner("dj2000").as_deref(), Some("dj2000"));
        assert_eq!(display_owner(""), None);
    }

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
