use super::manifest::Purpose;
use super::store::Loaded;
use crate::engine::EngineError;
use std::path::Path;

#[cfg(unix)]
use super::manifest::{HttpSpec, Piece, Runs, Step};
#[cfg(unix)]
use crate::credentials::SecretString;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Stdio};

#[cfg(unix)]
const CURL: &str = "/usr/bin/curl";
#[cfg(unix)]
const CONNECT_SECS: &str = "10";
#[cfg(unix)]
const TIME_SECS: &str = "120";
#[cfg(unix)]
const REPLY_BYTES_MAX: u64 = 16 * 1024 * 1024;
#[cfg(unix)]
const CONFIG_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const CONFIG_MARGIN: usize = 128;

/// The engine-root folder that holds each request body for its one call, an engine secret
/// no agent sandbox reaches (the owner's temp folder can be an agent's TMPDIR).
pub(crate) const SCRATCH: &str = "plugin-scratch";

fn failed() -> EngineError {
    EngineError("app_plugin_call_failed".into())
}

#[cfg(unix)]
fn invalid() -> EngineError {
    EngineError("app_plugin_reply_invalid".into())
}

/// Embeds texts with an HTTP embedding plugin: one POST through curl to the manifest's
/// URL, the bound keys read from their key files for this request only, no redirects.
/// Blocking; never call it on the engine tick. Inputs: the engine root, a plugin from
/// `store::ready`, 1 to `batch` texts and their purpose. Output: one L2-normalized vector
/// of the declared size per text, or `app_plugin_call_failed`, `_reply_invalid`, `_needs_key`.
#[cfg(unix)]
pub fn embed(
    root: &Path,
    loaded: &Loaded,
    inputs: &[&str],
    purpose: Purpose,
) -> Result<Vec<Vec<f32>>, EngineError> {
    let Runs::Http(spec) = &loaded.manifest.runs else {
        return Err(failed());
    };
    assert_eq!(
        loaded.bindings.len(),
        spec.secrets.len(),
        "one binding per declared secret"
    );
    let mut secrets = Vec::with_capacity(loaded.bindings.len());
    for bound in &loaded.bindings {
        secrets.push(super::store::secret_value(root, bound)?);
    }
    let body = serde_json::to_vec(&spec.body(inputs, purpose)).map_err(|_| failed())?;
    let scratch = Scratch::new(root)?;
    let path = scratch.write(&body)?;
    let mut buffer = Config::with_capacity(CONFIG_BYTES);
    config(spec, &secrets, &path, &mut buffer)?;
    drop(secrets);
    let reply = run(spec, &buffer.0);
    drop(buffer);
    drop(scratch);
    vectors(&reply?, spec, inputs.len())
}

/// No HTTP plugin call where the private request file cannot be made. Output:
/// `app_plugin_call_failed`.
#[cfg(not(unix))]
pub fn embed(
    _root: &Path,
    _loaded: &Loaded,
    _inputs: &[&str],
    _purpose: Purpose,
) -> Result<Vec<Vec<f32>>, EngineError> {
    Err(failed())
}

/// The curl config bytes, held in one buffer that never grows, so no copy of a key is left
/// in freed memory by a resize, and zeroed when dropped, so a key in it is wiped on every
/// exit: a send, an early error or an unwind (rv-v3sec PLUGINS-2).
#[cfg(unix)]
struct Config(Vec<u8>);

#[cfg(unix)]
impl Config {
    fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    fn push(&mut self, text: &str) {
        assert!(
            self.0.len() + text.len() <= self.0.capacity(),
            "the config buffer never grows"
        );
        self.0.extend_from_slice(text.as_bytes());
    }

    fn wipe(&mut self) {
        self.0.fill(0);
    }
}

#[cfg(unix)]
impl Drop for Config {
    fn drop(&mut self) {
        self.wipe();
    }
}

