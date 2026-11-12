use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::questions::{self, Ask, EngineSource};
use crate::storage::versioned::{self, Item, Stale};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One day in milliseconds.
pub const DAY_MS: i64 = 86_400_000;
/// The days one scorecard window holds.
pub const WINDOW_DAYS: i64 = 14;
/// The first days of a window the `first_days` baseline takes.
pub const BASELINE_DAYS: i64 = 3;
/// The most marks the ledger keeps; the oldest go first.
pub const MARKS_KEPT_MAX: i64 = 100_000;
/// The most bytes of a mark's subject, such as an edited file's path.
pub const SUBJECT_BYTES_MAX: usize = 256;
/// The most real tasks one report lists, newest first; the counts cover all of them.
pub const TASKS_LISTED_MAX: usize = 30;
/// The most change sets one listed task names.
pub const CHANGE_SETS_LISTED_MAX: usize = 8;
/// The most owner acts one report lists, newest first.
pub const ACTS_LISTED_MAX: usize = 30;
/// The most daily answers one report reads.
pub const ANSWERS_READ_MAX: usize = 1000;
/// The furthest a reader's UTC offset is from UTC, in minutes.
pub const OFFSET_MINUTES_MAX: i32 = 14 * 60;
/// The go/no-go targets every report carries.
pub const TARGETS: Targets = Targets {
    tasks: 10,
    clean_of_ten: 8,
    coordination_cut_percent: 25,
    chosen_of_ten: 7,
};

const ASKED_AGAIN_MS: i64 = 20 * 60 * 60 * 1000;
const DAILY_QUESTION: &str = "Did you choose CrazyAgent for your work today?";
const DAILY_CONTEXT: &str = concat!(
    "The go/no-go scorecard counts the days you answer Yes. ",
    "Turn this question off in Settings, Usage, Scorecard."
);
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS scorecard_marks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN
        ('owner_message','answer','owner_edit','owner_stop','owner_interrupt','owner_restart',
         'discard','accept','daily')),
    subject TEXT NOT NULL,
    owner_edited INTEGER CHECK(owner_edited IN (0,1)),
    gate_passed INTEGER CHECK(gate_passed IN (0,1)),
    yes INTEGER CHECK(yes IN (0,1)),
    at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS scorecard_marks_at ON scorecard_marks(kind, at_ms);
CREATE INDEX IF NOT EXISTS scorecard_marks_subject ON scorecard_marks(kind, subject);
CREATE TABLE IF NOT EXISTS scorecard_settings (
    scope TEXT PRIMARY KEY NOT NULL CHECK(scope = 'app'),
    body TEXT NOT NULL
);";
const INSERT: &str =
    "INSERT INTO scorecard_marks(project,kind,subject,owner_edited,gate_passed,yes,at_ms)
    VALUES (?1,?2,?3,?4,?5,?6,?7)";
const PRUNE: &str =
    "DELETE FROM scorecard_marks WHERE id <= (SELECT MAX(id) FROM scorecard_marks) - ?1";
const REAL: &str = "t.status = 'done' AND t.closed_ms >= ?1 AND t.closed_ms < ?2 AND EXISTS (
    SELECT 1 FROM tk_links l JOIN scorecard_marks m ON m.kind = 'accept' AND m.subject = l.target
    WHERE l.ticket = t.id AND l.kind = 'change_set' AND NOT EXISTS (
        SELECT 1 FROM tk_links e JOIN tk_tickets o ON o.id = e.ticket
        WHERE e.kind = 'change_set' AND e.target = l.target AND o.status = 'done'
          AND (e.created_ms < l.created_ms
               OR (e.created_ms = l.created_ms AND e.ticket < l.ticket))))";
const LINKED: &str = "SELECT 1 FROM tk_links l JOIN scorecard_marks m ON m.subject = l.target
    WHERE l.ticket = t.id AND l.kind = 'change_set' AND ";
const DISCARDED: &str = "m.kind = 'discard'";
const EDITED: &str = "m.kind = 'accept' AND m.owner_edited = 1";
const UNGATED: &str = "m.kind = 'accept' AND m.gate_passed = 0";
const ACTS: &str = "'owner_message','answer','owner_edit','owner_stop','owner_interrupt',
    'owner_restart','discard'";

