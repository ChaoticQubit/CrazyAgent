use super::{PINNED, Pinned, folder};
use crate::app_plugins::Problem;
use crate::engine::EngineError;
use crate::semantic::ModelState;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

const CURL: &str = "/usr/bin/curl";
#[cfg(feature = "e2e-model-source")]
const NO_CURL: &str = "/dev/null";
const USER_AGENT: &str = "CrazyAgent";
const CONNECT_SECONDS: &str = "15";
const TRANSFER_SECONDS: &str = "3600";
const CHUNK_BYTES: usize = 1 << 16;
const CURL_TOO_LARGE: i32 = 63;
const PART_SUFFIX: &str = ".part";
const FIRST_URL: &str = "https://huggingface.co/";
const ROOTS_MAX: usize = 256;

static BY_ROOT: LazyLock<Mutex<HashMap<PathBuf, Downloads>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Where the local runtime's one-time download stands: not on this computer, downloading
/// with the bytes received so far, ready (both pinned files at their sizes), or failed
/// with its code until the next start or delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    NotDownloaded,
    Downloading { done_bytes: u64 },
    Ready,
    Failed(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    Over,
    Mismatch,
    Store,
    Read,
}

impl Fault {
    fn code(self) -> &'static str {
        match self {
            Fault::Over | Fault::Mismatch => "search_model_mismatch",
            Fault::Store => "search_model_store_unavailable",
            Fault::Read => "search_model_download_failed",
        }
    }
}

enum Run {
    Idle,
    Running,
    Failed(&'static str),
}

struct Control {
    run: Run,
    child: Option<Child>,
    cancelled: bool,
}

struct Shared {
    root: PathBuf,
    curl: PathBuf,
    pins: [Pinned; 2],
    control: Mutex<Control>,
    done: AtomicU64,
}

/// The local runtime's download, one per engine. A start needs the owner's confirm of the
/// exact size; the files come through the engine's own curl child, which Cancel kills;
/// each lands in `<root>/models/<runtime>/` only after its size and sha256 match its pin.
#[derive(Clone)]
pub struct Downloads {
    shared: Arc<Shared>,
}

fn lock(control: &Mutex<Control>) -> MutexGuard<'_, Control> {
    control.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Downloads {
    /// The download of the pinned runtime through the system curl, or through the test
    /// model source in an engine built with the `e2e-model-source` feature. Input: the
    /// engine root.
    pub fn new(root: PathBuf) -> Self {
        let (curl, pins) = source();
        Self::with_curl(root, curl, pins)
    }

    /// The download through a given curl and pins. Inputs: the engine root, the curl
    /// program and the tokenizer and matrix pins, in that order.
    pub(crate) fn with_curl(root: PathBuf, curl: PathBuf, pins: [Pinned; 2]) -> Self {
        assert!(
            pins.iter()
                .all(|pin| pin.bytes > 0 && pin.sha256.len() == 64)
        );
        Self {
            shared: Arc::new(Shared {
                root,
                curl,
                pins,
                control: Mutex::new(Control {
                    run: Run::Idle,
                    child: None,
                    cancelled: false,
                }),
                done: AtomicU64::new(0),
            }),
        }
    }

    fn total(&self) -> u64 {
        self.shared.pins.iter().map(|pin| pin.bytes).sum()
    }

    /// Where the download stands; ready means both files are there at their pinned sizes,
    /// and `LocalModel::open` re-hashes them. Output: the phase.
    pub fn phase(&self) -> Phase {
        let control = lock(&self.shared.control);
        match control.run {
            Run::Running => Phase::Downloading {
                done_bytes: self.shared.done.load(Ordering::Relaxed),
            },
            Run::Failed(code) => Phase::Failed(code),
            Run::Idle if self.present() => Phase::Ready,
            Run::Idle => Phase::NotDownloaded,
        }
    }

    fn present(&self) -> bool {
        let place = folder(&self.shared.root);
        self.shared.pins.iter().all(|pin| {
            fs::symlink_metadata(place.join(pin.name))
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == pin.bytes)
        })
    }

    /// The download as the Search settings show it. Output: the model state, with the
    /// failure's plain sentence.
    pub fn state(&self) -> ModelState {
        let size_bytes = self.total();
        match self.phase() {
            Phase::NotDownloaded => ModelState::NotDownloaded { size_bytes },
            Phase::Downloading { done_bytes } => ModelState::Downloading {
                done_bytes,
                size_bytes,
            },
            Phase::Ready => ModelState::Ready { size_bytes },
            Phase::Failed(code) => ModelState::Failed {
                problem: Problem::new(code, None, None),
                size_bytes,
            },
        }
    }

