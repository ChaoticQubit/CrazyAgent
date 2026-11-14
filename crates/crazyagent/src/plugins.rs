use crate::engine::EngineError;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub mod check;
pub mod hook_folder;
pub mod skill_folder;

pub const SKILLS_PER_SCOPE_MAX: usize = 100;
pub const SERVERS_PER_SCOPE_MAX: usize = 50;
pub const SERVER_NAME_BYTES_MAX: usize = 64;
pub const COMMAND_BYTES_MAX: usize = 1024;
pub const ARGS_MAX: usize = 32;
pub const ARG_BYTES_MAX: usize = 1024;
pub const COMMAND_LINE_BYTES_MAX: usize = 4096;
pub const URL_BYTES_MAX: usize = 2048;
pub const SECRETS_PER_SERVER_MAX: usize = 16;
pub const SECRET_NAME_BYTES_MAX: usize = 64;
pub const TOOLS_MAX: usize = 256;
pub const TOOL_NAME_BYTES_MAX: usize = 128;
pub const TOOL_NAMES_BYTES_MAX: usize = 12 * 1024;
pub const HOOKS_PER_SCOPE_MAX: usize = 100;
pub const HOOKS_PER_PLUGIN_MAX: usize = 64;

/// Where a skill, MCP server or hook applies: every project, or one project only.
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
    /// a relative or oversized command, too many or bad arguments, a command and
    /// arguments over `COMMAND_LINE_BYTES_MAX` together, or a URL that is
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
                    || command.len() + args.iter().map(String::len).sum::<usize>()
                        > COMMAND_LINE_BYTES_MAX
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
/// Checks the secrets a server is added with: each a valid secret name, none named twice
/// and at most `SECRETS_PER_SERVER_MAX`. Input: the names. Output: Ok, or the name's own
/// refusal, `server_secret_duplicate` or `server_secret_limit`.
pub fn validate_secret_names(names: &[&str]) -> Result<(), EngineError> {
    for (index, name) in names.iter().enumerate() {
        validate_secret_name(name)?;
        if names[..index].contains(name) {
            return Err(error("server_secret_duplicate"));
        }
    }
    if names.len() > SECRETS_PER_SERVER_MAX {
        return Err(error("server_secret_limit"));
    }
    Ok(())
}

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
    pub line: String,
    pub scope: Scope,
    pub scope_label: String,
    pub enabled: bool,
    pub on_here: Option<bool>,
    pub source: String,
    pub origin: String,
    pub folder: String,
    pub version: u64,
    pub refused: Option<String>,
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
    pub version: u64,
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

/// One tool with its switches, as Settings lists it: a CrazyAgent built-in tool (`server_id`
/// none, `server` the CrazyAgent server name) or one checked tool of an MCP server, keyed by
/// the server's name as agents see it. `global` is the every-project switch, On when never
/// set; `here` is the project's own stored switch, none in global settings or when the project
/// follows the global one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolView {
    pub server: String,
    pub server_id: Option<String>,
    pub tool: String,
    pub description: String,
    pub builtin: bool,
    pub global: crate::coord::tool_switches::Switch,
    pub here: Option<crate::coord::tool_switches::Switch>,
}

/// One hook as Settings lists it: the plugin it came from, the Claude Code event and
/// matcher it runs on, its command, where it applies and whether it is on. `follows` names
/// the skill or MCP server the hook came with; the hook runs only while that item is on.
/// `version` is the hook's owner change version a project switch names, as for skills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookView {
    pub id: String,
    pub name: String,
    pub plugin: String,
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    pub scope: Scope,
    pub scope_label: String,
    pub enabled: bool,
    pub on_here: Option<bool>,
    pub follows: Option<String>,
    pub source: String,
    pub version: u64,
}

/// One hook an agent launch writes into its generated plugin: the plugin's stored folder
/// copy its command runs from, the event, matcher, command and timeout in seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveHook {
    pub plugin: String,
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    pub timeout: Option<u32>,
    pub folder: PathBuf,
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

/// Why a launch left the default pack's context-mode server out rather than run its npx
/// spec: the engine's shared install is still being set up, setting it up failed or cannot
/// run, the installed files did not match the pinned hashes, or the Node.js beside npx is
/// older than context-mode needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextModeOff {
    SettingUp,
    Failed,
    Mismatch,
    NodeTooOld,
}

