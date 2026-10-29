use super::{SandboxError, canonical, owner_home};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const RUST_MARKERS: &[&str] = &["Cargo.toml", "rust-toolchain", "rust-toolchain.toml"];
const NODE_MARKERS: &[&str] = &["package.json"];
const PYTHON_MARKERS: &[&str] = &["pyproject.toml"];
const NODE_HOMES: &[&str] = &[".nvm", ".volta", ".local/share/fnm"];
const PYTHON_HOMES: &[&str] = &[".pyenv", ".local/share/uv/python"];
/// Where the default pack's `uv tool install graphifyy` keeps graphify's own environment.
const GRAPHIFY_TOOL: &str = ".local/share/uv/tools/graphifyy";
const CARGO_SHARED: &[&str] = &["registry", "git"];
const RUSTUP_BINS: &[&str] = &["/opt/homebrew/opt/rustup/bin", "/usr/local/opt/rustup/bin"];
const TMP_PARENT: &str = "/private/tmp";
const TMP_PREFIX: &str = "ca-";
const TMP_HASH_BYTES: usize = 8;

/// The variables that keep every agent's toolchains from installing themselves: rustup
/// never fetches a missing toolchain, uv never downloads a Python, Corepack never fetches a
/// package manager and Homebrew never updates itself.
pub const AUTO_INSTALL_ENV: [(&str, &str); 4] = [
    ("RUSTUP_AUTO_INSTALL", "0"),
    ("UV_PYTHON_DOWNLOADS", "never"),
    ("COREPACK_ENABLE_NETWORK", "0"),
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
];

/// The folders the owner's toolchains live in: the owner's home, rustup's home and cargo's
/// home inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolHomes {
    home: PathBuf,
    rustup: PathBuf,
    cargo: PathBuf,
}

impl ToolHomes {
    /// The owner's tool homes: `~/.rustup` and `~/.cargo` in the account's home folder.
    /// Takes nothing. Returns the homes, or the refusal when the owner account is unknown.
    pub fn owner() -> Result<Self, SandboxError> {
        Ok(Self::at(&owner_home()?))
    }

    /// The tool homes inside one home folder. Input: the home. Output: the homes.
    pub fn at(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            rustup: home.join(".rustup"),
            cargo: home.join(".cargo"),
        }
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

    /// Tells whether a project uses this toolchain: one of its marker files (`Cargo.toml`
    /// or `rust-toolchain*`, `package.json`, `pyproject.toml`) sits at the project root.
    /// Input: the project folder. Output: true when used.
    pub fn used_by(self, project: &Path) -> bool {
        let markers = match self {
            Self::Rust => RUST_MARKERS,
            Self::Node => NODE_MARKERS,
            Self::Python => PYTHON_MARKERS,
        };
        markers.iter().any(|name| project.join(name).is_file())
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
    /// Python grants pyenv and uv's Pythons. Homebrew is already a system root. Inputs: the
    /// tool homes and the chosen toolchains. Output: the existing grants.
    pub fn grant(homes: &ToolHomes, chosen: &[Toolchain]) -> Self {
        let mut roots = Vec::new();
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

fn below(home: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(home.to_path_buf(), |at, part| at.join(part))
}

fn existing(path: &Path, folder: bool) -> Option<PathBuf> {
    let real = canonical(path).ok()?;
    let metadata = fs::metadata(&real).ok()?;
    let kind = if folder {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    kind.then_some(real)
}

/// The toolchain variables every agent process gets: a writable `CARGO_HOME` in the
/// agent's own home, the owner's rustup home read-only in `RUSTUP_HOME`, and
/// [`AUTO_INSTALL_ENV`]. Inputs: the agent home and the owner's tool homes. Output: the
/// variables in order.
pub fn agent_env(home: &Path, homes: &ToolHomes) -> Vec<(&'static str, OsString)> {
    let mut env = vec![
        ("CARGO_HOME", home.join(".cargo").into_os_string()),
        ("RUSTUP_HOME", homes.rustup.clone().into_os_string()),
    ];
    env.extend(
        AUTO_INSTALL_ENV
            .iter()
            .map(|(name, value)| (*name, OsString::from(value))),
    );
    env
}

/// The read-only folders graphify needs in an agent's sandbox when the default pack
/// installed it with uv (I3): its own environment, holding the `graphify` program, and the
/// uv Python install its interpreter runs from when that sits in the owner's home. Takes the
/// tool homes. Returns the canonical folders, none when graphify is not installed there.
pub fn graphify(homes: &ToolHomes) -> Vec<PathBuf> {
    let Ok(tool) = below(&homes.home, GRAPHIFY_TOOL).canonicalize() else {
        return Vec::new();
    };
    let inside = |folder: &Path| folder.starts_with(&homes.home) && folder != homes.home;
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
/// where they hold `cargo`, then `base`, then graphify's own program folder when the default
/// pack installed it (see [`graphify`]), last so it never shadows a toolchain's `python3`.
/// Inputs: the tool homes and the base path. Output: the joined path.
pub fn agent_path(homes: &ToolHomes, base: &str) -> String {
    let mut parts: Vec<String> = std::iter::once(homes.cargo.join("bin"))
        .chain(RUSTUP_BINS.iter().map(PathBuf::from))
        .filter(|bin| bin.join("cargo").is_file())
        .filter_map(|bin| {
            bin.to_str()
                .filter(|text| !text.contains(':'))
                .map(str::to_owned)
        })
        .collect();
    parts.push(base.to_owned());
    parts.extend(
        graphify(homes)
            .first()
            .and_then(|tool| tool.join("bin").to_str().map(str::to_owned))
            .filter(|bin| !bin.contains(':')),
    );
    parts.join(":")
}

/// The short temp folder of the agent whose home is `home`: `/private/tmp/ca-` and 16 hex
/// digits of the home path's SHA-256, short enough for Unix socket paths. Input: the agent
/// home. Output: the path, which may not exist.
pub fn tmp_path(home: &Path) -> PathBuf {
    let digest = Sha256::digest(home.as_os_str().as_encoded_bytes());
    let hash: String = digest[..TMP_HASH_BYTES]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = Path::new(TMP_PARENT).join(format!("{TMP_PREFIX}{hash}"));
    assert_eq!(
        path.as_os_str().len(),
        TMP_PARENT.len() + 1 + TMP_PREFIX.len() + 2 * TMP_HASH_BYTES
    );
    path
}

/// Makes the agent's short temp folder with mode 0700, or reuses the one this user already
/// owns there and makes it private again. Input: the agent home. Output: the folder, or a
/// refusal when the path is a link, a file or another user's folder.
#[cfg(unix)]
pub fn make_tmp(home: &Path) -> io::Result<PathBuf> {
    let path = tmp_path(home);
    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
        _ => {}
    }
    if !own_folder(&path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the agent temp path is not a folder of this user",
        ));
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    assert!(private_folder(&path));
    Ok(path)
}

/// The agent's short temp folder when it exists as this user's private folder. Input: the
/// agent home. Output: the folder, or None.
#[cfg(unix)]
pub fn existing_tmp(home: &Path) -> Option<PathBuf> {
    let path = tmp_path(home);
    private_folder(&path).then_some(path)
}

/// Removes the agent's short temp folder with everything in it. Input: the agent home.
/// Output: done when it is gone or never existed, or a refusal when the path is not this
/// user's folder.
#[cfg(unix)]
pub fn remove_tmp(home: &Path) -> io::Result<()> {
    let path = tmp_path(home);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) if !own_folder(&path) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the agent temp path is not a folder of this user",
            ));
        }
        Ok(_) => {}
    }
    fs::remove_dir_all(&path)
}

