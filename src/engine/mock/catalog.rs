//! A small fictional catalog. Names are made up; URIs have Spotify's shape
//! so they pass the same validation as real ones.

use crate::protocol::Track;

pub(super) const LIKED_SONGS: &str = "spotify:user:pocketspot-demo:collection";

struct DemoTrack {
    title: &'static str,
    artist: &'static str,
    album: &'static str,
    seconds: u32,
}

const TRACKS: [DemoTrack; 9] = [
    DemoTrack {
        title: "Northbound",
        artist: "Slow Orbit",
        album: "Northbound",
        seconds: 214,
    },
    DemoTrack {
        title: "Satellite Hearts",
        artist: "Slow Orbit",
        album: "Northbound",
        seconds: 188,
    },
    DemoTrack {
        title: "Paper Lanterns",
        artist: "Mira Vale",
        album: "Paper Lanterns",
        seconds: 201,
    },
    DemoTrack {
        title: "Kite Weather",
        artist: "Mira Vale",
        album: "Paper Lanterns",
        seconds: 176,
    },
    DemoTrack {
        title: "Night Signals",
        artist: "The Quiet Hours",
        album: "Night Signals",
        seconds: 256,
    },
    DemoTrack {
        title: "3 AM Radio",
        artist: "The Quiet Hours",
        album: "Night Signals",
        seconds: 84,
    },
    DemoTrack {
        title: "Low Tide",
        artist: "Harbor Lights",
        album: "Low Tide",
        seconds: 232,
    },
    DemoTrack {
        title: "Glass City",
        artist: "Neon Fields",
        album: "Glass City",
        seconds: 219,
    },
    DemoTrack {
        title: "Blue Hour Transit",
        artist: "Neon Fields",
        album: "Glass City",
        seconds: 247,
    },
];

/// (URI, name, track indices)
const PLAYLISTS: [(&str, &str, &[usize]); 3] = [
    (
        "spotify:playlist:PocketSpotDemoList0001",
        "Late-night focus",
        &[4, 5, 0, 6, 8],
    ),
    (
        "spotify:playlist:PocketSpotDemoList0002",
        "Morning run",
        &[7, 1, 3, 2],
    ),
    (
        "spotify:playlist:PocketSpotDemoList0003",
        "Handheld hits",
        &[0, 2, 4, 6, 7],
    ),
];

pub(super) fn track(index: usize) -> Track {
    let demo = &TRACKS[index];
    Track {
        uri: format!("spotify:track:PocketSpotDemoTrack{index:03}"),
        title: demo.title.into(),
        artists: demo.artist.into(),
        album: demo.album.into(),
        duration_ms: demo.seconds * 1000,
    }
}

/// The name and track list of a playable context, if it exists.
pub(super) fn context(uri: &str) -> Option<(&'static str, Vec<usize>)> {
    if uri == LIKED_SONGS {
        return Some(("Liked Songs", (0..TRACKS.len()).collect()));
    }
    PLAYLISTS
        .iter()
        .find(|(playlist, _, _)| *playlist == uri)
        .map(|(_, name, tracks)| (*name, tracks.to_vec()))
}

/// The library: Liked Songs, then the playlists.
pub(super) fn library() -> Vec<crate::protocol::LibraryItem> {
    use crate::protocol::{LibraryItem, LibraryKind};
    let liked = LibraryItem {
        kind: LibraryKind::LikedSongs,
        uri: LIKED_SONGS.into(),
        name: "Liked Songs".into(),
        owner: None,
        tracks: u32::try_from(TRACKS.len()).ok(),
    };
    let playlists = PLAYLISTS.iter().map(|(uri, name, tracks)| LibraryItem {
        kind: LibraryKind::Playlist,
        uri: (*uri).into(),
        name: (*name).into(),
        owner: Some("PocketSpot".into()),
        tracks: u32::try_from(tracks.len()).ok(),
    });
    std::iter::once(liked).chain(playlists).collect()
}

/// Every playlist URI and name, for tests and development.
pub(super) fn playlists() -> impl Iterator<Item = (&'static str, &'static str)> {
    PLAYLISTS.iter().map(|(uri, name, _)| (*uri, *name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_have_spotify_ids_of_the_right_shape() {
        for (uri, _) in playlists() {
            let id = uri.rsplit(':').next().unwrap();
            assert_eq!(id.len(), 22, "{uri}");
        }
        let id = track(3).uri.rsplit(':').next().unwrap().to_owned();
        assert_eq!(id.len(), 22);
        assert!(context("spotify:playlist:unknown").is_none());
    }
}
