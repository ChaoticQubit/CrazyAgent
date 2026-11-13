use super::process::{file_sha256, is_sha256, private_folder, run, staging_folder, version_from};
use super::{
    Dependency, GRAPHIFY_PYTHON, InstallError, Installer, LOCK_FILE, Pin, ToolLock, Version,
    keep_only,
};
use crate::tools::permission_fix;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const CURL: &str = "/usr/bin/curl";
const TAR: &str = "/usr/bin/tar";
const ARCHIVE_BYTES_MAX: u64 = 128 * 1024 * 1024;
const VERSION_TIME: Duration = Duration::from_secs(10);
const DOWNLOAD_TIME: Duration = Duration::from_secs(300);
const UV_TOOL_TIME: Duration = Duration::from_secs(900);
const UV_RECEIPT: &str = "uv-receipt.toml";
const NAME_BYTES_MAX: usize = 64;

/// A dependency setup found: its entry, its version and why the bundled tmux is not the one
/// used, if it is not.
type Chosen = (PathBuf, Version, Option<crate::tools::BundleNote>);

/// The folders searched after `setup/bin` on a Unix system: the owner's `~/.local/bin`,
/// the engine's own absolute `PATH` folders, then the platform's fixed folders.
/// Input: the fixed folders. Output: the folders in search order.
pub fn system_folders(fixed: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut system: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        system.push(PathBuf::from(home).join(".local/bin"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        system.extend(std::env::split_paths(&path).filter(|dir| dir.is_absolute()));
    }
    system.extend(fixed);
    system
}

/// Finds and installs uv and Graphify the same way on every Unix system: uv from its release
/// archive, kept only when its SHA-256 matches the pin, Graphify and other Python tools from
/// their bundled hash locks, all into `<engine root>/setup` (`setup/bin`, tools under
/// `setup/uv`). Lookups go through the tool resolver's places, Node as a usable node and npm
/// pair ([`super::node::usable`]). Nothing runs with sudo.
pub struct PinnedInstaller {
    setup: PathBuf,
    search: Vec<PathBuf>,
    places: crate::tools::Places,
}

impl PinnedInstaller {
    /// Uses the engine root's `setup` folder and searches it, then the given absolute
    /// system folders. Inputs: the engine root and the folders searched after `setup/bin`.
    /// Output: the installer.
    pub fn with_paths(engine_root: &Path, system: Vec<PathBuf>) -> Self {
        let setup = engine_root.join("setup");
        let mut search = vec![setup.join("bin")];
        for dir in system {
            if dir.is_absolute() && !search.contains(&dir) {
                search.push(dir);
            }
        }
        assert!(!search.is_empty());
        let places =
            crate::tools::Places::new(search.clone(), std::env::var_os("HOME").map(PathBuf::from));
        Self {
            setup,
            search,
            places,
        }
    }

    /// Same as [`PinnedInstaller::with_paths`], finding programs wherever the tool resolver
    /// looks in `places` ([`crate::tools::resolve`]), so setup finds a tool exactly where a
    /// launch that needs it does. Inputs: the engine root, the folders the programs setup
    /// starts search after `setup/bin`, and the places. Output: the installer.
    pub fn with_places(
        engine_root: &Path,
        system: Vec<PathBuf>,
        places: crate::tools::Places,
    ) -> Self {
        Self {
            places,
            ..Self::with_paths(engine_root, system)
        }
    }

    /// Finds a usable Node ([`super::node::usable`]) on the installer's places and asks it
    /// for its version. Output: the version, or none when no usable Node reports one.
    fn node(&self) -> Option<Version> {
        let found = super::node::usable(&self.places)?;
        self.version(Dependency::Node, &[found.node.into_os_string()])
            .ok()
    }

    /// Returns the private install folder, `<engine root>/setup`.
    pub fn setup_folder(&self) -> &Path {
        &self.setup
    }

    /// Finds a program where the tool resolver looks and its version. Input: the
    /// dependency. Output: its entry and version, or none when it is missing or reports none.
    pub fn locate(&self, dep: Dependency) -> Option<(PathBuf, Version)> {
        self.resolved(dep)
            .ok()?
            .ok()
            .map(|(entry, version, _)| (entry, version))
    }

    /// Finds a dependency wherever the tool resolver looks ([`crate::tools::resolve`]), the
    /// file a launch that needs it runs, and reads its version through the same words; tmux
    /// is the one the terminal uses ([`crate::tools::resolve_tmux`]), with why the bundled
    /// copy is not, if it is not. Input: the dependency. Output: its entry, version and note,
    /// why it cannot run, or the version check's code.
    fn resolved(
        &self,
        dep: Dependency,
    ) -> Result<Result<Chosen, crate::tools::Missing>, InstallError> {
        let found = match dep {
            Dependency::Tmux => crate::tools::resolve_tmux(
                &self.places,
                crate::tools::SHIPS_BUNDLED_TMUX,
                crate::terminal::tmux::runs,
            ),
            _ => crate::tools::resolve(dep.program(), &self.places).map(|found| (found, None)),
        };
        let (found, note) = match found {
            Ok(found) => found,
            Err(missing) => return Ok(Err(missing)),
        };
        let version = self.version(dep, &found.argv())?;
        assert!(found.entry.is_absolute());
        Ok(Ok((found.entry, version, note)))
    }

    /// Returns the `PATH` value that puts `setup/bin` first, for programs the engine runs.
    pub fn search_path(&self) -> OsString {
        std::env::join_paths(&self.search).unwrap_or_default()
    }

    /// The only environment the programs setup starts get: `PATH` with `setup/bin` first,
    /// and the engine's `HOME`, `TMPDIR` and `LANG` when set. Output: the pairs.
    pub(super) fn env(&self) -> Vec<(&'static str, OsString)> {
        let mut env = vec![("PATH", self.search_path())];
        for name in ["HOME", "TMPDIR", "LANG"] {
            if let Some(value) = std::env::var_os(name) {
                env.push((name, value));
            }
        }
        env
    }

    fn version(&self, dep: Dependency, words: &[OsString]) -> Result<Version, InstallError> {
        let flag = match dep {
            Dependency::Tmux => "-V",
            Dependency::Uv | Dependency::Graphify | Dependency::Node => "--version",
        };
        let (program, rest) = words
            .split_first()
            .ok_or(InstallError("setup_version_unreadable"))?;
        let mut arguments = rest.to_vec();
        arguments.push(OsString::from(flag));
        let (ok, output) = run(
            Path::new(program),
            &arguments,
            &self.env(),
            &std::env::temp_dir(),
            VERSION_TIME,
        )?;
        if !ok {
            return Err(InstallError("setup_version_unreadable"));
        }
        if dep == Dependency::Node {
            return node_version(&output);
        }
        version_from(&output)
    }

    fn release(
        &self,
        dep: Dependency,
        version: &Version,
        url: &str,
        sha256: &str,
        member: &str,
    ) -> Result<(), InstallError> {
        let member_path = Path::new(member);
        let valid = (url.starts_with("https://") || url.starts_with("file:///"))
            && url.len() <= 2048
            && !url.chars().any(|c| c.is_control() || c.is_whitespace())
            && is_sha256(sha256)
            && !member.starts_with('-')
            && member_path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && member_path.file_name() == Some(std::ffi::OsStr::new(dep.program()));
        if !valid {
            return Err(InstallError("setup_pin_invalid"));
        }
        let bin = self.setup.join("bin");
        private_folder(&bin)?;
        let staging = staging_folder(&self.setup, "download")?;
        let result = (|| {
            let archive = staging.join("archive.tar.gz");
            self.download(url, sha256, &archive, &staging)?;
            let out = staging.join("out");
            private_folder(&out)?;
            let (unpacked, _) = run(
                Path::new(TAR),
                &[
                    "-xzf".into(),
                    archive.clone().into_os_string(),
                    "-C".into(),
                    out.clone().into_os_string(),
                    member.into(),
                ],
                &self.env(),
                &staging,
                DOWNLOAD_TIME,
            )?;
            let program = out.join(member_path);
            let regular = std::fs::symlink_metadata(&program).is_ok_and(|m| m.is_file());
            if !unpacked || !regular {
                return Err(InstallError("setup_install_failed"));
            }
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| InstallError("setup_store_unavailable"))?;
            if self.version(dep, &[program.clone().into_os_string()])? != *version {
                return Err(InstallError("setup_version_mismatch"));
            }
            std::fs::rename(&program, bin.join(dep.program()))
                .map_err(|_| InstallError("setup_store_unavailable"))
        })();
        let _ = std::fs::remove_dir_all(&staging);
        result
    }

    /// Downloads one pinned archive with curl over https (or a local `file:///` source),
    /// bounded in size and time, and keeps it only when its SHA-256 is the pinned one.
    /// Inputs: the URL, the SHA-256, where to save it and the staging folder curl runs in.
    /// Output: Ok, `setup_download_failed` or `setup_checksum_mismatch`.
    pub(super) fn download(
        &self,
        url: &str,
        sha256: &str,
        archive: &Path,
        staging: &Path,
    ) -> Result<(), InstallError> {
        assert!(is_sha256(sha256));
        let (fetched, _) = run(
            Path::new(CURL),
            &[
                "-q".into(),
                "-fsSL".into(),
                "--proto".into(),
                "=https,file".into(),
                "--max-filesize".into(),
                ARCHIVE_BYTES_MAX.to_string().into(),
                "--max-time".into(),
                DOWNLOAD_TIME.as_secs().to_string().into(),
                "-o".into(),
                archive.as_os_str().to_owned(),
                url.into(),
            ],
            &self.env(),
            staging,
            DOWNLOAD_TIME + Duration::from_secs(5),
        )?;
        let size = std::fs::symlink_metadata(archive)
            .map(|metadata| metadata.is_file().then_some(metadata.len()))
            .ok()
            .flatten();
        if !fetched || size.is_none_or(|size| size > ARCHIVE_BYTES_MAX) {
            return Err(InstallError("setup_download_failed"));
        }
        if file_sha256(archive)? != sha256 {
            return Err(InstallError("setup_checksum_mismatch"));
        }
        Ok(())
    }

    fn uv_tool(&self, package: &str, version: &Version, python: &str) -> Result<(), InstallError> {
        if !plain_name(package) {
            return Err(InstallError("setup_pin_invalid"));
        }
        let lock = super::tool_lock(package, version.as_str())
            .ok_or(InstallError("setup_pin_unlocked"))?;
        self.install_locked(&lock, python)
    }

    /// Installs one Python tool exactly as its lock pins it: a fresh virtual environment
    /// under `setup/uv/tools/<package>`, into which uv installs only wheels whose SHA-256
    /// the lock lists, then, with group and other write removed from setup's Pythons and the
    /// environment (uv writes under the umask, DEC-162 R2), links to its programs in
    /// `setup/bin`. A tool already linked from the same lock is kept. A failed install
    /// removes its environment and leaves the old links. Inputs: the lock and the Python
    /// version. Output: `setup_install_failed` when uv refuses a file or a program is
    /// missing, or another setup code.
    pub(super) fn install_locked(
        &self,
        lock: &ToolLock<'_>,
        python: &str,
    ) -> Result<(), InstallError> {
        let valid = plain_name(lock.package)
            && !lock.programs.is_empty()
            && lock.programs.iter().all(|program| plain_name(program))
            && !lock.requirements.is_empty()
            && !python.is_empty()
            && python.len() <= 8
            && python.bytes().all(|b| b.is_ascii_digit() || b == b'.');
        if !valid {
            return Err(InstallError("setup_pin_invalid"));
        }
        let bin = self.setup.join("bin");
        let tools = self.setup.join("uv/tools");
        let home = tools.join(lock.package);
        if self.linked_from(&home, lock) {
            return Ok(());
        }
        let (uv, _) = self
            .locate(Dependency::Uv)
            .ok_or(InstallError("setup_uv_missing"))?;
        let cache = self.setup.join("uv/cache");
        let pythons = self.setup.join("uv/python");
        let python_bin = self.setup.join("uv/python-bin");
        for folder in [&bin, &tools] {
            private_folder(folder)?;
        }
        narrow_uv_tool_folder(&home)?;
        for folder in [&home, &cache, &pythons, &python_bin] {
            private_folder(folder)?;
        }
        let mut env = self.env();
        env.extend([
            ("UV_CACHE_DIR", cache.into_os_string()),
            ("UV_PYTHON_INSTALL_DIR", pythons.clone().into_os_string()),
            ("UV_PYTHON_BIN_DIR", python_bin.into_os_string()),
            ("UV_NO_CONFIG", "1".into()),
            ("UV_NO_PROGRESS", "1".into()),
        ]);
        let staged = staging_folder(&home, "env")?;
        let result = (|| {
            let requirements = staged.join(LOCK_FILE);
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&requirements)
                .and_then(|mut file| file.write_all(lock.requirements.as_bytes()))
                .map_err(|_| InstallError("setup_store_unavailable"))?;
            let venv = staged.join("venv");
            let commands =
                super::locked_tool_args(python, &venv, &venv.join("bin/python"), &requirements);
            for args in &commands {
                let (ok, _) = run(&uv, args, &env, &self.setup, UV_TOOL_TIME)?;
                if !ok {
                    return Err(InstallError("setup_install_failed"));
                }
            }
            let programs: Vec<PathBuf> = lock
                .programs
                .iter()
                .map(|program| venv.join("bin").join(program))
                .collect();
            let present = programs.iter().all(|program| {
                runnable(program) && std::fs::symlink_metadata(program).is_ok_and(|m| m.is_file())
            });
            if !present {
                return Err(InstallError("setup_install_failed"));
            }
            for tree in [&pythons, &staged] {
                crate::tools::permission_fix::tighten_setup(&self.setup, tree)
                    .map_err(|_| InstallError("setup_install_failed"))?;
            }
            for (program, name) in programs.iter().zip(lock.programs) {
                link(program, &bin.join(name))?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => keep_only(&home, &staged),
            Err(_) => {
                let _ = std::fs::remove_dir_all(&staged);
            }
        }
        result
    }

    fn linked_from(&self, home: &Path, lock: &ToolLock<'_>) -> bool {
        lock.programs.iter().all(|name| {
            let Ok(target) = std::fs::read_link(self.setup.join("bin").join(name)) else {
                return false;
            };
            let env = target.ancestors().nth(3);
            target.ends_with(Path::new("venv/bin").join(name))
                && env.and_then(Path::parent) == Some(home)
                && runnable(&target)
                && env.is_some_and(|env| {
                    std::fs::read(env.join(LOCK_FILE))
                        .is_ok_and(|text| text == lock.requirements.as_bytes())
                })
        })
    }
}

/// Narrows a tool folder the previous build's `uv tool install` left at the umask mode
/// (0755) to 0700, so the private check passes and `keep_only` clears it once the locked
/// install succeeds. Only a real folder of this user holding uv's receipt changes, and its
/// parent is checked private first. Input: the tool folder. Output: Ok, or
/// `setup_store_unavailable` when the mode cannot be set.
fn narrow_uv_tool_folder(home: &Path) -> Result<(), InstallError> {
    let Ok(metadata) = std::fs::symlink_metadata(home) else {
        return Ok(());
    };
    let receipt =
        std::fs::symlink_metadata(home.join(UV_RECEIPT)).is_ok_and(|entry| entry.is_file());
    if !receipt
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.permissions().mode() & 0o077 == 0
    {
        return Ok(());
    }
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| InstallError("setup_store_unavailable"))
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_' || b == b'.'
        })
}

