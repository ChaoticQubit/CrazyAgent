use crate::engine::EngineError;
use rusqlite::Connection;

/// The tables that hold running work, each with the condition that marks a row as work
/// Auto holds for: a team agent working (a live master in a turn is one), a research job
/// running, a change set being updated by its agent, and a master turn the engine still
/// has to run or is running. The master turn condition repeats the `master_outstanding`
/// index condition word for word, so SQLite answers it from that small index.
const SOURCES: [(&str, &str); 4] = [
    ("team_agents", "status='working'"),
    ("research_jobs", "status='running'"),
    ("landing_change_sets", "state='updating'"),
    (
        "master_turns",
        "phase IN ('queued','prepared','delivering','running','waiting_approval','uncertain') \
         AND phase NOT IN ('waiting_approval','uncertain')",
    ),
];

/// The most work one count reports; more than this many rows still holds, so the exact
/// number past it does not matter.
pub const WORK_MAX: u32 = 100_000;

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// Counts the running work Auto holds for (DEC-167): working team agents, running research
/// jobs, change sets being updated and master turns queued or running. A table an engine
/// has not created yet counts as no work. Input: the engine database. Output: the count,
/// at most `WORK_MAX`.
pub fn count(connection: &Connection) -> Result<u32, EngineError> {
    let mut names = connection
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name IN
             ('team_agents','research_jobs','landing_change_sets','master_turns')",
        )
        .map_err(error)?;
    let present: Vec<String> = names
        .query_map([], |row| row.get(0))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    assert!(present.len() <= SOURCES.len());
    let parts: Vec<String> = SOURCES
        .iter()
        .filter(|(table, _)| present.iter().any(|name| name == table))
        .map(|(table, running)| format!("(SELECT COUNT(*) FROM {table} WHERE {running})"))
        .collect();
    if parts.is_empty() {
        return Ok(0);
    }
    let total: i64 = connection
        .prepare(&format!("SELECT {}", parts.join(" + ")))
        .map_err(error)?
        .query_row([], |row| row.get(0))
        .map_err(error)?;
    assert!(total >= 0);
    let work = u32::try_from(total.min(i64::from(WORK_MAX))).map_err(error)?;
    assert!(work <= WORK_MAX);
    Ok(work)
}
