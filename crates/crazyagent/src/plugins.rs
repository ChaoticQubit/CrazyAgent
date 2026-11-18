use crate::engine::EngineError;
use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

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
    !text.chars().any(char::is_control) && !crate::memory::hidden_text(text)
}

/// Whether a server command is a full path on this system: rooted at `/` on Unix. Input:
/// the command. Output: the answer.
#[cfg(unix)]
pub(crate) fn absolute_command(command: &str) -> bool {
    command.starts_with('/')
}

/// Whether a server command is a full path on this system: on Windows a plain drive path
/// such as `C:\Program Files\nodejs\npx.cmd`, never a `/`, UNC, verbatim, device or stream
/// path ([`crate::sandbox::windows_clean_text`]). Input: the command. Output: the answer.
#[cfg(not(unix))]
pub(crate) fn absolute_command(command: &str) -> bool {
    crate::sandbox::windows_clean_text(command)
}

impl ServerSpec {
    /// Checks the command or URL before it is saved, approved or checked, refusing control and
    /// hidden characters (`crate::memory::hidden_text`) anywhere in it. Input: an untrusted
    /// spec. Output: Ok, or a code naming the first problem: a command that is not a full path
    /// ([`absolute_command`]) or is oversized, too many or bad arguments, a command and
    /// arguments over `COMMAND_LINE_BYTES_MAX` together, or a URL that is not https (plain
    /// http is allowed only for this Mac) or carries a password.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::Stdio { command, args } => {
                if command.is_empty()
                    || command.len() > COMMAND_BYTES_MAX
                    || !absolute_command(command)
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
    /// Holds bytes read for one launch as a secret, taking the buffer without a copy.
    /// Input: the secret bytes. Output: the value, wiped when it is dropped.
    pub fn new(bytes: Vec<u8>) -> Self {
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
        crate::credentials::wipe(&mut self.0);
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
/// spec: the shared install is still being set up, setting it up failed or cannot run, the
/// installed files did not match the pinned hashes, the Node.js beside npx is too old or sits
/// in a folder Windows sandboxes cannot read, or the tool resolver does not find that npx.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextModeOff {
    SettingUp,
    Failed,
    Mismatch,
    NodeTooOld,
    NodeFolderUnreadable,
    NpxNotFound,
}

/// The npm package of the default pack's context-mode server.
pub(crate) const CONTEXT_MODE_PACKAGE: &str = "context-mode";
const PACKAGE_VERSION_BYTES_MAX: usize = 32;

/// Reads the default pack's pinned context-mode launch from an approved spec: a rooted
/// `npx`, as the full path [`ServerSpec::validate`] asks for on this system, given exactly
/// `-y context-mode@<version>` with an exact version (RV-DS241-153). Input: the spec.
/// Output: npx's folder and the version, or none for any other spec.
pub(crate) fn pinned_context_mode(spec: &ServerSpec) -> Option<(&Path, &str)> {
    let ServerSpec::Stdio { command, args } = spec else {
        return None;
    };
    let command = Path::new(command);
    let [flag, package] = args.as_slice() else {
        return None;
    };
    let version = package
        .strip_prefix(CONTEXT_MODE_PACKAGE)
        .and_then(|rest| rest.strip_prefix('@'))
        .filter(|version| flag == "-y" && exact_version(version))?;
    let npx = command.has_root() && npx_program(command);
    command.parent().filter(|_| npx).map(|bin| (bin, version))
}

/// Reads the default pack's pinned context-mode launch from an approved spec only when its
/// npx is one the engine's tool resolver finds, as setup writes that launch; a spec only
/// shaped like it, such as a repository's own npx, never runs (RV-DS241-150). The probe
/// state is read before the places are built, so a probe ending between the two counts.
/// Inputs: the engine root and the spec. Output: as [`context_mode_on`].
pub(crate) fn found_context_mode<'a>(
    root: &Path,
    spec: &'a ServerSpec,
) -> Result<Option<(&'a Path, &'a str)>, ContextModeOff> {
    if pinned_context_mode(spec).is_none() {
        return Ok(None);
    }
    let pending = crate::tools::probe::mark().first_run_pending;
    let places = crate::tools::Places::from_environment(Some(root));
    context_mode_on(spec, &places, pending)
}

