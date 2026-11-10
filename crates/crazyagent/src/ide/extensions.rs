use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The built-in CrazyAgent extension's id, always allowed at its own version.
pub const BUILT_IN: &str = "crazyagent.crazyagent-ide";

/// The version word the IDE sends to ask for the newest version Open VSX lists.
pub const LATEST: &str = "latest";

/// The only extension source: Open VSX's API.
pub const OPEN_VSX: &str = "https://open-vsx.org/api/";

const IDE: &str = "ide";
const FOLDER: &str = "@extensions";
const POLICY: &str = "extensions.json";
const CACHE: &str = "vsix";
const SERVER_MAIN: &str = "out/server-main.js";
const POLICY_BYTES_MAX: u64 = 256 * 1024;
const METADATA_BYTES_MAX: u64 = 1024 * 1024;
const HASH_BYTES_MAX: u64 = 1024;
const VSIX_BYTES_MAX: u64 = 256 * 1024 * 1024;
const LIST_BYTES_MAX: u64 = 256 * 1024;
const APPROVED_MAX: usize = 64;
/// The extension the build grants the default chat participant, which brings the Chat view
/// back whatever `chat.disableAIFeatures` says (DEC-098).
const AI_LOCKED: &str = "github.copilot-chat";
const ASKS_PER_PROJECT_MAX: usize = 8;
const ASKS_MAX: usize = 256;
const ASK_TTL_MS: i64 = 10 * 60 * 1000;
const UPDATE_EVERY_MS: i64 = 24 * 60 * 60 * 1000;
const READ_TIME: Duration = Duration::from_secs(30);
const DOWNLOAD_TIME: Duration = Duration::from_secs(5 * 60);
const CLI_TIME: Duration = Duration::from_secs(120);
const CLI_POLL: Duration = Duration::from_millis(50);
static WRITES: Mutex<()> = Mutex::new(());

/// One extension the owner approved: its id, the exact version and the sha256 of the VSIX
/// Open VSX served for it, pinned when the owner approved it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approved {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub approved_ms: i64,
}

/// One install or update the IDE asked the owner to approve in the desktop app: the
/// project it was asked from, the extension, the version or `latest`, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    pub project: ProjectId,
    pub id: String,
    pub version: String,
    pub asked_ms: i64,
}

/// A newer version Open VSX lists for an approved extension. It is shown and never
/// installed until the owner approves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub id: String,
    pub version: String,
}

/// One ask as the desktop confirm shows it: the extension, the version or `latest`, and
/// the version approved now, for an update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskView {
    pub id: String,
    pub version: String,
    pub current: Option<String>,
}

/// What the IDE shows for one project: the approved extensions, this project's waiting
/// asks and the available updates with their approved version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    pub approved: Vec<Update>,
    pub asks: Vec<AskView>,
    pub updates: Vec<AskView>,
}

/// How an IDE install request ended: the extension is approved at that version already,
/// or the owner was asked to confirm it in the desktop app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    Approved,
    Confirm,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    approved: Vec<Approved>,
    asks: Vec<Ask>,
    updates: Vec<Update>,
    checked_ms: Option<i64>,
}

#[derive(Deserialize)]
struct Metadata {
    namespace: String,
    name: String,
    version: String,
    files: Files,
}

#[derive(Deserialize)]
struct Files {
    download: String,
    sha256: Option<String>,
}

/// Where extensions come from: Open VSX's API, or in tests a loopback server laid out
/// like it. Every download and hash URL must sit under this base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    base: url::Url,
}

impl Registry {
    /// The production source, `https://open-vsx.org/api/`. Output: the registry.
    pub fn open_vsx() -> Self {
        Self {
            base: url::Url::parse(OPEN_VSX).expect("the Open VSX base is a valid URL"),
        }
    }

