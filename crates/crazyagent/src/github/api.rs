use super::http::{Reply, Request};
use super::{GitHubDeviceCode, GitHubRepo, GitHubRepoPage, REPO_PAGE_MAX, REPOS_PER_PAGE, SCOPES};
use crate::credentials::SecretString;
use crate::engine::EngineError;
use serde::Deserialize;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const JSON: &str = "application/json";
const API_JSON: &str = "application/vnd.github+json";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const INTERVAL_MIN: Duration = Duration::from_secs(1);
const INTERVAL_MAX: Duration = Duration::from_secs(60);
const EXPIRY_MAX: Duration = Duration::from_secs(1800);
const UNREACHABLE_MAX: u32 = 5;
const GH_TIME: Duration = Duration::from_secs(10);
const LOGIN_BYTES_MAX: usize = 39;

/// Where the engine talks to GitHub: the REST API and the web host that serves the device
/// flow and git. `allow_http` is true only for a loopback server on 127.0.0.1.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub(super) api: url::Url,
    pub(super) web: url::Url,
    pub(super) allow_http: bool,
}

impl Endpoints {
    /// The real GitHub: `https://api.github.com/` and `https://github.com/`.
    pub fn github() -> Self {
        Self {
            api: url::Url::parse("https://api.github.com/").expect("fixed GitHub API address"),
            web: url::Url::parse("https://github.com/").expect("fixed GitHub web address"),
            allow_http: false,
        }
    }

    /// A GitHub stand-in served over plain HTTP on 127.0.0.1 at `port`, for both hosts.
    pub fn loopback(port: u16) -> Self {
        let base = url::Url::parse(&format!("http://127.0.0.1:{port}/")).expect("loopback address");
        Self {
            api: base.clone(),
            web: base,
            allow_http: true,
        }
    }

    pub(super) fn web_host(&self) -> &str {
        self.web.host_str().expect("web address has a host")
    }

    fn api(&self, path: &str) -> Result<url::Url, EngineError> {
        self.api
            .join(path)
            .map_err(|_| failure("github_request_invalid"))
    }

    fn web(&self, path: &str) -> Result<url::Url, EngineError> {
        self.web
            .join(path)
            .map_err(|_| failure("github_request_invalid"))
    }

    fn request<'a>(
        &self,
        url: url::Url,
        accept: &'a str,
        token: Option<&'a SecretString>,
        form: Option<Vec<(&'a str, &'a str)>>,
    ) -> Request<'a> {
        Request {
            url,
            accept,
            token,
            form,
            headers: Vec::new(),
            allow_http: self.allow_http,
        }
    }
}

