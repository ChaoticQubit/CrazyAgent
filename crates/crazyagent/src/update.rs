use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env::consts::EXE_SUFFIX;
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Largest release archive `crazyagent update` downloads.
pub const DOWNLOAD_BYTES_MAX: u64 = 200 * 1024 * 1024;
/// Largest release manifest `crazyagent update` reads.
pub const MANIFEST_BYTES_MAX: u64 = 1024 * 1024;
/// Longest one download may take.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

const CONNECT_TIMEOUT_SECONDS: u64 = 20;
const CHANNEL_FILE_BYTES_MAX: u64 = 4096;
const CHUNK_BYTES: usize = 64 * 1024;
const OUTPUT_BYTES_MAX: u64 = 16 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const UNPACK_TIMEOUT: Duration = Duration::from_secs(120);
const SERVICE_TIMEOUT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);
const PIPE_GRACE: Duration = Duration::from_secs(2);
const KEY_PLACEHOLDER: &str = "REPLACE_WITH_LEADER_PUBKEY";
const DESKTOP_CONFIG: &str = include_str!("../../../apps/desktop/src-tauri/tauri.conf.json");
const BINARIES: [&str; 4] = [
    "crazyagent",
    "crazyagent-rcli",
    "crazyagent-runner",
    "crazyagent-landlock",
];
const USAGE: &str = "usage: crazyagent update [--check] [--channel stable|dev]";

/// Release channel: stable is the latest non-prerelease GitHub Release, dev the rolling
/// `dev` prerelease rebuilt on every push to master.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Dev,
}

impl Channel {
    /// Reads a channel word. Input: "stable" or "dev". Output: the channel, or None.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "stable" => Some(Self::Stable),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }

    /// The channel's word as update.toml and the desktop app spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Dev => "dev",
        }
    }

    /// Where the desktop app's Tauri update manifest for this channel is published.
    pub fn desktop_manifest_url(self) -> &'static str {
        match self {
            Self::Stable => {
                "https://github.com/ChaoticQubit/CrazyAgent/releases/latest/download/latest.json"
            }
            Self::Dev => {
                "https://github.com/ChaoticQubit/CrazyAgent/releases/download/dev/latest.json"
            }
        }
    }

    /// Where the command line update manifest for this channel is published.
    pub fn cli_manifest_url(self) -> &'static str {
        match self {
            Self::Stable => {
                "https://github.com/ChaoticQubit/CrazyAgent/releases/latest/download/cli-latest.json"
            }
            Self::Dev => {
                "https://github.com/ChaoticQubit/CrazyAgent/releases/download/dev/cli-latest.json"
            }
        }
    }
}

/// Why an update step failed: a stable code for the desktop app and one plain sentence for
/// the owner.
#[derive(Debug)]
pub struct UpdateError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UpdateError {}

fn fail(code: &'static str, message: impl Into<String>) -> UpdateError {
    UpdateError {
        code,
        message: message.into(),
    }
}

fn io_failure(what: &str, error: std::io::Error) -> UpdateError {
    fail("update_io", format!("{what}: {error}"))
}

/// The engine data folder from the process environment; see [`data_root_from`].
pub fn data_root() -> Option<PathBuf> {
    data_root_from(|name| std::env::var_os(name))
}

/// The engine data folder per the release contract: `CRAZYAGENT_HOME` when absolute, else
/// `~/Library/Application Support/CrazyAgent/engine` on macOS,
/// `${XDG_DATA_HOME:-~/.local/share}/crazyagent/engine` on Linux and
/// `%LOCALAPPDATA%\CrazyAgent\engine` on Windows. Input: an environment lookup.
/// Output: the folder, or None without a usable absolute base.
pub fn data_root_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| {
        var(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(home) = absolute("CRAZYAGENT_HOME") {
        return Some(home);
    }
    if cfg!(target_os = "windows") {
        return absolute("LOCALAPPDATA").map(|base| base.join("CrazyAgent").join("engine"));
    }
    let home = absolute("HOME");
    if cfg!(target_os = "macos") {
        return home.map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("CrazyAgent")
                .join("engine")
        });
    }
    absolute("XDG_DATA_HOME")
        .or_else(|| home.map(|home| home.join(".local").join("share")))
        .map(|base| base.join("crazyagent").join("engine"))
}

