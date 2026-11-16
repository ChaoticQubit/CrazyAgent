use super::{Credential, McpError, REPLY_BYTES_MAX};
use crate::app_plugins::connector::Scheme;
use crate::app_plugins::http::{self, Config, Ended, Scratch};
use crate::app_plugins::store;
use crate::credentials::SecretString;
use crate::engine::EngineError;
use crate::storage::accounts::{self, PLUGIN_KEYS};
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};

const CONNECT_SECS: &str = "10";
const TIME_SECS: &str = "60";
const CONFIG_BYTES: usize = 48 * 1024;
const HEAD_BYTES_MAX: usize = 64 * 1024;
const CHALLENGES_MAX: usize = 8;
const SCOPE_BYTES_MAX: usize = 1024;
pub(in crate::connectors) const CURL_TIMED_OUT: i32 = 28;

fn failed() -> McpError {
    McpError::Refused("connector_call_failed")
}

fn invalid() -> McpError {
    McpError::Refused("connector_reply_invalid")
}

fn engine_failed(_: EngineError) -> McpError {
    failed()
}

/// What one request does: posts a JSON-RPC message, or ends a session.
pub(super) enum Verb<'a> {
    Post(&'a [u8]),
    Delete,
}

/// One HTTP request to a vendor: the checked address, the body, curl's address pins,
/// the headers that hold no secret, how to sign in, and the JSON-RPC id whose response
/// ends a stream.
pub(super) struct Request<'a> {
    pub(super) url: &'a url::Url,
    pub(super) verb: Verb<'a>,
    pub(super) pins: Option<&'a str>,
    pub(super) headers: &'a [(String, String)],
    pub(super) auth: &'a Credential,
    pub(super) wait: Option<Value>,
}

/// A reply's body: one JSON-RPC message, nothing, or bytes that are not one.
#[derive(Debug, PartialEq)]
pub(super) enum Body {
    Message(Value),
    Empty,
    Unreadable,
}

/// One HTTP reply: its status, the session id header as sent, and its body.
#[derive(Debug, PartialEq)]
pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) session: Option<String>,
    pub(super) body: Body,
}