const _: () = assert!(BASELINE_DAYS < WINDOW_DAYS);
const _: () = assert!(OFFSET_MINUTES_MAX as i64 * 60_000 < DAY_MS);
const _: () = assert!(
    TASKS_LISTED_MAX
        * (2 * trackwright_core::text::TITLE_BYTES_MAX + CHANGE_SETS_LISTED_MAX * 48 + 160)
        + ACTS_LISTED_MAX * (2 * SUBJECT_BYTES_MAX + 160)
        + 2 * WINDOW_DAYS as usize * 48
        + 2048
        <= crate::protocol::FRAME_BYTES_MAX
);

/// One owner act the scorecard counts, written to its ledger when it happens: an owner chat
/// message, an answered question, an owner edit of an agent's file, an owner stop,
/// interrupt or restart of an agent, an owner discard, an owner accept with its clean
/// facts, and a daily answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    OwnerMessage,
    Answer,
    OwnerEdit,
    OwnerStop,
    OwnerInterrupt,
    OwnerRestart,
    Discard,
    Accept,
    Daily,
}

impl Kind {
    const ALL: [Kind; 9] = [
        Kind::OwnerMessage,
        Kind::Answer,
        Kind::OwnerEdit,
        Kind::OwnerStop,
        Kind::OwnerInterrupt,
        Kind::OwnerRestart,
        Kind::Discard,
        Kind::Accept,
        Kind::Daily,
    ];

    /// The stored name. Output: the snake case name.
    pub fn name(self) -> &'static str {
        match self {
            Kind::OwnerMessage => "owner_message",
            Kind::Answer => "answer",
            Kind::OwnerEdit => "owner_edit",
            Kind::OwnerStop => "owner_stop",
            Kind::OwnerInterrupt => "owner_interrupt",
            Kind::OwnerRestart => "owner_restart",
            Kind::Discard => "discard",
            Kind::Accept => "accept",
            Kind::Daily => "daily",
        }
    }

    fn parse(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// Which 14 days a report reads: the 14 local days ending today (the proposed default), or
/// 14 days from a chosen start day, `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    #[default]
    Rolling,
    FromDay(String),
}

/// Which span coordination is compared against: the window's first 3 days, the rest being
/// measured (the proposed default), or the 14 days before the window, the window being
/// measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Baseline {
    #[default]
    FirstDays,
    PreviousWindow,
}

impl Baseline {
    /// Every option, the default first.
    pub const ALL: [Baseline; 2] = [Baseline::FirstDays, Baseline::PreviousWindow];
}

/// Whether the engine asks the owner once a day whether they chose CrazyAgent for their
/// work: off (the proposed default until the real-use period starts) or on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DailyQuestion {
    #[default]
    Off,
    On,
}

impl DailyQuestion {
    /// Every option, the default first.
    pub const ALL: [DailyQuestion; 2] = [DailyQuestion::Off, DailyQuestion::On];
}

/// The app's scorecard settings, every key set.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub window: Window,
    pub baseline: Baseline,
    pub daily_question: DailyQuestion,
}

/// The stored settings and the version the owner's screen sends back to change them, 0
/// before the first write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub settings: Settings,
    pub version: u64,
}

/// A span of time, from `start_ms` up to but not including `end_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Why a real task is not clean: a linked change set was discarded, the owner edited an
/// accepted one, or an accepted one's first check batch did not all pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flaw {
    Discarded,
    OwnerEdited,
    FirstBatchNotPassed,
}

/// One real task: a done ticket that is the first done ticket to link an accepted change
/// set, so one set backs one task however many tickets link it, by its short id, its
/// title, when it closed, the change sets it links (at most `CHANGE_SETS_LISTED_MAX`),
/// whether it is clean and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub ticket: String,
    pub title: String,
    pub closed_ms: i64,
    pub change_sets: Vec<String>,
    pub clean: bool,
    pub flaws: Vec<Flaw>,
}

/// The owner acts of one span and the real tasks closed in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Tally {
    pub messages: u32,
    pub answers: u32,
    pub interventions: u32,
    pub tasks: u32,
}

impl Tally {
    /// Every counted owner act of the span. Output: the sum.
    pub fn acts(&self) -> u32 {
        self.messages + self.answers + self.interventions
    }
}

/// Coordination of the baseline span against the measured span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coordination {
    pub baseline: Tally,
    pub measured: Tally,
}

/// One owner act behind the coordination numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Act {
    pub kind: Kind,
    pub project: String,
    pub subject: String,
    pub at_ms: i64,
}

