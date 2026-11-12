use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The environment variable that overrides the engine's data root.
pub const HOME_ENV: &str = "CRAZYAGENT_HOME";
/// The default socket name inside the data root.
pub const SOCKET_NAME: &str = "engine.sock";
/// The RCLI host executable looked up next to `crazyagent`, then on PATH.
pub const RCLI_EXECUTABLE: &str = "crazyagent-rcli";

const GLOBAL_DEFAULTS: &str = "version = 1\ntimeout_ms = 5000\n";
const PROJECT_DEFAULTS: &str = "version = 1\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathsError(pub String);

impl std::fmt::Display for PathsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PathsError {}

fn problem(path: &Path, what: impl std::fmt::Display) -> PathsError {
    PathsError(format!("{}: {what}", path.display()))
}

/// The three desktop platforms the data root and services are defined for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    pub const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::MacOs
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Linux
    };
}

/// The environment values that choose the data root, read once so the choice is testable
/// with real values on any host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootEnvironment {
    pub home_override: Option<OsString>,
    pub home: Option<OsString>,
    pub xdg_data_home: Option<OsString>,
    pub local_app_data: Option<OsString>,
}

impl RootEnvironment {
    /// Reads `CRAZYAGENT_HOME`, `HOME`, `XDG_DATA_HOME` and `LOCALAPPDATA` from this process.
    pub fn current() -> Self {
        Self {
            home_override: std::env::var_os(HOME_ENV),
            home: std::env::var_os("HOME"),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME"),
            local_app_data: std::env::var_os("LOCALAPPDATA"),
        }
    }
}

fn present(value: Option<&OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// Chooses the engine's data root. Inputs: the platform and its environment values.
/// Output: `CRAZYAGENT_HOME` when set (it must be absolute), else macOS
/// `~/Library/Application Support/CrazyAgent/engine`, Linux
/// `${XDG_DATA_HOME:-~/.local/share}/crazyagent/engine` or Windows
/// `%LOCALAPPDATA%\CrazyAgent\engine`; an error names a missing variable.
pub fn data_root(platform: Platform, env: &RootEnvironment) -> Result<PathBuf, PathsError> {
    if let Some(root) = present(env.home_override.as_ref()) {
        if !root.is_absolute() {
            return Err(PathsError(format!(
                "{HOME_ENV} must be an absolute folder path, not {}",
                root.display()
            )));
        }
        return Ok(root);
    }
    let home = || {
        present(env.home.as_ref())
            .filter(|home| home.is_absolute())
            .ok_or_else(|| PathsError("HOME is not set to an absolute folder path".into()))
    };
    let root = match platform {
        Platform::MacOs => home()?
            .join("Library")
            .join("Application Support")
            .join("CrazyAgent")
            .join("engine"),
        Platform::Linux => match present(env.xdg_data_home.as_ref()) {
            Some(data) if data.is_absolute() => data,
            _ => home()?.join(".local").join("share"),
        }
        .join("crazyagent")
        .join("engine"),
        Platform::Windows => present(env.local_app_data.as_ref())
            .ok_or_else(|| PathsError("LOCALAPPDATA is not set".into()))?
            .join("CrazyAgent")
            .join("engine"),
    };
    assert!(root.ends_with("engine"));
    Ok(root)
}

/// Where one engine keeps its database, socket, settings files and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// Names a layout at an absolute data root. Output: the layout, or an error for a
    /// relative root.
    pub fn new(root: PathBuf) -> Result<Self, PathsError> {
        if !root.is_absolute() {
            return Err(problem(&root, "the data folder must be an absolute path"));
        }
        Ok(Self { root })
    }

    /// The layout this process uses by default: `CRAZYAGENT_HOME` or the platform root.
    pub fn current() -> Result<Self, PathsError> {
        Self::new(data_root(Platform::CURRENT, &RootEnvironment::current())?)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn socket(&self) -> PathBuf {
        self.root.join(SOCKET_NAME)
    }

    pub fn global_settings(&self) -> PathBuf {
        self.root.join("global.toml")
    }

    pub fn project_settings(&self) -> PathBuf {
        self.root.join("project.toml")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn engine_log(&self) -> PathBuf {
        self.logs().join("engine.log")
    }

    /// Creates the data root and its logs folder private to this user (0700) and the
    /// default `global.toml` and `project.toml` (0600) when they are missing; existing
    /// files are kept. Output: an error naming the path for an unsafe or unusable entry.
    pub fn prepare(&self) -> Result<(), PathsError> {
        private_folder(&self.root)?;
        private_folder(&self.logs())?;
        private_settings(&self.global_settings(), GLOBAL_DEFAULTS)?;
        private_settings(&self.project_settings(), PROJECT_DEFAULTS)?;
        Ok(())
    }
}

/// Creates a folder and any missing parents private to this user (0700), then refuses
/// it unless it is a real folder this user owns with exactly mode 0700. Input: the
/// folder. Output: Ok, or an error naming the path and the fix.
pub(crate) fn private_folder(path: &Path) -> Result<(), PathsError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(path)
        .map_err(|error| problem(path, format!("could not create the folder ({error})")))?;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| problem(path, error))?;
    if !metadata.is_dir() {
        return Err(problem(path, "is not a folder"));
    }
    private(path, &metadata, 0o700, "700")
}

