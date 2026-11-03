use super::{
    Catalog, KEYS_MAX, Page, Resolved, Scope, Setting, Stored, VALUE_JSON_MAX, Written,
    check_value, item, key_valid, refusal, resolve, setting, values_page,
};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use crate::storage::versioned;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

/// The most stored keys the catalog no longer lists that one scope reads, in key order.
/// Later ones are read once the owner resets earlier ones.
pub const UNLISTED_MAX: usize = KEYS_MAX;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS harness_settings (
    scope TEXT NOT NULL CHECK (scope IN ('app', 'project')),
    project TEXT NOT NULL,
    harness TEXT NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    updated_ms INTEGER NOT NULL,
    CHECK ((scope = 'app') = (project = '')),
    PRIMARY KEY (scope, project, harness, key)
) WITHOUT ROWID;";

/// Reads the values one harness has stored at one scope, by key: every key the catalog
/// lists, then at most `UNLISTED_MAX` keys it no longer lists, so old rows never make a
/// scope unreadable. Input: a connection or transaction on `engine.db`, the catalog and
/// the scope. Output: the values, or `harness_settings_unavailable` when the table cannot
/// be read or holds a row no set could have written.
pub fn stored(db: &Connection, catalog: &Catalog, scope: &Scope) -> Result<Stored, EngineError> {
    if !exists(db)? {
        return Ok(Stored::new());
    }
    let (scope_text, project) = columns(scope);
    let mut values = Stored::new();
    let mut one = db
        .prepare(
            "SELECT value_json FROM harness_settings
             WHERE scope = ?1 AND project = ?2 AND harness = ?3 AND key = ?4",
        )
        .map_err(unavailable)?;
    for key in &catalog.keys {
        let json: Option<String> = one
            .query_row(
                params![scope_text, project, catalog.harness, key.key],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        if let Some(json) = json {
            values.insert(key.key.clone(), parsed(&json)?);
        }
    }
    let listed = values.len();
    let mut every = db
        .prepare(
            "SELECT key, value_json FROM harness_settings
             WHERE scope = ?1 AND project = ?2 AND harness = ?3 ORDER BY key",
        )
        .map_err(unavailable)?;
    let mut rows = every
        .query(params![scope_text, project, catalog.harness])
        .map_err(unavailable)?;
    let mut unlisted = 0;
    while unlisted < UNLISTED_MAX {
        let Some(row) = rows.next().map_err(unavailable)? else {
            break;
        };
        let key: String = row.get(0).map_err(unavailable)?;
        if catalog.key(&key).is_some() {
            continue;
        }
        if !key_valid(&key) {
            return Err(unavailable(()));
        }
        let json: String = row.get(1).map_err(unavailable)?;
        values.insert(key, parsed(&json)?);
        unlisted += 1;
    }
    assert_eq!(values.len(), listed + unlisted);
    assert!(values.len() <= catalog.keys.len() + UNLISTED_MAX);
    Ok(values)
}

/// Builds one page of a harness's settings at one scope, read in one snapshot. Inputs:
/// the engine database, the catalog, the installed version, the scope and the index of
/// the page's first row. Output: the page, or `harness_settings_project_unknown`,
/// `harness_settings_page_invalid` or `harness_settings_unavailable`.
pub fn values(
    db: &Connection,
    catalog: &Catalog,
    installed: Option<&str>,
    scope: &Scope,
    after: Option<u32>,
) -> Result<Page, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Deferred).map_err(unavailable)?;
    project_known(&tx, scope)?;
    let (app, project) = layers(&tx, catalog, scope)?;
    let here = match scope {
        Scope::App => &app,
        Scope::Project(_) => &project,
    };
    let names: Vec<String> = catalog
        .keys
        .iter()
        .map(|key| key.key.clone())
        .chain(
            here.keys()
                .filter(|name| catalog.key(name).is_none())
                .cloned(),
        )
        .collect();
    let mut rows = Vec::with_capacity(names.len());
    for name in &names {
        let version = versioned::current(&tx, &item(&catalog.harness, scope, name)?)?.version;
        rows.push(setting(
            catalog, installed, scope, name, &app, &project, version,
        ));
    }
    assert_eq!(rows.len(), names.len());
    values_page(catalog, installed, rows, after)
}

/// Sets one key at one scope as the next version of its item, when the owner read the
/// current one. Checks run in order: the key name, the project, whether the key may be set
/// here, the value, then the version. A reset may also remove a stored key the catalog no
/// longer lists, whatever version is installed. Inputs: the engine database, the catalog, the
/// installed version, the scope, the key, the value and the version the owner read.
/// Output: Saved or Stale with the key's row, Refused with the code and the row, or
/// `harness_settings_key_invalid`, `harness_settings_project_unknown`,
/// `harness_settings_value_invalid` or `harness_settings_unavailable`.
pub fn set(
    db: &Connection,
    catalog: &Catalog,
    installed: Option<&str>,
    scope: &Scope,
    key: &str,
    value: &Value,
    version: u64,
) -> Result<Written, EngineError> {
    write(db, catalog, installed, scope, key, Some(value), version)
}

/// Removes one key's value at one scope as the next version of its item, so the key falls
/// back to the app value or the catalog default. Inputs and output: as `set`, without a
/// value.
pub fn reset(
    db: &Connection,
    catalog: &Catalog,
    installed: Option<&str>,
    scope: &Scope,
    key: &str,
    version: u64,
) -> Result<Written, EngineError> {
    write(db, catalog, installed, scope, key, None, version)
}

