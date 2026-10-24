#[cfg(unix)]
pub mod bundled;
#[cfg(unix)]
pub mod fetch;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod pack;
#[cfg(unix)]
pub(crate) mod process;
pub mod progress;
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod run;
#[cfg(target_os = "windows")]
pub mod windows;

use serde::{Deserialize, Serialize};

pub use pack::{PackGroup, PackKind};

const VERSION_BYTES_MAX: usize = 32;

/// A system tool the app needs: tmux runs agent terminals, uv installs Graphify and
/// Graphify builds each project's code graph. Setup finds each one or installs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dependency {
    Tmux,
    Uv,
    Graphify,
}

impl Dependency {
    /// Every dependency in install order; uv comes before Graphify, which needs it.
    pub const ALL: [Self; 3] = [Self::Tmux, Self::Uv, Self::Graphify];

    /// The program name looked up on the search path, such as `tmux`.
    pub fn program(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Uv => "uv",
            Self::Graphify => "graphify",
        }
    }

    /// The plain name the setup screen shows, such as `Graphify`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Uv => "uv",
            Self::Graphify => "Graphify",
        }
    }
}

/// A plain reason setup could not find or install something, as a stable code such as
/// `setup_checksum_mismatch`. `plain_reason` turns the code into the sentence setup shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallError(pub &'static str);

impl std::fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for InstallError {}

/// A tool version as the tool prints it, such as `3.5a` or `0.9.9`: 1 to 32 characters
/// of letters, digits, `.`, `-` and `+`, starting with a letter or digit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version(String);

impl Version {
    /// Checks one version text. Input: the text. Output: the version, or
    /// `setup_version_unreadable` when it is empty, too long or has other characters.
    pub fn parse(text: &str) -> Result<Self, InstallError> {
        let valid = !text.is_empty()
            && text.len() <= VERSION_BYTES_MAX
            && text
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+');
        if !valid {
            return Err(InstallError("setup_version_unreadable"));
        }
        Ok(Self(text.to_owned()))
    }

    /// Returns the version text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Version {
    type Error = InstallError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<Version> for String {
    fn from(version: Version) -> Self {
        version.0
    }
}

/// Exactly what setup installs for one dependency. `Release` downloads one archive,
/// checks its SHA-256 and takes one program out of it; `UvTool` runs `uv tool install`
/// for one pinned package version; `Homebrew` installs one formula, whose bottles
/// Homebrew verifies itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    Release {
        version: Version,
        url: String,
        sha256: String,
        member: String,
    },
    UvTool {
        package: String,
        version: Version,
        python: String,
    },
    Homebrew {
        formula: String,
    },
}

/// Where one dependency stands on the setup screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DependencyState {
    Missing,
    Found { version: Version },
    Installing,
    Installed { version: Version },
    Failed { reason: String },
}

/// One dependency as the setup screen lists it: which tool, its plain name and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyStatus {
    pub name: Dependency,
    pub label: String,
    pub state: DependencyState,
}

/// Where one default-pack entry stands: not installed yet, being installed, installed
/// and on, installed and switched off, or failed with a plain reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackState {
    Available,
    Installing,
    On,
    Off,
    Failed { reason: String },
}

/// One default-pack entry as the setup screen and Settings list it: its id, kind and
/// group, where it comes from, its license, whether it starts ticked and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackEntry {
    pub id: String,
    pub kind: PackKind,
    pub group: PackGroup,
    pub source: String,
    pub license: String,
    pub on_by_default: bool,
    pub state: PackState,
}

/// Everything the setup screen shows: each dependency, each pack entry, and whether a
/// setup run is going on now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupStatus {
    pub dependencies: Vec<DependencyStatus>,
    pub pack: Vec<PackEntry>,
    pub running: bool,
}

/// Finds and installs the app's system dependencies on one platform. `find` returns the
/// installed version or none; `install` installs exactly the pin and returns the version
/// now present; `install_tool` installs one pinned Python tool an MCP server needs into
/// the app data folder; `program` finds a program setup installed or the system has.
/// Implementations never use sudo and never change global configuration.
pub trait Installer: Send + Sync {
    fn find(&self, dep: Dependency) -> Result<Option<Version>, InstallError>;
    fn install(&self, dep: Dependency, pin: &Pin) -> Result<Version, InstallError>;

    fn install_tool(&self, package: &str, version: &str) -> Result<(), InstallError> {
        let _ = (package, version);
        Err(InstallError("setup_tool_unsupported"))
    }

    fn program(&self, name: &str) -> Option<std::path::PathBuf> {
        let _ = name;
        None
    }
}

/// The code every setup step reports on a system setup has no installer for yet.
pub const PLATFORM_UNSUPPORTED: &str = "setup_platform_unsupported";

