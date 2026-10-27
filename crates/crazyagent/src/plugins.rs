use crate::engine::EngineError;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub mod check;
pub mod skill_folder;

pub const SKILLS_PER_SCOPE_MAX: usize = 100;
pub const SERVERS_PER_SCOPE_MAX: usize = 50;
pub const SERVER_NAME_BYTES_MAX: usize = 64;
pub const COMMAND_BYTES_MAX: usize = 1024;
pub const ARGS_MAX: usize = 32;
pub const ARG_BYTES_MAX: usize = 1024;
pub const URL_BYTES_MAX: usize = 2048;
pub const SECRETS_PER_SERVER_MAX: usize = 16;
pub const SECRET_NAME_BYTES_MAX: usize = 64;
pub const TOOLS_MAX: usize = 256;
pub const TOOL_NAME_BYTES_MAX: usize = 128;

/// Where a skill or MCP server applies: every project, or one project only.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    Global,
    Project { project_id: ProjectId },
}

impl Scope {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Global => "For every project",
            Self::Project { .. } => "This project only",
        }
    }
}

/// How the engine reaches an MCP server: a command on this Mac or a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerSpec {
    Stdio { command: String, args: Vec<String> },
    Http { url: String },
}

fn error(code: &str) -> EngineError {
    EngineError(code.into())
}

fn printable(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

impl ServerSpec {
    /// Checks the command or URL before it is saved.
    /// Input: an untrusted spec. Output: Ok, or a code naming the first problem:
    /// a relative or oversized command, too many or bad arguments, or a URL that is
    /// not https (plain http is allowed only for this Mac) or carries a password.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::Stdio { command, args } => {
                if command.is_empty()
                    || command.len() > COMMAND_BYTES_MAX
                    || !command.starts_with('/')
                    || !printable(command)
                {
                    return Err(error("server_command_invalid"));
                }
                if args.len() > ARGS_MAX
                    || args
                        .iter()
                        .any(|arg| arg.len() > ARG_BYTES_MAX || !printable(arg))
                {
                    return Err(error("server_args_invalid"));
                }
            }
            Self::Http { url } => {
                let invalid = || error("server_url_invalid");
                if url.len() > URL_BYTES_MAX || !printable(url) {
                    return Err(invalid());
                }
                let parsed = url::Url::parse(url).map_err(|_| invalid())?;
                let local = matches!(
                    parsed.host(),
                    Some(url::Host::Ipv4(ip)) if ip.is_loopback()
                ) || matches!(parsed.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
                    || parsed.host_str() == Some("localhost");
                let allowed = match parsed.scheme() {
                    "https" => parsed.host_str().is_some(),
                    "http" => local,
                    _ => false,
                };
                if !allowed || !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }

    /// Fingerprints exactly what the owner approves: the command and arguments, or the URL.
    /// Output: a lowercase hex SHA-256 that changes whenever any of them change.
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_vec(self).unwrap_or_default();
        assert!(!canonical.is_empty());
        let digest = Sha256::digest(&canonical);
        let text: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(text.len(), 64);
        text
    }

    /// Writes what the server runs as one plain line, such as `/usr/bin/uvx mcp-server-x`.
    /// A word that is empty or holds whitespace, a quote or a backslash, the command
    /// included, is single-quoted, so the line reads back as the same words.
    pub fn runs(&self) -> String {
        match self {
            Self::Stdio { command, args } => {
                let mut line = String::new();
                for word in std::iter::once(command).chain(args) {
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    let quoted = word.is_empty()
                        || word.contains(|c: char| c.is_whitespace() || c == '\'' || c == '\\');
                    if quoted {
                        line.push('\'');
                        line.push_str(&word.replace('\'', "'\\''"));
                        line.push('\'');
                    } else {
                        line.push_str(word);
                    }
                }
                line
            }
            Self::Http { url } => url.clone(),
        }
    }
}

/// Checks an MCP server name used as its key in agent configs.
/// Input: the owner's name. Output: Ok for 1 to 64 lowercase letters, digits, - or _,
/// starting with a letter or digit.
pub fn validate_server_name(name: &str) -> Result<(), EngineError> {
    let valid = !name.is_empty()
        && name.len() <= SERVER_NAME_BYTES_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric());
    if !valid {
        return Err(error("server_name_invalid"));
    }
    Ok(())
}

