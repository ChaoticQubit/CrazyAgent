use super::prices::Charged;
use super::rules::{CodexState, Usage, advisor_usages, claude_usage, codex_usage, timestamp_ms};
use super::{LimitSource, LimitWindow, Observation, failure, record_observation, storage};
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, ProjectId};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const FILES_PER_SCAN_MAX: usize = 256;
pub const READ_BYTES_MAX: usize = 4 * 1024 * 1024;
const HOUR_MS: i64 = 60 * 60 * 1000;
const FIVE_HOUR_MINUTES: u64 = 300;
const WEEK_MINUTES: u64 = 7 * 24 * 60;
const NAME_BYTES_MAX: usize = 128;
const HEAD_BYTES_MAX: u64 = 4096;
const AGENT_BYTES_MAX: usize = NAME_BYTES_MAX + 64;
const WALK_DEPTH_MAX: usize = 3;
const ENTRIES_PER_SCAN_MAX: usize = FILES_PER_SCAN_MAX * 64;
const RUN_SCAN_MS: i64 = 24 * HOUR_MS;
const FINISHED_RUNS_PER_SCAN: i64 = 64;
/// The most tokens one record line or one provider call counts in each field.
pub const LINE_TOKENS_MAX: u64 = 1_000_000_000;
pub(super) const ROW_TOKENS_MAX: i64 = 1_000_000_000_000_000;
const TOOLS_PER_READ_MAX: usize = 4096;
const ARGUMENTS_BYTES_MAX: usize = 64 * 1024;
/// The most records one read of one file parses before it stops at a line boundary.
pub(super) const USAGES_PER_READ_MAX: usize = 4096;
/// The most rows transcript lines can create in each counted table.
pub(super) const TABLE_ROWS_MAX: i64 = 4 * 1024 * 1024;
/// The records at which one scan stops opening files, so one write under the store's lock
/// holds at most one read more. The rest is read by the next scan.
const USAGES_PER_SCAN_MAX: usize = 8 * 1024;
/// The most scans a finished turn's home gets before its records' progress is forgotten,
/// since each scan reads a record only up to one read's bound: a turn's record is counted
/// whole up to this many reads, `READ_BYTES_MAX` bytes or `USAGES_PER_READ_MAX` records each
/// (COUNT-3-L1).
const TURN_SCANS_MAX: usize = 16;
const MESSAGES_KEPT_MS: i64 = 400 * 24 * HOUR_MS;
const MESSAGES_FORGOTTEN_PER_SCAN: i64 = 4096;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_tokens (
    hour_ms INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('master','agent','research')),
    project TEXT NOT NULL,
    pool TEXT NOT NULL,
    account TEXT NOT NULL,
    agent TEXT NOT NULL,
    harness TEXT NOT NULL,
    session TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    cost_nanos INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (hour_ms, kind, project, pool, account, agent, harness, session, model)
);
CREATE INDEX IF NOT EXISTS usage_tokens_hour ON usage_tokens(hour_ms);
CREATE TABLE IF NOT EXISTS usage_messages (
    id TEXT PRIMARY KEY,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    web INTEGER NOT NULL DEFAULT 0,
    last_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS usage_messages_last ON usage_messages(last_ms);
CREATE TABLE IF NOT EXISTS usage_seen (
    project TEXT NOT NULL,
    agent TEXT NOT NULL,
    session TEXT NOT NULL,
    last_ms INTEGER NOT NULL,
    PRIMARY KEY (project, agent, session)
);
CREATE TABLE IF NOT EXISTS usage_files (
    path TEXT PRIMARY KEY,
    progress TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_runs (
    home TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('master','agent','research')),
    project TEXT NOT NULL,
    account TEXT NOT NULL,
    agent TEXT NOT NULL,
    harness TEXT NOT NULL,
    started_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS usage_runs_started ON usage_runs(started_ms);
CREATE TABLE IF NOT EXISTS usage_refresh (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    requested_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_cut (
    home TEXT PRIMARY KEY,
    project TEXT NOT NULL,
    cut_ms INTEGER NOT NULL
);";

/// Token counts a run used: fresh input, output, input read from the provider's cache and
/// input written to it. Input never includes the cache reads, whatever the harness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    /// The headline count usage shows and budgets spend (DEC-117 Q6): new input, cache
    /// writes and output. Input: the count. Output: their saturating sum.
    pub fn headline(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write)
    }

    /// Adds another count, saturating. Input: the other count. Output: the sum.
    pub fn plus(self, other: Tokens) -> Tokens {
        Tokens {
            input: self.input.saturating_add(other.input),
            output: self.output.saturating_add(other.output),
            cache_read: self.cache_read.saturating_add(other.cache_read),
            cache_write: self.cache_write.saturating_add(other.cache_write),
        }
    }

    /// How much this count grew past an earlier one, field by field, never below zero.
    /// Input: the earlier count. Output: the growth.
    fn growth_over(self, earlier: Tokens) -> Tokens {
        Tokens {
            input: self.input.saturating_sub(earlier.input),
            output: self.output.saturating_sub(earlier.output),
            cache_read: self.cache_read.saturating_sub(earlier.cache_read),
            cache_write: self.cache_write.saturating_sub(earlier.cache_write),
        }
    }

    /// The largest of two counts, field by field. Input: the other count. Output: the result.
    pub fn largest(self, other: Tokens) -> Tokens {
        Tokens {
            input: self.input.max(other.input),
            output: self.output.max(other.output),
            cache_read: self.cache_read.max(other.cache_read),
            cache_write: self.cache_write.max(other.cache_write),
        }
    }

    /// Caps every count at `max`. Input: the cap. Output: the capped count.
    pub fn at_most(self, max: u64) -> Tokens {
        Tokens {
            input: self.input.min(max),
            output: self.output.min(max),
            cache_read: self.cache_read.min(max),
            cache_write: self.cache_write.min(max),
        }
    }

    /// Whether every count is zero. Output: true for an empty count.
    pub fn is_empty(self) -> bool {
        self == Tokens::default()
    }

    /// The tokens a harness reported for a turn, with an unreported count as zero.
    /// Input: the reported usage. Output: the count.
    pub fn reported(usage: &crate::adapters::events::TokenUsage) -> Tokens {
        Tokens {
            input: usage.input_tokens.unwrap_or(0),
            output: usage.output_tokens.unwrap_or(0),
            cache_read: usage.cache_read_input_tokens.unwrap_or(0),
            cache_write: usage.cache_creation_input_tokens.unwrap_or(0),
        }
    }
}

/// The kind of run a transcript belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Master,
    Agent,
    Research,
}

impl RunKind {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Agent => "agent",
            Self::Research => "research",
        }
    }
}

/// Which harness wrote the session records in a home.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Claude,
    Codex,
}

impl Format {
    /// The format of a harness id's session records. Input: the harness id.
    /// Output: the format, None for a harness whose records the engine cannot read.
    pub fn of(harness: &str) -> Option<Format> {
        match crate::harnesses::spec(harness).map(|spec| spec.id) {
            Some("claude-code") => Some(Format::Claude),
            Some("codex") => Some(Format::Codex),
            _ => None,
        }
    }
}

/// Who a run's tokens count against: its kind, project, pool, account, agent and harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribution {
    pub kind: RunKind,
    pub project: Option<ProjectId>,
    pub pool: Option<CredentialRef>,
    pub account: Option<CredentialRef>,
    pub agent: String,
    pub harness: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    identity: (u64, u64),
    offset: u64,
    skipping: bool,
    /// The running total older builds stored. Read once, to start the Codex counters.
    total: Tokens,
    #[serde(default)]
    codex: CodexState,
    /// How far the live scan had filed turns and tools when Use new set this record back.
    #[serde(default)]
    filed_to: u64,
    #[serde(default)]
    turn: super::activity::OpenTurn,
    /// How many of the record's first bytes `head_hash` covers; none in older progress.
    #[serde(default)]
    head_bytes: u64,
    /// The hash of those bytes, the second witness that the progress belongs to this file.
    #[serde(default)]
    head_hash: u64,
}

/// The records of one read, one entry per message. Inputs arrive line by line.
#[derive(Default)]
struct Reading {
    usages: Vec<Usage>,
    by_key: std::collections::HashMap<String, usize>,
    turns: Vec<super::activity::Closed>,
}

