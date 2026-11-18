use super::{Event, TABLES, TOUCHED, error, exists, number, project_id, signed, valid};
use crate::engine::EngineError;
use crate::master_session::MasterTurnPhase;
use crate::owner_texts::TextDigest;
use crate::protocol::{CredentialRef, ProjectId, RequestId, SessionId};
use rusqlite::types::{Value as Cell, ValueRef};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The most session or profile ids one dropped event lists, each at most 128 bytes quoted.
pub(super) const NAMES_MAX: usize = 256;
/// The most turns one session holds, as the `master_turns` ordinal check bounds them.
pub(super) const ORDINAL_MAX: u64 = 1_024;
/// The four tables after L13's three, each with the columns its snapshot row and its saved
/// event write, body last; the read of its rows whose session is stored, as those columns,
/// then the session's project and the holder key; and the key column a mark names.
const SHAPES: [(&str, &str, &str, &str); 4] = [
    (
        "master_turns",
        "session,ordinal,id,input,phase,body",
        "SELECT t.session,t.ordinal,t.id,t.input,t.phase,t.body,s.project,t.input FROM master_turns t JOIN master_sessions s ON s.id=t.session",
        "t.input",
    ),
    (
        "master_preparations",
        "session,turn,body",
        "SELECT p.session,p.turn,p.body,s.project,t.input FROM master_preparations p JOIN master_turns t ON t.session=p.session AND t.id=p.turn JOIN master_sessions s ON s.id=p.session",
        "t.input",
    ),
    (
        "master_waiting_resolves",
        "session,project,operation,input,body",
        "SELECT r.session,r.project,r.operation,r.input,r.body,r.project,r.session FROM master_waiting_resolves r JOIN master_sessions s ON s.id=r.session",
        "r.session",
    ),
    (
        "configured_master_profiles",
        "id,session,profile,body",
        "SELECT c.id,c.session,c.profile,c.body,s.project,c.id FROM configured_master_profiles c JOIN master_sessions s ON s.id=c.session",
        "c.id",
    ),
];

const _: () =
    assert!(NAMES_MAX * 131 + super::CHUNK_ENVELOPE_BYTES <= crate::state_log::BODY_BYTES_MAX);

/// One master turn row: its session's project and the turn's keys, its phase and the digest
/// its body is kept under (`owner_texts`), so no turn text rides in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnSaved {
    pub(crate) project: ProjectId,
    pub(crate) session: SessionId,
    pub(crate) ordinal: u64,
    pub(crate) id: RequestId,
    pub(crate) input: u64,
    pub(crate) phase: MasterTurnPhase,
    pub(crate) body: TextDigest,
}

/// One turn's preparation: its session's project, the turn's session, id and input, and the
/// digest its body is kept under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationSaved {
    pub(crate) project: ProjectId,
    pub(crate) session: SessionId,
    pub(crate) turn: RequestId,
    pub(crate) input: u64,
    pub(crate) body: TextDigest,
}

/// One session's waiting Resolve: its project, session, operation and input, and the digest
/// its request body is kept under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveSaved {
    pub(crate) project: ProjectId,
    pub(crate) session: SessionId,
    pub(crate) operation: RequestId,
    pub(crate) input: u64,
    pub(crate) body: TextDigest,
}

/// Waiting Resolves of one project gone from their table, by session, at most
/// [`NAMES_MAX`] in one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvesDropped {
    pub(crate) project: ProjectId,
    pub(crate) sessions: Vec<SessionId>,
}

/// One configured profile receipt: its session's project, its id, session and profile, and
/// the digest its record is kept under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSaved {
    pub(crate) project: ProjectId,
    pub(crate) session: SessionId,
    pub(crate) id: RequestId,
    pub(crate) profile: CredentialRef,
    pub(crate) body: TextDigest,
}

/// Configured profile receipts of one project gone from their table, by id, at most
/// [`NAMES_MAX`] in one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilesDropped {
    pub(crate) project: ProjectId,
    pub(crate) ids: Vec<RequestId>,
}

/// One row of the four tables as read for a close or a snapshot run: its row columns but
/// the body, its holder key, its session's project and the digest its body is kept under.
struct Kept {
    columns: Vec<Value>,
    key: String,
    project: ProjectId,
    body: TextDigest,
}

/// What a close found gone from the waiting Resolves and profile receipts, per project,
/// and the receipts it found saved, which it logs after every drop.
#[derive(Default)]
pub(super) struct Rest {
    resolves: BTreeMap<String, Vec<SessionId>>,
    profiles: BTreeMap<String, Vec<RequestId>>,
    receipts: Vec<Event>,
}