#[cfg(unix)]
fn push_quoted(config: &mut Config, text: &str) -> Result<(), EngineError> {
    if text.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
        || config.0.len() + 2 * text.len() + CONFIG_MARGIN > config.0.capacity()
    {
        return Err(failed());
    }
    for byte in text.bytes() {
        if matches!(byte, b'\\' | b'"') {
            config.0.push(b'\\');
        }
        config.0.push(byte);
    }
    Ok(())
}

#[cfg(unix)]
fn push_line(config: &mut Config, key: &str, value: &str) -> Result<(), EngineError> {
    config.push(key);
    config.push(" = \"");
    push_quoted(config, value)?;
    config.push("\"\n");
    Ok(())
}

#[cfg(unix)]
fn lines(
    config: &mut Config,
    spec: &HttpSpec,
    secrets: &[SecretString],
    body: &Path,
) -> Result<(), EngineError> {
    push_line(config, "url", spec.url.as_str())?;
    config.push("request = \"POST\"\nheader = \"Expect:\"\n");
    config.push("header = \"Content-Type: application/json\"\n");
    if spec.url.host_str() == Some("localhost") {
        let port = spec.url.port_or_known_default().ok_or_else(failed)?;
        push_line(config, "resolve", &format!("localhost:{port}:127.0.0.1"))?;
    }
    for header in &spec.headers {
        config.push("header = \"");
        push_quoted(config, &header.name)?;
        config.push(": ");
        for piece in &header.value {
            match piece {
                Piece::Text(text) => push_quoted(config, text)?,
                Piece::Secret(index) => push_quoted(config, secrets[*index].expose())?,
            }
        }
        config.push("\"\n");
    }
    let body = body.to_str().ok_or_else(failed)?;
    push_line(config, "data-binary", &format!("@{body}"))
}

/// Writes the curl config, which goes only to curl's stdin: the URL, a loopback pin for
/// `localhost`, the manifest's headers with secrets filled in, and the body file, quotes and
/// backslashes escaped. A write that fails part way zeroes what it wrote before returning.
/// Inputs: the plugin's HTTP spec, the secret values in declaration order, the body file and
/// an empty buffer. Output: Ok with the config in the buffer, or `app_plugin_call_failed`.
#[cfg(unix)]
fn config(
    spec: &HttpSpec,
    secrets: &[SecretString],
    body: &Path,
    config: &mut Config,
) -> Result<(), EngineError> {
    assert!(
        config.0.is_empty(),
        "a config is written into an empty buffer"
    );
    let capacity = config.0.capacity();
    let written = lines(config, spec, secrets, body);
    if written.is_err() {
        config.wipe();
    }
    assert_eq!(
        config.0.capacity(),
        capacity,
        "the config buffer never grows"
    );
    written
}

