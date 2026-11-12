use super::*;
use crate::team::history::{self, Fresh, Mark};
use rusqlite::TransactionBehavior;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub const CHUNKS_PER_ROUND: usize = 256;
pub const BYTES_PER_ROUND: u64 = 4 * 1024 * 1024;
pub const GONE_CHECKS_PER_ROUND: usize = 64;
pub const MARKS_MAX: usize = 20_000;
pub const MEMORIES_MAX: usize = 4096;
pub const PROJECTS_MAX: usize = 1024;
const TICK_EVERY: Duration = Duration::from_secs(5);
const BEHIND_EVERY: Duration = Duration::from_millis(250);
const WAIT_STEP: Duration = Duration::from_millis(100);
const ROUND_PANICKED: &str = "search_index_round_panicked";

const _: () = assert!(BYTES_PER_ROUND > 2 * history::LINE_BYTES_MAX as u64);
const _: () = assert!(CHUNKS_PER_ROUND >= 2);

/// What one round read of a project, kept until one write transaction stores it: each
/// conversation read with what it found, the keys found gone, the keys checked and kept,
/// and whether every listed conversation was read to its end.
pub(crate) struct Round {
    project: ProjectId,
    reads: Vec<(String, Fresh)>,
    gone: Vec<String>,
    kept: Vec<String>,
    caught_up: bool,
}

/// Indexes one project's new messages within the engine's bounds. Inputs: the database, the
/// agents folder, the project and the time. Output: whether the project is caught up.
pub(crate) fn index_project(
    db: &mut Connection,
    agents: &Path,
    project: &ProjectId,
    now_ms: i64,
) -> Result<bool, EngineError> {
    index_within(
        db,
        agents,
        project,
        now_ms,
        CHUNKS_PER_ROUND,
        BYTES_PER_ROUND,
    )
}

/// Indexes one project's new messages within given bounds: reads with no transaction
/// open, then stores in one. Inputs: the database, the agents folder, the project, the
/// time and the most chunks and bytes. Output: whether the project is caught up.
pub(crate) fn index_within(
    db: &mut Connection,
    agents: &Path,
    project: &ProjectId,
    now_ms: i64,
    chunks_max: usize,
    bytes_max: u64,
) -> Result<bool, EngineError> {
    let round = read_round(db, agents, project, now_ms, chunks_max, bytes_max)?;
    store_round(db, round, now_ms)
}

/// Reads one project's new messages within bounds, storing nothing: every listed
/// conversation newest first from its watermark, then up to `GONE_CHECKS_PER_ROUND`
/// indexed conversations the listing no longer shows, longest unchecked first. A
/// conversation that fails to read is left for the next round. Inputs: the database, the
/// agents folder, the project, the time and the bounds. Output: the round.
pub(crate) fn read_round(
    db: &Connection,
    agents: &Path,
    project: &ProjectId,
    now_ms: i64,
    chunks_max: usize,
    bytes_max: u64,
) -> Result<Round, EngineError> {
    assert!(chunks_max >= 2);
    assert!(bytes_max > history::LINE_BYTES_MAX as u64 + 1);
    let marks = marks(db, project)?;
    let listed = history::conversations(db, project, agents);
    let mut round = Round {
        project: project.clone(),
        reads: Vec::new(),
        gone: Vec::new(),
        kept: Vec::new(),
        caught_up: true,
    };
    let (mut chunks_left, mut bytes_left) = (chunks_max, bytes_max);
    for conversation in &listed {
        if chunks_left < 2 || bytes_left <= history::LINE_BYTES_MAX as u64 + 1 {
            round.caught_up = false;
            break;
        }
        let from = marks
            .get(conversation.id.as_str())
            .map(|(mark, _)| mark.clone())
            .unwrap_or_default();
        let Ok(fresh) = history::fresh(
            db,
            project,
            conversation,
            &from,
            now_ms,
            chunks_left,
            bytes_left,
        ) else {
            round.caught_up = false;
            continue;
        };
        chunks_left -= fresh.messages.len();
        bytes_left -= fresh.bytes;
        round.caught_up &= fresh.done;
        if fresh.restarted || fresh.mark != from {
            round.reads.push((conversation.id.clone(), fresh));
        }
    }
    let shown: HashSet<&str> = listed
        .iter()
        .map(|conversation| conversation.id.as_str())
        .collect();
    let mut unlisted: Vec<(&String, &(Mark, i64))> = marks
        .iter()
        .filter(|(key, _)| !shown.contains(key.as_str()))
        .collect();
    unlisted.sort_by_key(|(key, (_, checked_ms))| (*checked_ms, key.as_str()));
    for (key, (mark, _)) in unlisted.into_iter().take(GONE_CHECKS_PER_ROUND) {
        match history::still_there(db, project, agents, key, mark.place.as_deref()) {
            Ok(true) => round.kept.push(key.clone()),
            Ok(false) => round.gone.push(key.clone()),
            Err(_) => {}
        }
    }
    assert!(round.gone.len() + round.kept.len() <= GONE_CHECKS_PER_ROUND);
    Ok(round)
}

