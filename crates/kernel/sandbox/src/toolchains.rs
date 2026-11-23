use crate::error::SandboxError;
use crate::paths::{canonical, owner_home};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

const RUST_MARKERS: &[&str] = &["Cargo.toml", "rust-toolchain", "rust-toolchain.toml"];

const NODE_MARKERS: &[&str] = &["package.json", ".nvmrc", ".node-version"];

const PYTHON_MARKERS: &[&str] = &[
    "pyproject.toml",
    "uv.lock",
    ".python-version",
    "requirements.txt",
];

/// The most folders directly in a project that toolchain detection looks into for a marker
/// file, so a project whose `Cargo.toml` or `package.json` sits one folder down is found.
const MARKER_FOLDERS_MAX: usize = 64;

/// Folders directly in a project that never hold a project's own marker files.
const MARKER_SKIPPED: &[&str] = &["node_modules", "target", "worktrees", "graphify-out"];

/// Where `xcode-select` keeps the developer folder the owner picked.
const XCODE_SELECT_LINK: &str = "/private/var/db/xcode_select_link";

/// Apple's Command Line Tools, whose own compilers, linkers and git an agent runs in place
/// of the `/usr/bin` xcrun shims (R7-XCRUN).
const DEVELOPER_TOOLS: &str = "/Library/Developer/CommandLineTools";

const NODE_HOMES: &[&str] = &[".nvm", ".volta", ".local/share/fnm"];

const PYTHON_HOMES: &[&str] = &[".pyenv", ".local/share/uv/python"];

/// The crate caches in cargo's home an agent's own cargo home links to: the registry and
/// the git checkouts.
pub const CARGO_SHARED: &[&str] = &["registry", "git"];

/// The engine's parts the agent toolchain setup reaches. Production code passes the engine's
/// one value; rows pass stand-ins.
pub struct ToolchainHooks {
    /// Removes one entry of an agent's own cargo home without following a link, also a
    /// folder an agent shut. Inputs: the open cargo home, the entry name and the code for a
    /// refusal the engine has no own code for. Output: done, or the refusal code.
    #[cfg(unix)]
    pub remove_home_entry:
        fn(std::os::fd::BorrowedFd<'_>, &str, &'static str) -> Result<(), &'static str>,
    /// The real git the engine resolved, which an agent's own `git` runs. Takes nothing.
    /// Returns the path.
    #[cfg(unix)]
    pub sandboxed_git: fn() -> &'static Path,
}

/// The folders the owner's toolchains live in: the owner's home, rustup's home and cargo's
/// home inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolHomes {
    home: PathBuf,
    rustup: PathBuf,
    cargo: PathBuf,
    developer: PathBuf,
    xcode: Option<PathBuf>,
}

impl ToolHomes {
    /// The owner's tool homes: `~/.rustup` and `~/.cargo` in the account's home folder,
    /// and the Xcode app `xcode-select` picked, if it picked one (F26). Takes nothing.
    /// Returns the homes, or the refusal when the owner account is unknown.
    pub fn owner() -> Result<Self, SandboxError> {
        let homes = Self::at(&owner_home()?);
        Ok(match xcode_from_link(Path::new(XCODE_SELECT_LINK)) {
            Some(xcode) => homes.with_xcode(&xcode),
            None => homes,
        })
    }

    /// The tool homes inside one home folder, with the Command Line Tools at their system
    /// folder. Input: the home. Output: the homes.
    pub fn at(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            rustup: home.join(".rustup"),
            cargo: home.join(".cargo"),
            developer: PathBuf::from(DEVELOPER_TOOLS),
            xcode: None,
        }
    }

    /// These tool homes with an Xcode app's developer folder, read-only to agents, whose
    /// compilers and macOS SDK they use when the Command Line Tools lack `cc`. Input: the
    /// canonical developer folder. Output: the homes.
    pub fn with_xcode(mut self, developer: &Path) -> Self {
        self.xcode = Some(developer.to_path_buf());
        self
    }

    /// The Xcode app's developer folder agents read, if any.
    pub fn xcode(&self) -> Option<&Path> {
        self.xcode.as_deref()
    }

    /// These tool homes with the Command Line Tools looked for in `developer` instead.
    /// Input: the folder. Output: the homes.
    pub fn with_developer(mut self, developer: &Path) -> Self {
        self.developer = developer.to_path_buf();
        self
    }

    /// The folder the Command Line Tools are looked for in.
    pub fn developer(&self) -> &Path {
        &self.developer
    }

    /// The home folder the other tool homes sit in.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Rustup's home, holding the installed toolchains and the default-toolchain setting.
    pub fn rustup(&self) -> &Path {
        &self.rustup
    }

    /// Cargo's home, holding the proxies, the crate registry and the git checkouts.
    pub fn cargo(&self) -> &Path {
        &self.cargo
    }
}