/// Reads the pinned context-mode launch from a spec on the given places, where its npx must
/// pass the resolver's checks in a folder they look in. Any other npx is being set up while
/// the first background folder probe may still add its folder, and after that is one
/// CrazyAgent did not find, and the probes look again. Inputs: the spec, the places and
/// whether that probe was still going. Output: npx's folder and version, none, or why not.
pub(crate) fn context_mode_on<'a>(
    spec: &'a ServerSpec,
    places: &crate::tools::Places,
    pending: bool,
) -> Result<Option<(&'a Path, &'a str)>, ContextModeOff> {
    let (ServerSpec::Stdio { command, .. }, Some((bin, version))) =
        (spec, pinned_context_mode(spec))
    else {
        return Ok(None);
    };
    let listed = places.folders().iter().any(|folder| folder == bin);
    if listed && crate::tools::resolve_entry(Path::new(command), places).is_ok() {
        return Ok(Some((bin, version)));
    }
    if pending {
        return Err(ContextModeOff::SettingUp);
    }
    crate::tools::probe::missed();
    Err(ContextModeOff::NpxNotFound)
}

/// The spec a harness or a server check runs for one approved server, never through cmd.exe
/// (V243-WIN-MCP-NPX, #341 sec F1): on Windows a stdio command runs only as a program, as
/// written and as its real file ([`crate::tools::program_file`]), or as npm's own `npx.cmd`
/// mapped by its layout ([`npx_spec`]); a batch name that is not there is not found. Inputs:
/// the engine root and the spec. Output: the spec to run, or the code that leaves it out.
pub(crate) fn without_cmd_exe(root: &Path, spec: ServerSpec) -> Result<ServerSpec, &'static str> {
    #[cfg(windows)]
    if let ServerSpec::Stdio { command, args } = &spec {
        let command = Path::new(command);
        let found = command.canonicalize();
        let missing = found
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
        let real = found.ok();
        let batch = crate::tools::batch_file(command)
            || real.as_deref().is_some_and(crate::tools::batch_file);
        if batch && missing {
            return Err("server_command_not_found");
        }
        if batch {
            let places = crate::tools::Places::from_environment(Some(root));
            return npx_spec(command, args, &places);
        }
        let program = crate::tools::program_file(command)
            && real.as_deref().is_none_or(crate::tools::program_file);
        if !program {
            return Err("server_batch_file");
        }
    }
    #[cfg(not(windows))]
    let _ = root;
    Ok(spec)
}

/// npm's `npx.cmd` as the tool resolver runs it ([`crate::tools::resolve_entry`]), kept only
/// when the resolver mapped it by npx's layout ([`crate::tools::npx_real`]): `node.exe` with
/// `npx-cli.js` before the approved arguments, both in plain drive spelling. Inputs: the
/// command, its arguments and the places. Output: the spec, `server_command_not_found` for
/// an npx.cmd the resolver refuses, its cause logged (#341 code F1), or `server_batch_file`.
#[cfg(windows)]
fn npx_spec(
    command: &Path,
    args: &[String],
    places: &crate::tools::Places,
) -> Result<ServerSpec, &'static str> {
    let other = "server_batch_file";
    let (node, cli) = crate::tools::npx_real(command).ok_or(other)?;
    let tool = crate::tools::resolve_entry(command, places).map_err(|missing| {
        let why = missing.refused.as_ref().map(|refused| &refused.why);
        eprintln!(
            "plugin_server_npx_refused reason={:?} refused={why:?}",
            missing.reason
        );
        "server_command_not_found"
    })?;
    if tool.program != cli || tool.interpreter().map(|found| &found.program) != Some(&node) {
        return Err(other);
    }
    let plain = |file: &Path| {
        crate::sandbox::canonical(file)
            .ok()?
            .into_os_string()
            .into_string()
            .ok()
    };
    let mut words = Vec::with_capacity(args.len() + 1);
    words.push(plain(&cli).ok_or(other)?);
    words.extend_from_slice(args);
    assert!(words.len() <= ARGS_MAX + 1);
    Ok(ServerSpec::Stdio {
        command: plain(&node).ok_or(other)?,
        args: words,
    })
}