/// Checks a secret's name, used as an environment variable for commands or a header for URLs.
/// Input: the owner's name. Output: Ok for 1 to 64 letters, digits, _ or -, not starting
/// with a digit or -.
pub fn validate_secret_name(name: &str) -> Result<(), EngineError> {
    let valid = !name.is_empty()
        && name.len() <= SECRET_NAME_BYTES_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_');
    if !valid {
        return Err(error("server_secret_name_invalid"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerStatus {
    WaitingForApproval,
    Approved,
    Off,
    CantStart,
}

impl ServerStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::WaitingForApproval => "Waiting for approval",
            Self::Approved => "Approved",
            Self::Off => "Off",
            Self::CantStart => "Can't start",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub scope: Scope,
    pub scope_label: String,
    pub enabled: bool,
    pub on_here: Option<bool>,
    pub source: String,
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretView {
    pub name: String,
    pub saved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerView {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    pub scope_label: String,
    pub spec: ServerSpec,
    pub runs: String,
    pub enabled: bool,
    pub status: ServerStatus,
    pub status_label: String,
    pub problem: Option<String>,
    pub tools: Vec<String>,
    pub checks: u64,
    pub secrets: Vec<SecretView>,
    pub on_here: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveSkill {
    pub name: String,
    pub folder: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveSecret {
    pub name: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveServer {
    pub name: String,
    pub spec: ServerSpec,
    pub secrets: Vec<EffectiveSecret>,
}

/// The exact skills and MCP servers agents launched for one project receive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectivePlugins {
    pub skills: Vec<EffectiveSkill>,
    pub servers: Vec<EffectiveServer>,
}

/// One MCP server secret value read for a launch. The bytes are zeroed when it is dropped
/// and never shown by `Debug`.
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Lends the secret bytes to the launcher, which passes them as an environment value
    /// for a command server or a header value for a URL server.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("SecretValue([REDACTED])")
    }
}

#[derive(Debug)]
pub struct LaunchSecret {
    pub name: String,
    pub value: SecretValue,
}

#[derive(Debug)]
pub struct LaunchServer {
    pub name: String,
    pub spec: ServerSpec,
    pub secrets: Vec<LaunchSecret>,
}

/// The effective set with each server's secret values already read, ready for a harness
/// launch. Skills are the same folders as in `EffectivePlugins`.
#[derive(Debug, Default)]
pub struct LaunchPlugins {
    pub skills: Vec<EffectiveSkill>,
    pub servers: Vec<LaunchServer>,
}

const PROBLEMS: &[(&str, &str)] = &[
    ("skill_folder_not_found", "That folder was not found."),
    ("skill_not_a_folder", "Pick a folder, not a file."),
    (
        "skill_missing_skill_md",
        "This folder has no SKILL.md file.",
    ),
    (
        "skill_frontmatter_missing",
        "SKILL.md needs a name and description at the top.",
    ),
    (
        "skill_name_invalid",
        "The skill name in SKILL.md must be 1 to 64 lowercase letters, digits or hyphens.",
    ),
    (
        "skill_description_invalid",
        "The description in SKILL.md must be 1 to 1024 characters.",
    ),
    (
        "skill_too_many_files",
        "A skill folder can hold up to 200 files.",
    ),
    ("skill_too_large", "A skill folder can be up to 8 MB."),
    (
        "skill_too_deep",
        "A skill folder can be up to 8 folders deep.",
    ),
    (
        "skill_link_outside",
        "This folder links to files outside it.",
    ),
    (
        "skill_file_unsupported",
        "This folder holds something that is not a file or folder.",
    ),
    (
        "skill_unreadable",
        "Some files in this folder can't be read.",
    ),
    (
        "skill_name_taken",
        "A skill with this name already exists here.",
    ),
    ("skill_limit", "You can add up to 100 skills here."),
    ("skill_not_found", "This skill was removed."),
    (
        "server_name_invalid",
        "Use 1 to 64 lowercase letters, digits, - or _ for the name.",
    ),
    (
        "server_name_taken",
        "An MCP server with this name already exists here.",
    ),
    ("server_limit", "You can add up to 50 MCP servers here."),
    ("server_not_found", "This MCP server was removed."),
    (
        "server_command_invalid",
        "Enter the full path to the command, starting with /.",
    ),
    (
        "server_args_invalid",
        "Use up to 32 arguments, each on one line.",
    ),
    (
        "server_url_invalid",
        "Use an https address. Plain http works only for this Mac, and the address can't hold a password.",
    ),
    (
        "server_secret_name_invalid",
        "Use letters, digits, _ or - for the secret name, starting with a letter.",
    ),
    ("server_secret_limit", "A server can have up to 16 secrets."),
    ("server_secret_duplicate", "Each secret needs its own name."),
    ("server_secret_not_found", "This secret was removed."),
    (
        "server_secret_invalid",
        "Enter the secret on one line, up to 8192 characters.",
    ),
    (
        "server_changed",
        "This server changed. Review it again before approving.",
    ),
    (
        "server_check_command_only",
        "Only command servers can be checked from here.",
    ),
    (
        "server_check_busy",
        "Other checks are running. Try again in a moment.",
    ),
    (
        "server_command_not_found",
        "Can't start: command not found.",
    ),
    (
        "server_command_not_runnable",
        "Can't start: this file can't be run.",
    ),
    ("server_cant_start", "The server can't start."),
    (
        "server_cleanup_uncertain",
        "The server's processes could not be confirmed stopped.",
    ),
    ("server_exited", "The server stopped before answering."),
    (
        "server_check_timeout",
        "The server did not answer within 10 seconds.",
    ),
    (
        "server_output_too_large",
        "The server sent too much output.",
    ),
    ("server_refused", "The server refused the request."),
    (
        "server_reply_invalid",
        "The server's answer was not valid MCP.",
    ),
    ("plugin_project_unknown", "This project was not found."),
    ("plugin_not_found", "This item was removed."),
    (
        "plugin_switch_global_only",
        "Only items from global settings can be switched here.",
    ),
    (
        "plugin_store_unavailable",
        "CrazyAgent can't write its plugin folder.",
    ),
    (
        "plugin_store_unsafe",
        "CrazyAgent can't write its plugin folder.",
    ),
    (
        "plugin_secret_store_unavailable",
        "CrazyAgent can't write its plugin folder.",
    ),
];

/// Turns a plugins refusal code into the sentence shown to the owner.
/// Input: an engine rejection code. Output: the plain sentence, or None for other codes.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    PROBLEMS
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, sentence)| *sentence)
}

/// Returns the static form of a plugins refusal code, for clients that keep codes as
/// constants. Input: an engine rejection code. Output: the same code, or None if unknown.
pub fn known_code(code: &str) -> Option<&'static str> {
    PROBLEMS
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(known, _)| *known)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(command: &str, args: &[&str]) -> ServerSpec {
        ServerSpec::Stdio {
            command: command.into(),
            args: args.iter().map(|a| (*a).into()).collect(),
        }
    }

    fn http(url: &str) -> ServerSpec {
        ServerSpec::Http { url: url.into() }
    }

    #[test]
    fn server_specs_are_bounded_and_urls_are_https_or_this_mac() {
        stdio("/opt/homebrew/bin/uvx", &["mcp-server-postgres"])
            .validate()
            .unwrap();
        http("https://mcp.figma.com/mcp").validate().unwrap();
        http("http://127.0.0.1:8931/mcp").validate().unwrap();
        http("http://localhost:3000/mcp").validate().unwrap();
        http("http://[::1]:3000/mcp").validate().unwrap();
        let many: Vec<String> = (0..=ARGS_MAX).map(|i| format!("a{i}")).collect();
        let cases = [
            (stdio("uvx", &[]), "server_command_invalid"),
            (stdio("", &[]), "server_command_invalid"),
            (
                stdio(&format!("/{}", "c".repeat(COMMAND_BYTES_MAX)), &[]),
                "server_command_invalid",
            ),
            (stdio("/bin/x\n", &[]), "server_command_invalid"),
            (
                ServerSpec::Stdio {
                    command: "/bin/x".into(),
                    args: many,
                },
                "server_args_invalid",
            ),
            (stdio("/bin/x", &["a\u{0}b"]), "server_args_invalid"),
            (
                stdio("/bin/x", &[&"a".repeat(ARG_BYTES_MAX + 1)]),
                "server_args_invalid",
            ),
            (http("http://example.com/mcp"), "server_url_invalid"),
            (http("ftp://example.com"), "server_url_invalid"),
            (http("not a url"), "server_url_invalid"),
            (
                http("https://user:pass@example.com/mcp"),
                "server_url_invalid",
            ),
            (
                http(&format!("https://e.com/{}", "p".repeat(URL_BYTES_MAX))),
                "server_url_invalid",
            ),
        ];
        for (spec, code) in cases {
            assert_eq!(spec.validate().unwrap_err().0, code, "{spec:?}");
            assert!(plain_problem(code).is_some(), "{code}");
        }
    }

    #[test]
    fn the_digest_follows_every_part_the_owner_approves() {
        let base = stdio("/bin/x", &["a", "b"]);
        assert_eq!(base.digest().len(), 64);
        assert_eq!(base.digest(), stdio("/bin/x", &["a", "b"]).digest());
        for changed in [
            stdio("/bin/y", &["a", "b"]),
            stdio("/bin/x", &["a"]),
            stdio("/bin/x", &["ab"]),
            stdio("/bin/x", &["b", "a"]),
            http("https://x"),
        ] {
            assert_ne!(base.digest(), changed.digest(), "{changed:?}");
        }
        assert_eq!(base.runs(), "/bin/x a b");
        assert_eq!(stdio("/bin/x", &["two words"]).runs(), "/bin/x 'two words'");
        assert_eq!(
            http("https://mcp.figma.com/mcp").runs(),
            "https://mcp.figma.com/mcp"
        );
    }

    #[test]
    fn names_for_servers_and_secrets_are_plain_keys() {
        for name in ["github", "postgres-local", "a_1", "9lives"] {
            validate_server_name(name).unwrap();
        }
        for name in ["", "GitHub", "-x", "has space", &"n".repeat(65)] {
            assert_eq!(
                validate_server_name(name).unwrap_err().0,
                "server_name_invalid"
            );
        }
        for name in ["GITHUB_TOKEN", "Authorization", "X-Api-Key", "_x"] {
            validate_secret_name(name).unwrap();
        }
        for name in ["", "1ABC", "-x", "A B", "A=B", &"S".repeat(65)] {
            assert_eq!(
                validate_secret_name(name).unwrap_err().0,
                "server_secret_name_invalid"
            );
        }
        assert!(plain_problem("server_name_invalid").is_some());
        assert!(plain_problem("server_secret_name_invalid").is_some());
        assert!(plain_problem("skill_missing_skill_md").is_some());
    }
}
