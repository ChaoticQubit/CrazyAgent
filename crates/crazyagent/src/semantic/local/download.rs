use super::{PINNED, Pinned, folder};
use crate::app_plugins::Problem;
use crate::semantic::ModelState;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

const CURL: &str = "/usr/bin/curl";
const USER_AGENT: &str = "CrazyAgent";
const CONNECT_SECONDS: &str = "15";
const TRANSFER_SECONDS: &str = "3600";
const CHUNK_BYTES: usize = 1 << 16;
const CURL_TOO_LARGE: i32 = 63;
const PART_SUFFIX: &str = ".part";
const FIRST_URL: &str = "https://huggingface.co/";

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
pub struct Downloads {
    shared: Arc<Shared>,
}

fn lock(control: &Mutex<Control>) -> MutexGuard<'_, Control> {
    control.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Downloads {
    /// The download of the pinned runtime through the system curl. Input: the engine root.
    pub fn new(root: PathBuf) -> Self {
        Self::with_curl(root, PathBuf::from(CURL), PINNED)
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

    /// Starts the download in the background once the owner confirmed its exact size.
    /// Input: the size the owner confirmed. Output: unit, `search_model_confirm` for any
    /// other size, `search_model_busy` while one runs, or `search_model_unreachable` when
    /// no thread could start.
    pub fn start(&self, confirm_bytes: u64) -> Result<(), &'static str> {
        if confirm_bytes != self.total() {
            return Err("search_model_confirm");
        }
        let mut control = lock(&self.shared.control);
        if matches!(control.run, Run::Running) {
            return Err("search_model_busy");
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
            lock(&self.shared.control).run = Run::Failed("search_model_unreachable");
            return Err("search_model_unreachable");
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
    /// `search_model_busy` while a download runs, or `search_model_write`.
    pub fn delete(&self) -> Result<(), &'static str> {
        let mut control = lock(&self.shared.control);
        if matches!(control.run, Run::Running) {
            return Err("search_model_busy");
        }
        let place = folder(&self.shared.root);
        for pin in &self.shared.pins {
            for path in [place.join(pin.name), part_path(&place, pin)] {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(_) => return Err("search_model_write"),
                }
            }
        }
        control.run = Run::Idle;
        Ok(())
    }
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
            Err(_) => return Err("search_model_write"),
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
            return Err("search_model_unreachable");
        }
        let mut child =
            crate::spawn_gate::spawn(&mut command).map_err(|_| "search_model_unreachable")?;
        let stdin = child.stdin.take().expect("curl stdin is piped");
        let stdout = child.stdout.take().expect("curl stdout is piped");
        control.child = Some(child);
        (stdin, stdout)
    };
    let written = stdin.write_all(&config);
    drop(stdin);
    let received = receive(stdout, pin, &part, &shared.done);
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
            Err("search_model_unreachable")
        }
        Err(_) if status.and_then(|status| status.code()) == Some(CURL_TOO_LARGE) => {
            Err("search_model_too_large")
        }
        Err(code) if code == "search_model_too_large" || succeeded => Err(code),
        Err(_) => Err("search_model_unreachable"),
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
/// part path and the progress counter. Output: unit, `search_model_too_large`,
/// `search_model_hash`, `search_model_write` or `search_model_unreachable`.
pub(crate) fn receive(
    mut source: impl Read,
    pin: &Pinned,
    part: &Path,
    done: &AtomicU64,
) -> Result<(), &'static str> {
    let file = create_private(part).map_err(|_| "search_model_write")?;
    let outcome = stream(&mut source, &file, pin, done)
        .and_then(|()| file.sync_all().map_err(|_| "search_model_write"));
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
) -> Result<(), &'static str> {
    let mut hasher = Sha256::new();
    let mut chunk = vec![0; CHUNK_BYTES];
    let mut seen: u64 = 0;
    loop {
        let read = match source.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Err("search_model_unreachable"),
        };
        seen += read as u64;
        if seen > pin.bytes {
            return Err("search_model_too_large");
        }
        hasher.update(&chunk[..read]);
        file.write_all(&chunk[..read])
            .map_err(|_| "search_model_write")?;
        done.fetch_add(read as u64, Ordering::Relaxed);
    }
    if seen != pin.bytes || format!("{:x}", hasher.finalize()) != pin.sha256 {
        return Err("search_model_hash");
    }
    Ok(())
}

/// Renames a checked part file into place and syncs the folder. Inputs: the part and the
/// final path. Output: unit or `search_model_write`.
fn land(part: &Path, last: &Path) -> Result<(), &'static str> {
    if fs::rename(part, last).is_err() {
        let _ = fs::remove_file(part);
        return Err("search_model_write");
    }
    if let Some(parent) = last.parent() {
        let _ = File::open(parent).and_then(|folder| folder.sync_all());
    }
    Ok(())
}

/// Creates the model folder and its parents owner-only (0700), and accepts it only as a
/// real folder, never a link. Input: the folder. Output: unit or `search_model_write`.
fn private_folder(place: &Path) -> Result<(), &'static str> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(place).map_err(|_| "search_model_write")?;
    let metadata = fs::symlink_metadata(place).map_err(|_| "search_model_write")?;
    if !metadata.file_type().is_dir() {
        return Err("search_model_write");
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
