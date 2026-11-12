use super::integrity;
use super::profile::{CHECKS_MAX, HOLDOUTS_MAX};
use super::record::{self, CheckRun};
use super::settings::{self, GateMode, ProtectedChange, RemovedTests, SkipMarkers};
use super::{AdmittedProfile, CheckRefusal, ProfileId};
use crate::engine::owner::failure;
use crate::landing::{self, ChangeSetId};
use crate::team::now_ms;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use trackwright_core::model::ShortId;
use trackwright_core::runs::{IntegrityRef, RunState, Verdict};

/// The gap of a change set touching a protected path under `protected_change refuse`; it
/// refuses Accept under `warn` and `require`.
pub const PROTECTED_CHANGE: &str = "checks_protected_change";
/// The gap of a change set adding a skip under `skip_markers refuse`: a skip marker in its
/// changed test files, or a test a current passing run skipped; it refuses Accept under
/// `warn` and `require`.
pub const NEW_SKIPS: &str = "checks_new_skips";
/// The gap of a required check whose current passing run no longer reports tests its
/// inventory baseline had, or has no readable integrity block, unless `removed_tests` is
/// `off`; it refuses Accept under `require`, and under `warn` too with `removed_tests refuse`.
pub const TESTS_REMOVED: &str = "checks_tests_removed";
/// The gap of a required check whose run on the reviewed head ended without passing.
pub const FAILED: &str = "checks_failed";
/// The gap of a required check whose newest run covered another head, or ran under another
/// profile or an earlier approval of the current one.
pub const STALE: &str = "checks_stale";
/// The gap of a required check with no run, a stopped run or a run that never started, and
/// of a project with no current profile or a current profile that requires no check.
pub const NOT_RUN: &str = "checks_not_run";
/// Every gap code, in the order the gate picks the one it refuses with.
pub const CODES: [&str; 6] = [
    PROTECTED_CHANGE,
    NEW_SKIPS,
    TESTS_REMOVED,
    FAILED,
    STALE,
    NOT_RUN,
];
/// The most paths one gap lists; `paths_total` counts them all.
pub const PATHS_MAX: usize = 5;
/// The most linked tickets one note names.
pub const TICKETS_MAX: usize = 64;
/// The most notes one read returns, newest first.
pub const NOTES_READ_MAX: usize = 64;
/// The most bytes one stored note holds.
pub const NOTE_BYTES_MAX: usize = 256 * 1024;

const GAPS_MAX: usize = 2 * (CHECKS_MAX + HOLDOUTS_MAX) + 2;
const GAP_BYTES_MAX: usize = 4 * 1024;
const _: () = assert!(GAPS_MAX * GAP_BYTES_MAX + GAP_BYTES_MAX <= NOTE_BYTES_MAX);
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS check_gate_notes (
    change_set TEXT NOT NULL,
    head TEXT NOT NULL,
    note TEXT NOT NULL,
    tickets TEXT NOT NULL,
    recorded_ms INTEGER NOT NULL,
    PRIMARY KEY (change_set, head)
);";
const LINKED: &str = "SELECT p.key || '-' || t.number FROM tk_links l
    JOIN tk_tickets t ON t.id = l.ticket JOIN tk_projects p ON p.id = t.project
   WHERE l.kind = 'change_set' AND l.target = ?1 ORDER BY t.id LIMIT ?2";
const NOTES: &str = "SELECT change_set, note, tickets, recorded_ms FROM check_gate_notes
   WHERE change_set = ?1 ORDER BY recorded_ms DESC, head LIMIT ?2";
const TICKET_NOTES: &str = "SELECT n.change_set, n.note, n.tickets, n.recorded_ms
    FROM check_gate_notes n
   WHERE EXISTS (SELECT 1 FROM json_each(n.tickets) t WHERE t.value = ?1)
   ORDER BY n.recorded_ms DESC, n.change_set, n.head LIMIT ?2";

/// One thing the gate found missing for a change set's reviewed head: its code, the check
/// it is about (None for the project or the change as a whole), the run it read as
/// `<KEY>-R<n>`, and for a protected change or new skips at most `PATHS_MAX` paths or test
/// names with how many there are in all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gap {
    pub code: String,
    pub check: Option<String>,
    pub run: Option<String>,
    pub paths: Vec<String>,
    pub paths_total: u32,
}

