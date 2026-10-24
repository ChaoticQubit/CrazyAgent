#[cfg(unix)]
pub mod bundled;
#[cfg(unix)]
pub mod fetch;
#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod pack;
#[cfg(unix)]
pub mod pinned;
#[cfg(unix)]
pub(crate) mod process;
pub mod progress;
#[cfg(any(target_os = "linux", target_os = "windows", test))]
mod run;
#[cfg(any(target_os = "windows", test))]
pub mod windows;

use serde::{Deserialize, Serialize};

pub use pack::{PackGroup, PackKind};

const VERSION_BYTES_MAX: usize = 32;

/// The uv release setup installs on every system.
pub const UV_VERSION: &str = "0.13.5";
/// uv's package id in the winget community source, which Windows installs uv from.
pub const UV_WINGET_ID: &str = "astral-sh.uv";
/// The Python package that provides the `graphify` program.
pub const GRAPHIFY_PACKAGE: &str = "graphifyy";
/// The Graphify release setup installs on every system.
pub const GRAPHIFY_VERSION: &str = "0.9.9";
const GRAPHIFY_PYTHON: &str = "3.12";
const UV_RELEASES: [(&str, &str); 4] = [
    (
        "aarch64-apple-darwin",
        "743f85035a1996ecb350659e01cd9faea9709829be09b1b4b03c73eaf3c9cd73",
    ),
    (
        "x86_64-apple-darwin",
        "b0f52b0e3945b4107d89024ea38e625683011fbfc05a0d98f9fa2d6dcee28c66",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "4bb69cf95ed13f23713bb7bc09f241896ea0c75b7c557e615a20604b82f9c403",
    ),
    (
        "x86_64-unknown-linux-gnu",
        "f68d291c75783122dad58ab77def0e13f3233f358731db5d6d7e8715603b928f",
    ),
];
const WINGET_TARGETS: [&str; 2] = ["aarch64-pc-windows-msvc", "x86_64-pc-windows-msvc"];

