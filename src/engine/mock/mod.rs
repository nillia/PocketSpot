//! A fake playback engine: fictional music, no account, no network, no
//! audio. It follows the real engine's timelines (pairing, connecting,
//! reconnecting after a network loss) so clients can be built and tested
//! against it.
//!
//! [`MockCore`] is the state machine, driven with explicit times; [`spawn`]
//! runs it on its own thread against the real clock.

mod catalog;
mod core;

pub use core::{Fault, MockCore, MockOptions};

use super::{EngineHandle, Message, Published};
use std::{
    io,
    sync::mpsc::{Receiver, RecvTimeoutError},
    time::Duration,
};

/// URI of the mock's Liked Songs.
pub const LIKED_SONGS: &str = catalog::LIKED_SONGS;

/// The mock's playlists as (URI, name).
pub fn playlists() -> impl Iterator<Item = (&'static str, &'static str)> {
    catalog::playlists()
}

/// Start the mock engine on its own thread.
pub fn spawn(options: MockOptions, published: Published) -> io::Result<EngineHandle> {
    let (tx, rx) = EngineHandle::channel();
    let publisher = published.clone();
    let thread = std::thread::Builder::new()
        .name("pocketspot-mock".into())
        .spawn(move || run(MockCore::new(options, now_ms()), &rx, &publisher))?;
    Ok(EngineHandle::new(tx, published, thread))
}

fn run(mut core: MockCore, rx: &Receiver<Message>, published: &Published) {
    loop {
        let now = now_ms();
        core.advance(now);
        published.publish(core.state());
        // Sleep until the next timer or track end, or a message.
        let wait = core
            .next_deadline()
            .map_or(Duration::from_secs(3600), |at| {
                Duration::from_millis(at.saturating_sub(now))
            });
        let message = match rx.recv_timeout(wait) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let now = now_ms();
        match message {
            Message::Command(command, reply) => {
                let result = core.command(command, now);
                published.publish(core.state());
                let _ = reply.send(result);
            }
            Message::Pair(reply) => {
                let result = core.pair(now);
                published.publish(core.state());
                let _ = reply.send(result);
            }
            Message::Logout(reply) => {
                core.logout(now);
                published.publish(core.state());
                let _ = reply.send(Ok(()));
            }
            Message::Inject(fault) => core.inject(fault, now),
            Message::Stop => return,
        }
    }
}

fn now_ms() -> u64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Command, PlayState, Session};

    #[test]
    fn the_thread_applies_commands_and_publishes_each_change() {
        let published = Published::new(0);
        let options = MockOptions {
            signed_in: true,
            ..MockOptions::default()
        };
        let engine = spawn(options, published).unwrap();
        let (playlist, _) = playlists().next().unwrap();
        let before = engine.snapshot().revision;
        engine
            .command(Command::Play {
                context_uri: playlist.into(),
                track_uri: None,
            })
            .unwrap();
        let snapshot = engine.snapshot();
        assert_eq!(snapshot.session, Session::Ready);
        assert_eq!(snapshot.playback.state, PlayState::Playing);
        assert!(snapshot.revision > before);
        engine.logout().unwrap();
        assert!(matches!(engine.snapshot().session, Session::Pairing { .. }));
        engine.stop();
    }
}