/// Reads the version Node prints for `--version`: one word such as `v26.13.1` on the first
/// line, kept without its `v`. Input: the output. Output: the version, or
/// `setup_version_unreadable`.
fn node_version(output: &[u8]) -> Result<Version, InstallError> {
    let unreadable = InstallError("setup_version_unreadable");
    let text = std::str::from_utf8(output).map_err(|_| unreadable)?;
    let word = text.lines().next().unwrap_or_default().trim();
    Version::parse(word.strip_prefix('v').ok_or(unreadable)?)
}

/// Points `at` at `target` with a new link put in place by one rename, so a reader never
/// sees it missing. Inputs: the target and the link path. Output: Ok, or
/// `setup_store_unavailable`.
pub(super) fn link(target: &Path, at: &Path) -> Result<(), InstallError> {
    let unavailable = InstallError("setup_store_unavailable");
    let random = crate::engine::owner::random_id().map_err(|_| unavailable)?;
    let name = at.file_name().ok_or(unavailable)?.to_string_lossy();
    let staged = at.with_file_name(format!(".{name}-{}", &random[..24]));
    std::os::unix::fs::symlink(target, &staged).map_err(|_| unavailable)?;
    std::fs::rename(&staged, at).map_err(|_| {
        let _ = std::fs::remove_file(&staged);
        unavailable
    })
}

