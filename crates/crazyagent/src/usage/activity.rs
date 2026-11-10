use super::capture::{MODELS_PER_HOUR_MAX, has_room};
use super::rules::{model_name, timestamp_ms};
use super::storage;
use crate::coord::adoption::SHELL_TOOLS;
use crate::engine::EngineError;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
mod tests;

const TEXT_BYTES_MAX: usize = 16 * 1024;

const TEST: &[&str] = &[
    "test",
    "pytest",
    "vitest",
    "jest",
    "mocha",
    "spec",
    "coverage",
    "npm test",
    "npx vitest",
    "npx jest",
];
const GIT: &[&str] = &[
    "git push",
    "git pull",
    "git commit",
    "git merge",
    "git rebase",
    "git checkout",
    "git branch",
    "git stash",
    "git log",
    "git diff",
    "git status",
    "git add",
    "git reset",
    "git cherry-pick",
    "git tag",
];
const BUILD: &[&str] = &[
    "npm run build",
    "npm publish",
    "pip install",
    "docker",
    "deploy",
    "make build",
    "npm run dev",
    "npm start",
    "pm2",
    "systemctl",
    "brew",
    "cargo build",
];
const INSTALL: &[&str] = &[
    "npm install",
    "pip install",
    "brew install",
    "apt install",
    "cargo add",
];
const DEBUG: &[&str] = &[
    "fix",
    "bug",
    "error",
    "broken",
    "failing",
    "crash",
    "issue",
    "debug",
    "traceback",
    "exception",
    "stack trace",
    "stacktrace",
    "not working",
    "wrong",
    "unexpected",
    "status code",
    "404",
    "500",
    "401",
    "403",
];
const FEATURE: &[&str] = &[
    "add",
    "create",
    "implement",
    "new",
    "build",
    "feature",
    "introduce",
    "set up",
    "setup",
    "scaffold",
    "generate",
    "make a",
    "make me",
    "make the",
    "write a",
    "write me",
    "write the",
];
const REFACTOR: &[&str] = &[
    "refactor",
    "clean up",
    "cleanup",
    "rename",
    "reorganize",
    "simplify",
    "extract",
    "restructure",
    "move",
    "migrate",
    "split",
];
const BRAINSTORM: &[&str] = &[
    "brainstorm",
    "idea",
    "what if",
    "explore",
    "think about",
    "approach",
    "strategy",
    "design",
    "consider",
    "how should",
    "what would",
    "opinion",
    "suggest",
    "recommend",
];
const RESEARCH: &[&str] = &[
    "research",
    "investigate",
    "look into",
    "find out",
    "check",
    "search",
    "analyze",
    "review",
    "understand",
    "explain",
    "how does",
    "what is",
    "show me",
    "list",
    "compare",
];
const FILE_ENDINGS: &[&str] = &[
    "py", "js", "ts", "tsx", "jsx", "json", "yaml", "yml", "toml", "sql", "sh", "go", "rs", "java",
    "rb", "php", "css", "html", "md", "csv", "xml",
];
const SCRIPT: &[&str] = &[
    "execute",
    "script",
    "scrit",
    "curl",
    "endpoint",
    "request url",
    "query",
    "database",
];
const SCRIPT_THEN_TOKEN: &[&str] = &["api", "fetch", "db"];

/// The text of the `usage_activity` table's category check, so a stored name is always one of
/// the thirteen.
#[cfg(test)]
pub(super) const CATEGORY_CHECK: &str = "'coding','debugging','feature','refactoring','testing','exploration','planning','delegation','git','build_deploy','conversation','brainstorming','general'";

/// What a turn was mostly about. The thirteen categories and rules are CodeBurn's (MIT).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Coding,
    Debugging,
    Feature,
    Refactoring,
    Testing,
    Exploration,
    Planning,
    Delegation,
    Git,
    BuildDeploy,
    Conversation,
    Brainstorming,
    General,
}