/// What the gate found for one reviewed head: the gate and removed tests settings in
/// effect, the head, the current profile (None when the project has none) and every gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateNote {
    pub gate: GateMode,
    pub removed_tests: RemovedTests,
    pub head: String,
    pub profile: Option<ProfileId>,
    pub gaps: Vec<Gap>,
}

/// One note the gate kept when it let an Accept through under `warn` with gaps: the change
/// set, the note, the tickets linked to the change set then, and when. The change set's
/// state tells whether that Accept landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateRecord {
    pub change_set: ChangeSetId,
    pub note: GateNote,
    pub tickets: Vec<String>,
    pub recorded_ms: i64,
}

impl Gap {
    fn new(
        code: &'static str,
        check: Option<&str>,
        run: Option<&CheckRun>,
        paths: BTreeSet<String>,
    ) -> Self {
        let paths_total = u32::try_from(paths.len()).unwrap_or(u32::MAX);
        let paths: Vec<String> = paths
            .iter()
            .take(PATHS_MAX)
            .map(|path| integrity::recorded(path))
            .collect();
        assert!(paths.len() <= PATHS_MAX);
        Self {
            code: code.into(),
            check: check.map(str::to_owned),
            run: run.map(|run| run.id.to_string()),
            paths,
            paths_total,
        }
    }
}

fn unavailable<E>(_: E) -> CheckRefusal {
    failure("checks_unavailable")
}

fn initialize(db: &Connection) -> Result<(), CheckRefusal> {
    db.execute_batch(SCHEMA).map_err(unavailable)
}

/// Decides an Accept of a change set at its reviewed head, as every Accept calls it under
/// the repository hold and again in the transaction that moves the branch: `refusal` picks
/// the refusal, and under `warn` other gaps are kept as a note on the change set and its
/// linked tickets. Inputs: the engine connection, the change set and the head. Output: the
/// note, or a `CODES` refusal, `changes_not_found`, `changes_stale` or `checks_unavailable`.
pub fn admit(
    db: &Connection,
    change_set: &ChangeSetId,
    expected_head: &str,
) -> Result<GateNote, CheckRefusal> {
    let note = assess(db, change_set, expected_head)?;
    if let Some(code) = refusal(&note) {
        return Err(failure(code));
    }
    if !note.gaps.is_empty() {
        assert_eq!(note.gate, GateMode::Warn);
        remember(db, change_set, &note)?;
    }
    Ok(note)
}

