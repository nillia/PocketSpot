//! The account's library: its playlists (the "rootlist") plus Liked Songs.

use super::{player::bounded, policy::display_owner};
use crate::protocol::{LibraryItem, LibraryKind};
use librespot_core::session::Session;
use librespot_protocol::playlist4_external::SelectedListContent;
use protobuf::Message as _;

/// Playlists fetched at most.
const LIMIT: usize = 200;

/// Fetch the library. Errors are short messages for the user.
pub async fn fetch(
    session: &Session,
    liked_songs: Option<&str>,
) -> Result<Vec<LibraryItem>, &'static str> {
    let bytes = session
        .spclient()
        .get_rootlist(0, Some(LIMIT))
        .await
        .map_err(|_| "Playlists are unavailable")?;
    parse(&bytes, liked_songs)
}

/// Liked Songs first, then the playlists in the account's own order.
/// Folder markers and anything that is not a playlist are skipped.
fn parse(bytes: &[u8], liked_songs: Option<&str>) -> Result<Vec<LibraryItem>, &'static str> {
    let list =
        SelectedListContent::parse_from_bytes(bytes).map_err(|_| "Playlists could not be read")?;
    let contents = list.contents.get_or_default();
    let liked = liked_songs.map(|uri| LibraryItem {
        kind: LibraryKind::LikedSongs,
        uri: uri.to_owned(),
        name: "Liked Songs".into(),
        owner: None,
        tracks: None,
    });
    let playlists = contents
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.uri()
                .strip_prefix("spotify:playlist:")
                .is_some_and(|id| id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
        })
        .map(|(index, item)| {
            let meta = contents.meta_items.get(index);
            let name = meta
                .map(|m| m.attributes.get_or_default().name())
                .filter(|name| !name.is_empty())
                .map_or_else(|| "Untitled playlist".to_owned(), bounded);
            LibraryItem {
                kind: LibraryKind::Playlist,
                uri: item.uri().to_owned(),
                name,
                owner: meta.and_then(|m| display_owner(m.owner_username())),
                tracks: meta.and_then(|m| u32::try_from(m.length()).ok()),
            }
        });
    Ok(liked.into_iter().chain(playlists).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_protocol::playlist4_external::{Item, ListItems, MetaItem};

    fn rootlist() -> Vec<u8> {
        let mut contents = ListItems::new();
        // Required fields in Spotify's (proto2) message.
        contents.set_pos(0);
        contents.set_truncated(false);
        for (uri, name, owner, length) in [
            (
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
                "Focus",
                "spotify",
                50,
            ),
            ("spotify:start-group:abc:Folder", "", "", 0),
            (
                "spotify:playlist:5c6gat4fnoh36l24t162nj",
                "Road trip",
                "tail398iotvplvxde1b2fvs6i",
                12,
            ),
        ] {
            let mut item = Item::new();
            item.set_uri(uri.into());
            contents.items.push(item);
            let mut meta = MetaItem::new();
            meta.attributes
                .mut_or_insert_default()
                .set_name(name.into());
            meta.set_owner_username(owner.into());
            meta.set_length(length);
            contents.meta_items.push(meta);
        }
        let mut list = SelectedListContent::new();
        list.contents = protobuf::MessageField::some(contents);
        list.write_to_bytes().unwrap()
    }

    #[test]
    fn liked_songs_come_first_then_playlists_without_folders() {
        let items = parse(&rootlist(), Some("spotify:user:nico:collection")).unwrap();
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Liked Songs", "Focus", "Road trip"]);
        assert_eq!(items[0].kind, LibraryKind::LikedSongs);
        assert_eq!(items[1].owner.as_deref(), Some("Spotify"));
        assert_eq!(items[1].tracks, Some(50));
        assert_eq!(items[2].owner, None, "opaque owner ids are hidden");
    }

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        assert!(parse(b"\xff\xff not protobuf", None).is_err());
        assert!(parse(&[], None).unwrap().is_empty());
    }
}