/// Every activity, in the order the app lists them.
pub const ACTIVITIES: [Activity; 13] = [
    Activity::Coding,
    Activity::Debugging,
    Activity::Feature,
    Activity::Refactoring,
    Activity::Testing,
    Activity::Exploration,
    Activity::Planning,
    Activity::Delegation,
    Activity::Git,
    Activity::BuildDeploy,
    Activity::Conversation,
    Activity::Brainstorming,
    Activity::General,
];

impl Activity {
    /// The stored name of an activity. Output: the name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Debugging => "debugging",
            Self::Feature => "feature",
            Self::Refactoring => "refactoring",
            Self::Testing => "testing",
            Self::Exploration => "exploration",
            Self::Planning => "planning",
            Self::Delegation => "delegation",
            Self::Git => "git",
            Self::BuildDeploy => "build_deploy",
            Self::Conversation => "conversation",
            Self::Brainstorming => "brainstorming",
            Self::General => "general",
        }
    }
}

/// What a turn's first message mentions. Facts only; the message itself is never kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Words {
    test: bool,
    git: bool,
    build: bool,
    install: bool,
    debug: Option<u32>,
    feature: Option<u32>,
    refactor: Option<u32>,
    brainstorm: bool,
    research: bool,
    file: bool,
    script: bool,
    url: bool,
}

/// Which kinds of tool a turn used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    pub edits: bool,
    pub reads: bool,
    pub bash: bool,
    pub tasks: bool,
    pub search: bool,
    pub mcp: bool,
    pub skill: bool,
    pub plan_mode: bool,
    pub agent_spawn: bool,
}

fn word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn first_phrase(text: &str, phrases: &[&str]) -> Option<u32> {
    let bytes = text.as_bytes();
    phrases
        .iter()
        .filter_map(|phrase| {
            text.match_indices(phrase)
                .find(|(at, _)| {
                    let end = at + phrase.len();
                    (*at == 0 || !word_byte(bytes[*at - 1]))
                        && (end == bytes.len() || !word_byte(bytes[end]))
                })
                .map(|(at, _)| at as u32)
        })
        .min()
}

fn tokens_after<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = &'a str> {
    let bytes = text.as_bytes();
    text.match_indices(word).filter_map(move |(at, _)| {
        let after = at + word.len();
        if (at > 0 && word_byte(bytes[at - 1])) || bytes.get(after) != Some(&b' ') {
            return None;
        }
        text[after + 1..]
            .split(' ')
            .next()
            .filter(|token| !token.is_empty())
    })
}

fn names_a_file(text: &str) -> bool {
    text.match_indices('.').any(|(at, _)| {
        let rest = &text[at + 1..];
        FILE_ENDINGS.iter().any(|ending| {
            rest.starts_with(ending)
                && rest
                    .as_bytes()
                    .get(ending.len())
                    .is_none_or(|byte| !word_byte(*byte))
        })
    })
}

fn runs_a_file(text: &str) -> bool {
    tokens_after(text, "run").any(|token| {
        token.match_indices('.').any(|(at, _)| {
            at > 0
                && token
                    .as_bytes()
                    .get(at + 1)
                    .is_some_and(|byte| word_byte(*byte))
        })
    })
}

fn names_a_url(text: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        text.match_indices(scheme).any(|(at, _)| {
            text.as_bytes()
                .get(at + scheme.len())
                .is_some_and(|byte| *byte != b' ')
        })
    })
}

