//! The control socket server.
//!
//! The accept loop only accepts; each connection is answered on its own
//! short-lived thread, so a slow or silent client never delays others. When
//! idle, the loop sleeps in `poll(2)` on the socket and the shutdown pipe
//! together, so it does no work until a client connects or a stop is
//! requested.

use super::shutdown::Shutdown;
use crate::protocol::{
    self, Incoming, PROTOCOL_VERSION, Reject, Request, Response, ResponseEnvelope,
};
use rustix::event::{PollFd, PollFlags};
use std::{
    fs, io,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// Connections answered at once; more are closed unanswered.
pub const MAX_CONNECTIONS: usize = 16;
/// How long a client gets to send its request and read the reply.
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Answers requests. Implemented by the service on top of its engine;
/// tests use simple fakes.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, request: Request) -> Response;
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    /// Bind the control socket at `path`. Call only while holding the
    /// instance lock: any socket file already there is then known to be
    /// left over from a service that died, and is replaced.
    pub fn bind(path: &Path) -> io::Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(path)?;
        // The runtime directory already keeps others out; the socket's own
        // mode does not rely on that alone.
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path: path.to_owned(),
        })
    }

    /// Answer clients until `shutdown` is requested.
    pub fn run(&self, handler: Arc<dyn Handler>, shutdown: &Shutdown) -> io::Result<()> {
        let active = Arc::new(AtomicUsize::new(0));
        while !shutdown.is_requested() {
            let mut fds = [
                PollFd::new(&self.listener, PollFlags::IN),
                PollFd::from_borrowed_fd(shutdown.fd(), PollFlags::IN),
            ];
            match rustix::event::poll(&mut fds, None) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(errno) => return Err(errno.into()),
            }
            if !fds[0].revents().contains(PollFlags::IN) {
                continue;
            }
            self.accept_all(&handler, &active);
        }
        // Let clients being answered (such as the one that asked for this
        // shutdown) receive their reply before the process exits.
        let deadline = Instant::now() + CLIENT_IO_TIMEOUT;
        while active.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    /// Accept every pending connection, each answered on its own thread.
    fn accept_all(&self, handler: &Arc<dyn Handler>, active: &Arc<AtomicUsize>) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    log::warn!("accept failed: {}", error.kind());
                    return;
                }
            };
            if active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                log::warn!("too many clients at once; closing a connection");
                continue;
            }
            active.fetch_add(1, Ordering::AcqRel);
            let (handler, done) = (handler.clone(), active.clone());
            let spawned = std::thread::Builder::new()
                .name("pocketspot-client".into())
                .spawn(move || {
                    if let Err(error) = answer(stream, &*handler) {
                        log::info!("client connection ended early: {}", error.kind());
                    }
                    done.fetch_sub(1, Ordering::AcqRel);
                });
            if spawned.is_err() {
                active.fetch_sub(1, Ordering::AcqRel);
                log::warn!("could not start a thread for a client");
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Stop being reachable as soon as the server goes away.
        let _ = fs::remove_file(&self.path);
    }
}