impl Reading {
    /// Adds one line's record, folding it into an earlier line of the same message.
    /// Input: the record.
    fn add(&mut self, usage: Usage) {
        let earlier = usage
            .key
            .as_ref()
            .and_then(|key| self.by_key.get(key).copied());
        if let Some(index) = earlier {
            self.usages[index].merge(usage);
            return;
        }
        if let Some(key) = &usage.key {
            self.by_key.insert(key.clone(), self.usages.len());
        }
        self.usages.push(usage);
    }

    /// Whether the read holds its bound. Output: the answer.
    fn full(&self) -> bool {
        self.usages.len() >= USAGES_PER_READ_MAX
    }

    /// Ends the read. Output: the records to count and the finished turns.
    fn finished(self) -> (Vec<Usage>, Vec<super::activity::Closed>) {
        (
            self.usages.into_iter().map(Usage::settled).collect(),
            self.turns,
        )
    }
}

const TURNS_PER_READ_MAX: usize = 4096;
const KEYS_ASKED_MAX: usize = 64 * 1024;

/// What one scan can tell about a turn's first counted record: whether its key is stored or
/// this scan already asked about it, so that an original and its copy read together count once.
pub(super) struct Seen<'a> {
    db: &'a Connection,
    table: &'static str,
    asked: HashSet<String>,
}

impl<'a> Seen<'a> {
    pub(super) fn new(db: &'a Connection, table: &'static str) -> Self {
        Self {
            db,
            table,
            asked: HashSet::new(),
        }
    }

    /// Whether a key was seen before. Input: the key. Output: the answer; past
    /// `KEYS_ASKED_MAX` only stored keys are known, and a store that cannot say answers no.
    fn known(&mut self, key: &str) -> bool {
        let stored = self
            .db
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {} WHERE id=?1)", self.table),
                [key],
                |row| row.get(0),
            )
            .unwrap_or(false);
        let repeated = self.asked.contains(key);
        if self.asked.len() < KEYS_ASKED_MAX {
            self.asked.insert(key.to_owned());
        }
        repeated || stored
    }
}

/// Follows the turn one line belongs to and keeps the turn once it ends, unless it is a copy or
/// it ends on a line the live scan filed before Use new set the record back. Inputs: the line,
/// its format, the progress, the key of the line's own record, what the scan saw, the read
/// and whether the line is past what was filed. Output: whether the line belongs to a copied
/// turn.
fn follow_turn(
    value: &Value,
    format: Format,
    progress: &mut Progress,
    key: Option<&str>,
    seen: Option<&mut Seen<'_>>,
    reading: &mut Reading,
    filing: bool,
) -> bool {
    let replayed = format == Format::Codex && progress.codex.replayed;
    if replayed {
        progress.turn.judge(|| true);
    } else if let Some(key) = key {
        progress
            .turn
            .judge(|| seen.is_some_and(|seen| seen.known(key)));
    }
    let closed = match format {
        Format::Claude => super::activity::claude_line(value, &mut progress.turn),
        Format::Codex => {
            super::activity::codex_line(value, &mut progress.turn).map(|mut closed| {
                if closed.model.is_empty() {
                    closed.model.clone_from(&progress.codex.model);
                }
                closed
            })
        }
    };
    let copied = replayed
        || closed
            .as_ref()
            .map_or(progress.turn.repeat(), |closed| closed.repeat);
    if let Some(closed) = closed
        && !closed.repeat
        && filing
        && reading.turns.len() < TURNS_PER_READ_MAX
    {
        reading.turns.push(closed);
    }
    copied
}

/// One limit window a rollout line reports. The agent writes its own rollout, so a window
/// counts only when it resets within its own length from now. Inputs: the window and the
/// time. Output: its length in minutes and the window, or None.
fn codex_window(value: &Value, now_ms: i64) -> Option<(u64, LimitWindow)> {
    let used = value["used_percent"]
        .as_f64()
        .filter(|used| used.is_finite())?;
    let minutes = value["window_minutes"].as_u64()?;
    let latest = now_ms
        .saturating_add(i64::try_from(minutes).ok()?.saturating_mul(60_000))
        .min(super::RESET_MS_MAX);
    let resets_at_ms = match (
        value["resets_at"].as_i64(),
        value["resets_in_seconds"].as_i64(),
    ) {
        (Some(at), _) => at.checked_mul(1000),
        (None, Some(within)) => within
            .checked_mul(1000)
            .and_then(|ms| now_ms.checked_add(ms)),
        (None, None) => None,
    }
    .filter(|at| (1..=latest).contains(at))?;
    Some((
        minutes,
        LimitWindow {
            used_percent: used.clamp(0.0, 100.0),
            resets_at_ms: Some(resets_at_ms),
        },
    ))
}

/// What one Codex rollout line shows: the record it reports, advancing the rollout's
/// counters, and the account's limit windows when the line carries them. Inputs: the parsed
/// line, the file's progress, the time and the thread to use when no line named one.
/// Output: the record and the limits.
fn codex_line(
    value: &Value,
    progress: &mut Progress,
    now_ms: i64,
    fallback_thread: &str,
) -> (Option<Usage>, Option<Observation>) {
    let usage = codex_usage(value, progress.total, &mut progress.codex, fallback_thread);
    let payload = &value["payload"];
    if value["type"] != "event_msg" || payload["type"] != "token_count" {
        return (usage, None);
    }
    let limits = &payload["rate_limits"];
    let mut observation = Observation {
        plan: limits["plan_type"]
            .as_str()
            .filter(|plan| super::checked_text(Some(plan), super::PLAN_BYTES_MAX).is_ok())
            .map(str::to_owned),
        five_hour: None,
        weekly: None,
        provider: None,
    };
    for key in ["primary", "secondary"] {
        match codex_window(&limits[key], now_ms) {
            Some((FIVE_HOUR_MINUTES, window)) => observation.five_hour = Some(window),
            Some((WEEK_MINUTES, window)) => observation.weekly = Some(window),
            _ => {}
        }
    }
    let reported = observation.five_hour.is_some() || observation.weekly.is_some();
    (usage, reported.then_some(observation))
}

/// Names the tool call one Codex rollout line records, the way adoption counts it: a
/// function or custom tool call by its name and parsed arguments, a local shell call by its
/// command, and a skill it read by `skill:` and the skill's name too. Arguments longer than
/// `ARGUMENTS_BYTES_MAX` count the call by its name alone. Input: the parsed line. Output:
/// the counted names, at most two, none for any other line.
fn codex_tools(value: &Value) -> Vec<String> {
    let payload = &value["payload"];
    if value["type"] != "response_item" {
        return Vec::new();
    }
    let (name, input) = match payload["type"].as_str() {
        Some("function_call") => (
            payload["name"].as_str().unwrap_or_default(),
            payload["arguments"]
                .as_str()
                .filter(|arguments| arguments.len() <= ARGUMENTS_BYTES_MAX)
                .and_then(|arguments| serde_json::from_str(arguments).ok())
                .unwrap_or(Value::Null),
        ),
        Some("custom_tool_call") => (payload["name"].as_str().unwrap_or_default(), Value::Null),
        Some("local_shell_call") => ("local_shell_call", payload["action"].clone()),
        _ => return Vec::new(),
    };
    let names: Vec<String> = crate::coord::adoption::tool_key(name, &input)
        .into_iter()
        .chain(crate::coord::adoption::skill_key(name, &input))
        .collect();
    assert!(names.len() <= 2);
    names
}

/// Names the tool calls one Claude Code transcript line records, the way adoption counts
/// them: each `tool_use` item of an assistant line by its name and input, and a skill it
/// called or read by `skill:` and the skill's name too. Input: the parsed line. Output: the
/// counted names, at most two a call, none for any other line.
fn claude_tools(value: &Value) -> Vec<String> {
    if value["type"] != "assistant" {
        return Vec::new();
    }
    let mut names = Vec::new();
    for item in value["message"]["content"].as_array().into_iter().flatten() {
        if item["type"] != "tool_use" {
            continue;
        }
        let name = item["name"].as_str().unwrap_or_default();
        let called: Vec<String> = crate::coord::adoption::tool_key(name, &item["input"])
            .into_iter()
            .chain(crate::coord::adoption::skill_key(name, &item["input"]))
            .collect();
        assert!(called.len() <= 2);
        names.extend(called);
    }
    names
}