/// The read-only toolchain grants one project needs: canonical existing folders and files
/// of the owner's toolchains, never a folder that holds the owner's home or a cargo or
/// rustup home itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Toolchains {
    roots: Vec<PathBuf>,
    files: Vec<PathBuf>,
}

impl Toolchains {
    /// Detects the toolchains a project uses from the marker files at its root (see
    /// [`Toolchain::used_by`]) and grants them as [`Toolchains::grant`] does. Inputs: the
    /// project folder and tool homes. Output: the grants found.
    pub fn detect(project: &Path, homes: &ToolHomes) -> Self {
        let used: Vec<Toolchain> = Toolchain::ALL
            .into_iter()
            .filter(|toolchain| toolchain.used_by(project))
            .collect();
        Self::grant(homes, &used)
    }

    /// Grants the chosen toolchains read-only: Rust grants rustup's toolchains and settings
    /// and cargo's proxies, registry and git checkouts; Node grants nvm, Volta and fnm;
    /// Python grants pyenv and uv's Pythons. Homebrew is already a system root, and the
    /// Xcode app `xcode-select` picked is granted with every choice, as the Command Line
    /// Tools are a system root (F26). Inputs: the tool homes and the chosen toolchains.
    /// Output: the existing grants.
    pub fn grant(homes: &ToolHomes, chosen: &[Toolchain]) -> Self {
        let mut roots: Vec<PathBuf> = homes.xcode.iter().cloned().collect();
        let mut files = Vec::new();
        for toolchain in Toolchain::ALL {
            if chosen.contains(&toolchain) {
                let (more_roots, more_files) = toolchain.grants(homes);
                roots.extend(more_roots);
                files.extend(more_files);
            }
        }
        let holds_home = |root: &PathBuf| {
            [&homes.home, &homes.rustup, &homes.cargo]
                .iter()
                .any(|home| home.starts_with(root))
        };
        let detected = Self {
            roots: roots
                .iter()
                .filter_map(|root| existing(root, true))
                .filter(|root| !holds_home(root))
                .collect(),
            files: files
                .iter()
                .filter_map(|file| existing(file, false))
                .collect(),
        };
        assert!(detected.roots.iter().all(|root| !holds_home(root)));
        detected
    }

    /// The read-only folders, each granted with its credential names carved out.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The single read-only files, such as rustup's default-toolchain setting.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }
}

/// One toolchain family whose version managers live in the owner's home: Rust (rustup and
/// cargo), Node (nvm, Volta, fnm) and Python (pyenv, uv). Toolchains installed system-wide,
/// such as Homebrew's, are system roots every agent reads anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Toolchain {
    Rust,
    Node,
    Python,
}

impl Toolchain {
    /// Every toolchain family, in display order.
    pub const ALL: [Self; 3] = [Self::Rust, Self::Node, Self::Python];

