//! The Spotify engine: signs in through librespot with Spotify's device-code
//! flow, then plays through librespot's player under Spotify Connect's
//! control (`Spirc`), which also makes the handheld a Connect device.
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

mod library;
mod playback;
mod player;
mod policy;
mod store;

use super::{EngineHandle, Message, Published, QUEUE, Refusal, Reply};
use crate::{
    platform::ReadyProfile,
    protocol::{
        ActiveDevice, Command, FailureKind, Library, LibraryItem, LoadState, PlayState, Playback,
        Reject, Session, Snapshot,
    },
};
use librespot_connect::{ConnectConfig, LoadRequest, LoadRequestOptions, PlayingTrack, Spirc};
use librespot_core::{authentication::Credentials, cache::Cache, config::SessionConfig};
use librespot_oauth::DeviceAuthClientBuilder;
use librespot_playback::player::Player;
use playback::Event;
use policy::{
    AfterExpiry, ConnectFailure, PAIRING_LIFETIME, PairingError, PairingRound, backoff,
    classify_connect_error, classify_pairing_error, liked_songs_uri, percent_to_volume,
    validate_play,
};
use std::{io, sync::Arc, sync::mpsc as std_mpsc, time::Duration};
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
/// The name Spotify shows for this device.
const DEVICE_NAME: &str = "PocketSpot";
/// Volume before the user ever changed it, in percent.
const DEFAULT_VOLUME: u8 = 50;
/// A changed volume is saved this long after the last change, so holding a
/// volume key costs one write.
const VOLUME_SAVE_DELAY: Duration = Duration::from_secs(1);
/// How long leaving Spotify Connect may take when a session ends.
const LEAVE_WITHIN: Duration = Duration::from_secs(1);

type LibraryResult = Result<Vec<LibraryItem>, &'static str>;

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
    let volume = store.volume().unwrap_or(DEFAULT_VOLUME);
    let (tx, rx) = EngineHandle::channel();
    let publisher = published.clone();
    let thread = std::thread::Builder::new()
        .name("pocketspot-spotify".into())
        .spawn(move || run(rx, store, config, volume, publisher))?;
    Ok(EngineHandle::new(tx, published, thread))
}

