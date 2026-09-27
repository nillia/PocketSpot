//! The service log: one bounded, private file in the runtime directory.
//!
//! PocketSpot's own messages are logged from `info` up and never contain
//! secrets. Messages from dependencies are logged from `warn` up and pass
//! through [`redact`], because they were not written with that rule in mind.

use log::{Level, LevelFilter, Log, Metadata, Record};
use rustix::fs::{Mode, OFlags};
use std::{
    fs::File,
    io::{self, Seek, Write},
    path::Path,
    sync::Mutex,
};

/// Name of the log file in the runtime directory.
pub const LOG_FILE: &str = "service.log";
/// The file starts over once it grows past this.
pub const MAX_LOG_BYTES: u64 = 512 * 1024;
const MAX_LINE_CHARS: usize = 400;

pub struct FileLogger {
    file: Mutex<File>,
    max_bytes: u64,
}

impl FileLogger {
    /// Open (or create, 0600) the log file and install it as the logger.
    pub fn install(path: &Path) -> io::Result<()> {
        let logger = Self::open(path, MAX_LOG_BYTES)?;
        log::set_boxed_logger(Box::new(logger))
            .map_err(|_| io::Error::other("a logger is already installed"))?;
        log::set_max_level(LevelFilter::Info);
        Ok(())
    }

    fn open(path: &Path, max_bytes: u64) -> io::Result<Self> {
        let fd = rustix::fs::open(
            path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?;
        Ok(Self {
            file: Mutex::new(File::from(fd)),
            max_bytes,
        })
    }

    fn write_line(&self, level: Level, target: &str, message: &str) {
        let message: String = message
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_LINE_CHARS)
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let line = format!(
            "{}.{:03} {level} {target}: {message}\n",
            now.as_secs(),
            now.subsec_millis()
        );
        // A panic while logging must not make logging unusable afterwards.
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if file.metadata().is_ok_and(|m| m.len() > self.max_bytes) {
            let _ = file.set_len(0);
            let _ = file.rewind();
        }
        let _ = file.write_all(line.as_bytes());
    }
}

fn ours(target: &str) -> bool {
    target.starts_with("pocketspot")
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let floor = if ours(metadata.target()) {
            Level::Info
        } else {
            Level::Warn
        };
        metadata.level() <= floor
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        let message = if ours(record.target()) {
            message
        } else {
            redact(&message)
        };
        self.write_line(record.level(), record.target(), &message);
    }

    fn flush(&self) {
        let _ = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .flush();
    }
}

/// Log panics (thread, location and a redacted message) before the default
/// handler runs, so a crash leaves a trace in the log.
pub fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let location = info
            .location()
            .map_or_else(|| "unknown location".to_owned(), ToString::to_string);
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        log::error!(
            "panic in thread {} at {location}: {}",
            thread.name().unwrap_or("unnamed"),
            redact(&payload)
        );
        log::logger().flush();
        default(info);
    }));
}

/// Keys whose values are secret whatever they look like.
const SECRET_KEYS: [&str; 6] = [
    "token",
    "code",
    "secret",
    "password",
    "credential",
    "authorization",
];

/// Characters of an opaque token. `.`, `/` and `=` split runs, so paths,
/// host names and `key=value` pairs are judged part by part.
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '%' | '~')
}

/// A long run mixing letters and digits: an access token, a code, an id.
fn opaque(run: &str) -> bool {
    run.len() >= 20
        && run.chars().any(|c| c.is_ascii_digit())
        && run.chars().any(|c| c.is_ascii_alphabetic())
}

/// Remove what looks secret from a message: URL query strings, long opaque
/// tokens wherever they appear (also inside JSON, quotes or headers), and the
/// value right after a secret-sounding key (`access_token=…`,
/// `"code":"…"`, `Bearer …`).
pub fn redact(message: &str) -> String {
    let message = strip_url_queries(message);
    let mut out = String::with_capacity(message.len());
    let mut key = String::new();
    let mut rest = message.as_str();
    while !rest.is_empty() {
        let start = rest.find(token_char).unwrap_or(rest.len());
        let (separator, tail) = rest.split_at(start);
        out.push_str(separator);
        let end = tail.find(|c| !token_char(c)).unwrap_or(tail.len());
        let (run, tail) = tail.split_at(end);
        rest = tail;
        if run.is_empty() {
            continue;
        }
        // `key=value` and `"key":"value"` without spaces, or `Bearer value`.
        let after_secret_key = SECRET_KEYS.iter().any(|k| key.ends_with(k))
            && separator.contains(['=', ':'])
            && !separator.contains(char::is_whitespace);
        let after_bearer = key == "bearer" && separator.chars().all(char::is_whitespace);
        if after_secret_key || after_bearer || opaque(run) {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        key = run.to_ascii_lowercase();
    }
    out
}

fn strip_url_queries(message: &str) -> String {
    message
        .split_inclusive(char::is_whitespace)
        .map(|word| match word.find('?') {
            Some(query) if word[..query].contains("://") => {
                let trailing = &word[word.trim_end().len()..];
                format!("{}?[redacted]{trailing}", &word[..query])
            }
            _ => word.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_url_queries_and_opaque_tokens() {
        assert_eq!(
            redact("GET https://api.spotify.com/v1/me?code=ABC&state=1 failed"),
            "GET https://api.spotify.com/v1/me?[redacted] failed"
        );
        assert_eq!(
            redact("token BQD9x_f8aa7c0e1b2c3d4e5f6a7b8c9d0 expired"),
            "token [redacted] expired"
        );
        assert_eq!(
            redact("failed: 'BQD9x_f8aa7c0e1b2c3d4e5f6a7b8'"),
            "failed: '[redacted]'"
        );
    }

    #[test]
    fn redacts_values_after_secret_keys_in_json_headers_and_queries() {
        assert_eq!(
            redact(r#"{"access_token":"short","expires_in":3600}"#),
            r#"{"access_token":"[redacted]","expires_in":3600}"#
        );
        assert_eq!(
            redact("Authorization: Bearer abc123"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(
            redact("state=ok&code=XYZ&x=1"),
            "state=ok&code=[redacted]&x=1"
        );
    }

    #[test]
    fn leaves_ordinary_messages_alone() {
        for message in [
            "connection reset by peer",
            "status code: 429 Too Many Requests",
            "/mnt/UDISK/PocketSpot/credentials.json",
            "error: Unavailable",
        ] {
            assert_eq!(redact(message), message);
        }
    }

    #[test]
    fn the_log_file_is_private_bounded_and_filtered() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        let logger = FileLogger::open(&path, 1024).unwrap();
        for n in 0..100 {
            logger.write_line(Level::Info, "pocketspot", &format!("line {n}"));
        }
        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.len() <= 1024 + MAX_LINE_CHARS as u64 + 64);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

        let quiet = Metadata::builder()
            .level(Level::Info)
            .target("librespot")
            .build();
        assert!(!logger.enabled(&quiet), "dependencies log from warn up");
        let ours = Metadata::builder()
            .level(Level::Info)
            .target("pocketspot::service")
            .build();
        assert!(logger.enabled(&ours));
    }
}