fn invalid_channel(path: &Path) -> UpdateError {
    fail(
        "update_channel_invalid",
        format!(
            "{} must hold channel = \"stable\" or channel = \"dev\".",
            path.display()
        ),
    )
}

/// Reads the saved update channel from `<root>/update.toml`.
/// Input: the data folder. Output: the channel, stable when the file or key is missing,
/// `update_channel_invalid` for an unreadable, oversized or unknown value.
pub fn read_channel(root: &Path) -> Result<Channel, UpdateError> {
    #[derive(Deserialize)]
    struct UpdateFile {
        channel: Option<String>,
    }
    let path = root.join("update.toml");
    let metadata = match std::fs::metadata(&path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Channel::Stable),
        Err(_) => return Err(invalid_channel(&path)),
        Ok(metadata) => metadata,
    };
    if !metadata.is_file() || metadata.len() > CHANNEL_FILE_BYTES_MAX {
        return Err(invalid_channel(&path));
    }
    let text = std::fs::read_to_string(&path).map_err(|_| invalid_channel(&path))?;
    let file: UpdateFile = toml::from_str(&text).map_err(|_| invalid_channel(&path))?;
    match file.channel {
        None => Ok(Channel::Stable),
        Some(word) => Channel::parse(&word).ok_or_else(|| invalid_channel(&path)),
    }
}

/// Saves the update channel to `<root>/update.toml` (owner-only on Unix), replacing the
/// file atomically. Inputs: the data folder and the channel. Output: Ok, or `update_io`.
pub fn save_channel(root: &Path, channel: Channel) -> Result<(), UpdateError> {
    let failed = |error| io_failure("Can't save the update channel", error);
    std::fs::create_dir_all(root).map_err(failed)?;
    let temporary = root.join(".update.toml.new");
    if let Err(error) = std::fs::remove_file(&temporary)
        && error.kind() != ErrorKind::NotFound
    {
        return Err(failed(error));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(failed)?;
    file.write_all(format!("channel = \"{}\"\n", channel.as_str()).as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(failed)?;
    std::fs::rename(&temporary, root.join("update.toml")).map_err(failed)
}

/// A release manifest (`cli-latest.json`): the version and one archive per Rust target.
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub platforms: BTreeMap<String, Asset>,
}

/// One platform's release archive: where it is, its SHA-256 in hex and its Tauri-format
/// minisign signature (base64 of the `.sig` text).
#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    pub signature: String,
}

fn version(text: &str) -> Result<semver::Version, UpdateError> {
    semver::Version::parse(text.trim_start_matches('v')).map_err(|_| {
        fail(
            "update_manifest_invalid",
            format!("{text} is not a release version."),
        )
    })
}

/// Parses a release manifest. Input: its bytes. Output: the manifest, or
/// `update_manifest_invalid` when it is not JSON of that shape or its version is not semver.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, UpdateError> {
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(|error| {
        fail(
            "update_manifest_invalid",
            format!("The release manifest is not valid: {error}"),
        )
    })?;
    version(&manifest.version)?;
    Ok(manifest)
}

/// The Rust target this build's release archive is published under. Output: the triple, or
/// None on a platform with no release build.
pub fn target() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "aarch64",
        target_env = "gnu"
    )) {
        Some("aarch64-unknown-linux-gnu")
    } else if cfg!(all(
        target_os = "windows",
        target_arch = "x86_64",
        target_env = "msvc"
    )) {
        Some("x86_64-pc-windows-msvc")
    } else {
        None
    }
}

/// Whether a published version is newer than the running one, as semver (dev builds sort
/// below their stable). Inputs: both versions. Output: the answer or
/// `update_manifest_invalid`.
pub fn is_newer(available: &str, current: &str) -> Result<bool, UpdateError> {
    Ok(version(available)? > version(current)?)
}