#[cfg(unix)]
fn own_folder(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && metadata.uid() == nix::unistd::getuid().as_raw())
}

#[cfg(unix)]
fn private_folder(path: &Path) -> bool {
    own_folder(path)
        && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.mode() & 0o077 == 0)
}

/// The short temp folder of one agent run that ends inside a single call, removed with
/// everything in it when dropped.
#[derive(Debug)]
pub struct ScopedTmp {
    home: PathBuf,
    path: PathBuf,
}

impl ScopedTmp {
    /// Makes the folder as [`make_tmp`] does. Input: the agent home. Output: the scoped
    /// folder, or the refusal.
    pub fn make(home: &Path) -> io::Result<Self> {
        let path = make_tmp(home)?;
        Ok(Self {
            home: home.to_path_buf(),
            path,
        })
    }

    /// The folder, the run's `TMPDIR`.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScopedTmp {
    fn drop(&mut self) {
        let _ = remove_tmp(&self.home);
    }
}

/// Makes the agent's own cargo home, `.cargo` in its home with mode 0700, and links its
/// `registry` and `git` to the owner's, which the sandbox grants read-only, so offline
/// builds reuse the owner's crates and never change them. A link or folder the agent
/// already has stays as it is. Inputs: the agent home and the owner's tool homes. Output:
/// the cargo home, or a refusal when `.cargo` is not a plain folder.
#[cfg(unix)]
pub fn prepare_cargo_home(home: &Path, homes: &ToolHomes) -> io::Result<PathBuf> {
    use rustix::fs::{CWD, Mode, OFlags};
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
    for name in CARGO_SHARED {
        let Some(shared) = existing(&homes.cargo.join(name), true) else {
            continue;
        };
        match rustix::fs::symlinkat(&shared, &folder, *name) {
            Err(rustix::io::Errno::EXIST) | Ok(()) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(cargo)
}

/// No short temp folder outside Unix; agents keep the temp dir their launch sets. Takes the
/// agent home, unused. Returns None.
#[cfg(not(unix))]
pub fn existing_tmp(_home: &Path) -> Option<PathBuf> {
    None
}

/// No short temp folder outside Unix, where no agent sandbox runs. Takes the agent home,
/// unused. Returns `Unsupported`.
#[cfg(not(unix))]
pub fn make_tmp(_home: &Path) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Nothing to remove outside Unix. Takes the agent home, unused. Returns done.
#[cfg(not(unix))]
pub fn remove_tmp(_home: &Path) -> io::Result<()> {
    Ok(())
}

/// No agent cargo home outside Unix, where no agent sandbox runs. Takes the agent home and
/// tool homes, unused. Returns `Unsupported`.
#[cfg(not(unix))]
pub fn prepare_cargo_home(_home: &Path, _homes: &ToolHomes) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}