    /// A registry at another base, a loopback server in tests. Input: the base ending in
    /// `/`. Output: the registry.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn at(base: &str) -> Self {
        assert!(base.ends_with('/'));
        Self {
            base: url::Url::parse(base).unwrap(),
        }
    }

    /// The metadata URL of one version, or of the newest for `latest`. Inputs: the id and
    /// the version. Output: the URL.
    fn metadata(&self, id: &str, version: &str) -> Result<url::Url, EngineError> {
        let (namespace, name) = split(id)?;
        let path = if version == LATEST {
            format!("{namespace}/{name}")
        } else {
            format!("{namespace}/{name}/{version}")
        };
        let url = self
            .base
            .join(&path)
            .map_err(|_| failure("ide_extension_invalid"))?;
        assert!(url.as_str().starts_with(self.base.as_str()));
        Ok(url)
    }

    /// Accepts a URL from Open VSX's metadata only when it sits under the registry base:
    /// same scheme, host and port, a path under the base path, no user, query or fragment.
    /// Input: the URL text. Output: the URL, or `ide_extension_source_refused`.
    fn inside(&self, text: &str) -> Result<url::Url, EngineError> {
        let refused = || failure("ide_extension_source_refused");
        let url = url::Url::parse(text).map_err(|_| refused())?;
        let same = url.scheme() == self.base.scheme()
            && url.host_str() == self.base.host_str()
            && url.port_or_known_default() == self.base.port_or_known_default()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path().starts_with(self.base.path())
            && !url
                .path()
                .split('/')
                .any(|part| part == ".." || part == ".");
        if !same {
            return Err(refused());
        }
        Ok(url)
    }
}

/// Whether an extension id is `publisher.name` in lowercase, each part 1 to 128 ASCII
/// letters, digits, hyphens or underscores. Input: the id. Output: the answer.
pub fn valid_id(id: &str) -> bool {
    id.bytes().all(|byte| !byte.is_ascii_uppercase()) && super::bridge::extension_id(id)
}

/// Whether a version is `latest` or 1 to 64 ASCII letters, digits, dots, hyphens or plus
/// signs that does not start with a dot. Input: the version. Output: the answer.
pub fn valid_version(version: &str) -> bool {
    super::bridge::extension_version(version) && !version.starts_with('.')
}

/// The setting the IDE server reads to refuse any other extension: `extensions.allowed`
/// with each approved extension at its exact pinned version and the built-in at its own.
/// An unreadable policy allows only the built-in. Input: the engine root. Output: the map.
pub fn allowed_setting(root: &Path) -> serde_json::Map<String, serde_json::Value> {
    let approved = load(root).map(|policy| policy.approved).unwrap_or_default();
    let mut allowed = serde_json::Map::new();
    for extension in approved {
        allowed.insert(extension.id, serde_json::json!([extension.version]));
    }
    allowed.insert(BUILT_IN.to_owned(), serde_json::json!([built_in_version()]));
    let mut setting = serde_json::Map::new();
    setting.insert(
        "extensions.allowed".to_owned(),
        serde_json::Value::Object(allowed),
    );
    setting
}

/// Records that the IDE asked to install or update an extension, for the owner to confirm
/// in the desktop app; nothing is approved here. An approved version, or `latest` of an
/// approved extension, needs no ask. Inputs: the engine root, the project, the id, the
/// version or `latest`, and the time. Output: approved or confirm, or a plain refusal.
pub fn ask(
    root: &Path,
    project: &ProjectId,
    id: &str,
    version: &str,
    now_ms: i64,
) -> Result<Asked, EngineError> {
    checked(id, version)?;
    allowed_past_ai_lock(id)?;
    let _writes = writes();
    let mut policy = load(root)?;
    let approved = policy
        .approved
        .iter()
        .any(|known| known.id == id && (known.version == version || version == LATEST));
    if approved {
        return Ok(Asked::Approved);
    }
    policy
        .asks
        .retain(|old| !(old.project == *project && old.id == id));
    policy.asks.push(Ask {
        project: project.clone(),
        id: id.to_owned(),
        version: version.to_owned(),
        asked_ms: now_ms,
    });
    prune(&mut policy, now_ms);
    save(root, &policy)?;
    Ok(Asked::Confirm)
}

