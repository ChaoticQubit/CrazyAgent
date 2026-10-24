use crate::credentials::SecretString;
use crate::engine::EngineError;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CURL: &str = "/usr/bin/curl";
const BODY_BYTES_MAX: usize = 4 << 20;
const REQUEST_TIME: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "CrazyAgent";

/// One HTTP exchange through the system curl. The URL, headers, token and form body go
/// only into curl's config on stdin, so nothing secret reaches an argument or the
/// environment; curl reads no `.curlrc`, follows no redirects and only uses the allowed
/// schemes.
pub(super) struct Request<'a> {
    pub(super) url: url::Url,
    pub(super) accept: &'a str,
    pub(super) token: Option<&'a SecretString>,
    pub(super) form: Option<Vec<(&'a str, &'a str)>>,
    pub(super) allow_http: bool,
}

/// What came back: the HTTP status and at most 4 MiB of body.
pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) body: Vec<u8>,
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
    fn config(&self) -> Result<Vec<u8>, EngineError> {
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
        if let Some(form) = &self.form {
            let body = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(form.iter())
                .finish();
            config.push_str(&format!("data = {}\n", quoted(&body)?));
        }
        Ok(config.into_bytes())
    }

    /// Builds the curl command without running it. Output: the command, whose arguments
    /// and environment hold no URL, header or secret, and the config bytes for its stdin.
    pub(super) fn command(&self) -> Result<(Command, Vec<u8>), EngineError> {
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
            .args(["--write-out", "\n%{http_code}"])
            .args(["--config", "-"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok((command, self.config()?))
    }

    /// Sends the request and waits at most 30 s. Output: the reply, or
    /// `github_unreachable` when curl could not finish the exchange.
    pub(super) fn send(&self) -> Result<Reply, EngineError> {
        let (mut command, mut config) = self.command()?;
        let spawned = command.spawn();
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
                .take(BODY_BYTES_MAX as u64 + 16)
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
}

fn split(mut bytes: Vec<u8>) -> Result<Reply, EngineError> {
    let invalid = || failure("github_reply_invalid");
    if bytes.len() > BODY_BYTES_MAX + 4 {
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
    Ok(Reply {
        status,
        body: bytes,
    })
}
