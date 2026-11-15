use super::AccountKind;
use crate::app_plugins::yaml::{self, Value};
use serde::{Deserialize, Serialize};

/// The most bytes of a harness id, an alias or a program name in a manifest.
pub const NAME_BYTES_MAX: usize = 64;
/// The most bytes of one sentence or folder a manifest carries.
pub const SENTENCE_BYTES_MAX: usize = 240;
/// The most entries in any one list of a manifest.
pub const LIST_MAX: usize = 16;
/// The values the engine fills into manifest text, each written in braces, as in `{home}`.
/// Nothing else is substituted, and `{token}` is the relay's placeholder, never a key.
pub const PLACEHOLDERS: [&str; 8] = [
    "home",
    "session",
    "model",
    "prompt",
    "cwd",
    "relay_url",
    "token",
    "key_file",
];

/// The code of a manifest the strict YAML reader refused.
pub const YAML: &str = "harness_manifest_yaml";
/// The code of a manifest whose schema or kind is not one this engine reads.
pub const SCHEMA: &str = "harness_manifest_schema";
/// The code of a key no harness manifest has.
pub const UNKNOWN_KEY: &str = "harness_manifest_unknown_key";
/// The code of a needed setting that is not there.
pub const MISSING: &str = "harness_manifest_missing";
/// The code of a setting of the wrong type.
pub const TYPE: &str = "harness_manifest_type";
/// The code of a setting whose value is not allowed.
pub const VALUE: &str = "harness_manifest_value";
/// The code of a driver or record reader kind the engine does not have.
pub const KIND: &str = "harness_manifest_kind";
/// The code of a placeholder outside `PLACEHOLDERS`.
pub const PLACEHOLDER: &str = "harness_manifest_placeholder";
/// The code of an id, alias or program name two manifests share.
pub const DUPLICATE: &str = "harness_manifest_duplicate";

const TYPED: [&str; 18] = [
    "schema",
    "kind",
    "id",
    "name",
    "aliases",
    "binaries",
    "locations",
    "headless",
    "model_selection",
    "account_kinds",
    "account_protocols",
    "limits",
    "tools_limited",
    "cannot_start",
    "roles",
    "sub_agents",
    "piped",
    "terminal",
];

/// The top-level blocks a manifest may carry that this module keeps as written: the task
/// that owns each one reads it through `Manifest::block` and checks its own keys.
pub const BLOCKS: [&str; 8] = [
    "version",
    "update",
    "home",
    "accounts",
    "mcp",
    "instructions",
    "import",
    "rules",
];

/// How the engine talks to a harness that runs without a terminal. The first three are the
/// engine's own adapters for Codex, Claude Code and RCLI; the rest are shared drivers a
/// manifest fills in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    CodexAppServer,
    ClaudeStreamJson,
    Rcli,
    Acp,
    JsonlTurn,
    JsonlRpc,
    TextTurn,
}

impl DriverKind {
    /// Reads a driver kind by its manifest name. Input: the name, such as `jsonl-turn`.
    /// Output: the kind, or none for a name the engine has no driver for.
    pub fn of(name: &str) -> Option<Self> {
        match name {
            "codex-app-server" => Some(Self::CodexAppServer),
            "claude-stream-json" => Some(Self::ClaudeStreamJson),
            "rcli" => Some(Self::Rcli),
            "acp" => Some(Self::Acp),
            "jsonl-turn" => Some(Self::JsonlTurn),
            "jsonl-rpc" => Some(Self::JsonlRpc),
            "text-turn" => Some(Self::TextTurn),
            _ => None,
        }
    }
}

/// How the engine reads back what a harness did in a live terminal. `Unavailable` is a
/// harness with no terminal record the engine can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    CodexRollouts,
    ClaudeSession,
    JsonlAppend,
    JsonRewrite,
    SqlitePoll,
    MarkdownHistory,
    Screen,
    Unavailable,
}