/// One local day the owner answered the daily question, and whether any answer was yes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayAnswer {
    pub day: String,
    pub yes: bool,
}

/// The go/no-go targets: 10 real tasks, 8 of 10 clean, 25 percent less coordination per
/// task than the baseline, and 7 of 10 answered days chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Targets {
    pub tasks: u32,
    pub clean_of_ten: u32,
    pub coordination_cut_percent: u32,
    pub chosen_of_ten: u32,
}

/// Which targets the report meets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Met {
    pub tasks: bool,
    pub clean: bool,
    pub coordination: bool,
    pub chosen: bool,
}

/// One scorecard reading: the settings and version, the spans, the real and clean task
/// counts with the newest tasks, coordination with the newest acts behind it, the daily
/// answers, the targets and which are met.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scorecard {
    pub settings: Settings,
    pub version: u64,
    pub window: Span,
    pub baseline: Span,
    pub measured: Span,
    pub real: u32,
    pub clean: u32,
    pub tasks: Vec<Task>,
    pub coordination: Coordination,
    pub acts: Vec<Act>,
    pub chosen_days: u32,
    pub answered_days: u32,
    pub days: Vec<DayAnswer>,
    pub targets: Targets,
    pub met: Met,
}

/// Writes one owner act to the ledger in the caller's transaction, dropping the oldest
/// marks past `MARKS_KEPT_MAX`. A subject past `SUBJECT_BYTES_MAX` keeps its first bytes.
/// Accepts and daily answers have their own writers. Inputs: the connection or transaction,
/// the project, the act, its subject and its time. Output: Ok once written.
pub(crate) fn mark(
    db: &Connection,
    project: &ProjectId,
    kind: Kind,
    subject: &str,
    at_ms: i64,
) -> Result<(), EngineError> {
    assert!(kind != Kind::Accept);
    assert!(kind != Kind::Daily);
    insert(db, project, kind, subject, Facts::default(), at_ms)
}

/// Writes one owner act on a running agent from the Agents page, a stop, an interrupt or a
/// restart, after it took effect, logging a failed write by its code so the act stands.
/// Inputs: the engine root, the agent's project, the act and the agent id. Output: none.
pub(crate) fn intervened(root: &std::path::Path, project: &ProjectId, kind: Kind, agent: &str) {
    assert!(matches!(
        kind,
        Kind::OwnerStop | Kind::OwnerInterrupt | Kind::OwnerRestart
    ));
    assert!(!agent.is_empty());
    let written = questions::open(root).and_then(|db| mark(&db, project, kind, agent, now_ms()));
    logged(kind, written);
}

/// Logs a failed mark by its plain code, so the owner's act stands: the scorecard measures
/// acts and never refuses or rolls one back, inside the act's transaction or after it.
/// Inputs: the act and the write. Output: none.
pub(crate) fn logged(kind: Kind, written: Result<(), EngineError>) {
    if let Err(problem) = written {
        eprintln!(
            "scorecard_mark_failed kind={} cause={}",
            kind.name(),
            crate::protocol::plain_code(&problem.0)
        );
    }
}

/// Writes the Accept of a change set with the facts that decide whether its task is clean,
/// read now because Accept forgets the owner edits and a run may be pruned later: whether
/// the owner edited a file of it, and whether every run of its first check batch, the
/// batch of its lowest run, finished `passed`. No run is no passing gate. Inputs: the
/// store, the project, the change set and the time. Output: Ok once written.
pub(crate) fn accepted(
    db: &Connection,
    project: &ProjectId,
    change_set: &str,
    at_ms: i64,
) -> Result<(), EngineError> {
    assert!(!change_set.is_empty());
    let owner_edited = table_exists(db, "landing_owner_edits")?
        && db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM landing_owner_edits WHERE change_set=?1)",
                [change_set],
                |row| row.get::<_, bool>(0),
            )
            .map_err(unavailable)?;
    let gate_passed = first_batch_passed(db, change_set)?;
    let facts = Facts {
        owner_edited: Some(owner_edited),
        gate_passed: Some(gate_passed),
        yes: None,
    };
    insert(db, project, Kind::Accept, change_set, facts, at_ms)
}

