//! The playback service: one instance per user, running independently of
//! any client.

pub mod lock;
pub mod logging;
pub mod shutdown;

use crate::platform::{Platform, Profile, SystemEnvironment};
use lock::{InstanceLock, LockError};
use shutdown::Shutdown;
use std::time::Duration;

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
    eprintln!("pocketspotd: running; log: {}", log_path.display());
    loop {
        match shutdown.wait(Duration::from_secs(3600)) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => {
                log::error!("waiting for shutdown failed: {error}");
                return Exit::Failed;
            }
        }
    }
    log::info!("stopped");
    Exit::Stopped
}
