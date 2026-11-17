use super::{Chunk, SNAPSHOT_ROWS_MAX, cell, failure, has};
use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// Keeps a kept table's stored row, its cells by column name, as an owner text in parts
/// under the row's part holders, releasing every part an earlier write kept past them.
/// A table without a project column, such as verification candidates and attempts, keeps
/// its row with no project (QUESTION-1132 A). Inputs: the writer's transaction, the table
/// and the row's id. Output: the row's project and the part digests, none without the row,
/// `landing_log_change_set_too_large` past 64 MiB, `landing_log_row` or the owner text's
/// refusal.
pub(super) fn keep_row(
    tx: &Transaction<'_>,
    table: &str,
    key: &serde_json::Value,
) -> Result<Option<(Option<String>, Parts)>, EngineError> {
    let Some(cells) = row_cells(tx, table, key)? else {
        return Ok(None);
    };
    let refused = || failure("landing_log_row");
    let project = match cells.get("project") {
        None => None,
        Some(named) => Some(named.as_str().ok_or_else(refused)?.to_owned()),
    };
    let owner = project
        .as_deref()
        .map(crate::protocol::ProjectId::new)
        .transpose()
        .map_err(|_| refused())?;
    let text = serde_json::to_vec(&cells).map_err(|_| refused())?;
    if text.len() > ROW_BYTES_MAX {
        return Err(failure(ROW_TOO_LARGE));
    }
    let mut parts = Vec::with_capacity(ROW_PARTS_MAX);
    for (index, part) in text.chunks(crate::owner_texts::TEXT_BYTES_MAX).enumerate() {
        let holder = part_holder(table, key, index);
        parts.push(crate::owner_texts::keep(tx, &holder, owner.as_ref(), part)?);
    }
    assert!(
        (1..=ROW_PARTS_MAX).contains(&parts.len()),
        "a row is 1 to 64 parts"
    );
    for index in parts.len()..ROW_PARTS_MAX {
        crate::owner_texts::release(tx, &part_holder(table, key, index))?;
    }
    assert_eq!(
        owner.as_ref().map(|owner| owner.as_str()),
        project.as_deref()
    );
    Ok(Some((project, parts)))
}

/// Releases every part a kept row's text was kept in, for a row its writer deleted, so a
/// replay skips the saves before it as superseded. Inputs: the writer's transaction, the table
/// and the row's id. Output: Ok, or the owner text's refusal.
pub(super) fn release_row(
    tx: &Transaction<'_>,
    table: &str,
    key: &serde_json::Value,
) -> Result<(), EngineError> {
    assert!(KEPT_TABLES.contains(&table), "a kept table");
    assert!(
        !tx.is_autocommit(),
        "a row is released in its writer's transaction"
    );
    for index in 0..ROW_PARTS_MAX {
        crate::owner_texts::release(tx, &part_holder(table, key, index))?;
    }
    Ok(())
}

/// The part digests of one change set row's text, in order.
pub(super) type Parts = Vec<crate::owner_texts::TextDigest>;

/// The most parts one change set row's text is kept in, and the most bytes it holds: 64 MiB,
/// the cap on git's listings a row's conflicts come from (QUESTION-1115 A).
pub(super) const ROW_PARTS_MAX: usize = 64;
pub(super) const ROW_BYTES_MAX: usize = 64 << 20;
const _: () = assert!(ROW_PARTS_MAX * crate::owner_texts::TEXT_BYTES_MAX == ROW_BYTES_MAX);

/// The refusal of a change set row past [`ROW_BYTES_MAX`], at save only, never at open.
pub(super) const ROW_TOO_LARGE: &str = "landing_log_change_set_too_large";

/// The holder of one part of a kept row: `<table>/<id>/row/<index>`. Inputs: the table, the
/// row's id and the part's index. Output: the holder key.
pub(super) fn part_holder(table: &str, key: &serde_json::Value, index: usize) -> String {
    assert!(index < ROW_PARTS_MAX, "a part index is in its bound");
    let id = key.as_str().map_or_else(|| key.to_string(), str::to_owned);
    assert!(!id.is_empty());
    format!("{table}/{id}/row/{index}")
}

/// A kept row's id as the store binds it: a change set's text or a rule request's integer.
/// Input: the id. Output: the stored value, or `landing_log_row` for any other cell.
pub(super) fn bound(key: &serde_json::Value) -> Result<rusqlite::types::Value, EngineError> {
    use rusqlite::types::Value as Stored;
    match key {
        serde_json::Value::String(text) => Ok(Stored::Text(text.clone())),
        serde_json::Value::Number(number) => number
            .as_i64()
            .map(Stored::Integer)
            .ok_or_else(|| failure("landing_log_row")),
        _ => Err(failure("landing_log_row")),
    }
}

