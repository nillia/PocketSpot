//! Platform profiles: everything that differs between the systems
//! PocketSpot runs on.
//!
//! A [`Profile`] is resolved from the environment ([`Profile::resolve`]) and
//! then prepared ([`Profile::prepare`]), which creates and checks its
//! directories. Only a [`ReadyProfile`] is handed to the rest of the program,
//! so code that receives one can rely on its directories being safe to use.

mod private_dir;

use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

/// Environment variable that selects the platform explicitly.
pub const PLATFORM_VAR: &str = "POCKETSPOT_PLATFORM";
/// Environment variable that overrides the state directory.
pub const STATE_DIR_VAR: &str = "POCKETSPOT_STATE_DIR";
/// Environment variable that overrides the runtime directory.
pub const RUNTIME_DIR_VAR: &str = "POCKETSPOT_RUNTIME_DIR";

/// Internal ext4 partition of the TrimUI Brick; keeps Unix permissions,
/// unlike the exFAT SD card.
const BRICK_INTERNAL: &str = "/mnt/UDISK";
/// NextUI's system directory on the SD card.
const NEXTUI_SYSTEM: &str = "/mnt/SDCARD/.system";

/// The systems PocketSpot knows how to run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// TrimUI Brick running NextUI.
    NextUi,
    /// A developer machine.
    Development,
}

impl Platform {
    pub fn name(self) -> &'static str {
        match self {
            Platform::NextUi => "nextui",
            Platform::Development => "development",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Platform {
    type Err = ProfileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "nextui" => Ok(Platform::NextUi),
            "development" => Ok(Platform::Development),
            other => Err(ProfileError::UnknownPlatform(other.to_owned())),
        }
    }
}

/// How audio reaches the speaker or headphones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioOutput {
    /// ALSA's default device.
    AlsaDefault,
}

/// What the profile needs to know about the system it runs on. The real
/// system implements it with [`SystemEnvironment`]; tests use a fake.
pub trait Environment {
    fn var(&self, name: &str) -> Option<String>;
    fn exists(&self, path: &Path) -> bool;
    fn is_linux(&self) -> bool;
    /// Effective user id of this process.
    fn uid(&self) -> u32;
}

/// The environment of the running process.
pub struct SystemEnvironment;

impl Environment for SystemEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_linux(&self) -> bool {
        cfg!(target_os = "linux")
    }

    fn uid(&self) -> u32 {
        rustix::process::geteuid().as_raw()
    }
}

/// A resolved profile whose directories have not been checked yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub platform: Platform,
    /// Persistent files: the saved login, settings.
    pub state_dir: PathBuf,
    /// Files for the current boot: control socket, lock, log.
    pub runtime_dir: PathBuf,
    pub audio: AudioOutput,
    uid: u32,
}

impl Profile {
    /// Pick the platform and its directories.
    ///
    /// The platform is `explicit` if given, else [`PLATFORM_VAR`], else
    /// detected (NextUI when the Brick's internal partition and NextUI's
    /// system directory exist on Linux), else [`Platform::Development`].
    /// [`STATE_DIR_VAR`] and [`RUNTIME_DIR_VAR`] override the directories
    /// and must be absolute.
    pub fn resolve(
        env: &impl Environment,
        explicit: Option<Platform>,
    ) -> Result<Self, ProfileError> {
        let platform = match explicit {
            Some(platform) => platform,
            None => match env.var(PLATFORM_VAR) {
                Some(name) => name.parse()?,
                None => detect(env),
            },
        };
        let uid = env.uid();
        let (state_dir, runtime_dir) = match platform {
            Platform::NextUi => (
                Path::new(BRICK_INTERNAL).join("PocketSpot"),
                PathBuf::from(format!("/tmp/pocketspot-{uid}")),
            ),
            Platform::Development => {
                let home = env.var("HOME").ok_or(ProfileError::NoHome)?;
                let temp = env.var("TMPDIR").unwrap_or_else(|| "/tmp".to_owned());
                (
                    Path::new(&home).join(".local/state/pocketspot"),
                    Path::new(&temp).join(format!("pocketspot-{uid}")),
                )
            }
        };
        Ok(Self {
            platform,
            state_dir: overridden(env, STATE_DIR_VAR)?.unwrap_or(state_dir),
            runtime_dir: overridden(env, RUNTIME_DIR_VAR)?.unwrap_or(runtime_dir),
            audio: AudioOutput::AlsaDefault,
            uid,
        })
    }

    /// Create the directories if needed and check that only this user can
    /// use them. The only way to obtain a [`ReadyProfile`].
    pub fn prepare(self) -> Result<ReadyProfile, ProfileError> {
        private_dir::ensure(&self.state_dir, self.uid)?;
        private_dir::ensure(&self.runtime_dir, self.uid)?;
        Ok(ReadyProfile(self))
    }
}

fn detect(env: &impl Environment) -> Platform {
    let nextui = env.is_linux()
        && env.exists(Path::new(BRICK_INTERNAL))
        && env.exists(Path::new(NEXTUI_SYSTEM));
    if nextui {
        Platform::NextUi
    } else {
        Platform::Development
    }
}

fn overridden(env: &impl Environment, name: &'static str) -> Result<Option<PathBuf>, ProfileError> {
    match env.var(name).map(PathBuf::from) {
        Some(path) if !path.is_absolute() => Err(ProfileError::RelativeOverride { name, path }),
        other => Ok(other),
    }
}

/// A profile whose directories exist and are private to this user. It can
/// only be created by [`Profile::prepare`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyProfile(Profile);

impl ReadyProfile {
    pub fn platform(&self) -> Platform {
        self.0.platform
    }