/// Builds the curl command without running it: no `.curlrc`, one scheme, no redirects,
/// no proxy, unbuffered output with the reply's headers first, bounded time and a cleared
/// environment. Its arguments and environment hold no URL, header or key. Input: the one
/// scheme it may speak, as curl's `--proto` value. Output: the command.
pub(in crate::connectors) fn command(scheme: &str) -> Command {
    let mut command = Command::new(http::CURL);
    command
        .arg("-q")
        .args(["--silent", "--globoff", "--no-buffer", "--include"])
        .args(["--proto", scheme])
        .args(["--max-redirs", "0"])
        .args(["--noproxy", "*"])
        .args(["--connect-timeout", CONNECT_SECS])
        .args(["--max-time", TIME_SECS])
        .args(["--config", "-"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

fn keys(root: &Path, auth: &Credential) -> Result<Vec<SecretString>, McpError> {
    let key = match auth {
        Credential::Key(key) => key,
        Credential::OAuth { access, .. } => {
            let token = accounts::read_bearer_in(root, PLUGIN_KEYS, access)
                .map_err(|_| failed())?
                .ok_or(McpError::Refused("connector_sign_in_needed"))?;
            return Ok(vec![token]);
        }
    };
    let mut secrets = Vec::with_capacity(key.bindings.len());
    for bound in &key.bindings {
        let secret = store::secret_value(root, bound).map_err(|error| {
            if error.0 == "app_plugin_needs_key" {
                McpError::Refused("connector_key_missing")
            } else {
                failed()
            }
        })?;
        secrets.push(secret);
    }
    Ok(secrets)
}

fn header(config: &mut Config, name: &str, value: &str) -> Result<(), McpError> {
    config.push("header = \"");
    http::push_quoted(config, name).map_err(engine_failed)?;
    config.push(": ");
    http::push_quoted(config, value).map_err(engine_failed)?;
    config.push("\"\n");
    Ok(())
}

/// Writes the curl config, which goes only to curl's stdin: the URL, the verb, the fixed
/// headers, the address pins, the key header, the other headers and the body file. The
/// buffer never grows and is wiped when dropped, also on an early error. Inputs: the
/// request, the key values in declaration order and the body file. Output: the config, or
/// `connector_call_failed`.
fn config(
    request: &Request<'_>,
    secrets: &[SecretString],
    body: Option<&Path>,
) -> Result<Config, McpError> {
    let mut config = Config::with_capacity(CONFIG_BYTES);
    http::push_line(&mut config, "url", request.url.as_str()).map_err(engine_failed)?;
    config.push(match request.verb {
        Verb::Post(_) => "request = \"POST\"\n",
        Verb::Delete => "request = \"DELETE\"\n",
    });
    config.push("header = \"Expect:\"\nheader = \"Content-Type: application/json\"\n");
    config.push("header = \"Accept: application/json, text/event-stream\"\n");
    config.push(concat!(
        "header = \"User-Agent: CrazyAgent/",
        env!("CARGO_PKG_VERSION"),
        "\"\n"
    ));
    if let Some(pins) = request.pins {
        http::push_line(&mut config, "resolve", pins).map_err(engine_failed)?;
    }
    config.push("header = \"");
    match request.auth {
        Credential::Key(key) => {
            http::push_quoted(&mut config, &key.header).map_err(engine_failed)?;
            match (key.scheme, secrets) {
                (Scheme::Bearer, [secret]) => {
                    config.push(": Bearer ");
                    http::push_quoted(&mut config, secret.expose()).map_err(engine_failed)?;
                }
                (Scheme::Basic, [secret, user]) => {
                    config.push(": Basic ");
                    http::push_basic(&mut config, user.expose(), secret.expose())
                        .map_err(engine_failed)?;
                }
                _ => return Err(failed()),
            }
        }
        Credential::OAuth { .. } => {
            let [token] = secrets else {
                return Err(failed());
            };
            config.push("Authorization: Bearer ");
            http::push_quoted(&mut config, token.expose()).map_err(engine_failed)?;
        }
    }
    config.push("\"\n");
    for (name, value) in request.headers {
        header(&mut config, name, value)?;
    }
    if let Some(body) = body {
        let body = body.to_str().ok_or_else(failed)?;
        http::push_line(&mut config, "data-binary", &format!("@{body}")).map_err(engine_failed)?;
    }
    Ok(config)
}

/// Sends one request through curl and reads the reply as it arrives, stopping at the
/// awaited response of a stream or at the size bound. The key is read here, lives in the
/// wiped config only and is gone before the reply is read. Blocking. Inputs: the engine
/// root and the request. Output: the reply; a redirect, a refused key, an oversized reply
/// and a timeout come back as errors.
pub(super) fn send(root: &Path, request: &Request<'_>) -> Result<Reply, McpError> {
    let secrets = keys(root, request.auth)?;
    let scratch = Scratch::new(root).map_err(engine_failed)?;
    let body = match request.verb {
        Verb::Post(body) => Some(scratch.write(body).map_err(engine_failed)?),
        Verb::Delete => None,
    };
    let config = config(request, &secrets, body.as_deref())?;
    drop(secrets);
    let scheme = if cfg!(test) && request.url.scheme() == "http" {
        "=http"
    } else {
        "=https"
    };
    let mut reader = Reader::new(request.wait.clone());
    let ended = http::stream(&mut command(scheme), &config, &mut |chunk| {
        reader.feed(chunk)
    });
    drop(config);
    drop(scratch);
    reader.reply(ended.map_err(engine_failed)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Json,
    Stream,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Head,
    Body,
    Done,
    Over,
    Bad,
}

/// Reads curl's output as it arrives: the reply's head, then a JSON body or an event
/// stream, counting body bytes against `REPLY_BYTES_MAX`.
struct Reader {
    wait: Option<Value>,
    phase: Phase,
    buffer: Vec<u8>,
    status: u16,
    kind: Kind,
    session: Option<String>,
    challenges: Vec<String>,
    total: usize,
    data: Vec<u8>,
    found: Option<Value>,
}

pub(in crate::connectors) fn head_end(bytes: &[u8]) -> Option<(usize, usize)> {
    (0..bytes.len()).find_map(|at| {
        if bytes[at..].starts_with(b"\r\n\r\n") {
            Some((at, 4))
        } else if bytes[at..].starts_with(b"\n\n") {
            Some((at, 2))
        } else {
            None
        }
    })
}

pub(in crate::connectors) fn status_of(line: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.split(' ');
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let status: u16 = parts.next()?.parse().ok()?;
    (100..600).contains(&status).then_some(status)
}

fn without_return(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

impl Reader {
    fn new(wait: Option<Value>) -> Self {
        Self {
            wait,
            phase: Phase::Head,
            buffer: Vec::new(),
            status: 0,
            kind: Kind::Other,
            session: None,
            challenges: Vec::new(),
            total: 0,
            data: Vec::new(),
            found: None,
        }
    }

    /// Takes the next bytes curl wrote. Output: false to stop curl, when the awaited
    /// response is in, the reply is over the bound or its head cannot be read.
    fn feed(&mut self, chunk: &[u8]) -> bool {
        match self.phase {
            Phase::Head => {
                self.buffer.extend_from_slice(chunk);
                self.head()
            }
            Phase::Body => self.body(chunk),
            Phase::Done | Phase::Over | Phase::Bad => false,
        }
    }

    fn head(&mut self) -> bool {
        loop {
            let Some((end, gap)) = head_end(&self.buffer) else {
                if self.buffer.len() > HEAD_BYTES_MAX {
                    self.phase = Phase::Bad;
                    return false;
                }
                return true;
            };
            let rest = self.buffer.split_off(end + gap);
            let block = std::mem::replace(&mut self.buffer, rest);
            if !self.block(&block[..end]) {
                self.phase = Phase::Bad;
                return false;
            }
            if (100..200).contains(&self.status) {
                continue;
            }
            self.phase = Phase::Body;
            let rest = std::mem::take(&mut self.buffer);
            return self.body(&rest);
        }
    }

    fn block(&mut self, block: &[u8]) -> bool {
        let mut lines = block.split(|byte| *byte == b'\n').map(without_return);
        let Some(status) = lines.next().and_then(status_of) else {
            return false;
        };
        self.status = status;
        self.kind = Kind::Other;
        self.session = None;
        self.challenges.clear();
        for line in lines {
            let Some(colon) = line.iter().position(|byte| *byte == b':') else {
                continue;
            };
            let name = &line[..colon];
            let value = String::from_utf8_lossy(&line[colon + 1..]);
            let value = value.trim();
            if name.eq_ignore_ascii_case(b"content-type") {
                let value = value.to_ascii_lowercase();
                self.kind = if value.starts_with("application/json") {
                    Kind::Json
                } else if value.starts_with("text/event-stream") {
                    Kind::Stream
                } else {
                    Kind::Other
                };
            } else if name.eq_ignore_ascii_case(b"mcp-session-id") {
                self.session = Some(value.to_owned());
            } else if name.eq_ignore_ascii_case(b"www-authenticate")
                && self.challenges.len() < CHALLENGES_MAX
            {
                self.challenges.push(value.to_owned());
            }
        }
        true
    }

    fn body(&mut self, chunk: &[u8]) -> bool {
        self.total += chunk.len();
        if self.total > REPLY_BYTES_MAX {
            self.phase = Phase::Over;
            return false;
        }
        let from = self.buffer.len();
        self.buffer.extend_from_slice(chunk);
        if self.kind != Kind::Stream {
            return true;
        }
        let buffer = std::mem::take(&mut self.buffer);
        let mut start = 0;
        let mut scan = from;
        while let Some(offset) = buffer[scan..].iter().position(|byte| *byte == b'\n') {
            let end = scan + offset;
            let done = self.line(without_return(&buffer[start..end]));
            start = end + 1;
            scan = start;
            if done {
                self.phase = Phase::Done;
                return false;
            }
        }
        self.buffer = buffer[start..].to_vec();
        true
    }

    fn line(&mut self, line: &[u8]) -> bool {
        if line.is_empty() {
            return self.event();
        }
        if let Some(rest) = line.strip_prefix(b"data:") {
            self.data
                .extend_from_slice(rest.strip_prefix(b" ").unwrap_or(rest));
            self.data.push(b'\n');
        }
        false
    }

    fn event(&mut self) -> bool {
        let data = std::mem::take(&mut self.data);
        if data.is_empty() {
            return false;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&data) else {
            return false;
        };
        if self.answers(&message) {
            self.found = Some(message);
            true
        } else {
            false
        }
    }

    /// Whether a stream message is the response this request waits for: no method, and a
    /// result or error under the awaited id, or an error with no id at all. A request or a
    /// notice the vendor sends on the stream is never answered.
    fn answers(&self, message: &Value) -> bool {
        let Some(object) = message.as_object() else {
            return false;
        };
        if object.contains_key("method") {
            return false;
        }
        match (&self.wait, object.get("id")) {
            (Some(wait), Some(id)) if id == wait => {
                object.contains_key("result") || object.contains_key("error")
            }
            (_, None | Some(Value::Null)) => object.contains_key("error"),
            _ => false,
        }
    }

    fn reply(mut self, ended: Ended) -> Result<Reply, McpError> {
        match self.phase {
            Phase::Over => return Err(McpError::TooLarge),
            Phase::Bad => return Err(invalid()),
            Phase::Head | Phase::Body | Phase::Done => {}
        }
        match ended {
            Ended::Stopped | Ended::Exit(Some(0)) => {}
            Ended::Exit(Some(CURL_TIMED_OUT)) => return Err(McpError::Timeout),
            Ended::Exit(_) => return Err(failed()),
        }
        if self.phase == Phase::Head {
            return Err(invalid());
        }
        if (300..400).contains(&self.status) {
            return Err(McpError::Refused("connector_redirect_refused"));
        }
        if self.status == 401 {
            return Err(McpError::Unauthorized {
                scope: scope(&self.challenges),
            });
        }
        if self.status == 403 && insufficient(&self.challenges) {
            return Err(McpError::InsufficientScope {
                scope: scope(&self.challenges),
            });
        }
        let body = match self.kind {
            Kind::Stream => {
                if self.found.is_none() {
                    self.event();
                }
                self.found.take().map_or(Body::Unreadable, Body::Message)
            }
            Kind::Json | Kind::Other if self.buffer.is_empty() => Body::Empty,
            Kind::Json => {
                serde_json::from_slice(&self.buffer).map_or(Body::Unreadable, Body::Message)
            }
            Kind::Other => Body::Unreadable,
        };
        Ok(Reply {
            status: self.status,
            session: self.session,
            body,
        })
    }
}

fn parts(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (at, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&value[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

fn unquoted(value: &str) -> String {
    let value = value.trim();
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return value.to_owned();
    };
    let mut out = String::with_capacity(inner.len());
    let mut escaped = false;
    for character in inner.chars() {
        if escaped || character != '\\' {
            out.push(character);
            escaped = false;
        } else {
            escaped = true;
        }
    }
    out
}

/// Reads the Bearer parameters out of one `WWW-Authenticate` value, which may hold more
/// than one challenge. Input: the header value. Output: each Bearer parameter, its name
/// in lowercase and its value unquoted.
pub(in crate::connectors) fn bearer(value: &str) -> Vec<(String, String)> {
    let mut scheme = String::new();
    let mut found = Vec::new();
    for part in parts(value) {
        let part = part.trim();
        let Some((left, right)) = part.split_once('=') else {
            if let Some(word) = part.split_whitespace().next() {
                scheme = word.to_ascii_lowercase();
            }
            continue;
        };
        let left = left.trim();
        let name = match left.rsplit_once(char::is_whitespace) {
            Some((word, name)) => {
                scheme = word.trim().to_ascii_lowercase();
                name
            }
            None => left,
        };
        if scheme == "bearer" {
            found.push((name.to_ascii_lowercase(), unquoted(right)));
        }
    }
    found
}

fn insufficient(challenges: &[String]) -> bool {
    challenges
        .iter()
        .flat_map(|challenge| bearer(challenge))
        .any(|(name, value)| name == "error" && value == "insufficient_scope")
}

fn valid_scope(scope: &str) -> bool {
    let token =
        |byte: u8| byte == 0x21 || (0x23..=0x5b).contains(&byte) || (0x5d..=0x7e).contains(&byte);
    !scope.is_empty()
        && scope.len() <= SCOPE_BYTES_MAX
        && scope
            .split(' ')
            .all(|word| !word.is_empty() && word.bytes().all(token))
}

/// The scope a vendor's challenge asks for. Input: the `WWW-Authenticate` values. Output:
/// the first Bearer scope when it is a plain scope list, none otherwise, so a scope with a
/// hidden or odd character is never shown.
fn scope(challenges: &[String]) -> Option<String> {
    challenges
        .iter()
        .flat_map(|challenge| bearer(challenge))
        .find(|(name, _)| name == "scope")
        .map(|(_, value)| value)
        .filter(|value| valid_scope(value))
}