impl RecordKind {
    /// Reads a record reader kind by its manifest name. Input: the name, such as
    /// `jsonl-append`. Output: the kind, or none for a name the engine has no reader for.
    pub fn of(name: &str) -> Option<Self> {
        match name {
            "codex-rollouts" => Some(Self::CodexRollouts),
            "claude-session" => Some(Self::ClaudeSession),
            "jsonl-append" => Some(Self::JsonlAppend),
            "json-rewrite" => Some(Self::JsonRewrite),
            "sqlite-poll" => Some(Self::SqlitePoll),
            "markdown-history" => Some(Self::MarkdownHistory),
            "screen" => Some(Self::Screen),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// One thing an event rule can report about a harness, the same list for piped output and
/// for terminal records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fact {
    Ready,
    TurnStarted,
    Text,
    ToolUse,
    Waiting,
    Interrupted,
    TurnFinished,
    TurnError,
    Usage,
    Exited,
}

/// One event rule of a manifest: the dotted paths and values a JSON object must hold, the
/// fact it then reports, and the dotted paths the fact's details are read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub when: Vec<(String, String)>,
    pub emit: Fact,
    pub text: Option<String>,
    pub name: Option<String>,
    pub session: Option<String>,
    pub success: Option<String>,
    pub tokens: Vec<String>,
    pub rate_limited: Option<String>,
}

/// How a lead on a harness gets sub-agents: the harness's own agent team, its own threads,
/// or workers the app starts. The catalog serves it to both apps as `teams`, `threads` or
/// `app`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubAgents {
    Teams,
    Threads,
    App,
}

/// How the engine reads the free resets, credits and spend of a sign-in of a harness beyond
/// its usage windows: by the Codex reader or by the Claude reader. A manifest that names
/// neither has a sign-in whose limits are not reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitsKind {
    Codex,
    Claude,
}

impl LimitsKind {
    /// Reads a limits reader by its manifest name. Input: the name, `codex` or `claude`.
    /// Output: the kind, or none for a name the engine has no limits reader for.
    pub fn of(name: &str) -> Option<Self> {
        match name {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }
}

/// What a harness can be in a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Agent,
    Master,
    Research,
}

/// A refused manifest: a stable code and one plain sentence that names the setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub code: &'static str,
    pub message: String,
}

/// One harness as its manifest describes it. The catalog fields mirror `HarnessSpec`;
/// `cannot_start` is the plain reason CrazyAgent has no way to start it, none when it has.
/// `account_protocols` are the wire formats of the endpoint accounts it runs on, where a
/// name that ends in `*` stands for every format that starts with the text before it.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub binaries: Vec<String>,
    pub locations: Vec<String>,
    pub headless: String,
    pub model_selection: bool,
    pub account_kinds: Vec<AccountKind>,
    pub account_protocols: Vec<String>,
    pub limits: Option<LimitsKind>,
    pub tools_limited: Option<String>,
    pub cannot_start: Option<String>,
    pub roles: Vec<Role>,
    pub sub_agents: SubAgents,
    pub piped: Option<DriverKind>,
    pub record: RecordKind,
    pub owner_folders: Vec<String>,
    pub minimum_version: Option<String>,
    pub sign_in_minimum: Option<String>,
    source: Value,
}

impl Manifest {
    /// Gives one top-level block as it is written, for the task that owns it to read.
    /// Input: the key, such as `home` or `rules`. Output: the block, or none when the
    /// manifest has no such key.
    pub fn block(&self, key: &str) -> Option<&Value> {
        self.source.get(key)
    }
}

fn refuse<T>(code: &'static str, message: String) -> Result<T, Refused> {
    Err(Refused { code, message })
}