/// Drops the IDE's ask for an extension in one project and dismisses that version as an
/// available update, so the IDE no longer shows it. Inputs: the engine root, the project,
/// the id and the version. Output: Ok, or a plain refusal.
pub fn dismiss(
    root: &Path,
    project: &ProjectId,
    id: &str,
    version: &str,
) -> Result<(), EngineError> {
    checked(id, version)?;
    let _writes = writes();
    let mut policy = load(root)?;
    policy
        .asks
        .retain(|old| !(old.project == *project && old.id == id));
    policy
        .updates
        .retain(|update| !(update.id == id && update.version == version));
    save(root, &policy)
}

/// The asks waiting for the owner's confirm in one project, oldest first. Inputs: the
/// engine root, the project and the time. Output: the asks, or a plain refusal.
pub fn asks(root: &Path, project: &ProjectId, now_ms: i64) -> Result<Vec<AskView>, EngineError> {
    let policy = load(root)?;
    let mut waiting: Vec<&Ask> = policy
        .asks
        .iter()
        .filter(|ask| ask.project == *project && fresh(ask, now_ms))
        .collect();
    waiting.sort_by_key(|ask| ask.asked_ms);
    Ok(waiting
        .into_iter()
        .map(|ask| AskView {
            id: ask.id.clone(),
            version: ask.version.clone(),
            current: current(&policy, &ask.id),
        })
        .collect())
}

/// What the IDE shows for one project: approved extensions, its waiting asks and the
/// available updates. Inputs: the engine root, the project and the time. Output: the
/// listing, or a plain refusal.
pub fn listing(root: &Path, project: &ProjectId, now_ms: i64) -> Result<Listing, EngineError> {
    let policy = load(root)?;
    Ok(Listing {
        approved: policy
            .approved
            .iter()
            .map(|known| Update {
                id: known.id.clone(),
                version: known.version.clone(),
            })
            .collect(),
        asks: asks(root, project, now_ms)?,
        updates: policy
            .updates
            .iter()
            .map(|update| AskView {
                id: update.id.clone(),
                version: update.version.clone(),
                current: current(&policy, &update.id),
            })
            .collect(),
    })
}

/// Drops one project's ask without approving it, the desktop confirm's Not now.
/// Inputs: the engine root, the project, the id and the version. Output: Ok, or a refusal.
pub fn decline(
    root: &Path,
    project: &ProjectId,
    id: &str,
    version: &str,
) -> Result<(), EngineError> {
    checked(id, version)?;
    let _writes = writes();
    let mut policy = load(root)?;
    policy
        .asks
        .retain(|old| !(old.project == *project && old.id == id && old.version == version));
    save(root, &policy)
}

