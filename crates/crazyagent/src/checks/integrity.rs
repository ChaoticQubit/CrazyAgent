use super::AdmittedCheck;
use super::markers::{self, Language};
use super::settings::{ChecksSettings, InventoryMode, ProtectedChange, SkipMarkers};
use crate::engine::EngineError;
use crate::landing::{self, ChangeSetId, ChangedFile, FileKind, FileStatus};
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// The most bytes one `integrity_json` block may hold, as the run binding contract bounds it.
pub const INTEGRITY_BYTES_MAX: usize = 8 * 1024;
/// The most entries each list of the block holds; totals count past it.
pub const LIST_MAX: usize = 50;
/// The most bytes one recorded name or path keeps, its tail cut on a character boundary.
pub const TEXT_BYTES_MAX: usize = 200;
/// The most changed test files one assessment reads for skip markers; past it each file
/// is recorded as unscanned.
pub const SCAN_FILES_MAX: usize = 256;
/// The most results one baseline run is read with, the contract's bound per run.
pub const BASELINE_RESULTS_MAX: usize = 20_000;
/// The marker recorded for a changed test file the engine could not read for markers: too
/// large, not text, unreadable by name, past `SCAN_FILES_MAX`, or a head the lexer cannot
/// follow: a string, regular expression or comment left open, or interpolations nested too
/// deep. It counts as a new skip, so `refuse` fails closed.
pub const UNSCANNED: &str = "unscanned";
/// The marker recorded for a test the baseline run executed that this run reports skipped
/// or not run, whatever skipped it at run time.
pub const SKIPPED_IN_RUN: &str = "skipped_in_run";
/// The only actor whose runs an inventory compares with.
pub const ENGINE_ACTOR: &str = "engine:checks";

#[cfg(test)]
#[path = "tests/integrity.rs"]
mod tests;

const REMOVED_BUDGET: usize = 2400;
const SKIPS_BUDGET: usize = 3000;
const PROTECTED_BUDGET: usize = 2400;
const ELIDED: &str = "...";

/// The run one assessment is for: the change set and the head its snapshot checked out,
/// the trackwright project the run is recorded in, the admitted check (a holdout carries
/// its files), the approved profile's protected paths and the project's settings in effect.
#[derive(Debug, Clone, Copy)]
pub struct Subject<'a> {
    pub change_set: &'a ChangeSetId,
    pub head: &'a str,
    pub tracker_project: i64,
    pub check: &'a AdmittedCheck,
    pub protected_paths: &'a [String],
    pub settings: &'a ChecksSettings,
}

/// How one test result ended, as trackwright's `tk_results.outcome` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultOutcome {
    Passed,
    Failed,
    Skipped,
    Errored,
    NotRun,
}

impl ResultOutcome {
    /// Reads a stored outcome. Input: the text. Output: the outcome, or None for any other.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "passed" => Some(Self::Passed),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            "errored" => Some(Self::Errored),
            "not_run" => Some(Self::NotRun),
            _ => None,
        }
    }

    fn ran(self) -> bool {
        matches!(self, Self::Passed | Self::Failed | Self::Errored)
    }
}

/// What a finished check reported, as the recorder maps it from the parsed results: every
/// result name with its outcome, whether the parser saw a build failure, and the process
/// exit code, None when it was killed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported<'a> {
    pub results: Vec<(&'a str, ResultOutcome)>,
    pub build_failed: bool,
    pub exit: Option<i32>,
}

impl Reported<'_> {
    /// Tells a build failure apart from failing tests: the parser saw one, or the check
    /// exited nonzero without reporting a single result. Output: whether the build held.
    pub fn build_ok(&self) -> bool {
        let silent_failure = self.exit.is_some_and(|code| code != 0) && self.results.is_empty();
        !(self.build_failed || silent_failure)
    }

    /// Counts the results that executed, passed or failed, as the verdict rules count them.
    /// Output: the count.
    pub fn executed(&self) -> u32 {
        let executed = self
            .results
            .iter()
            .filter(|(_, outcome)| matches!(outcome, ResultOutcome::Passed | ResultOutcome::Failed))
            .count();
        u32::try_from(executed).unwrap_or(u32::MAX)
    }
}

/// Tests the base run reported that this run no longer does: the run compared with, as
/// trackwright's `<KEY>-R<n>` id, at most `LIST_MAX` names in order, and how many there
/// are in all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub baseline_run: Option<String>,
    pub removed: Vec<String>,
    pub removed_total: u32,
}

