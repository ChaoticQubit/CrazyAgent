#[cfg(unix)]
pub mod bundled;
#[cfg(unix)]
pub mod fetch;
#[cfg(any(target_os = "linux", all(test, unix)))]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(unix)]
pub mod node;
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
const TOOL_LOCKS: [ToolLock<'static>; 2] = [
    ToolLock {
        package: GRAPHIFY_PACKAGE,
        version: GRAPHIFY_VERSION,
        programs: &["graphify"],
        requirements: include_str!("setup/locks/graphifyy-0.9.9.txt"),
    },
    ToolLock {
        package: "headroom-ai",
        version: "0.41.0",
        programs: &["headroom"],
        requirements: include_str!("setup/locks/headroom-ai-0.41.0.txt"),
    },
];
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

/// A system tool the app needs: tmux runs agent terminals, uv installs Graphify,
/// Graphify builds each project's code graph and Node runs the harnesses npm installs.
/// Setup finds each one or installs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dependency {
    Tmux,
    Uv,
    Graphify,
    Node,
}

impl Dependency {
    /// Every dependency in install order; uv comes before Graphify, which needs it.
    pub const ALL: [Self; 4] = [Self::Tmux, Self::Uv, Self::Graphify, Self::Node];

    /// The program name looked up on the search path, such as `tmux`.
    pub fn program(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Uv => "uv",
            Self::Graphify => "graphify",
            Self::Node => "node",
        }
    }

    /// Why this dependency cannot run when no answer names another reason: it is found
    /// nowhere the tool resolver looks. Output: the missing tool, with its owner line.
    pub fn not_found(self) -> crate::tools::Missing {
        crate::tools::Missing {
            tool: self.program().to_owned(),
            reason: crate::tools::Reason::NotFound,
            needs: None,
            looked: Vec::new(),
            refused: None,
        }
    }

    /// The plain name the setup screen shows, such as `Graphify command`, kept apart
    /// from the pack entry of the same tool.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Uv => "uv",
            Self::Graphify => "Graphify command",
            Self::Node => "Node.js",
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
/// checks its SHA-256 and takes one program out of it; `UvTool` installs one pinned
/// package version from its bundled hash lock; `Homebrew` installs one formula, whose bottles
/// Homebrew verifies itself; `Winget` installs one package version, whose installer
/// winget checks against its manifest's SHA-256; `Archive` downloads one archive, checks its
/// SHA-256 and keeps its whole `top` folder, as Node's release is kept.
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
    Archive {
        version: Version,
        url: String,
        sha256: String,
        top: String,
    },
}

/// Where one dependency stands on the setup screen. `Checking` means its version check
/// is still running in the background; the screen reads the status again to see it.
/// `Missing` carries the tool resolver's owner line for why the tool cannot run
/// ([`crate::tools::Missing::line`]), the same words a launch that needs it says.
/// `Refused` is a tool the resolver found and passed over (DEC-162): its line, the file, the
/// part another user or group can change, the permission fix when one can help, and why the
/// owner's last fix did not, never counted as missing.
/// `Unsupported` means setup cannot install it on this system, so no Install is offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DependencyState {
    Checking,
    Missing {
        #[serde(default)]
        line: String,
    },
    Refused {
        line: String,
        file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        part: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fix: Option<ToolFix>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fix_failed: Option<String>,
    },
    Found {
        version: Version,
    },
    Bundled {
        version: Version,
    },
    System {
        version: Version,
        note: String,
    },
    Installing,
    Installed {
        version: Version,
    },
    Failed {
        reason: String,
    },
    Unsupported,
}

impl DependencyState {
    /// The state of a dependency setup could not find or install: unsupported for
    /// [`PLATFORM_UNSUPPORTED`], else failed with the code's plain reason. Input: the setup
    /// code. Output: the state.
    pub fn refused(code: &str) -> Self {
        if code == PLATFORM_UNSUPPORTED {
            return Self::Unsupported;
        }
        Self::Failed {
            reason: plain_reason(code).to_owned(),
        }
    }
}

