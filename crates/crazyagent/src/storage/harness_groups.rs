use crate::engine::EngineError;
use crate::protocol::{RequestId, SessionId};
use rusqlite::{Connection, Transaction, params};

const SCHEMA: &str = "CREATE TABLE harness_groups (
    session TEXT NOT NULL CHECK(length(session) BETWEEN 1 AND 128),
    turn TEXT NOT NULL CHECK(length(turn) BETWEEN 1 AND 128),
    process_group INTEGER CHECK(process_group > 1),
    started TEXT CHECK(length(started) BETWEEN 1 AND 64),
    UNIQUE(session,turn,process_group)
)";
const UNRECORDED: &str = "INSERT INTO harness_groups(session,turn,process_group,started)
    SELECT t.session,t.id,NULL,NULL FROM master_turns t JOIN master_sessions s ON s.id=t.session
    WHERE s.phase='configured' AND t.ordinal=(SELECT MAX(u.ordinal) FROM master_turns u
    WHERE u.session=t.session AND u.phase NOT IN ('queued','blocked','prepared'))";
const ROWS_MAX: i64 = 64;
const STARTED_MAX: usize = 64;

/// One recorded launch: its process group, unknown until the harness started, and the
/// group leader's start identity, unknown when it could not be read.
type Launch = (Option<i32>, Option<String>);

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// Creates the record of the harness process groups master turns launched (LB-23).
/// Input: the engine database. Output: success, or a schema conflict. The first creation
/// marks each open session's latest turn that may have launched as having an unknown group.
pub(super) fn initialize(db: &Connection) -> Result<(), EngineError> {
    let tx = db.unchecked_transaction().map_err(error)?;
    let actual: Option<String> = tx
        .query_row(
            "SELECT CASE WHEN length(CAST(sql AS BLOB))<=4096 THEN sql END FROM sqlite_schema
             WHERE type='table' AND name='harness_groups'",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|failure| match failure {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            failure => Err(error(failure)),
        })?;
    match actual {
        None => {
            tx.execute_batch(SCHEMA).map_err(error)?;
            tx.execute(UNRECORDED, []).map_err(error)?;
        }
        Some(sql) if sql == SCHEMA => {}
        Some(_) => return Err(error("harness_groups_schema_conflict")),
    }
    tx.commit().map_err(error)?;
    assert!(db.is_autocommit());
    Ok(())
}

/// Marks a master turn as launching a harness whose group is not known yet, before any
/// child starts, so a launch that never records its group still counts as a writer. Keeps
/// one older unknown launch per session. Inputs: the transaction, session and turn.
/// Output: success, or a capacity failure.
pub(super) fn launching(
    tx: &Transaction<'_>,
    session: &SessionId,
    turn: &RequestId,
) -> Result<(), EngineError> {
    assert!(!tx.is_autocommit());
    tx.execute(
        "DELETE FROM harness_groups WHERE session=?1 AND process_group IS NULL AND rowid<>(
         SELECT MIN(rowid) FROM harness_groups WHERE session=?1 AND process_group IS NULL)",
        [session.as_str()],
    )
    .map_err(error)?;
    tx.execute(
        "INSERT INTO harness_groups(session,turn,process_group,started) SELECT ?1,?2,NULL,NULL
         WHERE NOT EXISTS(SELECT 1 FROM harness_groups WHERE session=?1 AND turn=?2)",
        params![session.as_str(), turn.as_str()],
    )
    .map_err(error)?;
    bounded(tx, session)
}

/// Replaces a launching turn's unknown group with the process group its harness started
/// in and the leader's start identity, after forgetting this session's groups that are
/// gone. Inputs: the transaction, session, turn and group. Output: success, or a refusal
/// when the turn did not announce its launch, the group is invalid or a probe fails.
pub(super) fn record(
    tx: &Transaction<'_>,
    session: &SessionId,
    turn: &RequestId,
    group: i32,
) -> Result<(), EngineError> {
    assert!(!tx.is_autocommit());
    if group <= 1 {
        return Err(error("harness_group_invalid"));
    }
    let started = start_identity(group)?;
    settle(tx, session)?;
    let announced = tx
        .execute(
            "DELETE FROM harness_groups WHERE session=?1 AND turn=?2 AND process_group IS NULL",
            params![session.as_str(), turn.as_str()],
        )
        .map_err(error)?;
    if announced != 1 {
        return Err(error("harness_group_unannounced"));
    }
    let inserted = tx
        .execute(
            "INSERT INTO harness_groups(session,turn,process_group,started) VALUES (?1,?2,?3,?4)",
            params![session.as_str(), turn.as_str(), group, started],
        )
        .map_err(error)?;
    assert_eq!(inserted, 1);
    bounded(tx, session)
}