    /// The name shown to the owner.
    pub fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Node => "Node.js",
            Self::Python => "Python",
        }
    }

    /// Tells whether a project uses this toolchain: one of its marker files (`Cargo.toml` or
    /// `rust-toolchain*`; `package.json`, `.nvmrc` or `.node-version`; `pyproject.toml`,
    /// `uv.lock`, `.python-version` or `requirements.txt`) sits at the project root or in
    /// one of the first [`MARKER_FOLDERS_MAX`] plain folders directly in it (F26). Input: the
    /// project folder. Output: true when used.
    pub fn used_by(self, project: &Path) -> bool {
        let markers = match self {
            Self::Rust => RUST_MARKERS,
            Self::Node => NODE_MARKERS,
            Self::Python => PYTHON_MARKERS,
        };
        let holds = |folder: &Path| markers.iter().any(|name| folder.join(name).is_file());
        if holds(project) {
            return true;
        }
        let Ok(entries) = fs::read_dir(project) else {
            return false;
        };
        entries
            .flatten()
            .take(MARKER_FOLDERS_MAX)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| !name.starts_with('.') && !MARKER_SKIPPED.contains(&name))
            })
            .any(|entry| holds(&entry.path()))
    }

    /// Tells whether the owner has this toolchain in their home: one of its version-manager
    /// folders exists. Input: the tool homes. Output: true when installed.
    pub fn installed(self, homes: &ToolHomes) -> bool {
        self.grants(homes).0.iter().any(|root| root.is_dir())
    }

    fn grants(self, homes: &ToolHomes) -> (Vec<PathBuf>, Vec<PathBuf>) {
        match self {
            Self::Rust => {
                let mut roots = vec![homes.rustup.join("toolchains"), homes.cargo.join("bin")];
                roots.extend(CARGO_SHARED.iter().map(|name| homes.cargo.join(name)));
                (roots, vec![homes.rustup.join("settings.toml")])
            }
            Self::Node => (
                NODE_HOMES
                    .iter()
                    .map(|path| below(&homes.home, path))
                    .collect(),
                Vec::new(),
            ),
            Self::Python => (
                PYTHON_HOMES
                    .iter()
                    .map(|path| below(&homes.home, path))
                    .collect(),
                Vec::new(),
            ),
        }
    }
}

/// A path below a folder, from a relative path written with `/`. Inputs: the folder and
/// the relative path. Output: the joined path.
pub fn below(home: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(home.to_path_buf(), |at, part| at.join(part))
}