/// One skip the change set brings: a marker it adds, with the head path, its 1-based line
/// and the marker; a test file the engine could not read, line 1 and `UNSCANNED`; or a
/// test the baseline run executed that this run reports skipped or not run, its name as
/// the path, line 1 and `SKIPPED_IN_RUN`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewSkip {
    pub path: String,
    pub line: u32,
    pub marker: String,
}

/// The `integrity_json` block of one run, the contract's shape: results executed, the
/// inventory, new skips, protected paths touched, whether holdouts ran and whether the
/// build held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integrity {
    pub executed: u32,
    pub inventory: Inventory,
    pub new_skips: Vec<NewSkip>,
    pub protected_touched: Vec<String>,
    pub holdouts_ran: bool,
    pub build_ok: bool,
}

/// Who reads a recorded run: the owner, or an agent or master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    Owner,
    Agent,
}

/// What a reader may see of one run: everything, or only its state and verdict, as an
/// agent sees a run that had holdout files or whose integrity block is missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "sight", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunSight {
    Full,
    OutcomeOnly {
        run: i64,
        state: String,
        verdict: Option<String>,
    },
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

/// Assesses the test integrity of one check run of a change set: the tests it no longer
/// reports against the inventory baseline, the skip markers the change adds, the
/// protected paths it touches, whether holdouts ran and whether the build held. Reads git
/// through the engine's change set store, so call it before the recording transaction.
/// Inputs: the engine store, the subject and what the check reported. Output: the block,
/// or `changes_not_found`, `checks_stale` when the change set moved past the snapshot's
/// head, or a store or git failure.
pub fn assess(
    db: &Connection,
    subject: &Subject<'_>,
    reported: &Reported<'_>,
) -> Result<Integrity, EngineError> {
    let (project, view) =
        landing::change_set(db, subject.change_set)?.ok_or_else(|| failure("changes_not_found"))?;
    if view.head != subject.head {
        return Err(failure("checks_stale"));
    }
    let files = landing::ide::files(db, &project, subject.change_set)?;
    let protected_touched = bounded(protected(&files, subject.protected_paths), PROTECTED_BUDGET);
    let build_ok = reported.build_ok();
    let compared = if build_ok && !reported.results.is_empty() {
        inventory(db, subject, &view.base, reported)?
    } else {
        Compared::none()
    };
    let new_skips = match subject.settings.skip_markers {
        SkipMarkers::Off => Vec::new(),
        SkipMarkers::Flag | SkipMarkers::Refuse => {
            let mut found = skips(db, &project, subject.change_set, subject.head, &files)?;
            found.extend(compared.skipped);
            found.sort();
            bounded(found, SKIPS_BUDGET)
        }
    };
    let integrity = Integrity {
        executed: reported.executed(),
        inventory: compared.inventory,
        new_skips,
        protected_touched,
        holdouts_ran: subject.check.holdout.is_some(),
        build_ok,
    };
    let bytes = serde_json::to_vec(&integrity).map_err(|_| failure("checks_unavailable"))?;
    assert!(bytes.len() <= INTEGRITY_BYTES_MAX);
    assert!(integrity.inventory.removed.len() <= LIST_MAX);
    assert!(integrity.inventory.removed.len() <= integrity.inventory.removed_total as usize);
    assert!(integrity.new_skips.iter().all(|skip| skip.line >= 1));
    Ok(integrity)
}

/// The gate refusal an integrity block carries under the settings in effect: a touched
/// protected path under `protected_change refuse`, or an added skip under `skip_markers
/// refuse`. Inputs: the block and the settings. Output: the refusal code, or None.
pub fn refusal(integrity: &Integrity, settings: &ChecksSettings) -> Option<&'static str> {
    if settings.protected_change == ProtectedChange::Refuse
        && !integrity.protected_touched.is_empty()
    {
        return Some("checks_protected_change");
    }
    if settings.skip_markers == SkipMarkers::Refuse && !integrity.new_skips.is_empty() {
        return Some("checks_new_skips");
    }
    None
}

/// Decides what a reader may see of one recorded run. The owner sees every run in full;
/// an agent sees a run that had holdout files, or whose integrity block is missing or
/// unreadable, as its state and verdict only. Inputs: the store, the run row id and the
/// reader. Output: the sight, or `checks_run_not_found`.
pub fn sight(db: &Connection, run: i64, reader: Reader) -> Result<RunSight, EngineError> {
    let row: Option<(String, Option<String>, Option<String>)> = db
        .query_row(
            "SELECT state, verdict, integrity_json FROM tk_runs WHERE id = ?1",
            params![run],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| failure("checks_unavailable"))?;
    let (state, verdict, block) = row.ok_or_else(|| failure("checks_run_not_found"))?;
    if reader == Reader::Owner {
        return Ok(RunSight::Full);
    }
    let holdout = block
        .as_deref()
        .and_then(|text| serde_json::from_str::<Integrity>(text).ok())
        .is_none_or(|integrity| integrity.holdouts_ran);
    if holdout {
        return Ok(RunSight::OutcomeOnly {
            run,
            state,
            verdict,
        });
    }
    Ok(RunSight::Full)
}