/// Lists a home's session records, at most `FILES_PER_SCAN_MAX`, most recently changed
/// first by [`changed_at`]: Claude Code's `projects/<folder>/<session>.jsonl` with its
/// sub-agent records further down, also in the home a native lead's teammates run with
/// beside it (TPH-2 C), or Codex's `sessions/<year>/<month>/<day>/rollout-*.jsonl`. A
/// linked top folder is never walked. The walk examines at most `ENTRIES_PER_SCAN_MAX`
/// entries and `FILES_PER_SCAN_MAX * 4` folders of the home, entering the most recently
/// changed folder first, and keeps only the newest records it saw, so a home an agent filled
/// costs a bounded scan that leaves out its least recently changed folders. Inputs: the home
/// and the format. Output: the paths, the records it saw but left out with their identity
/// and length, fewer than `ENTRIES_PER_SCAN_MAX` since each is one examined entry, and
/// whether the walk stopped at a bound before it saw every entry.
pub(crate) fn records(home: &Path, format: Format) -> (Vec<PathBuf>, Vec<LeftOut>, bool) {
    let mut folders = match format {
        Format::Claude => vec![
            (crate::launch_context::mates_home(home).join("projects"), 0),
            (home.join("projects"), 0),
        ],
        Format::Codex => vec![(home.join("sessions"), 0)],
    };
    let mut newest = std::collections::BinaryHeap::with_capacity(FILES_PER_SCAN_MAX + 1);
    let mut visited = 0;
    let mut examined = 0;
    let mut cut = false;
    let mut left = Vec::new();
    let changed = |entry: &std::fs::DirEntry| {
        entry
            .metadata()
            .map_or(std::time::SystemTime::UNIX_EPOCH, |metadata| {
                changed_at(&metadata)
            })
    };
    'walk: while let Some((folder, level)) = folders.pop() {
        visited += 1;
        if visited > FILES_PER_SCAN_MAX * 4 {
            cut = true;
            break;
        }
        if level == 0 && !std::fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.is_dir())
        {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut below = Vec::new();
        for entry in entries.flatten() {
            if examined == ENTRIES_PER_SCAN_MAX {
                cut = true;
                break 'walk;
            }
            examined += 1;
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if kind.is_dir() && level < WALK_DEPTH_MAX {
                below.push((changed(&entry), entry.path()));
            } else if kind.is_file()
                && name.ends_with(".jsonl")
                && (format == Format::Claude || name.starts_with("rollout-"))
            {
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                newest.push((
                    std::cmp::Reverse(changed_at(&metadata)),
                    entry.path(),
                    file_identity(&metadata),
                    metadata.len(),
                ));
                if newest.len() > FILES_PER_SCAN_MAX
                    && let Some((_, path, identity, length)) = newest.pop()
                {
                    left.push(LeftOut {
                        path,
                        identity,
                        length,
                    });
                }
            }
        }
        below.sort_unstable();
        folders.extend(below.into_iter().map(|(_, path)| (path, level + 1)));
    }
    assert!(examined <= ENTRIES_PER_SCAN_MAX && newest.len() <= FILES_PER_SCAN_MAX);
    assert!(cut || folders.is_empty());
    assert!(left.len() + newest.len() <= examined);
    assert!(left.is_empty() || newest.len() == FILES_PER_SCAN_MAX);
    let paths = newest
        .into_sorted_vec()
        .into_iter()
        .map(|(_, path, _, _)| path)
        .collect();
    (paths, left, cut)
}

/// A record the walk saw but left out: its path, its file identity and its length then. One
/// scan holds fewer than `ENTRIES_PER_SCAN_MAX` of them.
pub(crate) struct LeftOut {
    pub(super) path: PathBuf,
    identity: (u64, u64),
    length: u64,
}

/// The identity a record's progress is kept under: its device and inode on unix, none
/// elsewhere, as [`open_record`] reads it. Input: the metadata. Output: the identity.
#[cfg(unix)]
pub(super) fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

/// The identity a record's progress is kept under: none on this platform, as
/// [`open_record`] reads it. Input: the metadata. Output: the identity.
#[cfg(not(unix))]
pub(super) fn file_identity(_: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// When a walked entry last changed, by a clock no agent can set: the inode change time,
/// which every write and every change of a file's dates moves to now, so an agent can never
/// make a record it wrote, or its folder, look older. Input: the entry's metadata. Output:
/// the time, the epoch for a time before it.
#[cfg(unix)]
fn changed_at(metadata: &std::fs::Metadata) -> std::time::SystemTime {
    use std::os::unix::fs::MetadataExt;
    let seconds = u64::try_from(metadata.ctime()).unwrap_or(0);
    let nanos = u32::try_from(metadata.ctime_nsec()).unwrap_or(0) % 1_000_000_000;
    std::time::UNIX_EPOCH
        .checked_add(std::time::Duration::new(seconds, nanos))
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// When a walked entry last changed: its modification time, as this platform has no inode
/// change time. Input: the entry's metadata. Output: the time, the epoch when unreadable.
#[cfg(not(unix))]
fn changed_at(metadata: &std::fs::Metadata) -> std::time::SystemTime {
    metadata
        .modified()
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
}

/// Keeps whether the last scan of a home left usage uncounted, so the usage page can say its
/// reading is partial: the walk stopped at a bound, or a record it left out is [`uncounted`]
/// (MSL2B-R3-1), reading at most `READ_BYTES_MAX` bytes of their tails in all (CUT-L1). A cut
/// scan stores the home with the time, keeping the `FILES_PER_SCAN_MAX` most recently cut
/// homes; a whole scan removes it. Inputs: the store, the home, its attribution, the records
/// left out, whether the walk stopped and the time. Output: done, or `usage_unavailable`.
fn mark_cut(
    db: &Connection,
    home: &Path,
    attribution: &Attribution,
    left: &[LeftOut],
    stopped: bool,
    now_ms: i64,
) -> Result<(), EngineError> {
    let key = home.to_string_lossy();
    let mut cut = stopped;
    let mut budget = READ_BYTES_MAX as u64;
    for record in left {
        if cut {
            break;
        }
        cut = uncounted(db, record, &mut budget)?;
    }
    if !cut {
        let stored: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM usage_cut WHERE home=?1)",
                [key.as_ref()],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if stored {
            db.execute("DELETE FROM usage_cut WHERE home=?1", [key.as_ref()])
                .map_err(storage)?;
        }
        return Ok(());
    }
    db.execute(
        "INSERT INTO usage_cut (home,project,cut_ms) VALUES (?1,?2,?3)
         ON CONFLICT(home) DO UPDATE SET project=excluded.project,cut_ms=excluded.cut_ms",
        params![
            key,
            text(attribution.project.as_ref().map(ProjectId::as_str)),
            now_ms
        ],
    )
    .map_err(storage)?;
    db.execute(
        "DELETE FROM usage_cut WHERE home NOT IN
         (SELECT home FROM usage_cut ORDER BY cut_ms DESC LIMIT ?1)",
        [FILES_PER_SCAN_MAX as i64],
    )
    .map_err(storage)?;
    Ok(())
}

/// Whether a record the walk left out holds lines no scan counted: it is not empty and its
/// stored progress is missing or belongs to another file, or the bytes past its stored
/// offset hold a line a scan would count (CUT-L1). Inputs: the store, the record and the
/// tail bytes this scan may still read. Output: the answer, or `usage_unavailable`.
fn uncounted(db: &Connection, record: &LeftOut, budget: &mut u64) -> Result<bool, EngineError> {
    if record.length == 0 {
        return Ok(false);
    }
    let progress = stored_progress(db, &record.path.to_string_lossy())?;
    if progress.identity != record.identity {
        return Ok(true);
    }
    Ok(progress.offset < record.length && countable_line(record, &progress, budget))
}

/// Whether the bytes of a left-out record from its stored offset to the length the walk saw
/// end a line a scan would count. A last line still being written or torn by a stopped agent
/// ends none, and the first ending closes the rest of a line the reads are skipping. Spends
/// the tail on `budget`; a tail past it, or a record replaced or unreadable since the walk,
/// answers true. Inputs: the record, its progress and the budget. Output: the answer.
fn countable_line(record: &LeftOut, progress: &Progress, budget: &mut u64) -> bool {
    let tail = record.length - progress.offset;
    if tail > *budget {
        return true;
    }
    *budget -= tail;
    let Some((mut file, identity, _)) = open_record(&record.path) else {
        return true;
    };
    let mut bytes = Vec::new();
    if identity != record.identity
        || file.seek(SeekFrom::Start(progress.offset)).is_err()
        || file.take(tail).read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 != tail
    {
        return true;
    }
    let endings = bytes.iter().filter(|byte| **byte == b'\n').count();
    endings > usize::from(progress.skipping)
}

#[cfg(unix)]
pub(crate) fn open_record(path: &Path) -> Option<(File, (u64, u64), u64)> {
    use std::os::unix::fs::MetadataExt;
    let file = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()
    .map(File::from)?;
    let metadata = file.metadata().ok()?;
    metadata
        .is_file()
        .then(|| (file, (metadata.dev(), metadata.ino()), metadata.len()))
}

#[cfg(not(unix))]
pub(crate) fn open_record(path: &Path) -> Option<(File, (u64, u64), u64)> {
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    metadata.is_file().then_some((file, (0, 0), metadata.len()))
}

pub(super) fn session_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut name: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c))
        .take(NAME_BYTES_MAX)
        .collect();
    if name.is_empty() {
        name.push('-');
    }
    name
}