/// Builds the curl command without running it: no `.curlrc`, only the plugin's scheme,
/// no redirects, bounded time, the reply status after the body, and a cleared
/// environment. Its arguments and environment hold no URL, header or key.
#[cfg(unix)]
fn command(spec: &HttpSpec) -> Command {
    let scheme = if spec.local_unencrypted {
        "=http"
    } else {
        "=https"
    };
    let mut command = Command::new(CURL);
    command
        .arg("-q")
        .args(["--silent", "--globoff"])
        .args(["--proto", scheme])
        .args(["--max-redirs", "0"])
        .args(["--connect-timeout", CONNECT_SECS])
        .args(["--max-time", TIME_SECS])
        .args(["--write-out", "\\n%{http_code}"])
        .args(["--config", "-"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

#[cfg(unix)]
fn run(spec: &HttpSpec, config: &[u8]) -> Result<Vec<u8>, EngineError> {
    use std::io::{Read, Write};
    let mut child = crate::spawn_gate::spawn(&mut command(spec)).map_err(|_| failed())?;
    let written = child.stdin.take().map(|mut input| input.write_all(config));
    let mut reply = Vec::new();
    let read = child
        .stdout
        .take()
        .map(|output| output.take(REPLY_BYTES_MAX + 1).read_to_end(&mut reply));
    let over = reply.len() as u64 > REPLY_BYTES_MAX;
    if over {
        let _ = child.kill();
    }
    let status = child.wait();
    if over {
        return Err(invalid());
    }
    match (written, read, status) {
        (Some(Ok(())), Some(Ok(_)), Ok(status)) if status.success() => {}
        _ => return Err(failed()),
    }
    let at = reply
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(failed)?;
    let code: u16 = std::str::from_utf8(&reply[at + 1..])
        .ok()
        .and_then(|code| code.parse().ok())
        .ok_or_else(failed)?;
    if !(200..300).contains(&code) {
        return Err(failed());
    }
    reply.truncate(at);
    Ok(reply)
}

#[cfg(unix)]
fn walk<'a>(value: &'a serde_json::Value, steps: &[Step]) -> Option<&'a serde_json::Value> {
    steps.iter().try_fold(value, |value, step| match step {
        Step::Key(key) => value.get(key),
        Step::Each => None,
    })
}

#[cfg(unix)]
fn normalize(vector: &mut [f32]) {
    let norm = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 && norm.is_finite() {
        for value in vector.iter_mut() {
            *value = (f64::from(*value) / norm) as f32;
        }
    }
}

/// Reads the vectors out of a plugin's reply along its declared path. Inputs: the reply
/// body, the plugin's HTTP spec and how many texts were sent. Output: one L2-normalized
/// vector of exactly `dimensions` finite numbers per text, in order (a zero vector stays
/// zero), or `app_plugin_reply_invalid`.
#[cfg(unix)]
fn vectors(reply: &[u8], spec: &HttpSpec, count: usize) -> Result<Vec<Vec<f32>>, EngineError> {
    let value: serde_json::Value = serde_json::from_slice(reply).map_err(|_| invalid())?;
    let each = spec
        .vectors
        .iter()
        .position(|step| *step == Step::Each)
        .expect("a validated vectors path has exactly one *");
    let items = walk(&value, &spec.vectors[..each])
        .and_then(serde_json::Value::as_array)
        .ok_or_else(invalid)?;
    if items.len() != count {
        return Err(invalid());
    }
    let mut vectors = Vec::with_capacity(count);
    for item in items {
        let numbers = walk(item, &spec.vectors[each + 1..])
            .and_then(serde_json::Value::as_array)
            .ok_or_else(invalid)?;
        if numbers.len() != usize::from(spec.dimensions) {
            return Err(invalid());
        }
        let mut vector = Vec::with_capacity(numbers.len());
        for number in numbers {
            let value = number.as_f64().ok_or_else(invalid)? as f32;
            if !value.is_finite() {
                return Err(invalid());
            }
            vector.push(value);
        }
        normalize(&mut vector);
        vectors.push(vector);
    }
    assert_eq!(vectors.len(), count);
    Ok(vectors)
}

/// A private folder for one request body inside the engine root's guarded `SCRATCH`
/// folder, readable only by the engine user and removed when dropped.
#[cfg(unix)]
struct Scratch(PathBuf);

#[cfg(unix)]
impl Scratch {
    fn new(root: &Path) -> Result<Self, EngineError> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        let folder = root.join(SCRATCH);
        match std::fs::DirBuilder::new().mode(0o700).create(&folder) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(failed()),
        }
        let metadata = std::fs::symlink_metadata(&folder).map_err(|_| failed())?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(failed());
        }
        let path = folder.join(crate::engine::owner::random_id()?);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| failed())?;
        Ok(Self(path))
    }

    fn write(&self, body: &[u8]) -> Result<PathBuf, EngineError> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.0.join("request");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&path)
            .map_err(|_| failed())?;
        file.write_all(body).map_err(|_| failed())?;
        Ok(path)
    }
}

#[cfg(unix)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(all(test, unix))]
#[path = "http_tests.rs"]
mod tests;