/// Reads what a message mentions. Input: the message. Output: facts about its words; only the
/// first `TEXT_BYTES_MAX` bytes are read.
pub fn words_of(text: &str) -> Words {
    let mut end = text.len().min(TEXT_BYTES_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = text[..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let has = |phrases: &[&str]| first_phrase(&text, phrases).is_some();
    Words {
        test: has(TEST),
        git: has(GIT),
        build: has(BUILD),
        install: has(INSTALL),
        debug: first_phrase(&text, DEBUG),
        feature: first_phrase(&text, FEATURE),
        refactor: first_phrase(&text, REFACTOR),
        brainstorm: has(BRAINSTORM),
        research: has(RESEARCH),
        file: names_a_file(&text),
        script: has(SCRIPT)
            || runs_a_file(&text)
            || SCRIPT_THEN_TOKEN
                .iter()
                .any(|word| tokens_after(&text, word).any(|token| token.bytes().any(word_byte))),
        url: names_a_url(&text),
    }
}

fn earliest(candidates: &[(Option<u32>, Activity)]) -> Option<Activity> {
    let mut best: Option<(u32, Activity)> = None;
    for (at, activity) in candidates {
        if let Some(at) = at
            && best.is_none_or(|(index, _)| *at < index)
        {
            best = Some((*at, *activity));
        }
    }
    best.map(|(_, activity)| activity)
}

fn by_tools(words: &Words, tools: &Tools) -> Option<Activity> {
    if tools.plan_mode {
        return Some(Activity::Planning);
    }
    if tools.agent_spawn {
        return Some(Activity::Delegation);
    }
    if tools.bash && !tools.edits {
        if words.test {
            return Some(Activity::Testing);
        }
        if words.git {
            return Some(Activity::Git);
        }
        if words.build || words.install {
            return Some(Activity::BuildDeploy);
        }
    }
    if tools.edits {
        return Some(Activity::Coding);
    }
    if tools.bash {
        return Some(if tools.reads {
            Activity::Exploration
        } else {
            Activity::Coding
        });
    }
    if tools.search || tools.mcp || tools.reads {
        return Some(Activity::Exploration);
    }
    if tools.tasks {
        return Some(Activity::Planning);
    }
    tools.skill.then_some(Activity::General)
}

fn conversation(words: &Words) -> Activity {
    if words.brainstorm {
        return Activity::Brainstorming;
    }
    if words.research {
        return Activity::Exploration;
    }
    if let Some(activity) = earliest(&[
        (words.feature, Activity::Feature),
        (words.debug, Activity::Debugging),
    ]) {
        return activity;
    }
    if words.file || words.script {
        return Activity::Coding;
    }
    if words.url {
        return Activity::Exploration;
    }
    Activity::Conversation
}

/// Names what a turn was about, by CodeBurn's rules (MIT, THIRD_PARTY_NOTICES.md): tools
/// first, then the words of its first message. Inputs: the words and the tools. Output: the
/// activity.
pub fn classify(words: &Words, tools: &Tools) -> Activity {
    match by_tools(words, tools) {
        Some(Activity::Coding) => earliest(&[
            (words.refactor, Activity::Refactoring),
            (words.feature, Activity::Feature),
            (words.debug, Activity::Debugging),
        ])
        .unwrap_or(Activity::Coding),
        Some(Activity::Exploration) if !words.research && words.debug.is_some() => {
            Activity::Debugging
        }
        Some(other) => other,
        None => conversation(words),
    }
}

const EDITS_MAX: usize = 64;
const NO_FILE: &str = "__no_file__";
const MORE_SHELLS: &[&str] = &["BashTool", "PowerShell", "PowerShellTool", "exec"];
const SHELL_WRAPPERS: &[&str] = &["bash", "sh", "zsh"];
const PATCH_HEADS: &[&str] = &["*** Update File:", "*** Add File:"];
const COMMAND_PREFIXES: &[&str] = &[
    "sudo", "doas", "npx", "bunx", "time", "nice", "nohup", "stdbuf", "rtk",
];
const READ_ONLY: &[&str] = &[
    "rg", "grep", "egrep", "fgrep", "ag", "cat", "head", "tail", "less", "more", "ls", "find",
    "fd", "tree", "wc", "stat", "file", "du", "df", "which", "type", "pwd", "printenv", "env",
    "readlink", "realpath", "basename", "dirname", "jq", "diff",
];
const GIT_READ_ONLY: &[&str] = &[
    "log",
    "diff",
    "status",
    "show",
    "blame",
    "grep",
    "shortlog",
    "describe",
    "rev-parse",
    "ls-files",
];

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_activity (
    hour_ms INTEGER NOT NULL,
    project TEXT NOT NULL,
    agent TEXT NOT NULL,
    model TEXT NOT NULL,
    activity TEXT NOT NULL CHECK(activity IN ('coding','debugging','feature','refactoring','testing','exploration','planning','delegation','git','build_deploy','conversation','brainstorming','general')),
    turns INTEGER NOT NULL,
    edit_turns INTEGER NOT NULL,
    first_try_turns INTEGER NOT NULL,
    PRIMARY KEY (hour_ms, project, agent, model, activity)
);";

/// A turn still being read: facts only, kept in the record's progress so a turn split across
/// two scans counts once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct OpenTurn {
    started: bool,
    words: Words,
    tools: Tools,
    model: String,
    message: u64,
    step: u32,
    edits: Vec<(u64, u32)>,
    last_verify: Option<u32>,
    retries: u32,
    judged: bool,
    repeat: bool,
    began_ms: Option<i64>,
}

/// One finished turn, and when its first user line was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Closed {
    pub activity: Activity,
    pub model: String,
    pub edit: bool,
    pub first_try: bool,
    pub repeat: bool,
    pub began_ms: Option<i64>,
}

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn strip_quoted(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut quote = None;
    for character in command.chars() {
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => {}
            None if matches!(character, '\'' | '"') => {
                quote = Some(character);
                out.push(' ');
            }
            None => out.push(character),
        }
    }
    out
}