/// Read one request, answer it, and close.
fn answer(mut stream: UnixStream, handler: &dyn Handler) -> io::Result<()> {
    // Accepted sockets may inherit non-blocking mode from the listener.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT))?;
    let response = match protocol::read_message(&mut stream) {
        Ok(line) => match protocol::decode_request(&line) {
            Incoming::Request(request) => handler.handle(request),
            Incoming::OtherVersion => Response::VersionMismatch {
                supported: PROTOCOL_VERSION,
            },
            Incoming::Malformed => Response::rejected(Reject::Malformed, "malformed request"),
        },
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            Response::rejected(Reject::Malformed, "message too large or unterminated")
        }
        Err(error) => return Err(error),
    };
    protocol::write_message(
        &mut stream,
        &ResponseEnvelope {
            protocol: PROTOCOL_VERSION,
            response,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        MAX_MESSAGE_BYTES, Snapshot,
        client::{self, ClientError},
    };
    use std::{
        io::{BufRead, BufReader, Write},
        sync::Mutex,
        thread::JoinHandle,
    };

    /// Records requests; answers snapshots and accepts everything else.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<Request>>);

    impl Handler for Recorder {
        fn handle(&self, request: Request) -> Response {
            self.0.lock().unwrap().push(request.clone());
            match request {
                Request::Snapshot { .. } => Response::Snapshot {
                    snapshot: Box::new(Snapshot {
                        revision: 3,
                        ..Snapshot::default()
                    }),
                },
                _ => Response::Accepted,
            }
        }
    }

    struct Running {
        _dir: tempfile::TempDir,
        socket: PathBuf,
        shutdown: Shutdown,
        thread: Option<JoinHandle<()>>,
        handler: Arc<Recorder>,
    }

    impl Running {
        fn start() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("control.sock");
            let server = Server::bind(&socket).unwrap();
            let shutdown = Shutdown::new().unwrap();
            let handler = Arc::new(Recorder::default());
            let (stop, serving) = (shutdown.clone(), handler.clone());
            let thread = std::thread::spawn(move || server.run(serving, &stop).unwrap());
            Self {
                _dir: dir,
                socket,
                shutdown,
                thread: Some(thread),
                handler,
            }
        }

        fn raw(&self, line: &[u8]) -> String {
            let mut stream = UnixStream::connect(&self.socket).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let _ = stream.write_all(line);
            let mut reply = String::new();
            let _ = BufReader::new(&stream).read_line(&mut reply);
            reply
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.shutdown.request();
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    #[test]
    fn answers_requests_over_a_real_socket() {
        let running = Running::start();
        let mode = fs::metadata(&running.socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        match client::request(&running.socket, Request::Snapshot { since: None }).unwrap() {
            Response::Snapshot { snapshot } => assert_eq!(snapshot.revision, 3),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            client::request(&running.socket, Request::Logout).unwrap(),
            Response::Accepted
        );
        assert_eq!(
            *running.handler.0.lock().unwrap(),
            [Request::Snapshot { since: None }, Request::Logout]
        );
    }

    #[test]
    fn malformed_oversized_and_other_version_requests_are_answered_not_fatal() {
        let running = Running::start();
        assert!(
            running
                .raw(b"{not json}\n")
                .contains(r#""reason":"malformed""#)
        );
        let big = vec![b'x'; MAX_MESSAGE_BYTES + 10];
        assert!(running.raw(&big).contains(r#""reason":"malformed""#));
        let logout_v9 = br#"{"protocol":9,"request":{"type":"logout"}}"#;
        assert!(
            running
                .raw(&[logout_v9.as_slice(), b"\n"].concat())
                .contains("version_mismatch")
        );
        // Still serving afterwards, and none of those reached the handler.
        client::request(&running.socket, Request::Identify).unwrap();
        assert_eq!(*running.handler.0.lock().unwrap(), [Request::Identify]);
    }

    #[test]
    fn identify_and_shutdown_work_from_any_protocol_version() {
        let running = Running::start();
        assert!(client::shutdown(&running.socket, 7, Duration::from_secs(2)).unwrap());
        assert_eq!(*running.handler.0.lock().unwrap(), [Request::Shutdown]);
    }

    #[test]
    fn a_silent_client_does_not_delay_others() {
        let running = Running::start();
        let _silent = UnixStream::connect(&running.socket).unwrap();
        let started = Instant::now();
        client::request(&running.socket, Request::Identify).unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// Answers after a delay; `Shutdown` also stops the server first, the
    /// way the service's handler does.
    struct SlowStopper(Shutdown);

    impl Handler for SlowStopper {
        fn handle(&self, request: Request) -> Response {
            if request == Request::Shutdown {
                self.0.request();
            }
            std::thread::sleep(Duration::from_millis(300));
            Response::Accepted
        }
    }

    #[test]
    fn the_client_that_asked_to_stop_still_gets_its_reply() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let server = Server::bind(&socket).unwrap();
        let shutdown = Shutdown::new().unwrap();
        let handler = Arc::new(SlowStopper(shutdown.clone()));
        let serving = std::thread::spawn(move || server.run(handler, &shutdown).unwrap());
        assert_eq!(
            client::request(&socket, Request::Shutdown).unwrap(),
            Response::Accepted
        );
        serving.join().unwrap();
    }

    #[test]
    fn a_stop_request_ends_the_loop_and_removes_the_socket() {
        let mut running = Running::start();
        let socket = running.socket.clone();
        running.shutdown.request();
        running.thread.take().unwrap().join().unwrap();
        // The server was moved into its thread and dropped when run ended.
        assert!(!socket.exists());
        assert!(matches!(
            client::request(&socket, Request::Identify),
            Err(ClientError::NotRunning(_))
        ));
    }

    #[test]
    fn a_left_over_socket_is_replaced_but_nothing_else_is() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        drop(UnixListener::bind(&socket).unwrap());
        assert!(matches!(
            client::request(&socket, Request::Identify),
            Err(ClientError::NotRunning(_))
        ));
        drop(Server::bind(&socket).unwrap());
        let file = dir.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(Server::bind(&file).is_err());
        assert!(file.exists());
    }

    #[test]
    fn a_reply_in_another_version_is_a_mismatch_for_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let fake = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = protocol::read_message(&mut stream);
            stream
                .write_all(b"{\"protocol\":2,\"response\":{\"type\":\"accepted\"}}\n")
                .unwrap();
        });
        assert!(matches!(
            client::request(&socket, Request::Logout),
            Err(ClientError::VersionMismatch { service: 2 })
        ));
        fake.join().unwrap();
    }
}
