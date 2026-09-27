//! The Spotify engine: signs in through librespot with Spotify's device-code
//! flow and keeps the session. Playback commands arrive with a later
//! release.
//!
//! librespot is asynchronous, so the engine thread runs a single-threaded
//! tokio runtime, and a small bridge thread forwards the service's messages
//! into it. The engine moves through phases (pairing, connecting, ready,
//! waiting to retry); each phase waits on its librespot future and on the
//! service's messages at the same time, so logout, stop and `pair` are
//! answered promptly in every phase.
//!
//! Pairing codes, URLs and tokens are never logged; librespot's error text
//! is only inspected, never written out.

mod policy;
mod store;

use super::{EngineHandle, Message, Published, QUEUE, Refusal, Reply};
use crate::{
    platform::ReadyProfile,
    protocol::{FailureKind, Reject, Session, Snapshot},
};
use librespot_core::{authentication::Credentials, cache::Cache, config::SessionConfig};
use librespot_oauth::DeviceAuthClientBuilder;
use policy::{
    AfterExpiry, ConnectFailure, PAIRING_LIFETIME, PairingError, PairingRound, backoff,
    classify_connect_error, classify_pairing_error,
};
use std::{io, sync::mpsc as std_mpsc, time::Duration};
use store::Store;
use tokio::{
    sync::mpsc,
    time::{Instant, sleep, sleep_until},
};

/// Directory for librespot's files inside the state directory.
const STATE_SUBDIR: &str = "spotify";
/// How often a ready session is checked for a lost connection.
const HEALTH_CHECK: Duration = Duration::from_secs(5);
/// The OAuth scope needed to stream.
const SCOPES: [&str; 1] = ["streaming"];

/// Start the Spotify engine on its own thread.
pub fn spawn(profile: &ReadyProfile, published: Published) -> io::Result<EngineHandle> {
    let dir = profile
        .private_state_subdir(STATE_SUBDIR)
        .map_err(io::Error::other)?;
    let store = Store::new(dir);
    let device_id = store.device_id(|| SessionConfig::default().device_id)?;
    let config = SessionConfig {
        device_id,
        tmp_dir: profile.runtime_dir().to_owned(),
        ..SessionConfig::default()
    };
    let (tx, rx) = EngineHandle::channel();
    let publisher = published.clone();
    let thread = std::thread::Builder::new()
        .name("pocketspot-spotify".into())
        .spawn(move || run(rx, store, config, publisher))?;
    Ok(EngineHandle::new(tx, published, thread))
}

/// The engine thread: a runtime for librespot, fed by a bridge thread.
fn run(rx: std_mpsc::Receiver<Message>, store: Store, config: SessionConfig, published: Published) {
    let (async_tx, async_rx) = mpsc::channel(QUEUE);
    let bridge = std::thread::Builder::new()
        .name("pocketspot-spotify-bridge".into())
        .spawn(move || {
            while let Ok(message) = rx.recv() {
                let stop = matches!(message, Message::Stop);
                if async_tx.blocking_send(message).is_err() || stop {
                    return;
                }
            }
        });
    let failed = |message: &str| {
        log::error!("{message}");
        published.publish(&Snapshot {
            session: Session::Failed {
                kind: FailureKind::Internal,
                message: message.into(),
                retry_at_ms: None,
            },
            ..Snapshot::default()
        });
    };
    if bridge.is_err() {
        return failed("the Spotify engine could not start");
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return failed("the Spotify engine could not start"),
    };
    let cache = match Cache::new(Some(store.dir()), None, None, None) {
        Ok(cache) => cache,
        Err(_) => return failed("the saved login cannot be read"),
    };
    runtime.block_on(async move {
        let driver = Driver {
            rx: async_rx,
            published,
            state: Snapshot::default(),
            store,
            config,
            cache,
            attempt: 0,
        };
        driver.run().await;
    });
}

/// What the engine does next.
enum Flow {
    Pair,
    Connect(Credentials),
    Stop,
}

