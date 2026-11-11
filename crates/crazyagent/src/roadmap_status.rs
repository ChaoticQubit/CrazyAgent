use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[cfg(all(test, unix))]
mod tests;

/// The most roadmap items one status holds.
pub const ITEMS_MAX: usize = 200;
/// The most bytes of one roadmap item's text.
pub const ITEM_BYTES_MAX: usize = 300;
/// The most bytes of the evidence line kept for one item.
pub const EVIDENCE_BYTES_MAX: usize = 300;
/// The most bytes of the heading an item sits under.
pub const SECTION_BYTES_MAX: usize = 120;
/// The most bytes of the roadmap file's path or of the words saying why a check failed.
pub const NOTE_BYTES_MAX: usize = 512;
/// The most bytes of a status agent's reply the engine reads.
pub const REPLY_BYTES_MAX: usize = 256 * 1024;
/// How long after the last landed change set a stale status waits before it is checked, so
/// a burst of landings costs one run.
pub const QUIET_MS: i64 = 10 * 60 * 1000;
/// The longest one status run may take.
pub const RUN_TIME: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// The most status runs one tick starts.
pub const STARTS_PER_TICK_MAX: usize = 4;
/// What a run that fails says when its reply holds no status.
pub const NO_STATUS: &str = "The agent's reply held no roadmap status";
/// What a run that never reported back says once its time is over.
pub const UNFINISHED: &str = "The last check did not finish";

const RUN_MS: i64 = RUN_TIME.as_millis() as i64;
const BODY_BYTES_MAX: usize =
    ITEMS_MAX * (2 * (SECTION_BYTES_MAX + ITEM_BYTES_MAX + EVIDENCE_BYTES_MAX) + 96);
const ENTRIES_READ_MAX: usize = ITEMS_MAX * 2;
const SKIPPED_SECTIONS: [&str; 3] = ["open questions", "release process", "owner answers"];
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS roadmap_status_settings (
    project TEXT PRIMARY KEY,
    refresh TEXT NOT NULL CHECK(refresh IN ('after_landing','refresh_only','off'))
);
CREATE TABLE IF NOT EXISTS roadmap_status (
    project TEXT PRIMARY KEY,
    body TEXT NOT NULL DEFAULT '[]',
    source TEXT,
    problem TEXT,
    checked_ms INTEGER,
    stale INTEGER NOT NULL DEFAULT 0 CHECK(stale IN (0,1)),
    due_ms INTEGER NOT NULL DEFAULT 0,
    running_ms INTEGER
);";

const _: () = assert!(RUN_MS > 0 && QUIET_MS > 0);
const _: () = assert!(BODY_BYTES_MAX <= 1024 * 1024);
const _: () = assert!(STARTS_PER_TICK_MAX >= 1 && ENTRIES_READ_MAX >= ITEMS_MAX);

/// Where one roadmap item stands: not started, being worked on, landed, or landed with a
/// test that covers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Pending,
    InProgress,
    Done,
    Tested,
}

impl ItemStatus {
    /// Reads a status name an agent wrote. Input: the name. Output: the status, or none
    /// for a name this engine does not know.
    fn parse(name: &str) -> Option<Self> {
        match name {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "done" => Some(Self::Done),
            "tested" => Some(Self::Tested),
            _ => None,
        }
    }
}

/// When a project's roadmap status is checked ("Roadmap status" in Project settings): after
/// each landed change set and on Refresh, on Refresh only, or never.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refresh {
    #[default]
    AfterLanding,
    RefreshOnly,
    Off,
}

impl Refresh {
    /// The name the settings table stores. Input: the choice. Output: its name.
    fn name(self) -> &'static str {
        match self {
            Self::AfterLanding => "after_landing",
            Self::RefreshOnly => "refresh_only",
            Self::Off => "off",
        }
    }

    /// Reads a stored name back. Input: the name. Output: the choice, or none for a name
    /// this engine never writes.
    fn parse(name: &str) -> Option<Self> {
        [Self::AfterLanding, Self::RefreshOnly, Self::Off]
            .into_iter()
            .find(|choice| choice.name() == name)
    }
}