/// The permission fix a refused tool's row offers (DEC-162): the folder or file it changes,
/// whether that folder changes as a whole tree, and the command that makes the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolFix {
    pub tree: String,
    pub recursive: bool,
    pub command: String,
}

impl From<&crate::tools::permission_fix::Target> for ToolFix {
    fn from(target: &crate::tools::permission_fix::Target) -> Self {
        Self {
            tree: target.path.to_string_lossy().into_owned(),
            recursive: target.recursive,
            command: target.command(),
        }
    }
}

/// The owner's choice for a tool found but not used (DEC-162 Q1): offer the permission fix
/// with its command, or show only the command, which the engine then enforces by refusing
/// the fix (`tool_fix_off`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusedTool {
    #[default]
    FixButton,
    CommandOnly,
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
/// group, where it comes from, its license, whether it starts ticked, its state and, for
/// an installed entry whose hooks were refused, the plain reason they were not added.
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
    #[serde(default)]
    pub hooks_refused: Option<String>,
}

/// Everything the setup screen shows: each dependency, each pack entry, whether a setup
/// run is going on now, the pack's version that a switch or removal names, and the owner's
/// choice for a tool found but not used ([`RefusedTool`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupStatus {
    pub dependencies: Vec<DependencyStatus>,
    pub pack: Vec<PackEntry>,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub version: u64,
    #[serde(default)]
    pub refused_tool: RefusedTool,
}

/// One Python tool setup installs only from its bundled lock: the package, its version,
/// the programs it links for the engine, and every requirement uv may install for it,
/// each pinned to one version with the SHA-256 of every file uv may download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolLock<'a> {
    pub package: &'a str,
    pub version: &'a str,
    pub programs: &'a [&'a str],
    pub requirements: &'a str,
}

const LOCK_FILE: &str = "requirements.txt";

/// Removes every entry of a tool's folder except the environment just installed, so old
/// environments and older install layouts go away. Inputs: the folder and the entry kept.
fn keep_only(home: &std::path::Path, kept: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(home) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == kept {
            continue;
        }
        let _ = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
    }
}

/// Finds the bundled lock for one Python tool version. Inputs: the package and version.
/// Output: the lock, or none when setup ships no lock for exactly that version.
pub fn tool_lock(package: &str, version: &str) -> Option<ToolLock<'static>> {
    TOOL_LOCKS
        .iter()
        .find(|lock| lock.package == package && lock.version == version)
        .copied()
}

/// The two uv commands that install a locked tool into a fresh virtual environment: the
/// first creates it with the pinned Python, the second installs exactly the lock's
/// wheels and refuses any file whose SHA-256 the lock lacks and any source build. Both
/// ignore uv configuration files. Inputs: the Python version, the environment folder,
/// its Python and the lock file. Output: each command's arguments.
pub fn locked_tool_args(
    python: &str,
    venv: &std::path::Path,
    venv_python: &std::path::Path,
    requirements: &std::path::Path,
) -> [Vec<std::ffi::OsString>; 2] {
    [
        vec![
            "venv".into(),
            "--no-project".into(),
            "--no-config".into(),
            "--python".into(),
            python.into(),
            venv.into(),
        ],
        vec![
            "pip".into(),
            "install".into(),
            "--no-config".into(),
            "--python".into(),
            venv_python.into(),
            "--require-hashes".into(),
            "--no-build".into(),
            "--requirements".into(),
            requirements.into(),
        ],
    ]
}

/// Finds and installs the app's system dependencies on one platform. `find` returns the
/// installed version or none; `install` installs exactly the pin and returns the version
/// now present; `install_tool` installs one pinned Python tool an MCP server needs into
/// the app data folder; `program` finds a program setup installed or the system has.
/// Implementations never use sudo and never change global configuration.
/// What looking for one dependency found: its version, why it cannot run
/// ([`crate::tools::Missing`]), or the check's code.
pub type Lookup = Result<Result<Located, crate::tools::Missing>, InstallError>;

