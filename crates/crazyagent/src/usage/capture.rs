use super::{LimitSource, LimitWindow, Observation, failure, record_observation, storage};
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, ProjectId};
use rusqlite::{Connection, OptionalExtension, params};
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
const WALK_DEPTH_MAX: usize = 3;
const RUN_SCAN_MS: i64 = 24 * HOUR_MS;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_tokens (
    hour_ms INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('master','agent','research')),
    project TEXT NOT NULL,
    pool TEXT NOT NULL,
    account TEXT NOT NULL,
    agent TEXT NOT NULL,
    harness TEXT NOT NULL,
    session TEXT NOT NULL,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    PRIMARY KEY (hour_ms, kind, project, pool, account, agent, harness, session)
);
CREATE INDEX IF NOT EXISTS usage_tokens_hour ON usage_tokens(hour_ms);
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

    /// Whether every count is zero. Output: true for an empty count.
    pub fn is_empty(self) -> bool {
        self == Tokens::default()
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
    pub project: ProjectId,
    pub pool: Option<CredentialRef>,
    pub account: CredentialRef,
    pub agent: String,
    pub harness: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    identity: (u64, u64),
    offset: u64,
    skipping: bool,
    message: Option<String>,
    counted: Tokens,
    total: Tokens,
}

/// The token growth one Claude Code transcript line shows. Each assistant line carries its
/// message's usage, and several lines of one message repeat it, so each message counts
/// once at its largest values. Inputs: the parsed line and the file's progress.
/// Output: the growth to add.
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
    };
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

fn codex_window(value: &Value, now_ms: i64) -> Option<(u64, LimitWindow)> {
    let used = value["used_percent"]
        .as_f64()
        .filter(|used| used.is_finite())?;
    let minutes = value["window_minutes"].as_u64()?;
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
    .filter(|at| (1..=super::RESET_MS_MAX).contains(at));
    Some((
        minutes,
        LimitWindow {
            used_percent: used.clamp(0.0, 100.0),
            resets_at_ms,
        },
    ))
}

/// What one Codex rollout line shows: the growth of the session's cumulative token totals
/// past the last ones counted, and the account's limit windows when the line carries them.
/// Codex counts cached input inside input; the growth keeps them apart.
/// Inputs: the parsed line, the file's progress and the time. Output: growth and limits.
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
        growth = seen.growth_over(progress.total);
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

/// Lists a home's session records, at most `FILES_PER_SCAN_MAX`, most recently changed
/// first: Claude Code's `projects/<folder>/<session>.jsonl` with its sub-agent records
/// further down, or Codex's `sessions/<year>/<month>/<day>/rollout-*.jsonl`.
/// Inputs: the home and the format. Output: the paths.
fn records(home: &Path, format: Format) -> Vec<PathBuf> {
    let root = match format {
        Format::Claude => home.join("projects"),
        Format::Codex => home.join("sessions"),
    };
    let mut found = Vec::new();
    let mut folders = vec![(root, 0)];
    let mut visited = 0;
    while let Some((folder, level)) = folders.pop() {
        visited += 1;
        if visited > FILES_PER_SCAN_MAX * 4 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten().take(FILES_PER_SCAN_MAX * 4) {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if kind.is_dir() && level < WALK_DEPTH_MAX {
                folders.push((entry.path(), level + 1));
            } else if kind.is_file()
                && name.ends_with(".jsonl")
                && (format == Format::Claude || name.starts_with("rollout-"))
            {
                let changed = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                found.push((std::cmp::Reverse(changed), entry.path()));
            }
        }
    }
    found.sort();
    found.truncate(FILES_PER_SCAN_MAX);
    found.into_iter().map(|(_, path)| path).collect()
}