/// One roadmap item: the heading it sits under and its one line of text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoadmapItem {
    pub section: String,
    pub text: String,
}

/// One roadmap item with where it stands and the evidence line that decided it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemState {
    pub item: RoadmapItem,
    pub status: ItemStatus,
    pub evidence: String,
}

/// A project's roadmap status as its views show it: the items of the last check, when it
/// ran, whether one runs now or is due, the refresh setting, the roadmap file it read and
/// why the last check failed, if it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoadmapStatus {
    pub items: Vec<ItemState>,
    pub checked_ms: Option<i64>,
    pub running: bool,
    pub stale: bool,
    pub refresh: Refresh,
    pub source: Option<String>,
    pub problem: Option<String>,
}

/// What `tick` starts a status run with. The real one runs the agent on its own thread and
/// reports through `finish`; a test counts the starts.
pub trait Runs {
    /// Starts one status run for a project `tick` just claimed. Input: the project.
    fn start(&mut self, project: &ProjectId);
}

fn unavailable<E>(_: E) -> EngineError {
    failure("roadmap_status_unavailable")
}

/// Creates the status and settings tables. Input: the engine connection. Output: unit, or
/// `roadmap_status_unavailable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)
}

/// One line of text an agent or a document wrote, made safe to store and show: hidden
/// characters shown escaped, control characters turned into spaces, trimmed and cut at a
/// character boundary. Inputs: the text and the most bytes. Output: the line.
fn line(text: &str, bytes_max: usize) -> String {
    let shown: String = crate::memory::visible_text(text)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let shown = shown.trim();
    let mut end = shown.len().min(bytes_max);
    while !shown.is_char_boundary(end) {
        end -= 1;
    }
    let cut = shown[..end].trim_end().to_owned();
    assert!(cut.len() <= bytes_max);
    assert!(!cut.chars().any(char::is_control));
    cut
}

/// The text of a list line at column 0 or 2: after `- `, `* ` or a number with `. `, with
/// a leading task box (`[ ]`, `[x]`) dropped. Input: the line as written. Output: the item
/// text, or none for any other line.
fn bullet(written: &str) -> Option<&str> {
    let indent = written.len() - written.trim_start_matches(' ').len();
    if indent != 0 && indent != 2 {
        return None;
    }
    let rest = &written[indent..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let text = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| {
            (digits > 0)
                .then(|| rest[digits..].strip_prefix(". "))
                .flatten()
        })?;
    let text = text.trim_start();
    let boxed = ["[ ] ", "[x] ", "[X] "]
        .into_iter()
        .find_map(|mark| text.strip_prefix(mark));
    Some(boxed.unwrap_or(text))
}

/// The roadmap items of a Markdown roadmap: every list line at column 0 or 2, under the
/// level 1 to 3 heading above it. The sections "Open questions", "Release process" and
/// "Owner answers" hold no items. Input: the roadmap text. Output: at most `ITEMS_MAX`
/// items, each within its byte bounds.
pub fn items(markdown: &str) -> Vec<RoadmapItem> {
    let mut found = Vec::new();
    let mut section = String::new();
    let mut skipped = false;
    for written in markdown.lines() {
        if found.len() == ITEMS_MAX {
            break;
        }
        let level = written.bytes().take_while(|byte| *byte == b'#').count();
        if (1..=3).contains(&level) && written[level..].starts_with(' ') {
            section = line(&written[level..], SECTION_BYTES_MAX);
            skipped = SKIPPED_SECTIONS
                .iter()
                .any(|name| section.eq_ignore_ascii_case(name));
            continue;
        }
        if skipped {
            continue;
        }
        let Some(text) = bullet(written).map(|text| line(text, ITEM_BYTES_MAX)) else {
            continue;
        };
        if !text.is_empty() {
            found.push(RoadmapItem {
                section: section.clone(),
                text,
            });
        }
    }
    assert!(found.len() <= ITEMS_MAX);
    assert!(found.iter().all(|item| item.text.len() <= ITEM_BYTES_MAX));
    found
}