/// A dependency setup found: the version it reports, whether it is the copy the app ships
/// beside the engine (DEC-157), which setup shows as bundled and never installs, and why
/// another tmux runs in place of that copy, if one does ([`crate::tools::BundleNote`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub version: Version,
    pub bundled: bool,
    pub note: Option<crate::tools::BundleNote>,
}

pub trait Installer: Send + Sync {
    fn find(&self, dep: Dependency) -> Result<Option<Version>, InstallError>;

    /// Finds a dependency as [`Installer::find`] does, naming why it cannot run when it is
    /// missing ([`crate::tools::Missing`]), the answer the setup screen shows. Input: the
    /// dependency. Output: its version, why it cannot run, or the check's code.
    fn lookup(&self, dep: Dependency) -> Lookup {
        Ok(self
            .find(dep)?
            .map(|version| Located {
                version,
                bundled: false,
                note: None,
            })
            .ok_or_else(|| dep.not_found()))
    }
    fn install(&self, dep: Dependency, pin: &Pin) -> Result<Version, InstallError>;

    fn install_tool(&self, package: &str, version: &str) -> Result<(), InstallError> {
        let _ = (package, version);
        Err(InstallError("setup_tool_unsupported"))
    }

    fn program(&self, name: &str) -> Option<std::path::PathBuf> {
        let _ = name;
        None
    }

    /// The permission fix target for a tool found and passed over, by this installer's
    /// places (DEC-162). Input: why the tool cannot run. Output: the target, or none.
    fn fix_target(
        &self,
        missing: &crate::tools::Missing,
    ) -> Option<crate::tools::permission_fix::Target> {
        let _ = missing;
        None
    }

    /// Removes group and other write from the target this installer resolves for a refused
    /// dependency, only when it is the one the owner confirmed (DEC-162). Inputs: the
    /// dependency and the confirmed target as the row showed it. Output: what changed, or the
    /// `tool_fix_*` code.
    fn fix_permissions(
        &self,
        dep: Dependency,
        confirmed: &str,
    ) -> Result<crate::tools::permission_fix::Report, &'static str> {
        let _ = (dep, confirmed);
        Err(crate::tools::permission_fix::TOOL_FIX_UNAVAILABLE)
    }
}

/// The code every setup step reports on a system setup has no installer for yet.
pub const PLATFORM_UNSUPPORTED: &str = "setup_platform_unsupported";

/// The code for a Mac with no Command Line Tools git, the only git whose helpers an agent
/// profile reads: the Setup note, and a refused agent git step.
pub const AGENT_GIT_NEEDS_TOOLS: &str = "agent_git_needs_tools";

const REASONS: [(&str, &str); 27] = [
    (
        "setup_checksum_mismatch",
        "The download did not match its checksum, so it was not installed.",
    ),
    (
        "setup_archive_unsafe",
        "The download held a file it should not, so it was not installed.",
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
    (
        "setup_pin_unlocked",
        "Setup has no locked list of this tool's files, so it was not installed.",
    ),
    ("setup_busy", "Setup is already running."),
    (
        "setup_outcome_unknown",
        "Setup did not finish answering. Check again in a moment.",
    ),
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
        AGENT_GIT_NEEDS_TOOLS,
        "Agent git needs the Command Line Tools: run xcode-select --install.",
    ),
    (
        "setup_local_app_data_missing",
        "Windows did not say where your local app data folder is, so setup can't install here.",
    ),
];

/// Turns a setup code into the plain sentence the setup screen shows. Input: a code such
/// as `setup_checksum_mismatch`. Output: its sentence, or a general one for other codes.
pub fn plain_reason(code: &str) -> &'static str {
    known_reason(code).unwrap_or("Something went wrong during setup.")
}

