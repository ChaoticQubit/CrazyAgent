use crate::credentials::{SecretString, Store};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

mod api;
mod clone;
pub(crate) mod http;
mod state;
#[cfg(all(test, target_os = "macos"))]
#[path = "github/test_server.rs"]
pub(crate) mod test_server;
#[cfg(all(test, target_os = "macos"))]
#[path = "github/github_tests.rs"]
mod tests;

pub use api::Endpoints;

pub const CLIENT_ID: &str = "Ov23liHICPRkMyS5sNMV";
pub const SCOPES: &str = "repo read:user";
pub const REPOS_PER_PAGE: u32 = 100;
pub const REPO_PAGE_MAX: u32 = 100;

/// How the engine holds the owner's GitHub token: device sign-in, a pasted token, a token
/// reused from a signed-in `gh` CLI, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubMethod {
    Device,
    Token,
    Cli,
    None,
}

/// What the GitHub settings page shows: signed in or not, the login name, the method,
/// whether a device sign-in is waiting for the owner, and a plain problem sentence such as
/// "GitHub sign-in is not set up yet: client id not configured."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubStatus {
    pub signed_in: bool,
    pub login: Option<String>,
    pub method: GitHubMethod,
    pub waiting: bool,
    pub problem: Option<String>,
}

/// The code the owner types at GitHub's device page, that page's address and how many
/// seconds the code stays valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubDeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_s: u64,
}

/// One repository on the signed-in owner's list. `refused` holds the failure code when
/// CrazyAgent can't clone it, such as a name starting with a dot; the name and branch are
/// then shown escaped and bounded, and the rest of the page still lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubRepo {
    pub full_name: String,
    pub private: bool,
    pub default_branch: String,
    pub size_kb: u64,
    pub refused: Option<String>,
}

/// One page of repositories, newest activity first, and the next page number if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubRepoPage {
    pub repos: Vec<GitHubRepo>,
    pub next: Option<u32>,
}

/// A finished clone: which repository and the project folder it landed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneReceipt {
    pub full_name: String,
    pub path: String,
}

const SERVICE_LEAF: &str = "github";
const TOKEN_ACCOUNT: &str = "token";
const CLI_ENV: [&str; 3] = ["HOME", "XDG_CONFIG_HOME", "GH_CONFIG_DIR"];

struct Pending {
    cancel: Arc<AtomicBool>,
    waiting: bool,
    problem: Option<String>,
}

static PENDING: Mutex<BTreeMap<PathBuf, Pending>> = Mutex::new(BTreeMap::new());

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn pending() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Pending>> {
    PENDING.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(all(test, target_os = "macos"))]
static LOOPBACK: Mutex<BTreeMap<PathBuf, u16>> = Mutex::new(BTreeMap::new());

/// Points the engine at one root to a GitHub stand-in on 127.0.0.1, or back to the real
/// GitHub with `None`. Test builds only; every other root keeps the real GitHub.
/// Inputs: the engine root and the stand-in's port.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn use_loopback(root: &Path, port: Option<u16>) {
    let mut roots = LOOPBACK.lock().unwrap_or_else(PoisonError::into_inner);
    match port {
        Some(port) => roots.insert(root.to_path_buf(), port),
        None => roots.remove(root),
    };
}

/// Where a test may hold one engine root's GitHub work: a device sign-in between saving
/// its token and settling, or a status read between the waiting flag and the saved sign-in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Pause {
    AfterKeep,
    InStatus,
}

#[cfg(all(test, target_os = "macos"))]
type Paused = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);

#[cfg(all(test, target_os = "macos"))]
static PAUSES: Mutex<BTreeMap<(PathBuf, Pause), Paused>> = Mutex::new(BTreeMap::new());

/// Holds the next pass of one engine root through a pause point. Test builds only.
/// Inputs: the engine root and the point. Output: the receiver told once the work reaches
/// the point, and the sender that lets it go on.
#[cfg(all(test, target_os = "macos"))]
fn pause_at(
    root: &Path,
    point: Pause,
) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (reached, told) = std::sync::mpsc::channel();
    let (go, resume) = std::sync::mpsc::channel();
    PAUSES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert((root.to_path_buf(), point), (reached, resume));
    (told, go)
}