/// Reads a status agent's reply: the JSON object from its first `{` to its last `}`, whose
/// `items` name roadmap items by number from 1 with a status and an evidence line. Every
/// roadmap item gets a state, pending when the reply leaves it out; an unknown number or
/// status is dropped. Inputs: the reply and the items it was asked about. Output: one state
/// per item in order, or the failure words.
pub fn parse_reply(reply: &str, items: &[RoadmapItem]) -> Result<Vec<ItemState>, &'static str> {
    assert!(items.len() <= ITEMS_MAX);
    if reply.len() > REPLY_BYTES_MAX {
        return Err(NO_STATUS);
    }
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err(NO_STATUS);
    };
    if end < start {
        return Err(NO_STATUS);
    }
    let value: serde_json::Value =
        serde_json::from_str(&reply[start..=end]).map_err(|_| NO_STATUS)?;
    let listed = value
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or(NO_STATUS)?;
    let mut states: Vec<ItemState> = items
        .iter()
        .map(|item| ItemState {
            item: item.clone(),
            status: ItemStatus::Pending,
            evidence: String::new(),
        })
        .collect();
    for entry in listed.iter().take(ENTRIES_READ_MAX) {
        let number = entry.get("item").and_then(serde_json::Value::as_u64);
        let status = entry
            .get("status")
            .and_then(serde_json::Value::as_str)
            .and_then(ItemStatus::parse);
        let at = number
            .and_then(|number| usize::try_from(number).ok())
            .and_then(|number| number.checked_sub(1));
        let (Some(at), Some(status)) = (at, status) else {
            continue;
        };
        let Some(state) = states.get_mut(at) else {
            continue;
        };
        let evidence = entry.get("evidence").and_then(serde_json::Value::as_str);
        state.status = status;
        state.evidence = line(evidence.unwrap_or(""), EVIDENCE_BYTES_MAX);
    }
    assert_eq!(states.len(), items.len());
    assert!(
        states
            .iter()
            .all(|state| state.evidence.len() <= EVIDENCE_BYTES_MAX)
    );
    Ok(states)
}

/// A project's refresh setting, the default before the first save. Inputs: the connection
/// and the project. Output: the setting, or `roadmap_status_unavailable` for a stored name
/// this engine never writes.
fn refresh_of(db: &Connection, project: &ProjectId) -> Result<Refresh, EngineError> {
    let stored: Option<String> = db
        .query_row(
            "SELECT refresh FROM roadmap_status_settings WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    match stored {
        None => Ok(Refresh::default()),
        Some(name) => Refresh::parse(&name).ok_or_else(|| unavailable(())),
    }
}

/// Saves a project's refresh setting. Turning it off also drops a check that was due, so
/// nothing runs after the owner said never. Inputs: the connection, the project and the
/// choice. Output: unit, or `roadmap_status_unavailable`.
pub fn set_refresh(
    db: &Connection,
    project: &ProjectId,
    refresh: Refresh,
) -> Result<(), EngineError> {
    let stored = db
        .execute(
            "INSERT INTO roadmap_status_settings(project,refresh) VALUES(?1,?2)
             ON CONFLICT(project) DO UPDATE SET refresh=excluded.refresh",
            params![project.as_str(), refresh.name()],
        )
        .map_err(unavailable)?;
    assert_eq!(stored, 1);
    if refresh == Refresh::Off {
        db.execute(
            "UPDATE roadmap_status SET stale=0 WHERE project=?1",
            [project.as_str()],
        )
        .map_err(unavailable)?;
    }
    assert_eq!(refresh_of(db, project)?, refresh);
    Ok(())
}

/// Marks a project's status stale after a change set landed, when its setting follows
/// landings; the check waits `QUIET_MS` after the last landing. Inputs: the landing's
/// transaction and the project. Output: unit, or `roadmap_status_unavailable`.
pub fn mark_stale(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    mark_stale_at(tx, project, crate::team::now_ms())
}

