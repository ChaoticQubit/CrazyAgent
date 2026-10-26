use super::CloneReceipt;
use super::api::Endpoints;
use crate::credentials::Store;
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
const CLONE_TIME: Duration = Duration::from_secs(1800);
const GIT_USER: &str = "x-access-token";
const NAME_BYTES_MAX: usize = 100;
const ERROR_BYTES_MAX: u64 = 16 << 10;
const CONFIG_BYTES_MAX: u64 = 64 << 10;

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

/// Checks a repository name in the `owner/name` form GitHub uses.
/// Output: owner and name, or `github_repo_name_invalid`.
pub(super) fn full_name(value: &str) -> Result<(&str, &str), EngineError> {
    let invalid = || failure("github_repo_name_invalid");
    let (owner, name) = value.split_once('/').ok_or_else(invalid)?;
    for part in [owner, name] {
        if part.is_empty()
            || part.len() > NAME_BYTES_MAX
            || part.starts_with(['.', '-'])
            || part.ends_with(".git")
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(invalid());
        }
    }
    Ok((owner, name))
}

/// Finds the git the engine runs: first in the engine's own PATH, never an agent's, then
/// in the platform's fixed folder. Input: the engine's PATH value. Output: the executable
/// with links resolved, or none when neither holds one.
fn git_program(path: Option<&OsStr>) -> Option<PathBuf> {
    crate::paths::find_in_path("git", path).or_else(|| {
        let folder = fixed_git_folder()?;
        crate::paths::find_in_path("git", Some(folder.as_os_str()))
    })
}

/// The folder git installs into when the engine's PATH misses it: `/usr/bin` on Unix.
#[cfg(not(windows))]
fn fixed_git_folder() -> Option<OsString> {
    Some(OsString::from("/usr/bin"))
}

/// The folder git installs into when the engine's PATH misses it: Git for Windows under
/// `%ProgramFiles%`, when that variable holds an absolute folder.
#[cfg(windows)]
fn fixed_git_folder() -> Option<OsString> {
    let programs = PathBuf::from(std::env::var_os("ProgramFiles")?);
    programs
        .is_absolute()
        .then(|| programs.join("Git").join("cmd").into_os_string())
}

/// Starts a git command that reads no system, global or owner configuration: an empty
/// private folder is its home and working folder, so no `.gitconfig`, `.netrc` or XDG git
/// file of the owner applies. Windows keeps the engine's PATH and SystemRoot, which Git
/// for Windows needs for its shell and network stack. Inputs: git and the empty home.
/// Output: the command, without arguments.
fn git_command(git: &Path, empty_home: &Path) -> Command {
    let mut command = Command::new(git);
    command
        .env_clear()
        .env("HOME", empty_home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
        .current_dir(empty_home);
    #[cfg(not(windows))]
    command.env("PATH", "/usr/bin:/bin");
    #[cfg(windows)]
    for name in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

/// Builds the clone command without running it. The token stays in the credential store:
/// git asks the store's helper for it, the remote URL carries no user or password, and the
/// command's arguments and environment hold no secret. Hooks, templates, submodules and
/// every transport but the endpoint's own scheme are off, and no askpass program runs.
/// Inputs: endpoints, the helper value, git, the empty home, the destination and
/// `owner/name`. Output: the command and the remote URL it clones.
pub(super) fn clone_command(
    endpoints: &Endpoints,
    helper: &str,
    git: &Path,
    empty_home: &Path,
    destination: &Path,
    name: &str,
) -> Result<(Command, url::Url), EngineError> {
    full_name(name)?;
    let remote = endpoints
        .web
        .join(&format!("{name}.git"))
        .map_err(|_| failure("github_repo_name_invalid"))?;
    assert!(remote.username().is_empty() && remote.password().is_none());
    let scheme = if endpoints.allow_http {
        "http"
    } else {
        "https"
    };
    assert_eq!(remote.scheme(), scheme);
    let mut command = git_command(git, empty_home);
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(["-c", "credential.helper="])
        .arg("-c")
        .arg(format!("credential.helper={helper}"))
        .args(["-c", "credential.interactive=never"])
        .arg("-c")
        .arg(format!("core.hooksPath={NULL_DEVICE}"))
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "core.pager=cat"])
        .args(["-c", "core.askPass="])
        .args(["-c", "protocol.allow=never"])
        .arg("-c")
        .arg(format!("protocol.{scheme}.allow=always"))
        .args(["-c", "http.followRedirects=false"])
        .args(["clone", "--quiet", "--no-recurse-submodules", "--template="])
        .arg("--")
        .arg(remote.as_str())
        .arg(destination)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    Ok((command, remote))
}

