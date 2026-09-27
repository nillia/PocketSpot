//! Files the Spotify engine keeps in the state directory.
//!
//! - `spotify/credentials.json`: librespot's reusable login, written by
//!   librespot itself and made owner-only (0600) here
//! - `spotify/device-id`: the stable Spotify Connect device id, so the
//!   handheld is the same device for Spotify on every start
//! - `spotify/volume`: the last volume in percent, restored at start
//!
//! Blocking file I/O: call these off the async runtime.

use rustix::fs::{Mode, OFlags};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const CREDENTIALS_FILE: &str = "credentials.json";
pub const DEVICE_ID_FILE: &str = "device-id";
pub const VOLUME_FILE: &str = "volume";

#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// `dir` must already be a private directory.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn credentials(&self) -> PathBuf {
        self.dir.join(CREDENTIALS_FILE)
    }

    /// Make the saved login owner-only. librespot writes it with its own
    /// mode; the private directory already keeps others out, and this does
    /// not rely on that alone. A symlink in its place is refused.
    pub fn tighten_credentials(&self) -> io::Result<()> {
        let path = self.credentials();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the saved login is not a regular file",
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Remove the saved login. A missing file is fine.
    pub fn remove_credentials(&self) -> io::Result<()> {
        match fs::remove_file(self.credentials()) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// The saved device id, or `fresh()` saved as the new one. A present
    /// but malformed file is an error rather than silently replaced.
    pub fn device_id(&self, fresh: impl FnOnce() -> String) -> io::Result<String> {
        let path = self.dir.join(DEVICE_ID_FILE);
        match fs::read_to_string(&path) {
            Ok(id) if valid_device_id(id.trim()) => Ok(id.trim().to_owned()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the saved device id is malformed",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let id = fresh();
                if !valid_device_id(&id) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the new device id is malformed",
                    ));
                }
                write_private(&path, format!("{id}\n").as_bytes())?;
                Ok(id)
            }
            Err(error) => Err(error),
        }
    }
}

impl Store {
    /// The saved volume in percent, if there is a valid one.
    pub fn volume(&self) -> Option<u8> {
        fs::read_to_string(self.dir.join(VOLUME_FILE))
            .ok()?
            .trim()
            .parse::<u8>()
            .ok()
            .filter(|percent| *percent <= 100)
    }

    pub fn save_volume(&self, percent: u8) -> io::Result<()> {
        write_private(
            &self.dir.join(VOLUME_FILE),
            format!("{}\n", percent.min(100)).as_bytes(),
        )
    }
}

/// A hyphenated UUID, nothing else (it ends up in file names and requests).
fn valid_device_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Replace `path` atomically with an owner-only file: write a temporary
/// file (refusing a planted symlink), flush it to disk, rename it in place.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut pending = path.as_os_str().to_owned();
    pending.push(".part");
    let pending = PathBuf::from(pending);
    let result = (|| {
        let fd = rustix::fs::open(
            &pending,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?;
        let mut file = fs::File::from(fd);
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&pending, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_device_id_is_created_once_privately_then_reused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_owned());
        assert_eq!(store.device_id(|| ID.into()).unwrap(), ID);
        assert_eq!(mode(&dir.path().join(DEVICE_ID_FILE)), 0o600);
        let again = store
            .device_id(|| panic!("must reuse the saved id"))
            .unwrap();
        assert_eq!(again, ID);
        fs::write(dir.path().join(DEVICE_ID_FILE), "../../etc/passwd").unwrap();
        assert!(store.device_id(|| ID.into()).is_err());
    }

    #[test]
    fn the_volume_is_saved_privately_and_bad_values_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_owned());
        assert_eq!(store.volume(), None);
        store.save_volume(62).unwrap();
        assert_eq!(store.volume(), Some(62));
        assert_eq!(mode(&dir.path().join(VOLUME_FILE)), 0o600);
        fs::write(dir.path().join(VOLUME_FILE), "250").unwrap();
        assert_eq!(store.volume(), None);
    }

    #[test]
    fn a_planted_symlink_is_not_followed_when_saving() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join("device-id.part")).unwrap();
        let store = Store::new(dir.path().to_owned());
        assert!(store.device_id(|| ID.into()).is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[test]
    fn the_saved_login_is_made_private_and_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_owned());
        store.tighten_credentials().unwrap();
        store.remove_credentials().unwrap();
        let path = dir.path().join(CREDENTIALS_FILE);
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        store.tighten_credentials().unwrap();
        assert_eq!(mode(&path), 0o600);
        store.remove_credentials().unwrap();
        assert!(!path.exists());

        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &path).unwrap();
        assert!(store.tighten_credentials().is_err());
    }
}
