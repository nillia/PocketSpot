//! Playback engines and the interface the service uses to drive them.
//!
//! An engine runs on its own thread and owns all playback state; nothing
//! else changes it. The service talks to it through an [`EngineHandle`]:
//! commands go in over a bounded channel and are answered on a reply
//! channel, and the engine publishes each new state as a [`Snapshot`] that
//! readers take without waiting for the engine.

#[cfg(feature = "mock")]
pub mod mock;

use crate::protocol::{Command, LOGOUT_REPLY_WITHIN, REPLY_WITHIN, Reject, Snapshot};
use std::{
    sync::{
        Arc, Mutex, MutexGuard,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::JoinHandle,
    time::Duration,
};

/// Commands waiting for the engine beyond this are answered as busy.
pub const QUEUE: usize = 32;

/// Why the engine did not carry out a request: a reason for programs and
/// a message for people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: Reject,
    pub message: String,
}

impl Refusal {
    pub fn new(reason: Reject, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }

    fn busy() -> Self {
        Self::new(Reject::Busy, "the playback engine is busy")
    }
}

pub type Reply = SyncSender<Result<(), Refusal>>;

/// What the service sends to the engine thread.
pub enum Message {
    Command(Command, Reply),
    Logout(Reply),
    /// Replay a failure (mock engine only, for tests).
    #[cfg(feature = "mock")]
    Inject(mock::Fault),
    Stop,
}

/// The latest published state, shared by the engine and its readers.
///
/// The engine publishes a whole new state; the revision is bumped only if
/// it differs from the previous one, so readers polling with `since` see a
/// change exactly when there is one.
#[derive(Clone)]
pub struct Published(Arc<Mutex<Arc<Snapshot>>>);

impl Published {
    pub fn new(first_revision: u64) -> Self {
        Self(Arc::new(Mutex::new(Arc::new(Snapshot {
            revision: first_revision,
            ..Snapshot::default()
        }))))
    }

    fn lock(&self) -> MutexGuard<'_, Arc<Snapshot>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The current snapshot; cheap (a reference count).
    pub fn get(&self) -> Arc<Snapshot> {
        Arc::clone(&self.lock())
    }

    /// Publish `state` (its revision is ignored) if it changed anything.
    pub fn publish(&self, state: &Snapshot) {
        let mut current = self.lock();
        let unchanged = Snapshot {
            revision: current.revision,
            ..state.clone()
        };
        if unchanged != **current {
            let next = Snapshot {
                revision: current.revision.wrapping_add(1),
                ..unchanged
            };
            *current = Arc::new(next);
        }
    }
}

/// The service's side of a running engine. It is shared with the threads
/// answering clients, so [`stop`](Self::stop) takes `&self` and the thread
/// handle sits in a `Mutex<Option<_>>` to be taken once.
pub struct EngineHandle {
    tx: SyncSender<Message>,
    published: Published,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl EngineHandle {
    /// Wrap an engine thread that reads `rx` and publishes to `published`.
    pub fn new(tx: SyncSender<Message>, published: Published, thread: JoinHandle<()>) -> Self {
        Self {
            tx,
            published,
            thread: Mutex::new(Some(thread)),
        }
    }

    /// A channel pair sized for engines.
    pub fn channel() -> (SyncSender<Message>, Receiver<Message>) {
        mpsc::sync_channel(QUEUE)
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.published.get()
    }

    /// Carry out a command, waiting at most the protocol's reply bound.
    pub fn command(&self, command: Command) -> Result<(), Refusal> {
        self.ask(|reply| Message::Command(command, reply), REPLY_WITHIN)
    }

    /// Remove the saved login, waiting at most the protocol's logout bound.
    pub fn logout(&self) -> Result<(), Refusal> {
        self.ask(Message::Logout, LOGOUT_REPLY_WITHIN)
    }

    fn ask(&self, message: impl FnOnce(Reply) -> Message, within: Duration) -> Result<(), Refusal> {
        let (reply, answer) = mpsc::sync_channel(1);
        match self.tx.try_send(message(reply)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(Refusal::busy()),
            Err(TrySendError::Disconnected(_)) => {
                return Err(Refusal::new(
                    Reject::Unavailable,
                    "the playback engine stopped",
                ));
            }
        }
        answer
            .recv_timeout(within)
            .unwrap_or_else(|_| Err(Refusal::busy()))
    }

    /// Replay a failure in the mock engine.
    #[cfg(feature = "mock")]
    pub fn inject(&self, fault: mock::Fault) -> Result<(), Refusal> {
        self.tx
            .try_send(Message::Inject(fault))
            .map_err(|_| Refusal::busy())
    }

    /// Stop the engine and wait for its thread. Later calls do nothing.
    pub fn stop(&self) {
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(thread) = thread else {
            return;
        };
        let _ = self.tx.send(Message::Stop);
        if thread.join().is_err() {
            log::error!("the playback engine thread panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Session;

    #[test]
    fn the_revision_changes_only_when_the_state_does() {
        let published = Published::new(100);
        let mut state = Snapshot::default();
        published.publish(&state);
        assert_eq!(published.get().revision, 100, "nothing changed");
        state.session = Session::Ready;
        published.publish(&state);
        assert_eq!(published.get().revision, 101);
        // The revision a caller passes in is ignored.
        state.revision = 7;
        published.publish(&state);
        assert_eq!(published.get().revision, 101);
    }

    #[test]
    fn a_full_queue_is_busy_and_a_stopped_engine_is_unavailable() {
        let (tx, rx) = mpsc::sync_channel(1);
        let published = Published::new(0);
        // An engine that never reads: the one queue slot fills up.
        let thread = std::thread::spawn(|| {});
        let handle = EngineHandle::new(tx, published, thread);
        let (reply, _answer) = mpsc::sync_channel(1);
        handle.tx.try_send(Message::Logout(reply)).unwrap();
        assert_eq!(
            handle.command(Command::Next).unwrap_err().reason,
            Reject::Busy
        );
        drop(rx);
        assert_eq!(
            handle.command(Command::Next).unwrap_err().reason,
            Reject::Unavailable
        );
    }
}