/// Reads what one record gained since its stored progress, at most `READ_BYTES_MAX`.
/// A replaced or shortened record starts over; one whose volume a reboot renumbered keeps
/// its place. A line longer than the read is skipped.
/// Inputs: the record, its progress and the time. Output: the records and the last limits.
#[cfg(all(test, unix))]
fn read_record(
    path: &Path,
    format: Format,
    progress: &mut Progress,
    now_ms: i64,
) -> (Vec<Usage>, Option<Observation>) {
    let Some(opened) = open_record(path) else {
        return (Vec::new(), None);
    };
    read_opened(opened, format, progress, now_ms, &session_name(path))
}

/// One tool call a record shows, and when its line was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ToolCall {
    pub name: String,
    pub at_ms: Option<i64>,
}

/// Reads what one record gained since its stored progress and follows the turns of its new
/// lines. Inputs: the record, its progress, the time, where to collect the tool calls, if
/// anywhere, and what the scan saw. Output: the records, the finished turns and the last limits.
fn read_record_with(
    path: &Path,
    format: Format,
    progress: &mut Progress,
    now_ms: i64,
    tools: Option<&mut Vec<ToolCall>>,
    seen: Option<&mut Seen<'_>>,
) -> (
    Vec<Usage>,
    Vec<super::activity::Closed>,
    Option<Observation>,
) {
    let Some(opened) = open_record(path) else {
        return (Vec::new(), Vec::new(), None);
    };
    read_opened_with(
        opened,
        format,
        progress,
        now_ms,
        tools,
        &session_name(path),
        seen,
    )
}

#[cfg(all(test, unix))]
fn read_opened(
    opened: (File, (u64, u64), u64),
    format: Format,
    progress: &mut Progress,
    now_ms: i64,
    fallback_thread: &str,
) -> (Vec<Usage>, Option<Observation>) {
    let (usages, _, limits) = read_opened_with(
        opened,
        format,
        progress,
        now_ms,
        None,
        fallback_thread,
        None,
    );
    (usages, limits)
}

/// Hashes the first `bytes` bytes of an open record. Inputs: the record and the count, at
/// most `HEAD_BYTES_MAX`. Output: the first eight bytes of their SHA-256 as a number, or
/// none when the record is shorter or cannot be read.
fn head_hash(file: &mut File, bytes: u64) -> Option<u64> {
    use sha2::{Digest, Sha256};
    assert!(bytes <= HEAD_BYTES_MAX);
    let mut head = Vec::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.by_ref().take(bytes).read_to_end(&mut head).ok()?;
    if head.len() as u64 != bytes {
        return None;
    }
    let digest = Sha256::digest(head.as_slice());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    Some(u64::from_be_bytes(first))
}

/// Whether an open record still starts with the bytes its progress hashed: the second
/// witness beside the inode, which Linux file systems hand to the next file made after a
/// removal. Progress that hashed nothing holds, and stored progress whose hash covers more
/// than it read does not. Inputs: the record and its progress. Output: the answer, or none
/// when the start cannot be read now.
fn head_holds(file: &mut File, progress: &Progress) -> Option<bool> {
    if progress.head_bytes == 0 {
        return Some(true);
    }
    if progress.head_bytes > progress.offset.min(HEAD_BYTES_MAX) {
        return Some(false);
    }
    let held = head_hash(file, progress.head_bytes)? == progress.head_hash;
    assert!(progress.head_bytes <= HEAD_BYTES_MAX);
    Some(held)
}

/// Hashes the start of a record up to where its progress has read, at most
/// `HEAD_BYTES_MAX`, once the progress moved past what the stored hash covers. A start
/// that cannot be read keeps the stored hash. Inputs: the record and its progress.
fn witness_head(file: &mut File, progress: &mut Progress) {
    let bytes = progress.offset.min(HEAD_BYTES_MAX);
    assert!(progress.head_bytes <= bytes);
    if bytes == progress.head_bytes {
        return;
    }
    if let Some(hash) = head_hash(file, bytes) {
        progress.head_bytes = bytes;
        progress.head_hash = hash;
    }
    assert!(progress.head_bytes <= progress.offset);
}

/// Reads what an open record gained since its stored progress, up to the length it had
/// when it was opened and at most `READ_BYTES_MAX`, so lines a harness appends meanwhile
/// wait for the next scan. The record is the one the progress pinned while its inode is
/// the same, under the crate-wide volume renumber rule, and its first bytes still hash to
/// what the progress holds; the pin then moves to the live device, so a reboot never counts
/// a whole record again. Inputs: the open record with its
/// identity and length, its progress, the time and where to collect the tool calls read,
/// at most `TOOLS_PER_READ_MAX`, if anywhere, the thread to use when no line named one and
/// what the scan saw. Output: the records, the finished turns and the last limits.
fn read_opened_with(
    (mut file, identity, length): (File, (u64, u64), u64),
    format: Format,
    progress: &mut Progress,
    now_ms: i64,
    mut tools: Option<&mut Vec<ToolCall>>,
    fallback_thread: &str,
    mut seen: Option<&mut Seen<'_>>,
) -> (
    Vec<Usage>,
    Vec<super::activity::Closed>,
    Option<Observation>,
) {
    let pinned = crate::file_identity::same_files(&[progress.identity], &[identity])
        && length >= progress.offset;
    let held = if pinned && length > progress.offset {
        let Some(held) = head_holds(&mut file, progress) else {
            return (Vec::new(), Vec::new(), None);
        };
        held
    } else {
        pinned
    };
    if held {
        progress.identity = identity;
    } else {
        *progress = Progress {
            identity,
            ..Progress::default()
        };
    }
    if length == progress.offset || file.seek(SeekFrom::Start(progress.offset)).is_err() {
        return (Vec::new(), Vec::new(), None);
    }
    let mut bytes = Vec::new();
    if file
        .by_ref()
        .take((length - progress.offset).min(READ_BYTES_MAX as u64))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return (Vec::new(), Vec::new(), None);
    }
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        if bytes.len() == READ_BYTES_MAX {
            progress.offset += bytes.len() as u64;
            progress.skipping = true;
            witness_head(&mut file, progress);
        }
        return (Vec::new(), Vec::new(), None);
    };
    let mut reading = Reading::default();
    let mut limits = None;
    let mut consumed: u64 = 0;
    for line in bytes[..end].split(|byte| *byte == b'\n') {
        if reading.full() {
            break;
        }
        consumed += line.len() as u64 + 1;
        if std::mem::take(&mut progress.skipping) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let mut key: Option<String> = None;
        match format {
            Format::Claude => {
                if let Some(usage) = claude_usage(&value) {
                    key.clone_from(&usage.key);
                    reading.add(usage);
                }
                for usage in advisor_usages(&value) {
                    reading.add(usage);
                }
            }
            Format::Codex => {
                let (usage, observed) = codex_line(&value, progress, now_ms, fallback_thread);
                if let Some(usage) = usage {
                    key.clone_from(&usage.key);
                    reading.add(usage);
                }
                limits = observed.or(limits);
            }
        }
        let already_filed = progress.offset + consumed <= progress.filed_to;
        let copied = follow_turn(
            &value,
            format,
            progress,
            key.as_deref(),
            seen.as_deref_mut(),
            &mut reading,
            !already_filed,
        );
        if let Some(tools) = tools.as_deref_mut().filter(|_| !copied && !already_filed) {
            let called = match format {
                Format::Claude => claude_tools(&value),
                Format::Codex => codex_tools(&value),
            };
            let at_ms = value["timestamp"].as_str().and_then(timestamp_ms);
            for name in called {
                if tools.len() < TOOLS_PER_READ_MAX {
                    tools.push(ToolCall { name, at_ms });
                }
            }
        }
    }
    progress.offset += consumed;
    assert!(progress.offset <= length);
    witness_head(&mut file, progress);
    assert!(tools.is_none_or(|tools| tools.len() <= TOOLS_PER_READ_MAX));
    assert!(reading.turns.len() <= TURNS_PER_READ_MAX);
    let (usages, turns) = reading.finished();
    (usages, turns, limits)
}