/// The canonical form of a path when it exists as a folder, or as a file when `folder` is
/// false. Inputs: the path and the kind wanted. Output: the canonical path, none otherwise.
pub fn existing(path: &Path, folder: bool) -> Option<PathBuf> {
    let real = canonical(path).ok()?;
    let metadata = fs::metadata(&real).ok()?;
    let kind = if folder {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    kind.then_some(real)
}

/// The developer folder of the Xcode app the link `xcode-select` keeps names: its canonical
/// `<name>.app/Contents/Developer`, never the Command Line Tools, a link that leads nowhere,
/// or a path with a character a sandbox profile or search path cannot hold. Takes the link.
/// Returns the folder, none otherwise.
fn xcode_from_link(link: &Path) -> Option<PathBuf> {
    let real = canonical(link).ok()?;
    let in_app = real.ends_with("Contents/Developer")
        && real
            .parent()
            .and_then(Path::parent)
            .and_then(Path::extension)
            .is_some_and(|extension| extension == "app");
    let plain = real.to_str().is_some_and(|text| {
        !text
            .chars()
            .any(|c| c.is_control() || matches!(c, ':' | '"' | '\\' | '\''))
    });
    (in_app && plain && real.is_dir() && real != Path::new(DEVELOPER_TOOLS)).then_some(real)
}

/// The compilers of an Xcode app, below its developer folder.
const XCODE_TOOLCHAIN_BIN: &str = "Toolchains/XcodeDefault.xctoolchain/usr/bin";

/// The macOS SDK of an Xcode app, below its developer folder.
const XCODE_SDK: &str = "Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";

/// Where the default pack's `uv tool install graphifyy` keeps graphify's own environment.
const GRAPHIFY_TOOL: &str = ".local/share/uv/tools/graphifyy";

const RUSTUP_BINS: &[&str] = &["/opt/homebrew/opt/rustup/bin", "/usr/local/opt/rustup/bin"];

/// The base path entry holding the xcrun shims the Command Line Tools' folder goes before.
const SYSTEM_BIN: &str = "/usr/bin";

/// The variables that keep every agent's toolchains from installing themselves: rustup
/// never fetches a missing toolchain, uv never downloads a Python, Corepack never fetches a
/// package manager and Homebrew never updates itself.
pub const AUTO_INSTALL_ENV: [(&str, &str); 4] = [
    ("RUSTUP_AUTO_INSTALL", "0"),
    ("UV_PYTHON_DOWNLOADS", "never"),
    ("COREPACK_ENABLE_NETWORK", "0"),
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
];

/// The toolchain variables every agent process gets: a writable `CARGO_HOME` in the
/// agent's own home, the owner's rustup home read-only in `RUSTUP_HOME`,
/// [`AUTO_INSTALL_ENV`], and `SDKROOT` at the macOS SDK of the developer tools
/// [`developer`] picks, so rustc and the cc crate never ask xcrun. Inputs: the agent home
/// and the owner's tool homes. Output: the variables in order.
pub fn agent_env(home: &Path, homes: &ToolHomes) -> Vec<(&'static str, OsString)> {
    let mut env = vec![
        ("CARGO_HOME", home.join(".cargo").into_os_string()),
        ("RUSTUP_HOME", homes.rustup().as_os_str().to_owned()),
    ];
    env.extend(
        AUTO_INSTALL_ENV
            .iter()
            .map(|(name, value)| (*name, OsString::from(value))),
    );
    env.extend(
        developer(homes)
            .map(|(_, sdk)| sdk)
            .filter(|sdk| sdk.is_dir())
            .map(|sdk| ("SDKROOT", sdk.into_os_string())),
    );
    env
}

/// The program folder and macOS SDK of the developer tools an agent runs: the Command Line
/// Tools when their folder holds `cc`, else the picked Xcode app's default toolchain when
/// it holds `cc` (F26). Every lookup by an xcrun shim under an agent's own home misses
/// xcrun's cache, whose write in the per-user temp folder the sandbox denies (R7-XCRUN), so
/// an agent runs these programs directly. Takes the tool homes. Returns the folder and the
/// SDK path, none when neither has the tools.
fn developer(homes: &ToolHomes) -> Option<(String, PathBuf)> {
    let tools = (
        homes.developer().join("usr").join("bin"),
        homes.developer().join("SDKs").join("MacOSX.sdk"),
    );
    let xcode = homes
        .xcode()
        .map(|xcode| (below(xcode, XCODE_TOOLCHAIN_BIN), below(xcode, XCODE_SDK)));
    let (bin, sdk) = std::iter::once(tools)
        .chain(xcode)
        .find(|(bin, _)| bin.join("cc").is_file())?;
    let text = bin.to_str().filter(|text| !text.contains(':'))?.to_owned();
    Some((text, sdk))
}

/// The read-only folders graphify needs in an agent's sandbox when the default pack
/// installed it with uv (I3): its own environment, holding the `graphify` program, and the
/// uv Python install its interpreter runs from when that sits in the owner's home. Takes the
/// tool homes. Returns the canonical folders, none when graphify is not installed there.
pub fn graphify(homes: &ToolHomes) -> Vec<PathBuf> {
    let Ok(tool) = below(homes.home(), GRAPHIFY_TOOL).canonicalize() else {
        return Vec::new();
    };
    let inside = |folder: &Path| folder.starts_with(homes.home()) && folder != homes.home();
    let program = std::fs::symlink_metadata(tool.join("bin").join("graphify"))
        .is_ok_and(|metadata| metadata.is_file());
    if !program || !inside(&tool) || !tool.is_dir() {
        return Vec::new();
    }
    let install = tool
        .join("bin")
        .join("python3")
        .canonicalize()
        .ok()
        .and_then(|python| python.parent()?.parent().map(Path::to_path_buf))
        .filter(|install| inside(install) && !tool.starts_with(install));
    let folders: Vec<PathBuf> = std::iter::once(tool).chain(install).collect();
    assert!((1..=2).contains(&folders.len()));
    folders
}

/// A tools-on agent's search path: the owner's cargo proxies and Homebrew's rustup proxies
/// where they hold `cargo`, then `base` with [`developer`]'s folder just before its `/usr/bin`,
/// then graphify's own program folder when the default pack installed it (see
/// [`graphify`]), last so it never shadows a toolchain's `python3`. Inputs: the tool homes
/// and the base path. Output: the joined path.
pub fn agent_path(homes: &ToolHomes, base: &str) -> String {
    let mut parts: Vec<String> = std::iter::once(homes.cargo().join("bin"))
        .chain(RUSTUP_BINS.iter().map(PathBuf::from))
        .filter(|bin| bin.join("cargo").is_file())
        .filter_map(|bin| {
            bin.to_str()
                .filter(|text| !text.contains(':'))
                .map(str::to_owned)
        })
        .collect();
    let mut developer = developer(homes).map(|(bin, _)| bin);
    for entry in base.split(':') {
        if entry == SYSTEM_BIN {
            parts.extend(developer.take());
        }
        parts.push(entry.to_owned());
    }
    parts.extend(
        graphify(homes)
            .first()
            .and_then(|tool| tool.join("bin").to_str().map(str::to_owned))
            .filter(|bin| !bin.contains(':')),
    );
    parts.join(":")
}

/// Makes the agent's own cargo home, `.cargo` in its home with mode 0700, and links its
/// `registry` and `git` to the owner's, which the sandbox grants read-only, so offline
/// builds reuse the owner's crates and never change them. Those names hold only that link
/// (DEC-173): where the owner has the cache, whatever else is there, a folder or a link
/// elsewhere, gives way to it; where the owner has none, a link there goes, and a folder an
/// agent made goes at the engine's launch reset, while a folder in
/// the owner's IDE terminal home stays. A launch that finds the exact link another launch
/// of the same home made at a name it saw free takes it (RV-DS241-199). Inputs: the agent
/// home, the hooks the removal goes through and the owner's tool homes. Output: the cargo
/// home, or a refusal when `.cargo` is
/// not a plain folder or a name will not give way, whose text is `cleanup_walk_too_long`
/// when removing the name ran past its walk budget (RESET-WALK-CODE).
#[cfg(unix)]
pub fn prepare_cargo_home(
    hooks: &ToolchainHooks,
    home: &Path,
    homes: &ToolHomes,
) -> io::Result<PathBuf> {
    use rustix::fs::{CWD, Mode, OFlags};
    use std::os::fd::AsFd;
    use std::os::unix::ffi::OsStrExt;
    let cargo = home.join(".cargo");
    match fs::DirBuilder::new().mode(0o700).create(&cargo) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
        _ => {}
    }
    let folder = rustix::fs::openat(
        CWD,
        &cargo,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let remove = |name: &str| {
        (hooks.remove_home_entry)(folder.as_fd(), name, "cargo cache name will not give way")
            .map_err(io::Error::other)
    };
    let linked = |name: &str, shared: &Path| {
        rustix::fs::readlinkat(&folder, name, Vec::new())
            .is_ok_and(|target| target.as_bytes() == shared.as_os_str().as_bytes())
    };
    for name in CARGO_SHARED {
        let found = rustix::fs::readlinkat(&folder, *name, Vec::new());
        let Some(shared) = existing(&homes.cargo().join(name), true) else {
            if found.is_ok() {
                remove(name)?;
            }
            continue;
        };
        match found {
            Ok(target) if target.as_bytes() == shared.as_os_str().as_bytes() => continue,
            Err(rustix::io::Errno::NOENT) => {}
            _ => remove(name)?,
        }
        match rustix::fs::symlinkat(&shared, &folder, *name) {
            Err(rustix::io::Errno::EXIST) if linked(name, &shared) => {}
            made => made?,
        }
    }
    Ok(cargo)
}

/// No agent cargo home outside Unix, where no agent sandbox runs. Takes the hooks, agent
/// home and tool homes, unused. Returns `Unsupported`.
#[cfg(not(unix))]
pub fn prepare_cargo_home(
    _hooks: &ToolchainHooks,
    _home: &Path,
    _homes: &ToolHomes,
) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, unix))]
mod tests;