/// Clones one repository into the projects home through the credential store, so the
/// token never lands in argv, the environment, `.git/config` or the remote URL. git
/// writes into a hidden private staging folder in the home, beside the empty folder it
/// uses as its home; the clone is renamed into place only when whole and the staging
/// folder is deleted afterwards, so the home never shows a half-written project.
/// Output: the receipt, or `clone_destination_exists`, `github_git_missing`,
/// `github_clone_failed`.
pub(super) fn clone_repo(
    store: &dyn Store,
    credential: &CredentialRef,
    endpoints: &Endpoints,
    home: &Path,
    name: &str,
) -> Result<CloneReceipt, EngineError> {
    let (_, folder) = full_name(name)?;
    if !home.is_absolute() || !home.is_dir() {
        return Err(failure("projects_home_not_set"));
    }
    let destination: PathBuf = home.join(folder);
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(failure("clone_destination_exists"));
    }
    let helper = store.git_credential_helper(credential, GIT_USER)?;
    let git = git_program(std::env::var_os("PATH").as_deref())
        .ok_or_else(|| failure("github_git_missing"))?;
    let mut token = [0u8; 8];
    getrandom::fill(&mut token).map_err(|_| failure("github_clone_failed"))?;
    let staging = home.join(format!(
        ".crazyagent-cloning-{}",
        token.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    private_folder(&staging).map_err(|_| failure("github_clone_failed"))?;
    let result = fetch(endpoints, &helper, &git, &staging, &destination, name);
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Creates a folder only this user can open (mode 0700). Input: the path. Output: the
/// io result.
#[cfg(unix)]
fn private_folder(path: &Path) -> std::io::Result<()> {
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(path)
}

/// Creates a folder with the access the projects home passes down to new folders on
/// Windows. Input: the path. Output: the io result.
#[cfg(not(unix))]
fn private_folder(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

fn fetch(
    endpoints: &Endpoints,
    helper: &str,
    git: &Path,
    staging: &Path,
    destination: &Path,
    name: &str,
) -> Result<CloneReceipt, EngineError> {
    let empty_home = staging.join("home");
    let checkout = staging.join("repo");
    private_folder(&empty_home).map_err(|_| failure("github_clone_failed"))?;
    let (mut command, remote) =
        clone_command(endpoints, helper, git, &empty_home, &checkout, name)?;
    let mut child =
        crate::spawn_gate::spawn(&mut command).map_err(|_| failure("github_clone_failed"))?;
    let stderr = child.stderr.take().expect("git stderr is piped");
    let reader = std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = stderr.take(ERROR_BYTES_MAX).read_to_end(&mut sink);
        sink.len()
    });
    let deadline = Instant::now() + CLONE_TIME;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| failure("github_clone_failed"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(failure("github_clone_timed_out"));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = reader.join();
    if !status.success() {
        return Err(failure("github_clone_failed"));
    }
    verify_config(git, &empty_home, &checkout.join(".git/config"), &remote)?;
    if std::fs::symlink_metadata(destination).is_ok() {
        return Err(failure("clone_destination_exists"));
    }
    std::fs::rename(&checkout, destination).map_err(|_| failure("github_clone_failed"))?;
    Ok(CloneReceipt {
        full_name: name.to_owned(),
        path: destination.to_string_lossy().into_owned(),
    })
}

/// Checks the `.git/config` a clone wrote: its origin URL is the remote cloned and it holds
/// no `credential.` key, so the store helper stayed out of the project. git lists the keys,
/// so a name in any case or subsection compares alike and URL text is never a key.
/// Inputs: git, the empty home, the config path and the remote. Output: `()`, or
/// `github_clone_failed`.
pub(super) fn verify_config(
    git: &Path,
    empty_home: &Path,
    config: &Path,
    remote: &url::Url,
) -> Result<(), EngineError> {
    let failed = || failure("github_clone_failed");
    let metadata = std::fs::symlink_metadata(config).map_err(|_| failed())?;
    if !metadata.is_file() || metadata.len() > CONFIG_BYTES_MAX {
        return Err(failed());
    }
    let listed = crate::spawn_gate::output(
        git_command(git, empty_home)
            .args(["config", "--file"])
            .arg(config)
            .args(["--no-includes", "-z", "--list"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|_| failed())?;
    if !listed.status.success() {
        return Err(failed());
    }
    let mut origins = 0_usize;
    for entry in listed.stdout.split(|byte| *byte == 0) {
        let (key, value) = match entry.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&entry[..at], &entry[at + 1..]),
            None => (entry, &entry[entry.len()..]),
        };
        if key.starts_with(b"credential.") {
            return Err(failed());
        }
        if key == b"remote.origin.url" {
            if value != remote.as_str().as_bytes() {
                return Err(failed());
            }
            origins += 1;
        }
    }
    if origins != 1 {
        return Err(failed());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_comes_from_the_engine_path_then_the_fixed_folder() {
        let folder = tempfile::tempdir().unwrap();
        let planted = folder
            .path()
            .join(if cfg!(windows) { "git.exe" } else { "git" });
        std::fs::write(&planted, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            git_program(Some(folder.path().as_os_str())),
            Some(planted.canonicalize().unwrap())
        );
        let empty = tempfile::tempdir().unwrap();
        let fixed = fixed_git_folder()
            .and_then(|folder| crate::paths::find_in_path("git", Some(folder.as_os_str())));
        assert_eq!(git_program(Some(empty.path().as_os_str())), fixed);
        assert_eq!(git_program(None), fixed);
        #[cfg(target_os = "macos")]
        assert_eq!(fixed, Some(PathBuf::from("/usr/bin/git")));
    }

    #[test]
    fn clone_runs_the_resolved_git_in_a_private_empty_home() {
        let staging = tempfile::tempdir().unwrap();
        let empty = staging.path().join("home");
        std::fs::create_dir(&empty).unwrap();
        let git = staging.path().join("git");
        let (command, remote) = clone_command(
            &Endpoints::loopback(9),
            "!true",
            &git,
            &empty,
            &staging.path().join("repo"),
            "octo-cat/tool",
        )
        .unwrap();
        assert_eq!(remote.as_str(), "http://127.0.0.1:9/octo-cat/tool.git");
        assert_eq!(command.get_program(), git.as_os_str());
        assert_eq!(command.get_current_dir(), Some(empty.as_path()));
        let envs: Vec<(String, String)> = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value
                        .map(|value| value.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
            })
            .collect();
        let env = |name: &str| {
            envs.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(env("HOME"), empty.to_str());
        assert_eq!(env("GIT_CONFIG_GLOBAL"), Some(NULL_DEVICE));
        assert_eq!(env("GIT_ASKPASS"), Some(""));
        let argv: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(argv.contains(&format!("core.hooksPath={NULL_DEVICE}")));
        assert!(argv.contains(&"core.askPass=".to_owned()));
        assert!(
            argv.iter()
                .chain(envs.iter().map(|(_, value)| value))
                .all(|text| !text.contains("/var/empty") && !text.contains("/usr/bin/false"))
        );
    }
}
