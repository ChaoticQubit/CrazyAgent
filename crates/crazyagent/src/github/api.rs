use super::http::{Reply, Request};
use super::{
    GitHubCreated, GitHubDeviceCode, GitHubNewRepo, GitHubOwner, GitHubOwners, GitHubRepo,
    GitHubRepoPage, REPO_PAGE_MAX, REPOS_PER_PAGE, SCOPES,
};
use crate::credentials::SecretString;
use crate::engine::EngineError;
use serde::Deserialize;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const JSON: &str = "application/json";
pub(super) const API_JSON: &str = "application/vnd.github+json";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const INTERVAL_MIN: Duration = Duration::from_secs(1);
const INTERVAL_MAX: Duration = Duration::from_secs(60);
const EXPIRY_MAX: Duration = Duration::from_secs(1800);
const UNREACHABLE_MAX: u32 = 5;
const GH_TIME: Duration = Duration::from_secs(10);
const LOGIN_BYTES_MAX: usize = 39;
const REPO_NAME_SHOWN_MAX: usize = 201;
const BRANCH_SHOWN_MAX: usize = 255;
const ORGS_PER_PAGE: usize = 100;
const DESCRIPTION_CHARS_MAX: usize = 350;
const NODE_ID_BYTES_MAX: usize = 100;
const READ_ORG: [&str; 3] = ["read:org", "write:org", "admin:org"];

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

    pub(super) fn api(&self, path: &str) -> Result<url::Url, EngineError> {
        self.api
            .join(path)
            .map_err(|_| failure("github_request_invalid"))
    }

    fn web(&self, path: &str) -> Result<url::Url, EngineError> {
        self.web
            .join(path)
            .map_err(|_| failure("github_request_invalid"))
    }

    pub(super) fn request<'a>(
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

pub(super) fn json<T: for<'de> Deserialize<'de>>(reply: &Reply) -> Result<T, EngineError> {
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

/// Shows text from GitHub the way the owner can read it: control and hidden characters,
/// line breaks and tabs included, escaped, then cut to at most `max` bytes on a character
/// boundary. Inputs: the text and the byte bound. Output: the visible text.
pub(super) fn shown(value: &str, max: usize) -> String {
    let flat = value.replace('\n', "\\u000a").replace('\t', "\\u0009");
    let mut text = crate::memory::visible_text(&flat);
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    assert!(text.len() <= max);
    assert!(!text.chars().any(char::is_control));
    text
}

/// Asks GitHub for a device code with the scopes in `SCOPES`.
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
    user(endpoints, token).map(|(login, _)| login)
}

/// Reads the account the token belongs to: its login, which also proves GitHub accepts
/// the token, and the scopes GitHub names for a classic token. Inputs: endpoints and
/// token. Output: the login and the scopes, None when GitHub names none, or
/// `github_token_refused`.
fn user(
    endpoints: &Endpoints,
    token: &SecretString,
) -> Result<(String, Option<Vec<String>>), EngineError> {
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
    Ok((account_name(&user.login)?, reply.scopes))
}

/// Checks an account name GitHub sent: at most 39 letters, digits and hyphens.
/// Input: the name. Output: the name, or `github_reply_invalid`.
fn account_name(value: &str) -> Result<String, EngineError> {
    let name = short_text(value, LOGIN_BYTES_MAX)?;
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(failure("github_reply_invalid"));
    }
    Ok(name)
}

/// Tells whether a token's scopes cover a need, a broader scope included, such as
/// `admin:org` for `read:org`. Inputs: the scopes and those that cover the need.
/// Output: true when one of them is held.
fn grants(scopes: &[String], covering: &[&str]) -> bool {
    scopes
        .iter()
        .any(|scope| covering.contains(&scope.as_str()))
}

/// Lists the accounts a new repository can go under: the own account first, then each
/// active admin or member organization from one page of at most 100. When the scopes
/// lack `read:org`, or GitHub refuses the list, only the own account comes back with the
/// sentence that says which. Inputs: endpoints and token. Output: the accounts.
pub fn owners(endpoints: &Endpoints, token: &SecretString) -> Result<GitHubOwners, EngineError> {
    #[derive(Deserialize)]
    struct Membership {
        state: String,
        role: String,
        organization: Organization,
    }
    #[derive(Deserialize)]
    struct Organization {
        login: String,
    }
    let (login, scopes) = user(endpoints, token)?;
    let mut owners = vec![GitHubOwner {
        login,
        personal: true,
    }];
    let without_orgs = |owners: Vec<GitHubOwner>, code: &str| GitHubOwners {
        owners,
        problem: Some(super::problem(code)),
    };
    if scopes
        .as_deref()
        .is_some_and(|scopes| !grants(scopes, &READ_ORG))
    {
        return Ok(without_orgs(owners, "github_scope_missing_read_org"));
    }
    let mut url = endpoints.api("user/memberships/orgs")?;
    url.query_pairs_mut()
        .append_pair("state", "active")
        .append_pair("per_page", &ORGS_PER_PAGE.to_string());
    let reply = endpoints.request(url, API_JSON, Some(token), None).send()?;
    match reply.status {
        200 => {}
        403 if scopes.is_none() => {
            return Ok(without_orgs(owners, "github_scope_missing_read_org"));
        }
        403 => return Ok(without_orgs(owners, "github_orgs_refused")),
        401 => return Err(failure("github_token_refused")),
        _ => return Err(failure("github_unreachable")),
    }
    let listed: Vec<Membership> = json(&reply)?;
    if listed.len() > ORGS_PER_PAGE {
        return Err(failure("github_reply_invalid"));
    }
    for membership in listed {
        let member =
            membership.state == "active" && matches!(membership.role.as_str(), "admin" | "member");
        let Ok(org) = account_name(&membership.organization.login) else {
            continue;
        };
        if member
            && !owners
                .iter()
                .any(|owner| owner.login.eq_ignore_ascii_case(&org))
        {
            owners.push(GitHubOwner {
                login: org,
                personal: false,
            });
        }
    }
    assert!(owners[0].personal && owners[1..].iter().all(|owner| !owner.personal));
    assert!(owners.len() <= ORGS_PER_PAGE + 1);
    Ok(GitHubOwners {
        owners,
        problem: None,
    })
}