/// Stores a round inside one IMMEDIATE write transaction that first finds its project, and
/// for engine turns their session, still there, so a removal or delete that commits while
/// the round read leaves no chunk, watermark or pass mark behind. Inputs: the database,
/// the round and the time. Output: whether the project is caught up.
pub(crate) fn store_round(
    db: &mut Connection,
    round: Round,
    now_ms: i64,
) -> Result<bool, EngineError> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let project = &round.project;
    if !project_kept(&tx, project)? {
        return Ok(true);
    }
    for (key, fresh) in &round.reads {
        if let Some(session) = key.strip_prefix(history::SESSION_PREFIX)
            && !history::session_kept(&tx, project, session).map_err(storage)?
        {
            continue;
        }
        if fresh.restarted {
            forget_key(&tx, project, key)?;
        }
        let chunks: Vec<(i64, &str)> = fresh
            .messages
            .iter()
            .map(|(ordinal, text)| (*ordinal, text.as_str()))
            .collect();
        append(&tx, Some(project), Source::Session, key, &chunks, now_ms)?;
        save_mark(&tx, project, key, &fresh.mark, now_ms)?;
    }
    for key in &round.gone {
        forget_key(&tx, project, key)?;
    }
    for key in &round.kept {
        tx.execute(
            "UPDATE search_sources SET checked_ms=?3 WHERE project=?1 AND key=?2",
            params![project.as_str(), key, now_ms],
        )
        .map_err(storage)?;
    }
    if round.caught_up {
        tx.execute(
            "INSERT INTO search_projects(project,caught_up_ms) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET caught_up_ms=COALESCE(search_projects.caught_up_ms, excluded.caught_up_ms)",
            params![project.as_str(), now_ms],
        )
        .map_err(storage)?;
    }
    tx.commit().map_err(storage)?;
    Ok(round.caught_up)
}

/// Removes the index rows of projects that are gone, their memories' chunks included, for
/// a removal that never ran the forget. Input: the database. Output: done, or the failure.
pub(crate) fn sweep_projects(db: &Connection) -> Result<(), EngineError> {
    for table in ["search_chunks", "search_sources", "search_projects"] {
        db.execute(
            &format!(
                "DELETE FROM {table} WHERE project IS NOT NULL AND project NOT IN (SELECT id FROM projects)"
            ),
            [],
        )
        .map_err(storage)?;
    }
    Ok(())
}