/// How a wait before retrying ended.
enum Waited {
    /// The delay passed, or the user asked to retry now.
    Retry,
    Flow(Flow),
}

struct Driver {
    rx: mpsc::Receiver<Message>,
    published: Published,
    state: Snapshot,
    store: Store,
    config: SessionConfig,
    cache: Cache,
    /// Failed connection attempts in a row, for the backoff.
    attempt: u32,
}

impl Driver {
    async fn run(mut self) {
        let mut next = match self.cache.credentials() {
            Some(credentials) => Flow::Connect(credentials),
            None => Flow::Pair,
        };
        loop {
            next = match next {
                Flow::Pair => self.pair().await,
                Flow::Connect(credentials) => self.connect(credentials).await,
                Flow::Stop => return,
            };
        }
    }

    fn set_session(&mut self, session: Session) {
        self.state.session = session;
        self.published.publish(&self.state);
    }

    fn fail(&mut self, kind: FailureKind, message: &str, retry_in: Option<Duration>) {
        let retry_at_ms = retry_in.map(|delay| now_ms() + millis(delay));
        self.set_session(Session::Failed {
            kind,
            message: message.into(),
            retry_at_ms,
        });
    }

    /// Show device codes until one is approved.
    async fn pair(&mut self) -> Flow {
        let client =
            match DeviceAuthClientBuilder::new(&self.config.client_id, SCOPES.to_vec()).build() {
                Ok(client) => client,
                Err(_) => {
                    self.fail(FailureKind::Internal, "Pairing is unavailable", None);
                    return self.wait_for_user().await;
                }
            };
        let mut round = PairingRound::default();
        let mut failures = 0;
        loop {
            log::info!("requesting a pairing code");
            let request = client.request_device_code_async();
            tokio::pin!(request);
            let requested = loop {
                tokio::select! {
                    result = &mut request => break result,
                    message = self.rx.recv() => if let Some(flow) = self.while_pairing(message).await {
                        return flow;
                    },
                }
            };
            let Ok(auth) = requested else {
                let delay = backoff(failures);
                failures = failures.saturating_add(1);
                log::warn!(
                    "pairing code request failed; retrying in {} s",
                    delay.as_secs()
                );
                self.fail(
                    FailureKind::Network,
                    "Could not get a pairing code",
                    Some(delay),
                );
                match self.wait(delay).await {
                    Waited::Retry => continue,
                    Waited::Flow(flow) => return flow,
                }
            };
            failures = 0;
            self.set_session(Session::Pairing {
                url: auth.url().to_owned(),
                code: auth.user_code().to_owned(),
                expires_at_ms: now_ms() + millis(PAIRING_LIFETIME),
            });
            log::info!("pairing code shown");
            let poll = client.poll_for_token_async(&auth);
            let deadline = sleep_until(Instant::now() + PAIRING_LIFETIME);
            tokio::pin!(poll, deadline);
            let polled = loop {
                tokio::select! {
                    token = &mut poll => break Some(token),
                    () = &mut deadline => break None,
                    message = self.rx.recv() => if let Some(flow) = self.while_pairing(message).await {
                        return flow;
                    },
                }
            };
            let expired = match polled {
                Some(Ok(token)) => {
                    log::info!("pairing approved");
                    return Flow::Connect(Credentials::with_access_token(token.access_token));
                }
                None => true,
                // Only the category of the error is used, never its text.
                Some(Err(error)) => match classify_pairing_error(&error.to_string()) {
                    PairingError::Expired => true,
                    PairingError::Declined => {
                        log::warn!("pairing declined");
                        self.fail(FailureKind::Auth, "Pairing was declined", None);
                        return self.wait_for_user().await;
                    }
                    PairingError::Other => {
                        let delay = backoff(failures);
                        failures = failures.saturating_add(1);
                        log::warn!("pairing interrupted; new code in {} s", delay.as_secs());
                        self.fail(FailureKind::Network, "Pairing was interrupted", Some(delay));
                        match self.wait(delay).await {
                            Waited::Retry => continue,
                            Waited::Flow(flow) => return flow,
                        }
                    }
                },
            };
            if expired {
                match round.expired() {
                    AfterExpiry::NewCode => log::info!("pairing code expired; showing a new one"),
                    AfterExpiry::WaitForUser => {
                        log::info!("pairing code expired; waiting for the user");
                        self.fail(FailureKind::Auth, "Pairing code expired", None);
                        return self.wait_for_user().await;
                    }
                }
            }
        }
    }

