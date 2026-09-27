//! librespot's player: building it with the platform's audio output, and
//! reducing its events to [`playback::Event`](super::playback::Event).

use super::{playback::Event, policy::volume_to_percent};
use crate::protocol::Track;
use librespot_core::session::Session;
use librespot_metadata::audio::{AudioItem, UniqueFields};
use librespot_playback::{
    audio_backend,
    config::{AudioFormat, Bitrate, PlayerConfig},
    mixer::{self, Mixer, MixerConfig},
    player::{Player, PlayerEvent},
};
use std::{sync::Arc, time::Duration};

/// The audio output: ALSA's default device on Linux (the handheld). Other
/// systems are development hosts; their builds have no audio backend, so
/// the player writes to `/dev/null` and plays silently.
#[cfg(target_os = "linux")]
const OUTPUT: (&str, Option<&str>) = ("alsa", None);
#[cfg(not(target_os = "linux"))]
const OUTPUT: (&str, Option<&str>) = ("pipe", Some("/dev/null"));

/// Build the software mixer and the player.
pub fn build(session: &Session) -> Result<(Arc<Player>, Arc<dyn Mixer>), &'static str> {
    let mixer_fn = mixer::find(None).ok_or("no volume control available")?;
    let mixer = mixer_fn(MixerConfig::default()).map_err(|_| "volume control could not start")?;
    let (backend, device) = OUTPUT;
    let sink_builder =
        audio_backend::find(Some(backend.to_owned())).ok_or("audio output unavailable")?;
    let config = PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        gapless: true,
        position_update_interval: Some(Duration::from_secs(1)),
        ..PlayerConfig::default()
    };
    let device = device.map(str::to_owned);
    let player = Player::new(
        config,
        session.clone(),
        mixer.get_soft_volume(),
        move || sink_builder(device, AudioFormat::default()),
    );
    Ok((player, mixer))
}

/// The part of a player event the snapshot shows, if any.
pub fn reduce(event: PlayerEvent) -> Option<Event> {
    Some(match event {
        PlayerEvent::TrackChanged { audio_item } => Event::TrackChanged(track(&audio_item)),
        PlayerEvent::Loading { position_ms, .. } => Event::Loading { position_ms },
        PlayerEvent::Playing { position_ms, .. } => Event::Playing { position_ms },
        PlayerEvent::Paused { position_ms, .. } => Event::Paused { position_ms },
        PlayerEvent::Stopped { .. } => Event::Stopped,
        PlayerEvent::PositionChanged { position_ms, .. }
        | PlayerEvent::PositionCorrection { position_ms, .. }
        | PlayerEvent::Seeked { position_ms, .. } => Event::Position { position_ms },
        PlayerEvent::VolumeChanged { volume } => Event::Volume {
            percent: volume_to_percent(volume),
        },
        PlayerEvent::ShuffleChanged { shuffle } => Event::Shuffle { enabled: shuffle },
        PlayerEvent::SetQueue { context_uri, .. } => Event::Context { uri: context_uri },
        _ => return None,
    })
}

fn track(item: &AudioItem) -> Track {
    let (artists, album) = match &item.unique_fields {
        UniqueFields::Track { artists, album, .. } => (
            artists
                .iter()
                .map(|artist| artist.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            album.clone(),
        ),
        UniqueFields::Episode { show_name, .. } => (show_name.clone(), String::new()),
        UniqueFields::Local { artists, album, .. } => (
            artists.clone().unwrap_or_default(),
            album.clone().unwrap_or_default(),
        ),
    };
    Track {
        uri: bounded(&item.uri),
        title: bounded(&item.name),
        artists: bounded(&artists),
        album: bounded(&album),
        duration_ms: item.duration_ms,
    }
}

/// Text from Spotify, cut to a sane length and without control characters.
pub fn bounded(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect()
}