/// What comparing a run with its baseline found: the inventory, and the tests the
/// baseline executed that this run skipped.
struct Compared {
    inventory: Inventory,
    skipped: Vec<NewSkip>,
}

impl Compared {
    fn none() -> Self {
        Self {
            inventory: Inventory {
                baseline_run: None,
                removed: Vec::new(),
                removed_total: 0,
            },
            skipped: Vec::new(),
        }
    }
}

fn inventory(
    db: &Connection,
    subject: &Subject<'_>,
    base: &str,
    reported: &Reported<'_>,
) -> Result<Compared, EngineError> {
    let any_finished = match subject.settings.inventory {
        InventoryMode::Off => return Ok(Compared::none()),
        InventoryMode::LastKnown => false,
        InventoryMode::RunBase => true,
    };
    let Some((baseline, short)) = baseline_run(db, subject, base, any_finished)? else {
        return Ok(Compared::none());
    };
    let results = baseline_results(db, baseline)?;
    let current: HashMap<&str, ResultOutcome> = reported.results.iter().copied().collect();
    let mut removed = BTreeSet::new();
    let mut skipped = Vec::new();
    for (name, before) in &results {
        match current.get(name.as_str()) {
            None => {
                removed.insert(name.as_str());
            }
            Some(ResultOutcome::Skipped | ResultOutcome::NotRun)
                if before.is_some_and(ResultOutcome::ran) =>
            {
                skipped.push(NewSkip {
                    path: recorded(name),
                    line: 1,
                    marker: SKIPPED_IN_RUN.into(),
                });
            }
            Some(_) => {}
        }
    }
    let removed_total = u32::try_from(removed.len()).map_err(|_| failure("checks_unavailable"))?;
    let removed = bounded(removed.into_iter().map(recorded).collect(), REMOVED_BUDGET);
    Ok(Compared {
        inventory: Inventory {
            baseline_run: Some(short),
            removed,
            removed_total,
        },
        skipped,
    })
}

fn baseline_run(
    db: &Connection,
    subject: &Subject<'_>,
    base: &str,
    any_finished: bool,
) -> Result<Option<(i64, String)>, EngineError> {
    db.query_row(
        "SELECT r.id, p.key || '-R' || r.number FROM tk_runs r
           JOIN tk_projects p ON p.id = r.project
          WHERE r.project = ?1 AND r.state = 'finished' AND r.recorded_by = ?2
            AND (r.verdict = 'passed' OR (?3 AND r.verdict = 'failed'))
            AND CASE WHEN json_valid(r.source_json)
                     THEN json_extract(r.source_json, '$.commit') END = ?4
            AND CASE WHEN json_valid(r.policy_json)
                     THEN json_extract(r.policy_json, '$.check') END = ?5
          ORDER BY r.finished_ms DESC, r.id DESC LIMIT 1",
        params![
            subject.tracker_project,
            ENGINE_ACTOR,
            any_finished,
            base,
            subject.check.spec.id
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|_| failure("checks_unavailable"))
}

fn baseline_results(
    db: &Connection,
    run: i64,
) -> Result<Vec<(String, Option<ResultOutcome>)>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT name, outcome FROM tk_results WHERE run_id = ?1 ORDER BY position LIMIT ?2",
        )
        .map_err(|_| failure("checks_unavailable"))?;
    let limit = i64::try_from(BASELINE_RESULTS_MAX).expect("the bound fits in i64");
    let results = statement
        .query_map(params![run, limit], |row| {
            let outcome: String = row.get(1)?;
            Ok((row.get::<_, String>(0)?, ResultOutcome::parse(&outcome)))
        })
        .map_err(|_| failure("checks_unavailable"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| failure("checks_unavailable"))?;
    assert!(results.len() <= BASELINE_RESULTS_MAX);
    Ok(results)
}