/// Writes the owner's settling of a question in the caller's transaction: a daily answer,
/// yes when it reads "yes" in any case, for the scorecard's own question, else an answered
/// question, logging a failed write so the settling stands. Inputs: the transaction, the
/// project, the stored asker, the question id, the answer text (none for one answered in a
/// terminal) and the time. Output: none.
pub(crate) fn answered(
    tx: &Connection,
    project: &ProjectId,
    asker: &str,
    question: u64,
    answer: Option<&str>,
    at_ms: i64,
) {
    assert!(question > 0);
    let subject = question.to_string();
    if Some(asker) != EngineSource::Scorecard.name().as_deref() {
        let written = insert(tx, project, Kind::Answer, &subject, Facts::default(), at_ms);
        logged(Kind::Answer, written);
        return;
    }
    let yes = answer.is_some_and(|text| text.trim().eq_ignore_ascii_case("yes"));
    let facts = Facts {
        yes: Some(yes),
        ..Facts::default()
    };
    logged(
        Kind::Daily,
        insert(tx, project, Kind::Daily, &subject, facts, at_ms),
    );
}

#[derive(Debug, Clone, Copy, Default)]
struct Facts {
    owner_edited: Option<bool>,
    gate_passed: Option<bool>,
    yes: Option<bool>,
}

fn insert(
    db: &Connection,
    project: &ProjectId,
    kind: Kind,
    subject: &str,
    facts: Facts,
    at_ms: i64,
) -> Result<(), EngineError> {
    assert_eq!(kind == Kind::Accept, facts.gate_passed.is_some());
    assert_eq!(kind == Kind::Daily, facts.yes.is_some());
    let mut end = subject.len().min(SUBJECT_BYTES_MAX);
    while !subject.is_char_boundary(end) {
        end -= 1;
    }
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let written = db
        .execute(
            INSERT,
            params![
                project.as_str(),
                kind.name(),
                &subject[..end],
                facts.owner_edited,
                facts.gate_passed,
                facts.yes,
                at_ms
            ],
        )
        .map_err(unavailable)?;
    assert_eq!(written, 1);
    db.execute(PRUNE, [MARKS_KEPT_MAX]).map_err(unavailable)?;
    Ok(())
}

fn first_batch_passed(db: &Connection, change_set: &str) -> Result<bool, EngineError> {
    if !table_exists(db, "check_runs")? || !table_exists(db, "tk_runs")? {
        return Ok(false);
    }
    let batch: Option<String> = db
        .query_row(
            "SELECT batch FROM check_runs WHERE change_set=?1 ORDER BY run LIMIT 1",
            [change_set],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    let Some(batch) = batch else {
        return Ok(false);
    };
    let (runs, passed): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*), COUNT(r.id) FILTER (WHERE r.state='finished' AND r.verdict='passed')
               FROM check_runs c LEFT JOIN tk_runs r ON r.id = c.run
              WHERE c.change_set=?1 AND c.batch=?2",
            params![change_set, batch],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(unavailable)?;
    assert!(runs >= 1);
    assert!(passed <= runs);
    Ok(passed == runs)
}

/// Reads the app's scorecard settings. Input: the engine connection. Output: the settings
/// and their version, or `scorecard_unavailable`.
pub fn stored(db: &Connection) -> Result<Stored, EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM scorecard_settings WHERE scope='app'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    let settings: Settings = match body {
        Some(body) => serde_json::from_str(&body).map_err(unavailable)?,
        None => Settings::default(),
    };
    if let Window::FromDay(day) = &settings.window
        && day_number(day).is_none()
    {
        return Err(failure("scorecard_unavailable"));
    }
    let version = versioned::current(db, &item()?)?.version;
    assert!(version > 0 || settings == Settings::default());
    Ok(Stored { settings, version })
}

/// Saves the owner's scorecard settings when the owner saw the current version. Inputs: the
/// engine connection, the settings and the version the owner's screen showed. Output: the
/// new version, the stale refusal (nothing written), `scorecard_settings_invalid` for a
/// start day that is no calendar day, or `scorecard_unavailable`.
pub fn set(
    db: &Connection,
    settings: &Settings,
    expected: u64,
) -> Result<Result<u64, Stale>, EngineError> {
    if let Window::FromDay(day) = &settings.window
        && day_number(day).is_none()
    {
        return Err(failure("scorecard_settings_invalid"));
    }
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    let version = match versioned::owner_write(&tx, &item()?, expected)? {
        Ok(version) => version,
        Err(stale) => return Ok(Err(stale)),
    };
    let body = serde_json::to_string(settings).map_err(unavailable)?;
    let written = tx
        .execute(
            "INSERT INTO scorecard_settings(scope, body) VALUES ('app', ?1)
             ON CONFLICT(scope) DO UPDATE SET body = excluded.body",
            [body],
        )
        .map_err(unavailable)?;
    assert_eq!(written, 1);
    tx.commit().map_err(unavailable)?;
    assert_eq!(stored(db)?.version, version);
    Ok(Ok(version))
}