/// Brings the memory chunks in line with the memories agents may read, in one write
/// transaction: none waiting for the owner, none with hidden characters, each under its
/// current project or app-wide; a chunk whose memory left, changed project or is no longer
/// readable goes. Then records the library's print, so recall reads the index only while
/// nothing changed since. Inputs: the database and the time. Output: done, or the failure.
pub(crate) fn sync_memories(db: &mut Connection, now_ms: i64) -> Result<(), EngineError> {
    if !full_text(db) || !table(db, "memory_library")? {
        return Ok(());
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let Some(print) = memory_print(&tx)? else {
        return Ok(());
    };
    let readable = readable_memories(&tx)?;
    let indexed: Vec<(String, Option<String>)> = {
        let mut statement = tx
            .prepare("SELECT key, project FROM search_chunks WHERE source='memory'")
            .map_err(storage)?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage)?;
        rows.collect::<Result<_, _>>().map_err(storage)?
    };
    let mut held = HashSet::new();
    for (key, project) in indexed {
        if readable
            .get(&key)
            .is_some_and(|(scope, _)| *scope == project)
        {
            held.insert(key);
        } else {
            tx.execute(
                "DELETE FROM search_chunks WHERE source='memory' AND key=?1",
                [key.as_str()],
            )
            .map_err(storage)?;
        }
    }
    for (id, (scope, text)) in &readable {
        if held.contains(id) {
            continue;
        }
        let project = match scope.as_deref().map(ProjectId::new) {
            Some(Ok(project)) => Some(project),
            Some(Err(_)) => continue,
            None => None,
        };
        append(
            &tx,
            project.as_ref(),
            Source::Memory,
            id,
            &[(0, text.as_str())],
            now_ms,
        )?;
    }
    tx.execute(
        "INSERT INTO search_memory(singleton,print,synced_ms) VALUES (1,?1,?2)
         ON CONFLICT(singleton) DO UPDATE SET print=excluded.print, synced_ms=excluded.synced_ms",
        params![print, now_ms],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)
}

/// Reads the memories an agent may read, keyed by id: not waiting for the owner, no
/// hidden characters, with an id that fits a chunk key; at most `MEMORIES_MAX`, oldest
/// first. Input: the transaction. Output: each one's project (None for app-wide) and text.
fn readable_memories(
    tx: &Transaction<'_>,
) -> Result<HashMap<String, (Option<String>, String)>, EngineError> {
    let mut statement = tx
        .prepare(&format!(
            "SELECT id, project, origin, text FROM memory_library ORDER BY ordinal LIMIT {MEMORIES_MAX}"
        ))
        .map_err(storage)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(storage)?;
    let mut readable = HashMap::new();
    for row in rows {
        let (id, project, origin, text) = row.map_err(storage)?;
        let approved = serde_json::from_str::<crate::memory::library::MemoryOrigin>(&origin)
            .is_ok_and(|origin| !origin.waiting());
        if !approved
            || crate::memory::hidden_text(&text)
            || id.is_empty()
            || id.len() > KEY_BYTES_MAX
        {
            continue;
        }
        readable.insert(id, (project, text));
    }
    assert!(readable.len() <= MEMORIES_MAX);
    Ok(readable)
}

/// Runs one engine tick: sweeps gone projects, syncs the memories, then one round for each
/// project followed by the plugin hook that embeds its new chunks, outside any transaction.
/// A project whose round fails leaves the others to run. Inputs: the database, the engine
/// root, the agents folder and the time. Output: whether a project moved but is still
/// behind, so the next tick comes sooner, or the first failure.
pub(crate) fn tick(
    db: &mut Connection,
    root: &Path,
    agents: &Path,
    now_ms: i64,
) -> Result<bool, EngineError> {
    if !full_text(db) {
        return Ok(false);
    }
    sweep_projects(db)?;
    sync_memories(db, now_ms)?;
    let mut behind = false;
    let mut failed = None;
    for project in projects(db)? {
        match read_round(
            db,
            agents,
            &project,
            now_ms,
            CHUNKS_PER_ROUND,
            BYTES_PER_ROUND,
        ) {
            Ok(round) => {
                let moved = !round.reads.is_empty();
                match store_round(db, round, now_ms) {
                    Ok(caught_up) => behind |= moved && !caught_up,
                    Err(problem) => failed = failed.or(Some(problem)),
                }
            }
            Err(problem) => failed = failed.or(Some(problem)),
        }
        crate::semantic::embed_pending(db, root, &project);
    }
    failed.map_or(Ok(behind), Err)
}

/// Lists the engine's projects, at most `PROJECTS_MAX`, skipping an id that is not valid.
fn projects(db: &Connection) -> Result<Vec<ProjectId>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT id FROM projects ORDER BY id LIMIT {PROJECTS_MAX}"
        ))
        .map_err(storage)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(storage)?;
    let mut found = Vec::new();
    for row in rows {
        if let Ok(project) = ProjectId::new(row.map_err(storage)?) {
            found.push(project);
        }
    }
    assert!(found.len() <= PROJECTS_MAX);
    Ok(found)
}