const REASONS: [(&str, &str); 23] = [
    (
        "setup_checksum_mismatch",
        "The download did not match its checksum, so it was not installed.",
    ),
    (
        "setup_download_failed",
        "The download failed. Check the internet connection and try again.",
    ),
    (
        "setup_homebrew_missing",
        "Homebrew is not installed. Install tmux yourself, then run setup again.",
    ),
    ("setup_install_failed", "The installer did not finish."),
    (
        "setup_timeout",
        "The install took too long and was stopped.",
    ),
    (
        "setup_version_mismatch",
        "A different version than the pinned one is installed.",
    ),
    (
        "setup_version_unreadable",
        "The installed program did not report its version.",
    ),
    (
        "setup_uv_missing",
        "uv is needed to install Graphify and it is not installed.",
    ),
    (
        "setup_store_unavailable",
        "The app's data folder can't be written.",
    ),
    ("setup_pin_invalid", "The pinned source is not valid."),
    ("setup_busy", "Setup is already running."),
    (
        "pack_commit_mismatch",
        "The source does not have the pinned version.",
    ),
    ("pack_fetch_failed", "The source could not be downloaded."),
    (
        "pack_no_skills",
        "The source has no skill folder with a SKILL.md file.",
    ),
    (
        "pack_server_invalid",
        "The server's start command is not valid.",
    ),
    (
        "pack_server_command_missing",
        "The program this server needs is not installed.",
    ),
    (
        "pack_bundled_unavailable",
        "This bundled entry is not available in this build.",
    ),
    ("pack_unknown", "This entry is not in the default pack."),
    (
        "pack_name_taken",
        "A skill or server with this name already exists.",
    ),
    (
        "pack_required",
        "This entry is always on and can't be removed.",
    ),
    (
        "setup_tool_unsupported",
        "This server's tool can't be installed on this system yet.",
    ),
    (
        "pack_not_installed",
        "This entry is not installed yet. Run setup first.",
    ),
    (
        PLATFORM_UNSUPPORTED,
        "Setup can't install this on this system yet. Install it yourself, then check again.",
    ),
];

/// Turns a setup code into the plain sentence the setup screen shows. Input: a code such
/// as `setup_checksum_mismatch`. Output: its sentence, or a general one for other codes.
pub fn plain_reason(code: &str) -> &'static str {
    REASONS
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, sentence)| *sentence)
        .unwrap_or("Something went wrong during setup.")
}

/// The installer for this platform, installing into `<engine root>/setup`.
/// Input: the engine root. Output: the installer.
#[cfg(target_os = "macos")]
pub fn platform_installer(engine_root: &std::path::Path) -> Box<dyn Installer> {
    Box::new(macos::MacInstaller::new(engine_root))
}

/// The installer on a system setup has none for yet: every lookup and install reports
/// [`PLATFORM_UNSUPPORTED`], so the setup screen says so plainly instead of guessing.
/// Input: the engine root, unused. Output: the installer.
#[cfg(not(target_os = "macos"))]
pub fn platform_installer(_engine_root: &std::path::Path) -> Box<dyn Installer> {
    Box::new(UnsupportedInstaller)
}

#[cfg(not(target_os = "macos"))]
struct UnsupportedInstaller;

#[cfg(not(target_os = "macos"))]
impl Installer for UnsupportedInstaller {
    fn find(&self, _dep: Dependency) -> Result<Option<Version>, InstallError> {
        Err(InstallError(PLATFORM_UNSUPPORTED))
    }

    fn install(&self, _dep: Dependency, _pin: &Pin) -> Result<Version, InstallError> {
        Err(InstallError(PLATFORM_UNSUPPORTED))
    }
}

/// The pinned install of one dependency on this platform. Input: the dependency.
/// Output: its pin, or `None` on a system setup has no installer for yet.
#[cfg(target_os = "macos")]
pub fn platform_pin(dep: Dependency) -> Option<Pin> {
    Some(macos::pin(dep))
}

/// The pinned install of one dependency on a system setup has no installer for yet.
/// Input: the dependency, unused. Output: `None`.
#[cfg(not(target_os = "macos"))]
pub fn platform_pin(_dep: Dependency) -> Option<Pin> {
    None
}

/// One owner change to the default pack, as the owner mutations carry it: start setup
/// with the ticked entries, switch an installed entry, or remove one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetupAction {
    Run { pack: Vec<String> },
    Set { id: String, on: bool },
    Remove { id: String },
}

const PACK_ID_BYTES_MAX: usize = 64;

fn valid_pack_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= PACK_ID_BYTES_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

impl SetupAction {
    /// Checks the shape of the entry ids before anything runs: at most the manifest's
    /// entry count, each 1 to 64 letters, digits, `-`, `_` or `.`, no id twice.
    /// Output: `pack_unknown` for anything else.
    pub fn validate(&self) -> Result<(), InstallError> {
        let valid = match self {
            Self::Run { pack } => {
                pack.len() <= pack::ENTRIES_MAX
                    && pack.iter().all(|id| valid_pack_id(id))
                    && pack
                        .iter()
                        .enumerate()
                        .all(|(index, id)| !pack[..index].contains(id))
            }
            Self::Set { id, .. } | Self::Remove { id } => valid_pack_id(id),
        };
        if !valid {
            return Err(InstallError("pack_unknown"));
        }
        Ok(())
    }
}