#[cfg(unix)]
fn npx_program(command: &Path) -> bool {
    command.file_name() == Some("npx".as_ref())
}

#[cfg(not(unix))]
fn npx_program(command: &Path) -> bool {
    command
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("npx"))
}

/// Whether a package version is exact: three numbers and an optional pre-release of letters,
/// digits, dots and dashes, at most 32 bytes. Input: the version. Output: the answer.
pub(crate) fn exact_version(version: &str) -> bool {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    version.len() <= PACKAGE_VERSION_BYTES_MAX
        && numbers.len() == 3
        && numbers
            .iter()
            .all(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        && pre.is_none_or(|pre| {
            !pre.is_empty()
                && pre
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        })
}

/// The effective set with secret values read, ready for a harness launch; skills as in
/// `EffectivePlugins`. `context_mode_off` says why context-mode was left out, `batch_files`
/// names each server left out as no Windows program ([`without_cmd_exe`]), `commands_not_found`
/// each one whose command was not found or whose npx.cmd was refused, and `left_out` every
/// server left out, whose hooks the launch leaves out too.
#[derive(Debug, Default)]
pub struct LaunchPlugins {
    pub skills: Vec<EffectiveSkill>,
    pub servers: Vec<LaunchServer>,
    pub context_mode_off: Option<ContextModeOff>,
    pub batch_files: Vec<String>,
    pub commands_not_found: Vec<String>,
    pub left_out: Vec<String>,
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
    #[cfg(unix)]
    (
        "server_command_invalid",
        "Enter the full path to the command, starting with /.",
    ),
    #[cfg(not(unix))]
    (
        "server_command_invalid",
        "Enter the full path to the command, such as C:\\Program Files\\nodejs\\npx.cmd.",
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
        "server_batch_file",
        "This command is not a Windows program (.exe or .com), which Windows may start through cmd.exe, so CrazyAgent does not start it. Set the command to the tool's .exe, or to node.exe with the tool's script as the first argument.",
    ),
    (
        "server_stored_spec_invalid",
        "This server's command or address holds hidden or control characters, so it can't start. Remove it, then add it again.",
    ),
    (
        crate::sandbox::OWNER_PORTS_UNREADABLE,
        "CrazyAgent can't read its settings file, so it did not run the check. Fix the file and check again.",
    ),
    (
        "server_cleanup_uncertain",
        "The server's processes could not be confirmed stopped.",
    ),
    (
        crate::harnesses::leftovers::SANDBOX_LEFT,
        "The server left processes running after its check. CrazyAgent ends them.",
    ),
    (
        crate::harnesses::leftovers::SANDBOX_UNCHECKED,
        "The server's processes could not be listed after its check. CrazyAgent keeps looking for them and ends them.",
    ),
    ("server_exited", "The server stopped before answering."),
    (
        "server_check_timeout_powershell",
        "The server did not answer within 10 seconds. It runs on PowerShell, so the likely cause is PowerShell reading every module installed on this computer before its first command, which takes longer when many modules are installed.",
    ),
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
        "server_npx_not_found",
        "CrazyAgent did not find the npx this context-mode server runs on this computer, so it did not start it.",
    ),
    (
        "server_node_folder_unreadable",
        "Windows does not let sandboxed programs read the Node.js folder context-mode runs from, and only an administrator can change that, so CrazyAgent did not start it.",
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
pub const CHECK_REASON_BYTES_MAX: usize = 1024;

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

/// The problem a server check records when it ends with `code`: a timed-out check of a
/// PowerShell server (`powershell` or `pwsh`, any case or extension) records
/// `server_check_timeout_powershell`, whose sentence names module reading as the likely cause.
/// Inputs: the check's code and the server's program. Output: the code to record.
pub fn timed_out_problem(code: &'static str, program: &Path) -> &'static str {
    assert!(!code.is_empty());
    assert!(!program.as_os_str().is_empty());
    let text = program.to_string_lossy();
    let name = text.rsplit(['/', '\\']).next().unwrap_or_default();
    let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
    let powershell = ["powershell", "pwsh"]
        .iter()
        .any(|known| stem.eq_ignore_ascii_case(known));
    let named = "server_check_timeout_powershell";
    if code == "server_check_timeout" && powershell {
        assert!(known_code(named).is_some());
        named
    } else {
        code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const UVX: &str = "/opt/homebrew/bin/uvx";
    #[cfg(not(unix))]
    const UVX: &str = r"C:\Users\owner\.local\bin\uvx.exe";
    #[cfg(unix)]
    const PROGRAM: &str = "/bin/x";
    #[cfg(not(unix))]
    const PROGRAM: &str = r"C:\bin\x.exe";
    #[cfg(unix)]
    const NPX: &str = "/usr/local/bin/npx";
    #[cfg(not(unix))]
    const NPX: &str = r"C:\Program Files\nodejs\npx.cmd";

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
        stdio(UVX, &["mcp-server-postgres"]).validate().unwrap();
        http("https://mcp.figma.com/mcp").validate().unwrap();
        http("http://127.0.0.1:8931/mcp").validate().unwrap();
        http("http://localhost:3000/mcp").validate().unwrap();
        http("http://[::1]:3000/mcp").validate().unwrap();
        let many: Vec<String> = (0..=ARGS_MAX).map(|i| format!("a{i}")).collect();
        let cases = [
            (stdio("uvx", &[]), "server_command_invalid"),
            (stdio("", &[]), "server_command_invalid"),
            (
                stdio(&format!("{PROGRAM}{}", "c".repeat(COMMAND_BYTES_MAX)), &[]),
                "server_command_invalid",
            ),
            (
                stdio(&format!("{PROGRAM}\n"), &[]),
                "server_command_invalid",
            ),
            (
                ServerSpec::Stdio {
                    command: PROGRAM.into(),
                    args: many,
                },
                "server_args_invalid",
            ),
            (stdio(PROGRAM, &["a\u{0}b"]), "server_args_invalid"),
            (stdio(PROGRAM, &["a\u{200b}b"]), "server_args_invalid"),
            (
                stdio(PROGRAM, &["--path=\u{202e}txt.exe"]),
                "server_args_invalid",
            ),
            (
                stdio(&format!("{PROGRAM}\u{feff}"), &[]),
                "server_command_invalid",
            ),
            (http("https://e.com/\u{2060}mcp"), "server_url_invalid"),
            (
                stdio(PROGRAM, &[&"a".repeat(ARG_BYTES_MAX + 1)]),
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

    /// V243-WIN-PWSH-CONFINED-CHECK: a check of a PowerShell server that ran out of time says
    /// the server did not answer in time and names PowerShell reading its installed modules as
    /// the likely cause; any other program, and any other code, keeps its own words.
    #[test]
    fn a_powershell_server_that_timed_out_names_its_module_reading_as_the_likely_cause() {
        let timed_out = "server_check_timeout";
        let named = "server_check_timeout_powershell";
        for program in [
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Program Files\PowerShell\7\PWSH.EXE",
            "/usr/local/bin/pwsh",
            "/opt/microsoft/powershell/7/pwsh",
        ] {
            let program = Path::new(program);
            assert_eq!(timed_out_problem(timed_out, program), named, "{program:?}");
            assert_eq!(timed_out_problem("server_exited", program), "server_exited");
        }
        for program in [
            r"C:\Program Files\nodejs\node.exe",
            "/usr/local/bin/powershell-helper",
            "/opt/pwsh/bin/uvx",
        ] {
            let program = Path::new(program);
            assert_eq!(
                timed_out_problem(timed_out, program),
                timed_out,
                "{program:?}"
            );
        }
        assert_eq!(
            check_problem(timed_out),
            "The server did not answer within 10 seconds."
        );
        assert_eq!(
            check_problem(named),
            "The server did not answer within 10 seconds. It runs on PowerShell, so the likely cause is PowerShell reading every module installed on this computer before its first command, which takes longer when many modules are installed."
        );
        assert_eq!(known_code(named), Some(named));
    }

    /// V243-WIN-MCP-NPX: a check of a Windows command that is no program, npm's npx.cmd
    /// aside, says what to point the command at instead, and the code is known to every client.
    #[test]
    fn a_check_of_a_batch_command_says_what_to_point_it_at() {
        let words = "This command is not a Windows program (.exe or .com), which Windows may start through cmd.exe, so CrazyAgent does not start it. Set the command to the tool's .exe, or to node.exe with the tool's script as the first argument.";
        assert_eq!(check_problem("server_batch_file"), words);
        assert_eq!(known_code("server_batch_file"), Some("server_batch_file"));
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

    /// RV-DS241-150 r2: a check of a pinned-shaped context-mode spec whose npx the tool
    /// resolver does not find reads one sentence of its own, a code the desktop knows.
    #[test]
    fn a_check_of_a_context_mode_npx_the_resolver_does_not_find_says_so_in_one_sentence() {
        let words = "CrazyAgent did not find the npx this context-mode server runs on this computer, so it did not start it.";
        assert_eq!(check_problem("server_npx_not_found"), words);
        assert_eq!(plain_problem("server_npx_not_found"), Some(words));
        assert_eq!(
            known_code("server_npx_not_found"),
            Some("server_npx_not_found")
        );
    }

    /// V244-WIN-NPM: a check whose context-mode install met a Node.js folder no Windows
    /// sandbox may read, and only an administrator can open, reads one sentence of its own,
    /// a code the desktop knows; a boundary reason that names a folder shows that folder.
    #[test]
    fn a_check_on_a_node_folder_windows_sandboxes_cannot_read_says_so_in_one_sentence() {
        let words = "Windows does not let sandboxed programs read the Node.js folder context-mode runs from, and only an administrator can change that, so CrazyAgent did not start it.";
        assert_eq!(check_problem("server_node_folder_unreadable"), words);
        assert_eq!(plain_problem("server_node_folder_unreadable"), Some(words));
        assert_eq!(
            known_code("server_node_folder_unreadable"),
            Some("server_node_folder_unreadable")
        );
        let folder = std::path::Path::new(r"C:\Program Files\nodejs");
        let reason = crate::sandbox::unreadable_reason(folder);
        assert!(reason.contains(r"C:\Program Files\nodejs"), "{reason}");
        assert!(check_problem(&check_refused(&reason)).contains(&reason));
    }

    /// RV-DS241-153: the pinned context-mode launch is read from the full npx path that
    /// `validate` accepts on this system, a drive path on Windows since WIN-PACK-SEED, so it is
    /// the pinned launch there too and is never run through npx as approved.
    #[test]
    fn the_pinned_context_mode_launch_is_read_from_the_rooted_npx_validate_accepts() {
        let spec = stdio(NPX, &["-y", "context-mode@1.0.169"]);
        assert!(spec.validate().is_ok());
        assert_eq!(
            pinned_context_mode(&spec),
            Some((Path::new(NPX).parent().unwrap(), "1.0.169"))
        );
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// WIN-PACK-SEED: on Windows a server command is a plain drive path, such as the `npx.cmd`
    /// setup finds, never a `/` path, a bare name, or a UNC, verbatim, device, drive-relative,
    /// forward-slash or stream path ([`crate::sandbox::windows_clean_text`]).
    #[test]
    fn a_windows_server_command_is_a_plain_drive_path() {
        let stdio = |command: &str| ServerSpec::Stdio {
            command: command.into(),
            args: vec!["-y".into()],
        };
        assert!(stdio(r"C:\Program Files\nodejs\npx.cmd").validate().is_ok());
        assert_eq!(
            plain_problem("server_command_invalid"),
            Some(r"Enter the full path to the command, such as C:\Program Files\nodejs\npx.cmd.")
        );
        for command in [
            "/usr/local/bin/npx",
            "npx.cmd",
            r"\nodejs\npx.cmd",
            r"\\server\share\npx.cmd",
            r"\\?\C:\nodejs\npx.cmd",
            r"\\.\pipe\npx",
            r"C:nodejs\npx.cmd",
            "C:/nodejs/npx.cmd",
            r"C:\nodejs\npx.cmd:stream",
        ] {
            let refused = stdio(command).validate().unwrap_err();
            assert_eq!(refused.0, "server_command_invalid", "{command}");
        }
    }

    /// V243-WIN-MCP-NPX and #341: npm's `npx.cmd`, found by its layout, runs as the `node.exe`
    /// beside it with `npx-cli.js` before the approved arguments, in plain drive spelling; one
    /// the resolver refuses, and a `.cmd` that is not there, npx.cmd included, is not found; any
    /// other `.cmd` or `.bat`, and a name with no extension or a dotted one with a `.cmd` beside
    /// it, there or not, is a batch file; a `.exe` spec is unchanged.
    #[test]
    fn npm_s_npx_cmd_runs_as_node_and_every_other_batch_command_is_left_out() {
        let folder = tempfile::tempdir().unwrap();
        let real = crate::sandbox::canonical(folder.path()).unwrap();
        let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("where.exe");
        std::fs::copy(&shipped, real.join("node.exe")).unwrap();
        let bin = real.join("node_modules").join("npm").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("npx-cli.js"), "#!/usr/bin/env node\n").unwrap();
        for name in ["npx.cmd", "other.cmd", "run.bat", "tool.cmd", "tool.v2.cmd"] {
            std::fs::write(real.join(name), "@ECHO off\r\n").unwrap();
        }
        let places = crate::tools::Places::new(vec![real.clone()], None);
        let args = vec!["-y".to_owned(), "@scope/server".to_owned()];
        let text = |path: PathBuf| path.to_str().unwrap().to_owned();
        let spec = |name: &str| ServerSpec::Stdio {
            command: text(real.join(name)),
            args: args.clone(),
        };
        let mapped = npx_spec(&real.join("npx.cmd"), &args, &places);
        assert_eq!(
            mapped,
            Ok(ServerSpec::Stdio {
                command: text(real.join("node.exe")),
                args: vec![
                    text(bin.join("npx-cli.js")),
                    "-y".into(),
                    "@scope/server".into()
                ],
            })
        );
        assert!(mapped.unwrap().validate().is_ok());
        for name in ["other.cmd", "run.bat"] {
            let refused = npx_spec(&real.join(name), &args, &places);
            assert_eq!(refused, Err("server_batch_file"), "{name}");
            assert_eq!(without_cmd_exe(&real, spec(name)), Err("server_batch_file"));
        }
        for name in ["tool", "tool.v2", "gone", "gone.v2"] {
            assert!(spec(name).validate().is_ok(), "{name}");
            assert_eq!(without_cmd_exe(&real, spec(name)), Err("server_batch_file"));
        }
        std::fs::copy(&shipped, real.join("tool")).unwrap();
        std::fs::copy(&shipped, real.join("tool.v2")).unwrap();
        for name in ["tool", "tool.v2"] {
            assert_eq!(without_cmd_exe(&real, spec(name)), Err("server_batch_file"));
        }
        assert_eq!(
            without_cmd_exe(&real, spec("gone.exe")),
            Ok(spec("gone.exe"))
        );
        std::fs::write(bin.join("npx-cli.js"), "#!/usr/bin/env deno\n").unwrap();
        let refused = npx_spec(&real.join("npx.cmd"), &args, &places);
        assert_eq!(refused, Err("server_command_not_found"));
        std::fs::remove_file(real.join("node.exe")).unwrap();
        let lone = npx_spec(&real.join("npx.cmd"), &args, &places);
        assert_eq!(lone, Err("server_batch_file"));
        assert_eq!(
            without_cmd_exe(&real, spec("gone.cmd")),
            Err("server_command_not_found")
        );
        std::fs::remove_file(real.join("npx.cmd")).unwrap();
        assert_eq!(
            without_cmd_exe(&real, spec("npx.cmd")),
            Err("server_command_not_found")
        );
        let program = ServerSpec::Stdio {
            command: text(shipped),
            args: args.clone(),
        };
        assert_eq!(without_cmd_exe(&real, program.clone()), Ok(program));
    }
}
