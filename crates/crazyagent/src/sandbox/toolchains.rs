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
/// Apple's Command Line Tools, whose own compilers, linkers and git an agent runs in place
/// of the `/usr/bin` xcrun shims (R7-XCRUN).
const DEVELOPER_TOOLS: &str = "/Library/Developer/CommandLineTools";
/// The base path entry holding the xcrun shims the Command Line Tools' folder goes before.
const SYSTEM_BIN: &str = "/usr/bin";
const TMP_PARENT: &str = "/private/tmp";
const TMP_PREFIX: &str = "ca-";
const TMP_HASH_BYTES: usize = 8;
/// The folder of a native lead's short temp folder that is the lead's own temp folder.
const NATIVE_LEAD_TMP: &str = "lead";
/// The folder of an agent's short temp folder that holds its own `git`, and that program.
pub(super) const GIT_FOLDER: &str = "bin";
const GIT_PROGRAM: &str = "git";
/// The folder of an agent's launch folder ([`crate::launch_context::STATE`]) that holds its
/// own `getconf` ([`write_getconf`]), and that program.
const GETCONF_FOLDER: &str = "bin";
const GETCONF_PROGRAM: &str = "getconf";
/// The most bytes of an agent's `getconf` script.
#[cfg(target_os = "macos")]
const GETCONF_SCRIPT_BYTES_MAX: usize = 8192;
/// The script an agent's own `getconf` holds, with `@MATES@` for a native lead's teammates'
/// home and `@SHARED@` and `@OWN@` for their temp folder and the agent's own, each quoted,
/// the folders ending in a slash as the system answer does: DARWIN_USER_TEMP_DIR names the
/// caller's TMPDIR, where Seatbelt lets it write, and every other call runs the system one.
#[cfg(target_os = "macos")]
const GETCONF_SCRIPT: &str = r#"#!/bin/sh
if [ "$#" -eq 1 ] && [ "$1" = DARWIN_USER_TEMP_DIR ]; then
  case $HOME in
    @MATES@) printf '%s\n' @SHARED@ ;;
    *) printf '%s\n' @OWN@ ;;
  esac
  exit 0
fi
exec /usr/bin/getconf "$@"
"#;
/// The xcrun shim at `/usr/bin/git`, whose cache file write Seatbelt denies (Q73).
#[cfg(unix)]
const GIT_SHIM: &str = "/usr/bin/git";
/// The most bytes of an agent's `git` script read to tell whether it is current.
#[cfg(unix)]
const GIT_SCRIPT_BYTES_MAX: u64 = 8192;
/// The script an agent's own `git` holds, with `@GIT@` for the real git and `@LEAD@` for 1
/// in a lead's copy and 0 in any other agent's. It refuses merge, pull, rebase and
/// cherry-pick, and aliases for them, with a plain reason (DEC-107 rule 2); a lead may
/// still merge its own workers' `BRANCH--NAME` branches. Every other command runs as is.
#[cfg(unix)]
const GIT_SCRIPT: &str = r##"#!/bin/sh
git='@GIT@'
lead=@LEAD@
worker_reason='You work only on your own branch: never merge, pull, rebase or cherry-pick another branch into it (DEC-107). If you need work from another branch, stop and tell your lead what you need; the master tells the owner.'
lead_reason='A lead merges only the branches of its own workers, named BRANCH--NAME after its own branch, and never pulls, rebases or cherry-picks other branches (DEC-107). The engine merge tool merges app-spawned workers. If you need other work, stop and tell the master what you need.'
refuse() {
  printf 'crazyagent: git %s is refused. %s\n' "$1" "$2" >&2
  exit 1
}
reason() {
  if [ "$lead" = 1 ]; then
    printf '%s' "$lead_reason"
  else
    printf '%s' "$worker_reason"
  fi
}
aliased() {
  case "$1" in
    [Aa][Ll][Ii][Aa][Ss].*) refuse "$2" "Aliases set on the command line are refused. $(reason)" ;;
  esac
}
sub=
at=0
skip=
moved=0
for word do
  at=$((at + 1))
  if [ -n "$skip" ]; then
    if [ "$skip" = c ]; then
      aliased "$word" "-c $word"
    fi
    skip=
    continue
  fi
  case "$word" in
    -c|--config-env) skip=c ;;
    --config-env=*) aliased "${word#--config-env=}" "$word" ;;
    -C|--git-dir|--work-tree)
      skip=v
      moved=1
      ;;
    --namespace|--super-prefix) skip=v ;;
    --git-dir=*|--work-tree=*) moved=1 ;;
    -*) ;;
    *)
      sub=$word
      break
      ;;
  esac