/// A device sign-in GitHub has started: what the owner sees, and what the engine keeps
/// to finish it. The device code stays in memory only.
pub struct DeviceStart {
    pub code: GitHubDeviceCode,
    device_code: SecretString,
    interval: Duration,
    expires: Instant,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn json<T: for<'de> Deserialize<'de>>(reply: &Reply) -> Result<T, EngineError> {
    serde_json::from_slice(&reply.body).map_err(|_| failure("github_reply_invalid"))
}

fn short_text(value: &str, max: usize) -> Result<String, EngineError> {
    if value.is_empty()
        || value.len() > max
        || value.chars().any(|character| character.is_control())
    {
        return Err(failure("github_reply_invalid"));
    }
    Ok(value.to_owned())
}

/// Asks GitHub for a device code with the `repo read:user` scopes.
/// Inputs: endpoints and the OAuth App's public client id. Output: the started sign-in, or
/// `github_device_flow_refused` when GitHub does not offer the device flow for this app.
pub fn start_device(endpoints: &Endpoints, client_id: &str) -> Result<DeviceStart, EngineError> {
    #[derive(Deserialize)]
    struct Started {
        device_code: String,
        user_code: String,
        verification_uri: String,
        expires_in: u64,
        interval: Option<u64>,
    }
    let reply = endpoints
        .request(
            endpoints.web("login/device/code")?,
            JSON,
            None,
            Some(vec![("client_id", client_id), ("scope", SCOPES)]),
        )
        .send()?;
    if reply.status != 200 {
        return Err(failure("github_device_flow_refused"));
    }
    if let Ok(refused) = json::<Refusal>(&reply) {
        return Err(failure(refused.code()));
    }
    let started: Started = json(&reply)?;
    let user_code = short_text(&started.user_code, 32)?;
    let verification =
        url::Url::parse(&started.verification_uri).map_err(|_| failure("github_reply_invalid"))?;
    let same_site = verification.host_str() == Some(endpoints.web_host())
        && (verification.scheme() == "https"
            || (endpoints.allow_http && verification.scheme() == "http"));
    if !same_site || started.expires_in == 0 {
        return Err(failure("github_reply_invalid"));
    }
    let expires_in = Duration::from_secs(started.expires_in).min(EXPIRY_MAX);
    let device_code = SecretString::new(started.device_code.into_bytes())
        .map_err(|_| failure("github_reply_invalid"))?;
    Ok(DeviceStart {
        code: GitHubDeviceCode {
            user_code,
            verification_uri: verification.to_string(),
            expires_s: expires_in.as_secs(),
        },
        device_code,
        interval: Duration::from_secs(started.interval.unwrap_or(5))
            .clamp(INTERVAL_MIN, INTERVAL_MAX),
        expires: Instant::now() + expires_in,
    })
}

#[derive(Deserialize)]
struct Refusal {
    error: String,
    interval: Option<u64>,
}

impl Refusal {
    fn code(&self) -> &'static str {
        match self.error.as_str() {
            "authorization_pending" => "github_sign_in_pending",
            "slow_down" => "github_sign_in_slow_down",
            "expired_token" => "github_sign_in_expired",
            "access_denied" => "github_sign_in_denied",
            "device_flow_disabled" => "github_device_flow_refused",
            "incorrect_client_credentials" | "unsupported_grant_type" => "github_client_id_refused",
            _ => "github_sign_in_failed",
        }
    }
}