    /// Sign in with `credentials`; on success, stay in the ready phase.
    async fn connect(&mut self, credentials: Credentials) -> Flow {
        self.set_session(Session::Connecting);
        log::info!("signing in");
        let session =
            librespot_core::session::Session::new(self.config.clone(), Some(self.cache.clone()));
        let connecting = session.connect(credentials.clone(), true);
        tokio::pin!(connecting);
        let result = loop {
            tokio::select! {
                result = &mut connecting => break result,
                message = self.rx.recv() => if let Some(flow) = self.while_connecting(message).await {
                    session.shutdown();
                    return flow;
                },
            }
        };
        match result {
            Ok(()) => {
                self.attempt = 0;
                let store = self.store.clone();
                if let Err(error) = blocking(move || store.tighten_credentials()).await {
                    log::warn!("saved login permissions: {}", error.kind());
                }
                log::info!("signed in");
                self.set_session(Session::Ready);
                self.ready(&session).await
            }
            Err(error) => {
                session.shutdown();
                match classify_connect_error(error.kind) {
                    ConnectFailure::LoginRejected => {
                        log::warn!("Spotify rejected the saved login; pairing again");
                        Flow::Pair
                    }
                    ConnectFailure::Retry(kind) => {
                        let delay = backoff(self.attempt);
                        self.attempt = self.attempt.saturating_add(1);
                        log::warn!(
                            "signing in failed ({:?}); retrying in {} s",
                            error.kind,
                            delay.as_secs()
                        );
                        self.fail(kind, "Could not reach Spotify", Some(delay));
                        match self.wait(delay).await {
                            Waited::Retry => Flow::Connect(credentials),
                            Waited::Flow(flow) => flow,
                        }
                    }
                }
            }
        }
    }

