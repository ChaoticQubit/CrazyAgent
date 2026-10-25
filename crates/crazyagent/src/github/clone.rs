use super::CloneReceipt;
use super::api::Endpoints;
use crate::credentials::Store;
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GIT: &str = "/usr/bin/git";
const CLONE_TIME: Duration = Duration::from_secs(1800);
const GIT_USER: &str = "x-access-token";
const NAME_BYTES_MAX: usize = 100;
const ERROR_BYTES_MAX: u64 = 16 << 10;

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

/// Builds the clone command without running it. The token stays in the credential store:
/// git asks the store's helper for it, the remote URL carries no user or password, and the
/// command's arguments and environment hold no secret. Hooks, templates, submodules and
/// every transport but the endpoint's own scheme are off.
/// Inputs: endpoints, the helper value, the destination and `owner/name`.
/// Output: the command and the remote URL it clones.
pub(super) fn clone_command(
    endpoints: &Endpoints,
    helper: &str,
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
    let mut command = Command::new(GIT);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/var/empty")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/usr/bin/false")
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(["-c", "credential.helper="])
        .arg("-c")
        .arg(format!("credential.helper={helper}"))
        .args(["-c", "credential.interactive=never"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "core.pager=cat"])
        .args(["-c", "core.askPass=/usr/bin/false"])
        .args(["-c", "protocol.allow=never"])
        .arg("-c")
        .arg(format!("protocol.{scheme}.allow=always"))
        .args(["-c", "http.followRedirects=false"])
        .args(["clone", "--quiet", "--no-recurse-submodules", "--template="])
        .arg("--")
        .arg(remote.as_str())
        .arg(destination)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    Ok((command, remote))
}

/// Clones one repository into the projects home through the credential store, so the
/// token never lands in argv, the environment, `.git/config` or the remote URL. git
/// writes into a hidden staging folder in the home, renamed into place only after a
/// whole clone and deleted on any failure, so the home never shows a half-written project.
/// Output: the receipt, or `clone_destination_exists`, `github_clone_failed`.
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
    let mut token = [0u8; 8];
    getrandom::fill(&mut token).map_err(|_| failure("github_clone_failed"))?;
    let staging = home.join(format!(
        ".crazyagent-cloning-{}",
        token.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    let result = fetch(endpoints, &helper, &staging, &destination, name);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn fetch(
    endpoints: &Endpoints,
    helper: &str,
    staging: &Path,
    destination: &Path,
    name: &str,
) -> Result<CloneReceipt, EngineError> {
    let (mut command, remote) = clone_command(endpoints, helper, staging, name)?;
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
    let config =
        std::fs::read(staging.join(".git/config")).map_err(|_| failure("github_clone_failed"))?;
    let config = String::from_utf8_lossy(&config);
    assert!(config.contains(remote.as_str()));
    assert!(!config.contains("credential"));
    if std::fs::symlink_metadata(destination).is_ok() {
        return Err(failure("clone_destination_exists"));
    }
    std::fs::rename(staging, destination).map_err(|_| failure("github_clone_failed"))?;
    Ok(CloneReceipt {
        full_name: name.to_owned(),
        path: destination.to_string_lossy().into_owned(),
    })
}