/// The change set table, the rule request table and the three verification tables, logged
/// by their rows' part digests rather than by cells (QUESTION-1056 A, QUESTION-1120 A,
/// QUESTION-1132 A), in run order.
pub(super) const CHANGE_SETS: &str = "landing_change_sets";
pub(super) const RULE_REQUESTS: &str = "github_rule_requests";
pub(super) const VERIFICATION_TABLES: [&str; 3] = [
    "verification_baselines",
    "verification_candidates",
    "verification_attempts",
];
pub(super) const KEPT_TABLES: [&str; 5] = [
    CHANGE_SETS,
    RULE_REQUESTS,
    VERIFICATION_TABLES[0],
    VERIFICATION_TABLES[1],
    VERIFICATION_TABLES[2],
];

/// The most columns a kept row has, a bound on the reads of its cells.
pub(super) const ROW_COLUMNS_MAX: usize = 64;

/// One kept row's cells by column name: text, integer or null. Inputs: the connection, the
/// table and the row's id. Output: the cells, none without the row, or the refusal.
pub(super) fn row_cells(
    db: &Connection,
    table: &str,
    key: &serde_json::Value,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, EngineError> {
    assert!(KEPT_TABLES.contains(&table), "a kept table");
    let unavailable = |_| failure("changes_unavailable");
    let id = bound(key)?;
    let mut statement = db
        .prepare(&format!("SELECT * FROM {table} WHERE id=?1"))
        .map_err(unavailable)?;
    let names: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert!(
        names.len() <= ROW_COLUMNS_MAX,
        "a change set row is bounded"
    );
    let stored = statement
        .query_row([id], |row| {
            (0..names.len())
                .map(|column| row.get::<_, rusqlite::types::Value>(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .optional()
        .map_err(unavailable)?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let mut cells = serde_json::Map::new();
    for (name, value) in names.into_iter().zip(stored) {
        cells.insert(name, cell(value)?);
    }
    assert_eq!(cells.get("id"), Some(key), "the row is the one asked for");
    Ok(Some(cells))
}

/// Upserts a change set row from the text kept in its parts; a row with a part a later
/// write released is superseded and is skipped. Inputs: the connection, the row's id and
/// project, none for a table without one, and the part digests. Output: whether the row was
/// upserted, `landing_log_row` for a part count past the bound or a text that is not one
/// row's cells, or the refusal of [`upsert_row`].
pub(super) fn upsert_kept(
    db: &Connection,
    table: &str,
    key: &serde_json::Value,
    project: Option<&str>,
    parts: &[crate::owner_texts::TextDigest],
) -> Result<bool, EngineError> {
    if !(1..=ROW_PARTS_MAX).contains(&parts.len()) {
        return Err(failure("landing_log_row"));
    }
    let mut missing = Vec::new();
    let mut text = Vec::new();
    for part in parts {
        text.extend(crate::owner_texts::replayed(db, part, &mut missing)?);
    }
    if !missing.is_empty() {
        return Ok(false);
    }
    assert!(text.len() <= ROW_BYTES_MAX, "kept parts are in their bound");
    let cells: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&text).map_err(|_| failure("landing_log_row"))?;
    upsert_row(db, table, key, project, &cells)?;
    assert!(missing.is_empty());
    Ok(true)
}

/// Upserts a change set row from its cells by column name: a cell the table has no column
/// for, or of a type no column holds, fails closed; a column with no cell takes its default.
/// A store without the table refuses every cell. Inputs: the connection, the row's id and
/// project, none for a table without one, and the cells. Output: Ok, `landing_log_row` for
/// cells of another row, `landing_log_row_cell`, or the store's refusal.
pub(super) fn upsert_row(
    db: &Connection,
    table: &str,
    key: &serde_json::Value,
    project: Option<&str>,
    cells: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), EngineError> {
    use rusqlite::types::Value as Stored;
    use serde_json::Value as Cell;
    assert!(KEPT_TABLES.contains(&table), "a kept table");
    let refused = || failure("landing_log_row_cell");
    let named = cells.get("project").and_then(Cell::as_str);
    if cells.get("id") != Some(key) || named != project {
        return Err(failure("landing_log_row"));
    }
    let columns = table_columns(db, table)?;
    let mut names = Vec::with_capacity(cells.len());
    let mut values = Vec::with_capacity(cells.len());
    for (name, value) in cells {
        if !columns.contains(name) {
            return Err(refused());
        }
        let stored = match value {
            Cell::Null => Stored::Null,
            Cell::String(text) => Stored::Text(text.clone()),
            Cell::Number(number) => number.as_i64().map(Stored::Integer).ok_or_else(refused)?,
            _ => return Err(refused()),
        };
        names.push(format!("\"{name}\""));
        values.push(stored);
    }
    let marks = (1..=names.len()).map(|index| format!("?{index}"));
    let updates = columns
        .iter()
        .filter(|column| column.as_str() != "id")
        .map(|column| format!("\"{column}\"=excluded.\"{column}\""));
    let sql = format!(
        "INSERT INTO {table}({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {}",
        names.join(","),
        marks.collect::<Vec<_>>().join(","),
        updates.collect::<Vec<_>>().join(",")
    );
    db.execute(&sql, rusqlite::params_from_iter(values))
        .map_err(|_| failure("changes_unavailable"))?;
    assert!(names.len() <= columns.len());
    assert!(names.len() >= 2, "a row has its id and a cell");
    Ok(())
}

/// The column names of a table, in schema order. Inputs: the connection and the table.
/// Output: the names, or `changes_unavailable` past [`ROW_COLUMNS_MAX`].
pub(super) fn table_columns(db: &Connection, table: &str) -> Result<Vec<String>, EngineError> {
    let unavailable = |_| failure("changes_unavailable");
    let limit = i64::try_from(ROW_COLUMNS_MAX + 1).map_err(|_| failure("changes_unavailable"))?;
    let columns = db
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid LIMIT ?2")
        .map_err(unavailable)?
        .query_map(params![table, limit], |row| row.get::<_, String>(0))
        .map_err(unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(unavailable)?;
    if columns.len() > ROW_COLUMNS_MAX {
        return Err(failure("changes_unavailable"));
    }
    assert!(columns.iter().all(|column| !column.is_empty()));
    Ok(columns)
}

/// Every row of a kept table as a snapshot carries it: its id, project (null for a table
/// without one) and the part digests
/// of its row, kept in the run's transaction; a row past 64 MiB is left out by code, never
/// refused, so the store opens; a store without the table has none. Inputs: the run's
/// transaction and the table. Output: the rows by id, `landing_log_snapshot_rows` past
/// [`SNAPSHOT_ROWS_MAX`], or the refusal of [`keep_row`].
pub(super) fn kept_rows(
    tx: &Transaction<'_>,
    table: &str,
) -> Result<Vec<serde_json::Value>, EngineError> {
    if !has(tx, table)? {
        return Ok(Vec::new());
    }
    let unavailable = |_| failure("changes_unavailable");
    let limit =
        i64::try_from(SNAPSHOT_ROWS_MAX + 1).map_err(|_| failure("landing_log_snapshot_rows"))?;
    let ids = tx
        .prepare(&format!("SELECT id FROM {table} ORDER BY id LIMIT ?1"))
        .map_err(unavailable)?
        .query_map([limit], |row| row.get::<_, rusqlite::types::Value>(0))
        .map_err(unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(unavailable)?;
    if ids.len() > SNAPSHOT_ROWS_MAX {
        return Err(failure("landing_log_snapshot_rows"));
    }
    let mut rows = Vec::with_capacity(ids.len());
    for id in ids {
        let id = cell(id)?;
        match keep_row(tx, table, &id) {
            Ok(Some((project, parts))) => rows.push(serde_json::json!([id, project, parts])),
            Err(refused) if refused.0 == ROW_TOO_LARGE => {
                eprintln!("landing_log_change_set_left_out code={ROW_TOO_LARGE}");
            }
            Ok(None) => return Err(failure("changes_unavailable")),
            Err(refused) => return Err(refused),
        }
    }
    assert!(rows.len() <= SNAPSHOT_ROWS_MAX);
    assert!(
        rows.iter()
            .all(|row| row.as_array().is_some_and(|row| row.len() == 3))
    );
    Ok(rows)
}

/// Upserts each row of a kept table's chunk from the row text its parts name, skipping
/// a row a later write released. Inputs: the connection and the chunk. Output: Ok,
/// `landing_log_chunk_invalid` for a row that is not an id, a project and 1 to 64 part
/// digests, or the refusal of [`upsert_kept`].
pub(super) fn restore_kept(db: &Connection, chunk: &Chunk) -> Result<(), EngineError> {
    let table = KEPT_TABLES
        .into_iter()
        .find(|table| *table == chunk.table)
        .ok_or_else(|| failure("landing_log_chunk_invalid"))?;
    if chunk.rows.len() > SNAPSHOT_ROWS_MAX {
        return Err(failure("landing_log_chunk_invalid"));
    }
    for row in &chunk.rows {
        let (key, project, parts): (serde_json::Value, Option<String>, Parts) =
            serde_json::from_value(row.clone())
                .map_err(|_| failure("landing_log_chunk_invalid"))?;
        if !(1..=ROW_PARTS_MAX).contains(&parts.len()) {
            return Err(failure("landing_log_chunk_invalid"));
        }
        upsert_kept(db, table, &key, project.as_deref(), &parts)?;
    }
    assert!(chunk.rows.len() <= SNAPSHOT_ROWS_MAX);
    Ok(())
}