fn stored_progress(db: &Connection, key: &str) -> Result<Progress, EngineError> {
    let stored: Option<String> = db
        .query_row(
            "SELECT progress FROM usage_files WHERE path=?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    match stored {
        Some(text) => serde_json::from_str(&text).map_err(storage),
        None => Ok(Progress::default()),
    }
}

/// Counts what every session record in one agent home gained since the last scan and adds
/// it to the run's attribution in the hour of `now_ms`; Codex limit windows the records
/// carry update the account's limits, from the most recently changed record that has one.
/// Every record is read before the store's write lock
/// is taken, and a record whose stored progress moved meanwhile is left to the scan that
/// moved it, so no file read holds the lock and no line counts twice.
/// Inputs: the store, the home, its format, the attribution and the time.
/// Output: the tokens added, or `usage_unavailable`.
pub fn scan_home(
    db: &Connection,
    home: &Path,
    format: Format,
    attribution: &Attribution,
    now_ms: i64,
) -> Result<Tokens, EngineError> {
    scan(db, home, format, attribution, None, now_ms).map(|(added, _)| added)
}

/// Counts a team agent's home as `scan_home` does and also counts each tool call the new
/// lines record against the agent for the coordination layer, in the same write as the
/// tokens, so each call counts once: a Codex agent's always, and a Claude Code agent's only
/// when its launch fires no hooks (DEC-104 bare-hooks), since hooks count the others.
/// Inputs: the store, the home, its format, the attribution, the agent and the time.
/// Output: the tokens added, or a plain code.
pub fn scan_agent_home(
    db: &Connection,
    home: &Path,
    format: Format,
    attribution: &Attribution,
    agent: i64,
    now_ms: i64,
) -> Result<Tokens, EngineError> {
    scan(db, home, format, attribution, Some(agent), now_ms).map(|(added, _)| added)
}

/// Counts what a home's records gained, as [`scan_home`] describes. Inputs: the store, the
/// home, its format, the attribution, the agent whose tool calls it counts and the time.
/// Output: the tokens added and whether any record's read moved its progress, so a caller
/// that must count a home to its end knows to scan again; or a plain code.
fn scan(
    db: &Connection,
    home: &Path,
    format: Format,
    attribution: &Attribution,
    agent: Option<i64>,
    now_ms: i64,
) -> Result<(Tokens, bool), EngineError> {
    checked_attribution(attribution)?;
    let counting = match agent {
        Some(id) if format == Format::Codex || launched_without_hooks(db, id)? => Some(id),
        _ => None,
    };
    let (paths, left, stopped) = records(home, format);
    mark_cut(db, home, attribution, &left, stopped, now_ms)?;
    let mut changed = Vec::new();
    let mut budget = USAGES_PER_SCAN_MAX;
    let mut seen = Seen::new(db, Messages::Live { file: None }.table());
    for path in paths {
        if budget == 0 {
            break;
        }
        let key = path.to_string_lossy().into_owned();
        let before = stored_progress(db, &key)?;
        let mut progress = before.clone();
        let mut tools = Vec::new();
        let (usages, turns, observed) = read_record_with(
            &path,
            format,
            &mut progress,
            now_ms,
            counting.is_some().then_some(&mut tools),
            Some(&mut seen),
        );
        budget = budget.saturating_sub(usages.len());
        if progress != before {
            changed.push((
                key,
                session_name(&path),
                before,
                progress,
                usages,
                turns,
                observed,
                tools,
            ));
        }
    }
    if changed.is_empty() {
        return Ok((Tokens::default(), false));
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    let tracking = super::recount::keeps_live_keys(&tx)?;
    let recount_started = super::recount::started_ms(&tx)?;
    let mut added = Tokens::default();
    let mut limits = None;
    for (key, session, before, progress, usages, turns, observed, tools) in changed {
        if stored_progress(&tx, &key)? != before {
            continue;
        }
        if let Some(agent) = counting {
            for tool in &tools {
                crate::coord::count_tool(
                    &tx,
                    agent,
                    &tool.name,
                    now_ms,
                    super::activity::live_owns(tool.at_ms, recount_started),
                )?;
            }
        }
        tx.execute(
            "INSERT INTO usage_files (path,progress) VALUES (?1,?2)
             ON CONFLICT(path) DO UPDATE SET progress=excluded.progress",
            params![key, serde_json::to_string(&progress).map_err(storage)?],
        )
        .map_err(storage)?;
        super::activity::add_turns(
            &tx,
            now_ms - now_ms.rem_euclid(HOUR_MS),
            attribution.project.as_ref().map_or("", ProjectId::as_str),
            &attribution.agent,
            &super::activity::live_turns(turns, recount_started),
        )?;
        for (model, growth) in count_usages(
            &tx,
            Messages::Live {
                file: tracking.then_some(key.as_str()),
            },
            &usages,
            now_ms,
        )? {
            add_tokens(&tx, now_ms, attribution, &session, &model, growth)?;
            added = added.plus(growth.tokens);
        }
        limits = limits.or(observed);
    }
    if let (Some(observation), Some(account)) = (limits, &attribution.account) {
        record_observation(&tx, account, &observation, LimitSource::Transcript, now_ms)?;
    }
    tx.commit().map_err(storage)?;
    Ok((added, true))
}

/// Says whether a team agent's launch fires no hooks (`team_agents.hookless`). Inputs: the
/// store and the agent. Output: the answer, false for an agent the store does not hold, or
/// `usage_unavailable`.
fn launched_without_hooks(db: &Connection, agent: i64) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT hookless FROM team_agents WHERE id=?1",
        [agent],
        |row| row.get(0),
    )
    .optional()
    .map(Option::unwrap_or_default)
    .map_err(storage)
}

fn text(value: Option<&str>) -> &str {
    value.unwrap_or("")
}

/// What one bounded read of a record gave the recount: its records, the record's progress,
/// the bytes consumed, whether the record is finished, whether it was read to its end, and
/// the turns and tool calls it finished.
pub(super) struct RecountRead {
    pub usages: Vec<Usage>,
    pub turns: Vec<super::activity::Closed>,
    pub tools: Vec<ToolCall>,
    pub progress: String,
    pub read: u64,
    pub done: bool,
    pub complete: bool,
}