/// The owner's approval from the desktop confirm of an ask the IDE made: reads the
/// version's metadata from the registry, downloads its VSIX and checks it against the
/// registry's sha256 and any pin of that version, then pins the exact version and sha256,
/// keeps the verified VSIX and clears the asks and update for it. Nothing is installed
/// here; each IDE server installs approved extensions when it starts. Inputs: the engine
/// root, the registry, the project, the id, the version or `latest`, and the time.
/// Output: the pin, or a plain refusal such as `ide_extension_ask_missing`,
/// `ide_extension_source_refused` or `ide_extension_hash_mismatch`.
pub fn approve(
    root: &Path,
    registry: &Registry,
    project: &ProjectId,
    id: &str,
    version: &str,
    now_ms: i64,
) -> Result<Approved, EngineError> {
    checked(id, version)?;
    allowed_past_ai_lock(id)?;
    {
        let policy = load(root)?;
        let asked = policy.asks.iter().any(|ask| {
            ask.project == *project && ask.id == id && ask.version == version && fresh(ask, now_ms)
        });
        if !asked {
            return Err(failure("ide_extension_ask_missing"));
        }
    }
    let fetched = fetch(root, registry, id, version)?;
    let _writes = writes();
    let mut policy = load(root)?;
    if policy.approved.iter().any(|known| {
        known.id == id && known.version == fetched.version && known.sha256 != fetched.sha256
    }) {
        return Err(failure("ide_extension_hash_mismatch"));
    }
    let replaced = policy
        .approved
        .iter()
        .find(|known| known.id == id && known.version != fetched.version)
        .map(|known| cached(root, &known.id, &known.version));
    policy.approved.retain(|known| known.id != id);
    if policy.approved.len() >= APPROVED_MAX {
        return Err(failure("ide_extensions_full"));
    }
    let pinned = Approved {
        id: id.to_owned(),
        version: fetched.version.clone(),
        sha256: fetched.sha256.clone(),
        approved_ms: now_ms,
    };
    policy.approved.push(pinned.clone());
    policy.asks.retain(|ask| ask.id != id);
    policy.updates.retain(|update| update.id != id);
    save(root, &policy)?;
    fetched.keep(&cached(root, id, &pinned.version))?;
    if let Some(old) = replaced {
        let _ = std::fs::remove_file(old);
    }
    assert!(policy.approved.len() <= APPROVED_MAX);
    Ok(pinned)
}

/// Prepares one IDE server's extensions before it starts: installs every approved
/// extension from its verified VSIX, uninstalls any other one but the built-in, then
/// starts the daily update check on its own thread when one is due. Inputs: the engine
/// root, the registry, the canonical root of a verified IDE build, the server's
/// extensions and data folders and the time. Output: Ok, or a plain refusal.
pub fn on_server_start(
    root: &Path,
    registry: &Registry,
    build: &Path,
    extensions: &Path,
    server_data: &Path,
    now_ms: i64,
) -> Result<(), EngineError> {
    install_approved(root, registry, build, extensions, server_data)?;
    if update_due(root, now_ms) {
        let (root, registry) = (root.to_path_buf(), registry.clone());
        let started = std::thread::Builder::new()
            .name("ide-extension-updates".into())
            .spawn(move || {
                if let Err(problem) = check_updates(&root, &registry, now_ms) {
                    eprintln!("ide_extension_update_check_failed code={}", problem.0);
                }
            });
        if started.is_err() {
            eprintln!("ide_extension_update_check_failed code=ide_thread_unavailable");
        }
    }
    Ok(())
}

/// Installs every approved extension that the server's extensions folder lacks, each from
/// a VSIX whose sha256 equals its pin, and uninstalls every installed extension that is
/// neither approved at that version nor the built-in. Inputs: the engine root, the
/// registry, the build root and the server's extensions and data folders. Output: Ok once
/// the folder holds exactly the approved versions, or a plain refusal.
pub fn install_approved(
    root: &Path,
    registry: &Registry,
    build: &Path,
    extensions: &Path,
    server_data: &Path,
) -> Result<(), EngineError> {
    let approved = load(root)?.approved;
    let server = Server {
        build,
        extensions,
        server_data,
    };
    let installed = server.list()?;
    for (id, version) in &installed {
        let kept = id == BUILT_IN
            || approved
                .iter()
                .any(|known| known.id == *id && known.version == *version);
        if !kept {
            server.run(&[OsStr::new("--uninstall-extension"), OsStr::new(id)])?;
        }
    }
    for known in &approved {
        let present = installed
            .iter()
            .any(|(id, version)| *id == known.id && *version == known.version);
        if present {
            continue;
        }
        let vsix = verified(root, registry, known)?;
        server.run(&[
            OsStr::new("--install-extension"),
            vsix.as_os_str(),
            OsStr::new("--force"),
        ])?;
    }
    let now = server.list()?;
    let exact = approved.iter().all(|known| {
        now.iter()
            .any(|(id, version)| *id == known.id && *version == known.version)
    }) && now.iter().all(|(id, version)| {
        id == BUILT_IN
            || approved
                .iter()
                .any(|known| known.id == *id && known.version == *version)
    });
    if !exact {
        return Err(failure("ide_extension_install_failed"));
    }
    Ok(())
}

