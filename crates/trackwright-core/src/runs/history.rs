use super::{
    Loaded, PruneEvent, RUN_CANCEL, RUN_FINISH, RUN_PRUNE, RUN_RECORD, RUN_START, RecordEvent,
    ResultView, RunView, TestId, project_id, results_from, stored_run,
};
use crate::model::ProjectKey;
use crate::store::StoreError;
use crate::tests_model::RESULTS_PER_RUN_MAX;
use rusqlite::Transaction;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(super) const RUNS_OF_PROJECT: &str = "SELECT r.id, r.number, p.number, r.batch, r.title,
    r.state, r.verdict, r.reason, r.source_json, r.policy_json, r.integrity_json,
    r.started_ms, r.finished_ms, r.recorded_by, r.total, r.passed, r.failed, r.skipped,
    r.errored, r.version
    FROM tk_runs r LEFT JOIN tk_plans p ON p.id = r.plan_id
    WHERE r.project = ?1 ORDER BY r.number";
pub(super) const EVENTS_OF_RUNS: &str = "SELECT op, before_json, after_json FROM tk_events
    WHERE project = ?1 AND ticket IS NULL
    AND op IN ('run.start', 'run.record', 'run.finish', 'run.cancel', 'run.prune')
    ORDER BY id";

/// A run with every one of its results, as stored or as rebuilt from its events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRun {
    pub run: RunView,
    pub results: Vec<ResultView>,
}

/// Reads every run of a project with all its results from the tables, oldest first. One
/// read per run, so it is an audit and test path, not a page. Inputs: the transaction and
/// the project. Output: the runs.
pub fn stored(tx: &Transaction<'_>, key: &ProjectKey) -> Result<Vec<StoredRun>, StoreError> {
    let project_id = project_id(tx, key)?;
    let mut statement = tx.prepare(RUNS_OF_PROJECT)?;
    let loaded: Vec<Loaded> = statement
        .query_map([project_id], |row| {
            Ok((row.get::<_, i64>(0)?, stored_run(key, row)))
        })?
        .map(|row| {
            let (row_id, view) = row?;
            Ok(Loaded {
                row_id,
                project_id,
                view: view?,
            })
        })
        .collect::<Result<_, StoreError>>()?;
    let mut runs = Vec::with_capacity(loaded.len());
    for run in loaded {
        let results = results_from(tx, &run, 0, RESULTS_PER_RUN_MAX)?;
        if results.len() != run.view.totals.total as usize {
            return Err(StoreError::Corrupt(format!(
                "run {} counts {} results and holds {}",
                run.view.id,
                run.view.totals.total,
                results.len()
            )));
        }
        runs.push(StoredRun {
            run: run.view,
            results,
        });
    }
    assert!(
        runs.windows(2)
            .all(|pair| pair[0].run.id.number() < pair[1].run.id.number())
    );
    Ok(runs)
}

/// Rebuilds every run of a project from its run events alone, oldest first, so it can be
/// compared with `stored`. Inputs: the transaction and the project. Output: the runs, or
/// `Corrupt` for an event that does not follow from the ones before it.
pub fn replay(tx: &Transaction<'_>, key: &ProjectKey) -> Result<Vec<StoredRun>, StoreError> {
    let project_id = project_id(tx, key)?;
    let mut statement = tx.prepare(EVENTS_OF_RUNS)?;
    let events: Vec<(String, Option<String>, Option<String>)> = statement
        .query_map([project_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut runs: BTreeMap<u32, StoredRun> = BTreeMap::new();
    for (op, before, after) in &events {
        applied(&mut runs, key, op, before.as_deref(), after.as_deref())?;
    }
    assert!(runs.len() <= events.len());
    Ok(runs.into_values().collect())
}

fn applied(
    runs: &mut BTreeMap<u32, StoredRun>,
    key: &ProjectKey,
    op: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<(), StoreError> {
    let corrupt = |why: &str| StoreError::Corrupt(format!("a {op} event {why}"));
    match op {
        RUN_START => {
            let run: RunView = decoded(after, op)?;
            owned(&run.id, key, op)?;
            let fresh = StoredRun {
                run,
                results: Vec::new(),
            };
            if runs.insert(fresh.run.id.number(), fresh).is_some() {
                return Err(corrupt("starts a run that already started"));
            }
        }
        RUN_RECORD => {
            let written: RecordEvent = decoded(after, op)?;
            owned(&written.id, key, op)?;
            let run = runs
                .get_mut(&written.id.number())
                .ok_or_else(|| corrupt("names a run that never started"))?;
            run.run.totals = written.totals;
            run.run.version = written.version;
            run.results.extend(written.results);
        }
        RUN_FINISH | RUN_CANCEL => {
            let view: RunView = decoded(after, op)?;
            owned(&view.id, key, op)?;
            let run = runs
                .get_mut(&view.id.number())
                .ok_or_else(|| corrupt("names a run that never started"))?;
            run.run = view;
        }
        RUN_PRUNE => {
            let pruned: PruneEvent = decoded(before, op)?;
            owned(&pruned.id, key, op)?;
            runs.remove(&pruned.id.number())
                .ok_or_else(|| corrupt("prunes a run that is not there"))?;
        }
        _ => return Err(corrupt("is not a run event")),
    }
    Ok(())
}

fn owned(id: &TestId, key: &ProjectKey, op: &str) -> Result<(), StoreError> {
    if id.key() != key {
        return Err(StoreError::Corrupt(format!(
            "a {op} event names {id} in project {key}"
        )));
    }
    Ok(())
}

fn decoded<T: DeserializeOwned>(json: Option<&str>, op: &str) -> Result<T, StoreError> {
    let json = json.ok_or_else(|| StoreError::Corrupt(format!("a {op} event has no body")))?;
    serde_json::from_str(json)
        .map_err(|_| StoreError::Corrupt(format!("a {op} event body does not read back")))
}