/// Reads one more chunk of a record for the recount, through the live scan's open and parser.
/// A record that cannot be opened, is no longer the file that was listed, has shrunk or no
/// longer starts with the bytes the last chunk hashed is finished and not complete. A turn is a copy when the recount already counted its first
/// message. Inputs: the store, the record, its format, the stored progress (empty at the
/// start), the identity it was listed with and the time. Output: the read.
pub(super) fn recount_read(
    db: &Connection,
    path: &Path,
    format: Format,
    stored: &str,
    listed: (u64, u64),
    now_ms: i64,
) -> RecountRead {
    let mut progress: Progress = serde_json::from_str(stored).unwrap_or_default();
    let before = progress.offset;
    let stopped = RecountRead {
        usages: Vec::new(),
        turns: Vec::new(),
        tools: Vec::new(),
        progress: stored.to_owned(),
        read: 1,
        done: true,
        complete: false,
    };
    let Some(mut opened) = open_record(path) else {
        return stopped;
    };
    let (identity, length) = (opened.1, opened.2);
    if !crate::file_identity::same_files(&[listed], &[identity])
        || length < before
        || head_holds(&mut opened.0, &progress) != Some(true)
    {
        return stopped;
    }
    let mut tools = Vec::new();
    let mut seen = Seen::new(db, "usage_recount_messages");
    let (usages, turns, _) = read_opened_with(
        opened,
        format,
        &mut progress,
        now_ms,
        Some(&mut tools),
        &session_name(path),
        Some(&mut seen),
    );
    let whole = progress.offset >= length;
    let done = whole || progress.offset == before;
    let complete = whole || (done && only_an_unfinished_line(path, before, length));
    let read = progress.offset.saturating_sub(before).max(1);
    let progress = serde_json::to_string(&progress).expect("progress always serializes");
    RecountRead {
        usages,
        turns,
        tools,
        progress,
        read,
        done,
        complete,
    }
}

/// Whether what is left of a record is one line still being written, which holds nothing to
/// count yet. Inputs: the record, where the read stopped and the record's length. Output: the
/// answer; a record that cannot be read has no such answer and gets false.
fn only_an_unfinished_line(path: &Path, from: u64, length: u64) -> bool {
    let Some((mut file, _, _)) = open_record(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    length - from < READ_BYTES_MAX as u64
        && file.seek(SeekFrom::Start(from)).is_ok()
        && file.take(length - from).read_to_end(&mut bytes).is_ok()
        && !bytes.contains(&b'\n')
}

/// Lists every record of a home, the ones a live scan leaves out too. Inputs: the home and its
/// format. Output: the records, and whether the walk was cut short.
pub(super) fn all_records(home: &Path, format: Format) -> (Vec<PathBuf>, bool) {
    let (mut paths, left, cut) = records(home, format);
    paths.extend(left.into_iter().map(|record| record.path));
    (paths, cut)
}

/// Which table holds the keys already counted: the live store's or the recount's. `file`
/// names the record a live key came from while a recount is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Messages<'a> {
    Live { file: Option<&'a str> },
    Recount,
}

impl Messages<'_> {
    fn table(self) -> &'static str {
        match self {
            Self::Live { .. } => "usage_messages",
            Self::Recount => "usage_recount_messages",
        }
    }
}