/// Looks up the newest version of every approved extension, at most once a day, and
/// records each one newer than the pin as an available update; nothing is installed.
/// Inputs: the engine root, the registry and the time. Output: how many updates are shown.
pub fn check_updates(root: &Path, registry: &Registry, now_ms: i64) -> Result<usize, EngineError> {
    if !update_due(root, now_ms) {
        return Ok(load(root)?.updates.len());
    }
    let approved = {
        let _writes = writes();
        let mut policy = load(root)?;
        policy.checked_ms = Some(now_ms);
        save(root, &policy)?;
        policy.approved
    };
    let mut found = Vec::new();
    for known in &approved {
        match newest(registry, &known.id) {
            Ok(version) if version != known.version => found.push(Update {
                id: known.id.clone(),
                version,
            }),
            Ok(_) => {}
            Err(problem) => eprintln!(
                "ide_extension_update_unread id={} code={}",
                known.id, problem.0
            ),
        }
    }
    let _writes = writes();
    let mut policy = load(root)?;
    for update in found {
        let still = policy
            .approved
            .iter()
            .any(|known| known.id == update.id && known.version != update.version);
        if still {
            policy.updates.retain(|old| old.id != update.id);
            policy.updates.push(update);
        }
    }
    policy.updates.truncate(APPROVED_MAX);
    save(root, &policy)?;
    Ok(policy.updates.len())
}

fn update_due(root: &Path, now_ms: i64) -> bool {
    load(root).is_ok_and(|policy| {
        policy
            .checked_ms
            .is_none_or(|checked| now_ms.saturating_sub(checked) >= UPDATE_EVERY_MS)
    })
}

fn newest(registry: &Registry, id: &str) -> Result<String, EngineError> {
    let bytes = read(&registry.metadata(id, LATEST)?, METADATA_BYTES_MAX)?;
    let metadata: Metadata =
        serde_json::from_slice(&bytes).map_err(|_| failure("ide_extension_metadata_invalid"))?;
    if format!("{}.{}", metadata.namespace, metadata.name).to_ascii_lowercase() != id
        || !valid_version(&metadata.version)
        || metadata.version == LATEST
    {
        return Err(failure("ide_extension_metadata_invalid"));
    }
    Ok(metadata.version)
}

/// A VSIX downloaded and checked against the registry's sha256, held under a private
/// name until it is kept or dropped.
struct Fetched {
    version: String,
    sha256: String,
    part: PathBuf,
}

impl Fetched {
    /// Moves the checked VSIX to its cache name. Input: the cache path. Output: Ok.
    fn keep(&self, path: &Path) -> Result<(), EngineError> {
        std::fs::rename(&self.part, path).map_err(|_| failure("ide_extensions_unavailable"))
    }
}

impl Drop for Fetched {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.part);
    }
}