/// `mark_stale` at a given time. A Refresh that still waits keeps running at once. Inputs:
/// the connection, the project and now. Output: unit, or `roadmap_status_unavailable`.
fn mark_stale_at(db: &Connection, project: &ProjectId, now_ms: i64) -> Result<(), EngineError> {
    assert!(now_ms >= 0);
    if refresh_of(db, project)? != Refresh::AfterLanding {
        return Ok(());
    }
    let due_ms = now_ms.saturating_add(QUIET_MS);
    let stored = db
        .execute(
            "INSERT INTO roadmap_status(project,stale,due_ms) VALUES(?1,1,?2)
             ON CONFLICT(project) DO UPDATE SET
               due_ms=CASE WHEN stale=1 AND due_ms=0 THEN 0 ELSE excluded.due_ms END,
               stale=1",
            params![project.as_str(), due_ms],
        )
        .map_err(unavailable)?;
    assert_eq!(stored, 1);
    Ok(())
}

/// Asks for a check now, without the quiet period (the Refresh button); one run per project
/// still runs at a time. Inputs: the connection and the project. Output: unit,
/// `roadmap_status_off` when the setting is Off, or `roadmap_status_unavailable`.
pub fn refresh(db: &Connection, project: &ProjectId) -> Result<(), EngineError> {
    if refresh_of(db, project)? == Refresh::Off {
        return Err(failure("roadmap_status_off"));
    }
    let stored = db
        .execute(
            "INSERT INTO roadmap_status(project,stale,due_ms) VALUES(?1,1,0)
             ON CONFLICT(project) DO UPDATE SET stale=1, due_ms=0",
            [project.as_str()],
        )
        .map_err(unavailable)?;
    assert_eq!(stored, 1);
    Ok(())
}

/// Reads a project's roadmap status for its views. Inputs: the connection and the project.
/// Output: the status, empty before the first check, or `roadmap_status_unavailable`.
pub fn read(db: &Connection, project: &ProjectId) -> Result<RoadmapStatus, EngineError> {
    read_at(db, project, crate::team::now_ms())
}