fn assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

/// Says whether a shell command only looks things up, so it does not verify an edit. Ported
/// from CodeBurn (MIT). Input: the command. Output: true when every part is read-only.
pub(super) fn read_shaped(command: &str) -> bool {
    let stripped = strip_quoted(command).replace("&&", ";");
    let mut seen = false;
    for part in stripped.split([';', '|']) {
        let mut tokens = part
            .split_whitespace()
            .skip_while(|token| assignment(token) || COMMAND_PREFIXES.contains(token));
        let Some(first) = tokens.next() else {
            continue;
        };
        seen = true;
        let name = first.rsplit('/').next().unwrap_or(first);
        let read_only = if name == "git" {
            tokens
                .next()
                .is_some_and(|sub| GIT_READ_ONLY.contains(&sub))
        } else {
            READ_ONLY.contains(&name)
        };
        if !read_only {
            return false;
        }
    }
    seen
}

impl OpenTurn {
    fn begin(&mut self, text: &str, began_ms: Option<i64>) {
        *self = OpenTurn {
            started: true,
            words: words_of(text),
            began_ms,
            ..OpenTurn::default()
        };
    }

    fn close(&mut self) -> Option<Closed> {
        if !self.started {
            return None;
        }
        let closed = Closed {
            activity: classify(&self.words, &self.tools),
            model: std::mem::take(&mut self.model),
            edit: self.tools.edits,
            first_try: self.tools.edits && self.retries == 0,
            repeat: self.repeat,
            began_ms: self.began_ms,
        };
        *self = OpenTurn::default();
        Some(closed)
    }

    /// Decides once whether this turn is a copy, at its first counted record. Input: how to
    /// ask whether that record was seen before.
    pub(super) fn judge(&mut self, known: impl FnOnce() -> bool) {
        if self.started && !self.judged {
            self.judged = true;
            self.repeat = known();
        }
    }

    /// Whether this turn is a copy. Output: the answer so far.
    pub(super) fn repeat(&self) -> bool {
        self.repeat
    }

    fn edited(&mut self, file: Option<&str>) {
        self.tools.edits = true;
        let key = fnv(file.unwrap_or(NO_FILE));
        let step = self.step;
        if let Some((_, previous)) = self.edits.iter_mut().find(|(known, _)| *known == key) {
            if self
                .last_verify
                .is_some_and(|verify| verify > *previous && verify < step)
            {
                self.retries = self.retries.saturating_add(1);
            }
            *previous = step;
        } else if self.edits.len() < EDITS_MAX {
            self.edits.push((key, step));
        }
    }