fn allowed_url(text: &str) -> Result<url::Url, UpdateError> {
    let refused = || {
        fail(
            "update_url_refused",
            format!("CrazyAgent downloads updates only over https: {text}"),
        )
    };
    let url = url::Url::parse(text).map_err(|_| refused())?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        _ => Err(refused()),
    }
}

#[cfg(windows)]
fn system_tool(name: &str) -> PathBuf {
    let executable = format!("{name}.exe");
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join(&executable))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(executable))
}

#[cfg(not(windows))]
fn system_tool(name: &str) -> PathBuf {
    let path = Path::new("/usr/bin").join(name);
    if path.is_file() {
        path
    } else {
        PathBuf::from(name)
    }
}

fn last_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    if let Some(mut pipe) = pipe {
        let _ = std::thread::Builder::new()
            .name("update-pipe".to_owned())
            .spawn(move || {
                let mut buffer = vec![0_u8; 4096];
                let mut sent: u64 = 0;
                loop {
                    match pipe.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) if sent < OUTPUT_BYTES_MAX => {
                            sent = sent.saturating_add(read as u64);
                            let _ = sender.send(buffer[..read].to_vec());
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            });
    }
    receiver
}

fn gathered(receiver: &Receiver<Vec<u8>>, deadline: Instant) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Ok(chunk) = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        bytes.extend_from_slice(&chunk);
    }
    bytes.truncate(OUTPUT_BYTES_MAX as usize);
    bytes
}

fn too_large(limit: u64) -> UpdateError {
    fail(
        "update_too_large",
        format!("The download is larger than {limit} bytes, so it was stopped."),
    )
}

fn copy_bounded(
    child: &mut std::process::Child,
    out: &mut impl Write,
    limit: u64,
) -> Result<u64, UpdateError> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| fail("update_download_failed", "curl has no output pipe."))?;
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    let mut total: u64 = 0;
    loop {
        let read = match stdout.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(fail(
                    "update_download_failed",
                    format!("Reading the download failed: {error}"),
                ));
            }
        };
        if read == 0 {
            return Ok(total);
        }
        total = total.saturating_add(read as u64);
        if total > limit {
            return Err(too_large(limit));
        }
        out.write_all(&buffer[..read])
            .map_err(|error| io_failure("Can't save the download", error))?;
    }
}

/// Downloads one URL with the system curl, which honors the system proxy and trust store.
/// Only https, or http to this computer's loopback address, is fetched; redirects stay on
/// https. Inputs: the URL, where the bytes go, the byte cap and the time limit.
/// Output: the byte count, `update_url_refused`, `update_too_large` (nothing past the cap
/// is written) or `update_download_failed` with curl's reason.
pub fn download(
    url: &str,
    out: &mut impl Write,
    limit: u64,
    timeout: Duration,
) -> Result<u64, UpdateError> {
    let url = allowed_url(url)?;
    let seconds = timeout.as_secs().max(1);
    let mut command = Command::new(system_tool("curl"));
    command
        .arg("-q")
        .args(["--silent", "--show-error", "--fail", "--location"])
        .args(["--max-redirs", "5", "--proto-redir", "=https", "--proto"])
        .arg(if url.scheme() == "https" {
            "=https"
        } else {
            "=http"
        })
        .arg("--connect-timeout")
        .arg(CONNECT_TIMEOUT_SECONDS.min(seconds).to_string())
        .arg("--max-time")
        .arg(seconds.to_string())
        .arg("--max-filesize")
        .arg(limit.to_string())
        .arg("--user-agent")
        .arg(concat!("crazyagent/", env!("CARGO_PKG_VERSION")))
        .arg("--url")
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(|error| {
        fail(
            "update_download_failed",
            format!("Can't start curl: {error}"),
        )
    })?;
    let stderr = drain(child.stderr.take());
    let copied = copy_bounded(&mut child, out, limit);
    if copied.is_err() {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|error| io_failure("curl did not finish", error))?;
    let stderr = gathered(&stderr, Instant::now() + PIPE_GRACE);
    let total = copied?;
    if status.code() == Some(63) {
        return Err(too_large(limit));
    }
    if !status.success() {
        let reason = last_line(&stderr);
        return Err(fail(
            "update_download_failed",
            if reason.is_empty() {
                format!("The download failed: curl exited with {status}.")
            } else {
                format!("The download failed: {reason}")
            },
        ));
    }
    Ok(total)
}