/// Waits at a pause point while a test holds it for this root, once. Test builds only.
/// Inputs: the engine root and the point.
#[cfg(all(test, target_os = "macos"))]
fn paused_at(root: &Path, point: Pause) {
    let paused = PAUSES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&(root.to_path_buf(), point));
    if let Some((reached, resume)) = paused
        && reached.send(()).is_ok()
    {
        let _ = resume.recv();
    }
}

/// A pause point outside test builds: nothing ever waits. Inputs: unused.
#[cfg(not(all(test, target_os = "macos")))]
fn paused_at(_root: &Path, _point: Pause) {}

/// The GitHub an engine root talks to. Input: the engine root. Output: the real GitHub.
#[cfg(not(all(test, target_os = "macos")))]
fn endpoints(_root: &Path) -> Endpoints {
    Endpoints::github()
}

/// The GitHub an engine root talks to: the stand-in `use_loopback` set for that root,
/// else the real GitHub. Input: the engine root. Output: the endpoints.
#[cfg(all(test, target_os = "macos"))]
fn endpoints(root: &Path) -> Endpoints {
    let roots = LOOPBACK.lock().unwrap_or_else(PoisonError::into_inner);
    roots
        .get(root)
        .map_or_else(Endpoints::github, |port| Endpoints::loopback(*port))
}

/// Names the credential store namespace of one engine root, so two engine folders never
/// share or overwrite each other's GitHub token. Input: the engine root.
/// Output: `dev.crazyagent.e` and 12 hex digits of the root path's SHA-256.
pub fn namespace(root: &Path) -> String {
    let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
    let hex: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("dev.crazyagent.e{hex}")
}

/// Turns a GitHub failure code into the plain sentence the settings page shows.
/// Input: the code. Output: one sentence that names what the owner can do next.
pub fn problem(code: &str) -> String {
    match code {
        "github_client_id_not_configured" => {
            "GitHub sign-in is not set up yet: client id not configured."
        }
        "github_client_id_refused" => "GitHub did not accept this app's client id.",
        "github_device_flow_refused" => {
            "GitHub refused device sign-in for this app. Paste a token or use the GitHub CLI."
        }
        "github_sign_in_expired" => "The GitHub code expired before it was entered. Sign in again.",
        "github_sign_in_denied" => "GitHub sign-in was denied.",
        "github_token_refused" => "GitHub did not accept the token. Sign in again.",
        "github_unreachable" => "GitHub could not be reached. Check the network and try again.",
        "github_reply_invalid" => "GitHub sent a reply the engine could not read. Try again.",
        "github_cli_missing" => "The GitHub CLI (gh) is not installed.",
        "github_cli_not_signed_in" => "The GitHub CLI is not signed in. Run gh auth login first.",
        "credential_missing" => {
            "The saved GitHub token is gone from the credential store. Sign in again."
        }
        "credential_store_unavailable" => "The system credential store could not be used.",
        "github_config_unsafe" | "github_config_invalid" => {
            "The GitHub settings file in the engine folder is unreadable or not private."
        }
        "github_state_invalid" | "github_state_unavailable" => {
            "The saved GitHub sign-in could not be read. Sign in again."
        }
        _ => "GitHub sign-in failed. Try again.",
    }
    .to_owned()
}

fn replace_pending(root: &Path, next: Option<Pending>) {
    let mut pending = pending();
    cancel_waiting(&mut pending, root);
    if let Some(next) = next {
        pending.insert(root.to_path_buf(), next);
    }
}

/// Ends the device sign-in waiting for one engine root, if any: forgets it and sets its
/// cancel flag, so its thread saves nothing. Inputs: the held pending map and the root.
fn cancel_waiting(pending: &mut BTreeMap<PathBuf, Pending>, root: &Path) {
    if let Some(old) = pending.remove(root) {
        old.cancel.store(true, Ordering::Release);
    }
}

fn settle(root: &Path, cancel: &AtomicBool, error: Option<EngineError>) {
    settle_held(&mut pending(), root, cancel, error);
}

/// Ends one device sign-in's waiting and records its problem, if that sign-in is still the
/// root's current one. Inputs: the held pending map, the root, the sign-in's cancel flag
/// and its failure, if any.
fn settle_held(
    pending: &mut BTreeMap<PathBuf, Pending>,
    root: &Path,
    cancel: &AtomicBool,
    error: Option<EngineError>,
) {
    if let Some(entry) = pending.get_mut(root)
        && std::ptr::eq(Arc::as_ptr(&entry.cancel), cancel)
    {
        entry.waiting = false;
        entry.problem = error
            .filter(|error| error.0 != "github_sign_in_cancelled")
            .map(|error| problem(&error.0));
    }
}