    pub fn state_dir(&self) -> &Path {
        &self.0.state_dir
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.0.runtime_dir
    }

    pub fn audio(&self) -> AudioOutput {
        self.0.audio
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("unknown platform {0:?} (expected \"nextui\" or \"development\")")]
    UnknownPlatform(String),
    #[error("HOME is not set")]
    NoHome,
    #[error("{name} must be an absolute path, got {}", path.display())]
    RelativeOverride { name: &'static str, path: PathBuf },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    #[error("{} belongs to user {owner}, not to this user", path.display())]
    WrongOwner { path: PathBuf, owner: u32 },
    #[error("{} is accessible to other users (mode {mode:o})", path.display())]
    TooOpen { path: PathBuf, mode: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// A made-up system: environment variables, existing paths, OS, uid.
    #[derive(Default)]
    struct FakeEnvironment {
        vars: HashMap<&'static str, String>,
        paths: HashSet<PathBuf>,
        linux: bool,
    }

    impl FakeEnvironment {
        fn with_var(mut self, name: &'static str, value: &str) -> Self {
            self.vars.insert(name, value.to_owned());
            self
        }

        fn brick() -> Self {
            Self {
                paths: [BRICK_INTERNAL, NEXTUI_SYSTEM].map(PathBuf::from).into(),
                linux: true,
                ..Self::default()
            }
        }

        fn mac() -> Self {
            Self::default()
                .with_var("HOME", "/Users/nico")
                .with_var("TMPDIR", "/var/folders/xy/T")
        }
    }

    impl Environment for FakeEnvironment {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }

        fn exists(&self, path: &Path) -> bool {
            self.paths.contains(path)
        }

        fn is_linux(&self) -> bool {
            self.linux
        }

        fn uid(&self) -> u32 {
            1000
        }
    }

    #[test]
    fn detects_nextui_on_the_brick_with_its_directories() {
        let profile = Profile::resolve(&FakeEnvironment::brick(), None).unwrap();
        assert_eq!(profile.platform, Platform::NextUi);
        assert_eq!(profile.state_dir, Path::new("/mnt/UDISK/PocketSpot"));
        assert_eq!(profile.runtime_dir, Path::new("/tmp/pocketspot-1000"));
        assert_eq!(profile.audio, AudioOutput::AlsaDefault);
    }

    #[test]
    fn falls_back_to_development_with_home_and_tmpdir() {
        let profile = Profile::resolve(&FakeEnvironment::mac(), None).unwrap();
        assert_eq!(profile.platform, Platform::Development);
        assert_eq!(
            profile.state_dir,
            Path::new("/Users/nico/.local/state/pocketspot")
        );
        assert_eq!(
            profile.runtime_dir,
            Path::new("/var/folders/xy/T/pocketspot-1000")
        );
    }

    #[test]
    fn nextui_is_not_detected_without_all_its_markers() {
        let mut env = FakeEnvironment::brick().with_var("HOME", "/root");
        env.paths.remove(Path::new(NEXTUI_SYSTEM));
        assert_eq!(
            Profile::resolve(&env, None).unwrap().platform,
            Platform::Development
        );
        let mut env = FakeEnvironment::brick().with_var("HOME", "/root");
        env.linux = false;
        assert_eq!(
            Profile::resolve(&env, None).unwrap().platform,
            Platform::Development
        );
    }

    #[test]
    fn explicit_choice_beats_the_variable_which_beats_detection() {
        let env = FakeEnvironment::brick()
            .with_var("HOME", "/root")
            .with_var(PLATFORM_VAR, "development");
        assert_eq!(
            Profile::resolve(&env, None).unwrap().platform,
            Platform::Development
        );
        assert_eq!(
            Profile::resolve(&env, Some(Platform::NextUi))
                .unwrap()
                .platform,
            Platform::NextUi
        );
    }

    #[test]
    fn directory_overrides_must_be_absolute() {
        let env = FakeEnvironment::mac()
            .with_var(STATE_DIR_VAR, "/data/state")
            .with_var(RUNTIME_DIR_VAR, "/data/run");
        let profile = Profile::resolve(&env, None).unwrap();
        assert_eq!(profile.state_dir, Path::new("/data/state"));
        assert_eq!(profile.runtime_dir, Path::new("/data/run"));

        let env = FakeEnvironment::mac().with_var(STATE_DIR_VAR, "state");
        assert!(matches!(
            Profile::resolve(&env, None),
            Err(ProfileError::RelativeOverride {
                name: STATE_DIR_VAR,
                ..
            })
        ));
    }

    #[test]
    fn unknown_platform_names_and_a_missing_home_are_errors() {
        let env = FakeEnvironment::mac().with_var(PLATFORM_VAR, "muos");
        assert!(matches!(
            Profile::resolve(&env, None),
            Err(ProfileError::UnknownPlatform(name)) if name == "muos"
        ));
        assert!(matches!(
            Profile::resolve(&FakeEnvironment::default(), None),
            Err(ProfileError::NoHome)
        ));
    }

    #[test]
    fn preparing_creates_private_directories() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let runtime = root.path().join("run");
        let env = FakeEnvironment::mac()
            .with_var(STATE_DIR_VAR, state.to_str().unwrap())
            .with_var(RUNTIME_DIR_VAR, runtime.to_str().unwrap());
        let mut profile = Profile::resolve(&env, None).unwrap();
        // The fake uid is 1000; use the real one for real directories.
        profile.uid = SystemEnvironment.uid();
        let ready = profile.prepare().unwrap();
        assert_eq!(ready.state_dir(), state);
        assert_eq!(ready.runtime_dir(), runtime);
        assert!(state.is_dir() && runtime.is_dir());
    }
}