/// Reads a version's metadata, refuses files outside the registry, downloads the VSIX
/// and its sha256 and refuses bytes that do not match. Inputs: the engine root, the
/// registry, the id and the version or `latest`. Output: the checked VSIX.
fn fetch(
    root: &Path,
    registry: &Registry,
    id: &str,
    version: &str,
) -> Result<Fetched, EngineError> {
    let bytes = read(&registry.metadata(id, version)?, METADATA_BYTES_MAX)?;
    let metadata: Metadata =
        serde_json::from_slice(&bytes).map_err(|_| failure("ide_extension_metadata_invalid"))?;
    let named = format!("{}.{}", metadata.namespace, metadata.name).to_ascii_lowercase();
    if named != id
        || !valid_version(&metadata.version)
        || metadata.version == LATEST
        || (version != LATEST && metadata.version != version)
    {
        return Err(failure("ide_extension_metadata_invalid"));
    }
    let download = registry.inside(&metadata.files.download)?;
    let hash = registry.inside(
        metadata
            .files
            .sha256
            .as_deref()
            .ok_or_else(|| failure("ide_extension_hash_missing"))?,
    )?;
    let expected = hash_text(&read(&hash, HASH_BYTES_MAX)?)?;
    let cache = cache_folder(root)?;
    let part = cache.join(format!(".{id}-{}-{}.part", metadata.version, suffix()?));
    let fetched = Fetched {
        version: metadata.version,
        sha256: expected,
        part,
    };
    let actual = download_to(download.as_str(), &fetched.part)?;
    if actual != fetched.sha256 {
        return Err(failure("ide_extension_hash_mismatch"));
    }
    Ok(fetched)
}

/// The VSIX of an approved extension whose sha256 equals its pin: the cached file, or
/// that exact version downloaded again from the registry. A cached file that does not
/// match is removed. Inputs: the engine root, the registry and the pin. Output: the path,
/// or `ide_extension_hash_mismatch` when the registry no longer serves the pinned bytes.
fn verified(root: &Path, registry: &Registry, pin: &Approved) -> Result<PathBuf, EngineError> {
    let path = cached(root, &pin.id, &pin.version);
    if file_sha256(&path).is_ok_and(|found| found == pin.sha256) {
        return Ok(path);
    }
    let _ = std::fs::remove_file(&path);
    let fetched = fetch(root, registry, &pin.id, &pin.version)?;
    if fetched.sha256 != pin.sha256 || fetched.version != pin.version {
        return Err(failure("ide_extension_hash_mismatch"));
    }
    fetched.keep(&path)?;
    if !file_sha256(&path).is_ok_and(|found| found == pin.sha256) {
        let _ = std::fs::remove_file(&path);
        return Err(failure("ide_extension_hash_mismatch"));
    }
    Ok(path)
}

/// The IDE build's own command line for extension work, run against one server's
/// folders with a cleared environment.
struct Server<'a> {
    build: &'a Path,
    extensions: &'a Path,
    server_data: &'a Path,
}

impl Server<'_> {
    /// The installed extensions as `(id, version)`, ids lowercased. Output: the list.
    fn list(&self) -> Result<Vec<(String, String)>, EngineError> {
        let printed = self.run(&[
            OsStr::new("--list-extensions"),
            OsStr::new("--show-versions"),
        ])?;
        let text = String::from_utf8(printed).map_err(|_| failure("ide_extension_list_invalid"))?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let (id, version) = line
                    .trim()
                    .rsplit_once('@')
                    .ok_or_else(|| failure("ide_extension_list_invalid"))?;
                Ok((id.to_ascii_lowercase(), version.to_owned()))
            })
            .collect()
    }

    /// Runs one command of the build's server script and waits at most `CLI_TIME`.
    /// Input: the arguments after the folders. Output: what it printed, at most
    /// `LIST_BYTES_MAX` bytes, or `ide_extension_install_failed`.
    fn run(&self, args: &[&OsStr]) -> Result<Vec<u8>, EngineError> {
        let refused = || failure("ide_extension_install_failed");
        let mut command = Command::new(self.build.join("node"));
        command
            .arg(self.build.join(SERVER_MAIN))
            .arg("--extensions-dir")
            .arg(self.extensions)
            .arg("--server-data-dir")
            .arg(self.server_data)
            .arg("--accept-server-license-terms")
            .args(args)
            .current_dir(self.server_data)
            .env_clear()
            .env("HOME", self.server_data)
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = crate::spawn_gate::spawn(&mut command).map_err(|_| refused())?;
        let stdout = child.stdout.take().ok_or_else(refused)?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut source = stdout.take(LIST_BYTES_MAX + 1);
            let read = source.read_to_end(&mut bytes);
            let _ = std::io::copy(&mut source.into_inner(), &mut std::io::sink());
            read.map(|_| bytes)
        });
        let deadline = Instant::now() + CLI_TIME;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|_| refused())? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(refused());
            }
            std::thread::sleep(CLI_POLL);
        };
        let printed = reader
            .join()
            .map_err(|_| refused())?
            .map_err(|_| refused())?;
        if !status.success() || printed.len() as u64 > LIST_BYTES_MAX {
            return Err(refused());
        }
        Ok(printed)
    }
}

