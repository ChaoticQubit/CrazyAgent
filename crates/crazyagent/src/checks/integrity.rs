use super::AdmittedCheck;
use super::markers::{self, Language};
use super::settings::{ChecksSettings, InventoryMode, ProtectedChange, SkipMarkers};
use crate::engine::EngineError;
use crate::landing::{self, ChangeSetId, ChangedFile, FileKind, FileStatus};
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

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
/// large, not text, unreadable by name, or past `SCAN_FILES_MAX`. It counts as a new skip,
/// so `refuse` fails closed.
pub const UNSCANNED: &str = "unscanned";
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

/// What a finished check reported, as the recorder maps it from the parsed results: every
/// result name in any outcome, how many passed or failed, whether the parser saw a build
/// failure, and the process exit code, None when it was killed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported<'a> {
    pub names: Vec<&'a str>,
    pub executed: u32,
    pub build_failed: bool,
    pub exit: Option<i32>,
}

impl Reported<'_> {
    /// Tells a build failure apart from failing tests: the parser saw one, or the check
    /// exited nonzero without reporting a single result. Output: whether the build held.
    pub fn build_ok(&self) -> bool {
        let silent_failure = self.exit.is_some_and(|code| code != 0) && self.names.is_empty();
        !(self.build_failed || silent_failure)
    }
}

/// Tests the base run reported that this run no longer does: the run compared with, at
/// most `LIST_MAX` names in order, and how many there are in all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub baseline_run: Option<i64>,
    pub removed: Vec<String>,
    pub removed_total: u32,
}

/// One skip marker the change set adds: the head path, its 1-based line and the marker,
/// or line 0 and `UNSCANNED` for a test file the engine could not read.
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
    let new_skips = match subject.settings.skip_markers {
        SkipMarkers::Off => Vec::new(),
        SkipMarkers::Flag | SkipMarkers::Refuse => bounded(
            skips(db, &project, subject.change_set, &files)?,
            SKIPS_BUDGET,
        ),
    };
    let build_ok = reported.build_ok();
    let inventory = if build_ok && !reported.names.is_empty() {
        inventory(db, subject, &view.base, reported)?
    } else {
        Inventory {
            baseline_run: None,
            removed: Vec::new(),
            removed_total: 0,
        }
    };
    let integrity = Integrity {
        executed: reported.executed,
        inventory,
        new_skips,
        protected_touched,
        holdouts_ran: subject.check.holdout.is_some(),
        build_ok,
    };
    let bytes = serde_json::to_vec(&integrity).map_err(|_| failure("checks_unavailable"))?;
    assert!(bytes.len() <= INTEGRITY_BYTES_MAX);
    assert!(integrity.inventory.removed.len() <= LIST_MAX);
    assert!(integrity.inventory.removed.len() <= integrity.inventory.removed_total as usize);
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

fn inventory(
    db: &Connection,
    subject: &Subject<'_>,
    base: &str,
    reported: &Reported<'_>,
) -> Result<Inventory, EngineError> {
    let none = Inventory {
        baseline_run: None,
        removed: Vec::new(),
        removed_total: 0,
    };
    let any_finished = match subject.settings.inventory {
        InventoryMode::Off => return Ok(none),
        InventoryMode::LastKnown => false,
        InventoryMode::RunBase => true,
    };
    let Some(baseline) = baseline_run(db, subject, base, any_finished)? else {
        return Ok(none);
    };
    let names = baseline_names(db, baseline)?;
    let current: HashSet<&str> = reported.names.iter().copied().collect();
    let removed: BTreeSet<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| !current.contains(name))
        .collect();
    let removed_total = u32::try_from(removed.len()).map_err(|_| failure("checks_unavailable"))?;
    let removed = bounded(removed.into_iter().map(shortened).collect(), REMOVED_BUDGET);
    Ok(Inventory {
        baseline_run: Some(baseline),
        removed,
        removed_total,
    })
}

fn baseline_run(
    db: &Connection,
    subject: &Subject<'_>,
    base: &str,
    any_finished: bool,
) -> Result<Option<i64>, EngineError> {
    db.query_row(
        "SELECT id FROM tk_runs
          WHERE project = ?1 AND state = 'finished' AND recorded_by = ?2
            AND (verdict = 'passed' OR (?3 AND verdict = 'failed'))
            AND CASE WHEN json_valid(source_json)
                     THEN json_extract(source_json, '$.commit') END = ?4
            AND CASE WHEN json_valid(policy_json)
                     THEN json_extract(policy_json, '$.check') END = ?5
          ORDER BY finished_ms DESC, id DESC LIMIT 1",
        params![
            subject.tracker_project,
            ENGINE_ACTOR,
            any_finished,
            base,
            subject.check.spec.id
        ],
        |row| row.get(0),
    )
    .optional()
    .map_err(|_| failure("checks_unavailable"))
}

fn baseline_names(db: &Connection, run: i64) -> Result<Vec<String>, EngineError> {
    let mut statement = db
        .prepare("SELECT name FROM tk_results WHERE run_id = ?1 ORDER BY position LIMIT ?2")
        .map_err(|_| failure("checks_unavailable"))?;
    let limit = i64::try_from(BASELINE_RESULTS_MAX).expect("the bound fits in i64");
    let names = statement
        .query_map(params![run, limit], |row| row.get::<_, String>(0))
        .map_err(|_| failure("checks_unavailable"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| failure("checks_unavailable"))?;
    assert!(names.len() <= BASELINE_RESULTS_MAX);
    Ok(names)
}

/// Lists the changed paths a protected path covers, on either side of a rename: the
/// protected path itself, anything under it, a folder or link replacing one of its
/// parents, and a `.gitattributes` file whose rules reach it, since attributes change what
/// a checkout writes. Paths compare case-folded with the characters HFS+ ignores dropped,
/// as a case-insensitive disk would. Inputs: the changed files and the protected paths.
/// Output: the touched paths in order, each cut to `TEXT_BYTES_MAX`.
fn protected(files: &[ChangedFile], entries: &[String]) -> Vec<String> {
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
                touched.insert(shortened(path));
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

fn skips(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
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
            path: shortened(&file.path),
            line: 0,
            marker: UNSCANNED.into(),
        };
        if file.kind == FileKind::Binary || scanned >= SCAN_FILES_MAX {
            found.push(unscanned);
            continue;
        }
        scanned += 1;
        match added_in(db, project, id, file, language) {
            Ok(markers) => found.extend(markers.into_iter().map(|marker| NewSkip {
                path: shortened(&file.path),
                line: marker.line,
                marker: marker.marker.into(),
            })),
            Err(_) => found.push(unscanned),
        }
    }
    assert!(scanned <= SCAN_FILES_MAX);
    found.sort();
    Ok(found)
}

fn added_in(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    file: &ChangedFile,
    language: Language,
) -> Result<Vec<markers::Marker>, EngineError> {
    let head = landing::ide::head_text(db, project, id, &file.path)?;
    let base = if file.status == FileStatus::Added {
        std::sync::Arc::from("")
    } else {
        landing::ide::base_text(db, project, id, &file.path)?
    };
    Ok(markers::added(language, &base, &head))
}

fn shortened(text: &str) -> String {
    if text.len() <= TEXT_BYTES_MAX {
        return text.to_owned();
    }
    let mut end = TEXT_BYTES_MAX - ELIDED.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = format!("{}{ELIDED}", &text[..end]);
    assert!(cut.len() <= TEXT_BYTES_MAX);
    cut
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