#[cfg(unix)]
fn open_record(path: &Path) -> Option<(File, (u64, u64), u64)> {
    use std::os::unix::fs::MetadataExt;
    let file = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
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
fn open_record(path: &Path) -> Option<(File, (u64, u64), u64)> {
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    metadata.is_file().then(|| (file, (0, 0), metadata.len()))
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
/// A replaced or shortened record starts over. A line longer than the read is skipped.
/// Inputs: the record, its progress and the time. Output: the growth and the last limits.
fn read_record(
    path: &Path,
    format: Format,
    progress: &mut Progress,
    now_ms: i64,
) -> (Tokens, Option<Observation>) {
    let Some((mut file, identity, length)) = open_record(path) else {
        return (Tokens::default(), None);
    };
    if progress.identity != identity || length < progress.offset {
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
        .take(READ_BYTES_MAX as u64)
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
    }
    assert!(progress.offset <= length);
    (growth, limits)
}

/// Counts what every session record in one agent home gained since the last scan and adds
/// it to the run's attribution in the hour of `now_ms`; Codex limit windows the records
/// carry update the account's limits. Progress and counts commit together, so no line
/// counts twice. Inputs: the store, the home, its format, the attribution and the time.
/// Output: the tokens added, or `usage_unavailable`.
pub fn scan_home(
    db: &Connection,
    home: &Path,
    format: Format,
    attribution: &Attribution,
    now_ms: i64,
) -> Result<Tokens, EngineError> {
    checked_attribution(attribution)?;
    let hour_ms = now_ms - now_ms.rem_euclid(HOUR_MS);
    let tx = db.unchecked_transaction().map_err(storage)?;
    let mut added = Tokens::default();
    let mut limits = None;
    for path in records(home, format) {
        let key = path.to_string_lossy().into_owned();
        let stored: Option<String> = tx
            .query_row(
                "SELECT progress FROM usage_files WHERE path=?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        let mut progress: Progress = match stored {
            Some(text) => serde_json::from_str(&text).map_err(storage)?,
            None => Progress::default(),
        };
        let before = progress.clone();
        let (growth, observed) = read_record(&path, format, &mut progress, now_ms);
        limits = observed.or(limits);
        if progress != before {
            tx.execute(
                "INSERT INTO usage_files (path,progress) VALUES (?1,?2)
                 ON CONFLICT(path) DO UPDATE SET progress=excluded.progress",
                params![key, serde_json::to_string(&progress).map_err(storage)?],
            )
            .map_err(storage)?;
        }
        if growth.is_empty() {
            continue;
        }
        tx.execute(
            "INSERT INTO usage_tokens (hour_ms,kind,project,pool,account,agent,harness,session,\
                input,output,cache_read,cache_write)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(hour_ms,kind,project,pool,account,agent,harness,session) DO UPDATE SET
                input=input+excluded.input,output=output+excluded.output,
                cache_read=cache_read+excluded.cache_read,cache_write=cache_write+excluded.cache_write",
            params![
                hour_ms,
                attribution.kind.name(),
                attribution.project.as_str(),
                attribution.pool.as_ref().map_or("", |pool| pool.as_str()),
                attribution.account.as_str(),
                attribution.agent,
                attribution.harness,
                session_name(&path),
                clamp(growth.input),
                clamp(growth.output),
                clamp(growth.cache_read),
                clamp(growth.cache_write),
            ],
        )
        .map_err(storage)?;
        added = added.plus(growth);
    }
    if let Some(observation) = limits {
        record_observation(
            &tx,
            &attribution.account,
            &observation,
            LimitSource::Transcript,
            now_ms,
        )?;
    }
    tx.commit().map_err(storage)?;
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
            attribution.project.as_str(),
            attribution.account.as_str(),
            attribution.agent,
            attribution.harness,
            now_ms,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// Counts what the records of every run registered in the last day gained.
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
        let (Some(format), Ok(project), Ok(account)) = (
            Format::of(&harness),
            ProjectId::new(project),
            CredentialRef::new(account),
        ) else {
            continue;
        };
        let attribution = Attribution {
            kind,
            pool: pool(project.as_str(), &account),
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
    Ok(added)
}

fn checked_attribution(attribution: &Attribution) -> Result<(), EngineError> {
    if attribution.agent.is_empty()
        || attribution.agent.len() > NAME_BYTES_MAX
        || attribution.harness.is_empty()
        || attribution.harness.len() > NAME_BYTES_MAX
    {
        return Err(failure("usage_attribution_invalid"));
    }
    Ok(())
}

fn clamp(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

#[cfg(all(test, unix))]
mod tests;