/// Counts what one record adds over what its key already counted. Inputs: the open write,
/// the key table, the record and the time. Output: the tokens and web searches it adds.
fn count_message(
    tx: &Connection,
    table: Messages<'_>,
    usage: &Usage,
    now_ms: i64,
) -> Result<(Tokens, u64), EngineError> {
    let Some(key) = &usage.key else {
        return Ok((usage.tokens, usage.web_searches));
    };
    let name = table.table();
    let prior: Option<(Tokens, u64)> = tx
        .query_row(
            &format!("SELECT input,output,cache_read,cache_write,web FROM {name} WHERE id=?1"),
            [key],
            |row| {
                let count = |index: usize| -> rusqlite::Result<u64> {
                    Ok(row.get::<_, i64>(index)?.max(0) as u64)
                };
                Ok((
                    Tokens {
                        input: count(0)?,
                        output: count(1)?,
                        cache_read: count(2)?,
                        cache_write: count(3)?,
                    },
                    count(4)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?;
    if prior.is_none() && !has_room(tx, name)? {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| {
            eprintln!("usage: message keys are at their cap, repeats of new messages count again")
        });
        return Ok((usage.tokens, usage.web_searches));
    }
    let (before, web_before) = prior.unwrap_or_default();
    let largest = before.largest(usage.tokens);
    let web = web_before.max(usage.web_searches);
    tx.execute(
        &format!(
            "INSERT INTO {name} (id,input,output,cache_read,cache_write,web,last_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(id) DO UPDATE SET input=excluded.input,output=excluded.output,
                cache_read=excluded.cache_read,cache_write=excluded.cache_write,
                web=excluded.web,last_ms=excluded.last_ms"
        ),
        params![
            key,
            clamp(largest.input),
            clamp(largest.output),
            clamp(largest.cache_read),
            clamp(largest.cache_write),
            clamp(web),
            now_ms
        ],
    )
    .map_err(storage)?;
    let growth = usage.tokens.growth_over(before);
    if let Messages::Live { file: Some(file) } = table
        && (prior.is_none() || !growth.is_empty())
        && has_room(tx, "usage_recount_live")?
    {
        tx.execute(
            "INSERT OR IGNORE INTO usage_recount_live (id,path) VALUES (?1,?2)",
            params![key, file],
        )
        .map_err(storage)?;
    }
    if usage.exact && prior.is_some() {
        return Ok((Tokens::default(), 0));
    }
    Ok((growth, web - web_before))
}

/// Counts one read's records, each priced when it is counted, because the long-context tier
/// and the one-hour write are per call. A repeat pays the one-hour rate only past what its
/// earlier copies wrote, the least its one-hour part can have grown, so it never reads over.
/// Inputs: the open write, the key table, the records and the time. Output: what each model
/// added.
pub(super) fn count_usages(
    tx: &Connection,
    table: Messages<'_>,
    usages: &[Usage],
    now_ms: i64,
) -> Result<BTreeMap<String, Added>, EngineError> {
    let mut by_model: BTreeMap<String, Added> = BTreeMap::new();
    for usage in usages {
        let (growth, web) = count_message(tx, table, usage, now_ms)?;
        let charged = Charged {
            tokens: growth,
            prompt_tokens: usage
                .tokens
                .input
                .saturating_add(usage.tokens.cache_read)
                .saturating_add(usage.tokens.cache_write),
            one_hour: usage
                .one_hour
                .saturating_sub(usage.tokens.cache_write.saturating_sub(growth.cache_write)),
            web_searches: web,
            fast: usage.fast,
        };
        let cost = super::prices::table()
            .cost_nanos(&usage.model, charged)
            .unwrap_or(0);
        let row = by_model.entry(usage.model.clone()).or_default();
        row.tokens = row.tokens.plus(growth);
        row.cost_nanos = row.cost_nanos.saturating_add(cost);
    }
    Ok(by_model)
}

/// Forgets counted keys past their age, a bounded number per scan. Inputs: the store and the
/// time. Output: done, or `usage_unavailable`.
fn forget_old_messages(db: &Connection, now_ms: i64) -> Result<(), EngineError> {
    db.execute(
        "DELETE FROM usage_messages WHERE id IN
            (SELECT id FROM usage_messages WHERE last_ms<?1 LIMIT ?2)",
        params![now_ms - MESSAGES_KEPT_MS, MESSAGES_FORGOTTEN_PER_SCAN],
    )
    .map_err(storage)?;
    Ok(())
}

/// The most models one session keeps rows for in one hour; later models file under `''`.
pub(super) const MODELS_PER_HOUR_MAX: i64 = 16;
/// The tables whose rows transcript lines create.
const COUNTED_TABLES: &[&str] = &[
    "usage_messages",
    "usage_tokens",
    "usage_activity",
    "usage_recount_messages",
    "usage_recount_rows",
    "usage_recount_live",
];

/// A usage row's key without its session and model.
pub(super) type RowKey<'a> = (i64, &'a str, &'a str, &'a str, &'a str, &'a str, &'a str);

/// Starts counting each table's rows, at the first open from the rows it holds, in one write so
/// that none is missed. A store already counting takes no write lock, because every open
/// comes here. Input: the store. Output: done, or `usage_unavailable`.
pub(super) fn count_rows(db: &Connection) -> Result<(), EngineError> {
    let missing = |table: &&str| -> bool {
        db.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='trigger' AND name=?1)",
            [format!("{table}_gone")],
            |row| row.get(0),
        )
        .unwrap_or(true)
    };
    if !COUNTED_TABLES.iter().any(missing) {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS usage_row_counts (name TEXT PRIMARY KEY, rows INTEGER NOT NULL) WITHOUT ROWID;")
        .map_err(storage)?;
    for table in COUNTED_TABLES {
        tx.execute_batch(&format!(
            "INSERT INTO usage_row_counts (name,rows) SELECT '{table}',(SELECT COUNT(*) FROM {table})
                WHERE NOT EXISTS (SELECT 1 FROM usage_row_counts WHERE name='{table}');
             CREATE TRIGGER IF NOT EXISTS {table}_added AFTER INSERT ON {table}
                BEGIN UPDATE usage_row_counts SET rows=rows+1 WHERE name='{table}'; END;
             CREATE TRIGGER IF NOT EXISTS {table}_gone AFTER DELETE ON {table}
                BEGIN UPDATE usage_row_counts SET rows=rows-1 WHERE name='{table}'; END;"
        ))
        .map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

/// Whether a counted table can take one more row. Inputs: the store and the table. Output: the
/// answer; a table that has no counter yet has room.
pub(super) fn has_room(tx: &Connection, table: &str) -> Result<bool, EngineError> {
    let rows: Option<i64> = tx
        .query_row(
            "SELECT rows FROM usage_row_counts WHERE name=?1",
            [table],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    Ok(rows.is_none_or(|rows| rows < TABLE_ROWS_MAX))
}

/// The model one session's growth is filed under in one hour: its own name while the hour
/// holds fewer than `MODELS_PER_HOUR_MAX` models or already has this one, else `''`. Inputs:
/// the open write, the table, the row key, the session and the model. Output: the model and
/// whether its row exists already.
pub(super) fn filed_model<'a>(
    tx: &Connection,
    table: &str,
    (hour_ms, kind, project, pool, account, agent, harness): RowKey<'_>,
    session: &str,
    model: &'a str,
) -> Result<(&'a str, bool), EngineError> {
    let (models, named): (i64, i64) = tx
        .query_row(
            &format!(
                "SELECT COUNT(*),COALESCE(SUM(model=?9),0) FROM {table}
                 WHERE hour_ms=?1 AND kind=?2 AND project=?3 AND pool=?4 AND account=?5
                    AND agent=?6 AND harness=?7 AND session=?8"
            ),
            params![
                hour_ms, kind, project, pool, account, agent, harness, session, model
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(storage)?;
    Ok(if named > 0 || models < MODELS_PER_HOUR_MAX {
        (model, named > 0)
    } else {
        ("", false)
    })
}

/// Step 10 of the engine database: usage rows gain the model and their estimated dollars, the
/// spellings of one harness become one row, and the message a transcript was counting moves
/// to `usage_messages`. A store already in the new shape only gets what it lacks.
/// Input: the open upgrade transaction. Output: done, or `usage_unavailable`.
#[cfg(any(unix, test))]
pub(crate) fn migrate_counts(tx: &Connection) -> Result<(), EngineError> {
    let shaped: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('usage_tokens') WHERE name='model')",
            [],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if !shaped {
        tx.execute_batch(&format!(
            "ALTER TABLE usage_tokens RENAME TO usage_tokens_old;
             DROP INDEX IF EXISTS usage_tokens_hour;
             {SCHEMA}
             INSERT INTO usage_tokens (hour_ms,kind,project,pool,account,agent,harness,session,
                input,output,cache_read,cache_write)
             SELECT hour_ms,kind,project,pool,account,agent,
                CASE harness WHEN 'claude' THEN 'claude-code' ELSE harness END,session,
                MIN(SUM(input),{max}),MIN(SUM(output),{max}),
                MIN(SUM(cache_read),{max}),MIN(SUM(cache_write),{max})
             FROM usage_tokens_old GROUP BY 1,2,3,4,5,6,7,8;
             DROP TABLE usage_tokens_old;",
            max = ROW_TOKENS_MAX
        ))
        .map_err(storage)?;
    }
    tx.execute_batch(SCHEMA).map_err(storage)?;
    tx.execute_batch(
        "INSERT INTO usage_messages (id,input,output,cache_read,cache_write,last_ms)
         SELECT 'claude:'||json_extract(progress,'$.message'),
            json_extract(progress,'$.counted.input'),json_extract(progress,'$.counted.output'),
            json_extract(progress,'$.counted.cache_read'),json_extract(progress,'$.counted.cache_write'),
            CAST(strftime('%s','now') AS INTEGER)*1000
         FROM usage_files WHERE CASE WHEN json_valid(progress) THEN
            json_type(progress,'$.message')='text'
            AND length(CAST(json_extract(progress,'$.message') AS BLOB)) BETWEEN 1 AND 128
            AND json_type(progress,'$.counted.input')='integer'
            AND json_type(progress,'$.counted.output')='integer'
            AND json_type(progress,'$.counted.cache_read')='integer'
            AND json_type(progress,'$.counted.cache_write')='integer' ELSE 0 END
         ON CONFLICT(id) DO UPDATE SET input=MAX(input,excluded.input),
            output=MAX(output,excluded.output),cache_read=MAX(cache_read,excluded.cache_read),
            cache_write=MAX(cache_write,excluded.cache_write);
         UPDATE usage_files SET progress=json_remove(progress,'$.message','$.counted')
         WHERE CASE WHEN json_valid(progress) THEN
            json_type(progress,'$.message') IS NOT NULL OR json_type(progress,'$.counted') IS NOT NULL
            ELSE 0 END;",
    )
    .map_err(storage)
}

/// One model's growth in one write: its tokens and estimated dollars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Added {
    pub(super) tokens: Tokens,
    pub(super) cost_nanos: i64,
}

/// Adds a session's new tokens and dollars to the usage hour that holds `now_ms`, after
/// recording `now_ms` as when that run was last counted. Inputs: the store, the time, the
/// attribution, the session, the model and what it added. Output: done, or
/// `usage_unavailable`.
fn add_tokens(
    tx: &Connection,
    now_ms: i64,
    attribution: &Attribution,
    session: &str,
    model: &str,
    added: Added,
) -> Result<(), EngineError> {
    if added.tokens.is_empty() && added.cost_nanos == 0 {
        return Ok(());
    }
    let hour_ms = now_ms - now_ms.rem_euclid(HOUR_MS);
    let project = text(attribution.project.as_ref().map(ProjectId::as_str));
    let pool = text(attribution.pool.as_ref().map(CredentialRef::as_str));
    let account = text(attribution.account.as_ref().map(CredentialRef::as_str));
    let harness = crate::harnesses::canonical(&attribution.harness);
    tx.execute(
        "INSERT INTO usage_seen (project,agent,session,last_ms) VALUES (?1,?2,?3,?4)
         ON CONFLICT(project,agent,session) DO UPDATE SET last_ms=MAX(last_ms,excluded.last_ms)",
        params![project, attribution.agent, session, now_ms],
    )
    .map_err(storage)?;
    let key = (
        hour_ms,
        attribution.kind.name(),
        project,
        pool,
        account,
        attribution.agent.as_str(),
        harness,
    );
    let (model, exists) = filed_model(tx, "usage_tokens", key, session, model)?;
    let (session, model) = if exists || has_room(tx, "usage_tokens")? {
        (session, model)
    } else {
        ("", "")
    };
    tx.execute(
        "INSERT INTO usage_tokens (hour_ms,kind,project,pool,account,agent,harness,session,model,\
            input,output,cache_read,cache_write,cost_nanos)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
         ON CONFLICT(hour_ms,kind,project,pool,account,agent,harness,session,model) DO UPDATE SET
            input=MIN(input+excluded.input,?15),output=MIN(output+excluded.output,?15),
            cache_read=MIN(cache_read+excluded.cache_read,?15),
            cache_write=MIN(cache_write+excluded.cache_write,?15),
            cost_nanos=MIN(cost_nanos+excluded.cost_nanos,?15)",
        params![
            hour_ms,
            attribution.kind.name(),
            project,
            pool,
            account,
            attribution.agent,
            harness,
            session,
            model,
            clamp(added.tokens.input),
            clamp(added.tokens.output),
            clamp(added.tokens.cache_read),
            clamp(added.tokens.cache_write),
            added.cost_nanos.clamp(0, ROW_TOKENS_MAX),
            ROW_TOKENS_MAX,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// Counts the usage a harness reported for one turn, such as a piped Claude Code agent that
/// keeps no records of its own, into the hour of `now_ms`. Inputs: the store, the
/// attribution, the tokens, the session name and the time. Output: done, or
/// `usage_attribution_invalid` / `usage_unavailable`.
pub fn count_reported(
    db: &Connection,
    attribution: &Attribution,
    tokens: Tokens,
    session: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    checked_attribution(attribution)?;
    if session.is_empty() || session.len() > NAME_BYTES_MAX {
        return Err(failure("usage_attribution_invalid"));
    }
    add_tokens(
        db,
        now_ms,
        attribution,
        session,
        "",
        Added {
            tokens,
            cost_nanos: 0,
        },
    )
}

/// Counts one finished turn whose home is about to be removed, such as an engine-driven
/// master turn: the usage the harness reported for the turn when it reported any, else
/// what the home's records hold; then forgets the home's records so nothing stays behind.
/// Inputs: the store, the home, its format, the attribution, the reported usage, the turn's
/// session name and the time. Output: the tokens added, or `usage_unavailable`.
pub fn count_turn(
    db: &Connection,
    home: &Path,
    format: Format,
    attribution: &Attribution,
    reported: Tokens,
    session: &str,
    now_ms: i64,
) -> Result<Tokens, EngineError> {
    let added = if reported.is_empty() {
        checked_attribution(attribution)?;
        scan_turn(db, home, format, attribution, now_ms)?
    } else {
        count_reported(db, attribution, reported, session, now_ms)?;
        reported
    };
    let prefix = format!("{}/", home.to_string_lossy());
    db.execute(
        "DELETE FROM usage_files WHERE substr(path,1,length(?1))=?1",
        params![prefix],
    )
    .map_err(storage)?;
    Ok(added)
}

/// Scans a finished turn's home until no record's read moves its progress, at most
/// `TURN_SCANS_MAX` times, since one scan stops at its read and scan bounds (COUNT-3-L1). A
/// home still moving at the last scan is marked cut, so the usage page reads as partial.
/// Inputs: the store, the home, its format, the attribution and the time. Output: the tokens
/// added, or `usage_unavailable`.
fn scan_turn(
    db: &Connection,
    home: &Path,
    format: Format,
    attribution: &Attribution,
    now_ms: i64,
) -> Result<Tokens, EngineError> {
    let mut added = Tokens::default();
    for _ in 0..TURN_SCANS_MAX {
        let (gained, moved) = scan(db, home, format, attribution, None, now_ms)?;
        added = added.plus(gained);
        if !moved {
            return Ok(added);
        }
    }
    mark_cut(db, home, attribution, &[], true, now_ms)?;
    Ok(added)
}

/// Registers a run whose home the usage upkeep counts for a day after it starts, such as a
/// research job's agent. The pool is resolved when its records are counted.
/// Inputs: the store, the run's home, its attribution and the start time.
/// Output: done, `usage_attribution_invalid`, or `usage_unavailable`.
pub fn register_run(
    db: &Connection,
    home: &Path,
    attribution: &Attribution,
    now_ms: i64,
) -> Result<(), EngineError> {
    if !home.is_absolute() || Format::of(&attribution.harness).is_none() {
        return Err(failure("usage_attribution_invalid"));
    }
    checked_attribution(attribution)?;
    db.execute(
        "INSERT INTO usage_runs (home,kind,project,account,agent,harness,started_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(home) DO UPDATE SET kind=excluded.kind,project=excluded.project,
            account=excluded.account,agent=excluded.agent,harness=excluded.harness,
            started_ms=excluded.started_ms",
        params![
            home.to_string_lossy(),
            attribution.kind.name(),
            text(attribution.project.as_ref().map(ProjectId::as_str)),
            text(attribution.account.as_ref().map(CredentialRef::as_str)),
            attribution.agent,
            attribution.harness,
            now_ms,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// The format and attribution of a registered run. Inputs: its stored fields and the pool
/// resolver. Output: both, None for a harness whose records the engine cannot read.
pub(super) fn run_attribution(
    kind: &str,
    project: String,
    account: String,
    agent: String,
    harness: String,
    pool: &impl Fn(&str, &CredentialRef) -> Option<CredentialRef>,
) -> Option<(Format, Attribution)> {
    let kind = match kind {
        "master" => RunKind::Master,
        "agent" => RunKind::Agent,
        _ => RunKind::Research,
    };
    let format = Format::of(&harness)?;
    let project = ProjectId::new(project).ok();
    let account = CredentialRef::new(account).ok();
    Some((
        format,
        Attribution {
            kind,
            pool: match (&project, &account) {
                (Some(project), Some(account)) => pool(project.as_str(), account),
                _ => None,
            },
            project,
            account,
            agent,
            harness,
        },
    ))
}

/// Counts what the records of every run registered in the last day gained, then forgets
/// runs past that day whose home is gone, with their record progress.
/// Inputs: the store, the pool lookup by project and account, and the time.
/// Output: the tokens added, or `usage_unavailable`.
pub fn scan_runs(
    db: &Connection,
    pool: impl Fn(&str, &CredentialRef) -> Option<CredentialRef>,
    now_ms: i64,
) -> Result<Tokens, EngineError> {
    let rows: Vec<(String, String, String, String, String, String)> = {
        let mut statement = db
            .prepare(
                "SELECT home,kind,project,account,agent,harness FROM usage_runs
                 WHERE started_ms>=?1 ORDER BY started_ms DESC LIMIT ?2",
            )
            .map_err(storage)?;
        statement
            .query_map(
                params![now_ms - RUN_SCAN_MS, FILES_PER_SCAN_MAX as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?
    };
    let mut added = Tokens::default();
    for (home, kind, project, account, agent, harness) in rows {
        let Some((format, attribution)) =
            run_attribution(&kind, project, account, agent, harness, &pool)
        else {
            continue;
        };
        added = added.plus(scan_home(
            db,
            Path::new(&home),
            format,
            &attribution,
            now_ms,
        )?);
    }
    forget_finished_runs(db, now_ms)?;
    forget_old_messages(db, now_ms)?;
    Ok(added)
}

/// Forgets up to `FINISHED_RUNS_PER_SCAN` runs, picked at random among those past their day,
/// whose home is gone: first the progress of their records, then the run. A home that is still
/// there keeps both, so a run registered on it again never counts its records twice. Counted
/// tokens stay. Inputs: the store and the time. Output: done, or `usage_unavailable`.
fn forget_finished_runs(db: &Connection, now_ms: i64) -> Result<(), EngineError> {
    let ended = now_ms - RUN_SCAN_MS;
    let homes: Vec<String> = {
        let mut statement = db
            .prepare("SELECT home FROM usage_runs WHERE started_ms<?1 ORDER BY random() LIMIT ?2")
            .map_err(storage)?;
        statement
            .query_map(params![ended, FINISHED_RUNS_PER_SCAN], |row| row.get(0))
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?
    };
    assert!(homes.len() <= FINISHED_RUNS_PER_SCAN as usize);
    for home in homes {
        let gone = std::fs::symlink_metadata(&home)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
        if !gone {
            continue;
        }
        db.execute(
            "DELETE FROM usage_files WHERE substr(path,1,length(?1))=?1",
            params![format!("{home}/")],
        )
        .map_err(storage)?;
        db.execute("DELETE FROM usage_cut WHERE home=?1", [&home])
            .map_err(storage)?;
        db.execute(
            "DELETE FROM usage_runs WHERE home=?1 AND started_ms<?2",
            params![home, ended],
        )
        .map_err(storage)?;
    }
    Ok(())
}

fn checked_attribution(attribution: &Attribution) -> Result<(), EngineError> {
    if attribution.agent.is_empty()
        || attribution.agent.len() > AGENT_BYTES_MAX
        || attribution.harness.is_empty()
        || attribution.harness.len() > NAME_BYTES_MAX
    {
        return Err(failure("usage_attribution_invalid"));
    }
    Ok(())
}

pub(super) fn clamp(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX).min(ROW_TOKENS_MAX)
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(test)]
mod migration_tests;

#[cfg(test)]
mod counting_tests;
