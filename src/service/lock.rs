//! Single-instance lock.
//!
//! An advisory `flock` on a file in the runtime directory. The kernel
//! releases it when the process exits, however it exits, so a crashed service
//! never leaves a stale lock behind (unlike a PID file).

use rustix::fs::{FlockOperation, Mode, OFlags};
use std::{
    fs::File,
    io,
    path::Path,
    time::{Duration, Instant},
};

/// Name of the lock file in the runtime directory.
pub const LOCK_FILE: &str = "service.lock";

/// Held for the life of the service; dropping it releases the lock.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another PocketSpot service is running")]
    Busy,
    #[error("cannot use the lock file {}: {source}", path.display())]
    Io {
        path: std::path::PathBuf,
        source: io::Error,
    },
}

impl InstanceLock {
    /// Take the lock in `runtime_dir`, waiting up to `patience` for a
    /// service that is still shutting down.
    pub fn acquire(runtime_dir: &Path, patience: Duration) -> Result<Self, LockError> {
        let path = runtime_dir.join(LOCK_FILE);
        let io_error = |source: io::Error| LockError::Io {
            path: path.clone(),
            source,
        };
        let fd = rustix::fs::open(
            &path,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|errno| io_error(errno.into()))?;
        let deadline = Instant::now() + patience;
        loop {
            match rustix::fs::flock(&fd, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
                    return Ok(Self {
                        _file: File::from(fd),
                    });
                }
                Err(rustix::io::Errno::WOULDBLOCK) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(rustix::io::Errno::WOULDBLOCK) => return Err(LockError::Busy),
                Err(errno) => return Err(io_error(errno.into())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_is_busy_until_the_first_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let first = InstanceLock::acquire(dir.path(), Duration::ZERO).unwrap();
        assert!(matches!(
            InstanceLock::acquire(dir.path(), Duration::ZERO),
            Err(LockError::Busy)
        ));
        drop(first);
        InstanceLock::acquire(dir.path(), Duration::ZERO).unwrap();
    }

    #[test]
    fn waiting_takes_over_a_lock_released_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let first = InstanceLock::acquire(dir.path(), Duration::ZERO).unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(first);
        });
        let started = Instant::now();
        InstanceLock::acquire(dir.path(), Duration::from_secs(5)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        releaser.join().unwrap();
    }
}