/// Turns a pack install or hooks code into the plain sentence a pack entry shows: a setup
/// or pack code from the setup table, any other from the plugin table first. Input: the
/// code. Output: its sentence, or the general setup sentence.
pub fn pack_reason(code: &str) -> String {
    if code.starts_with("setup_") || code.starts_with("pack_") {
        return plain_reason(code).to_owned();
    }
    crate::plugins::plain_problem(code)
        .unwrap_or_else(|| plain_reason(code))
        .to_owned()
}

/// Finds the setup sentence for a code. Input: a code such as `setup_checksum_mismatch`.
/// Output: its sentence, or none for a code setup has no sentence for.
pub fn known_reason(code: &str) -> Option<&'static str> {
    REASONS
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, sentence)| *sentence)
}

/// The engine-owned npm prefix npm harnesses install into, `<engine root>/setup/harnesses`:
/// inside the setup folder, the one part of the engine data the tool resolver and harness
/// finding accept as the engine's own and no agent sandbox may write. Input: the engine
/// root. Output: the folder.
pub fn harness_prefix(engine_root: &std::path::Path) -> std::path::PathBuf {
    engine_root.join("setup").join("harnesses")
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
    let shared = pinned::PinnedInstaller::with_places(
        engine_root,
        pinned::system_folders(
            linux::SYSTEM_PATH
                .iter()
                .map(std::path::PathBuf::from)
                .collect(),
        ),
        crate::tools::Places::from_environment(Some(engine_root)),
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
/// Graphify through `uv tool install`; tmux by its formula name; Node from its official
/// archive on Mac and Linux. Inputs: the target and the dependency. Output: the pin, or
/// `None` when that target has no uv or Node pin.
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
        Dependency::Node => node_pin(target),
    }
}

/// The official Node archive for one target as a setup pin. Input: the target.
/// Output: the pin, or none where no Node archive is pinned.
#[cfg(unix)]
fn node_pin(target: &str) -> Option<Pin> {
    let pinned = node::pin(target)?;
    Some(Pin::Archive {
        version: Version(node::NODE_VERSION.to_owned()),
        url: pinned.url,
        sha256: pinned.sha256,
        top: pinned.top,
    })
}

/// Pins no Node where setup cannot unpack one. Input: the target, unused. Output: none.
#[cfg(not(unix))]
fn node_pin(_target: &str) -> Option<Pin> {
    None
}

/// The pinned install of one dependency on this system. Input: the dependency.
/// Output: its pin, or `None` when this system's target has no uv pin.
pub fn platform_pin(dep: Dependency) -> Option<Pin> {
    pin_for(TARGET, dep)
}