fn word(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= NAME_BYTES_MAX
        && text.as_bytes()[0].is_ascii_alphanumeric()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn program(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= NAME_BYTES_MAX
        && text.as_bytes()[0].is_ascii_alphanumeric()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn relative(text: &str) -> bool {
    !text.contains('\\')
        && text
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn protocol(text: &str) -> bool {
    text.strip_suffix('*')
        .unwrap_or(text)
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn entries<'a>(value: &'a Value, path: &str) -> Result<&'a [(String, Value)], Refused> {
    match value {
        Value::Map(entries) => Ok(entries),
        _ => refuse(TYPE, format!("{path}: this must be a block of settings.")),
    }
}

fn block<'a>(map: &'a Value, key: &str) -> Result<Option<&'a Value>, Refused> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => entries(value, key).map(|_| Some(value)),
    }
}

fn text(map: &Value, key: &str, path: &str, limit: usize) -> Result<Option<String>, Refused> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Str(text)) if text.is_empty() || text.len() > limit => refuse(
            VALUE,
            format!("{path}: this must be 1 to {limit} bytes of text."),
        ),
        Some(Value::Str(text)) => Ok(Some(text.clone())),
        Some(_) => refuse(TYPE, format!("{path}: this must be text in quotes.")),
    }
}

fn needed<T>(value: Option<T>, path: &str) -> Result<T, Refused> {
    value.ok_or_else(|| Refused {
        code: MISSING,
        message: format!("{path}: this setting is needed."),
    })
}

fn texts(map: &Value, key: &str, path: &str, limit: usize) -> Result<Vec<String>, Refused> {
    let items = match map.get(key) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::List(items)) => items,
        Some(_) => return refuse(TYPE, format!("{path}: this must be a list of texts.")),
    };
    if items.len() > LIST_MAX {
        return refuse(
            VALUE,
            format!("{path}: a list holds at most {LIST_MAX} entries."),
        );
    }
    let mut listed: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let Value::Str(text) = item else {
            return refuse(TYPE, format!("{path}: every entry must be text in quotes."));
        };
        if text.is_empty() || text.len() > limit {
            return refuse(
                VALUE,
                format!("{path}: every entry must be 1 to {limit} bytes of text."),
            );
        }
        if listed.contains(text) {
            return refuse(VALUE, format!("{path}: {text} is listed twice."));
        }
        listed.push(text.clone());
    }
    assert!(listed.len() <= LIST_MAX);
    Ok(listed)
}

fn shaped(
    listed: Vec<String>,
    path: &str,
    fits: fn(&str) -> bool,
    shape: &str,
) -> Result<Vec<String>, Refused> {
    if let Some(text) = listed.iter().find(|text| !fits(text.as_str())) {
        return refuse(VALUE, format!("{path}: {text} is not {shape}."));
    }
    Ok(listed)
}

fn placeholders(value: &Value, path: &str, depth: usize) -> Result<(), Refused> {
    assert!(depth <= yaml::DEPTH_MAX + 1);
    match value {
        Value::Str(text) => {
            let mut rest = text.as_str();
            while let Some(open) = rest.find('{') {
                let after = &rest[open + 1..];
                let length = after
                    .bytes()
                    .take_while(|byte| byte.is_ascii_lowercase() || *byte == b'_')
                    .count();
                let name = &after[..length];
                let closed = after[length..].starts_with('}');
                if closed && !name.is_empty() && !PLACEHOLDERS.contains(&name) {
                    return refuse(
                        PLACEHOLDER,
                        format!("{path}: {{{name}}} is not a value CrazyAgent fills in."),
                    );
                }
                rest = after;
            }
            Ok(())
        }
        Value::List(items) => items
            .iter()
            .try_for_each(|item| placeholders(item, path, depth + 1)),
        Value::Map(entries) => entries.iter().try_for_each(|(key, item)| {
            let inner = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            placeholders(item, &inner, depth + 1)
        }),
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => Ok(()),
    }
}

fn account_kinds(map: &Value) -> Result<Vec<AccountKind>, Refused> {
    let names = texts(map, "account_kinds", "account_kinds", NAME_BYTES_MAX)?;
    if names.is_empty() {
        return refuse(MISSING, "account_kinds: this setting is needed.".to_owned());
    }
    names
        .iter()
        .map(|name| {
            serde_json::from_value::<AccountKind>(serde_json::Value::String(name.clone())).or_else(
                |_| {
                    refuse(
                        VALUE,
                        format!("account_kinds: {name} is not a kind of account."),
                    )
                },
            )
        })
        .collect()
}

