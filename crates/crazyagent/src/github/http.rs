use crate::engine::EngineError;
use kernel_sandbox::egress::{BODY_BYTES_MAX, ExchangeError, exchange};
use kernel_security::secret::Piece::{Escaped, Form, Plain};
use kernel_security::secret::SecretString;
use std::process::{Command, Stdio};

const CURL: &str = "/usr/bin/curl";
const WRITE_OUT: &str = "\n%header{x-oauth-scopes}\n%{http_code}";
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

/// What came back from [`exchange`], read by the GitHub API and rules calls.
pub(crate) use kernel_sandbox::egress::Reply;

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

impl Request<'_> {
    /// Builds curl's config: the URL, headers, token, and either the form body or a
    /// method with its JSON body, never both; a bodyless PUT, POST or PATCH sends an empty
    /// body so it says its length is zero. Input: the method and body, none for a GET or a
    /// form. Output: the config bytes, or `github_request_invalid` for a control character.
    fn config(&self, body: Option<(Verb, Option<&str>)>) -> Result<Vec<u8>, EngineError> {
        assert!(body.is_none() || self.form.is_none());
        let mut pieces = vec![
            Plain("url = \""),
            Escaped(self.url.as_str()),
            Plain("\"\nheader = \"Accept: "),
            Escaped(self.accept),
            Plain("\"\nuser-agent = \""),
            Escaped(USER_AGENT),
            Plain("\"\n"),
        ];
        if self.url.host_str() == Some("api.github.com") || self.allow_http {
            pieces.push(Plain("header = \"X-GitHub-Api-Version: 2022-11-28\"\n"));
        }
        if let Some(token) = self.token {
            pieces.extend([
                Plain("header = \"Authorization: Bearer "),
                Escaped(token.expose()),
                Plain("\"\n"),
            ]);
        }
        for (name, value) in &self.headers {
            pieces.extend([
                Plain("header = \""),
                Escaped(name),
                Plain(": "),
                Escaped(value),
                Plain("\"\n"),
            ]);
        }
        if let Some(form) = &self.form {
            pieces.push(Plain("data = \""));
            for (index, (name, value)) in form.iter().enumerate() {
                if index > 0 {
                    pieces.push(Plain("&"));
                }
                pieces.extend([Form(name), Plain("="), Form(value)]);
            }
            pieces.push(Plain("\"\n"));
        }
        if let Some((verb, json)) = body {
            pieces.extend([Plain("request = \""), Plain(verb.name()), Plain("\"\n")]);
            match json {
                Some(json) => pieces.extend([
                    Plain("header = \"Content-Type: application/json\"\ndata-raw = \""),
                    Escaped(json),
                    Plain("\"\n"),
                ]),
                None if verb != Verb::Delete => pieces.push(Plain("data-raw = \"\"\n")),
                None => {}
            }
        }
        kernel_security::secret::sized(&pieces).ok_or_else(|| failure("github_request_invalid"))
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
        exchange(command, config).map_err(code)
    }

    /// POSTs a JSON body and waits at most 30 s, as `send` does. Input: the body.
    /// Output: the reply, or `github_unreachable` when curl could not finish the exchange.
    pub(crate) fn send_json(&self, json: &str) -> Result<Reply, EngineError> {
        let (command, config) = self.json_command(json)?;
        exchange(command, config).map_err(code)
    }

    /// Sends a request with a method and an optional JSON body and waits at most 30 s, as
    /// `send` does. Inputs: the method and the body. Output: the reply, or
    /// `github_unreachable` when curl could not finish the exchange.
    pub(crate) fn send_body(&self, verb: Verb, json: Option<&str>) -> Result<Reply, EngineError> {
        let (command, config) = self.command_with(Some((verb, json)))?;
        exchange(command, config).map_err(code)
    }
}

/// The engine code of an exchange that gave no reply. Input: the kernel's reason. Output:
/// `github_unreachable` or `github_reply_invalid`.
fn code(error: ExchangeError) -> EngineError {
    failure(match error {
        ExchangeError::Unreachable => "github_unreachable",
        ExchangeError::ReplyInvalid => "github_reply_invalid",
    })
}

#[cfg(test)]
mod tests;