/// Reads the gaps of a change set at a head, writing nothing: the profile must require a
/// check, and each required one, holdouts included, needs its newest run to pass on that
/// head under the current profile and approval without removing tests; a protected change
/// and new skips count under `refuse`. Inputs: the engine connection, the change set and
/// the head. Output: the note, or `changes_not_found`, `changes_stale`, `checks_unavailable`.
pub fn assess(
    db: &Connection,
    change_set: &ChangeSetId,
    expected_head: &str,
) -> Result<GateNote, CheckRefusal> {
    super::initialize(db)?;
    let (project, view) =
        landing::change_set(db, change_set)?.ok_or_else(|| failure("changes_not_found"))?;
    if view.head != expected_head {
        return Err(failure("changes_stale"));
    }
    let settings = settings::effective(db, &project)?;
    let mut note = GateNote {
        gate: settings.gate,
        removed_tests: settings.removed_tests,
        head: view.head.clone(),
        profile: None,
        gaps: Vec::new(),
    };
    if settings.gate == GateMode::Off {
        return Ok(note);
    }
    let Some(profile) = super::current_profile(db, &project)? else {
        note.gaps
            .push(Gap::new(NOT_RUN, None, None, BTreeSet::new()));
        return Ok(note);
    };
    note.profile = Some(profile.id.clone());
    let runs = record::latest(db, change_set)?;
    let checks = profile.checks();
    let mut required = checks.iter().filter(|check| check.spec.required).peekable();
    if required.peek().is_none() {
        note.gaps
            .push(Gap::new(NOT_RUN, None, None, BTreeSet::new()));
    }
    let mut passed = Vec::new();
    for check in required {
        let name = check.spec.id.as_str();
        let run = runs.iter().find(|run| run.check == name);
        let code = match run {
            None => NOT_RUN,
            Some(run) if !covers(run, &view.head, &profile) => STALE,
            Some(run)
                if run.state != RunState::Finished || run.verdict == Some(Verdict::Unavailable) =>
            {
                NOT_RUN
            }
            Some(run) if run.verdict == Some(Verdict::Passed) => {
                passed.push(run);
                continue;
            }
            Some(_) => FAILED,
        };
        note.gaps
            .push(Gap::new(code, Some(name), run, BTreeSet::new()));
    }
    let protect = settings.protected_change == ProtectedChange::Refuse
        && !profile.profile.protected_paths.is_empty();
    let skips = settings.skip_markers == SkipMarkers::Refuse;
    if protect || skips {
        let files = landing::ide::files(db, &project, change_set)?;
        if protect {
            let touched: BTreeSet<String> =
                integrity::protected(&files, &profile.profile.protected_paths)
                    .into_iter()
                    .collect();
            if !touched.is_empty() {
                note.gaps
                    .push(Gap::new(PROTECTED_CHANGE, None, None, touched));
            }
        }
        if skips {
            let added: BTreeSet<String> =
                integrity::skips(db, &project, change_set, &view.head, &files)?
                    .into_iter()
                    .map(|skip| skip.path)
                    .collect();
            if !added.is_empty() {
                note.gaps.push(Gap::new(NEW_SKIPS, None, None, added));
            }
        }
    }
    for run in passed {
        let block = integrity_of(db, run)?;
        if skips && let Some(names) = skipped_in(block.as_ref(), run) {
            note.gaps
                .push(Gap::new(NEW_SKIPS, Some(&run.check), Some(run), names));
        }
        if settings.removed_tests != RemovedTests::Off
            && let Some((names, total)) = removed_in(block.as_ref(), run)
        {
            let mut gap = Gap::new(TESTS_REMOVED, Some(&run.check), Some(run), names);
            gap.paths_total = gap.paths_total.max(total);
            note.gaps.push(gap);
        }
    }
    assert!(note.gaps.len() <= GAPS_MAX);
    assert!(
        note.gaps
            .iter()
            .all(|gap| CODES.contains(&gap.code.as_str()))
    );
    Ok(note)
}

/// The code a note refuses Accept with: none under `off`; under `warn` a protected change,
/// new skips, or removed tests under `removed_tests refuse`; under `require` any gap, the
/// first of `CODES` present. Input: the note. Output: the code, or None when Accept may
/// proceed.
pub fn refusal(note: &GateNote) -> Option<&'static str> {
    let present = |code: &&str| note.gaps.iter().any(|gap| gap.code == *code);
    match note.gate {
        GateMode::Off => None,
        GateMode::Warn => {
            let removed = (note.removed_tests == RemovedTests::Refuse).then_some(TESTS_REMOVED);
            [Some(PROTECTED_CHANGE), Some(NEW_SKIPS), removed]
                .into_iter()
                .flatten()
                .find(present)
        }
        GateMode::Require => CODES.iter().copied().find(present),
    }
}

/// The notes the gate kept for one change set, newest first, at most `NOTES_READ_MAX`.
/// Inputs: the engine connection and the change set. Output: the notes, or
/// `checks_unavailable`.
pub fn notes(db: &Connection, change_set: &ChangeSetId) -> Result<Vec<GateRecord>, CheckRefusal> {
    initialize(db)?;
    read(db, NOTES, change_set.as_str())
}

/// The notes the gate kept for the change sets one ticket was linked to when they were
/// written, newest first, at most `NOTES_READ_MAX`. Inputs: the engine connection and the
/// ticket. Output: the notes, or `checks_unavailable`.
pub fn ticket_notes(db: &Connection, ticket: &ShortId) -> Result<Vec<GateRecord>, CheckRefusal> {
    initialize(db)?;
    read(db, TICKET_NOTES, &ticket.to_string())
}

fn covers(run: &CheckRun, head: &str, profile: &AdmittedProfile) -> bool {
    run.commit == head
        && run.profile == profile.id
        && run.profile_approved_ms == profile.approved_ms
}

