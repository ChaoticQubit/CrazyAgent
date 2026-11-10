use super::capture::{
    self, Added, Attribution, Format, Messages, ROW_TOKENS_MAX, Tokens, all_records, clamp,
    count_usages, recount_read,
};
use super::{failure, storage};
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, ProjectId};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[cfg(test)]
mod tests;

const HOUR_MS: i64 = 60 * 60 * 1000;
/// The bytes one upkeep step reads for the recount.
pub(super) const BYTES_PER_STEP: u64 = 16 * 1024 * 1024;
const HOMES_MAX: usize = 16 * 1024;
/// What listing one home costs a step, whatever it holds.
const LISTING_COST: u64 = 4 * 1024 * 1024;
/// The least one read costs a step, so that many tiny files stay bounded.
const CHUNK_COST_MIN: u64 = 64 * 1024;
/// The most records one recount lists.
const FILES_MAX: i64 = 1 << 20;
/// The most keys one step copies into the live table.
const SEEDED_PER_STEP: i64 = 64_000;
/// How many reads of one record may fail before it is given up on.
const ATTEMPTS_MAX: i64 = 3;
/// 2024-01-01 UTC. No supported harness wrote a line before it.
const LINE_TIME_FLOOR_MS: i64 = 1_704_067_200_000;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_recount (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    state TEXT NOT NULL CHECK(state IN ('running','ready','used','kept','nothing')),
    started_ms INTEGER NOT NULL,
    decided_ms INTEGER,
    homes_partial INTEGER NOT NULL DEFAULT 0,
    seeded_to TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS usage_recount_homes (
    home TEXT PRIMARY KEY,
    format TEXT NOT NULL CHECK(format IN ('claude','codex')),
    kind TEXT NOT NULL CHECK(kind IN ('master','agent','research')),
    project TEXT NOT NULL,
    pool TEXT NOT NULL,
    account TEXT NOT NULL,
    agent TEXT NOT NULL,
    harness TEXT NOT NULL,
    listed INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS usage_recount_homes_todo ON usage_recount_homes(listed) WHERE listed=0;
CREATE TABLE IF NOT EXISTS usage_recount_files (
    path TEXT PRIMARY KEY,
    home TEXT NOT NULL,
    session TEXT NOT NULL,
    fallback_ms INTEGER NOT NULL,
    progress TEXT NOT NULL DEFAULT '',
    done INTEGER NOT NULL DEFAULT 0,
    dev INTEGER NOT NULL DEFAULT 0,
    ino INTEGER NOT NULL DEFAULT 0,
    complete INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS usage_recount_files_todo ON usage_recount_files(done) WHERE done=0;
CREATE TABLE IF NOT EXISTS usage_recount_rows (
    hour_ms INTEGER NOT NULL,
    kind TEXT NOT NULL,
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
CREATE TABLE IF NOT EXISTS usage_recount_messages (
    id TEXT PRIMARY KEY,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    web INTEGER NOT NULL DEFAULT 0,
    last_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_recount_sessions (
    project TEXT NOT NULL,
    agent TEXT NOT NULL,
    session TEXT NOT NULL,
    PRIMARY KEY (project, agent, session)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_recount_live (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL
) WITHOUT ROWID;";

const COVERED: &str = "EXISTS (SELECT 1 FROM usage_recount_sessions s
    WHERE s.project=t.project AND s.agent=t.agent AND s.session=t.session)";
const STAGED: &str = "EXISTS (SELECT 1 FROM usage_recount_sessions s
    WHERE s.project=r.project AND s.agent=r.agent AND s.session=r.session)";
const CLEAR_STAGING: &str = "DELETE FROM usage_recount_rows; DELETE FROM usage_recount_messages;
    DELETE FROM usage_recount_files; DELETE FROM usage_recount_homes;
    DELETE FROM usage_recount_sessions; DELETE FROM usage_recount_live;";

/// One agent home the recount reads, and who its tokens count against. `removed` marks a run
/// of a removed project, whose name no project id can hold. `team_agent` is the team agent
/// the home belongs to, whose tool calls fill its tool hours; a research run has none.
pub(super) struct Home {
    pub path: PathBuf,
    pub format: Format,
    pub attribution: Attribution,
    pub removed: bool,
    pub team_agent: Option<i64>,
}

/// What the owner sees once: the totals as they stand and as they would be, and how many
/// sessions each side covers. Counts only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecountStatus {
    pub old: Tokens,
    pub new: Tokens,
    pub new_cost_nanos: i64,
    pub sessions_recounted: u64,
    pub sessions_kept_old: u64,
    pub homes_partial: u64,
}

/// The stored state of the recount. Input: the store. Output: the state, None before it starts.
pub(super) fn state(db: &Connection) -> Result<Option<String>, EngineError> {
    db.query_row(
        "SELECT state FROM usage_recount WHERE singleton=1",
        [],
        |row| row.get(0),
    )
    .optional()
    .map_err(storage)
}

/// Whether the live scan must note the file each new key came from. Input: the store.
/// Output: true from the start of the read until the owner chooses.
pub(super) fn keeps_live_keys(db: &Connection) -> Result<bool, EngineError> {
    Ok(matches!(state(db)?.as_deref(), Some("running" | "ready")))
}

/// Whether the recount has been started. Input: the store. Output: the answer.
pub(super) fn started(db: &Connection) -> Result<bool, EngineError> {
    Ok(state(db)?.is_some())
}

/// Starts the one recount this store ever runs and lists its homes; a store that already
/// started one is left alone. Inputs: the store, the homes and the time. Output: done.
pub(super) fn begin(db: &Connection, homes: &[Home], now_ms: i64) -> Result<(), EngineError> {
    assert!(
        homes.len() <= HOMES_MAX,
        "the home list is bounded by its reader"
    );
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    let fresh = tx
        .execute(
            "INSERT INTO usage_recount (singleton,state,started_ms) VALUES (1,'running',?1)
             ON CONFLICT(singleton) DO NOTHING",
            [now_ms],
        )
        .map_err(storage)?;
    if fresh == 1 {
        for home in homes {
            let who = &home.attribution;
            tx.execute(
                "INSERT OR IGNORE INTO usage_recount_homes
                 (home,format,kind,project,pool,account,agent,harness) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    home.path.to_string_lossy(),
                    if home.format == Format::Claude { "claude" } else { "codex" },
                    who.kind.name(),
                    if home.removed {
                        super::REMOVED_PROJECTS
                    } else {
                        who.project.as_ref().map_or("", ProjectId::as_str)
                    },
                    who.pool.as_ref().map_or("", CredentialRef::as_str),
                    who.account.as_ref().map_or("", CredentialRef::as_str),
                    who.agent,
                    crate::harnesses::canonical(&who.harness),
                ],
            )
            .map_err(storage)?;
        }
    }
    tx.commit().map_err(storage)
}

/// Lists one home's records, up to the room left under `FILES_MAX`, with the identity each
/// has now. Inputs: the store, the home and its format. Output: done.
fn list_home(db: &Connection, home: &str, format: Format, now_ms: i64) -> Result<(), EngineError> {
    let (paths, walked_short) = all_records(Path::new(home), format);
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    let held: i64 = tx
        .query_row("SELECT COUNT(*) FROM usage_recount_files", [], |row| {
            row.get(0)
        })
        .map_err(storage)?;
    let room = usize::try_from(FILES_MAX - held).unwrap_or(0);
    let cut = walked_short || paths.len() > room;
    for path in paths.into_iter().take(room) {
        let metadata = std::fs::symlink_metadata(&path).ok();
        let (dev, ino) = metadata.as_ref().map_or((0, 0), capture::file_identity);
        let fallback = metadata
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(now_ms, |age| age.as_millis() as i64);
        tx.execute(
            "INSERT OR IGNORE INTO usage_recount_files (path,home,session,fallback_ms,dev,ino) VALUES (?1,?2,?3,?4,?5,?6)",
            params![path.to_string_lossy(), home, capture::session_name(&path), fallback, dev as i64, ino as i64],
        )
        .map_err(storage)?;
    }
    if cut {
        tx.execute(
            "UPDATE usage_recount SET homes_partial=homes_partial+1 WHERE singleton=1",
            [],
        )
        .map_err(storage)?;
    }
    tx.execute(
        "UPDATE usage_recount_homes SET listed=1 WHERE home=?1",
        [home],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)
}

/// One listed record still to read.
struct Listed {
    path: String,
    home: String,
    session: String,
    fallback_ms: i64,
    progress: String,
    dev: i64,
    ino: i64,
    attempts: i64,
}

/// The time a line files under. An agent writes its own transcript, so a time after now or
/// before the floor is not believed. Inputs: the line's time, the file's time and now.
/// Output: a time within `LINE_TIME_FLOOR_MS` and now.
fn line_time(at_ms: Option<i64>, fallback_ms: i64, now_ms: i64) -> i64 {
    at_ms
        .filter(|at| (LINE_TIME_FLOOR_MS..=now_ms).contains(at))
        .unwrap_or_else(|| fallback_ms.min(now_ms).max(LINE_TIME_FLOOR_MS))
}

/// Reads one chunk of one record into the staging tables, in one write with its cursor, so a
/// restart repeats no line. The try is counted first, so a record that panics or fails is
/// given up on after `ATTEMPTS_MAX`. Who it counts against is read inside that write, so a
/// project removed during the read files under its removed name. Inputs: the store, the record
/// and the time. Output: the budget the read costs.
fn read_chunk(db: &Connection, file: Listed, now_ms: i64) -> Result<u64, EngineError> {
    type Who = (String, String, String, String, String, String);
    let format: String = db
        .query_row(
            "SELECT format FROM usage_recount_homes WHERE home=?1",
            [&file.home],
            |row| row.get(0),
        )
        .map_err(storage)?;
    let format = if format == "claude" {
        Format::Claude
    } else {
        Format::Codex
    };
    db.execute(
        "UPDATE usage_recount_files SET attempts=attempts+1 WHERE path=?1",
        [&file.path],
    )
    .map_err(storage)?;
    let read = recount_read(
        Path::new(&file.path),
        format,
        &file.progress,
        (file.dev as u64, file.ino as u64),
        now_ms,
    );
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    let (kind, project, pool, account, agent, harness): Who = tx
        .query_row(
            "SELECT kind,project,pool,account,agent,harness FROM usage_recount_homes WHERE home=?1",
            [&file.home],
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
        .map_err(storage)?;
    for usage in &read.usages {
        let hour_of = line_time(usage.at_ms, file.fallback_ms, now_ms);
        let hour_ms = hour_of - hour_of.rem_euclid(HOUR_MS);
        for (model, added) in
            count_usages(&tx, Messages::Recount, std::slice::from_ref(usage), now_ms)?
        {
            add_row(
                &tx,
                hour_ms,
                (&kind, &project, &pool, &account, &agent, &harness),
                &file.session,
                &model,
                added,
            )?;
        }
    }
    tx.execute(
        "UPDATE usage_recount_files SET progress=?2,done=?3,complete=?4,attempts=0 WHERE path=?1",
        params![file.path, read.progress, read.done, read.complete],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(read.read.max(CHUNK_COST_MIN))
}

/// Adds one staged line's growth to its session's row for the hour it was written. Inputs: the
/// open write, the hour, who it counts against, the session, the model and the growth.
/// Output: done, or `usage_unavailable`.
fn add_row(
    tx: &Connection,
    hour_ms: i64,
    (kind, project, pool, account, agent, harness): (&str, &str, &str, &str, &str, &str),
    session: &str,
    model: &str,
    added: Added,
) -> Result<(), EngineError> {
    if added.tokens.is_empty() {
        return Ok(());
    }
    let key = (hour_ms, kind, project, pool, account, agent, harness);
    let (model, _) = capture::filed_model(tx, "usage_recount_rows", key, session, model)?;
    tx.execute(
        "INSERT INTO usage_recount_rows (hour_ms,kind,project,pool,account,agent,harness,session,model,
            input,output,cache_read,cache_write,cost_nanos)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
         ON CONFLICT(hour_ms,kind,project,pool,account,agent,harness,session,model) DO UPDATE SET
            input=MIN(input+excluded.input,?15),output=MIN(output+excluded.output,?15),
            cache_read=MIN(cache_read+excluded.cache_read,?15),
            cache_write=MIN(cache_write+excluded.cache_write,?15),
            cost_nanos=MIN(cost_nanos+excluded.cost_nanos,?15)",
        params![
            hour_ms, kind, project, pool, account, agent, harness, session, model,
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

/// Copies one page of the staged keys into the live table, so that a resumed session counts
/// nothing the read saw, even under Keep old or before the owner chooses. The larger reading
/// wins. Input: the store. Output: true when a page was copied.
fn seed_page(db: &Connection) -> Result<bool, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    let seeded_to: String = tx
        .query_row(
            "SELECT seeded_to FROM usage_recount WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(storage)?;
    let last: Option<String> = tx
        .query_row(
            "SELECT MAX(id) FROM (SELECT id FROM usage_recount_messages WHERE id>?1 ORDER BY id LIMIT ?2)",
            params![seeded_to, SEEDED_PER_STEP],
            |row| row.get(0),
        )
        .map_err(storage)?;
    let Some(last) = last else {
        return Ok(false);
    };
    if !capture::has_room(&tx, "usage_messages")? {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO usage_messages (id,input,output,cache_read,cache_write,web,last_ms)
         SELECT id,input,output,cache_read,cache_write,web,last_ms FROM usage_recount_messages WHERE id>?1 AND id<=?2
         ON CONFLICT(id) DO UPDATE SET input=MAX(input,excluded.input),output=MAX(output,excluded.output),
            cache_read=MAX(cache_read,excluded.cache_read),cache_write=MAX(cache_write,excluded.cache_write),
            web=MAX(web,excluded.web),last_ms=MAX(last_ms,excluded.last_ms)",
        params![seeded_to, last],
    )
    .map_err(storage)?;
    tx.execute(
        "UPDATE usage_recount SET seeded_to=?1 WHERE singleton=1",
        [&last],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(true)
}

/// Ends the read. A session counts as recounted only when every one of its files was read to
/// its end, and it then goes to what the read found, even zero. A recount with nothing to
/// compare is never offered. Inputs: the store and the time. Output: done.
fn finish(db: &Connection, now_ms: i64) -> Result<(), EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    tx.execute(
        "INSERT OR IGNORE INTO usage_recount_sessions (project,agent,session)
         SELECT h.project,h.agent,f.session FROM usage_recount_files f JOIN usage_recount_homes h ON h.home=f.home
         GROUP BY h.project,h.agent,f.session HAVING MIN(f.complete)=1",
        [],
    )
    .map_err(storage)?;
    let found: bool = tx
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM usage_recount_rows r WHERE {STAGED}) OR EXISTS(SELECT 1 FROM usage_tokens t WHERE {COVERED})"),
            [],
            |row| row.get(0),
        )
        .map_err(storage)?;
    tx.execute(
        "UPDATE usage_recount SET state=?1,decided_ms=?2 WHERE singleton=1 AND state='running'",
        params![
            if found { "ready" } else { "nothing" },
            (!found).then_some(now_ms)
        ],
    )
    .map_err(storage)?;
    if !found {
        tx.execute_batch(CLEAR_STAGING).map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

/// Does one bounded part of the recount. Once the staging tables are full, the files not yet
/// read keep their old numbers. A step that read a chunk ends without copying keys, so a live
/// scan runs between the last read and the first copy. Inputs: the store, the budget and the
/// time. Output: true while work remains.
pub(super) fn step(db: &Connection, byte_budget: u64, now_ms: i64) -> Result<bool, EngineError> {
    if state(db)?.as_deref() != Some("running") {
        return Ok(false);
    }
    let mut budget = byte_budget;
    let mut read_any = false;
    loop {
        let unlisted: Option<(String, String)> = db
            .query_row(
                "SELECT home,format FROM usage_recount_homes WHERE listed=0 ORDER BY rowid LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        if let Some((home, format)) = unlisted {
            list_home(
                db,
                &home,
                if format == "claude" {
                    Format::Claude
                } else {
                    Format::Codex
                },
                now_ms,
            )?;
            budget = budget.saturating_sub(LISTING_COST);
            if budget == 0 {
                return Ok(true);
            }
            continue;
        }
        if !capture::has_room(db, "usage_recount_rows")?
            || !capture::has_room(db, "usage_recount_messages")?
        {
            db.execute("UPDATE usage_recount_files SET done=1 WHERE done=0", [])
                .map_err(storage)?;
        }
        let file: Option<Listed> = db
            .query_row(
                "SELECT path,home,session,fallback_ms,progress,dev,ino,attempts FROM usage_recount_files
                 WHERE done=0 ORDER BY rowid LIMIT 1",
                [],
                |row| {
                    Ok(Listed {
                        path: row.get(0)?,
                        home: row.get(1)?,
                        session: row.get(2)?,
                        fallback_ms: row.get(3)?,
                        progress: row.get(4)?,
                        dev: row.get(5)?,
                        ino: row.get(6)?,
                        attempts: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(storage)?;
        let Some(file) = file else {
            if read_any {
                return Ok(true);
            }
            if seed_page(db)? {
                return Ok(true);
            }
            finish(db, now_ms)?;
            return Ok(false);
        };
        let cost = if file.attempts >= ATTEMPTS_MAX {
            db.execute(
                "UPDATE usage_recount_files SET done=1 WHERE path=?1",
                [&file.path],
            )
            .map_err(storage)?;
            CHUNK_COST_MIN
        } else {
            read_any = true;
            read_chunk(db, file, now_ms)?
        };
        budget = budget.saturating_sub(cost);
        if budget == 0 {
            return Ok(true);
        }
    }
}

/// Reads what the owner has not yet decided: old and new side by side. Input: the store.
/// Output: the comparison, None unless the recount is ready to show.
pub fn status(db: &Connection) -> Result<Option<RecountStatus>, EngineError> {
    if state(db)?.as_deref() != Some("ready") {
        return Ok(None);
    }
    let sum = |sql: &str| -> Result<(Tokens, i64), EngineError> {
        db.query_row(sql, [], |row| {
            let count = |index: usize| -> rusqlite::Result<u64> {
                Ok(row.get::<_, f64>(index)?.max(0.0) as u64)
            };
            Ok((
                Tokens {
                    input: count(0)?,
                    output: count(1)?,
                    cache_read: count(2)?,
                    cache_write: count(3)?,
                },
                row.get::<_, f64>(4)? as i64,
            ))
        })
        .map_err(storage)
    };
    const COLUMNS: &str =
        "TOTAL(input),TOTAL(output),TOTAL(cache_read),TOTAL(cache_write),TOTAL(cost_nanos)";
    let (old, _) = sum(&format!("SELECT {COLUMNS} FROM usage_tokens"))?;
    let (kept, kept_cost) = sum(&format!(
        "SELECT {COLUMNS} FROM usage_tokens t WHERE NOT {COVERED}"
    ))?;
    let (staged, staged_cost) = sum(&format!(
        "SELECT {COLUMNS} FROM usage_recount_rows r WHERE {STAGED}"
    ))?;
    let sessions = |sql: &str| -> Result<u64, EngineError> {
        db.query_row(sql, [], |row| row.get::<_, i64>(0))
            .map(|count| count.max(0) as u64)
            .map_err(storage)
    };
    Ok(Some(RecountStatus {
        old,
        new: kept.plus(staged),
        new_cost_nanos: kept_cost.saturating_add(staged_cost),
        sessions_recounted: sessions("SELECT COUNT(*) FROM usage_recount_sessions")?,
        sessions_kept_old: sessions(&format!(
            "SELECT COUNT(*) FROM (SELECT DISTINCT project,agent,session FROM usage_tokens t WHERE NOT {COVERED})"
        ))?,
        homes_partial: sessions("SELECT homes_partial FROM usage_recount WHERE singleton=1")?,
    }))
}

const COPY_KEPT_MS: i64 = 30 * 24 * HOUR_MS;

/// The records of the sessions the recount replaces.
const REWOUND: &str = "SELECT f.path,f.progress FROM usage_recount_files f JOIN usage_recount_homes h ON h.home=f.home
    WHERE EXISTS (SELECT 1 FROM usage_recount_sessions s
        WHERE s.project=h.project AND s.agent=h.agent AND s.session=f.session)";
const OLD_COUNTING: &str = "These numbers still use the old counting.";
const KEPT_COUNTING: &str = "Older numbers use the old counting, because you chose Keep old numbers. New work uses the new counting.";

/// The sentence the usage pages show about how their numbers were counted. Input: the store.
/// Output: the sentence, None once the numbers are the new counting's or there was nothing
/// to compare.
pub fn counting_notice(db: &Connection) -> Result<Option<String>, EngineError> {
    Ok(match state(db)?.as_deref() {
        None | Some("running" | "ready") => Some(OLD_COUNTING.to_owned()),
        Some("kept") => Some(KEPT_COUNTING.to_owned()),
        _ => None,
    })
}

/// Applies the owner's choice, once. Use new replaces the rows of each session read to its
/// end, keeps a copy of them, sets their records back to what the recount read and forgets
/// only the keys the live scan took from those records since, so a line written since is read
/// again and counts once. Each record remembers how far the live scan had filed turns and
/// tools. Keep old leaves every row. Inputs: the store, the choice and the time. Output: done,
/// or `usage_recount_not_ready`.
pub fn decide(db: &Connection, use_new: bool, now_ms: i64) -> Result<(), EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(storage)?;
    if state(&tx)?.as_deref() != Some("ready") {
        return Err(failure("usage_recount_not_ready"));
    }
    if use_new {
        tx.execute_batch(&format!(
            "DROP TABLE IF EXISTS usage_tokens_before_recount;
             CREATE TABLE usage_tokens_before_recount AS SELECT t.* FROM usage_tokens t WHERE {COVERED};
             DELETE FROM usage_tokens AS t WHERE {COVERED};
             INSERT INTO usage_tokens (hour_ms,kind,project,pool,account,agent,harness,session,model,
                input,output,cache_read,cache_write,cost_nanos)
             SELECT hour_ms,kind,project,pool,account,agent,harness,session,model,
                input,output,cache_read,cache_write,cost_nanos FROM usage_recount_rows r WHERE {STAGED}
             ON CONFLICT(hour_ms,kind,project,pool,account,agent,harness,session,model) DO UPDATE SET
                input=MIN(input+excluded.input,{max}),output=MIN(output+excluded.output,{max}),
                cache_read=MIN(cache_read+excluded.cache_read,{max}),
                cache_write=MIN(cache_write+excluded.cache_write,{max}),
                cost_nanos=MIN(cost_nanos+excluded.cost_nanos,{max});
             DELETE FROM usage_messages
                WHERE id IN (SELECT l.id FROM usage_recount_live l JOIN ({REWOUND}) w ON w.path=l.path)
                AND id NOT IN (SELECT id FROM usage_recount_messages);
             UPDATE usage_messages SET (input,output,cache_read,cache_write,web)=
                (SELECT m.input,m.output,m.cache_read,m.cache_write,m.web
                 FROM usage_recount_messages m WHERE m.id=usage_messages.id)
                WHERE id IN (SELECT l.id FROM usage_recount_live l JOIN ({REWOUND}) w ON w.path=l.path)
                AND id IN (SELECT id FROM usage_recount_messages);
             INSERT INTO usage_files (path,progress)
             SELECT w.path,json_set(w.progress,'$.filed_to',COALESCE(json_extract(u.progress,'$.offset'),0))
             FROM ({REWOUND}) w LEFT JOIN usage_files u ON u.path=w.path WHERE true
             ON CONFLICT(path) DO UPDATE SET progress=excluded.progress;",
            max = ROW_TOKENS_MAX
        ))
        .map_err(storage)?;
    }
    tx.execute_batch(CLEAR_STAGING).map_err(storage)?;
    tx.execute(
        "UPDATE usage_recount SET state=?1,decided_ms=?2 WHERE singleton=1",
        params![if use_new { "used" } else { "kept" }, now_ms],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)
}

/// Drops the copy of the replaced rows once it is past its age. Inputs: the store and the
/// time. Output: done, or `usage_unavailable`.
pub(super) fn forget_copy(db: &Connection, now_ms: i64) -> Result<(), EngineError> {
    let due: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM usage_recount WHERE state='used' AND decided_ms<=?1)",
            [now_ms - COPY_KEPT_MS],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if due {
        db.execute_batch("DROP TABLE IF EXISTS usage_tokens_before_recount")
            .map_err(storage)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use crate::usage::capture::RunKind;

    /// Runs a recount of one home under the only agent of `project` and uses it. Inputs: the
    /// store, the home, the project and the time. Only tests call this.
    pub(crate) fn recount_and_use(db: &Connection, home: &Path, project: &str, now_ms: i64) {
        let (agent, harness, kind): (String, String, String) = db
            .query_row(
                "SELECT agent,harness,kind FROM usage_tokens WHERE project=?1 LIMIT 1",
                [project],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let attribution = Attribution {
            kind: if kind == "master" {
                RunKind::Master
            } else {
                RunKind::Agent
            },
            project: Some(ProjectId::new(project).unwrap()),
            pool: None,
            account: None,
            agent,
            harness,
        };
        begin(
            db,
            &[Home {
                path: home.to_path_buf(),
                format: Format::Claude,
                attribution,
                removed: false,
                team_agent: None,
            }],
            now_ms,
        )
        .unwrap();
        while step(db, BYTES_PER_STEP, now_ms).unwrap() {}
        decide(db, true, now_ms).unwrap();
    }
}