fn checked(id: &str, version: &str) -> Result<(), EngineError> {
    if !valid_id(id) || !valid_version(version) {
        return Err(failure("ide_extension_invalid"));
    }
    Ok(())
}

fn allowed_past_ai_lock(id: &str) -> Result<(), EngineError> {
    if id == AI_LOCKED {
        return Err(failure("ide_extension_ai_locked"));
    }
    Ok(())
}

fn split(id: &str) -> Result<(&str, &str), EngineError> {
    id.split_once('.')
        .filter(|_| valid_id(id))
        .ok_or_else(|| failure("ide_extension_invalid"))
}

fn built_in_version() -> &'static str {
    super::bridge::EXTENSION
        .strip_prefix(BUILT_IN)
        .and_then(|rest| rest.strip_prefix('-'))
        .expect("the built-in extension folder is its id, a hyphen and its version")
}

fn fresh(ask: &Ask, now_ms: i64) -> bool {
    now_ms.saturating_sub(ask.asked_ms) < ASK_TTL_MS
}

fn current(policy: &Policy, id: &str) -> Option<String> {
    policy
        .approved
        .iter()
        .find(|known| known.id == id)
        .map(|known| known.version.clone())
}

fn prune(policy: &mut Policy, now_ms: i64) {
    policy.asks.retain(|ask| fresh(ask, now_ms));
    policy
        .asks
        .sort_by_key(|ask| std::cmp::Reverse(ask.asked_ms));
    let mut per_project = std::collections::BTreeMap::<ProjectId, usize>::new();
    policy.asks.retain(|ask| {
        let count = per_project.entry(ask.project.clone()).or_default();
        *count += 1;
        *count <= ASKS_PER_PROJECT_MAX
    });
    policy.asks.truncate(ASKS_MAX);
    policy.asks.sort_by_key(|ask| ask.asked_ms);
    assert!(policy.asks.len() <= ASKS_MAX);
}

fn writes() -> MutexGuard<'static, ()> {
    WRITES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn folder(root: &Path) -> Result<PathBuf, EngineError> {
    let ide = root.join(IDE);
    let folder = ide.join(FOLDER);
    for path in [&ide, &folder] {
        super::bridge::private_folder(path).map_err(|_| failure("ide_extensions_unavailable"))?;
    }
    Ok(folder)
}

fn cache_folder(root: &Path) -> Result<PathBuf, EngineError> {
    let cache = folder(root)?.join(CACHE);
    super::bridge::private_folder(&cache).map_err(|_| failure("ide_extensions_unavailable"))?;
    Ok(cache)
}

fn cached(root: &Path, id: &str, version: &str) -> PathBuf {
    root.join(IDE)
        .join(FOLDER)
        .join(CACHE)
        .join(format!("{id}-{version}.vsix"))
}