    /// Starts the download in the background once the owner confirmed its exact size; a
    /// start while one runs or with both files ready starts nothing. Input: the size the
    /// owner confirmed. Output: unit, `search_model_confirm` for any other size, or
    /// `search_model_download_failed` when no thread could start.
    pub fn start(&self, confirm_bytes: u64) -> Result<(), &'static str> {
        if confirm_bytes != self.total() {
            return Err("search_model_confirm");
        }
        let mut control = lock(&self.shared.control);
        let ready = matches!(control.run, Run::Idle) && self.present();
        if matches!(control.run, Run::Running) || ready {
            return Ok(());
        }
        control.run = Run::Running;
        control.cancelled = false;
        control.child = None;
        self.shared.done.store(0, Ordering::Relaxed);
        drop(control);
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("model-download".to_owned())
            .spawn(move || {
                let outcome = fetch_all(&shared);
                let mut control = lock(&shared.control);
                assert!(control.child.is_none(), "every child is waited for");
                control.run = match outcome {
                    Ok(()) => Run::Idle,
                    Err(_) if control.cancelled => Run::Idle,
                    Err(code) => Run::Failed(code),
                };
            });
        if spawned.is_err() {
            lock(&self.shared.control).run = Run::Failed("search_model_download_failed");
            return Err("search_model_download_failed");
        }
        Ok(())
    }

    /// Stops a running download: its curl child is killed and its part file deleted, and
    /// the download reads not downloaded, never failed. Does nothing when none runs.
    pub fn cancel(&self) {
        let mut control = lock(&self.shared.control);
        if !matches!(control.run, Run::Running) {
            return;
        }
        control.cancelled = true;
        if let Some(child) = control.child.as_mut() {
            let _ = child.kill();
        }
    }

    /// Deletes the downloaded files and any part file and clears a failure. Output: unit,
    /// `search_model_downloading` while a download runs, or `search_model_store_unavailable`.
    pub fn delete(&self) -> Result<(), &'static str> {
        let mut control = lock(&self.shared.control);
        if matches!(control.run, Run::Running) {
            return Err("search_model_downloading");
        }
        let place = folder(&self.shared.root);
        for pin in &self.shared.pins {
            for path in [place.join(pin.name), part_path(&place, pin)] {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(_) => return Err("search_model_store_unavailable"),
                }
            }
        }
        control.run = Run::Idle;
        Ok(())
    }
}

/// The variable a desktop end-to-end run sets to the folder its test model source lives in;
/// only an engine built with the `e2e-model-source` feature reads it.
#[cfg(feature = "e2e-model-source")]
pub const SOURCE_ENV: &str = "CRAZYAGENT_E2E_MODEL_SOURCE";

/// What `SOURCE_ENV` named, read once per engine: the test model source's curl and pins,
/// none when the variable is not set, or why the named folder cannot serve.
#[cfg(feature = "e2e-model-source")]
type Source = Result<Option<(PathBuf, [Pinned; 2])>, String>;

#[cfg(feature = "e2e-model-source")]
static SOURCE: LazyLock<Source> = LazyLock::new(|| {
    std::env::var_os(SOURCE_ENV)
        .map_or(Ok(None), |folder| source_pins(Path::new(&folder)).map(Some))
});

/// Whether this engine downloads from a test model source: true when `SOURCE_ENV` names a
/// usable one, false when it is not set, and why when the named folder cannot serve, so
/// the engine refuses to start rather than reach the network. Output: that answer.
#[cfg(feature = "e2e-model-source")]
pub fn e2e_source() -> Result<bool, String> {
    SOURCE.as_ref().map(Option::is_some).map_err(Clone::clone)
}

/// The curl and pins every download uses: the system curl and the shipped pins.
#[cfg(not(feature = "e2e-model-source"))]
fn source() -> (PathBuf, [Pinned; 2]) {
    (PathBuf::from(CURL), PINNED)
}

/// The curl and pins every download uses: the test model source's when `SOURCE_ENV` names
/// a usable one, the system curl and shipped pins when it is not set, and a curl that
/// cannot run when the named source is unusable, so no download reaches the network.
#[cfg(feature = "e2e-model-source")]
fn source() -> (PathBuf, [Pinned; 2]) {
    match &*SOURCE {
        Ok(Some((curl, pins))) => (curl.clone(), *pins),
        Ok(None) => (PathBuf::from(CURL), PINNED),
        Err(_) => (PathBuf::from(NO_CURL), PINNED),
    }
}