impl Rest {
    /// Reads one marked key of the four tables back: a present row as its event, its body
    /// kept again, a receipt held back for [`Rest::finish`]; an absent row's holder let go,
    /// a Resolve or receipt gathered as dropped. Inputs: the transaction, the table, the key
    /// and the mark's project. Output: the event to log now, if any, or the refusal.
    pub(super) fn settle(
        &mut self,
        db: &Connection,
        table: &str,
        key: &str,
        project: &str,
    ) -> Result<Option<Event>, EngineError> {
        let cell = if table == TABLES[3] || table == TABLES[4] {
            Cell::Integer(signed(number(key)?)?)
        } else {
            Cell::Text(key.to_owned())
        };
        let mut found = read(db, table, Some(cell))?;
        assert!(found.len() <= 1, "a key names one row");
        let Some(kept) = found.pop() else {
            crate::owner_texts::release(db, &holder(table, key))?;
            let invalid = || error("sessions_log_mark_invalid");
            if table == TABLES[5] {
                let session = SessionId::new(key).map_err(|_| invalid())?;
                self.resolves
                    .entry(project.to_owned())
                    .or_default()
                    .push(session);
            } else if table == TABLES[6] {
                let id = RequestId::new(key).map_err(|_| invalid())?;
                self.profiles
                    .entry(project.to_owned())
                    .or_default()
                    .push(id);
            }
            return Ok(None);
        };
        let event = saved(table, kept)?;
        if matches!(event, Event::Profile(_)) {
            self.receipts.push(event);
            return Ok(None);
        }
        Ok(Some(event))
    }

    /// The events a close logs last: dropped Resolves, then dropped receipts, then saved
    /// receipts, so replay never meets a session's profile twice. Output: the events, or
    /// `sessions_log_mark_invalid` for a stored project id that is not one.
    pub(super) fn finish(self) -> Result<Vec<Event>, EngineError> {
        let mut events = Vec::new();
        for (project, sessions) in self.resolves {
            for part in sessions.chunks(NAMES_MAX) {
                events.push(Event::ResolvesDropped(ResolvesDropped {
                    project: project_id(&project)?,
                    sessions: part.to_vec(),
                }));
            }
        }
        for (project, ids) in self.profiles {
            for part in ids.chunks(NAMES_MAX) {
                events.push(Event::ProfilesDropped(ProfilesDropped {
                    project: project_id(&project)?,
                    ids: part.to_vec(),
                }));
            }
        }
        events.extend(self.receipts);
        Ok(events)
    }
}

/// Marks a master turn a live writer inserted or saved, by its input, with its session's
/// project; a turn of no stored session is not marked. Inputs: the transaction and the
/// turn's input. Output: Ok, or the store's refusal.
pub(crate) fn mark_turn(db: &Connection, input: u64) -> Result<(), EngineError> {
    mark_rows(
        db,
        "SELECT 'master_turns', CAST(t.input AS TEXT), s.project FROM master_turns t JOIN master_sessions s ON s.id=t.session WHERE t.input=?1",
        params![signed(input)?],
    )
}

/// Marks a turn's preparation a live writer inserted, by the turn's input. Inputs: the
/// transaction, the session and the turn id. Output: Ok, or the store's refusal.
pub(crate) fn mark_preparation(
    db: &Connection,
    session: &str,
    turn: &str,
) -> Result<(), EngineError> {
    mark_rows(
        db,
        "SELECT 'master_preparations', CAST(t.input AS TEXT), s.project FROM master_turns t JOIN master_sessions s ON s.id=t.session WHERE t.session=?1 AND t.id=?2",
        params![session, turn],
    )
}

/// Marks a session's waiting Resolve a live writer saved or is about to delete, when the
/// session is stored in that project. Inputs: the transaction, the project and the session.
/// Output: Ok, or the store's refusal.
pub(crate) fn mark_resolve(
    db: &Connection,
    project: &str,
    session: &str,
) -> Result<(), EngineError> {
    mark_rows(
        db,
        "SELECT 'master_waiting_resolves', id, project FROM master_sessions WHERE id=?2 AND project=?1",
        params![project, session],
    )
}

/// Marks a configured profile receipt a live writer inserted or is about to delete, with
/// its session's project. Inputs: the transaction and the receipt id. Output: Ok, or the
/// store's refusal.
pub(crate) fn mark_profile(db: &Connection, id: &str) -> Result<(), EngineError> {
    mark_rows(
        db,
        "SELECT 'configured_master_profiles', c.id, s.project FROM configured_master_profiles c JOIN master_sessions s ON s.id=c.session WHERE c.id=?1",
        params![id],
    )
}