fn load(root: &Path) -> Result<Policy, EngineError> {
    let path = folder(root)?.join(POLICY);
    let file = match open_plain(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Policy::default()),
        Err(_) => return Err(failure("ide_extensions_unavailable")),
    };
    let mut bytes = Vec::new();
    file.take(POLICY_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("ide_extensions_unavailable"))?;
    if bytes.len() as u64 > POLICY_BYTES_MAX {
        return Err(failure("ide_extensions_unavailable"));
    }
    let mut policy: Policy =
        serde_json::from_slice(&bytes).map_err(|_| failure("ide_extensions_unavailable"))?;
    let sound = policy.approved.len() <= APPROVED_MAX
        && policy.asks.len() <= ASKS_MAX
        && policy.updates.len() <= APPROVED_MAX
        && policy.approved.iter().all(|known| {
            valid_id(&known.id)
                && valid_version(&known.version)
                && known.version != LATEST
                && is_sha256(&known.sha256)
        })
        && policy
            .asks
            .iter()
            .all(|ask| valid_id(&ask.id) && valid_version(&ask.version))
        && policy
            .updates
            .iter()
            .all(|update| valid_id(&update.id) && valid_version(&update.version));
    if !sound {
        return Err(failure("ide_extensions_unavailable"));
    }
    policy.approved.retain(|known| known.id != AI_LOCKED);
    policy.asks.retain(|ask| ask.id != AI_LOCKED);
    policy.updates.retain(|update| update.id != AI_LOCKED);
    Ok(policy)
}

fn save(root: &Path, policy: &Policy) -> Result<(), EngineError> {
    assert!(policy.approved.len() <= APPROVED_MAX);
    let folder = folder(root)?;
    let bytes = serde_json::to_vec_pretty(policy).map_err(unavailable)?;
    if bytes.len() as u64 > POLICY_BYTES_MAX {
        return Err(failure("ide_extensions_full"));
    }
    let part = folder.join(format!(".{POLICY}-{}.part", suffix()?));
    let written = create_private(&part)
        .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()))
        .map_err(unavailable)
        .and_then(|()| std::fs::rename(&part, folder.join(POLICY)).map_err(unavailable));
    if written.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    written
}

fn unavailable<E>(_: E) -> EngineError {
    failure("ide_extensions_unavailable")
}

fn read(url: &url::Url, limit: u64) -> Result<Vec<u8>, EngineError> {
    let mut bytes = Vec::new();
    crate::update::download(url.as_str(), &mut bytes, limit, READ_TIME).map_err(fetch_failure)?;
    Ok(bytes)
}

fn download_to(url: &str, path: &Path) -> Result<String, EngineError> {
    let file = create_private(path).map_err(|_| failure("ide_extensions_unavailable"))?;
    let mut hashing = Hashing {
        file,
        hasher: Sha256::new(),
    };
    crate::update::download(url, &mut hashing, VSIX_BYTES_MAX, DOWNLOAD_TIME)
        .map_err(fetch_failure)?;
    hashing
        .file
        .sync_all()
        .map_err(|_| failure("ide_extensions_unavailable"))?;
    Ok(format!("{:x}", hashing.hasher.finalize()))
}

fn fetch_failure(error: crate::update::UpdateError) -> EngineError {
    failure(match error.code {
        "update_url_refused" => "ide_extension_source_refused",
        "update_too_large" => "ide_extension_too_large",
        "update_download_missing" => "ide_extension_not_found",
        _ => "ide_extension_download_failed",
    })
}

struct Hashing {
    file: std::fs::File,
    hasher: Sha256,
}

impl Write for Hashing {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn hash_text(bytes: &[u8]) -> Result<String, EngineError> {
    let text = std::str::from_utf8(bytes).map_err(|_| failure("ide_extension_hash_invalid"))?;
    let word = text
        .split_whitespace()
        .next()
        .map(str::to_ascii_lowercase)
        .filter(|word| is_sha256(word))
        .ok_or_else(|| failure("ide_extension_hash_invalid"))?;
    Ok(word)
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = open_plain(path)?.take(VSIX_BYTES_MAX + 1);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn suffix() -> Result<String, EngineError> {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).map_err(|_| failure("ide_entropy_unavailable"))?;
    Ok(super::bridge::hex(&bytes))
}

#[cfg(unix)]
fn open_plain(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a file"));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_plain(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
