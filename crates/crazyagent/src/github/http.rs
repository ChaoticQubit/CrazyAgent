use crate::credentials::SecretString;
use crate::engine::EngineError;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CURL: &str = "/usr/bin/curl";
const BODY_BYTES_MAX: usize = 4 << 20;
const SCOPES_LINE_MAX: usize = 1024;
const TRAILER_BYTES_MAX: usize = SCOPES_LINE_MAX + 16;
const WRITE_OUT: &str = "\n%header{x-oauth-scopes}\n%{http_code}";
const REQUEST_TIME: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "CrazyAgent";

/// One HTTP exchange through the system curl. The URL, headers, token and form body go
/// only into curl's config on stdin, so nothing secret reaches an argument or the
/// environment; curl reads no `.curlrc`, follows no redirects and only uses the allowed
/// schemes.
pub(crate) struct Request<'a> {
    pub(crate) url: url::Url,
    pub(crate) accept: &'a str,
    pub(crate) token: Option<&'a SecretString>,
    pub(crate) form: Option<Vec<(&'a str, &'a str)>>,
    pub(crate) headers: Vec<(&'a str, &'a str)>,
    pub(crate) allow_http: bool,
}

/// The method of a request that changes something on GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Post,
    Put,
    Patch,
    Delete,
}

