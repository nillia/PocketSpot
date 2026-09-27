//! Directories only the current user can use.
//!
//! PocketSpot keeps its runtime files under `/tmp`, which every user can
//! write to, so creating the directory is not enough: whatever is at the path
//! (a directory someone else created, a symlink) must be checked before use.

use super::ProfileError;
use std::{
    fs::{self, DirBuilder},
    io,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::Path,
};

/// What is at a path, without following symlinks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileKind {
    Directory,
    Symlink,
    Other,
}

/// Why an existing path is not a usable private directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unsafe {
    NotADirectory,
    WrongOwner { owner: u32 },
    TooOpen { mode: u32 },
}

/// Decide whether a path with these properties is a private directory of
/// user `me`. Pure, so every case is testable, including a directory owned
/// by another user, which a test cannot create.
pub(super) fn verdict(kind: FileKind, owner: u32, mode: u32, me: u32) -> Result<(), Unsafe> {
    if kind != FileKind::Directory {
        return Err(Unsafe::NotADirectory);
    }
    if owner != me {
        return Err(Unsafe::WrongOwner { owner });
    }
    if mode & 0o077 != 0 {
        return Err(Unsafe::TooOpen { mode: mode & 0o777 });
    }
    Ok(())
}

/// Create `path` (and missing parents) with mode 0700, or accept an existing
/// directory, then verify it is a real directory owned by `me` with no
/// group or other permissions.
///
/// On filesystems without Unix permissions (such as the exFAT SD card),
/// the reported mode is open to everyone, so they are refused as well.
pub(super) fn ensure(path: &Path, me: u32) -> Result<(), ProfileError> {
    let io_error = |source: io::Error| ProfileError::Io {
        path: path.to_owned(),
        source,
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(io_error)?;
    // `symlink_metadata` describes the path itself; `metadata` would follow
    // a planted symlink and report on its target instead.
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    let kind = if metadata.file_type().is_symlink() {
        FileKind::Symlink
    } else if metadata.is_dir() {
        FileKind::Directory
    } else {
        FileKind::Other
    };
    verdict(kind, metadata.uid(), metadata.mode(), me).map_err(|problem| match problem {
        Unsafe::NotADirectory => ProfileError::NotADirectory(path.to_owned()),
        Unsafe::WrongOwner { owner } => ProfileError::WrongOwner {
            path: path.to_owned(),
            owner,
        },
        Unsafe::TooOpen { mode } => ProfileError::TooOpen {
            path: path.to_owned(),
            mode,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const ME: u32 = 1000;

    #[test]
    fn only_a_private_directory_of_the_current_user_passes() {
        use FileKind::*;
        let cases = [
            (Directory, ME, 0o40700, Ok(())),
            (Directory, ME, 0o40750, Err(Unsafe::TooOpen { mode: 0o750 })),
            (Directory, ME, 0o40777, Err(Unsafe::TooOpen { mode: 0o777 })),
            (Directory, 0, 0o40700, Err(Unsafe::WrongOwner { owner: 0 })),
            (Symlink, ME, 0o120777, Err(Unsafe::NotADirectory)),
            (Other, ME, 0o100600, Err(Unsafe::NotADirectory)),
        ];
        for (kind, owner, mode, expected) in cases {
            assert_eq!(
                verdict(kind, owner, mode, ME),
                expected,
                "{kind:?} {mode:o}"
            );
        }
    }

    fn me() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn creates_a_missing_directory_and_its_parents_as_0700() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state/pocketspot");
        ensure(&path, me()).unwrap();
        assert_eq!(mode(&path), 0o700);
        // Running it again accepts the directory it made.
        ensure(&path, me()).unwrap();
    }

    #[test]
    fn refuses_an_existing_directory_others_can_use() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("open");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            ensure(&path, me()),
            Err(ProfileError::TooOpen { mode: 0o755, .. })
        ));
    }

    #[test]
    fn refuses_a_symlink_even_to_a_private_directory() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        ensure(&target, me()).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            ensure(&link, me()),
            Err(ProfileError::NotADirectory(_))
        ));
    }

    #[test]
    fn refuses_a_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        fs::write(&path, "").unwrap();
        assert!(matches!(ensure(&path, me()), Err(ProfileError::Io { .. })));
    }
}