fn limits(map: &Value) -> Result<Option<LimitsKind>, Refused> {
    let Some(name) = text(map, "limits", "limits", NAME_BYTES_MAX)? else {
        return Ok(None);
    };
    match LimitsKind::of(&name) {
        Some(kind) => Ok(Some(kind)),
        None => refuse(
            KIND,
            format!("limits: CrazyAgent has no limits reader named {name}."),
        ),
    }
}

fn roles(map: &Value) -> Result<Vec<Role>, Refused> {
    texts(map, "roles", "roles", NAME_BYTES_MAX)?
        .iter()
        .map(|name| match name.as_str() {
            "agent" => Ok(Role::Agent),
            "master" => Ok(Role::Master),
            "research" => Ok(Role::Research),
            _ => refuse(
                VALUE,
                format!("roles: {name} is not agent, master or research."),
            ),
        })
        .collect()
}

fn sub_agents(map: &Value) -> Result<SubAgents, Refused> {
    match text(map, "sub_agents", "sub_agents", NAME_BYTES_MAX)?.as_deref() {
        None | Some("app") => Ok(SubAgents::App),
        Some("teams") => Ok(SubAgents::Teams),
        Some("threads") => Ok(SubAgents::Threads),
        Some(name) => refuse(
            VALUE,
            format!("sub_agents: {name} is not teams, threads or app."),
        ),
    }
}

fn piped(map: &Value) -> Result<Option<DriverKind>, Refused> {
    let Some(piped) = block(map, "piped")? else {
        return Ok(None);
    };
    let name = needed(
        text(piped, "driver", "piped.driver", NAME_BYTES_MAX)?,
        "piped.driver",
    )?;
    match DriverKind::of(&name) {
        Some(kind) => Ok(Some(kind)),
        None => refuse(
            KIND,
            format!("piped.driver: CrazyAgent has no driver named {name}."),
        ),
    }
}

fn record(map: &Value) -> Result<RecordKind, Refused> {
    let Some(terminal) = block(map, "terminal")? else {
        return Ok(RecordKind::Unavailable);
    };
    let Some(record) = block(terminal, "record")? else {
        return Ok(RecordKind::Unavailable);
    };
    let path = "terminal.record.reader";
    let name = needed(text(record, "reader", path, NAME_BYTES_MAX)?, path)?;
    match RecordKind::of(&name) {
        Some(kind) => Ok(kind),
        None => refuse(
            KIND,
            format!("{path}: CrazyAgent has no record reader named {name}."),
        ),
    }
}