impl Verb {
    fn name(self) -> &'static str {
        match self {
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// What came back: the HTTP status, at most 4 MiB of body, and the scopes a classic
/// GitHub token holds when GitHub named them in `X-OAuth-Scopes`.
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
    pub(crate) scopes: Option<Vec<String>>,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn quoted(value: &str) -> Result<String, EngineError> {
    if value.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(failure("github_request_invalid"));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

impl Request<'_> {
    /// Builds curl's config: the URL, headers, token, and either the form body or a
    /// method with its JSON body, never both; a bodyless PUT, POST or PATCH sends an empty
    /// body so it says its length is zero. Input: the method and body, none for a GET or a
    /// form. Output: the config bytes, or `github_request_invalid` for a control character.
    fn config(&self, body: Option<(Verb, Option<&str>)>) -> Result<Vec<u8>, EngineError> {
        assert!(body.is_none() || self.form.is_none());
        let mut config = format!(
            "url = {}\nheader = {}\nuser-agent = {}\n",
            quoted(self.url.as_str())?,
            quoted(&format!("Accept: {}", self.accept))?,
            quoted(USER_AGENT)?
        );
        if self.url.host_str() == Some("api.github.com") || self.allow_http {
            config.push_str("header = \"X-GitHub-Api-Version: 2022-11-28\"\n");
        }
        if let Some(token) = self.token {
            config.push_str(&format!(
                "header = {}\n",
                quoted(&format!("Authorization: Bearer {}", token.expose()))?
            ));
        }
        for (name, value) in &self.headers {
            config.push_str(&format!(
                "header = {}\n",
                quoted(&format!("{name}: {value}"))?
            ));
        }
        if let Some(form) = &self.form {
            let body = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(form.iter())
                .finish();
            config.push_str(&format!("data = {}\n", quoted(&body)?));
        }
        if let Some((verb, json)) = body {
            config.push_str(&format!("request = \"{}\"\n", verb.name()));
            match json {
                Some(json) => {
                    config.push_str("header = \"Content-Type: application/json\"\n");
                    config.push_str(&format!("data-raw = {}\n", quoted(json)?));
                }
                None if verb != Verb::Delete => config.push_str("data-raw = \"\"\n"),
                None => {}
            }
        }
        Ok(config.into_bytes())
    }

    /// Builds the curl command without running it. Output: the command, whose arguments
    /// and environment hold no URL, header or secret, and the config bytes for its stdin.
    pub(crate) fn command(&self) -> Result<(Command, Vec<u8>), EngineError> {
        self.command_with(None)
    }

    /// Builds the curl command for a POST of a JSON body without running it. Input: the
    /// body. Output: the command, whose arguments and environment hold no URL, header,
    /// body or secret, and the config bytes for its stdin.
    pub(crate) fn json_command(&self, json: &str) -> Result<(Command, Vec<u8>), EngineError> {
        self.command_with(Some((Verb::Post, Some(json))))
    }

    fn command_with(
        &self,
        body: Option<(Verb, Option<&str>)>,
    ) -> Result<(Command, Vec<u8>), EngineError> {
        let secure = self.url.scheme() == "https";
        let loopback = self.url.scheme() == "http"
            && self.allow_http
            && self.url.host_str() == Some("127.0.0.1");
        if !secure && !loopback {
            return Err(failure("github_request_invalid"));
        }
        let mut command = Command::new(CURL);
        command
            .arg("-q")
            .args(["--silent", "--show-error", "--globoff"])
            .args(["--proto", if loopback { "=http" } else { "=https" }])
            .args(["--max-redirs", "0"])
            .args(["--connect-timeout", "10"])
            .args(["--max-time", "30"])
            .args(["--max-filesize", &BODY_BYTES_MAX.to_string()])
            .args(["--write-out", WRITE_OUT])
            .args(["--config", "-"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok((command, self.config(body)?))
    }

    /// Sends the request and waits at most 30 s. Output: the reply, or
    /// `github_unreachable` when curl could not finish the exchange.
    pub(crate) fn send(&self) -> Result<Reply, EngineError> {
        let (command, config) = self.command()?;
        exchange(command, config)
    }

    /// POSTs a JSON body and waits at most 30 s, as `send` does. Input: the body.
    /// Output: the reply, or `github_unreachable` when curl could not finish the exchange.
    pub(crate) fn send_json(&self, json: &str) -> Result<Reply, EngineError> {
        let (command, config) = self.json_command(json)?;
        exchange(command, config)
    }

    /// Sends a request with a method and an optional JSON body and waits at most 30 s, as
    /// `send` does. Inputs: the method and the body. Output: the reply, or
    /// `github_unreachable` when curl could not finish the exchange.
    pub(crate) fn send_body(&self, verb: Verb, json: Option<&str>) -> Result<Reply, EngineError> {
        let (command, config) = self.command_with(Some((verb, json)))?;
        exchange(command, config)
    }
}

/// Runs a built curl command, writes its config to curl's stdin, wipes the config and
/// reads the reply within the request time. Inputs: the command and its config. Output:
/// the reply, or `github_unreachable`.
fn exchange(mut command: Command, mut config: Vec<u8>) -> Result<Reply, EngineError> {
    let spawned = crate::spawn_gate::spawn(&mut command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            config.fill(0);
            return Err(failure("github_unreachable"));
        }
    };
    let mut stdin = child.stdin.take().expect("curl stdin is piped");
    let written = stdin.write_all(&config);
    config.fill(0);
    drop(stdin);
    let stdout = child.stdout.take().expect("curl stdout is piped");
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout
            .take((BODY_BYTES_MAX + TRAILER_BYTES_MAX) as u64)
            .read_to_end(&mut bytes);
        (read, bytes)
    });
    let deadline = Instant::now() + REQUEST_TIME + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| failure("github_unreachable"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(failure("github_unreachable"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (read, bytes) = reader.join().map_err(|_| failure("github_unreachable"))?;
    if written.is_err() || read.is_err() || !status.success() {
        return Err(failure("github_unreachable"));
    }
    split(bytes)
}

/// Splits curl's output into the body, the `X-OAuth-Scopes` line and the status line
/// that `WRITE_OUT` puts after it. Input: the output. Output: the reply, or
/// `github_reply_invalid` for a body past 4 MiB or a missing or bad status.
fn split(mut bytes: Vec<u8>) -> Result<Reply, EngineError> {
    let invalid = || failure("github_reply_invalid");
    if bytes.len() > BODY_BYTES_MAX + TRAILER_BYTES_MAX {
        return Err(invalid());
    }
    let cut = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(invalid)?;
    let code = std::str::from_utf8(&bytes[cut + 1..]).map_err(|_| invalid())?;
    let status: u16 = code.parse().map_err(|_| invalid())?;
    if !(100..=599).contains(&status) {
        return Err(invalid());
    }
    bytes.truncate(cut);
    let line = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(invalid)?;
    let scopes = scopes(&bytes[line + 1..]);
    bytes.truncate(line);
    if bytes.len() > BODY_BYTES_MAX {
        return Err(invalid());
    }
    Ok(Reply {
        status,
        body: bytes,
        scopes,
    })
}

/// Reads the `X-OAuth-Scopes` line: the comma-separated scopes of a classic token.
/// Input: the line curl wrote. Output: the scopes, or None for an empty line (a token
/// kind GitHub names no scopes for), a curl too old to print headers, which writes the
/// variable as it is, or any line that is not a list of scope names.
fn scopes(line: &[u8]) -> Option<Vec<String>> {
    if line.is_empty() || line.len() > SCOPES_LINE_MAX {
        return None;
    }
    let scopes: Vec<String> = std::str::from_utf8(line)
        .ok()?
        .split(',')
        .map(str::trim)
        .map(str::to_owned)
        .collect();
    let named = scopes.iter().all(|scope| {
        !scope.is_empty()
            && scope
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b':' || byte == b'_')
    });
    named.then_some(scopes)
}
