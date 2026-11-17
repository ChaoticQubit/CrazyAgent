use super::*;

pub(super) fn snapshot(tx: &Transaction<'_>) -> Result<Outcome, EngineError> {
    let projects = read_rows(tx, "SELECT body FROM projects ORDER BY id LIMIT 129")?;
    let tasks = read_rows(tx, "SELECT body FROM tasks ORDER BY id LIMIT 129")?;
    if projects.len() > 128 || tasks.len() > 128 {
        return Err(error("state_capacity"));
    }
    assert!(projects.len() <= 128);
    assert!(tasks.len() <= 128);
    Ok(Outcome::Snapshot { projects, tasks })
}

fn read_rows<T: serde::de::DeserializeOwned>(
    tx: &Transaction<'_>,
    sql: &str,
) -> Result<Vec<T>, EngineError> {
    let mut statement = tx.prepare(sql).map_err(error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?;
    rows.map(|row| serde_json::from_str(&row.map_err(error)?).map_err(error))
        .collect()
}

pub(super) fn enforce_snapshot_bound(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let snapshot = snapshot(tx)?;
    let bytes = serde_json::to_vec(&snapshot).map_err(error)?;
    if bytes.len() > FRAME_BYTES_MAX - 1024 {
        return Err(error("state_capacity"));
    }
    assert!(bytes.len() <= FRAME_BYTES_MAX - 1024);
    Ok(())
}