    fn called(&mut self, name: &str, file: Option<&str>, command: Option<&str>) {
        match name {
            "Edit" | "Write" | "FileEditTool" | "FileWriteTool" | "NotebookEdit" | "write_file"
            | "apply_diff" | "apply_patch" => self.edited(file),
            shell if SHELL_TOOLS.contains(&shell) || MORE_SHELLS.contains(&shell) => {
                self.tools.bash = true;
                if !command.is_some_and(read_shaped) {
                    self.last_verify = Some(self.step);
                }
            }
            "Read" | "Grep" | "Glob" | "FileReadTool" | "GrepTool" | "GlobTool" | "read_file"
            | "read_dir" => self.tools.reads = true,
            "TaskCreate" | "TaskUpdate" | "TaskGet" | "TaskList" | "TaskOutput" | "TaskStop"
            | "TodoWrite" => self.tools.tasks = true,
            "WebSearch" | "WebFetch" | "ToolSearch" => self.tools.search = true,
            "Skill" => self.tools.skill = true,
            "EnterPlanMode" => self.tools.plan_mode = true,
            "Agent" | "Task" | "spawn_agent" | "close_agent" | "wait_agent" => {
                self.tools.agent_spawn = true
            }
            other if other.starts_with("mcp__") => self.tools.mcp = true,
            _ => {}
        }
    }
}

fn user_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let parts = content.as_array()?;
    if parts.iter().any(|part| part["type"] == "tool_result") {
        return None;
    }
    let text = parts
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

/// Follows one Claude transcript line. Inputs: the line and the open turn. Output: the turn
/// that this line closed, if any.
pub(super) fn claude_line(value: &Value, turn: &mut OpenTurn) -> Option<Closed> {
    match value["type"].as_str()? {
        "user" if value["isMeta"] != true => {
            let text = user_text(&value["message"]["content"])?;
            let closed = turn.close();
            turn.begin(&text, line_ms(value));
            closed
        }
        "assistant" if turn.started => {
            let message = &value["message"];
            let id = fnv(message["id"].as_str().unwrap_or(""));
            if id != turn.message {
                turn.message = id;
                turn.step = turn.step.saturating_add(1);
            }
            let model = model_name(&message["model"]);
            if !model.is_empty() {
                turn.model = model;
            }
            for part in message["content"].as_array().map_or(&[][..], Vec::as_slice) {
                if part["type"] == "tool_use" {
                    let input = &part["input"];
                    turn.called(
                        part["name"].as_str().unwrap_or(""),
                        input["file_path"].as_str(),
                        input["command"].as_str(),
                    );
                }
            }
            if message["stop_reason"] == "end_turn" {
                return turn.close();
            }
            None
        }
        _ => None,
    }
}

fn shell_text(command: &Value) -> Option<String> {
    if let Some(text) = command.as_str() {
        return Some(text.to_owned());
    }
    let parts: Vec<&str> = command
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if let [shell, flag, script] = parts[..]
        && SHELL_WRAPPERS.contains(&shell.rsplit('/').next().unwrap_or(shell))
        && flag.starts_with('-')
        && flag.ends_with('c')
    {
        return Some(script.to_owned());
    }
    Some(parts.join(" "))
}

/// The first file a Codex patch updates or adds. Input: the patch text. Output: the path, used
/// only to tell two files apart and never stored.
fn patched_file(patch: &str) -> Option<&str> {
    patch.lines().find_map(|line| {
        PATCH_HEADS
            .iter()
            .find_map(|head| line.strip_prefix(head))
            .map(str::trim)
    })
}