/// The integrity block of one run, None when it is missing or unreadable. Inputs: the
/// engine connection and the run. Output: the block, or `checks_unavailable`.
fn integrity_of(db: &Connection, run: &CheckRun) -> Result<Option<IntegrityRef>, CheckRefusal> {
    let block: Option<String> = db
        .query_row(
            "SELECT integrity_json FROM tk_runs WHERE id = ?1",
            [run.run],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?
        .flatten();
    Ok(block
        .as_deref()
        .and_then(|text| IntegrityRef::parse(text).ok()))
}

/// The tests one passing run's integrity block lists as new skips: None when it lists
/// none, no names for a holdout run so its tests stay unnamed, and no names when the block
/// is missing or unreadable, which counts as a skip so `refuse` fails closed. Inputs: the
/// block and the run. Output: the names.
fn skipped_in(block: Option<&IntegrityRef>, run: &CheckRun) -> Option<BTreeSet<String>> {
    match block.map(|block| &block.new_skips) {
        Some(skips) if skips.is_empty() => None,
        Some(_) if run.holdout => Some(BTreeSet::new()),
        Some(skips) => Some(skips.iter().map(|skip| skip.path.clone()).collect()),
        None => Some(BTreeSet::new()),
    }
}

/// The tests one passing run's integrity block lists as removed against its baseline, with
/// how many in all: None when it lists none, nothing for a holdout run so its tests stay
/// unnamed and uncounted, and nothing when the block is missing or unreadable, which counts
/// as removed tests so the gate fails closed. Inputs: the block and the run. Output: the
/// names and the total.
fn removed_in(block: Option<&IntegrityRef>, run: &CheckRun) -> Option<(BTreeSet<String>, u32)> {
    let Some(inventory) = block.map(|block| &block.inventory) else {
        return Some((BTreeSet::new(), 0));
    };
    if inventory.removed_total == 0 {
        assert!(inventory.removed.is_empty());
        return None;
    }
    if run.holdout {
        return Some((BTreeSet::new(), 0));
    }
    Some((
        inventory.removed.iter().cloned().collect(),
        inventory.removed_total,
    ))
}

fn remember(
    db: &Connection,
    change_set: &ChangeSetId,
    note: &GateNote,
) -> Result<(), CheckRefusal> {
    initialize(db)?;
    let tickets = linked(db, change_set)?;
    let body = serde_json::to_string(note).map_err(unavailable)?;
    assert!(body.len() <= NOTE_BYTES_MAX);
    let written = db
        .execute(
            "INSERT INTO check_gate_notes(change_set, head, note, tickets, recorded_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(change_set, head) DO UPDATE SET note = excluded.note,
                 tickets = excluded.tickets, recorded_ms = excluded.recorded_ms",
            params![
                change_set.as_str(),
                note.head,
                body,
                serde_json::to_string(&tickets).map_err(unavailable)?,
                now_ms()
            ],
        )
        .map_err(unavailable)?;
    assert_eq!(written, 1);
    Ok(())
}

/// The tickets linked to a change set, by short id in creation order, at most
/// `TICKETS_MAX`; none in a store without the tracker's tables. Inputs: the engine
/// connection and the change set. Output: the ids, or `checks_unavailable`.
fn linked(db: &Connection, change_set: &ChangeSetId) -> Result<Vec<String>, CheckRefusal> {
    let tracked: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tk_links')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !tracked {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(TICKETS_MAX).expect("the bound fits in i64");
    let mut statement = db.prepare(LINKED).map_err(unavailable)?;
    let tickets = statement
        .query_map(params![change_set.as_str(), limit], |row| row.get(0))
        .map_err(unavailable)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(unavailable)?;
    assert!(tickets.len() <= TICKETS_MAX);
    Ok(tickets)
}

fn read(db: &Connection, sql: &str, key: &str) -> Result<Vec<GateRecord>, CheckRefusal> {
    let limit = i64::try_from(NOTES_READ_MAX).expect("the bound fits in i64");
    let mut statement = db.prepare(sql).map_err(unavailable)?;
    let rows = statement
        .query_map(params![key, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    assert!(rows.len() <= NOTES_READ_MAX);
    rows.into_iter()
        .map(|(change_set, note, tickets, recorded_ms)| {
            Ok(GateRecord {
                change_set: ChangeSetId::new(change_set).map_err(unavailable)?,
                note: serde_json::from_str(&note).map_err(unavailable)?,
                tickets: serde_json::from_str(&tickets).map_err(unavailable)?,
                recorded_ms,
            })
        })
        .collect()
}