/// Lists the changed paths a protected path covers, on either side of a rename: the
/// protected path itself, anything under it, a folder or link replacing one of its
/// parents, and a `.gitattributes` file whose rules reach it, since attributes change what
/// a checkout writes. Paths compare case-folded with the characters HFS+ ignores dropped,
/// as a case-insensitive disk would. Inputs: the changed files and the protected paths.
/// Output: the touched paths in order, each cut to `TEXT_BYTES_MAX`.
pub(crate) fn protected(files: &[ChangedFile], entries: &[String]) -> Vec<String> {
    let entries: Vec<String> = entries
        .iter()
        .map(|entry| format!("{}/", folded(entry.trim_end_matches('/'))))
        .collect();
    let mut touched = BTreeSet::new();
    for file in files {
        for path in std::iter::once(&file.path).chain(file.old_path.as_ref()) {
            let path_folded = folded(path);
            let within = format!("{path_folded}/");
            let attributes = path_folded
                .strip_suffix(".gitattributes")
                .filter(|folder| folder.is_empty() || folder.ends_with('/'));
            let covered = entries.iter().any(|entry| {
                within.starts_with(entry.as_str())
                    || entry.starts_with(within.as_str())
                    || attributes.is_some_and(|folder| entry.starts_with(folder))
            });
            if covered {
                touched.insert(recorded(path));
            }
        }
    }
    touched.into_iter().collect()
}

fn folded(path: &str) -> String {
    path.chars()
        .filter(|c| {
            !matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
        })
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn skips(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
    files: &[ChangedFile],
) -> Result<Vec<NewSkip>, EngineError> {
    let mut found = Vec::new();
    let mut scanned = 0usize;
    for file in files {
        if file.status == FileStatus::Deleted
            || matches!(file.kind, FileKind::Symlink | FileKind::Gitlink)
        {
            continue;
        }
        let Some(language) = Language::of_path(&file.path) else {
            continue;
        };
        let unscanned = NewSkip {
            path: recorded(&file.path),
            line: 1,
            marker: UNSCANNED.into(),
        };
        if file.kind == FileKind::Binary || scanned >= SCAN_FILES_MAX {
            found.push(unscanned);
            continue;
        }
        scanned += 1;
        match added_in(db, project, id, head, file, language) {
            Some(markers) => found.extend(markers.into_iter().map(|marker| NewSkip {
                path: recorded(&file.path),
                line: marker.line,
                marker: marker.marker.into(),
            })),
            None => found.push(unscanned),
        }
    }
    assert!(scanned <= SCAN_FILES_MAX);
    Ok(found)
}

/// Reads one changed test file at base and head and lists the markers its change adds.
/// Inputs: the store, the project, the change set, its checked head, the file and its
/// language. Output: the markers, or None when either side cannot be read or the head
/// leaves a string or comment open, so the file counts as unscanned.
fn added_in(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
    file: &ChangedFile,
    language: Language,
) -> Option<Vec<markers::Marker>> {
    let text = landing::ide::head_text(db, project, id, head, &file.path).ok()?;
    let base = if file.status == FileStatus::Added {
        std::sync::Arc::from("")
    } else {
        landing::ide::base_text(db, project, id, head, &file.path).ok()?
    };
    markers::added(language, &base, &text)
}

/// Makes a name or path fit a trackwright run shape: every control character, line break
/// and character a reader cannot see or that reorders text becomes U+FFFD, and a text past
/// `TEXT_BYTES_MAX` keeps its head cut on a character boundary with `...`. Input: the text.
/// Output: the recorded text.
pub(crate) fn recorded(text: &str) -> String {
    let plain: String = text
        .chars()
        .map(|c| if hidden(c) { '\u{FFFD}' } else { c })
        .collect();
    if plain.len() <= TEXT_BYTES_MAX {
        return plain;
    }
    let mut end = TEXT_BYTES_MAX - ELIDED.len();
    while !plain.is_char_boundary(end) {
        end -= 1;
    }
    let cut = format!("{}{ELIDED}", &plain[..end]);
    assert!(cut.len() <= TEXT_BYTES_MAX);
    cut
}

fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{061c}'
                | '\u{115f}'
                | '\u{1160}'
                | '\u{17b4}'
                | '\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e0fff}'
        )
}

fn bounded<T: Serialize>(items: Vec<T>, budget: usize) -> Vec<T> {
    let mut used = 0usize;
    let mut kept = Vec::new();
    for item in items.into_iter().take(LIST_MAX) {
        let bytes = serde_json::to_vec(&item).map_or(budget, |bytes| bytes.len() + 1);
        if !kept.is_empty() && used + bytes > budget {
            break;
        }
        used += bytes;
        kept.push(item);
    }
    assert!(kept.len() <= LIST_MAX);
    kept
}