fn bad_signature() -> UpdateError {
    fail(
        "update_signature_invalid",
        "The download's signature does not match CrazyAgent's release key, so it was not installed.",
    )
}

fn key_missing() -> UpdateError {
    fail(
        "update_key_missing",
        "This build has no release key, so it can't verify updates.",
    )
}

fn decode_text(base64: &str) -> Option<String> {
    STANDARD
        .decode(base64.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// Checks a downloaded archive against its SHA-256 and its minisign signature, then checks
/// that the signature was made for the announced version, so an old signed archive can't be
/// passed off as a new release. Inputs: the file, the hex SHA-256, the Tauri-format signature
/// and public key, and the manifest version. Output: Ok, or `update_checksum_mismatch`,
/// `update_signature_invalid`, `update_signature_unversioned`,
/// `update_signed_version_mismatch` or `update_key_missing`.
pub fn verify_file(
    path: &Path,
    sha256: &str,
    signature: &str,
    public_key: &str,
    version: &str,
) -> Result<(), UpdateError> {
    if public_key.trim().is_empty() || public_key == KEY_PLACEHOLDER {
        return Err(key_missing());
    }
    let key = decode_text(public_key)
        .and_then(|text| minisign_verify::PublicKey::decode(&text).ok())
        .ok_or_else(|| {
            fail(
                "update_key_invalid",
                "This build's release key can't be read, so it can't verify updates.",
            )
        })?;
    let signature = decode_text(signature)
        .and_then(|text| minisign_verify::Signature::decode(&text).ok())
        .ok_or_else(bad_signature)?;
    let mut verifier = key.verify_stream(&signature).map_err(|_| bad_signature())?;
    let mut hasher = Sha256::new();
    let mut file =
        std::fs::File::open(path).map_err(|error| io_failure("Can't read the download", error))?;
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_failure("Can't read the download", error)),
        };
        hasher.update(&buffer[..read]);
        verifier.update(&buffer[..read]);
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(sha256.trim()) {
        return Err(fail(
            "update_checksum_mismatch",
            "The download's SHA-256 does not match the release, so it was not installed.",
        ));
    }
    verifier.finalize().map_err(|_| bad_signature())?;
    let signed = signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.strip_prefix("version:"))
        .ok_or_else(|| {
            fail(
                "update_signature_unversioned",
                "The download's signature names no version, so it was not installed.",
            )
        })?;
    let announced = self::version(version)?;
    if semver::Version::parse(signed.trim_start_matches('v')).ok() != Some(announced) {
        return Err(fail(
            "update_signed_version_mismatch",
            format!("The download was signed as {signed}, not {version}, so it was not installed."),
        ));
    }
    Ok(())
}

/// Reads the update public key from a Tauri config. Input: the config JSON.
/// Output: `plugins.updater.pubkey`, or `update_key_missing` when it is absent, empty or
/// still the placeholder.
pub fn key_from_config(text: &str) -> Result<String, UpdateError> {
    let config: serde_json::Value = serde_json::from_str(text).map_err(|_| key_missing())?;
    match config
        .pointer("/plugins/updater/pubkey")
        .and_then(serde_json::Value::as_str)
    {
        Some(key) if !key.trim().is_empty() && key != KEY_PLACEHOLDER => Ok(key.to_owned()),
        _ => Err(key_missing()),
    }
}

/// The update public key compiled into this build from the desktop app's tauri.conf.json, so
/// the desktop updater and `crazyagent update` trust one key. Output: the key, or
/// `update_key_missing` while the config holds the placeholder.
pub fn public_key() -> Result<String, UpdateError> {
    key_from_config(DESKTOP_CONFIG)
}

