//! The playback service: one instance per user, running independently of
//! any client.

pub mod lock;
pub mod logging;
pub mod server;
pub mod shutdown;

use crate::{
    platform::{Platform, Profile, SystemEnvironment},
    protocol::{PROTOCOL_VERSION, Reject, Request, Response, SERVICE_NAME, SOCKET_FILE, Snapshot},
};
use lock::{InstanceLock, LockError};
use server::{Handler, Server};
use shutdown::Shutdown;
use std::{sync::Arc, time::Duration};

/// How long a new service waits for a previous one that is still stopping.
pub const LOCK_PATIENCE: Duration = Duration::from_secs(8);

/// How the service ended; the process exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Stopped on request or by a signal.
    Stopped = 0,
    /// An unexpected failure.
    Failed = 1,
    /// Bad arguments or an unusable profile.
    Unusable = 2,
    /// Another service holds the lock.
    AlreadyRunning = 3,
}

/// Run the service until it is asked to stop.
///
/// Diagnostics go to standard error only until the log is open. After that
/// the service never writes to the terminal, which may be gone by then.
pub fn run(explicit: Option<Platform>) -> Exit {
    if let Err(error) = shutdown::ignore_hangup() {
        eprintln!("pocketspotd: cannot handle signals: {error}");
        return Exit::Failed;
    }
    let profile = match Profile::resolve(&SystemEnvironment, explicit).and_then(Profile::prepare) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("pocketspotd: {error}");
            return Exit::Unusable;
        }
    };
    // Never keep the launcher's directory (the SD card) busy.
    if let Err(error) = std::env::set_current_dir("/") {
        eprintln!("pocketspotd: cannot change to /: {error}");
        return Exit::Failed;
    }
    let _lock = match InstanceLock::acquire(profile.runtime_dir(), LOCK_PATIENCE) {
        Ok(lock) => lock,
        Err(LockError::Busy) => {
            eprintln!("pocketspotd: already running");
            return Exit::AlreadyRunning;
        }
        Err(error) => {
            eprintln!("pocketspotd: {error}");
            return Exit::Failed;
        }
    };
    let log_path = profile.runtime_dir().join(logging::LOG_FILE);
    if let Err(error) = logging::FileLogger::install(&log_path) {
        eprintln!(
            "pocketspotd: cannot open the log {}: {error}",
            log_path.display()
        );
        return Exit::Failed;
    }
    logging::log_panics();
    let shutdown = match Shutdown::new().and_then(|s| s.register_signals().map(|()| s)) {
        Ok(shutdown) => shutdown,
        Err(error) => {
            log::error!("cannot handle signals: {error}");
            return Exit::Failed;
        }
    };
    log::info!(
        "started {} on {} (pid {})",
        env!("CARGO_PKG_VERSION"),
        profile.platform(),
        std::process::id()
    );
    // The lock is held, so a socket file already there is left over.
    let server = match Server::bind(&profile.runtime_dir().join(SOCKET_FILE)) {
        Ok(server) => server,
        Err(error) => {
            log::error!("cannot open the control socket: {error}");
            return Exit::Failed;
        }
    };
    eprintln!("pocketspotd: running; log: {}", log_path.display());
    let handler = Arc::new(ServiceHandler {
        snapshot: Snapshot {
            revision: start_revision(),
            ..Snapshot::default()
        },
        shutdown: shutdown.clone(),
    });
    let result = server.run(handler, &shutdown);
    // Unreachable from now on, before anything else shuts down.
    drop(server);
    match result {
        Ok(()) => {
            log::info!("stopped");
            Exit::Stopped
        }
        Err(error) => {
            log::error!("control socket failed: {error}");
            Exit::Failed
        }
    }
}

/// The first snapshot revision: the start time in ms × 1000, so a restarted
/// service never repeats a revision an earlier one reported.
fn start_revision() -> u64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since_epoch.as_millis())
        .unwrap_or(u64::MAX / 1000)
        .saturating_mul(1000)
}

/// Answers the protocol. Playback commands become available with the
/// playback engine.
struct ServiceHandler {
    snapshot: Snapshot,
    shutdown: Shutdown,
}

impl Handler for ServiceHandler {
    fn handle(&self, request: Request) -> Response {
        match request {
            Request::Identify => Response::Identity {
                server: SERVICE_NAME.into(),
                version: env!("CARGO_PKG_VERSION").into(),
                pid: std::process::id(),
                protocol: PROTOCOL_VERSION,
            },
            Request::Snapshot { since } if since == Some(self.snapshot.revision) => {
                Response::Unchanged {
                    revision: self.snapshot.revision,
                }
            }
            Request::Snapshot { .. } => Response::Snapshot {
                snapshot: Box::new(self.snapshot.clone()),
            },
            Request::Command { .. } | Request::Logout => {
                Response::rejected(Reject::Unavailable, "playback is not available yet")
            }
            Request::Shutdown => {
                log::info!("shutdown requested");
                self.shutdown.request();
                Response::Accepted
            }
        }
    }
}