/// The owner's GitHub account for one engine root: the token lives in the system
/// credential store under the root's namespace, and `<root>/github.json` keeps only the
/// method, login name and the store reference.
#[derive(Clone)]
pub struct GitHub {
    root: PathBuf,
    namespace: String,
    store: Arc<dyn Store>,
    endpoints: Endpoints,
}

impl GitHub {
    /// Opens the real GitHub account of an engine root through this computer's store.
    /// Input: the private engine root. Output: the account, or `credential_store_unavailable`.
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        let namespace = namespace(root);
        let store = crate::credentials::platform_store(&namespace)?;
        Ok(Self::new(root, namespace, store, endpoints(root)))
    }

    /// Builds the account from parts, such as a loopback GitHub stand-in.
    /// Inputs: an absolute engine root, the store's namespace, the store and endpoints.
    pub fn new(
        root: &Path,
        namespace: String,
        store: Arc<dyn Store>,
        endpoints: Endpoints,
    ) -> Self {
        assert!(root.is_absolute());
        assert!(namespace.starts_with("dev.crazyagent."));
        Self {
            root: root.to_path_buf(),
            namespace,
            store,
            endpoints,
        }
    }

    fn service(&self) -> String {
        format!("{}.{SERVICE_LEAF}", self.namespace)
    }

    fn signed_out(&self, waiting: bool, problem: Option<String>) -> GitHubStatus {
        GitHubStatus {
            signed_in: false,
            login: None,
            method: GitHubMethod::None,
            waiting,
            problem,
        }
    }

    /// Reads what the settings page shows. A saved sign-in counts only while its token is
    /// still in the store; any failure becomes a plain problem sentence. The waiting flag and
    /// the saved sign-in are read under the pending lock every save takes, so a sign-in
    /// never reads as saved and still waiting. Output: the status, which never carries the
    /// token.
    pub fn status(&self) -> GitHubStatus {
        let pending = pending();
        let (waiting, last) = pending
            .get(&self.root)
            .map(|entry| (entry.waiting, entry.problem.clone()))
            .unwrap_or((false, None));
        paused_at(&self.root, Pause::InStatus);
        let status = match state::load(&self.root) {
            Ok(Some(saved)) => match self.store.get(&saved.credential) {
                Ok(_) => GitHubStatus {
                    signed_in: true,
                    login: Some(saved.login),
                    method: saved.method,
                    waiting,
                    problem: last,
                },
                Err(error) => self.signed_out(waiting, Some(problem(&error.0))),
            },
            Ok(None) => {
                let setup = state::client_id(&self.root)
                    .err()
                    .map(|error| problem(&error.0));
                self.signed_out(waiting, last.or(setup))
            }
            Err(error) => self.signed_out(waiting, Some(problem(&error.0))),
        };
        drop(pending);
        status
    }

    /// Starts a device sign-in and finishes it on a background thread once the owner
    /// enters the code at GitHub; `status` shows `waiting` until then and any problem
    /// after. A new sign-in cancels the previous one. Output: the code to show the owner.
    pub fn sign_in(&self) -> Result<GitHubDeviceCode, EngineError> {
        let client_id = state::client_id(&self.root)?;
        let start = api::start_device(&self.endpoints, &client_id)?;
        let code = start.code.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        replace_pending(
            &self.root,
            Some(Pending {
                cancel: Arc::clone(&cancel),
                waiting: true,
                problem: None,
            }),
        );
        let account = self.clone();
        let flag = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new()
            .name("github-sign-in".into())
            .spawn(move || {
                let outcome = api::finish_device(&account.endpoints, &client_id, start, &flag)
                    .and_then(|token| {
                        account.keep(&token, GitHubMethod::Device, Some(flag.as_ref()))
                    });
                paused_at(&account.root, Pause::AfterKeep);
                settle(&account.root, &flag, outcome.err());
            });
        if spawned.is_err() {
            settle(&self.root, &cancel, Some(failure("github_sign_in_failed")));
            return Err(failure("github_sign_in_failed"));
        }
        Ok(code)
    }

    /// Signs in with a token the owner pasted through the secret channel. GitHub must
    /// accept it first. Input: the token. Output: the signed-in status.
    pub fn use_token(&self, token: SecretString) -> Result<GitHubStatus, EngineError> {
        self.sign_in_with(&token, GitHubMethod::Token)
    }

    /// Signs in with the token of the signed-in GitHub CLI, copied into the store.
    /// Output: the signed-in status, or `github_cli_missing`, `github_cli_not_signed_in`.
    pub fn use_cli(&self) -> Result<GitHubStatus, EngineError> {
        let gh = api::find_cli().ok_or_else(|| failure("github_cli_missing"))?;
        let env: Vec<(&str, OsString)> = CLI_ENV
            .into_iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
            .collect();
        let env: Vec<(&str, &OsStr)> = env
            .iter()
            .map(|(name, value)| (*name, value.as_os_str()))
            .collect();
        self.use_cli_at(gh, &env)
    }

    /// Signs in with the token a given `gh` prints for `github.com`.
    /// Inputs: the `gh` path and its environment. Output: the signed-in status.
    pub fn use_cli_at(
        &self,
        gh: &Path,
        env: &[(&str, &OsStr)],
    ) -> Result<GitHubStatus, EngineError> {
        let token = api::cli_token(gh, env)?;
        self.sign_in_with(&token, GitHubMethod::Cli)
    }

    fn sign_in_with(
        &self,
        token: &SecretString,
        method: GitHubMethod,
    ) -> Result<GitHubStatus, EngineError> {
        let login = self.keep(token, method, None)?;
        Ok(GitHubStatus {
            signed_in: true,
            login: Some(login),
            method,
            waiting: false,
            problem: None,
        })
    }

    /// Saves a token GitHub accepted. A device sign-in passes its cancel flag, saves
    /// nothing once a sign-out or newer sign-in set it, and ends its own waiting once saved;
    /// any other sign-in that saved its token cancels the one waiting. All of it runs under
    /// the pending lock sign-out and status hold throughout, so no token lands during or
    /// after a sign-out and no status reads a saved sign-in as still waiting.
    /// Inputs: the token, the method and the device flag. Output: the login, or the refusal.
    fn keep(
        &self,
        token: &SecretString,
        method: GitHubMethod,
        device: Option<&AtomicBool>,
    ) -> Result<String, EngineError> {
        let login = api::login(&self.endpoints, token)?;
        let mut pending = pending();
        if device.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
            return Err(failure("github_sign_in_cancelled"));
        }
        let credential = self.store.put(&self.service(), TOKEN_ACCOUNT, token)?;
        state::save(
            &self.root,
            &state::Saved {
                method,
                login: login.clone(),
                credential,
            },
        )?;
        match device {
            Some(cancel) => settle_held(&mut pending, &self.root, cancel, None),
            None => cancel_waiting(&mut pending, &self.root),
        }
        Ok(login)
    }

    /// Signs out: cancels a waiting device sign-in, deletes the stored token and forgets
    /// the saved sign-in, all under the pending lock every save takes, so no pasted, CLI or
    /// device token is saved between the read and the forget. Output: the signed-out status.
    pub fn sign_out(&self) -> Result<GitHubStatus, EngineError> {
        let mut pending = pending();
        cancel_waiting(&mut pending, &self.root);
        if let Some(saved) = state::load(&self.root)? {
            self.store.delete(&saved.credential)?;
        }
        state::forget(&self.root)?;
        drop(pending);
        Ok(self.signed_out(false, None))
    }

    fn saved(&self) -> Result<state::Saved, EngineError> {
        state::load(&self.root)?.ok_or_else(|| failure("github_not_signed_in"))
    }

    /// Lists one page of the owner's repositories. Input: a page from 1 to 100.
    /// Output: the page, or `github_not_signed_in`.
    pub fn repos(&self, page: u32) -> Result<GitHubRepoPage, EngineError> {
        let token = self.store.get(&self.saved()?.credential)?;
        api::repos(&self.endpoints, &token, page)
    }

    /// Clones one repository into the projects home through the store's git helper.
    /// Inputs: the projects home folder and `owner/name`. Output: the receipt.
    pub fn clone_repo(&self, home: &Path, full_name: &str) -> Result<CloneReceipt, EngineError> {
        let saved = self.saved()?;
        clone::clone_repo(
            &*self.store,
            &saved.credential,
            &self.endpoints,
            home,
            full_name,
        )
    }
}