/// The engine thread: a runtime for librespot, fed by a bridge thread.
fn run(
    rx: std_mpsc::Receiver<Message>,
    store: Store,
    config: SessionConfig,
    volume: u8,
    published: Published,
) {
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
            state: Snapshot {
                playback: Playback {
                    volume,
                    ..Playback::default()
                },
                ..Snapshot::default()
            },
            store,
            config,
            cache,
            attempt: 0,
            liked_songs: None,
            volume_save: None,
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

/// Why the ready phase ended.
enum Leave {
    Lost,
    Logout(Reply),
    Flow(Flow),
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
    /// The signed-in account's Liked Songs URI.
    liked_songs: Option<String>,
    /// A volume to save, and when (debounced).
    volume_save: Option<(u8, Instant)>,
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

    /// Sign in with `credentials` and start playback control; on success,
    /// stay in the ready phase.
    async fn connect(&mut self, credentials: Credentials) -> Flow {
        self.set_session(Session::Connecting);
        log::info!("signing in");
        let session =
            librespot_core::session::Session::new(self.config.clone(), Some(self.cache.clone()));
        let (player, mixer) = match player::build(&session) {
            Ok(built) => built,
            Err(message) => {
                log::error!("audio: {message}");
                session.shutdown();
                return self
                    .retry_later(FailureKind::Audio, message, credentials)
                    .await;
            }
        };
        let connect_config = ConnectConfig {
            name: DEVICE_NAME.into(),
            initial_volume: percent_to_volume(self.state.playback.volume),
            emit_set_queue_events: true,
            ..ConnectConfig::default()
        };
        // Spirc signs the session in with the credentials, then takes part
        // in Spotify Connect.
        let starting = Spirc::new(
            connect_config,
            session.clone(),
            credentials.clone(),
            player.clone(),
            mixer,
        );
        tokio::pin!(starting);
        let result = loop {
            tokio::select! {
                result = &mut starting => break result,
                message = self.rx.recv() => if let Some(flow) = self.while_connecting(message).await {
                    session.shutdown();
                    return flow;
                },
            }
        };
        match result {
            Ok((spirc, task)) => {
                self.attempt = 0;
                let store = self.store.clone();
                if let Err(error) = blocking(move || store.tighten_credentials()).await {
                    log::warn!("saved login permissions: {}", error.kind());
                }
                self.liked_songs = liked_songs_uri(&session.username());
                log::info!("signed in");
                self.set_session(Session::Ready);
                let leave = self.ready(&session, &player, &spirc, task).await;
                self.leave_session(&session).await;
                match leave {
                    Leave::Lost => self.lost().await,
                    Leave::Logout(reply) => self.logout(reply).await,
                    Leave::Flow(flow) => flow,
                }
            }
            Err(error) => {
                session.shutdown();
                match classify_connect_error(error.kind) {
                    ConnectFailure::LoginRejected => {
                        log::warn!("Spotify rejected the saved login; pairing again");
                        Flow::Pair
                    }
                    ConnectFailure::Retry(kind) => {
                        log::warn!("signing in failed ({:?})", error.kind);
                        self.retry_later(kind, "Could not reach Spotify", credentials)
                            .await
                    }
                }
            }
        }
    }

    /// Report a failure, wait with backoff, and sign in again.
    async fn retry_later(
        &mut self,
        kind: FailureKind,
        message: &str,
        credentials: Credentials,
    ) -> Flow {
        let delay = backoff(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        log::info!("retrying in {} s", delay.as_secs());
        self.fail(kind, message, Some(delay));
        match self.wait(delay).await {
            Waited::Retry => Flow::Connect(credentials),
            Waited::Flow(flow) => flow,
        }
    }

    /// Signed in: play, answer messages, and watch the connection.
    async fn ready(
        &mut self,
        session: &librespot_core::session::Session,
        player: &Arc<Player>,
        spirc: &Spirc,
        task: impl std::future::Future<Output = ()>,
    ) -> Leave {
        let mut events = player.get_player_event_channel();
        let (library_tx, mut library_rx) = mpsc::channel::<LibraryResult>(1);
        self.load_library(session, &library_tx);
        let mut health = tokio::time::interval(HEALTH_CHECK);
        health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tokio::pin!(task);
        let leave = loop {
            let save_at = self.volume_save.map(|(_, at)| at);
            tokio::select! {
                () = &mut task => {
                    log::warn!("Spotify Connect stopped");
                    // The task is finished; it must not be awaited again.
                    return self.leave_now(spirc, Leave::Lost);
                }
                _ = health.tick() => {
                    if session.is_invalid() || player.is_invalid() {
                        log::warn!("connection to Spotify lost");
                        break Leave::Lost;
                    }
                }
                event = events.recv() => match event {
                    Some(event) => self.player_event(event),
                    None => {
                        log::warn!("the player stopped");
                        break Leave::Lost;
                    }
                },
                Some(result) = library_rx.recv() => self.library_loaded(result),
                () = sleep_until_opt(save_at), if save_at.is_some() => self.save_volume().await,
                message = self.rx.recv() => match message {
                    None | Some(Message::Stop) => break Leave::Flow(Flow::Stop),
                    Some(Message::Command(command, reply)) => {
                        let result = self.command(spirc, session, &library_tx, command);
                        let _ = reply.send(result);
                    }
                    Some(Message::Pair(reply)) => refuse(reply, Reject::Invalid, "already signed in"),
                    Some(Message::Logout(reply)) => break Leave::Logout(reply),
                    #[cfg(feature = "mock")]
                    Some(Message::Inject(_)) => {}
                },
            }
        };
        // Pause and leave Spotify Connect cleanly, but never wait long.
        let _ = spirc.shutdown();
        let _ = tokio::time::timeout(LEAVE_WITHIN, &mut task).await;
        leave
    }

    /// Leave the ready phase when Spirc's task already ended.
    fn leave_now(&mut self, spirc: &Spirc, leave: Leave) -> Leave {
        let _ = spirc.shutdown();
        leave
    }

    /// After a session: save a pending volume, close the session and show
    /// that nothing plays here any more.
    async fn leave_session(&mut self, session: &librespot_core::session::Session) {
        if self.volume_save.is_some() {
            self.save_volume().await;
        }
        session.shutdown();
        self.state.playback.state = PlayState::Stopped;
        self.state.device = ActiveDevice::None;
        self.published.publish(&self.state);
    }

    fn command(
        &mut self,
        spirc: &Spirc,
        session: &librespot_core::session::Session,
        library_tx: &mpsc::Sender<LibraryResult>,
        command: Command,
    ) -> Result<(), Refusal> {
        let loaded =
            self.state.playback.track.is_some() && self.state.playback.state != PlayState::Stopped;
        let nothing_playing = || Refusal::new(Reject::Invalid, "nothing is playing");
        let sent = match command {
            Command::Play {
                context_uri,
                track_uri,
            } => {
                validate_play(
                    &context_uri,
                    track_uri.as_deref(),
                    self.liked_songs.as_deref(),
                )
                .map_err(|message| Refusal::new(Reject::Invalid, message))?;
                let options = LoadRequestOptions {
                    start_playing: true,
                    playing_track: track_uri.map(PlayingTrack::Uri),
                    ..LoadRequestOptions::default()
                };
                spirc
                    .activate()
                    .and_then(|()| spirc.load(LoadRequest::from_context_uri(context_uri, options)))
            }
            Command::Pause if loaded => spirc.pause(),
            Command::Resume if loaded => spirc.play(),
            Command::Next if loaded => spirc.next(),
            Command::Previous if loaded => spirc.prev(),
            Command::Pause | Command::Resume | Command::Next | Command::Previous => {
                return Err(nothing_playing());
            }
            Command::SetVolume { percent } if percent <= 100 => {
                spirc.set_volume(percent_to_volume(percent))
            }
            Command::SetVolume { .. } => {
                return Err(Refusal::new(
                    Reject::Invalid,
                    "volume must be between 0 and 100",
                ));
            }
            Command::SetShuffle { enabled } => spirc.shuffle(enabled),
            Command::Stop => {
                let sent = spirc.disconnect(true);
                playback::apply(&mut self.state, Event::Stopped, now_ms());
                self.published.publish(&self.state);
                sent
            }
            Command::RefreshLibrary => {
                self.load_library(session, library_tx);
                Ok(())
            }
        };
        sent.map_err(|_| {
            log::warn!("Spotify Connect did not accept a command");
            Refusal::new(Reject::Unavailable, "Spotify did not accept the command")
        })
    }

    fn player_event(&mut self, event: librespot_playback::player::PlayerEvent) {
        let Some(event) = player::reduce(event) else {
            return;
        };
        if let Event::Volume { percent } = event {
            self.volume_save = Some((percent, Instant::now() + VOLUME_SAVE_DELAY));
        }
        playback::apply(&mut self.state, event, now_ms());
        self.published.publish(&self.state);
    }

    /// Fetch the library in the background; the result arrives on `tx`.
    fn load_library(
        &mut self,
        session: &librespot_core::session::Session,
        tx: &mpsc::Sender<LibraryResult>,
    ) {
        self.state.library.state = LoadState::Loading;
        self.published.publish(&self.state);
        let (session, liked, tx) = (session.clone(), self.liked_songs.clone(), tx.clone());
        tokio::spawn(async move {
            let result = library::fetch(&session, liked.as_deref()).await;
            let _ = tx.send(result).await;
        });
    }

    fn library_loaded(&mut self, result: LibraryResult) {
        match result {
            Ok(items) => {
                log::info!("library loaded ({} items)", items.len());
                self.state.library = Library {
                    state: LoadState::Ready,
                    items,
                };
                // A context shown before the library arrived gets its name.
                if let Some(context) = &self.state.playback.context
                    && context.name.is_none()
                {
                    let name = playback::context_name(&self.state, &context.uri);
                    if let Some(context) = &mut self.state.playback.context {
                        context.name = name;
                    }
                }
            }
            Err(message) => {
                log::warn!("library unavailable");
                self.state.library.state = LoadState::Failed {
                    message: message.into(),
                };
            }
        }
        self.published.publish(&self.state);
    }

    /// Save the pending volume off the runtime thread.
    async fn save_volume(&mut self) {
        let Some((percent, _)) = self.volume_save.take() else {
            return;
        };
        let store = self.store.clone();
        if let Err(error) = blocking(move || store.save_volume(percent)).await {
            log::warn!("volume not saved: {}", error.kind());
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
                self.liked_songs = None;
                let volume = self.state.playback.volume;
                self.state.playback = Playback {
                    volume,
                    ..Playback::default()
                };
                self.state.library = Library::default();
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

/// Sleep until `at`, or forever without one.
async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
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