/// Whether an executable belongs to an installed desktop app, which replaces it through its
/// own updater: inside a macOS `.app`, a mounted AppImage, or a Windows install folder with
/// the app's `uninstall.exe`. Input: the canonical executable path. Output: the answer.
pub fn inside_app_bundle(executable: &Path) -> bool {
    let bundled = executable.ancestors().skip(1).any(|folder| {
        let name = folder
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        name.ends_with(".app") || name.starts_with(".mount_")
    });
    bundled
        || executable
            .parent()
            .is_some_and(|folder| folder.join("uninstall.exe").is_file())
}

fn regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn run_bounded(
    command: &mut Command,
    timeout: Duration,
    code: &'static str,
) -> Result<Output, UpdateError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(command)
        .map_err(|error| fail(code, format!("Can't start {command:?}: {error}")))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail(
                    code,
                    format!("{command:?} did not finish within {} s.", timeout.as_secs()),
                ));
            }
            Err(error) => return Err(fail(code, format!("{command:?} failed: {error}"))),
        }
    };
    let deadline = Instant::now() + PIPE_GRACE;
    Ok(Output {
        status,
        stdout: gathered(&stdout, deadline),
        stderr: gathered(&stderr, deadline),
    })
}

/// Unpacks a verified release archive with the system tar (which also reads `.zip` on
/// Windows) into a new folder. Inputs: the archive and a folder that must not exist yet.
/// Output: the folder holding `crazyagent`, at the archive root or in its one top folder,
/// or `update_archive_invalid`.
pub fn unpack(archive: &Path, into: &Path) -> Result<PathBuf, UpdateError> {
    std::fs::create_dir(into).map_err(|error| io_failure("Can't unpack the download", error))?;
    let output = run_bounded(
        Command::new(system_tool("tar"))
            .arg("-x")
            .arg("-f")
            .arg(archive)
            .arg("-C")
            .arg(into),
        UNPACK_TIMEOUT,
        "update_archive_invalid",
    )?;
    if !output.status.success() {
        return Err(fail(
            "update_archive_invalid",
            format!(
                "The release archive can't be unpacked: {}",
                last_line(&output.stderr)
            ),
        ));
    }
    let main = format!("crazyagent{EXE_SUFFIX}");
    if regular_file(&into.join(&main)) {
        return Ok(into.to_path_buf());
    }
    let folders: Vec<PathBuf> = std::fs::read_dir(into)
        .map_err(|error| io_failure("Can't read the unpacked release", error))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    match folders.as_slice() {
        [only] if regular_file(&only.join(&main)) => Ok(only.clone()),
        _ => Err(fail(
            "update_archive_invalid",
            "The release archive has no crazyagent.",
        )),
    }
}