fn item() -> Result<Item, EngineError> {
    Item::new("scorecard:app")
}

/// Asks the owner the daily question in the first of the projects when the setting is on,
/// no scorecard question was asked in the last 20 hours and none waits unanswered, so an
/// unanswered day carries over. Inputs: the engine connection, the active projects and the
/// time. Output: the question asked, or none.
pub fn ask_daily(
    db: &Connection,
    projects: &[ProjectId],
    now_ms: i64,
) -> Result<Option<u64>, EngineError> {
    let Some(project) = projects.first() else {
        return Ok(None);
    };
    if stored(db)?.settings.daily_question == DailyQuestion::Off {
        return Ok(None);
    }
    let asker = EngineSource::Scorecard
        .name()
        .expect("the scorecard source is named");
    if table_exists(db, "owner_questions")? {
        let waiting: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM owner_questions
                  WHERE asker=?1 AND (status='pending' OR created_ms > ?2))",
                params![asker, now_ms - ASKED_AGAIN_MS],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if waiting {
            return Ok(None);
        }
    }
    let ask = Ask {
        question: DAILY_QUESTION.into(),
        kind: questions::QuestionKind::Single,
        options: vec!["Yes".into(), "No".into()],
        recommended: None,
        context: Some(DAILY_CONTEXT.into()),
    };
    let id = questions::raise(db, project, &EngineSource::Scorecard, &ask)?;
    assert!(id > 0);
    Ok(Some(id))
}

/// Computes the scorecard over the window the settings choose, in the reader's local days.
/// Inputs: the engine connection, the time and the reader's UTC offset in minutes, at most
/// `OFFSET_MINUTES_MAX` either way. Output: the scorecard, or `scorecard_unavailable`.
pub fn report(db: &Connection, now_ms: i64, offset_minutes: i32) -> Result<Scorecard, EngineError> {
    assert!((-OFFSET_MINUTES_MAX..=OFFSET_MINUTES_MAX).contains(&offset_minutes));
    let Stored { settings, version } = stored(db)?;
    let (window, baseline, measured) = spans(&settings, now_ms, offset_minutes);
    let tracked = table_exists(db, "tk_tickets")? && table_exists(db, "tk_links")?;
    let (real, clean) = if tracked { counts(db, window)? } else { (0, 0) };
    let tasks = if tracked {
        tasks(db, window)?
    } else {
        Vec::new()
    };
    let coordination = Coordination {
        baseline: tally(db, baseline, tracked)?,
        measured: tally(db, measured, tracked)?,
    };
    let from = Span {
        start_ms: baseline.start_ms.min(window.start_ms),
        end_ms: window.end_ms,
    };
    let acts = acts(db, from)?;
    let days = days(db, window, offset_minutes)?;
    let answered_days = u32::try_from(days.len()).map_err(unavailable)?;
    let chosen_days =
        u32::try_from(days.iter().filter(|day| day.yes).count()).map_err(unavailable)?;
    let met = met(real, clean, &coordination, chosen_days, answered_days);
    assert!(clean <= real);
    assert!(chosen_days <= answered_days);
    Ok(Scorecard {
        settings,
        version,
        window,
        baseline,
        measured,
        real,
        clean,
        tasks,
        coordination,
        acts,
        chosen_days,
        answered_days,
        days,
        targets: TARGETS,
        met,
    })
}

fn spans(settings: &Settings, now_ms: i64, offset_minutes: i32) -> (Span, Span, Span) {
    let shift = i64::from(offset_minutes) * 60_000;
    let first = match &settings.window {
        Window::Rolling => (now_ms + shift).div_euclid(DAY_MS) - (WINDOW_DAYS - 1),
        Window::FromDay(day) => day_number(day).expect("a stored start day is a calendar day"),
    };
    let start_ms = first * DAY_MS - shift;
    let window = Span {
        start_ms,
        end_ms: start_ms + WINDOW_DAYS * DAY_MS,
    };
    let (baseline, measured) = match settings.baseline {
        Baseline::FirstDays => {
            let split = start_ms + BASELINE_DAYS * DAY_MS;
            let baseline = Span {
                start_ms,
                end_ms: split,
            };
            let measured = Span {
                start_ms: split,
                end_ms: window.end_ms,
            };
            (baseline, measured)
        }
        Baseline::PreviousWindow => {
            let baseline = Span {
                start_ms: start_ms - WINDOW_DAYS * DAY_MS,
                end_ms: start_ms,
            };
            (baseline, window)
        }
    };
    assert!(baseline.start_ms < baseline.end_ms);
    assert!(measured.end_ms == window.end_ms);
    (window, baseline, measured)
}