/// Checks the account and name of a repository to make before any request: an account
/// of at most 39 letters, digits and hyphens not starting with one, and a name the clone
/// accepts. Inputs: the account and the name. Output: `owner/name`, or
/// `github_repo_name_invalid`.
fn repo_name(owner: &str, name: &str) -> Result<String, EngineError> {
    let owner_valid = !owner.is_empty()
        && owner.len() <= LOGIN_BYTES_MAX
        && !owner.starts_with('-')
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !owner_valid {
        return Err(failure("github_repo_name_invalid"));
    }
    let full_name = format!("{owner}/{name}");
    super::clone::full_name(&full_name)?;
    Ok(full_name)
}

/// Checks whether `owner/name` is free on GitHub: 404 is free, while a repository or the
/// redirect a rename leaves is taken. The name is checked before any request.
/// Inputs: endpoints, token, the account and the name. Output: true when free, or
/// `github_repo_name_invalid`, `github_token_refused`.
pub fn name_free(
    endpoints: &Endpoints,
    token: &SecretString,
    owner: &str,
    name: &str,
) -> Result<bool, EngineError> {
    let full_name = repo_name(owner, name)?;
    let reply = endpoints
        .request(
            endpoints.api(&format!("repos/{full_name}"))?,
            API_JSON,
            Some(token),
            None,
        )
        .send()?;
    match reply.status {
        404 => Ok(true),
        200 | 301 => Ok(false),
        401 | 403 => Err(failure("github_token_refused")),
        _ => Err(failure("github_unreachable")),
    }
}

/// Makes an empty repository with CrazyAgent's standard settings under the own account
/// or an organization, checking the name and description before any request. A reply
/// with another name or visibility than asked is refused so nothing is pushed into it.
/// Inputs: endpoints, token and the repository. Output: what GitHub made, or the
/// refusal code.
pub fn create(
    endpoints: &Endpoints,
    token: &SecretString,
    repo: &GitHubNewRepo,
) -> Result<GitHubCreated, EngineError> {
    #[derive(Deserialize)]
    struct Made {
        full_name: String,
        node_id: String,
        private: bool,
    }
    #[derive(Deserialize)]
    struct Refused {
        #[serde(default)]
        errors: Vec<Problem>,
    }
    #[derive(Deserialize)]
    struct Problem {
        #[serde(default)]
        field: String,
        #[serde(default)]
        message: String,
    }
    let full_name = repo_name(&repo.owner, &repo.name)?;
    if repo.description.chars().count() > DESCRIPTION_CHARS_MAX
        || repo.description.chars().any(char::is_control)
    {
        return Err(failure("github_repo_description_invalid"));
    }
    let (login, _) = user(endpoints, token)?;
    let path = if repo.owner.eq_ignore_ascii_case(&login) {
        "user/repos".to_owned()
    } else {
        format!("orgs/{}/repos", repo.owner)
    };
    let body = serde_json::json!({
        "name": repo.name,
        "description": repo.description,
        "private": repo.private,
        "has_issues": true,
        "has_wiki": false,
        "has_projects": false,
        "allow_squash_merge": true,
        "allow_merge_commit": false,
        "allow_rebase_merge": false,
        "delete_branch_on_merge": true,
        "allow_auto_merge": false,
        "auto_init": false,
    });
    let reply = endpoints
        .request(endpoints.api(&path)?, API_JSON, Some(token), None)
        .send_json(&body.to_string())?;
    match reply.status {
        201 => {}
        422 => {
            let taken = json::<Refused>(&reply).is_ok_and(|refused| {
                refused.errors.iter().any(|problem| {
                    problem.field == "name" && problem.message.contains("already exists")
                })
            });
            return Err(failure(if taken {
                "github_repo_name_taken"
            } else {
                "github_repo_create_refused"
            }));
        }
        401 => return Err(failure("github_token_refused")),
        403 | 404 => return Err(failure("github_repo_create_refused")),
        _ => return Err(failure("github_unreachable")),
    }
    let made: Made = json(&reply)?;
    if !made.full_name.eq_ignore_ascii_case(&full_name) {
        return Err(failure("github_reply_invalid"));
    }
    if made.private != repo.private {
        return Err(failure("github_repo_visibility_mismatch"));
    }
    Ok(GitHubCreated {
        full_name: made.full_name,
        node_id: short_text(&made.node_id, NODE_ID_BYTES_MAX)?,
        private: made.private,
    })
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
    let repos: Vec<GitHubRepo> = listed
        .into_iter()
        .map(|repo| {
            let refused = super::clone::full_name(&repo.full_name).is_err();
            let branch = repo
                .default_branch
                .as_deref()
                .filter(|branch| !branch.is_empty());
            GitHubRepo {
                full_name: if refused {
                    shown(&repo.full_name, REPO_NAME_SHOWN_MAX)
                } else {
                    repo.full_name
                },
                private: repo.private,
                default_branch: shown(branch.unwrap_or("main"), BRANCH_SHOWN_MAX),
                size_kb: repo.size,
                refused: refused.then(|| "github_repo_name_unsupported".to_owned()),
            }
        })
        .collect();
    assert!(repos.len() <= REPOS_PER_PAGE as usize);
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
