use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The environment variable that overrides the engine's data root.
pub const HOME_ENV: &str = "CRAZYAGENT_HOME";
/// The default socket name inside the data root.
pub const SOCKET_NAME: &str = "engine.sock";

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_default_socket_sits_in_the_data_root() {
        let layout = Layout::new(PathBuf::from("/srv/crazy/engine")).unwrap();
        assert_eq!(layout.socket(), Path::new("/srv/crazy/engine/engine.sock"));
        let error = Layout::new(PathBuf::from("relative")).unwrap_err();
        assert!(error.0.contains("absolute"), "{error}");
    }
}