fn counts(db: &Connection, span: Span) -> Result<(u32, u32), EngineError> {
    let sql = format!(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE NOT EXISTS ({LINKED}{DISCARDED})
                AND NOT EXISTS ({LINKED}{EDITED}) AND NOT EXISTS ({LINKED}{UNGATED}))
           FROM tk_tickets t WHERE {REAL}"
    );
    let (real, clean): (u32, u32) = db
        .query_row(&sql, params![span.start_ms, span.end_ms], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(unavailable)?;
    assert!(clean <= real);
    Ok((real, clean))
}

fn tasks(db: &Connection, span: Span) -> Result<Vec<Task>, EngineError> {
    let sql = format!(
        "SELECT t.id, p.key || '-' || t.number, t.title, t.closed_ms,
                EXISTS ({LINKED}{DISCARDED}), EXISTS ({LINKED}{EDITED}), EXISTS ({LINKED}{UNGATED})
           FROM tk_tickets t JOIN tk_projects p ON p.id = t.project
          WHERE {REAL} ORDER BY t.closed_ms DESC, t.id DESC LIMIT ?3"
    );
    let mut statement = db.prepare(&sql).map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![span.start_ms, span.end_ms, TASKS_LISTED_MAX as i64],
            |row| -> rusqlite::Result<(i64, String, String, i64, [bool; 3])> {
                let flags = [row.get(4)?, row.get(5)?, row.get(6)?];
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, flags))
            },
        )
        .map_err(unavailable)?;
    let mut listed = Vec::new();
    for row in rows {
        let (id, ticket, title, closed_ms, flags) = row.map_err(unavailable)?;
        let flaws: Vec<Flaw> = [
            Flaw::Discarded,
            Flaw::OwnerEdited,
            Flaw::FirstBatchNotPassed,
        ]
        .into_iter()
        .zip(flags)
        .filter_map(|(flaw, found)| found.then_some(flaw))
        .collect();
        listed.push(Task {
            ticket,
            title,
            closed_ms,
            change_sets: linked(db, id)?,
            clean: flaws.is_empty(),
            flaws,
        });
    }
    assert!(listed.len() <= TASKS_LISTED_MAX);
    Ok(listed)
}

fn linked(db: &Connection, ticket: i64) -> Result<Vec<String>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT target FROM tk_links WHERE ticket=?1 AND kind='change_set'
              ORDER BY created_ms, target LIMIT ?2",
        )
        .map_err(unavailable)?;
    let sets = statement
        .query_map(params![ticket, CHANGE_SETS_LISTED_MAX as i64], |row| {
            row.get(0)
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(unavailable)?;
    assert!(!sets.is_empty());
    assert!(sets.len() <= CHANGE_SETS_LISTED_MAX);
    Ok(sets)
}

fn tally(db: &Connection, span: Span, tracked: bool) -> Result<Tally, EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let mut statement = db
        .prepare(&format!(
            "SELECT kind, COUNT(*) FROM scorecard_marks
              WHERE kind IN ({ACTS}) AND at_ms >= ?1 AND at_ms < ?2 GROUP BY kind"
        ))
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![span.start_ms, span.end_ms], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
        })
        .map_err(unavailable)?;
    let mut tally = Tally::default();
    for row in rows {
        let (kind, count) = row.map_err(unavailable)?;
        match Kind::parse(&kind).ok_or_else(|| failure("scorecard_unavailable"))? {
            Kind::OwnerMessage => tally.messages += count,
            Kind::Answer => tally.answers += count,
            Kind::OwnerEdit
            | Kind::OwnerStop
            | Kind::OwnerInterrupt
            | Kind::OwnerRestart
            | Kind::Discard => tally.interventions += count,
            Kind::Accept | Kind::Daily => unreachable!("the query reads owner acts only"),
        }
    }
    if tracked {
        tally.tasks = counts(db, span)?.0;
    }
    assert!(tally.acts() >= tally.messages);
    Ok(tally)
}