/// The target triple this engine was built for, such as `aarch64-apple-darwin`, or
/// `unknown` on a system no uv release or winget pin names.
pub const TARGET: &str = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    "aarch64-apple-darwin"
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    "x86_64-apple-darwin"
} else if cfg!(all(
    target_os = "linux",
    target_env = "gnu",
    target_arch = "aarch64"
)) {
    "aarch64-unknown-linux-gnu"
} else if cfg!(all(
    target_os = "linux",
    target_env = "gnu",
    target_arch = "x86_64"
)) {
    "x86_64-unknown-linux-gnu"
} else if cfg!(all(
    target_os = "windows",
    target_env = "msvc",
    target_arch = "aarch64"
)) {
    "aarch64-pc-windows-msvc"
} else if cfg!(all(
    target_os = "windows",
    target_env = "msvc",
    target_arch = "x86_64"
)) {
    "x86_64-pc-windows-msvc"
} else {
    "unknown"
};

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
/// Homebrew verifies itself; `Winget` installs one package version, whose installer
/// winget checks against its manifest's SHA-256.
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
    Winget {
        id: String,
        version: Version,
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

const REASONS: [(&str, &str); 24] = [
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
    (
        "setup_local_app_data_missing",
        "Windows did not say where your local app data folder is, so setup can't install here.",
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

/// The installer for this Mac, installing into `<engine root>/setup`.
/// Input: the engine root. Output: the installer.
#[cfg(target_os = "macos")]
pub fn platform_installer(engine_root: &std::path::Path) -> Box<dyn Installer> {
    Box::new(macos::MacInstaller::new(engine_root))
}

/// The installer for this Linux system: uv and Graphify through the shared pinned
/// installer into `<engine root>/setup`, tmux from Homebrew or, as root, apt or dnf.
/// Input: the engine root. Output: the installer.
#[cfg(target_os = "linux")]
pub fn platform_installer(engine_root: &std::path::Path) -> Box<dyn Installer> {
    let shared = pinned::PinnedInstaller::with_paths(
        engine_root,
        pinned::system_folders(&linux::SYSTEM_PATH),
    );
    Box::new(linux::LinuxInstaller::new(Box::new(shared)))
}

/// The installer for this Windows system: Graphify and its Python in
/// `<engine root>/setup`, uv from winget under the owner's `%LOCALAPPDATA%`. When that
/// variable is unset or relative, every step reports why instead of guessing a folder.
/// Input: the engine root. Output: the installer.
#[cfg(target_os = "windows")]
pub fn platform_installer(engine_root: &std::path::Path) -> Box<dyn Installer> {
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
    match windows::WindowsInstaller::new(&engine_root.join("setup"), local_app_data.as_deref()) {
        Ok(installer) => Box::new(installer),
        Err(code) => Box::new(RefusingInstaller(code)),
    }
}

/// The installer on a system setup has none for: every lookup and install reports
/// [`PLATFORM_UNSUPPORTED`], so the setup screen says so plainly instead of guessing.
/// Input: the engine root, unused. Output: the installer.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn platform_installer(_engine_root: &std::path::Path) -> Box<dyn Installer> {
    Box::new(RefusingInstaller(InstallError(PLATFORM_UNSUPPORTED)))
}

/// An installer that only refuses: every lookup and install reports its one plain code,
/// either that setup has no installer for this system or why this system's could not start.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
struct RefusingInstaller(InstallError);

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl Installer for RefusingInstaller {
    fn find(&self, _dep: Dependency) -> Result<Option<Version>, InstallError> {
        Err(self.0)
    }

    fn install(&self, _dep: Dependency, _pin: &Pin) -> Result<Version, InstallError> {
        Err(self.0)
    }
}

/// The pinned install of one dependency for one target triple: uv from its release
/// archive with the published SHA-256 where one is pinned, or from winget on Windows;
/// Graphify through `uv tool install`; tmux by its formula name. Inputs: the target and
/// the dependency. Output: the pin, or `None` when that target has no uv pin.
pub fn pin_for(target: &str, dep: Dependency) -> Option<Pin> {
    match dep {
        Dependency::Uv => {
            if let Some((_, sha256)) = UV_RELEASES.iter().find(|(known, _)| *known == target) {
                let name = format!("uv-{target}");
                return Some(Pin::Release {
                    version: Version(UV_VERSION.to_owned()),
                    url: format!(
                        "https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/{name}.tar.gz"
                    ),
                    sha256: (*sha256).to_owned(),
                    member: format!("{name}/uv"),
                });
            }
            WINGET_TARGETS.contains(&target).then(|| Pin::Winget {
                id: UV_WINGET_ID.to_owned(),
                version: Version(UV_VERSION.to_owned()),
            })
        }
        Dependency::Graphify => Some(Pin::UvTool {
            package: GRAPHIFY_PACKAGE.to_owned(),
            version: Version(GRAPHIFY_VERSION.to_owned()),
            python: GRAPHIFY_PYTHON.to_owned(),
        }),
        Dependency::Tmux => Some(Pin::Homebrew {
            formula: "tmux".to_owned(),
        }),
    }
}

/// The pinned install of one dependency on this system. Input: the dependency.
/// Output: its pin, or `None` when this system's target has no uv pin.
pub fn platform_pin(dep: Dependency) -> Option<Pin> {
    pin_for(TARGET, dep)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use process::is_sha256;

    const LINUX: [&str; 2] = ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"];

    #[test]
    fn every_uv_release_pin_names_its_target_and_a_distinct_checksum() {
        for (target, sha256) in UV_RELEASES {
            let Some(Pin::Release {
                version,
                url,
                sha256: pinned,
                member,
            }) = pin_for(target, Dependency::Uv)
            else {
                panic!("{target} has no uv release pin");
            };
            assert_eq!(version.as_str(), UV_VERSION);
            assert_eq!(
                url,
                format!(
                    "https://github.com/astral-sh/uv/releases/download/0.13.5/uv-{target}.tar.gz"
                )
            );
            assert_eq!(member, format!("uv-{target}/uv"));
            assert_eq!(pinned, sha256);
            assert!(is_sha256(&pinned), "{target}");
            let twins = UV_RELEASES.iter().filter(|(_, other)| *other == sha256);
            assert_eq!(twins.count(), 1, "{target}");
        }
        for target in LINUX {
            assert!(
                matches!(pin_for(target, Dependency::Uv), Some(Pin::Release { .. })),
                "{target}"
            );
        }
    }

    #[test]
    fn macos_keeps_its_exact_uv_checksums() {
        for (target, sha256) in [
            (
                "aarch64-apple-darwin",
                "743f85035a1996ecb350659e01cd9faea9709829be09b1b4b03c73eaf3c9cd73",
            ),
            (
                "x86_64-apple-darwin",
                "b0f52b0e3945b4107d89024ea38e625683011fbfc05a0d98f9fa2d6dcee28c66",
            ),
        ] {
            let Some(Pin::Release { sha256: pinned, .. }) = pin_for(target, Dependency::Uv) else {
                panic!("{target} has no uv release pin");
            };
            assert_eq!(pinned, sha256);
        }
        assert_eq!(
            platform_pin(Dependency::Uv),
            pin_for(TARGET, Dependency::Uv)
        );
        if cfg!(target_os = "macos") {
            assert!(TARGET.ends_with("-apple-darwin"), "{TARGET}");
        }
    }

    #[test]
    fn windows_takes_uv_from_winget_and_an_unknown_target_has_no_uv_pin() {
        for target in WINGET_TARGETS {
            assert_eq!(
                pin_for(target, Dependency::Uv),
                Some(Pin::Winget {
                    id: "astral-sh.uv".into(),
                    version: Version::parse(UV_VERSION).unwrap(),
                }),
                "{target}"
            );
        }
        for target in [
            "unknown",
            "riscv64gc-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
        ] {
            assert_eq!(pin_for(target, Dependency::Uv), None, "{target}");
        }
        let everywhere = UV_RELEASES
            .iter()
            .map(|(target, _)| *target)
            .chain(WINGET_TARGETS)
            .chain(["unknown"]);
        for target in everywhere {
            assert_eq!(
                pin_for(target, Dependency::Graphify),
                Some(Pin::UvTool {
                    package: "graphifyy".into(),
                    version: Version::parse("0.9.9").unwrap(),
                    python: "3.12".into(),
                }),
                "{target}"
            );
            assert_eq!(
                pin_for(target, Dependency::Tmux),
                Some(Pin::Homebrew {
                    formula: "tmux".into(),
                }),
                "{target}"
            );
        }
    }
}