/// Tells whether nothing a master session's earlier turns launched can still run: no launch
/// has an unknown group and every recorded group is gone. Inputs: the transaction and the
/// session. Output: true when the session's worktree has no earlier writer.
pub(super) fn quiescent(tx: &Transaction<'_>, session: &SessionId) -> Result<bool, EngineError> {
    for (group, started) in rows(tx, session)? {
        let Some(group) = group else {
            return Ok(false);
        };
        if !gone(group, started.as_deref())? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Forgets every recorded launch of a master session once its worktree is proved free.
/// Inputs: the transaction and the session. Output: success.
pub(super) fn clear(tx: &Transaction<'_>, session: &SessionId) -> Result<(), EngineError> {
    assert!(!tx.is_autocommit());
    tx.execute(
        "DELETE FROM harness_groups WHERE session=?1",
        [session.as_str()],
    )
    .map_err(error)?;
    Ok(())
}

/// Deletes a session's recorded groups that are gone, keeping unknown launches.
/// Inputs: the transaction and the session. Output: success, or a probe failure.
fn settle(tx: &Transaction<'_>, session: &SessionId) -> Result<(), EngineError> {
    for (group, started) in rows(tx, session)? {
        let Some(group) = group else {
            continue;
        };
        if gone(group, started.as_deref())? {
            tx.execute(
                "DELETE FROM harness_groups WHERE session=?1 AND process_group=?2",
                params![session.as_str(), group],
            )
            .map_err(error)?;
        }
    }
    Ok(())
}

/// Reads a session's recorded launches within the row bound. Inputs: the transaction and
/// the session. Output: each launch's group, if known, and start identity.
fn rows(tx: &Transaction<'_>, session: &SessionId) -> Result<Vec<Launch>, EngineError> {
    let mut statement = tx
        .prepare("SELECT process_group,started FROM harness_groups WHERE session=?1 LIMIT ?2")
        .map_err(error)?;
    let found = statement
        .query_map(params![session.as_str(), ROWS_MAX + 1], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    if i64::try_from(found.len()).map_err(error)? > ROWS_MAX {
        return Err(error("harness_groups_capacity"));
    }
    Ok(found)
}

/// Refuses a session holding more launches than the bound. Inputs: the transaction and
/// the session. Output: success, or `harness_groups_capacity`.
fn bounded(tx: &Transaction<'_>, session: &SessionId) -> Result<(), EngineError> {
    let stored: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM harness_groups WHERE session=?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if stored > ROWS_MAX {
        return Err(error("harness_groups_capacity"));
    }
    Ok(())
}

/// Tells whether a recorded harness process group is gone: no process is in the group and
/// its leader is not alive with the recorded start identity. Inputs: the group and the
/// recorded start identity, if any. Output: true only when both hold.
#[cfg(unix)]
pub(super) fn gone(group: i32, started: Option<&str>) -> Result<bool, EngineError> {
    let pid = rustix::process::Pid::from_raw(group)
        .filter(|_| group > 1)
        .ok_or_else(|| error("harness_group_invalid"))?;
    match rustix::process::test_kill_process_group(pid) {
        Ok(()) | Err(rustix::io::Errno::PERM) => return Ok(false),
        Err(rustix::io::Errno::SRCH) => {}
        Err(_) => return Err(error("harness_group_probe_failed")),
    }
    Ok(match start_identity(group)? {
        None => true,
        Some(now) => started.is_some_and(|recorded| recorded != now),
    })
}

/// Refuses to call a group gone where there is no process group probe.
/// Inputs: unused. Output: `harness_group_probe_unsupported`.
#[cfg(not(unix))]
pub(super) fn gone(_group: i32, _started: Option<&str>) -> Result<bool, EngineError> {
    Err(error("harness_group_probe_unsupported"))
}

/// Reads when a process started, through `ps`, as an identity that a reused process id
/// does not share. Input: the process id. Output: the start identity, or none when no
/// such process exists.
#[cfg(target_os = "macos")]
pub(super) fn start_identity(pid: i32) -> Result<Option<String>, EngineError> {
    use std::process::{Command, Stdio};
    if pid <= 1 {
        return Err(error("harness_group_invalid"));
    }
    let output = crate::spawn_gate::output(
        Command::new("/bin/ps")
            .env_clear()
            .env("LC_ALL", "C")
            .env("TZ", "UTC0")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|_| error("harness_group_probe_failed"))?;
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| error("harness_group_probe_failed"))?
        .trim();
    match (output.status.success(), text.is_empty()) {
        (false, true) => Ok(None),
        (true, false) if text.len() <= STARTED_MAX => Ok(Some(text.to_owned())),
        _ => Err(error("harness_group_probe_failed")),
    }
}

/// Reads when a process started, in clock ticks after boot from `/proc/<pid>/stat`, as an
/// identity that a reused process id does not share. Input: the process id. Output: the
/// start identity, or none when no such process exists.
#[cfg(target_os = "linux")]
pub(super) fn start_identity(pid: i32) -> Result<Option<String>, EngineError> {
    if pid <= 1 {
        return Err(error("harness_group_invalid"));
    }
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error("harness_group_probe_failed")),
    };
    let started = stat
        .rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .filter(|field| {
            !field.is_empty()
                && field.len() <= STARTED_MAX
                && field.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(|| error("harness_group_probe_failed"))?;
    Ok(Some(started.to_owned()))
}

/// Refuses to read a start identity where neither `ps` nor `/proc` is qualified.
/// Input: unused. Output: `harness_group_probe_unsupported`.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn start_identity(_pid: i32) -> Result<Option<String>, EngineError> {
    Err(error("harness_group_probe_unsupported"))
}

#[cfg(all(test, unix))]
mod tests;