    /// Signed in: answer messages and watch the connection.
    async fn ready(&mut self, session: &librespot_core::session::Session) -> Flow {
        let mut health = tokio::time::interval(HEALTH_CHECK);
        health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = health.tick() => {
                    if session.is_invalid() {
                        log::warn!("connection to Spotify lost");
                        return self.lost().await;
                    }
                }
                message = self.rx.recv() => match message {
                    None | Some(Message::Stop) => {
                        session.shutdown();
                        return Flow::Stop;
                    }
                    Some(Message::Command(_, reply)) => refuse(reply, Reject::Unavailable, "playback is not available yet"),
                    Some(Message::Pair(reply)) => refuse(reply, Reject::Invalid, "already signed in"),
                    Some(Message::Logout(reply)) => {
                        session.shutdown();
                        return self.logout(reply).await;
                    }
                    #[cfg(feature = "mock")]
                    Some(Message::Inject(_)) => {}
                },
            }
        }
    }

    /// The session dropped: reconnect with the saved login after a delay.
    async fn lost(&mut self) -> Flow {
        let delay = backoff(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        self.fail(
            FailureKind::Network,
            "Connection to Spotify lost",
            Some(delay),
        );
        match self.wait(delay).await {
            Waited::Retry => self.saved_login(),
            Waited::Flow(flow) => flow,
        }
    }

    fn saved_login(&self) -> Flow {
        match self.cache.credentials() {
            Some(credentials) => Flow::Connect(credentials),
            None => Flow::Pair,
        }
    }

    /// Remove the saved login and return to pairing.
    async fn logout(&mut self, reply: Reply) -> Flow {
        let store = self.store.clone();
        match blocking(move || store.remove_credentials()).await {
            Ok(()) => {
                log::info!("signed out; saved login removed");
                let _ = reply.send(Ok(()));
                Flow::Pair
            }
            Err(error) => {
                log::error!("could not remove the saved login: {}", error.kind());
                let _ = reply.send(Err(Refusal::new(
                    Reject::Unavailable,
                    "could not remove the saved login",
                )));
                self.fail(
                    FailureKind::Internal,
                    "Could not remove the saved login",
                    None,
                );
                self.wait_for_user().await
            }
        }
    }

    /// A message while a pairing code is being requested or shown.
    async fn while_pairing(&mut self, message: Option<Message>) -> Option<Flow> {
        match message? {
            Message::Stop => Some(Flow::Stop),
            Message::Pair(reply) => {
                let _ = reply.send(Ok(()));
                Some(Flow::Pair)
            }
            Message::Logout(reply) => Some(self.logout(reply).await),
            Message::Command(_, reply) => {
                refuse(reply, Reject::NotReady, "not signed in yet");
                None
            }
            #[cfg(feature = "mock")]
            Message::Inject(_) => None,
        }
    }

    /// A message while signing in.
    async fn while_connecting(&mut self, message: Option<Message>) -> Option<Flow> {
        match message? {
            Message::Stop => Some(Flow::Stop),
            Message::Logout(reply) => Some(self.logout(reply).await),
            Message::Pair(reply) => {
                refuse(reply, Reject::Invalid, "already signing in");
                None
            }
            Message::Command(_, reply) => {
                refuse(reply, Reject::NotReady, "not signed in yet");
                None
            }
            #[cfg(feature = "mock")]
            Message::Inject(_) => None,
        }
    }

    /// Wait `delay` before retrying; `pair` retries at once.
    async fn wait(&mut self, delay: Duration) -> Waited {
        let timer = sleep(delay);
        tokio::pin!(timer);
        loop {
            tokio::select! {
                () = &mut timer => return Waited::Retry,
                message = self.rx.recv() => match message {
                    None | Some(Message::Stop) => return Waited::Flow(Flow::Stop),
                    Some(Message::Pair(reply)) => {
                        let _ = reply.send(Ok(()));
                        return Waited::Retry;
                    }
                    Some(Message::Logout(reply)) => return Waited::Flow(self.logout(reply).await),
                    Some(Message::Command(_, reply)) => refuse(reply, Reject::NotReady, "not signed in"),
                    #[cfg(feature = "mock")]
                    Some(Message::Inject(_)) => {}
                },
            }
        }
    }

    /// Wait until the user asks for a new pairing code (or stops).
    async fn wait_for_user(&mut self) -> Flow {
        loop {
            match self.rx.recv().await {
                None | Some(Message::Stop) => return Flow::Stop,
                Some(Message::Pair(reply)) => {
                    let _ = reply.send(Ok(()));
                    return Flow::Pair;
                }
                Some(Message::Logout(reply)) => {
                    let store = self.store.clone();
                    let result = blocking(move || store.remove_credentials()).await;
                    let _ = reply.send(result.map_err(|_| {
                        Refusal::new(Reject::Unavailable, "could not remove the saved login")
                    }));
                    return Flow::Pair;
                }
                Some(Message::Command(_, reply)) => {
                    refuse(reply, Reject::NotReady, "not signed in")
                }
                #[cfg(feature = "mock")]
                Some(Message::Inject(_)) => {}
            }
        }
    }
}

fn refuse(reply: Reply, reason: Reject, message: &str) {
    let _ = reply.send(Err(Refusal::new(reason, message)));
}

/// Run blocking file I/O off the runtime's only thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|_| Err(io::Error::other("background task failed")))
}

fn now_ms() -> u64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    millis(since_epoch)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
