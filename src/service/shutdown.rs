//! Shutdown requests from signals and from the service itself.
//!
//! A request sets a flag and writes a byte to a socket pair (the self-pipe
//! pattern). A thread waiting in `poll(2)` on the read end, alongside its own
//! descriptors, wakes up at once instead of checking a flag on a timer.

use rustix::event::{PollFd, PollFlags, Timespec};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use std::{
    io::{self, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::net::UnixStream,
    },
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Keep running when the terminal that started the service closes.
///
/// Call it first thing: a shell that exits right after starting the service
/// in the background sends SIGHUP within milliseconds, before any later
/// setup. Handling SIGHUP with a flag nobody reads replaces the default
/// action (terminate) with nothing.
pub fn ignore_hangup() -> io::Result<()> {
    signal_hook::flag::register(SIGHUP, Arc::default()).map(drop)
}

/// Cheap to clone; all clones share one request.
#[derive(Clone)]
pub struct Shutdown(Arc<Inner>);

struct Inner {
    requested: Arc<AtomicBool>,
    reader: UnixStream,
    writer: UnixStream,
}

impl Shutdown {
    pub fn new() -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Self(Arc::new(Inner {
            requested: Arc::default(),
            reader,
            writer,
        })))
    }

    /// SIGTERM and SIGINT request a shutdown.
    pub fn register_signals(&self) -> io::Result<()> {
        for signal in [SIGTERM, SIGINT] {
            signal_hook::flag::register(signal, self.0.requested.clone())?;
            signal_hook::low_level::pipe::register(signal, self.0.writer.try_clone()?)?;
        }
        Ok(())
    }

    /// Ask the service to stop.
    pub fn request(&self) {
        self.0.requested.store(true, Ordering::Release);
        // A full socket buffer already guarantees a pending wake-up.
        let _ = (&self.0.writer).write(&[1]);
    }

    pub fn is_requested(&self) -> bool {
        self.0.requested.load(Ordering::Acquire)
    }

    /// Readable once a shutdown was requested (never drained), for waiting
    /// on it together with other descriptors.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.0.reader.as_fd()
    }

    /// Block until a shutdown is requested or `timeout` passes. Returns
    /// whether it was requested.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        let deadline = Timespec::try_from(timeout).map_err(io::Error::other)?;
        let mut fds = [PollFd::new(&self.0.reader, PollFlags::IN)];
        while !self.is_requested() {
            match rustix::event::poll(&mut fds, Some(&deadline)) {
                Ok(0) => return Ok(false),
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => {}
                Err(errno) => return Err(errno.into()),
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_request_wakes_a_waiting_thread_at_once() {
        let shutdown = Shutdown::new().unwrap();
        let remote = shutdown.clone();
        let requester = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            remote.request();
        });
        let started = Instant::now();
        assert!(shutdown.wait(Duration::from_secs(10)).unwrap());
        assert!(started.elapsed() < Duration::from_secs(2));
        requester.join().unwrap();
        // It stays requested.
        assert!(shutdown.wait(Duration::ZERO).unwrap());
    }

    #[test]
    fn waiting_times_out_without_a_request() {
        let shutdown = Shutdown::new().unwrap();
        assert!(!shutdown.wait(Duration::from_millis(50)).unwrap());
        assert!(!shutdown.is_requested());
    }
}