/// A test model source: a folder holding a stand-in `curl` and both pinned files. The pins
/// keep their names and URLs and take the files' own sizes and sha256, so a download from
/// it never reaches the network. Input: the folder. Output: its curl and the pins, or why
/// it cannot serve.
#[cfg(any(test, feature = "e2e-model-source"))]
pub(crate) fn source_pins(folder: &Path) -> Result<(PathBuf, [Pinned; 2]), String> {
    if !folder.is_absolute() {
        return Err(format!(
            "the test model source {} is not an absolute folder",
            folder.display()
        ));
    }
    let curl = folder.join("curl");
    if !fs::symlink_metadata(&curl).is_ok_and(|metadata| metadata.is_file()) {
        return Err(format!(
            "the test model source {} has no curl file",
            folder.display()
        ));
    }
    let mut pins = PINNED;
    for pin in &mut pins {
        let path = folder.join(pin.name);
        let (bytes, digest) = File::open(&path)
            .and_then(|mut file| {
                let mut hasher = Sha256::new();
                let bytes = std::io::copy(&mut file, &mut hasher)?;
                Ok((bytes, hasher.finalize()))
            })
            .map_err(|error| format!("{} cannot be read: {error}", path.display()))?;
        if bytes == 0 {
            return Err(format!("{} is empty", path.display()));
        }
        pin.bytes = bytes;
        pin.sha256 = String::leak(format!("{digest:x}"));
    }
    Ok((curl, pins))
}

fn for_root(root: &Path) -> Downloads {
    let mut by_root = BY_ROOT.lock().unwrap_or_else(PoisonError::into_inner);
    let downloads = by_root
        .entry(root.to_path_buf())
        .or_insert_with(|| Downloads::new(root.to_path_buf()))
        .clone();
    assert!(by_root.len() <= ROOTS_MAX, "one download per engine root");
    downloads
}

/// The local model's download as the Search settings show it; cheap, nothing is hashed.
/// Input: the engine root. Output: the model state.
pub fn state(root: &Path) -> ModelState {
    for_root(root).state()
}

/// Starts the one-time download for the owner's Download, once the request carries the
/// exact size the owner confirmed (leader QC); a start while one runs or with the model
/// ready starts nothing. Inputs: the engine root and the confirmed size. Output: unit or
/// `search_model_confirm` / `search_model_download_failed`.
pub fn start_download(root: &Path, confirm_bytes: u64) -> Result<(), EngineError> {
    for_root(root)
        .start(confirm_bytes)
        .map_err(|code| EngineError(code.to_owned()))
}

/// Cancels a running download for the owner's Cancel. Input: the engine root.
pub fn cancel_download(root: &Path) {
    for_root(root).cancel();
}

/// Deletes the downloaded model for the owner's Delete. Input: the engine root. Output:
/// unit, `search_model_downloading` or `search_model_store_unavailable`.
pub fn delete(root: &Path) -> Result<(), EngineError> {
    for_root(root)
        .delete()
        .map_err(|code| EngineError(code.to_owned()))
}

fn part_path(place: &Path, pin: &Pinned) -> PathBuf {
    place.join(format!("{}{PART_SUFFIX}", pin.name))
}

/// Fetches both pinned files in order into the model folder, after clearing part files a
/// crash left. Input: the shared download. Output: unit or the first file's code.
fn fetch_all(shared: &Shared) -> Result<(), &'static str> {
    let place = folder(&shared.root);
    private_folder(&place)?;
    for pin in &shared.pins {
        match fs::remove_file(part_path(&place, pin)) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err("search_model_store_unavailable"),
        }
    }
    for pin in &shared.pins {
        fetch_one(shared, pin, &place)?;
    }
    Ok(())
}

/// Fetches one pinned file with a curl child that Cancel can kill: the URL goes on its
/// stdin, the body streams into a private part file, and the part is renamed into place
/// only when curl succeeded and the size and sha256 match. Inputs: the shared download, the
/// pin and the model folder. Output: unit or the code.
fn fetch_one(shared: &Shared, pin: &Pinned, place: &Path) -> Result<(), &'static str> {
    let part = part_path(place, pin);
    let (mut command, config) = curl_command(&shared.curl, pin);
    let (mut stdin, stdout) = {
        let mut control = lock(&shared.control);
        if control.cancelled {
            return Err("search_model_download_failed");
        }
        let mut child =
            crate::spawn_gate::spawn(&mut command).map_err(|_| "search_model_download_failed")?;
        let stdin = child.stdin.take().expect("curl stdin is piped");
        let stdout = child.stdout.take().expect("curl stdout is piped");
        control.child = Some(child);
        (stdin, stdout)
    };
    let written = stdin.write_all(&config);
    drop(stdin);
    let received = receive_part(stdout, pin, &part, &shared.done);
    let child = lock(&shared.control).child.take();
    let status = child.and_then(|mut child| {
        if received.is_err() {
            let _ = child.kill();
        }
        child.wait().ok()
    });
    let succeeded = status.is_some_and(|status| status.success());
    match received {
        Ok(()) if succeeded && written.is_ok() => land(&part, &place.join(pin.name)),
        Ok(()) => {
            let _ = fs::remove_file(&part);
            Err("search_model_download_failed")
        }
        Err(fault @ (Fault::Over | Fault::Store)) => Err(fault.code()),
        Err(_) if status.and_then(|status| status.code()) == Some(CURL_TOO_LARGE) => {
            Err("search_model_mismatch")
        }
        Err(fault) if succeeded => Err(fault.code()),
        Err(_) => Err("search_model_download_failed"),
    }
}