fn project_kept(tx: &Transaction<'_>, project: &ProjectId) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project.as_str()],
        |row| row.get(0),
    )
    .map_err(storage)
}

/// Reads a project's watermarks by key, at most `MARKS_MAX`, each with when it was last
/// checked. Inputs: the database and the project. Output: the marks.
fn marks(
    db: &Connection,
    project: &ProjectId,
) -> Result<HashMap<String, (Mark, i64)>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT key, position, seen_ms, next_ordinal, inode, place, skipping, checked_ms
             FROM search_sources WHERE project=?1 LIMIT {MARKS_MAX}"
        ))
        .map_err(storage)?;
    let rows = statement
        .query_map([project.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    Mark {
                        position: row.get(1)?,
                        seen_ms: row.get(2)?,
                        ordinal: row.get(3)?,
                        inode: row.get(4)?,
                        place: row.get(5)?,
                        skipping: row.get(6)?,
                    },
                    row.get::<_, i64>(7)?,
                ),
            ))
        })
        .map_err(storage)?;
    rows.collect::<Result<_, _>>().map_err(storage)
}

fn save_mark(
    tx: &Transaction<'_>,
    project: &ProjectId,
    key: &str,
    mark: &Mark,
    now_ms: i64,
) -> Result<(), EngineError> {
    assert!(mark.position >= 0 && mark.ordinal >= 0);
    assert!(!key.is_empty() && key.len() <= KEY_BYTES_MAX);
    tx.execute(
        "INSERT INTO search_sources(project,key,position,seen_ms,next_ordinal,inode,place,skipping,checked_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(project,key) DO UPDATE SET position=excluded.position, seen_ms=excluded.seen_ms,
             next_ordinal=excluded.next_ordinal, inode=excluded.inode, place=excluded.place,
             skipping=excluded.skipping, checked_ms=excluded.checked_ms",
        params![
            project.as_str(),
            key,
            mark.position,
            mark.seen_ms,
            mark.ordinal,
            mark.inode,
            mark.place,
            mark.skipping,
            now_ms
        ],
    )
    .map_err(storage)?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The running search indexer thread of an engine.
pub struct SearchIndexer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SearchIndexer {
    /// Stops the thread and waits for its current tick. Output: none.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the search indexer of a running engine (DEC-105): one thread with its own
/// connection ticking every `TICK_EVERY`, sooner while a project is behind; a panic ends
/// only that tick, and a failure is logged by its code once until it changes. Input: the
/// runtime folder. Output: the indexer, or the reason it could not start.
pub fn start_for_engine(runtime: &Path) -> Result<SearchIndexer, String> {
    let root: PathBuf = runtime
        .canonicalize()
        .map_err(|problem| format!("search index runtime: {problem}"))?;
    let mut db = Connection::open(root.join("engine.db"))
        .map_err(|problem| format!("search index database: {problem}"))?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|problem| format!("search index database: {problem}"))?;
    initialize(&db).map_err(|problem| problem.0)?;
    let agents = root.join("team").join("agents");
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("search-index".into())
        .spawn(move || {
            let mut last: Option<String> = None;
            while !flag.load(Ordering::SeqCst) {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tick(&mut db, &root, &agents, now_ms())
                }));
                let (behind, problem) = match outcome {
                    Ok(Ok(behind)) => (behind, None),
                    Ok(Err(problem)) => (false, Some(problem.0)),
                    Err(_) => (false, Some(ROUND_PANICKED.to_owned())),
                };
                if let Some(code) = problem.as_deref().filter(|_| problem != last) {
                    eprintln!("search index tick failed ({code})");
                }
                last = problem;
                let pause = if behind { BEHIND_EVERY } else { TICK_EVERY };
                let mut waited = Duration::ZERO;
                while waited < pause && !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(WAIT_STEP);
                    waited += WAIT_STEP;
                }
            }
        })
        .map_err(|problem| format!("search index thread: {problem}"))?;
    Ok(SearchIndexer {
        stop,
        thread: Some(thread),
    })
}