/// Reads what one harness start applies for a project, or app-wide, per `resolve`. Inputs:
/// the engine database, the catalog, the installed version and the project, none for an
/// app-level start. Output: the values in apply order, or `harness_settings_unavailable`.
pub fn resolved(
    db: &Connection,
    catalog: &Catalog,
    installed: Option<&str>,
    project: Option<&ProjectId>,
) -> Result<Vec<Resolved>, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Deferred).map_err(unavailable)?;
    let app = stored(&tx, catalog, &Scope::App)?;
    let overrides = match project {
        Some(project) => Some(stored(&tx, catalog, &Scope::Project(project.clone()))?),
        None => None,
    };
    Ok(resolve(catalog, installed, &app, overrides.as_ref()))
}

fn write(
    db: &Connection,
    catalog: &Catalog,
    installed: Option<&str>,
    scope: &Scope,
    key: &str,
    value: Option<&Value>,
    version: u64,
) -> Result<Written, EngineError> {
    if !key_valid(key) {
        return Err(failure("harness_settings_key_invalid"));
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    project_known(&tx, scope)?;
    let item = item(&catalog.harness, scope, key)?;
    let row = |tx: &Transaction<'_>| -> Result<Setting, EngineError> {
        let (app, project) = layers(tx, catalog, scope)?;
        let version = versioned::current(tx, &item)?.version;
        Ok(setting(
            catalog, installed, scope, key, &app, &project, version,
        ))
    };
    let definition = catalog.key(key);
    let retired = value.is_none() && definition.is_none() && holds(&tx, catalog, scope, key)?;
    if let Some(code) = refusal(catalog, installed, scope, key).filter(|_| !retired) {
        return Ok(Written::Refused {
            code: code.into(),
            setting: row(&tx)?,
        });
    }
    assert!(definition.is_some() || retired);
    if let (Some(definition), Some(value)) = (definition, value) {
        check_value(definition, value)?;
    }
    let next = match versioned::owner_write(&tx, &item, version)? {
        Ok(next) => next,
        Err(_) => {
            return Ok(Written::Stale { setting: row(&tx)? });
        }
    };
    tx.execute_batch(SCHEMA).map_err(unavailable)?;
    let (scope_text, project) = columns(scope);
    let changed = match value {
        Some(value) => {
            let json = serde_json::to_string(value).map_err(unavailable)?;
            assert!(json.len() <= VALUE_JSON_MAX);
            tx.execute(
                "INSERT INTO harness_settings(scope, project, harness, key, value_json, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(scope, project, harness, key)
                 DO UPDATE SET value_json = excluded.value_json, updated_ms = excluded.updated_ms",
                params![
                    scope_text,
                    project,
                    catalog.harness,
                    key,
                    json,
                    crate::team::now_ms()
                ],
            )
            .map_err(unavailable)?
        }
        None => tx
            .execute(
                "DELETE FROM harness_settings
                 WHERE scope = ?1 AND project = ?2 AND harness = ?3 AND key = ?4",
                params![scope_text, project, catalog.harness, key],
            )
            .map_err(unavailable)?,
    };
    assert!(changed <= 1);
    assert!(value.is_none() || changed == 1);
    let setting = row(&tx)?;
    assert_eq!(setting.version, next);
    assert_eq!(setting.here.as_ref(), value);
    tx.commit().map_err(unavailable)?;
    Ok(Written::Saved { setting })
}

fn layers(
    db: &Connection,
    catalog: &Catalog,
    scope: &Scope,
) -> Result<(Stored, Stored), EngineError> {
    let app = stored(db, catalog, &Scope::App)?;
    let project = match scope {
        Scope::App => Stored::new(),
        Scope::Project(_) => stored(db, catalog, scope)?,
    };
    Ok((app, project))
}

/// Tells whether one key has a stored row at one scope, whether or not `stored` reads it.
/// Inputs: the transaction, the catalog, the scope and a valid key. Output: true when the
/// row exists, or `harness_settings_unavailable`.
fn holds(
    db: &Connection,
    catalog: &Catalog,
    scope: &Scope,
    key: &str,
) -> Result<bool, EngineError> {
    if !exists(db)? {
        return Ok(false);
    }
    let (scope_text, project) = columns(scope);
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM harness_settings
         WHERE scope = ?1 AND project = ?2 AND harness = ?3 AND key = ?4)",
        params![scope_text, project, catalog.harness, key],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

/// Reads one stored value. Input: its JSON text. Output: the value, or
/// `harness_settings_unavailable` for text longer than any set writes or not JSON.
fn parsed(json: &str) -> Result<Value, EngineError> {
    if json.len() > VALUE_JSON_MAX {
        return Err(unavailable(()));
    }
    serde_json::from_str(json).map_err(unavailable)
}

fn project_known(db: &Connection, scope: &Scope) -> Result<(), EngineError> {
    let Scope::Project(project) = scope else {
        return Ok(());
    };
    let known: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if known {
        Ok(())
    } else {
        Err(failure("harness_settings_project_unknown"))
    }
}

fn columns(scope: &Scope) -> (&'static str, &str) {
    match scope {
        Scope::App => ("app", ""),
        Scope::Project(project) => ("project", project.as_str()),
    }
}

fn exists(db: &Connection) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table'
         AND name = 'harness_settings')",
        [],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

fn unavailable<E>(_: E) -> EngineError {
    failure("harness_settings_unavailable")
}
