use crate::error::SandboxError;
use crate::paths::{canonical, owner_home};
use serde::{Deserialize, Serialize};
use std::fs;
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

#[cfg(all(test, unix))]
mod tests;