/// Tells whether a path is a regular file someone may run. Input: the path.
/// Output: true for a file with any execute bit, following links.
pub(super) fn runnable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
#[path = "pinned_tests.rs"]
mod tests;

impl Installer for PinnedInstaller {
    fn find(&self, dep: Dependency) -> Result<Option<Version>, InstallError> {
        Ok(self.lookup(dep)?.ok().map(|located| located.version))
    }

    fn lookup(&self, dep: Dependency) -> super::Lookup {
        if dep == Dependency::Node {
            return Ok(self
                .node()
                .map(|version| super::Located {
                    version,
                    bundled: false,
                    note: None,
                })
                .ok_or_else(|| dep.not_found()));
        }
        let bundled = self.places.bundled(dep.program());
        Ok(self.resolved(dep)?.map(|(entry, version, note)| {
            let located = super::Located {
                bundled: bundled.as_ref() == Some(&entry),
                version,
                note,
            };
            assert!(!(located.bundled && located.note.is_some()));
            located
        }))
    }

    fn install_tool(&self, package: &str, version: &str) -> Result<(), InstallError> {
        private_folder(&self.setup)?;
        self.uv_tool(package, &Version::parse(version)?, GRAPHIFY_PYTHON)
    }

    fn program(&self, name: &str) -> Option<PathBuf> {
        self.lookup_program(name).ok()
    }