/// `read` at a given time: a run counts as running until `RUN_TIME` after it started.
/// Inputs: the connection, the project and now. Output: as `read`.
fn read_at(
    db: &Connection,
    project: &ProjectId,
    now_ms: i64,
) -> Result<RoadmapStatus, EngineError> {
    type Row = (
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        bool,
        Option<i64>,
    );
    let refresh = refresh_of(db, project)?;
    let row: Option<Row> = db
        .query_row(
            "SELECT body,source,problem,checked_ms,stale,running_ms FROM roadmap_status
             WHERE project=?1 AND length(body) <= ?2",
            params![project.as_str(), BODY_BYTES_MAX as i64],
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
        .optional()
        .map_err(unavailable)?;
    let Some((body, source, problem, checked_ms, stale, running_ms)) = row else {
        return Ok(RoadmapStatus {
            items: Vec::new(),
            checked_ms: None,
            running: false,
            stale: false,
            refresh,
            source: None,
            problem: None,
        });
    };
    let items: Vec<ItemState> = serde_json::from_str(&body).map_err(unavailable)?;
    assert!(
        items.len() <= ITEMS_MAX,
        "a stored status holds at most ITEMS_MAX items"
    );
    assert!(
        items
            .iter()
            .all(|state| state.evidence.len() <= EVIDENCE_BYTES_MAX),
        "stored evidence stays within its bound"
    );
    Ok(RoadmapStatus {
        items,
        checked_ms,
        running: running_ms.is_some_and(|started| now_ms.saturating_sub(started) < RUN_MS),
        stale,
        refresh,
        source,
        problem,
    })
}

/// Stores how a status run ended and frees the project for its next run: the item states
/// and the roadmap file they came from, or the words saying why it failed, which keep the
/// last good states. Inputs: the connection, the project, the outcome and now. Output:
/// unit, or `roadmap_status_unavailable`.
pub fn finish(
    db: &Connection,
    project: &ProjectId,
    outcome: Result<(&[ItemState], &str), &str>,
    now_ms: i64,
) -> Result<(), EngineError> {
    let stored = match outcome {
        Ok((states, source)) => {
            assert!(states.len() <= ITEMS_MAX);
            assert!(
                states
                    .iter()
                    .all(|state| state.evidence.len() <= EVIDENCE_BYTES_MAX
                        && state.item.text.len() <= ITEM_BYTES_MAX
                        && state.item.section.len() <= SECTION_BYTES_MAX)
            );
            let body = serde_json::to_string(states).map_err(unavailable)?;
            assert!(body.len() <= BODY_BYTES_MAX);
            db.execute(
                "INSERT INTO roadmap_status(project,body,source,problem,checked_ms,running_ms)
                 VALUES(?1,?2,?3,NULL,?4,NULL)
                 ON CONFLICT(project) DO UPDATE SET body=excluded.body, source=excluded.source,
                   problem=NULL, checked_ms=excluded.checked_ms, running_ms=NULL",
                params![project.as_str(), body, line(source, NOTE_BYTES_MAX), now_ms],
            )
        }
        Err(words) => db.execute(
            "INSERT INTO roadmap_status(project,problem,running_ms) VALUES(?1,?2,NULL)
             ON CONFLICT(project) DO UPDATE SET problem=excluded.problem, running_ms=NULL",
            params![project.as_str(), line(words, NOTE_BYTES_MAX)],
        ),
    }
    .map_err(unavailable)?;
    assert_eq!(stored, 1);
    Ok(())
}

/// The projects whose check is due: stale, past their quiet period, not switched off and
/// with no run in flight. Inputs: the connection and now. Output: at most
/// `STARTS_PER_TICK_MAX` projects, the longest waiting first.
fn due(db: &Connection, now_ms: i64) -> Result<Vec<ProjectId>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT project FROM roadmap_status WHERE stale=1 AND due_ms <= ?1
               AND running_ms IS NULL
               AND NOT EXISTS (SELECT 1 FROM roadmap_status_settings AS settings
                 WHERE settings.project=roadmap_status.project AND settings.refresh='off')
             ORDER BY due_ms, project LIMIT ?2",
        )
        .map_err(unavailable)?;
    let names = statement
        .query_map(params![now_ms, STARTS_PER_TICK_MAX as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    let projects: Vec<ProjectId> = names
        .into_iter()
        .filter_map(|name| ProjectId::new(name).ok())
        .collect();
    assert!(projects.len() <= STARTS_PER_TICK_MAX);
    Ok(projects)
}

/// Starts the status runs that are due: a run whose time is over is closed with
/// `UNFINISHED`, then each due project is claimed (no longer stale, running from now) and
/// handed to `runs`. A landing during a run marks the project stale again, so it is checked
/// once more after the run. Inputs: the connection, the runs and now. Output: how many
/// runs started, or `roadmap_status_unavailable`.
pub fn tick(db: &Connection, runs: &mut dyn Runs, now_ms: i64) -> Result<usize, EngineError> {
    assert!(now_ms >= 0);
    db.execute(
        "UPDATE roadmap_status SET running_ms=NULL, problem=?1
         WHERE running_ms IS NOT NULL AND running_ms <= ?2",
        params![UNFINISHED, now_ms.saturating_sub(RUN_MS)],
    )
    .map_err(unavailable)?;
    let mut started = 0;
    for project in due(db, now_ms)? {
        let claimed = db
            .execute(
                "UPDATE roadmap_status SET stale=0, running_ms=?2
                 WHERE project=?1 AND stale=1 AND running_ms IS NULL",
                params![project.as_str(), now_ms],
            )
            .map_err(unavailable)?;
        if claimed == 1 {
            runs.start(&project);
            started += 1;
        }
    }
    assert!(started <= STARTS_PER_TICK_MAX);
    Ok(started)
}