fn private_settings(path: &Path, defaults: &str) -> Result<(), PathsError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(path) {
        Ok(mut file) => std::io::Write::write_all(&mut file, defaults.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| problem(path, format!("could not write settings ({error})")))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(problem(
                path,
                format!("could not create settings ({error})"),
            ));
        }
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|error| problem(path, error))?;
    if !metadata.is_file() {
        return Err(problem(path, "is not a regular file"));
    }
    private(path, &metadata, 0o600, "600")
}

#[cfg(unix)]
fn private(
    path: &Path,
    metadata: &std::fs::Metadata,
    mode: u32,
    shown: &str,
) -> Result<(), PathsError> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(problem(path, "is owned by another user"));
    }
    if metadata.mode() & 0o7777 != mode {
        return Err(problem(
            path,
            format!(
                "must be private to you; run: chmod {shown} '{}'",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Accepts every entry where Unix owner and mode bits do not exist. Inputs unused.
/// Output: Ok; the engine itself still refuses roots it cannot check on Windows.
#[cfg(not(unix))]
fn private(
    _path: &Path,
    _metadata: &std::fs::Metadata,
    _mode: u32,
    _shown: &str,
) -> Result<(), PathsError> {
    Ok(())
}

fn executable_names(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{name}.exe"), name.to_owned()]
    } else {
        vec![name.to_owned()]
    }
}

fn executable_file(path: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    let metadata = std::fs::metadata(&real).ok()?;
    #[cfg(unix)]
    let runnable = std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o111 != 0;
    #[cfg(not(unix))]
    let runnable = true;
    (metadata.is_file() && runnable).then_some(real)
}

/// Finds the PATH entry a shell would run for a name, skipping relative PATH folders.
/// Inputs: the name and a PATH value. Output: the absolute entry with its links kept,
/// or none when no entry leads to a runnable file.
fn path_entry(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    for folder in std::env::split_paths(path?) {
        if !folder.is_absolute() {
            continue;
        }
        for file in executable_names(name) {
            let entry = folder.join(file);
            if executable_file(&entry).is_some() {
                assert!(entry.is_absolute());
                return Some(entry);
            }
        }
    }
    None
}

/// Finds an executable by name the way a shell would, skipping relative PATH entries.
/// Inputs: the name and a PATH value. Output: the first match with every link resolved,
/// or none when it is missing.
pub fn find_in_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    path_entry(name, path).and_then(|entry| executable_file(&entry))
}

/// The harness executables one engine start registers; none means not installed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Harnesses {
    pub codex: Option<PathBuf>,
    pub claude: Option<PathBuf>,
    pub rcli: Option<PathBuf>,
}

/// Resolves the harnesses for one engine start, never reusing an earlier answer. Inputs:
/// the given paths (flags, else the owner's chosen files), the engine search, the PATH value
/// and the folder holding the running executable. Output: each given path, else the `codex`
/// and `claude` entries the search finds as the catalog does (PATH, system, then HOME
/// folders), and the RCLI host next to the executable, then on PATH, links kept.
pub fn resolve_harnesses(
    given: Harnesses,
    search: &crate::harnesses::Search,
    path: Option<&OsStr>,
    executable_folder: Option<&Path>,
) -> Harnesses {
    Harnesses {
        codex: given.codex.or_else(|| harness_entry("codex", search, path)),
        claude: given
            .claude
            .or_else(|| harness_entry("claude-code", search, path)),
        rcli: rcli_entry(given.rcli, path, executable_folder),
    }
}

/// Resolves the RCLI host for one engine start: the given path, else the host next to the
/// running executable, then on PATH, links kept. Inputs: the given path, the PATH value and
/// the folder holding the running executable. Output: the entry, or none.
pub fn rcli_entry(
    given: Option<PathBuf>,
    path: Option<&OsStr>,
    executable_folder: Option<&Path>,
) -> Option<PathBuf> {
    let beside = || {
        let folder = executable_folder?;
        executable_names(RCLI_EXECUTABLE)
            .into_iter()
            .map(|file| folder.join(file))
            .find(|entry| executable_file(entry).is_some())
    };
    given
        .or_else(beside)
        .or_else(|| path_entry(RCLI_EXECUTABLE, path))
}

/// How the team runtime, usage upkeep, research and onboarding find a harness the engine did
/// not have when it started: the engine answers with the entry of a harness it has pinned
/// since, so nothing is launched that did not pass the checks of a registration. Only engine
/// start hands one out; a runtime built without it never looks past what it was given.
/// Input: the launch harness id. Output: the entry, or none.
pub type HarnessLookup = std::sync::Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

/// Finds the entry of a harness CrazyAgent starts itself, the one lookup engine start, the
/// catalog, Check again and the install pin share: the search's folders first, then PATH by
/// the platform's executable names. Inputs: the catalog id, the engine search and the PATH
/// value. Output: the entry with its links kept; none for an alias or unknown id, for a
/// harness without a launch adapter and for a harness found nowhere.
pub fn harness_entry(
    harness: &str,
    search: &crate::harnesses::Search,
    path: Option<&OsStr>,
) -> Option<PathBuf> {
    let spec = crate::harnesses::spec(harness)
        .filter(|spec| spec.id == harness && spec.launch.is_none())?;
    let entry = search.find(harness).or_else(|| {
        spec.binaries.iter().find_map(|name| {
            path_entry(name, path).filter(|entry| !crate::harnesses::is_shim(search.home(), entry))
        })
    })?;
    assert!(entry.is_absolute());
    Some(entry)
}

/// Every file a pin may check for a harness CrazyAgent starts itself, in the order
/// [`harness_entry`] looks: the search's candidates, shims included, then each PATH match by
/// the platform's executable names, each once. Inputs: the catalog id, the engine search and
/// the PATH value. Output: the absolute entries, bounded; empty for a harness with an adapter.
pub fn harness_candidates(
    harness: &str,
    search: &crate::harnesses::Search,
    path: Option<&OsStr>,
) -> Vec<PathBuf> {
    let Some(spec) =
        crate::harnesses::spec(harness).filter(|spec| spec.id == harness && spec.launch.is_none())
    else {
        return Vec::new();
    };
    let mut entries = search.candidates(harness);
    for entry in spec
        .binaries
        .iter()
        .filter_map(|name| path_entry(name, path))
    {
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    assert!(entries.len() <= crate::harnesses::CANDIDATES_MAX + spec.binaries.len());
    assert!(entries.iter().all(|entry| entry.is_absolute()));
    entries
}

/// Resolves a harness entry kept at engine start to the real file a launch runs and its
/// sandbox grants, as the entry stands now. Inputs: the absolute entry and the PATH value.
/// Output: the entry's real file; when an entry in a PATH folder is gone, the first PATH
/// match for its name; else NotFound, which a launch reports as not installed.
pub fn launch_executable(entry: &Path, path: Option<&OsStr>) -> std::io::Result<PathBuf> {
    assert!(entry.is_absolute(), "harness entry {}", entry.display());
    let on_path = || {
        let folder = entry.parent()?;
        let name = entry.file_name()?.to_str()?;
        std::env::split_paths(path?)
            .any(|listed| listed == folder)
            .then(|| find_in_path(name, path))
            .flatten()
    };
    let real = executable_file(entry).or_else(on_path).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "harness is not installed")
    })?;
    assert!(real.is_absolute());
    Ok(real)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn env(
        home_override: Option<&str>,
        home: Option<&str>,
        xdg: Option<&str>,
        local: Option<&str>,
    ) -> RootEnvironment {
        RootEnvironment {
            home_override: home_override.map(OsString::from),
            home: home.map(OsString::from),
            xdg_data_home: xdg.map(OsString::from),
            local_app_data: local.map(OsString::from),
        }
    }

    #[test]
    fn default_root_follows_each_platform_convention() {
        let home = Some("/Users/owner");
        assert_eq!(
            data_root(Platform::MacOs, &env(None, home, None, None)).unwrap(),
            Path::new("/Users/owner/Library/Application Support/CrazyAgent/engine")
        );
        assert_eq!(
            data_root(Platform::Linux, &env(None, Some("/home/owner"), None, None)).unwrap(),
            Path::new("/home/owner/.local/share/crazyagent/engine")
        );
        assert_eq!(
            data_root(
                Platform::Linux,
                &env(None, Some("/home/owner"), Some("/data/xdg"), None)
            )
            .unwrap(),
            Path::new("/data/xdg/crazyagent/engine")
        );
        assert_eq!(
            data_root(
                Platform::Linux,
                &env(None, Some("/home/owner"), Some("relative/xdg"), None)
            )
            .unwrap(),
            Path::new("/home/owner/.local/share/crazyagent/engine")
        );
        let local = r"C:\Users\owner\AppData\Local";
        let windows = data_root(Platform::Windows, &env(None, None, None, Some(local))).unwrap();
        assert!(windows.starts_with(local));
        assert!(windows.ends_with(Path::new("CrazyAgent").join("engine")));
    }

    #[test]
    fn crazyagent_home_overrides_every_platform_and_must_be_absolute() {
        for platform in [Platform::MacOs, Platform::Linux, Platform::Windows] {
            let chosen = env(Some("/srv/crazy"), Some("/Users/owner"), None, Some("/l"));
            assert_eq!(
                data_root(platform, &chosen).unwrap(),
                Path::new("/srv/crazy")
            );
        }
        let relative = env(Some("crazy"), Some("/Users/owner"), None, None);
        let error = data_root(Platform::MacOs, &relative).unwrap_err();
        assert!(
            error.0.contains("CRAZYAGENT_HOME must be an absolute"),
            "{error}"
        );
        let empty = env(Some(""), Some("/Users/owner"), None, None);
        assert_eq!(
            data_root(Platform::MacOs, &empty).unwrap(),
            Path::new("/Users/owner/Library/Application Support/CrazyAgent/engine")
        );
    }

    #[test]
    fn missing_home_or_local_app_data_is_a_named_error() {
        let none = RootEnvironment::default();
        assert!(
            data_root(Platform::MacOs, &none)
                .unwrap_err()
                .0
                .contains("HOME")
        );
        assert!(
            data_root(Platform::Linux, &none)
                .unwrap_err()
                .0
                .contains("HOME")
        );
        assert!(
            data_root(Platform::Windows, &none)
                .unwrap_err()
                .0
                .contains("LOCALAPPDATA")
        );
    }

    #[test]
    fn prepare_creates_private_root_logs_and_default_settings_once() {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let layout = Layout::new(temp.path().join("CrazyAgent").join("engine")).unwrap();
        layout.prepare().unwrap();
        for folder in [
            layout.root().parent().unwrap().to_path_buf(),
            layout.root().to_path_buf(),
            layout.logs(),
        ] {
            let metadata = std::fs::symlink_metadata(&folder).unwrap();
            assert!(metadata.is_dir());
            assert_eq!(metadata.mode() & 0o7777, 0o700, "{}", folder.display());
        }
        for (file, text) in [
            (layout.global_settings(), "version = 1\ntimeout_ms = 5000\n"),
            (layout.project_settings(), "version = 1\n"),
        ] {
            assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
            assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o7777, 0o600);
        }
        crate::config::resolve_config(&layout.global_settings(), &layout.project_settings())
            .unwrap();
        std::fs::write(layout.global_settings(), "version = 1\ntimeout_ms = 2000\n").unwrap();
        layout.prepare().unwrap();
        assert_eq!(
            std::fs::read_to_string(layout.global_settings()).unwrap(),
            "version = 1\ntimeout_ms = 2000\n"
        );
        assert_eq!(layout.socket(), layout.root().join("engine.sock"));
        assert_eq!(layout.engine_log(), layout.root().join("logs/engine.log"));
    }

    #[test]
    fn prepare_refuses_an_open_root_or_settings_file_with_the_fix() {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let layout = Layout::new(temp.path().join("engine")).unwrap();
        std::fs::create_dir(layout.root()).unwrap();
        std::fs::set_permissions(layout.root(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = layout.prepare().unwrap_err();
        assert!(error.0.contains("chmod 700"), "{error}");
        std::fs::set_permissions(layout.root(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(layout.global_settings(), "version = 1\n").unwrap();
        std::fs::set_permissions(
            layout.global_settings(),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let error = layout.prepare().unwrap_err();
        assert!(error.0.contains("chmod 600"), "{error}");
        let link = temp.path().join("linked");
        std::os::unix::fs::symlink(layout.root(), &link).unwrap();
        let error = Layout::new(link).unwrap().prepare().unwrap_err();
        assert!(error.0.contains("is not a folder"), "{error}");
    }

    /// A data root too long for a socket address prepares: the engine reaches its sockets
    /// through a short address, so a long account name needs no shorter root.
    #[test]
    fn a_data_root_too_long_for_a_socket_address_still_prepares() {
        let temp = tempfile::tempdir().unwrap();
        let deep = temp.path().join("d".repeat(100));
        Layout::new(deep.clone()).unwrap().prepare().unwrap();
        assert!(deep.join("logs").is_dir());
        assert!(deep.join("global.toml").is_file());
    }

    /// The default macOS data root's engine sockets have an address for an account name of
    /// any length, where a home of 30 bytes or more used to be refused.
    #[test]
    fn the_default_macos_root_has_socket_addresses_for_long_account_names() {
        for length in [29, 30, 64, 128] {
            let home = format!("/Users/{}", "u".repeat(length));
            let root = data_root(Platform::MacOs, &env(None, Some(&home), None, None)).unwrap();
            for socket in [
                root.join("o00000000").join("owner.sock"),
                root.join(SOCKET_NAME),
            ] {
                assert!(crate::socket_address::fits(&socket), "{}", socket.display());
            }
        }
    }

    /// A search over the PATH folders alone, with no system folders and no HOME, so a row
    /// never depends on what this machine has installed. Input: the PATH value. Output: it.
    fn on_path(path: &OsStr) -> crate::harnesses::Search {
        crate::harnesses::Search::new(std::env::split_paths(path).collect(), None, Vec::new())
    }

    /// A search that looks nowhere. Output: it.
    fn nowhere() -> crate::harnesses::Search {
        crate::harnesses::Search::new(Vec::new(), None, Vec::new())
    }

    fn program(path: &Path, mode: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_harness_updated_behind_its_path_link_is_launched_from_the_new_install() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let bin = root.join("bin");
        let old = root.join("cask/2.1.309/claude");
        let new = root.join("cask/2.1.322/claude");
        program(&old, 0o755);
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&old, bin.join("claude")).unwrap();
        let path = std::env::join_paths([bin.clone()]).unwrap();
        let entry = resolve_harnesses(Harnesses::default(), &on_path(&path), Some(&path), None)
            .claude
            .unwrap();
        assert_eq!(entry, bin.join("claude"));
        assert_eq!(launch_executable(&entry, Some(&path)).unwrap(), old);
        program(&new, 0o755);
        std::fs::remove_dir_all(old.parent().unwrap()).unwrap();
        std::fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink(&new, &entry).unwrap();
        assert_eq!(launch_executable(&entry, Some(&path)).unwrap(), new);
    }

    #[test]
    fn a_harness_installed_after_engine_start_is_found_on_the_engines_path_by_its_launch_id() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let bin = root.join("bin");
        let real = root.join("cask/2.1.322/claude");
        program(&real, 0o755);
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&real, bin.join("claude")).unwrap();
        program(&bin.join("aider"), 0o755);
        let path = std::env::join_paths([bin.clone()]).unwrap();
        assert_eq!(
            harness_entry("claude-code", &on_path(&path), Some(&path)),
            Some(bin.join("claude"))
        );
        assert_eq!(harness_entry("codex", &on_path(&path), Some(&path)), None);
        assert_eq!(harness_entry("aider", &on_path(&path), Some(&path)), None);
        assert_eq!(harness_entry("claude", &on_path(&path), Some(&path)), None);
        assert_eq!(harness_entry("claude-code", &nowhere(), None), None);
        let started = crate::harnesses::specs()
            .iter()
            .filter(|spec| spec.launch.is_none());
        let mut covered = 0;
        for spec in started {
            let entry = bin.join(spec.binaries[0]);
            if !entry.exists() {
                program(&entry, 0o755);
            }
            assert_eq!(
                harness_entry(spec.id, &on_path(&path), Some(&path)),
                Some(entry)
            );
            assert!(crate::team::launch_harness(spec.id).is_some());
            covered += 1;
        }
        assert!(covered >= 2);
    }

    #[test]
    fn a_harness_gone_from_its_path_entry_is_looked_up_on_path_again() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        program(&first.join("claude"), 0o755);
        program(&second.join("claude"), 0o755);
        let path = std::env::join_paths([first.clone(), second.clone()]).unwrap();
        let entry = resolve_harnesses(Harnesses::default(), &on_path(&path), Some(&path), None)
            .claude
            .unwrap();
        assert_eq!(entry, first.join("claude"));
        std::fs::remove_file(&entry).unwrap();
        assert_eq!(
            launch_executable(&entry, Some(&path)).unwrap(),
            second.join("claude")
        );
        std::fs::remove_file(second.join("claude")).unwrap();
        let gone = launch_executable(&entry, Some(&path)).unwrap_err();
        assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);
        program(&first.join("claude"), 0o755);
        let flagged = root.join("flags/claude");
        let gone = launch_executable(&flagged, Some(&path)).unwrap_err();
        assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn harness_lookup_takes_flags_then_path_and_keeps_links() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        let cellar = root.join("cellar/codex-1.2.3");
        program(&first.join("claude"), 0o644);
        program(&second.join("claude"), 0o755);
        program(&cellar, 0o755);
        std::fs::create_dir_all(&first).unwrap();
        std::os::unix::fs::symlink(&cellar, first.join("codex")).unwrap();
        let path = std::env::join_paths([PathBuf::from("relative"), first.clone(), second.clone()])
            .unwrap();
        let found = resolve_harnesses(Harnesses::default(), &on_path(&path), Some(&path), None);
        assert_eq!(found.codex, Some(first.join("codex")));
        assert_eq!(found.claude, Some(second.join("claude")));
        assert_eq!(found.rcli, None);
        let explicit = Harnesses {
            codex: Some(PathBuf::from("/explicit/codex")),
            claude: None,
            rcli: Some(PathBuf::from("/explicit/rcli")),
        };
        let found = resolve_harnesses(explicit, &on_path(&path), Some(&path), None);
        assert_eq!(found.codex, Some(PathBuf::from("/explicit/codex")));
        assert_eq!(found.claude, Some(second.join("claude")));
        assert_eq!(found.rcli, Some(PathBuf::from("/explicit/rcli")));
        assert_eq!(
            resolve_harnesses(Harnesses::default(), &nowhere(), None, None),
            Harnesses::default()
        );
    }

    #[test]
    fn rcli_host_is_found_beside_the_executable_before_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let app = root.join("app");
        let bin = root.join("bin");
        program(&bin.join(RCLI_EXECUTABLE), 0o755);
        let path = std::env::join_paths([bin.clone()]).unwrap();
        let found = resolve_harnesses(
            Harnesses::default(),
            &on_path(&path),
            Some(&path),
            Some(&app),
        );
        assert_eq!(found.rcli, Some(bin.join(RCLI_EXECUTABLE)));
        program(&app.join(RCLI_EXECUTABLE), 0o755);
        let found = resolve_harnesses(
            Harnesses::default(),
            &on_path(&path),
            Some(&path),
            Some(&app),
        );
        assert_eq!(found.rcli, Some(app.join(RCLI_EXECUTABLE)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_finder_path_still_finds_codex_in_a_system_folder_at_engine_start() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let homebrew = root.join("opt/homebrew/bin");
        program(&homebrew.join("codex"), 0o755);
        let finder = std::ffi::OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
        let search = crate::harnesses::Search::engine(Some(&finder), &[homebrew.clone()], None);
        let found = resolve_harnesses(Harnesses::default(), &search, Some(&finder), None);
        assert_eq!(found.codex, Some(homebrew.join("codex")));
        let path_only =
            resolve_harnesses(Harnesses::default(), &on_path(&finder), Some(&finder), None);
        assert_eq!(path_only.codex, None);
    }

    #[test]
    fn an_nvm_codex_is_found_at_engine_start_default_alias_first_then_newest() {
        let temp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temp.path()).unwrap();
        let versions = home.join(".nvm/versions/node");
        for version in ["v18.20.4", "v20.17.0", "v22.11.0", "not-a-version"] {
            program(&versions.join(version).join("bin/codex"), 0o755);
        }
        let search = crate::harnesses::Search::engine(None, &[], Some(home.clone()));
        let newest = resolve_harnesses(Harnesses::default(), &search, None, None);
        assert_eq!(newest.codex, Some(versions.join("v22.11.0/bin/codex")));
        std::fs::create_dir_all(home.join(".nvm/alias")).unwrap();
        std::fs::write(home.join(".nvm/alias/default"), "20\n").unwrap();
        let search = crate::harnesses::Search::engine(None, &[], Some(home.clone()));
        let aliased = resolve_harnesses(Harnesses::default(), &search, None, None);
        assert_eq!(aliased.codex, Some(versions.join("v20.17.0/bin/codex")));
        std::fs::write(home.join(".nvm/alias/default"), "lts/*\n").unwrap();
        let search = crate::harnesses::Search::engine(None, &[], Some(home.clone()));
        let named = resolve_harnesses(Harnesses::default(), &search, None, None);
        assert_eq!(named.codex, Some(versions.join("v22.11.0/bin/codex")));
    }

    #[test]
    fn nvm_reads_a_bounded_number_of_versions() {
        let temp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temp.path()).unwrap();
        let versions = home.join(".nvm/versions/node");
        for minor in 0..200 {
            std::fs::create_dir_all(versions.join(format!("v1.{minor}.0/bin"))).unwrap();
        }
        let search = crate::harnesses::Search::engine(None, &[], Some(home.clone()));
        let looked = search.looked("codex");
        let nvm = looked
            .iter()
            .filter(|folder| folder.starts_with(&versions))
            .count();
        assert!((1..=8).contains(&nvm), "{nvm}");
    }

    #[test]
    fn home_tool_folders_are_found_at_engine_start_and_shims_never_are() {
        let temp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temp.path()).unwrap();
        for shim in [".volta/bin", ".asdf/shims", ".local/share/mise/shims"] {
            program(&home.join(shim).join("codex"), 0o755);
        }
        let search = crate::harnesses::Search::engine(None, &[], Some(home.clone()));
        assert_eq!(
            resolve_harnesses(Harnesses::default(), &search, None, None).codex,
            None
        );
        let looked = search.looked("codex");
        assert!(looked.contains(&home.join(".volta/bin")));
        assert!(looked.contains(&home.join(".asdf/shims")));
        for (folder, found) in [
            (".local/share/fnm/aliases/default/bin", true),
            ("Library/pnpm", true),
            (".npm-global/bin", true),
            (".local/bin", true),
        ] {
            let entry = home.join(folder).join("codex");
            program(&entry, 0o755);
            let search = crate::harnesses::Search::engine(None, &[], Some(home.clone()));
            let codex = resolve_harnesses(Harnesses::default(), &search, None, None).codex;
            assert_eq!(codex.is_some(), found, "{folder}");
            std::fs::remove_file(&entry).unwrap();
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_catalog_and_engine_start_find_the_same_codex_and_claude() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let home = root.join("home");
        let on_path = root.join("on-path");
        let system = root.join("system");
        std::fs::create_dir_all(&on_path).unwrap();
        let path = std::env::join_paths([on_path.clone()]).unwrap();
        let layouts: [(&str, PathBuf); 6] = [
            ("codex", on_path.join("codex")),
            ("codex", system.join("codex")),
            ("codex", home.join(".local/bin/codex")),
            ("codex", home.join(".nvm/versions/node/v22.11.0/bin/codex")),
            ("claude-code", home.join(".claude/local/claude")),
            ("claude-code", home.join("bin/claude")),
        ];
        for (id, entry) in layouts {
            program(&entry, 0o755);
            let search = crate::harnesses::Search::engine(
                Some(&path),
                &[system.clone()],
                Some(home.clone()),
            );
            let started = resolve_harnesses(Harnesses::default(), &search, Some(&path), None);
            let catalog = crate::harnesses::catalog(&search);
            let listed = catalog
                .iter()
                .find(|row| row.id == id)
                .and_then(|row| row.executable.clone());
            let found = if id == "codex" {
                started.codex
            } else {
                started.claude
            };
            assert_eq!(found.as_ref(), Some(&entry), "{id} {}", entry.display());
            assert_eq!(listed, found, "{id} {}", entry.display());
            std::fs::remove_file(&entry).unwrap();
        }
    }

    #[test]
    fn the_default_socket_sits_in_the_data_root() {
        let layout = Layout::new(PathBuf::from("/srv/crazy/engine")).unwrap();
        assert_eq!(layout.socket(), Path::new("/srv/crazy/engine/engine.sock"));
        let error = Layout::new(PathBuf::from("relative")).unwrap_err();
        assert!(error.0.contains("absolute"), "{error}");
    }
}