    fn lookup_program(&self, name: &str) -> Result<PathBuf, crate::tools::Missing> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return Err(super::program_not_found(name));
        }
        crate::tools::resolve(name, &self.places).map(|found| found.entry)
    }

    fn fix_target(&self, missing: &crate::tools::Missing) -> Option<permission_fix::Target> {
        if missing.reason != crate::tools::Reason::Refused {
            return None;
        }
        permission_fix::target(missing.refused.as_deref()?, &self.places)
    }

    fn fix_permissions(
        &self,
        dep: Dependency,
        confirmed: &str,
    ) -> Result<permission_fix::Report, &'static str> {
        let missing = match crate::tools::resolve(dep.program(), &self.places) {
            Ok(_) => return Err(permission_fix::TOOL_FIX_NOT_NEEDED),
            Err(missing) => missing,
        };
        let target = self
            .fix_target(&missing)
            .ok_or(permission_fix::TOOL_FIX_UNAVAILABLE)?;
        if target.path.to_string_lossy() != confirmed {
            return Err(permission_fix::TOOL_FIX_CHANGED);
        }
        permission_fix::fix(&target, &self.places)
    }

    fn install(&self, dep: Dependency, pin: &Pin) -> Result<Version, InstallError> {
        private_folder(&self.setup)?;
        let pinned = match pin {
            Pin::Release {
                version,
                url,
                sha256,
                member,
            } => {
                self.release(dep, version, url, sha256, member)?;
                version
            }
            Pin::UvTool {
                package,
                version,
                python,
            } => {
                self.uv_tool(package, version, python)?;
                version
            }
            Pin::Archive {
                version,
                url,
                sha256,
                top,
            } if dep == Dependency::Node => {
                let engine_root = self
                    .setup
                    .parent()
                    .ok_or(InstallError("setup_store_unavailable"))?;
                let pinned = super::node::NodePin {
                    url: url.clone(),
                    sha256: sha256.clone(),
                    top: top.clone(),
                };
                super::node::install_pin(engine_root, &pinned)?;
                version
            }
            Pin::Homebrew { .. } | Pin::Winget { .. } | Pin::Archive { .. } => {
                return Err(InstallError("setup_pin_invalid"));
            }
        };
        let (_, found) = self
            .locate(dep)
            .ok_or(InstallError("setup_install_failed"))?;
        if *pinned != found {
            return Err(InstallError("setup_version_mismatch"));
        }
        Ok(found)
    }
}