/// The curl child for one pinned file: the URL only in the config on stdin, no `.curlrc`,
/// no header, cookie or credential, https only, one redirect (to the signed CDN URL),
/// HTTP errors fail, and the body capped at the pinned size. Inputs: the curl program and
/// the pin. Output: the command, whose arguments and environment hold no URL, and the
/// config for its stdin.
pub(crate) fn curl_command(curl: &Path, pin: &Pinned) -> (Command, Vec<u8>) {
    assert!(
        pin.url.starts_with(FIRST_URL),
        "the first URL is huggingface.co"
    );
    assert!(
        pin.url
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\'),
        "a pinned URL needs no quoting"
    );
    let mut command = Command::new(curl);
    command
        .arg("-q")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--globoff",
            "--location",
        ])
        .args(["--max-redirs", "1"])
        .args(["--proto", "=https"])
        .args(["--proto-redir", "=https"])
        .args(["--connect-timeout", CONNECT_SECONDS])
        .args(["--max-time", TRANSFER_SECONDS])
        .args(["--max-filesize", &pin.bytes.to_string()])
        .args(["--config", "-"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let config = format!("url = \"{}\"\nuser-agent = \"{USER_AGENT}\"\n", pin.url);
    (command, config.into_bytes())
}

/// Streams a body into a new private part file (0600, never through a link), counting the
/// bytes into the progress, and keeps it only when it is exactly the pinned size with the
/// pinned sha256; on any failure the part file is deleted. Inputs: the body, the pin, the
/// part path and the progress counter. Output: unit, `search_model_mismatch`,
/// `search_model_store_unavailable` or `search_model_download_failed`.
#[cfg(test)]
pub(crate) fn receive(
    source: impl Read,
    pin: &Pinned,
    part: &Path,
    done: &AtomicU64,
) -> Result<(), &'static str> {
    receive_part(source, pin, part, done).map_err(Fault::code)
}

fn receive_part(
    mut source: impl Read,
    pin: &Pinned,
    part: &Path,
    done: &AtomicU64,
) -> Result<(), Fault> {
    let file = create_private(part).map_err(|_| Fault::Store)?;
    let outcome = stream(&mut source, &file, pin, done)
        .and_then(|()| file.sync_all().map_err(|_| Fault::Store));
    if outcome.is_err() {
        let _ = fs::remove_file(part);
    }
    outcome
}

fn stream(
    source: &mut impl Read,
    mut file: &File,
    pin: &Pinned,
    done: &AtomicU64,
) -> Result<(), Fault> {
    let mut hasher = Sha256::new();
    let mut chunk = vec![0; CHUNK_BYTES];
    let mut seen: u64 = 0;
    loop {
        let read = match source.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Err(Fault::Read),
        };
        seen += read as u64;
        if seen > pin.bytes {
            return Err(Fault::Over);
        }
        hasher.update(&chunk[..read]);
        file.write_all(&chunk[..read]).map_err(|_| Fault::Store)?;
        done.fetch_add(read as u64, Ordering::Relaxed);
    }
    if seen != pin.bytes || format!("{:x}", hasher.finalize()) != pin.sha256 {
        return Err(Fault::Mismatch);
    }
    Ok(())
}

/// Renames a checked part file into place and syncs the folder. Inputs: the part and the
/// final path. Output: unit or `search_model_store_unavailable`.
fn land(part: &Path, last: &Path) -> Result<(), &'static str> {
    if fs::rename(part, last).is_err() {
        let _ = fs::remove_file(part);
        return Err("search_model_store_unavailable");
    }
    if let Some(parent) = last.parent() {
        let _ = File::open(parent).and_then(|folder| folder.sync_all());
    }
    Ok(())
}

/// Creates the model folder and its parents owner-only (0700), and accepts it only as a
/// real folder, never a link. Input: the folder. Output: unit or `search_model_store_unavailable`.
fn private_folder(place: &Path) -> Result<(), &'static str> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(place)
        .map_err(|_| "search_model_store_unavailable")?;
    let metadata = fs::symlink_metadata(place).map_err(|_| "search_model_store_unavailable")?;
    if !metadata.file_type().is_dir() {
        return Err("search_model_store_unavailable");
    }
    Ok(())
}

/// Creates a new file owner-only (0600 on Unix); an existing file or a link there is an
/// error. Input: the path. Output: the file.
fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
