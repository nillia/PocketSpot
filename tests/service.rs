//! End to end: the `pocketspotd` binary, started and signalled as the
//! launcher and the user would.

use pocketspot::protocol::{
    self, PROTOCOL_VERSION, Reject, Request, Response,
    client::{self, ClientError},
};
use rustix::process::{Pid, Signal};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(15);

struct Dirs {
    _root: tempfile::TempDir,
    state: PathBuf,
    runtime: PathBuf,
}

impl Dirs {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let (state, runtime) = (root.path().join("state"), root.path().join("run"));
        Self {
            _root: root,
            state,
            runtime,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pocketspotd"));
        command
            .args(["--platform", "development"])
            .env("POCKETSPOT_STATE_DIR", &self.state)
            .env("POCKETSPOT_RUNTIME_DIR", &self.runtime)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn socket(&self) -> PathBuf {
        self.runtime.join("control.sock")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.runtime.join("service.log")).unwrap_or_default()
    }

    /// Start a service and wait until it logged its start.
    fn start(&self) -> Child {
        let starts_before = self.log().matches(" started ").count();
        let child = self.command().spawn().unwrap();
        until("the service to start", || {
            self.log().matches(" started ").count() > starts_before
        });
        child
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn signal(child: &Child, signal: Signal) {
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, signal).unwrap();
}

fn exit_of(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "the service did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn stops_cleanly_on_sigterm_and_releases_the_lock() {
    let dirs = Dirs::new();
    let mut service = dirs.start();
    signal(&service, Signal::TERM);
    assert_eq!(exit_of(&mut service).code(), Some(0));
    assert!(dirs.log().contains(" stopped"));
    // The lock is free: the next service starts at once.
    let mut next = dirs.start();
    signal(&next, Signal::INT);
    assert_eq!(exit_of(&mut next).code(), Some(0));
}

#[test]
fn keeps_running_when_the_terminal_hangs_up() {
    let dirs = Dirs::new();
    let mut service = dirs.start();
    signal(&service, Signal::HUP);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(service.try_wait().unwrap(), None, "SIGHUP must not stop it");
    signal(&service, Signal::TERM);
    assert_eq!(exit_of(&mut service).code(), Some(0));
}

#[test]
fn a_second_service_waits_for_one_that_is_stopping_then_takes_over() {
    let dirs = Dirs::new();
    let mut first = dirs.start();
    let mut second = dirs.command().spawn().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(second.try_wait().unwrap(), None, "waiting for the lock");
    signal(&first, Signal::TERM);
    assert_eq!(exit_of(&mut first).code(), Some(0));
    until("the second service to start", || {
        dirs.log().matches(" started ").count() == 2
    });
    signal(&second, Signal::TERM);
    assert_eq!(exit_of(&mut second).code(), Some(0));
}

#[test]
fn creates_its_directories_privately_and_logs_privately() {
    use std::os::unix::fs::PermissionsExt;
    let dirs = Dirs::new();
    let mut service = dirs.start();
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dirs.state), 0o700);
    assert_eq!(mode(&dirs.runtime), 0o700);
    assert_eq!(mode(&dirs.runtime.join("service.log")), 0o600);
    #[cfg(target_os = "linux")]
    assert_eq!(
        std::fs::read_link(format!("/proc/{}/cwd", service.id())).unwrap(),
        Path::new("/"),
        "runs from / so it keeps no SD card directory busy"
    );
    signal(&service, Signal::TERM);
    exit_of(&mut service);
}