fn acts(db: &Connection, span: Span) -> Result<Vec<Act>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT kind, project, subject, at_ms FROM scorecard_marks
              WHERE kind IN ({ACTS}) AND at_ms >= ?1 AND at_ms < ?2
              ORDER BY at_ms DESC, id DESC LIMIT ?3"
        ))
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![span.start_ms, span.end_ms, ACTS_LISTED_MAX as i64],
            |row| -> rusqlite::Result<(String, String, String, i64)> {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            },
        )
        .map_err(unavailable)?;
    let mut listed = Vec::new();
    for row in rows {
        let (kind, project, subject, at_ms) = row.map_err(unavailable)?;
        let kind = Kind::parse(&kind).ok_or_else(|| failure("scorecard_unavailable"))?;
        listed.push(Act {
            kind,
            project,
            subject,
            at_ms,
        });
    }
    assert!(listed.len() <= ACTS_LISTED_MAX);
    Ok(listed)
}

fn days(db: &Connection, span: Span, offset_minutes: i32) -> Result<Vec<DayAnswer>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT at_ms, yes FROM scorecard_marks
              WHERE kind='daily' AND at_ms >= ?1 AND at_ms < ?2 ORDER BY at_ms LIMIT ?3",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![span.start_ms, span.end_ms, ANSWERS_READ_MAX as i64],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
        )
        .map_err(unavailable)?;
    let shift = i64::from(offset_minutes) * 60_000;
    let mut by_day: BTreeMap<i64, bool> = BTreeMap::new();
    for row in rows {
        let (at_ms, yes) = row.map_err(unavailable)?;
        *by_day
            .entry((at_ms + shift).div_euclid(DAY_MS))
            .or_default() |= yes;
    }
    assert!(by_day.len() <= WINDOW_DAYS as usize);
    let days: Vec<DayAnswer> = by_day
        .into_iter()
        .map(|(day, yes)| DayAnswer {
            day: day_label(day * DAY_MS, 0),
            yes,
        })
        .collect();
    assert!(days.len() <= ANSWERS_READ_MAX);
    Ok(days)
}

fn met(real: u32, clean: u32, coordination: &Coordination, chosen: u32, answered: u32) -> Met {
    let Coordination { baseline, measured } = coordination;
    let cut = u64::from(100 - TARGETS.coordination_cut_percent);
    let coordination = baseline.tasks > 0
        && measured.tasks > 0
        && u64::from(measured.acts()) * u64::from(baseline.tasks) * 100
            <= cut * u64::from(baseline.acts()) * u64::from(measured.tasks);
    Met {
        tasks: real >= TARGETS.tasks,
        clean: real >= TARGETS.tasks && clean * 10 >= real * TARGETS.clean_of_ten,
        coordination,
        chosen: answered >= 10 && chosen * 10 >= answered * TARGETS.chosen_of_ten,
    }
}

/// The local calendar day of a time, `YYYY-MM-DD`. Inputs: the time and the UTC offset in
/// minutes. Output: the day.
pub fn day_label(ms: i64, offset_minutes: i32) -> String {
    let days = (ms + i64::from(offset_minutes) * 60_000).div_euclid(DAY_MS);
    let (year, month, day) = civil(days);
    assert!((1..=12).contains(&month));
    assert!((1..=31).contains(&day));
    format!("{year:04}-{month:02}-{day:02}")
}

/// The days since 1970-01-01 of a calendar day written `YYYY-MM-DD`. Input: the text.
/// Output: the day number, or none for text that is no calendar day.
pub fn day_number(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(at, byte)| at == 4 || at == 7 || byte.is_ascii_digit());
    if !shaped {
        return None;
    }
    let year: i64 = text[..4].parse().ok()?;
    let month: u32 = text[5..7].parse().ok()?;
    let day: u32 = text[8..].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let month_index = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let number = era * 146_097 + day_of_era - 719_468;
    (civil(number) == (year, month, day)).then_some(number)
}

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn table_exists(db: &Connection, name: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

/// The time now in milliseconds since the Unix epoch. Output: the time.
pub fn now_ms() -> i64 {
    crate::team::now_ms()
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    failure("scorecard_unavailable")
}

#[cfg(all(test, unix))]
mod tests;