fn answers_version(executable: &Path, expected: &str) -> Result<(), UpdateError> {
    let failed = |detail: String| {
        fail(
            "update_new_version_failed",
            format!("The new crazyagent did not answer --version as {expected}: {detail}"),
        )
    };
    let output = run_bounded(
        Command::new(executable).arg("--version"),
        VERSION_TIMEOUT,
        "update_new_version_failed",
    )?;
    if !output.status.success() {
        return Err(failed(format!("it exited with {}", output.status)));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut words = text.split_whitespace();
    let answered = match (words.next(), words.next()) {
        (Some("crazyagent"), Some(word)) => {
            semver::Version::parse(word.trim_start_matches('v')).ok()
        }
        _ => None,
    };
    if answered == Some(version(expected)?) {
        Ok(())
    } else {
        Err(failed(format!("it answered {}", text.trim())))
    }
}

fn old_path(install: &Path, name: &str) -> PathBuf {
    install.join(format!("{name}.old"))
}

fn replace(install: &Path, staged: &Path, name: &str) -> Result<bool, UpdateError> {
    let failed = |error| io_failure(&format!("Can't replace {name}"), error);
    let target = install.join(name);
    let old = old_path(install, name);
    if let Err(error) = std::fs::remove_file(&old)
        && error.kind() != ErrorKind::NotFound
    {
        return Err(failed(error));
    }
    let had_old = std::fs::symlink_metadata(&target).is_ok();
    if had_old {
        std::fs::rename(&target, &old).map_err(failed)?;
    }
    if let Err(error) = std::fs::rename(staged.join(name), &target) {
        if had_old {
            let _ = std::fs::rename(&old, &target);
        }
        return Err(failed(error));
    }
    Ok(had_old)
}

fn restore(install: &Path, moved: &[(String, bool)]) {
    for (name, had_old) in moved.iter().rev() {
        let target = install.join(name);
        if *had_old {
            let _ = std::fs::rename(old_path(install, name), &target);
        } else {
            let _ = std::fs::remove_file(&target);
        }
    }
}

/// Replaces the installed executables with the unpacked release by atomic renames in the
/// same folder. The new `crazyagent` must answer `--version` with the release version before
/// and after the swap; each old file stays as `<name>.old` until then, and any failure puts
/// every old file back. Only the known CrazyAgent executables are moved.
/// Inputs: the install folder, the unpacked folder and the release version.
/// Output: Ok, or the failure after the old files are restored.
pub fn swap(install: &Path, staged: &Path, version: &str) -> Result<(), UpdateError> {
    let main = format!("crazyagent{EXE_SUFFIX}");
    let names: Vec<String> = BINARIES
        .iter()
        .map(|name| format!("{name}{EXE_SUFFIX}"))
        .filter(|name| regular_file(&staged.join(name)))
        .collect();
    if !names.contains(&main) {
        return Err(fail(
            "update_archive_invalid",
            "The release archive has no crazyagent.",
        ));
    }
    answers_version(&staged.join(&main), version)?;
    let mut moved: Vec<(String, bool)> = Vec::with_capacity(names.len());
    for name in &names {
        match replace(install, staged, name) {
            Ok(had_old) => moved.push((name.clone(), had_old)),
            Err(error) => {
                restore(install, &moved);
                return Err(error);
            }
        }
    }
    assert_eq!(moved.len(), names.len());
    if let Err(error) = answers_version(&install.join(&main), version) {
        restore(install, &moved);
        return Err(error);
    }
    for (name, had_old) in &moved {
        if *had_old {
            let _ = std::fs::remove_file(old_path(install, name));
        }
    }
    Ok(())
}

/// Runs `crazyagent service <action>` with a time limit, as both `crazyagent update` and the
/// desktop app do around an update. Inputs: the crazyagent executable and the action.
/// Output: its trimmed stdout, or `update_service_failed` with its last error line.
pub fn service_command(executable: &Path, action: &str) -> Result<String, UpdateError> {
    let output = run_bounded(
        Command::new(executable).arg("service").arg(action),
        SERVICE_TIMEOUT,
        "update_service_failed",
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let reason = last_line(&output.stderr);
    Err(fail(
        "update_service_failed",
        format!(
            "crazyagent service {action} failed: {}",
            if reason.is_empty() {
                last_line(stdout.as_bytes())
            } else {
                reason
            }
        ),
    ))
}

struct Staging {
    path: PathBuf,
}

impl Staging {
    fn create(install: &Path) -> Result<Self, UpdateError> {
        let path = install.join(format!(".crazyagent-update-{}", std::process::id()));
        let failed = |error| io_failure(&format!("Can't write to {}", install.display()), error);
        if std::fs::symlink_metadata(&path).is_ok() {
            std::fs::remove_dir_all(&path).map_err(failed)?;
        }
        std::fs::create_dir(&path).map_err(failed)?;
        Ok(Self { path })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn install_release(manifest: &Manifest, install: &Path, key: &str) -> Result<String, UpdateError> {
    let platform = target().ok_or_else(|| {
        fail(
            "update_platform_missing",
            "CrazyAgent publishes no release for this computer's platform.",
        )
    })?;
    let asset = manifest.platforms.get(platform).ok_or_else(|| {
        fail(
            "update_platform_missing",
            format!("Release {} has no build for {platform}.", manifest.version),
        )
    })?;
    let staging = Staging::create(install)?;
    let archive = staging.path.join("release-archive");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive)
        .map_err(|error| io_failure("Can't save the download", error))?;
    download(&asset.url, &mut file, DOWNLOAD_BYTES_MAX, DOWNLOAD_TIMEOUT)?;
    file.sync_all()
        .map_err(|error| io_failure("Can't save the download", error))?;
    drop(file);
    verify_file(
        &archive,
        &asset.sha256,
        &asset.signature,
        key,
        &manifest.version,
    )?;
    let unpacked = unpack(&archive, &staging.path.join("unpacked"))?;
    let current = install.join(format!("crazyagent{EXE_SUFFIX}"));
    service_command(&current, "stop")?;
    if let Err(error) = swap(install, &unpacked, &manifest.version) {
        return Err(match service_command(&current, "start") {
            Ok(_) => error,
            Err(start) => fail(error.code, format!("{} {}", error.message, start.message)),
        });
    }
    Ok(match service_command(&current, "start") {
        Ok(_) => format!(
            "Updated crazyagent to {}. The engine service restarted.",
            manifest.version
        ),
        Err(start) => format!(
            "Updated crazyagent to {}. {}",
            manifest.version, start.message
        ),
    })
}

#[derive(Debug)]
struct Options {
    check: bool,
    channel: Option<Channel>,
}

fn parse_arguments(args: &[OsString]) -> Result<Options, UpdateError> {
    let usage = || fail("update_usage", USAGE);
    let mut options = Options {
        check: false,
        channel: None,
    };
    let mut words = args.iter().map(|arg| arg.to_str().ok_or_else(usage));
    while let Some(word) = words.next() {
        match word? {
            "--check" if !options.check => options.check = true,
            "--channel" if options.channel.is_none() => {
                let word = words.next().ok_or_else(usage)??;
                options.channel = Some(Channel::parse(word).ok_or_else(usage)?);
            }
            _ => return Err(usage()),
        }
    }
    Ok(options)
}

/// `crazyagent update [--check] [--channel stable|dev]`: reads the channel (`--channel` also
/// saves it), fetches that channel's `cli-latest.json`, and with `--check` only reports. Else
/// it downloads this platform's archive next to the executable, verifies its SHA-256 and
/// signature, stops the engine service, swaps the executables and starts the service again.
/// An executable inside the desktop app is refused. Input: the arguments after `update`.
/// Output: one line for the owner, or the failure; a failure changes no installed file.
pub fn run(args: &[OsString]) -> Result<String, UpdateError> {
    let options = parse_arguments(args)?;
    let root = data_root().ok_or_else(|| {
        fail(
            "update_no_data_root",
            "Can't find CrazyAgent's data folder. Set CRAZYAGENT_HOME to an absolute path.",
        )
    })?;
    let channel = match options.channel {
        Some(channel) => {
            save_channel(&root, channel)?;
            channel
        }
        None => read_channel(&root)?,
    };
    let mut bytes = Vec::new();
    download(
        channel.cli_manifest_url(),
        &mut bytes,
        MANIFEST_BYTES_MAX,
        DOWNLOAD_TIMEOUT,
    )?;
    let manifest = parse_manifest(&bytes)?;
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(&manifest.version, current)? {
        return Ok(format!(
            "crazyagent {current} is up to date on the {} channel.",
            channel.as_str()
        ));
    }
    if options.check {
        return Ok(format!(
            "crazyagent {} is available on the {} channel. This is {current}. Run crazyagent update to install it.",
            manifest.version,
            channel.as_str()
        ));
    }
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| io_failure("Can't find this crazyagent", error))?;
    if inside_app_bundle(&executable) {
        return Err(fail(
            "update_inside_app",
            "This crazyagent belongs to the CrazyAgent app. Update the app instead: open CrazyAgent, then Settings > Updates.",
        ));
    }
    let install = executable
        .parent()
        .ok_or_else(|| fail("update_io", "Can't find the folder holding crazyagent."))?;
    install_release(&manifest, install, &public_key()?)
}

#[cfg(test)]
mod tests;