done
own_workers() {
  [ "$moved" = 0 ] || return 1
  branch=$("$git" symbolic-ref --short -q HEAD) || return 1
  [ -n "$branch" ] || return 1
  named=0
  value=0
  for word do
    if [ "$value" = 1 ]; then
      value=0
      continue
    fi
    case "$word" in
      -m|-F|-s|-X|--message|--file|--strategy|--strategy-option|--into-name|--cleanup) value=1 ;;
      -*) ;;
      *)
        word=${word#refs/heads/}
        case "$word" in
          "$branch"--*) ;;
          *) return 1 ;;
        esac
        name=${word#"$branch"--}
        case "$name" in
          ''|*/*|*--*|.*|*[!A-Za-z0-9._-]*) return 1 ;;
        esac
        named=1
        ;;
    esac
  done
  [ "$named" = 1 ]
}
gate() {
  shift "$1"
  shift
  if [ "$#" = 1 ]; then
    case "$1" in
      --abort|--quit|--continue|--skip) return 0 ;;
    esac
  fi
  [ "$lead" = 1 ] && [ "$sub" = merge ] && own_workers "$@"
}
case "$sub" in
  '') ;;
  merge|pull|rebase|cherry-pick)
    gate "$at" "$@" || refuse "$sub" "$(reason)"
    ;;
  add|am|apply|archive|bisect|blame|branch|bundle|cat-file|checkout|cherry|clean|clone|commit|config|describe|diff|diff-files|diff-index|diff-tree|difftool|fetch|for-each-ref|format-patch|fsck|gc|grep|hash-object|help|init|log|ls-files|ls-remote|ls-tree|merge-base|merge-file|merge-tree|mergetool|mv|name-rev|notes|push|range-diff|reflog|remote|reset|restore|rev-list|rev-parse|revert|rm|shortlog|show|show-ref|sparse-checkout|stash|status|submodule|switch|symbolic-ref|tag|update-index|update-ref|version|worktree) ;;
  *)
    expanded=$("$git" config --get "alias.$sub" 2>/dev/null)
    case "$expanded" in
      merge|'merge '*|pull|'pull '*|rebase|'rebase '*|cherry-pick|'cherry-pick '*) refuse "$sub" "Aliases for merge, pull, rebase or cherry-pick are refused as those commands are. $(reason)" ;;
    esac
    ;;
esac
exec "$git" "$@"
"##;

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
    developer: PathBuf,
}

impl ToolHomes {
    /// The owner's tool homes: `~/.rustup` and `~/.cargo` in the account's home folder.
    /// Takes nothing. Returns the homes, or the refusal when the owner account is unknown.
    pub fn owner() -> Result<Self, SandboxError> {
        Ok(Self::at(&owner_home()?))
    }

    /// The tool homes inside one home folder, with the Command Line Tools at their system
    /// folder. Input: the home. Output: the homes.
    pub fn at(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            rustup: home.join(".rustup"),
            cargo: home.join(".cargo"),
            developer: PathBuf::from(DEVELOPER_TOOLS),
        }
    }

    /// These tool homes with the Command Line Tools looked for in `developer` instead.
    /// Input: the folder. Output: the homes.
    pub fn with_developer(mut self, developer: &Path) -> Self {
        self.developer = developer.to_path_buf();
        self
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
/// agent's own home, the owner's rustup home read-only in `RUSTUP_HOME`,
/// [`AUTO_INSTALL_ENV`], and `SDKROOT` at the Command Line Tools' SDK when `developer_bin`
/// finds their `cc`, so rustc and the cc crate never ask xcrun. Inputs: the agent home and
/// the owner's tool homes. Output: the variables in order.
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
    env.extend(
        developer_bin(homes)
            .map(|_| homes.developer.join("SDKs").join("MacOSX.sdk"))
            .filter(|sdk| sdk.is_dir())
            .map(|sdk| ("SDKROOT", sdk.into_os_string())),
    );
    env
}

/// The Command Line Tools' program folder when it holds `cc`. Every lookup by an xcrun shim
/// under an agent's own home misses xcrun's cache, whose write in the per-user temp folder
/// the sandbox denies (R7-XCRUN), so an agent runs these programs directly. Takes the tool
/// homes. Returns the folder, none when the tools are absent.
fn developer_bin(homes: &ToolHomes) -> Option<String> {
    let bin = homes.developer.join("usr").join("bin");
    if !bin.join("cc").is_file() {
        return None;
    }
    bin.to_str()
        .filter(|text| !text.contains(':'))
        .map(str::to_owned)
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
/// where they hold `cargo`, then `base` with `developer_bin` just before its `/usr/bin`,
/// then graphify's own program folder when the default pack installed it (see
/// [`graphify`]), last so it never shadows a toolchain's `python3`. Inputs: the tool homes
/// and the base path. Output: the joined path.
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
    let mut developer = developer_bin(homes);
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
/// owns there and makes it private again, and writes the agent's own `git` into it (see
/// [`with_agent_git`]); a `git` that cannot be written is logged and the agent keeps the
/// system one. Input: the agent home. Output: the folder, or a refusal when the path is a
/// link, a file or another user's folder.
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
    if let Some(script) = git_script(crate::research::runner::sandboxed_git(), false)
        && let Err(error) = write_git(&path, &script)
    {
        eprintln!("agent_git_not_written kind={:?}", error.kind());
    }
    Ok(path)
}

/// The script an agent's own `git` holds: it runs the real git the engine itself resolved,
/// so the agent's git calls never reach the `/usr/bin/git` xcrun shim, whose `xcrun_db`
/// cache write Seatbelt denies, and git still finds its helpers beside its real path, which
/// a link to it would lose; it refuses merges as [`GIT_SCRIPT`] says. Inputs: the real git
/// and whether the agent leads workers. Output: the script, or none when git is the shim or
/// its path is not plain.
#[cfg(unix)]
fn git_script(git: &Path, lead: bool) -> Option<String> {
    let text = plain_path(git).filter(|_| git != Path::new(GIT_SHIM))?;
    let script = GIT_SCRIPT
        .replace("@GIT@", text)
        .replace("@LEAD@", if lead { "1" } else { "0" });
    assert!(!script.contains("@GIT@") && !script.contains("@LEAD@"));
    assert!(script.len() as u64 <= GIT_SCRIPT_BYTES_MAX);
    Some(script)
}

/// A path as text a script may hold in single quotes as is: absolute and of letters,
/// digits, spaces and `/._-+` only. Input: the path. Output: the text, or none.
#[cfg(unix)]
fn plain_path(path: &Path) -> Option<&str> {
    let text = path.to_str()?;
    let plain = path.is_absolute()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-+ ".contains(&byte));
    plain.then_some(text)
}

/// The script of an agent's own `getconf` ([`GETCONF_SCRIPT`]). Inputs: a native lead's
/// teammates' home, their temp folder and the agent's own. Output: the script, or none when
/// a path is not plain or the script is too long.
#[cfg(target_os = "macos")]
fn getconf_script(mates: &Path, shared: &Path, own: &Path) -> Option<String> {
    let script = GETCONF_SCRIPT
        .replace("@MATES@", &format!("'{}'", plain_path(mates)?))
        .replace("@SHARED@", &format!("'{}/'", plain_path(shared)?))
        .replace("@OWN@", &format!("'{}/'", plain_path(own)?));
    assert!(
        ["@MATES@", "@SHARED@", "@OWN@"]
            .iter()
            .all(|mark| !script.contains(mark))
    );
    (script.len() <= GETCONF_SCRIPT_BYTES_MAX).then_some(script)
}

/// Writes the agent's own `getconf` into `bin` of the launch folder the engine has just
/// made for one launch, which neither the agent nor a native lead's teammates may write
/// (LL-2, TPH). context-mode makes its script folders under `getconf DARWIN_USER_TEMP_DIR`,
/// ignoring TMPDIR, and Seatbelt denies the system answer, so this one names where the
/// caller may write: its short temp folder, or the lead's own one in it for a native lead
/// (T3-15/18). A research job makes its short temp folder after the install, so it calls this
/// again then (R7-CTX F1): a launch folder that already holds this same `getconf` is kept,
/// and one that holds anything else is refused. Inputs: the agent home and whether it leads a
/// native team. Output: done, also when the agent has no short temp folder or a path is not
/// plain and none is written, or the write failure.
#[cfg(target_os = "macos")]
pub(crate) fn write_getconf(home: &Path, native_lead: bool) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let Some(shared) = existing_tmp(home) else {
        return Ok(());
    };
    let own = if native_lead {
        native_lead_tmp(&shared)
    } else {
        shared.clone()
    };
    let mates = crate::launch_context::mates_home(home);
    let Some(script) = getconf_script(&mates, &shared, &own) else {
        return Ok(());
    };
    let folder = getconf_folder(home);
    let path = folder.join(GETCONF_PROGRAM);
    if let Err(error) = fs::DirBuilder::new().mode(0o700).create(&folder) {
        if error.kind() == io::ErrorKind::AlreadyExists && holds_getconf(&folder, &path, &script) {
            return Ok(());
        }
        return Err(error);
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open(&path)?;
    file.write_all(script.as_bytes())
        .and_then(|()| file.sync_all())?;
    let metadata = file.metadata()?;
    assert_eq!(metadata.mode() & 0o777, 0o700);
    assert_eq!(metadata.len(), script.len() as u64);
    Ok(())
}

/// Whether the launch folder already holds this exact `getconf`: `bin` a real folder and
/// `getconf` in it a real file of mode 0700 with the script's bytes, opened without following
/// a link. Inputs: the folder, the file and the script. Output: the answer.
#[cfg(target_os = "macos")]
fn holds_getconf(folder: &Path, path: &Path, script: &str) -> bool {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    if !fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir()) {
        return false;
    }
    let Ok(file) = fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open(path)
    else {
        return false;
    };
    let plain = file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o777 == 0o700);
    let mut held = Vec::with_capacity(script.len());
    plain
        && file
            .take(GETCONF_SCRIPT_BYTES_MAX as u64 + 1)
            .read_to_end(&mut held)
            .is_ok()
        && held == script.as_bytes()
}

/// The folder of the agent's launch folder that holds its own `getconf`. Input: the agent
/// home. Output: the path, which may not exist.
fn getconf_folder(home: &Path) -> PathBuf {
    home.join(crate::launch_context::STATE).join(GETCONF_FOLDER)
}

/// Rewrites the agent's own `git` in its short temp folder as a lead's copy, which lets the
/// lead merge its own workers' branches (DEC-107 rule 2), after [`make_tmp`] wrote the
/// copy every agent gets. Input: the folder `make_tmp` returned. Output: Ok, also when git
/// is the shim and no copy is written, or the write failure.
#[cfg(unix)]
pub fn lead_git(tmp: &Path) -> io::Result<()> {
    match git_script(crate::research::runner::sandboxed_git(), true) {
        Some(script) => write_git(tmp, &script),
        None => Ok(()),
    }
}

/// Writes the agent's own `git` into `bin` of its short temp folder as a 0700 file of this
/// user, replacing a copy that differs, through descriptors that never follow a link the
/// agent left there. Inputs: the temp folder and the script. Output: done, or the refusal.
#[cfg(unix)]
fn write_git(tmp: &Path, script: &str) -> io::Result<()> {
    use rustix::fs::{AtFlags, CWD, Mode, OFlags};
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicU64, Ordering};
    static STAGED: AtomicU64 = AtomicU64::new(0);
    let folder_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let tmp = rustix::fs::openat(CWD, tmp, folder_flags, Mode::empty())?;
    match rustix::fs::mkdirat(&tmp, GIT_FOLDER, Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let folder = rustix::fs::openat(&tmp, GIT_FOLDER, folder_flags, Mode::empty())?;
    if rustix::fs::fstat(&folder)?.st_uid != nix::unistd::getuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the agent git folder is not this user's",
        ));
    }
    rustix::fs::fchmod(&folder, Mode::from_raw_mode(0o700))?;
    let current = rustix::fs::openat(
        &folder,
        GIT_PROGRAM,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()
    .map(fs::File::from)
    .and_then(|file| {
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.mode() & 0o7777 != 0o700 {
            return None;
        }
        let mut text = String::new();
        file.take(GIT_SCRIPT_BYTES_MAX + 1)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    });
    if current.as_deref() == Some(script) {
        return Ok(());
    }
    let staged = format!(
        ".{GIT_PROGRAM}-{}-{}",
        std::process::id(),
        STAGED.fetch_add(1, Ordering::Relaxed)
    );
    let file = rustix::fs::openat(
        &folder,
        staged.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o700),
    )?;
    let mut file = fs::File::from(file);
    let placed = file
        .write_all(script.as_bytes())
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(0o700)))
        .and_then(|()| {
            rustix::fs::renameat(&folder, staged.as_str(), &folder, GIT_PROGRAM)
                .map_err(io::Error::from)
        });
    if placed.is_err() {
        let _ = rustix::fs::unlinkat(&folder, staged.as_str(), AtFlags::empty());
    }
    placed
}

/// Puts the agent's own `getconf` (see [`write_getconf`]) and then its own `git` (see
/// [`make_tmp`]) at the front of a harness's search path, ahead of every toolchain and
/// system folder, so the agent's git calls under Seatbelt run the real git, not the
/// `/usr/bin/git` shim, and context-mode makes its temp folders where the agent may write.
/// Only a harness's own path gets them, never a command the engine runs under the profile,
/// since the agent can change its `git` folder. Inputs: the agent home and the path.
/// Output: the path, without each program the agent does not have.
pub fn with_agent_git(home: &Path, path: String) -> String {
    let path = match existing_tmp(home) {
        Some(tmp) => in_front(&tmp.join(GIT_FOLDER), GIT_PROGRAM, path),
        None => path,
    };
    in_front(&getconf_folder(home), GETCONF_PROGRAM, path)
}

/// Puts a folder at the front of a search path when it holds the program as a file and its
/// name has no `:`. Inputs: the folder, the program and the path. Output: the path.
fn in_front(folder: &Path, program: &str, path: String) -> String {
    let present =
        fs::symlink_metadata(folder.join(program)).is_ok_and(|metadata| metadata.is_file());
    match folder.to_str() {
        Some(text) if present && !text.contains(':') => format!("{text}:{path}"),
        _ => path,
    }
}

/// The agent's short temp folder when it exists as this user's private folder. Input: the
/// agent home. Output: the folder, or None.
#[cfg(unix)]
pub fn existing_tmp(home: &Path) -> Option<PathBuf> {
    let path = tmp_path(home);
    private_folder(&path).then_some(path)
}

/// A native lead's own temp folder, its `TMPDIR`: `lead` in its short temp folder, which its
/// teammates share for their tmux socket and temp files but never reach inside (TPH-2
/// residual). Input: the lead's short temp folder. Output: the path, which may not exist.
pub fn native_lead_tmp(scratch: &Path) -> PathBuf {
    scratch.join(NATIVE_LEAD_TMP)
}

/// Makes a native lead's own temp folder empty and private for one start: whatever holds
/// the name, a link, a file or a folder, is removed without following a link, then the
/// folder is made with mode 0700, so nothing a teammate left there under an earlier build
/// reaches the lead. Input: the lead's short temp folder. Output: the folder, or the error,
/// also when something takes the name in between or the temp folder is not private.
#[cfg(unix)]
pub fn fresh_native_lead_tmp(scratch: &Path) -> io::Result<PathBuf> {
    if !private_folder(scratch) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the agent temp path is not a private folder of this user",
        ));
    }
    let path = native_lead_tmp(scratch);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&path)?,
        Ok(_) => fs::remove_file(&path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    seal_new_folder(&path)?;
    Ok(path)
}

/// Checks the folder just made at `path` with mode 0700 through one descriptor opened
/// without following a link: it must be empty and still the private folder the name holds,
/// so a link or a folder a teammate swaps in meanwhile is refused, never followed, changed
/// or kept. Input: the new folder's path. Output: done, or the refusal.
#[cfg(unix)]
fn seal_new_folder(path: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, Dir, Mode, OFlags};
    use rustix::io::Errno;
    let changed = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the lead's own temp folder changed while it was made",
        )
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let folder = match rustix::fs::openat(CWD, path, flags, Mode::empty()) {
        Ok(folder) => fs::File::from(folder),
        Err(Errno::LOOP | Errno::NOTDIR) => return Err(changed()),
        Err(error) => return Err(error.into()),
    };
    for entry in Dir::read_from(&folder)? {
        if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
            return Err(changed());
        }
    }
    let metadata = folder.metadata()?;
    if private_folder_pin(path) != Some((metadata.dev(), metadata.ino())) {
        return Err(changed());
    }
    Ok(())
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

/// Removes everything in the agent's short temp folder but the socket named `kept`, which
/// stays there with the folder; with no such socket in this user's folder the folder goes
/// as [`remove_tmp`] removes it. A native lead's caller socket is kept this way as the lead
/// ends, so a teammate still running asks a socket no engine answers on and is refused,
/// never finding no socket (rv-secreview RH2R2-1). Input: the agent home and the kept name.
/// Output: as [`remove_tmp`].
#[cfg(unix)]
pub fn remove_tmp_keeping(home: &Path, kept: &str) -> io::Result<()> {
    let path = tmp_path(home);
    let keeps = own_folder(&path)
        && fs::symlink_metadata(path.join(kept))
            .is_ok_and(|metadata| std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()));
    if !keeps {
        return remove_tmp(home);
    }
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        if entry.file_name() == kept {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn own_folder(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && metadata.uid() == nix::unistd::getuid().as_raw())
}

#[cfg(unix)]
fn private_folder(path: &Path) -> bool {
    private_folder_pin(path).is_some()
}

/// The (device, inode) of a folder that is this user's own private folder, read in one
/// look that never follows a link. Input: the path. Output: the pin, or None for a link, a
/// file, another user's folder or one open to others.
#[cfg(unix)]
pub fn private_folder_pin(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.is_dir()
        && metadata.uid() == nix::unistd::getuid().as_raw()
        && metadata.mode() & 0o077 == 0)
        .then(|| (metadata.dev(), metadata.ino()))
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

/// No native lead's own temp folder outside Unix, where no native team runs. Takes the
/// short temp folder, unused. Returns `Unsupported`.
#[cfg(not(unix))]
pub fn fresh_native_lead_tmp(_scratch: &Path) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}

/// No agent `git` outside Unix. Takes the agent temp folder, unused. Returns `Unsupported`.
#[cfg(not(unix))]
pub fn lead_git(_tmp: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Nothing to remove outside Unix. Takes the agent home, unused. Returns done.
#[cfg(not(unix))]
pub fn remove_tmp(_home: &Path) -> io::Result<()> {
    Ok(())
}

/// Nothing to remove outside Unix. Takes the agent home and the kept name, unused. Returns
/// done.
#[cfg(not(unix))]
pub fn remove_tmp_keeping(_home: &Path, _kept: &str) -> io::Result<()> {
    Ok(())
}

/// No agent cargo home outside Unix, where no agent sandbox runs. Takes the agent home and
/// tool homes, unused. Returns `Unsupported`.
#[cfg(not(unix))]
pub fn prepare_cargo_home(_home: &Path, _homes: &ToolHomes) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, unix))]
mod tests;