/// The effective set with each server's secret values already read, ready for a harness
/// launch. Skills are the same folders as in `EffectivePlugins`. `context_mode_off` says why
/// the default pack's context-mode server was left out, none when it was not.
#[derive(Debug, Default)]
pub struct LaunchPlugins {
    pub skills: Vec<EffectiveSkill>,
    pub servers: Vec<LaunchServer>,
    pub context_mode_off: Option<ContextModeOff>,
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
        "skill_description_hidden",
        "The description in SKILL.md has characters you cannot see, so agents could read something you did not approve.",
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
        "skill_text_too_large",
        "This SKILL.md is too large to show here.",
    ),
    (
        "skill_page_invalid",
        "This skill changed while it was read. Open it again.",
    ),
    (
        "skill_import_too_large",
        "A skill brought in from your skills folders can be up to 2 MB.",
    ),
    (
        "skill_import_name_changed",
        "This folder now holds a different skill. Look for your skills again.",
    ),
    (
        "skill_import_home_unknown",
        "Your home folder is unknown, so your skills can't be found.",
    ),
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
        "Use up to 32 arguments, each on one line, with at most 4 KB for the command and its arguments together.",
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
        "server_secret_too_short",
        "Use a secret of at least 8 characters. A shorter one is part of an agent's launch command, so CrazyAgent cannot keep it hidden.",
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
        crate::sandbox::OWNER_PORTS_UNREADABLE,
        "CrazyAgent can't read its settings file, so it did not run the check. Fix the file and check again.",
    ),
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
        "server_setting_up",
        "context-mode is still being set up. Check again in a minute.",
    ),
    (
        "server_install_failed",
        "Setting up context-mode failed. Check again in 10 minutes.",
    ),
    (
        "server_npm_missing",
        "context-mode needs npm and node in the same folder as npx. Install Node.js, then check again.",
    ),
    (
        "server_version_unpinned",
        "This context-mode version is not the one the default pack pins, so CrazyAgent does not install it. Use the default pack's version.",
    ),
    (
        "server_installs_busy",
        "Too many installs are running. Check again in a minute.",
    ),
    (
        "server_node_too_old",
        "context-mode needs Node.js 22.5 or newer. Update Node.js, then check again.",
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
        "stale_revision",
        "This changed in another window, so yours was not saved. The list shows it now.",
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
    ("hooks_none_found", "This folder has no Claude Code hooks."),
    (
        "hooks_invalid",
        "The hooks file in this folder is not valid hooks JSON.",
    ),
    ("hooks_too_many", "This plugin has more than 64 hooks."),
    (
        "hooks_too_large",
        "The files these hooks run are larger than 16 MB or 2048 files.",
    ),
    (
        "hook_plugin_name_invalid",
        "The plugin name must be 1 to 32 letters, digits, ., - or _.",
    ),
    (
        "hooks_plugin_manifest_missing",
        "This folder has hooks but no .claude-plugin/plugin.json naming the plugin.",
    ),
    (
        "hooks_already_added",
        "These hooks were already added here.",
    ),
    ("hook_limit", "There are already 100 hooks here."),
    (
        "tool_unknown",
        "That tool is not on this server. Check the server again to list its tools.",
    ),
    (
        "tool_delete_builtin_only",
        "Only a CrazyAgent tool can be deleted. Turn an MCP server's tool off instead.",
    ),
    (
        "coord_tool_invalid",
        "This tool's name has characters a switch can't hold. Only letters, digits, _, - and . work.",
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

/// The start of a recorded check problem that the agent boundary refused before the
/// server ran. The boundary's own plain reason follows it, such as the kept port and the
/// range Linux hands out by itself (REL220-F3).
pub const CHECK_REFUSED: &str = "server_check_refused:";

/// The longest boundary reason a recorded check problem carries.
const CHECK_REASON_BYTES_MAX: usize = 1024;

/// Records a boundary refusal of a server check with the boundary's reason. Input: the plain
/// reason, never empty. Output: the problem to record, which [`check_problem`] shows.
pub fn check_refused(reason: &str) -> String {
    assert!(!reason.is_empty());
    assert!(reason.len() <= CHECK_REASON_BYTES_MAX);
    let problem = format!("{CHECK_REFUSED}{reason}");
    assert!(check_problem(&problem).contains(reason.trim_end_matches('.')));
    problem
}

/// The start of a recorded check problem for a failed context-mode shared install, followed
/// by the minutes from the check until a check or an agent launch may try the install again.
const INSTALL_RETRY: &str = "server_install_failed:";

/// The most minutes a failed context-mode shared install holds the next one back; the
/// install's own backoff, rounded up, is held to this at compile time.
pub const INSTALL_RETRY_MINUTES_MAX: u64 = 10;

/// Records a check that found the context-mode shared install held back after a failure.
/// Input: the minutes until it may be tried again, 1 to [`INSTALL_RETRY_MINUTES_MAX`].
/// Output: the problem to record, which [`check_problem`] shows with those minutes.
pub fn install_failed(minutes: u64) -> String {
    assert!((1..=INSTALL_RETRY_MINUTES_MAX).contains(&minutes));
    let problem = format!("{INSTALL_RETRY}{minutes}");
    assert!(check_problem(&problem).contains(&format!(" {minutes} minute")));
    problem
}

/// Turns a recorded server check problem into the sentence shown to the owner: a boundary
/// refusal ([`check_refused`]) shows the boundary's own reason, a failed shared install
/// ([`install_failed`]) the minutes until the retry, a plugins code its plain sentence, and
/// anything else reads as a server that can't start. Input: the recorded problem. Output:
/// the sentence.
pub fn check_problem(problem: &str) -> String {
    if let Some(left) = problem.strip_prefix(INSTALL_RETRY) {
        let minutes = left
            .parse::<u64>()
            .ok()
            .filter(|minutes| (1..=INSTALL_RETRY_MINUTES_MAX).contains(minutes));
        return match minutes {
            Some(minutes) => format!(
                "Setting up context-mode failed. Check again in {minutes} {}.",
                if minutes == 1 { "minute" } else { "minutes" }
            ),
            None => plain_problem("server_install_failed")
                .unwrap_or("The server can't start.")
                .to_owned(),
        };
    }
    match problem.strip_prefix(CHECK_REFUSED) {
        Some(reason) if !reason.is_empty() => format!(
            "CrazyAgent did not run the check: {}.",
            reason.trim_end_matches('.')
        ),
        _ => plain_problem(problem)
            .unwrap_or("The server can't start.")
            .to_owned(),
    }
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

    #[test]
    fn a_check_the_boundary_refused_shows_the_boundary_reason() {
        let reason = "port 40000 is kept free for the owner's apps, a local model or a model \
             proxy, and this Linux hands out ports 32768 to 60999 by itself";
        assert_eq!(
            check_problem(&check_refused(reason)),
            format!("CrazyAgent did not run the check: {reason}.")
        );
        assert_eq!(
            check_problem(&check_refused("an ended reason.")),
            "CrazyAgent did not run the check: an ended reason."
        );
        assert_eq!(
            check_problem("server_exited"),
            "The server stopped before answering."
        );
        assert_eq!(
            check_problem("server_setting_up"),
            "context-mode is still being set up. Check again in a minute."
        );
        for other in ["server_cant_start", "no_such_code", CHECK_REFUSED, ""] {
            assert_eq!(check_problem(other), "The server can't start.", "{other:?}");
        }
    }

    #[test]
    fn a_check_that_found_no_shared_install_says_why_in_its_own_words() {
        assert_eq!(
            check_problem(&install_failed(7)),
            "Setting up context-mode failed. Check again in 7 minutes."
        );
        assert_eq!(
            check_problem(&install_failed(1)),
            "Setting up context-mode failed. Check again in 1 minute."
        );
        for unreadable in [
            "server_install_failed:",
            "server_install_failed:0",
            "server_install_failed:99",
            "server_install_failed:x",
        ] {
            assert_eq!(
                check_problem(unreadable),
                "Setting up context-mode failed. Check again in 10 minutes.",
                "{unreadable:?}"
            );
        }
        for (code, words) in [
            (
                "server_install_failed",
                "Setting up context-mode failed. Check again in 10 minutes.",
            ),
            (
                "server_npm_missing",
                "context-mode needs npm and node in the same folder as npx. Install Node.js, then check again.",
            ),
            (
                "server_version_unpinned",
                "This context-mode version is not the one the default pack pins, so CrazyAgent does not install it. Use the default pack's version.",
            ),
            (
                "server_installs_busy",
                "Too many installs are running. Check again in a minute.",
            ),
            (
                "server_setting_up",
                "context-mode is still being set up. Check again in a minute.",
            ),
        ] {
            assert_eq!(check_problem(code), words, "{code}");
            assert_eq!(plain_problem(code), Some(words), "{code}");
        }
    }

    #[test]
    fn a_check_on_a_node_too_old_for_context_mode_names_the_version_it_needs() {
        let words = "context-mode needs Node.js 22.5 or newer. Update Node.js, then check again.";
        assert_eq!(check_problem("server_node_too_old"), words);
        assert_eq!(plain_problem("server_node_too_old"), Some(words));
    }
}
