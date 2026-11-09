use super::{LimitSource, LimitWindow, Observation, failure, record_observation, storage};
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, ProjectId};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const FILES_PER_SCAN_MAX: usize = 256;
pub const READ_BYTES_MAX: usize = 4 * 1024 * 1024;
const HOUR_MS: i64 = 60 * 60 * 1000;
const FIVE_HOUR_MINUTES: u64 = 300;
const WEEK_MINUTES: u64 = 7 * 24 * 60;
const NAME_BYTES_MAX: usize = 128;
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
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    counted: Tokens,
    total: Tokens,
}

/// The token growth one Claude Code transcript line shows. Each assistant line carries its
/// message's usage, and several lines of one message repeat it, so each message counts
/// once at its largest values, each count capped at `LINE_TOKENS_MAX`.
/// Inputs: the parsed line and the file's progress. Output: the growth to add.
fn claude_line(value: &Value, progress: &mut Progress) -> Tokens {
    let message = &value["message"];
    if value["type"] != "assistant" || !message["usage"].is_object() {
        return Tokens::default();
    }
    let usage = &message["usage"];
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let seen = Tokens {
        input: count("input_tokens"),
        output: count("output_tokens"),
        cache_read: count("cache_read_input_tokens"),
        cache_write: count("cache_creation_input_tokens"),
    }
    .at_most(LINE_TOKENS_MAX);
    let id = message["id"].as_str().map(str::to_owned);
    if id.is_some() && id == progress.message {
        let growth = seen.growth_over(progress.counted);
        progress.counted = Tokens {
            input: progress.counted.input.max(seen.input),
            output: progress.counted.output.max(seen.output),
            cache_read: progress.counted.cache_read.max(seen.cache_read),
            cache_write: progress.counted.cache_write.max(seen.cache_write),
        };
        return growth;
    }
    progress.message = id;
    progress.counted = seen;
    seen
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

/// What one Codex rollout line shows: the growth of the session's cumulative token totals
/// past the last ones counted, each capped at `LINE_TOKENS_MAX`, and the account's limit
/// windows when the line carries them. Codex counts cached input inside input; the growth
/// keeps them apart. Inputs: the parsed line, the file's progress and the time.
/// Output: growth and limits.
fn codex_line(
    value: &Value,
    progress: &mut Progress,
    now_ms: i64,
) -> (Tokens, Option<Observation>) {
    let payload = &value["payload"];
    if value["type"] != "event_msg" || payload["type"] != "token_count" {
        return (Tokens::default(), None);
    }
    let mut growth = Tokens::default();
    let total = &payload["info"]["total_token_usage"];
    if total.is_object() {
        let count = |key: &str| total[key].as_u64().unwrap_or(0);
        let cached = count("cached_input_tokens");
        let seen = Tokens {
            input: count("input_tokens").saturating_sub(cached),
            output: count("output_tokens"),
            cache_read: cached,
            cache_write: 0,
        };
        growth = seen.growth_over(progress.total).at_most(LINE_TOKENS_MAX);
        progress.total = seen;
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
    (growth, reported.then_some(observation))
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
    path: PathBuf,
    identity: (u64, u64),
    length: u64,
}

/// The identity a record's progress is kept under: its device and inode on unix, none
/// elsewhere, as [`open_record`] reads it. Input: the metadata. Output: the identity.
#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

/// The identity a record's progress is kept under: none on this platform, as
/// [`open_record`] reads it. Input: the metadata. Output: the identity.
#[cfg(not(unix))]
fn file_identity(_: &std::fs::Metadata) -> (u64, u64) {
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

fn session_name(path: &Path) -> String {
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
/// Inputs: the record, its progress and the time. Output: the growth and the last limits.
fn read_record(
    path: &Path,
    format: Format,
    progress: &mut Progress,
    now_ms: i64,
) -> (Tokens, Option<Observation>) {
    let Some(opened) = open_record(path) else {
        return (Tokens::default(), None);
    };
    read_opened(opened, format, progress, now_ms)
}

/// Reads one record as `read_record` does and also collects the tool calls its new lines
/// record. Inputs: the record, its progress, the time and the collected tools.
/// Output: the growth and the last limits.
fn read_record_with(
    path: &Path,
    format: Format,
    progress: &mut Progress,
    now_ms: i64,
    tools: &mut Vec<String>,
) -> (Tokens, Option<Observation>) {
    let Some(opened) = open_record(path) else {
        return (Tokens::default(), None);
    };
    read_opened_with(opened, format, progress, now_ms, Some(tools))
}

fn read_opened(
    opened: (File, (u64, u64), u64),
    format: Format,
    progress: &mut Progress,
    now_ms: i64,
) -> (Tokens, Option<Observation>) {
    read_opened_with(opened, format, progress, now_ms, None)
}

/// Reads what an open record gained since its stored progress, up to the length it had
/// when it was opened and at most `READ_BYTES_MAX`, so lines a harness appends meanwhile
/// wait for the next scan. The record is the one the progress pinned while its inode is
/// the same, under the crate-wide volume renumber rule, and the pin then moves to the live
/// device, so a reboot never counts a whole record again. Inputs: the open record with its
/// identity and length, its progress, the time and where to collect the tool calls read,
/// at most `TOOLS_PER_READ_MAX`, if anywhere. Output: the growth and the last limits.
fn read_opened_with(
    (mut file, identity, length): (File, (u64, u64), u64),
    format: Format,
    progress: &mut Progress,
    now_ms: i64,
    mut tools: Option<&mut Vec<String>>,
) -> (Tokens, Option<Observation>) {
    if crate::file_identity::same_files(&[progress.identity], &[identity])
        && length >= progress.offset
    {
        progress.identity = identity;
    } else {
        *progress = Progress {
            identity,
            ..Progress::default()
        };
    }
    if length == progress.offset || file.seek(SeekFrom::Start(progress.offset)).is_err() {
        return (Tokens::default(), None);
    }
    let mut bytes = Vec::new();
    if file
        .take((length - progress.offset).min(READ_BYTES_MAX as u64))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return (Tokens::default(), None);
    }
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        if bytes.len() == READ_BYTES_MAX {
            progress.offset += bytes.len() as u64;
            progress.skipping = true;
        }
        return (Tokens::default(), None);
    };
    progress.offset += end as u64 + 1;
    let mut growth = Tokens::default();
    let mut limits = None;
    for line in bytes[..end].split(|byte| *byte == b'\n') {
        if std::mem::take(&mut progress.skipping) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match format {
            Format::Claude => growth = growth.plus(claude_line(&value, progress)),
            Format::Codex => {
                let (gained, observed) = codex_line(&value, progress, now_ms);
                growth = growth.plus(gained);
                limits = observed.or(limits);
            }
        }
        if let Some(tools) = tools.as_deref_mut() {
            let called = match format {
                Format::Claude => claude_tools(&value),
                Format::Codex => codex_tools(&value),
            };
            for tool in called {
                if tools.len() < TOOLS_PER_READ_MAX {
                    tools.push(tool);
                }
            }
        }
    }
    assert!(progress.offset <= length);
    assert!(tools.is_none_or(|tools| tools.len() <= TOOLS_PER_READ_MAX));
    (growth, limits)
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
    scan(db, home, format, attribution, None, now_ms)
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
    scan(db, home, format, attribution, Some(agent), now_ms)
}

fn scan(
    db: &Connection,
    home: &Path,
    format: Format,
    attribution: &Attribution,
    agent: Option<i64>,
    now_ms: i64,
) -> Result<Tokens, EngineError> {
    checked_attribution(attribution)?;
    let counting = match agent {
        Some(id) if format == Format::Codex || launched_without_hooks(db, id)? => Some(id),
        _ => None,
    };
    let (paths, left, stopped) = records(home, format);
    mark_cut(db, home, attribution, &left, stopped, now_ms)?;
    let mut changed = Vec::new();
    for path in paths {
        let key = path.to_string_lossy().into_owned();
        let before = stored_progress(db, &key)?;
        let mut progress = before.clone();
        let mut tools = Vec::new();
        let (growth, observed) = match counting {
            Some(_) => read_record_with(&path, format, &mut progress, now_ms, &mut tools),
            None => read_record(&path, format, &mut progress, now_ms),
        };
        if progress != before {
            changed.push((
                key,
                session_name(&path),
                before,
                progress,
                growth,
                observed,
                tools,
            ));
        }
    }
    if changed.is_empty() {
        return Ok(Tokens::default());
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    let mut added = Tokens::default();
    let mut limits = None;
    for (key, session, before, progress, growth, observed, tools) in changed {
        if stored_progress(&tx, &key)? != before {
            continue;
        }
        if let Some(agent) = counting {
            for tool in &tools {
                crate::coord::record_tool(&tx, agent, tool, now_ms)?;
            }
        }
        tx.execute(
            "INSERT INTO usage_files (path,progress) VALUES (?1,?2)
             ON CONFLICT(path) DO UPDATE SET progress=excluded.progress",
            params![key, serde_json::to_string(&progress).map_err(storage)?],
        )
        .map_err(storage)?;
        add_tokens(
            &tx,
            now_ms,
            attribution,
            &session,
            "",
            Added {
                tokens: growth,
                cost_nanos: 0,
            },
        )?;
        added = added.plus(growth);
        limits = limits.or(observed);
    }
    if let (Some(observation), Some(account)) = (limits, &attribution.account) {
        record_observation(&tx, account, &observation, LimitSource::Transcript, now_ms)?;
    }
    tx.commit().map_err(storage)?;
    Ok(added)
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

/// Step 9 of the engine database: usage rows gain the model and their estimated dollars, the
/// spellings of one harness become one row, and the message a transcript was counting moves
/// to `usage_messages`. A store already in the new shape only gets what it lacks.
/// Input: the open upgrade transaction. Output: done, or `usage_unavailable`.
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
         FROM usage_files WHERE json_type(progress,'$.message')='text'
         ON CONFLICT(id) DO UPDATE SET input=MAX(input,excluded.input),
            output=MAX(output,excluded.output),cache_read=MAX(cache_read,excluded.cache_read),
            cache_write=MAX(cache_write,excluded.cache_write);
         UPDATE usage_files SET progress=json_remove(progress,'$.message','$.counted')
         WHERE json_type(progress,'$.counted') IS NOT NULL;",
    )
    .map_err(storage)
}

/// One model's growth in one write: its tokens and estimated dollars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Added {
    pub(super) tokens: Tokens,
    pub(super) cost_nanos: i64,
}

/// Adds a session's new tokens to the usage hour that holds `now_ms`, after recording
/// `now_ms` as when that run was last counted. Inputs: the store, the time, the
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
    if added.tokens.is_empty() {
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
            added.cost_nanos.max(0),
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
        scan_home(db, home, format, attribution, now_ms)?
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
        let kind = match kind.as_str() {
            "master" => RunKind::Master,
            "agent" => RunKind::Agent,
            _ => RunKind::Research,
        };
        let Some(format) = Format::of(&harness) else {
            continue;
        };
        let project = ProjectId::new(project).ok();
        let account = CredentialRef::new(account).ok();
        let attribution = Attribution {
            kind,
            pool: match (&project, &account) {
                (Some(project), Some(account)) => pool(project.as_str(), account),
                _ => None,
            },
            project,
            account,
            agent,
            harness,
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

fn clamp(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX).min(ROW_TOKENS_MAX)
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(test)]
mod migration_tests;