/// Reads one harness manifest: strict YAML of at most 16 KiB, only the keys a manifest has,
/// every name, list and sentence within its bound, only driver and record reader kinds the
/// engine has, and only the placeholders in `PLACEHOLDERS`. Input: the file's bytes.
/// Output: the manifest, or the refusal with one plain sentence.
pub fn parse(bytes: &[u8]) -> Result<Manifest, Refused> {
    let source = yaml::parse_bytes(bytes).map_err(|error| Refused {
        code: YAML,
        message: error.message(),
    })?;
    for (key, _) in entries(&source, "the file")? {
        if !TYPED.contains(&key.as_str()) && !BLOCKS.contains(&key.as_str()) {
            return refuse(
                UNKNOWN_KEY,
                format!("{key}: this key is not one a harness manifest has."),
            );
        }
    }
    if source.get("schema") != Some(&Value::Int(1)) {
        return refuse(
            SCHEMA,
            "schema: this app reads schema: 1 harness manifests only.".to_owned(),
        );
    }
    if source.get("kind") != Some(&Value::Str("harness".to_owned())) {
        return refuse(SCHEMA, "kind: the only kind is harness.".to_owned());
    }
    for key in BLOCKS {
        block(&source, key)?;
    }
    placeholders(&source, "", 0)?;
    let id = needed(text(&source, "id", "id", NAME_BYTES_MAX)?, "id")?;
    if !word(&id) {
        return refuse(
            VALUE,
            format!("id: {id} is not lower-case letters, digits and dashes."),
        );
    }
    let aliases = shaped(
        texts(&source, "aliases", "aliases", NAME_BYTES_MAX)?,
        "aliases",
        word,
        "lower-case letters, digits and dashes",
    )?;
    if aliases.contains(&id) {
        return refuse(VALUE, format!("aliases: {id} is the id itself."));
    }
    let binaries = shaped(
        texts(&source, "binaries", "binaries", NAME_BYTES_MAX)?,
        "binaries",
        program,
        "a program name without a folder",
    )?;
    if binaries.is_empty() {
        return refuse(MISSING, "binaries: this setting is needed.".to_owned());
    }
    let locations = shaped(
        texts(&source, "locations", "locations", SENTENCE_BYTES_MAX)?,
        "locations",
        relative,
        "a folder under the home folder",
    )?;
    let model_selection = match source.get("model_selection") {
        Some(Value::Bool(flag)) => *flag,
        None | Some(Value::Null) => {
            return refuse(
                MISSING,
                "model_selection: this setting is needed.".to_owned(),
            );
        }
        Some(_) => {
            return refuse(
                TYPE,
                "model_selection: this must be true or false.".to_owned(),
            );
        }
    };
    let owner_folders = match block(&source, "home")? {
        Some(home) => shaped(
            texts(
                home,
                "owner_folders",
                "home.owner_folders",
                SENTENCE_BYTES_MAX,
            )?,
            "home.owner_folders",
            relative,
            "a folder under the home folder",
        )?,
        None => Vec::new(),
    };
    let (minimum_version, sign_in_minimum) = match block(&source, "version")? {
        Some(found) => (
            text(found, "minimum", "version.minimum", NAME_BYTES_MAX)?,
            text(
                found,
                "sign_in_minimum",
                "version.sign_in_minimum",
                NAME_BYTES_MAX,
            )?,
        ),
        None => (None, None),
    };
    for (floor, path) in [
        (&minimum_version, "version.minimum"),
        (&sign_in_minimum, "version.sign_in_minimum"),
    ] {
        if let Some(floor) = floor
            .as_deref()
            .filter(|floor| !super::update::plain_version(floor))
        {
            return refuse(
                VALUE,
                format!("{path}: {floor} is not a version of two to six numbers joined by dots."),
            );
        }
    }
    let account_protocols = shaped(
        texts(
            &source,
            "account_protocols",
            "account_protocols",
            NAME_BYTES_MAX,
        )?,
        "account_protocols",
        protocol,
        "a wire format name, with * allowed only as its last character",
    )?;
    let manifest = Manifest {
        name: needed(text(&source, "name", "name", NAME_BYTES_MAX)?, "name")?,
        headless: needed(
            text(&source, "headless", "headless", SENTENCE_BYTES_MAX)?,
            "headless",
        )?,
        account_kinds: account_kinds(&source)?,
        limits: limits(&source)?,
        tools_limited: text(
            &source,
            "tools_limited",
            "tools_limited",
            SENTENCE_BYTES_MAX,
        )?,
        cannot_start: text(&source, "cannot_start", "cannot_start", SENTENCE_BYTES_MAX)?,
        roles: roles(&source)?,
        sub_agents: sub_agents(&source)?,
        piped: piped(&source)?,
        record: record(&source)?,
        id,
        aliases,
        binaries,
        locations,
        model_selection,
        owner_folders,
        minimum_version,
        sign_in_minimum,
        account_protocols,
        source,
    };
    assert!(word(&manifest.id) && !manifest.binaries.is_empty());
    assert!(!manifest.account_kinds.is_empty());
    Ok(manifest)
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