/// One owner change to setup, as the owner mutations carry it: start setup with the
/// ticked entries, switch or remove an installed entry at the pack version the screen
/// showed, or install one missing dependency alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetupAction {
    Run { pack: Vec<String> },
    Set { id: String, on: bool, version: u64 },
    Remove { id: String, version: u64 },
    Install { name: Dependency },
    FixPermissions { name: Dependency, tree: String },
    SetRefusedTool { choice: RefusedTool },
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
            Self::Set { id, .. } | Self::Remove { id, .. } => valid_pack_id(id),
            Self::Install { .. } | Self::FixPermissions { .. } | Self::SetRefusedTool { .. } => {
                true
            }
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

    fn requirements(lock: &ToolLock<'_>) -> Vec<(String, Vec<String>)> {
        let mut parsed: Vec<(String, Vec<String>)> = Vec::new();
        for line in lock.requirements.lines() {
            let words = line.trim().trim_end_matches('\\').trim();
            if let Some(hash) = words.strip_prefix("--hash=sha256:") {
                parsed.last_mut().unwrap().1.push(hash.to_owned());
            } else if !line.starts_with(' ') && !words.is_empty() {
                let pin = words.split(" ; ").next().unwrap().to_owned();
                parsed.push((pin, Vec::new()));
            } else {
                assert!(words.is_empty(), "{line}");
            }
        }
        parsed
    }

    #[test]
    fn every_bundled_tool_lock_pins_each_requirement_with_sha256_hashes() {
        for lock in TOOL_LOCKS {
            let parsed = requirements(&lock);
            assert!(parsed.len() > 1, "{}", lock.package);
            for (pin, hashes) in &parsed {
                let (name, version) = pin.split_once("==").expect(pin);
                assert!(!name.is_empty() && !version.is_empty(), "{pin}");
                assert!(!hashes.is_empty(), "{pin}");
                assert!(hashes.iter().all(|hash| is_sha256(hash)), "{pin}");
            }
            let own = format!("{}=={}", lock.package, lock.version);
            assert_eq!(parsed.iter().filter(|(pin, _)| *pin == own).count(), 1);
            assert!(!lock.programs.is_empty());
            assert_eq!(tool_lock(lock.package, lock.version), Some(lock));
        }
        let Some(Pin::UvTool {
            package, version, ..
        }) = pin_for("unknown", Dependency::Graphify)
        else {
            panic!("graphify installs as a uv tool");
        };
        assert!(tool_lock(&package, version.as_str()).is_some());
        let pack = pack::default_pack().unwrap();
        let tools: Vec<(&str, &str)> = pack
            .entries
            .iter()
            .filter_map(|entry| entry.install.strip_prefix("uv tool install "))
            .map(|rest| rest.split([',', ' ']).next().unwrap())
            .map(|step| step.split_once("==").unwrap())
            .collect();
        assert_eq!(tools.len(), 2);
        for (package, version) in tools {
            assert!(
                tool_lock(package, version).is_some(),
                "{package}=={version}"
            );
        }
        assert_eq!(tool_lock("headroom-ai", "0.41.1"), None);
    }

    #[test]
    fn a_locked_tool_installs_with_required_hashes_and_no_source_builds() {
        let [venv, install] = locked_tool_args(
            "3.12",
            std::path::Path::new("/e/venv"),
            std::path::Path::new("/e/venv/bin/python"),
            std::path::Path::new("/e/requirements.txt"),
        );
        assert!(venv.iter().eq([
            "venv",
            "--no-project",
            "--no-config",
            "--python",
            "3.12",
            "/e/venv"
        ]));
        assert!(install.iter().eq([
            "pip",
            "install",
            "--no-config",
            "--python",
            "/e/venv/bin/python",
            "--require-hashes",
            "--no-build",
            "--requirements",
            "/e/requirements.txt"
        ]));
    }

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

    #[test]
    fn node_is_a_setup_row_the_owner_installs_alone_from_the_official_archive() {
        assert_eq!(Dependency::ALL.last(), Some(&Dependency::Node));
        assert_eq!(Dependency::Node.program(), "node");
        assert_eq!(Dependency::Node.label(), "Node.js");
        assert_eq!(
            serde_json::to_value(Dependency::Node).unwrap(),
            serde_json::json!("node")
        );
        let unix = [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu",
        ];
        for target in unix {
            let pinned = node::pin(target).unwrap();
            assert_eq!(
                pin_for(target, Dependency::Node),
                Some(Pin::Archive {
                    version: Version::parse(node::NODE_VERSION).unwrap(),
                    url: pinned.url,
                    sha256: pinned.sha256,
                    top: pinned.top,
                }),
                "{target}"
            );
        }
        for target in WINGET_TARGETS.into_iter().chain(["unknown"]) {
            assert_eq!(pin_for(target, Dependency::Node), None, "{target}");
        }
        let install = crate::engine::owner::OwnerMutation::InstallDependency {
            name: Dependency::Node,
        };
        assert_eq!(
            serde_json::to_value(&install).unwrap(),
            serde_json::json!({"kind": "InstallDependency", "name": "node"})
        );
        let action = SetupAction::Install {
            name: Dependency::Node,
        };
        assert!(matches!(
            install.command().unwrap(),
            crate::protocol::Command::OwnerSetup { action: mapped } if mapped == action
        ));
        assert_eq!(action.validate(), Ok(()));
        assert_eq!(
            serde_json::to_value(&action).unwrap(),
            serde_json::json!({"kind": "install", "name": "node"})
        );
    }
}