/// Polls GitHub until the owner approves the code, at GitHub's interval; `slow_down`
/// raises the interval to the one GitHub sends, or by 5 s when it sends none.
/// Inputs: endpoints, client id, the started sign-in and a cancel flag.
/// Output: the access token, or the plain refusal code.
pub fn finish_device(
    endpoints: &Endpoints,
    client_id: &str,
    start: DeviceStart,
    cancel: &AtomicBool,
) -> Result<SecretString, EngineError> {
    #[derive(Deserialize)]
    struct Granted {
        access_token: String,
    }
    let mut interval = start.interval;
    let mut unreachable = 0;
    loop {
        let wake = Instant::now() + interval;
        while Instant::now() < wake {
            if cancel.load(Ordering::Acquire) {
                return Err(failure("github_sign_in_cancelled"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if Instant::now() >= start.expires {
            return Err(failure("github_sign_in_expired"));
        }
        let form = vec![
            ("client_id", client_id),
            ("device_code", start.device_code.expose()),
            ("grant_type", DEVICE_GRANT),
        ];
        let reply = match endpoints
            .request(
                endpoints.web("login/oauth/access_token")?,
                JSON,
                None,
                Some(form),
            )
            .send()
        {
            Ok(reply) => reply,
            Err(error) if error.0 == "github_unreachable" && unreachable < UNREACHABLE_MAX => {
                unreachable += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        unreachable = 0;
        if let Ok(refusal) = json::<Refusal>(&reply) {
            match refusal.code() {
                "github_sign_in_pending" => continue,
                "github_sign_in_slow_down" => {
                    interval = refusal
                        .interval
                        .map(Duration::from_secs)
                        .unwrap_or(interval + Duration::from_secs(5))
                        .clamp(interval, INTERVAL_MAX);
                    continue;
                }
                code => return Err(failure(code)),
            }
        }
        if reply.status != 200 {
            return Err(failure("github_sign_in_failed"));
        }
        let granted: Granted = json(&reply)?;
        return SecretString::new(granted.access_token.into_bytes())
            .map_err(|_| failure("github_reply_invalid"));
    }
}

/// Reads the login name the token belongs to, which also proves GitHub accepts it.
/// Output: the login, or `github_token_refused` for a token GitHub does not accept.
pub fn login(endpoints: &Endpoints, token: &SecretString) -> Result<String, EngineError> {
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    let reply = endpoints
        .request(endpoints.api("user")?, API_JSON, Some(token), None)
        .send()?;
    match reply.status {
        200 => {}
        401 | 403 => return Err(failure("github_token_refused")),
        _ => return Err(failure("github_unreachable")),
    }
    let user: User = json(&reply)?;
    let login = short_text(&user.login, LOGIN_BYTES_MAX)?;
    if !login
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(failure("github_reply_invalid"));
    }
    Ok(login)
}

/// Lists one page of the repositories the token can read, most recently pushed first.
/// Inputs: endpoints, token and a page from 1 to 100. Output: the page and the next number.
pub fn repos(
    endpoints: &Endpoints,
    token: &SecretString,
    page: u32,
) -> Result<GitHubRepoPage, EngineError> {
    #[derive(Deserialize)]
    struct Listed {
        full_name: String,
        private: bool,
        default_branch: Option<String>,
        size: u64,
    }
    if !(1..=REPO_PAGE_MAX).contains(&page) {
        return Err(failure("github_repo_page_invalid"));
    }
    let mut url = endpoints.api("user/repos")?;
    url.query_pairs_mut()
        .append_pair("per_page", &REPOS_PER_PAGE.to_string())
        .append_pair("page", &page.to_string())
        .append_pair("sort", "pushed")
        .append_pair("affiliation", "owner,collaborator,organization_member");
    let reply = endpoints.request(url, API_JSON, Some(token), None).send()?;
    match reply.status {
        200 => {}
        401 | 403 => return Err(failure("github_token_refused")),
        _ => return Err(failure("github_unreachable")),
    }
    let listed: Vec<Listed> = json(&reply)?;
    if listed.len() > REPOS_PER_PAGE as usize {
        return Err(failure("github_reply_invalid"));
    }
    let full = listed.len() == REPOS_PER_PAGE as usize;
    let repos = listed
        .into_iter()
        .map(|repo| {
            super::clone::full_name(&repo.full_name)?;
            Ok(GitHubRepo {
                full_name: repo.full_name,
                private: repo.private,
                default_branch: short_text(repo.default_branch.as_deref().unwrap_or("main"), 255)?,
                size_kb: repo.size,
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    Ok(GitHubRepoPage {
        repos,
        next: (full && page < REPO_PAGE_MAX).then_some(page + 1),
    })
}

/// Finds a GitHub CLI installed by Homebrew or the system. Output: its path, if any.
pub fn find_cli() -> Option<&'static Path> {
    ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
}

/// Takes the token of a signed-in GitHub CLI with `gh auth token`. The output goes straight
/// into memory and is never logged. Inputs: the `gh` path and the environment it needs
/// (at least `HOME`). Output: the token, or `github_cli_not_signed_in`.
pub fn cli_token(gh: &Path, env: &[(&str, &std::ffi::OsStr)]) -> Result<SecretString, EngineError> {
    let unavailable = || failure("github_cli_not_signed_in");
    let mut child = crate::spawn_gate::spawn(
        Command::new(gh)
            .args(["auth", "token", "--hostname", "github.com"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .envs(env.iter().copied())
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|_| failure("github_cli_missing"))?;
    let stdout = child.stdout.take().expect("gh stdout is piped");
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout.take(2048).read_to_end(&mut bytes);
        (read, bytes)
    });
    let deadline = Instant::now() + GH_TIME;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| unavailable())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(unavailable());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (read, mut bytes) = reader.join().map_err(|_| unavailable())?;
    if read.is_err() || !status.success() {
        bytes.fill(0);
        return Err(unavailable());
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes.pop();
    }
    SecretString::new(bytes).map_err(|_| unavailable())
}