/// Marks every configured profile receipt of a stored session, before a bulk delete.
/// Input: the transaction. Output: Ok, or the store's refusal.
pub(crate) fn mark_profiles(db: &Connection) -> Result<(), EngineError> {
    mark_rows(
        db,
        "SELECT 'configured_master_profiles', c.id, s.project FROM configured_master_profiles c JOIN master_sessions s ON s.id=c.session",
        [],
    )
}

/// Marks a deleted session's waiting Resolve and profile receipts, so the Delete's close
/// logs them again after its redaction. Inputs: the transaction, the project and the
/// session. Output: Ok, or the store's refusal.
pub(crate) fn mark_session_rest(
    db: &Connection,
    project: &str,
    session: &str,
) -> Result<(), EngineError> {
    mark_rows(
        db,
        "SELECT 'master_waiting_resolves', r.session, s.project FROM master_waiting_resolves r JOIN master_sessions s ON s.id=r.session WHERE r.session=?2 AND s.project=?1",
        params![project, session],
    )?;
    mark_rows(
        db,
        "SELECT 'configured_master_profiles', c.id, s.project FROM configured_master_profiles c JOIN master_sessions s ON s.id=c.session WHERE c.session=?2 AND s.project=?1",
        params![project, session],
    )
}

/// Lets go of the kept bodies of a session's turns and preparations before a Delete removes
/// them. Inputs: the transaction and the session. Output: Ok, or the store's refusal.
pub(crate) fn release_session_turns(db: &Connection, session: &str) -> Result<(), EngineError> {
    let inputs: Vec<i64> = db
        .prepare("SELECT input FROM master_turns WHERE session=?1")
        .and_then(|mut statement| {
            statement
                .query_map([session], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(error)?;
    assert!(
        inputs.len() as u64 <= ORDINAL_MAX,
        "a session holds its bounded turns"
    );
    for input in inputs {
        let key = input.to_string();
        crate::owner_texts::release(db, &holder(TABLES[3], &key))?;
        crate::owner_texts::release(db, &holder(TABLES[4], &key))?;
    }
    Ok(())
}

/// Deletes a session's preparations and turns, for replay of L12's `SessionDeleted`, as
/// the live Delete does. Inputs: the connection and the session. Output: Ok, or the store's
/// refusal.
pub(super) fn delete_session_turns(db: &Connection, session: &str) -> Result<(), EngineError> {
    for table in [TABLES[4], TABLES[3]] {
        db.execute(&format!("DELETE FROM {table} WHERE session=?1"), [session])
            .map_err(error)?;
    }
    Ok(())
}

/// Every row of one of the four tables whose session is stored, in key order, as a JSON
/// array of its columns with its body as the kept digest last. Inputs: the transaction and
/// the table. Output: the rows, or the refusal.
pub(super) fn table_rows(db: &Connection, table: &str) -> Result<Vec<Value>, EngineError> {
    let rows: Vec<Value> = read(db, table, None)?
        .into_iter()
        .map(|kept| {
            let mut columns = kept.columns;
            columns.push(Value::String(kept.body.as_str().to_owned()));
            Value::Array(columns)
        })
        .collect();
    assert!(rows.len() <= super::SNAPSHOT_ROWS_MAX);
    Ok(rows)
}

/// Writes one row of the four tables from a snapshot row or a saved event, its columns in
/// the table's order with the body's digest last; a body no longer kept skips the row as
/// replay skips it. Inputs: the connection, the table, the cells and the verb (`INSERT` for
/// a run, `INSERT OR REPLACE` for an event). Output: whether the row was written, or
/// `sessions_log_chunk_invalid` for cells that are not the table's or a row it refuses.
pub(super) fn write(
    db: &Connection,
    table: &str,
    cells: &[Value],
    verb: &str,
) -> Result<bool, EngineError> {
    let invalid = || error("sessions_log_chunk_invalid");
    let (_, columns, _, _) = shape(table).ok_or_else(invalid)?;
    let width = columns.split(',').count();
    let [values @ .., Value::String(digest)] = cells else {
        return Err(invalid());
    };
    if cells.len() != width {
        return Err(invalid());
    }
    let digest = TextDigest::new(digest.clone()).map_err(|_| invalid())?;
    let mut missing = Vec::new();
    let text = crate::owner_texts::replayed(db, &digest, &mut missing)?;
    if !missing.is_empty() {
        return Ok(false);
    }
    let mut bound = values
        .iter()
        .map(|value| match value {
            Value::String(word) => Ok(Cell::Text(word.clone())),
            Value::Number(number) => number.as_i64().map(Cell::Integer).ok_or_else(invalid),
            _ => Err(invalid()),
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    bound.push(Cell::Text(String::from_utf8(text).map_err(|_| invalid())?));
    let slots = vec!["?"; width].join(",");
    db.execute(
        &format!("{verb} INTO {table}({columns}) VALUES ({slots})"),
        rusqlite::params_from_iter(bound),
    )
    .map_err(|_| invalid())?;
    Ok(true)
}

/// Applies one saved event of the four tables for replay: its row as it stands, replacing
/// the one it keeps; a body no longer kept skips it. Inputs: the connection, the table and
/// the row's cells. Output: Ok, or `sessions_log_chunk_invalid`.
pub(super) fn replace(db: &Connection, table: &str, row: Value) -> Result<(), EngineError> {
    let cells = row
        .as_array()
        .ok_or_else(|| error("sessions_log_chunk_invalid"))?;
    write(db, table, cells, "INSERT OR REPLACE")?;
    Ok(())
}

/// Gives `sessions_touched` the seven-table check when an older shape of it exists, every
/// mark kept. Input: the upgrade transaction. Output: Ok, or the store's refusal.
#[cfg(any(unix, test))]
pub(super) fn widen_touched(db: &Connection) -> Result<(), EngineError> {
    use rusqlite::OptionalExtension;
    let found: Option<String> = db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name='sessions_touched'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(sql) = found.filter(|sql| !sql.contains(TABLES[6])) else {
        return Ok(());
    };
    assert!(sql.contains(TABLES[0]), "an older shape names L13's tables");
    let count = || -> Result<i64, EngineError> {
        db.query_row("SELECT COUNT(*) FROM sessions_touched", [], |row| {
            row.get(0)
        })
        .map_err(error)
    };
    let before = count()?;
    db.execute_batch("ALTER TABLE sessions_touched RENAME TO sessions_touched_narrow")
        .map_err(error)?;
    db.execute_batch(TOUCHED).map_err(error)?;
    db.execute_batch(
        "INSERT INTO sessions_touched(tbl,key,project) SELECT tbl,key,project FROM sessions_touched_narrow; DROP TABLE sessions_touched_narrow",
    )
    .map_err(error)?;
    assert_eq!(count()?, before, "every mark is kept");
    Ok(())
}

/// Whether the log holds an event or a snapshot chunk of the four tables. Input: the
/// transaction. Output: the answer, or the store's refusal.
#[cfg(any(unix, test))]
pub(super) fn on_the_log(db: &Connection) -> Result<bool, EngineError> {
    use crate::state_log::{Aggregate, SNAPSHOT_CHUNK};
    if !exists(db, "state_log")? {
        return Ok(false);
    }
    let sessions = Aggregate::Sessions.name();
    let kinds = &super::KINDS[5..];
    let logged: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM state_log WHERE aggregate=?1 AND kind IN (?2,?3,?4,?5,?6,?7))",
            params![sessions, kinds[0], kinds[1], kinds[2], kinds[3], kinds[4], kinds[5]],
            |row| row.get(0),
        )
        .map_err(error)?;
    if logged {
        return Ok(true);
    }
    #[derive(Deserialize)]
    struct Named {
        table: String,
    }
    let mut statement = db
        .prepare("SELECT body FROM state_log WHERE aggregate=?1 AND kind=?2")
        .map_err(error)?;
    let bodies = statement
        .query_map(params![sessions, SNAPSHOT_CHUNK], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .map_err(error)?;
    for body in bodies {
        let named: Named = serde_json::from_slice(&body.map_err(error)?)
            .map_err(|_| error("sessions_log_chunk_invalid"))?;
        if TABLES[3..].contains(&named.table.as_str()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Inserts the marks one select names into `sessions_touched`, once each. Inputs: the
/// transaction, the select of table, key and project, and its parameters. Output: Ok, or
/// the store's refusal.
fn mark_rows(
    db: &Connection,
    select: &str,
    values: impl rusqlite::Params,
) -> Result<(), EngineError> {
    assert!(
        !db.is_autocommit(),
        "a key is marked in its write's transaction"
    );
    db.execute_batch(TOUCHED).map_err(error)?;
    db.execute(
        &format!("INSERT OR IGNORE INTO sessions_touched(tbl,key,project) {select}"),
        values,
    )
    .map_err(error)?;
    Ok(())
}

/// Reads the rows of one of the four tables whose session is stored, the one row of a key
/// or every row in key order, each body kept under its holder in the live transaction.
/// Inputs: the transaction, the table and the key. Output: the rows, none when a table it
/// reads is missing, `sessions_log_snapshot_rows` past [`super::SNAPSHOT_ROWS_MAX`], or
/// `sessions_log_row_invalid`.
fn read(db: &Connection, table: &str, key: Option<Cell>) -> Result<Vec<Kept>, EngineError> {
    let invalid = || error("sessions_log_row_invalid");
    let (_, columns, select, column) = shape(table).ok_or_else(invalid)?;
    for needed in ["master_sessions", "master_turns", table] {
        if !exists(db, needed)? {
            return Ok(Vec::new());
        }
    }
    let (sql, value) = match key {
        Some(key) => (format!("{select} WHERE {column}=?1"), key),
        None => (
            format!("{select} ORDER BY {column} LIMIT ?1"),
            Cell::Integer(signed(super::SNAPSHOT_ROWS_MAX as u64 + 1)?),
        ),
    };
    let width = columns.split(',').count() + 2;
    let stored = db
        .prepare(&sql)
        .and_then(|mut statement| {
            statement
                .query_map([value], |row| (0..width).map(|at| cell(row, at)).collect())?
                .collect::<rusqlite::Result<Vec<Vec<Value>>>>()
        })
        .map_err(error)?;
    if stored.len() > super::SNAPSHOT_ROWS_MAX {
        return Err(error("sessions_log_snapshot_rows"));
    }
    let mut rows = Vec::with_capacity(stored.len());
    for mut columns in stored {
        let (Some(key), Some(Value::String(project)), Some(Value::String(body))) =
            (columns.pop(), columns.pop(), columns.pop())
        else {
            return Err(invalid());
        };
        let key = match key {
            Value::Number(number) => number.to_string(),
            Value::String(word) => word,
            _ => return Err(invalid()),
        };
        let project = project_id(&project)?;
        let body =
            crate::owner_texts::keep(db, &holder(table, &key), Some(&project), body.as_bytes())?;
        rows.push(Kept {
            columns,
            key,
            project,
            body,
        });
    }
    Ok(rows)
}

/// One column of a read row as JSON: an integer as a number, text as a string, anything
/// else as null, which the reader refuses. Inputs: the row and the column. Output: the value.
fn cell(row: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Value> {
    Ok(match row.get_ref(at)? {
        ValueRef::Integer(number) => Value::from(number),
        ValueRef::Text(text) => {
            std::str::from_utf8(text).map_or(Value::Null, |text| Value::String(text.to_owned()))
        }
        _ => Value::Null,
    })
}

/// A present row of the four tables as its saved event, checked as replay checks it.
/// Inputs: the table and the row. Output: the event, or `sessions_log_row_invalid`.
fn saved(table: &str, kept: Kept) -> Result<Event, EngineError> {
    let invalid = || error("sessions_log_row_invalid");
    let Kept {
        columns,
        key,
        project,
        body,
    } = kept;
    let event = match (table, columns.as_slice()) {
        ("master_turns", [session, ordinal, id, input, phase]) => serde_json::from_value(json!({
            "project": project, "session": session, "ordinal": ordinal, "id": id,
            "input": input, "phase": phase, "body": body,
        }))
        .map(Event::Turn),
        ("master_preparations", [session, turn]) => serde_json::from_value(json!({
            "project": project, "session": session, "turn": turn, "input": number(&key)?,
            "body": body,
        }))
        .map(Event::Preparation),
        ("master_waiting_resolves", [session, stored, operation, input]) => {
            serde_json::from_value(json!({
                "project": stored, "session": session, "operation": operation,
                "input": input, "body": body,
            }))
            .map(Event::Resolve)
        }
        ("configured_master_profiles", [id, session, profile]) => serde_json::from_value(json!({
            "project": project, "session": session, "id": id, "profile": profile,
            "body": body,
        }))
        .map(Event::Profile),
        _ => return Err(invalid()),
    }
    .map_err(|_| invalid())?;
    if !valid(&event) {
        return Err(invalid());
    }
    Ok(event)
}

/// The shape of one of the four tables. Input: the table. Output: its shape, or none.
fn shape(table: &str) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    SHAPES.into_iter().find(|(name, _, _, _)| *name == table)
}

/// The holder key a row of the four tables keeps its body under. Inputs: the table and the
/// row's key. Output: `<table>/<key>/body`.
fn holder(table: &str, key: &str) -> String {
    format!("{table}/{key}/body")
}