/// Follows one Codex rollout line. Inputs: the line and the open turn. Output: the turn that
/// this line closed, if any.
pub(super) fn codex_line(value: &Value, turn: &mut OpenTurn) -> Option<Closed> {
    let payload = &value["payload"];
    match (value["type"].as_str()?, payload["type"].as_str()) {
        ("event_msg", Some("user_message")) => {
            let text = payload["message"].as_str()?;
            let closed = turn.close();
            turn.begin(text, line_ms(value));
            closed
        }
        ("event_msg", Some("task_complete")) => turn.close(),
        ("turn_context", _) if turn.started => {
            let model = model_name(&payload["model"]);
            if !model.is_empty() {
                turn.model = model;
            }
            None
        }
        ("response_item", Some("function_call" | "custom_tool_call")) if turn.started => {
            turn.step = turn.step.saturating_add(1);
            let arguments: Value = payload["arguments"]
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(Value::Null);
            let command = shell_text(
                arguments
                    .get("command")
                    .or_else(|| arguments.get("cmd"))
                    .unwrap_or(&Value::Null),
            );
            let patch = payload["input"]
                .as_str()
                .or_else(|| arguments["input"].as_str());
            let file = arguments["file_path"]
                .as_str()
                .or_else(|| arguments["path"].as_str())
                .or_else(|| patch.and_then(patched_file));
            turn.called(
                payload["name"].as_str().unwrap_or(""),
                file,
                command.as_deref(),
            );
            None
        }
        ("response_item", Some("local_shell_call")) if turn.started => {
            turn.step = turn.step.saturating_add(1);
            let command = shell_text(&payload["action"]["command"]);
            turn.called("local_shell_call", None, command.as_deref());
            None
        }
        ("response_item", Some("web_search_call")) if turn.started => {
            turn.step = turn.step.saturating_add(1);
            turn.tools.search = true;
            None
        }
        _ => None,
    }
}

/// The time a line was written. Input: the line. Output: the time, None when it has none.
fn line_ms(value: &Value) -> Option<i64> {
    value["timestamp"].as_str().and_then(timestamp_ms)
}

/// Keeps the turns the live scan owns: those begun at or after the recount's start, and those
/// with no time. Until a recount has started, history is the recount's. Inputs: the finished
/// turns and the recount's start. Output: the turns to count now.
pub(super) fn live_turns(closed: Vec<Closed>, recount_started: Option<i64>) -> Vec<Closed> {
    let Some(started_ms) = recount_started else {
        return Vec::new();
    };
    closed
        .into_iter()
        .filter(|turn| turn.began_ms.is_none_or(|at| at >= started_ms))
        .collect()
}

/// Keeps the turns the recount owns: those begun before its start. Inputs: the finished turns
/// and the recount's start. Output: the turns it files.
pub(super) fn history_turns(closed: Vec<Closed>, started_ms: i64) -> Vec<Closed> {
    closed
        .into_iter()
        .filter(|turn| turn.began_ms.is_some_and(|at| at < started_ms))
        .collect()
}

/// Files finished turns. Inputs: the open transaction, the hour, the project and agent that
/// ran them and the turns. Output: done, or `usage_unavailable`.
pub(super) fn add_turns(
    tx: &Connection,
    hour_ms: i64,
    project: &str,
    agent: &str,
    closed: &[Closed],
) -> Result<(), EngineError> {
    for turn in closed {
        if !has_room(tx, "usage_activity")? {
            return Ok(());
        }
        let (models, named): (i64, i64) = tx
            .query_row(
                "SELECT COUNT(DISTINCT model),COALESCE(MAX(model=?4),0) FROM usage_activity
                 WHERE hour_ms=?1 AND project=?2 AND agent=?3",
                params![hour_ms, project, agent, turn.model],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(storage)?;
        let model = if named > 0 || models < MODELS_PER_HOUR_MAX {
            turn.model.as_str()
        } else {
            ""
        };
        tx.execute(
            "INSERT INTO usage_activity (hour_ms,project,agent,model,activity,turns,edit_turns,first_try_turns)
             VALUES (?1,?2,?3,?4,?5,1,?6,?7)
             ON CONFLICT(hour_ms,project,agent,model,activity) DO UPDATE SET
                turns=turns+1,
                edit_turns=edit_turns+excluded.edit_turns,
                first_try_turns=first_try_turns+excluded.first_try_turns",
            params![
                hour_ms,
                project,
                agent,
                model,
                turn.activity.name(),
                i64::from(turn.edit),
                i64::from(turn.first_try),
            ],
        )
        .map_err(storage)?;
    }
    Ok(())
}