#[test]
fn reports_bad_arguments_and_its_version() {
    let dirs = Dirs::new();
    let status = dirs.command().arg("--bogus").status().unwrap();
    assert_eq!(status.code(), Some(2));
    let output = Command::new(env!("CARGO_BIN_EXE_pocketspotd"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("pocketspotd "));
}

#[test]
fn answers_the_protocol_and_stops_on_request() {
    let dirs = Dirs::new();
    let mut service = dirs.start();
    let identity = client::identify(&dirs.socket(), Duration::from_secs(2)).unwrap();
    assert_eq!(identity.server, "pocketspotd");
    assert_eq!(identity.pid, service.id());
    assert_eq!(identity.protocol, PROTOCOL_VERSION);

    let Response::Snapshot { snapshot } =
        client::request(&dirs.socket(), Request::Snapshot { since: None }).unwrap()
    else {
        panic!("expected a snapshot");
    };
    assert_eq!(
        client::request(
            &dirs.socket(),
            Request::Snapshot {
                since: Some(snapshot.revision)
            }
        )
        .unwrap(),
        Response::Unchanged {
            revision: snapshot.revision
        }
    );
    assert!(matches!(
        client::request(
            &dirs.socket(),
            Request::Command {
                command: protocol::Command::Next
            }
        )
        .unwrap(),
        Response::Rejected {
            reason: Reject::Unavailable,
            ..
        }
    ));

    assert_eq!(
        client::request(&dirs.socket(), Request::Shutdown).unwrap(),
        Response::Accepted
    );
    assert_eq!(exit_of(&mut service).code(), Some(0));
    assert!(!dirs.socket().exists(), "the socket goes with the service");
    assert!(matches!(
        client::request(&dirs.socket(), Request::Identify),
        Err(ClientError::NotRunning(_))
    ));
}

#[test]
fn a_socket_left_by_a_killed_service_is_replaced() {
    let dirs = Dirs::new();
    let mut killed = dirs.start();
    signal(&killed, Signal::KILL);
    exit_of(&mut killed);
    assert!(dirs.socket().exists(), "SIGKILL leaves the socket behind");
    let mut next = dirs.start();
    assert_eq!(
        client::identify(&dirs.socket(), Duration::from_secs(2))
            .unwrap()
            .pid,
        next.id()
    );
    signal(&next, Signal::TERM);
    assert_eq!(exit_of(&mut next).code(), Some(0));
}

#[cfg(feature = "mock")]
#[test]
fn plays_fictional_music_with_the_mock_engine() {
    use pocketspot::protocol::{PlayState, Session, Snapshot};

    let dirs = Dirs::new();
    let starts_before = dirs.log().matches(" started ").count();
    let mut service = dirs
        .command()
        .arg("--mock")
        .env("POCKETSPOT_MOCK_SIGNED_IN", "1")
        .spawn()
        .unwrap();
    until("the service to start", || {
        dirs.log().matches(" started ").count() > starts_before
    });
    let snapshot = || -> Snapshot {
        match client::request(&dirs.socket(), Request::Snapshot { since: None }).unwrap() {
            Response::Snapshot { snapshot } => *snapshot,
            other => panic!("{other:?}"),
        }
    };
    until("the session to be ready", || {
        snapshot().session == Session::Ready
    });
    let command = |command| client::request(&dirs.socket(), Request::Command { command }).unwrap();

    let (playlist, name) = pocketspot::engine::mock::playlists().next().unwrap();
    let play = protocol::Command::Play {
        context_uri: playlist.into(),
        track_uri: None,
    };
    assert_eq!(command(play), Response::Accepted);
    let playing = snapshot();
    assert_eq!(playing.playback.state, PlayState::Playing);
    let context = playing.playback.context.unwrap();
    assert_eq!(context.name.as_deref(), Some(name));
    let first = playing.playback.track.unwrap().title;

    assert_eq!(command(protocol::Command::Next), Response::Accepted);
    assert_ne!(snapshot().playback.track.unwrap().title, first);
    assert_eq!(command(protocol::Command::Pause), Response::Accepted);
    assert_eq!(snapshot().playback.state, PlayState::Paused);
    assert!(matches!(
        command(protocol::Command::SetVolume { percent: 200 }),
        Response::Rejected {
            reason: Reject::Invalid,
            ..
        }
    ));

    assert_eq!(
        client::request(&dirs.socket(), Request::Logout).unwrap(),
        Response::Accepted
    );
    assert!(matches!(snapshot().session, Session::Pairing { .. }));
    client::request(&dirs.socket(), Request::Shutdown).unwrap();
    assert_eq!(exit_of(&mut service).code(), Some(0));
}
