use super::rules::PickRule;
use super::{
    CATALOG_MODELS_MAX, CUSTOM_MAX, Candidate, MODEL_BYTES_MAX, Models, NOT_SUPPORTED, NativeModel,
    Origin, Row, Slot, VALUE_JSON_MAX, Written, assert_catalog, fallback, harness_id, item,
    layered, model_valid, supported, unavailable, value_ok,
};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::harness_settings::{Access, Scope, Source};
use crate::protocol::ProjectId;
use crate::storage::versioned;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS model_choices (
    scope TEXT NOT NULL CHECK (scope IN ('app', 'project')),
    project TEXT NOT NULL,
    harness TEXT NOT NULL,
    slot TEXT NOT NULL,
    value_json TEXT NOT NULL,
    updated_ms INTEGER NOT NULL,
    CHECK ((scope = 'app') = (project = '')),
    PRIMARY KEY (scope, project, harness, slot)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS model_custom (
    harness TEXT NOT NULL,
    model TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'verified', 'failed')),
    checked_ms INTEGER NOT NULL,
    PRIMARY KEY (harness, model)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS model_live (
    harness TEXT NOT NULL PRIMARY KEY,
    models_json TEXT NOT NULL,
    reported INTEGER NOT NULL CHECK (reported IN (0, 1)),
    read_ms INTEGER NOT NULL
) WITHOUT ROWID;";

/// The longest kept live list a row may hold: `CATALOG_MODELS_MAX` quoted ids and commas.
const LIVE_JSON_MAX: usize = CATALOG_MODELS_MAX * (MODEL_BYTES_MAX + 3) + 2;

/// What a spawn's model pick does: run on the model named, none for the account's own,
/// run on the default instead because the owner's pick rule overrules every pick, refuse
/// a pick outside the models the rule allows with those models and the default, or
/// refuse any pick on a harness whose model CrazyAgent does not choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Run(Option<String>),
    Overruled(Option<String>),
    Refused {
        allowed: Vec<String>,
        default: Option<String>,
    },
    Unsupported,
}

/// What adding an owner's model id did: it waits for its test launch, the harness's
/// catalog already lists it, or it already passed its test launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Begun {
    Pending,
    Listed,
    Verified,
}

/// Builds a harness's Models section at one scope, read in one snapshot, saying whether
/// the shipped catalog was checked on the installed version (DEC-150 G1). Inputs: the
/// engine database, the harness name, its model catalog, the scope and the installed
/// version, none when unknown. Output: the section, or `model_choice_harness_unknown`,
/// `model_choice_project_unknown` or `model_choice_unavailable`.
pub fn values(
    db: &Connection,
    harness: &str,
    catalog: &[String],
    scope: &Scope,
    installed: Option<&str>,
) -> Result<Models, EngineError> {
    let harness = known(harness)?;
    assert_catalog(harness, catalog);
    let own = snapshot(db)?;
    let tx = own.as_deref().unwrap_or(db);
    project_known(tx, scope)?;
    let candidates = candidates(tx, harness, catalog)?;
    let usable = usable(&candidates);
    let mut rows = Vec::with_capacity(Slot::ALL.len());
    for slot in Slot::ALL {
        rows.push(row(tx, harness, scope, slot, &usable)?);
    }
    assert_eq!(rows.len(), Slot::ALL.len());
    let reason = super::unqualified(harness, installed);
    let reported = super::live::reader(harness).is_some()
        && live_kept(tx, harness)?.is_some_and(|(_, reported)| reported);
    Ok(Models {
        harness: harness.to_owned(),
        rows,
        candidates,
        qualified: reason.is_none(),
        reason,
        testable: super::test_launch::supported(harness),
        live_note: (!reported).then(|| super::live::not_reported(harness)),
    })
}

/// Lists the model ids a harness offers (DEC-150 addendum 11): its shipped ids first, then
/// the ids its last good live read reported, with every earlier one a saved slot still
/// names. Every launch, pick and Models read checks against this list. Inputs: the engine
/// database and the harness name. Output: the ids, or `model_choice_unavailable`.
pub fn catalog(db: &Connection, harness: &str) -> Result<Vec<String>, EngineError> {
    let shipped = super::catalog(harness);
    let Some(harness) = harness_id(harness) else {
        return Ok(shipped);
    };
    let Some((live, _)) = live_kept(db, harness)? else {
        return Ok(shipped);
    };
    Ok(super::live::merged(harness, &shipped, &live))
}

/// Keeps what one live read of a harness's model list found: a reported list replaces the
/// kept one, keeping each earlier id a saved slot of the harness still names, so a saved
/// model is never dropped; none keeps the list and marks the read unreported. Inputs: the
/// engine database, the harness name and the reported ids, none when it reported none.
/// Output: Ok, or `model_choice_harness_unknown` or `model_choice_unavailable`.
pub fn live_record(
    db: &Connection,
    harness: &str,
    read: Option<&[String]>,
) -> Result<(), EngineError> {
    let harness = known(harness)?;
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    tx.execute_batch(SCHEMA).map_err(unavailable)?;
    let earlier = live_kept(&tx, harness)?.map(|(models, _)| models);
    let mut models = match read {
        None => earlier.unwrap_or_default(),
        Some(read) => {
            let saved = saved_ids(&tx, harness)?;
            let shipped = super::catalog(harness);
            let held: Vec<String> = earlier
                .unwrap_or_default()
                .into_iter()
                .filter(|id| saved.contains(id) && !shipped.contains(id))
                .collect();
            let room = CATALOG_MODELS_MAX.saturating_sub(shipped.len() + held.len());
            let mut seen = std::collections::BTreeSet::new();
            let mut models: Vec<String> = read
                .iter()
                .filter(|id| {
                    model_valid(harness, id)
                        && !shipped.contains(id)
                        && !held.contains(id)
                        && seen.insert(id.as_str())
                })
                .take(room)
                .cloned()
                .collect();
            models.extend(held);
            models
        }
    };
    models.truncate(CATALOG_MODELS_MAX);
    assert_catalog(harness, &models);
    let json = serde_json::to_string(&models).map_err(unavailable)?;
    assert!(json.len() <= LIVE_JSON_MAX);
    tx.execute(
        "INSERT INTO model_live(harness, models_json, reported, read_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(harness) DO UPDATE SET models_json = excluded.models_json,
         reported = excluded.reported, read_ms = excluded.read_ms",
        params![harness, json, read.is_some(), crate::team::now_ms()],
    )
    .map_err(unavailable)?;
    tx.commit().map_err(unavailable)
}

/// Reads the live list kept for a harness and whether its last read reported it. Inputs:
/// the engine database and the harness id. Output: the ids and the flag, none when no
/// read was kept, or `model_choice_unavailable` for a row out of contract.
fn live_kept(db: &Connection, harness: &str) -> Result<Option<(Vec<String>, bool)>, EngineError> {
    if !table(db, "model_live")? {
        return Ok(None);
    }
    let row: Option<(String, bool)> = db
        .query_row(
            "SELECT models_json, reported FROM model_live WHERE harness = ?1",
            [harness],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((json, reported)) = row else {
        return Ok(None);
    };
    if json.len() > LIVE_JSON_MAX {
        return Err(unavailable(()));
    }
    let models: Vec<String> = serde_json::from_str(&json).map_err(unavailable)?;
    let mut seen = std::collections::BTreeSet::new();
    if models.len() > CATALOG_MODELS_MAX
        || !models
            .iter()
            .all(|id| model_valid(harness, id) && seen.insert(id.as_str()))
    {
        return Err(unavailable(()));
    }
    Ok(Some((models, reported)))
}

/// Lists every model id a saved slot of a harness names, at any scope: a master or agent
/// model, or an id on an allowed list. Inputs: the engine database and the harness id.
/// Output: the ids, or `model_choice_unavailable`.
fn saved_ids(
    db: &Connection,
    harness: &str,
) -> Result<std::collections::BTreeSet<String>, EngineError> {
    let mut ids = std::collections::BTreeSet::new();
    if !exists(db)? {
        return Ok(ids);
    }
    let mut query = db
        .prepare("SELECT value_json FROM model_choices WHERE harness = ?1")
        .map_err(unavailable)?;
    let mut rows = query.query([harness]).map_err(unavailable)?;
    while let Some(row) = rows.next().map_err(unavailable)? {
        let json: String = row.get(0).map_err(unavailable)?;
        if json.len() > VALUE_JSON_MAX {
            return Err(unavailable(()));
        }
        match serde_json::from_str::<Value>(&json).map_err(unavailable)? {
            Value::String(id) => {
                ids.insert(id);
            }
            Value::Array(items) => {
                ids.extend(items.into_iter().filter_map(|item| match item {
                    Value::String(id) => Some(id),
                    _ => None,
                }));
            }
            _ => {}
        }
    }
    Ok(ids)
}

/// Sets one slot at one scope as the next version of its item, when the owner read the
/// current one. Checks run in order: the harness, the project, whether the slot applies to
/// the harness, the value, then the version. Inputs: the engine database, the harness
/// name, its model catalog, the scope, the slot, the value and the version the owner read.
/// Output: Saved or Stale with the slot's row, Refused with the code and the row, or a
/// `MODEL_CHOICE_CODES` refusal.
pub fn set(
    db: &Connection,
    harness: &str,
    catalog: &[String],
    scope: &Scope,
    slot: Slot,
    value: &Value,
    version: u64,
) -> Result<Written, EngineError> {
    write(db, harness, catalog, scope, slot, Some(value), version)
}

/// Removes one slot's value at one scope as the next version of its item, so the slot
/// falls back to the app value or the account's own model. A reset is open on every
/// harness, so a value left from an earlier choice can always be cleared. Inputs and
/// output: as `set`, without a value.
pub fn reset(
    db: &Connection,
    harness: &str,
    catalog: &[String],
    scope: &Scope,
    slot: Slot,
    version: u64,
) -> Result<Written, EngineError> {
    write(db, harness, catalog, scope, slot, None, version)
}

/// Reads the model one start of a harness runs on by default: the project's master or
/// agent model, then the app's, none for the account's own model or a harness whose
/// model CrazyAgent does not choose. Inputs: the engine database, the harness name, its
/// model catalog, the project, none for an app-level start, and the master or agent slot.
/// Output: the model, or `model_choice_unavailable`.
pub fn launch_model(
    db: &Connection,
    harness: &str,
    catalog: &[String],
    project: Option<&ProjectId>,
    slot: Slot,
) -> Result<Option<String>, EngineError> {
    assert!(matches!(slot, Slot::Master | Slot::Agent), "{slot:?}");
    let Some(harness) = harness_id(harness).filter(|id| supported(id, slot)) else {
        return Ok(None);
    };
    assert_catalog(harness, catalog);
    let own = snapshot(db)?;
    let tx = own.as_deref().unwrap_or(db);
    let usable = usable(&candidates(tx, harness, catalog)?);
    let (value, _) = effective(tx, harness, project, slot, &usable)?;
    assert!(value.is_null() || value.is_string());
    Ok(value.as_str().map(str::to_owned))
}

/// Reads what a native lead's own sub-agents run on, the owner's Native sub-agent model
/// setting (DEC-150 M-11): the project's value, then the app's, else Follow the agent's
/// model. A harness without native sub-agents always follows. Inputs: the engine database,
/// the harness name and the project, none for an app-level start. Output: the setting, or
/// `model_choice_unavailable`.
pub fn native_model(
    db: &Connection,
    harness: &str,
    project: Option<&ProjectId>,
) -> Result<NativeModel, EngineError> {
    let Some(harness) = harness_id(harness).filter(|id| supported(id, Slot::Native)) else {
        return Ok(NativeModel::Follow);
    };
    let own = snapshot(db)?;
    let tx = own.as_deref().unwrap_or(db);
    let (value, _) = effective(tx, harness, project, Slot::Native, &[])?;
    let model = NativeModel::from_value(&value).expect("a layered native value passed value_ok");
    assert!(supported(harness, Slot::Native));
    Ok(model)
}

/// Checks the model the master or a lead picked for a new agent under the owner's pick
/// rule (DEC-150 Q2): no pick runs the project's agent default, and so does a pick equal
/// to it. Strict runs a pick on the allowed list, the project's when set, else the app's;
/// Hint runs any model the harness offers; Default overrules every other pick with the
/// default. Anything else is refused with the models the rule allows. A pick on a harness
/// whose model CrazyAgent does not choose is refused. Inputs: the engine database, the
/// agent's harness, its model catalog, the project and the pick. Output: the answer, or
/// `model_choice_unavailable`.
pub fn pick(
    db: &Connection,
    harness: &str,
    catalog: &[String],
    project: &ProjectId,
    picked: Option<&str>,
) -> Result<Pick, EngineError> {
    let Some(Terms {
        default,
        allowed,
        usable,
        rule,
    }) = terms(db, harness, catalog, project)?
    else {
        return Ok(match picked {
            None => Pick::Run(None),
            Some(_) => Pick::Unsupported,
        });
    };
    let offered = picked.is_some_and(|model| usable.iter().any(|id| id == model));
    let model = match picked {
        None => return Ok(Pick::Run(default)),
        Some(model) if default.as_deref() == Some(model) => return Ok(Pick::Run(default)),
        Some(model) => model,
    };
    Ok(match rule {
        PickRule::Default => Pick::Overruled(default),
        PickRule::Strict if allowed.iter().any(|id| id == model) => {
            Pick::Run(Some(model.to_owned()))
        }
        PickRule::Hint if offered => Pick::Run(Some(model.to_owned())),
        PickRule::Strict | PickRule::Hint => Pick::Refused { allowed, default },
    })
}

/// What a spawn may pick for a harness's agents in one project (DEC-150 F9): the agent
/// default, none for the account's own, and the ids the owner's pick rule lists, as
/// `pick` judges a pick; Default lists none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub default: Option<String>,
    pub allowed: Vec<String>,
}

/// Reads what a spawn may pick for a harness's agents in a project, as `pick` judges it
/// (DEC-150 F9). Inputs: the engine database, the harness name, its model catalog and the
/// project. Output: the offer, none for a harness whose model CrazyAgent does not choose,
/// or `model_choice_unavailable`.
pub fn offer(
    db: &Connection,
    harness: &str,
    catalog: &[String],
    project: &ProjectId,
) -> Result<Option<Offer>, EngineError> {
    Ok(terms(db, harness, catalog, project)?.map(|terms| Offer {
        default: terms.default,
        allowed: terms.allowed,
    }))
}

/// The terms a pick is judged by: the agent default, the ids the pick rule lists, every
/// usable id and the rule.
struct Terms {
    default: Option<String>,
    allowed: Vec<String>,
    usable: Vec<String>,
    rule: PickRule,
}

/// Reads the terms a pick for a harness's agents in a project is judged by, in one read
/// snapshot. Inputs: the engine database, the harness name, its catalog and the project.
/// Output: the terms, none for a harness whose model CrazyAgent does not choose, or
/// `model_choice_unavailable`.
fn terms(
    db: &Connection,
    harness: &str,
    catalog: &[String],
    project: &ProjectId,
) -> Result<Option<Terms>, EngineError> {
    let Some(harness) = harness_id(harness).filter(|id| supported(id, Slot::Agent)) else {
        return Ok(None);
    };
    assert_catalog(harness, catalog);
    let own = snapshot(db)?;
    let tx = own.as_deref().unwrap_or(db);
    let usable = usable(&candidates(tx, harness, catalog)?);
    let (default, _) = effective(tx, harness, Some(project), Slot::Agent, &usable)?;
    let default = default.as_str().map(str::to_owned);
    let (allowed, _) = effective(tx, harness, Some(project), Slot::Allowed, &usable)?;
    let allowed: Vec<String> = allowed
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    assert!(allowed.iter().all(|id| usable.contains(id)));
    let rule = super::rules::rules(tx)?.pick;
    let allowed: Vec<String> = match rule {
        PickRule::Strict => allowed,
        PickRule::Hint => usable.iter().take(super::ALLOWED_MAX).cloned().collect(),
        PickRule::Default => Vec::new(),
    };
    assert!(allowed.len() <= super::ALLOWED_MAX);
    Ok(Some(Terms {
        default,
        allowed,
        usable,
        rule,
    }))
}

/// Adds one model id the owner typed for a harness with a test launch, waiting for it,
/// unless the harness's catalog lists it or it already passed. A failed id may be tried
/// again. Inputs: the engine database, the harness name, its model catalog and the id.
/// Output: what was done, or `model_choice_not_supported`, `model_choice_value_invalid`,
/// `model_choice_custom_full`, `model_choice_harness_unknown` or `model_choice_unavailable`.
pub fn custom_begin(
    db: &Connection,
    harness: &str,
    catalog: &[String],
    model: &str,
) -> Result<Begun, EngineError> {
    let harness = known(harness)?;
    if !super::test_launch::supported(harness) {
        return Err(failure("model_choice_not_supported"));
    }
    assert_catalog(harness, catalog);
    if !model_valid(harness, model) {
        return Err(failure("model_choice_value_invalid"));
    }
    if catalog.iter().any(|id| id == model) {
        return Ok(Begun::Listed);
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    tx.execute_batch(SCHEMA).map_err(unavailable)?;
    let state: Option<String> = tx
        .query_row(
            "SELECT state FROM model_custom WHERE harness = ?1 AND model = ?2",
            params![harness, model],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    if state.as_deref() == Some("verified") {
        return Ok(Begun::Verified);
    }
    let held: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM model_custom WHERE harness = ?1",
            [harness],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    let held = usize::try_from(held).map_err(unavailable)?;
    if state.is_none() && held >= CUSTOM_MAX {
        return Err(failure("model_choice_custom_full"));
    }
    let changed = tx
        .execute(
            "INSERT INTO model_custom(harness, model, state, checked_ms)
             VALUES (?1, ?2, 'pending', ?3)
             ON CONFLICT(harness, model)
             DO UPDATE SET state = 'pending', checked_ms = excluded.checked_ms",
            params![harness, model, crate::team::now_ms()],
        )
        .map_err(unavailable)?;
    assert_eq!(changed, 1);
    tx.commit().map_err(unavailable)?;
    Ok(Begun::Pending)
}

/// Records the test launch of one pending owner id: passed makes it usable, failed keeps
/// it shown as failed and never usable. Inputs: the engine database, the harness name,
/// the id and whether the launch passed. Output: Ok, or `model_choice_custom_not_pending`
/// for an id that is not waiting, `model_choice_harness_unknown` or
/// `model_choice_unavailable`.
pub fn custom_checked(
    db: &Connection,
    harness: &str,
    model: &str,
    passed: bool,
) -> Result<(), EngineError> {
    let harness = known(harness)?;
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    tx.execute_batch(SCHEMA).map_err(unavailable)?;
    let state = if passed { "verified" } else { "failed" };
    let changed = tx
        .execute(
            "UPDATE model_custom SET state = ?3, checked_ms = ?4
             WHERE harness = ?1 AND model = ?2 AND state = 'pending'",
            params![harness, model, state, crate::team::now_ms()],
        )
        .map_err(unavailable)?;
    assert!(changed <= 1);
    if changed == 0 {
        return Err(failure("model_choice_custom_not_pending"));
    }
    tx.commit().map_err(unavailable)
}

/// Tells whether one owner id waits for its test launch, so a test never spends a run on
/// an id that is listed, verified, failed or gone. Inputs: the engine database, the
/// harness name and the id. Output: true when it is pending, or
/// `model_choice_harness_unknown` or `model_choice_unavailable`.
pub fn custom_pending(db: &Connection, harness: &str, model: &str) -> Result<bool, EngineError> {
    let harness = known(harness)?;
    if !exists(db)? {
        return Ok(false);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM model_custom
         WHERE harness = ?1 AND model = ?2 AND state = 'pending')",
        params![harness, model],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

/// Removes one owner id from a harness. A slot that names it passes it over from then on
/// and shows it with its reset open. Inputs: the engine database, the harness name and
/// the id. Output: true when it was there, or `model_choice_harness_unknown` or
/// `model_choice_unavailable`.
pub fn custom_remove(db: &Connection, harness: &str, model: &str) -> Result<bool, EngineError> {
    let harness = known(harness)?;
    if !exists(db)? {
        return Ok(false);
    }
    let changed = db
        .execute(
            "DELETE FROM model_custom WHERE harness = ?1 AND model = ?2",
            params![harness, model],
        )
        .map_err(unavailable)?;
    assert!(changed <= 1);
    Ok(changed == 1)
}

fn write(
    db: &Connection,
    harness: &str,
    catalog: &[String],
    scope: &Scope,
    slot: Slot,
    value: Option<&Value>,
    version: u64,
) -> Result<Written, EngineError> {
    let harness = known(harness)?;
    assert_catalog(harness, catalog);
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    project_known(&tx, scope)?;
    let usable = usable(&candidates(&tx, harness, catalog)?);
    if value.is_some() && !supported(harness, slot) {
        return Ok(Written::Refused {
            code: "model_choice_not_supported".into(),
            row: row(&tx, harness, scope, slot, &usable)?,
        });
    }
    if let Some(value) = value
        && !value_ok(harness, slot, value, &usable)
    {
        return Err(failure("model_choice_value_invalid"));
    }
    let item = item(harness, scope, slot)?;
    let next = match versioned::owner_write(&tx, &item, version)? {
        Ok(next) => next,
        Err(_) => {
            return Ok(Written::Stale {
                row: row(&tx, harness, scope, slot, &usable)?,
            });
        }
    };
    tx.execute_batch(SCHEMA).map_err(unavailable)?;
    let (scope_text, project) = columns(scope);
    let changed = match value {
        Some(value) => {
            let json = serde_json::to_string(value).map_err(unavailable)?;
            assert!(json.len() <= VALUE_JSON_MAX);
            tx.execute(
                "INSERT INTO model_choices(scope, project, harness, slot, value_json, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(scope, project, harness, slot)
                 DO UPDATE SET value_json = excluded.value_json, updated_ms = excluded.updated_ms",
                params![
                    scope_text,
                    project,
                    harness,
                    slot.as_str(),
                    json,
                    crate::team::now_ms()
                ],
            )
            .map_err(unavailable)?
        }
        None => tx
            .execute(
                "DELETE FROM model_choices
                 WHERE scope = ?1 AND project = ?2 AND harness = ?3 AND slot = ?4",
                params![scope_text, project, harness, slot.as_str()],
            )
            .map_err(unavailable)?,
    };
    assert!(changed <= 1);
    assert!(value.is_none() || changed == 1);
    let row = row(&tx, harness, scope, slot, &usable)?;
    assert_eq!(row.version, next);
    assert_eq!(row.here.as_ref(), value);
    tx.commit().map_err(unavailable)?;
    Ok(Written::Saved { row })
}

/// Builds one slot's row at one scope: the value stored here, the value a start uses and
/// where it comes from, its version, and read-only with `NOT_SUPPORTED` on a harness whose
/// model CrazyAgent does not choose in that slot.
fn row(
    db: &Connection,
    harness: &str,
    scope: &Scope,
    slot: Slot,
    usable: &[String],
) -> Result<Row, EngineError> {
    let version = versioned::current(db, &item(harness, scope, slot)?)?.version;
    let here = stored(db, harness, scope, slot)?;
    if !supported(harness, slot) {
        return Ok(Row {
            slot,
            here,
            effective: fallback(slot),
            source: Source::NotApplied,
            version,
            access: Access::ReadOnly {
                reason: NOT_SUPPORTED.into(),
            },
        });
    }
    let project = match scope {
        Scope::App => None,
        Scope::Project(project) => Some(project),
    };
    let (effective, source) = effective(db, harness, project, slot, usable)?;
    Ok(Row {
        slot,
        here,
        effective,
        source,
        version,
        access: Access::Editable,
    })
}

fn effective(
    db: &Connection,
    harness: &str,
    project: Option<&ProjectId>,
    slot: Slot,
    usable: &[String],
) -> Result<(Value, Source), EngineError> {
    let app = stored(db, harness, &Scope::App, slot)?;
    let overrides = match project {
        Some(project) => stored(db, harness, &Scope::Project(project.clone()), slot)?,
        None => None,
    };
    Ok(layered(
        harness,
        slot,
        usable,
        app.as_ref(),
        overrides.as_ref(),
    ))
}

fn stored(
    db: &Connection,
    harness: &str,
    scope: &Scope,
    slot: Slot,
) -> Result<Option<Value>, EngineError> {
    if !exists(db)? {
        return Ok(None);
    }
    let (scope_text, project) = columns(scope);
    let json: Option<String> = db
        .query_row(
            "SELECT value_json FROM model_choices
             WHERE scope = ?1 AND project = ?2 AND harness = ?3 AND slot = ?4",
            params![scope_text, project, harness, slot.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    match json {
        None => Ok(None),
        Some(json) if json.len() <= VALUE_JSON_MAX => {
            serde_json::from_str(&json).map(Some).map_err(unavailable)
        }
        Some(_) => Err(unavailable(())),
    }
}

/// Lists a harness's candidate ids: the catalog's in its order, then at most `CUSTOM_MAX`
/// owner ids in id order that the catalog does not list.
fn candidates(
    db: &Connection,
    harness: &str,
    catalog: &[String],
) -> Result<Vec<Candidate>, EngineError> {
    let mut candidates: Vec<Candidate> = catalog
        .iter()
        .map(|id| Candidate {
            id: id.clone(),
            origin: Origin::Catalog,
        })
        .collect();
    if !exists(db)? {
        return Ok(candidates);
    }
    let mut query = db
        .prepare("SELECT model, state FROM model_custom WHERE harness = ?1 ORDER BY model LIMIT ?2")
        .map_err(unavailable)?;
    let limit = i64::try_from(CUSTOM_MAX).map_err(unavailable)?;
    let mut rows = query.query(params![harness, limit]).map_err(unavailable)?;
    while let Some(row) = rows.next().map_err(unavailable)? {
        let id: String = row.get(0).map_err(unavailable)?;
        let state: String = row.get(1).map_err(unavailable)?;
        let origin = match state.as_str() {
            "pending" => Origin::Pending,
            "verified" => Origin::Verified,
            "failed" => Origin::Failed,
            _ => return Err(unavailable(())),
        };
        if !model_valid(harness, &id) {
            return Err(unavailable(()));
        }
        if catalog.contains(&id) {
            continue;
        }
        candidates.push(Candidate { id, origin });
    }
    assert!(candidates.len() <= catalog.len() + CUSTOM_MAX);
    Ok(candidates)
}

fn usable(candidates: &[Candidate]) -> Vec<String> {
    candidates
        .iter()
        .filter(|candidate| candidate.origin.usable())
        .map(|candidate| candidate.id.clone())
        .collect()
}

/// Opens a deferred transaction for one read snapshot, none when the caller already holds
/// one, so a spawn checks its pick inside the transaction that admits its task. Input: the
/// connection. Output: the transaction, or `model_choice_unavailable`.
fn snapshot(db: &Connection) -> Result<Option<Transaction<'_>>, EngineError> {
    if !db.is_autocommit() {
        return Ok(None);
    }
    Transaction::new_unchecked(db, TransactionBehavior::Deferred)
        .map(Some)
        .map_err(unavailable)
}

fn known(harness: &str) -> Result<&'static str, EngineError> {
    harness_id(harness).ok_or_else(|| failure("model_choice_harness_unknown"))
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
        Err(failure("model_choice_project_unknown"))
    }
}

fn columns(scope: &Scope) -> (&'static str, &str) {
    match scope {
        Scope::App => ("app", ""),
        Scope::Project(project) => ("project", project.as_str()),
    }
}

/// The most harnesses one removed project may hold model choices for; more means a
/// broken store.
const FORGOTTEN_HARNESSES_MAX: usize = 64;

/// Clears a removed project's model choices (DEC-150 F6): its project-scope values go and
/// every slot item of each harness it ever saved moves on a version, so a project added
/// again under the same id inherits nothing and a screen read before cannot save onto it.
/// Takes the removal's immediate transaction and the project. Returns
/// `model_choice_unavailable` when the store cannot be read or written.
pub fn forget_project(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    if exists(tx)? {
        tx.execute(
            "DELETE FROM model_choices WHERE scope = 'project' AND project = ?1",
            [project.as_str()],
        )
        .map_err(unavailable)?;
    }
    let versions: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table'
             AND name = 'versioned_items')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !versions {
        return Ok(());
    }
    let marker = format!(":project:{}:", project.as_str());
    let items: Vec<String> = tx
        .prepare(
            "SELECT item FROM versioned_items
             WHERE substr(item, 1, length(?1)) = ?1 AND instr(item, ?2) > 0",
        )
        .and_then(|mut statement| {
            statement
                .query_map(params![format!("{}:", super::ITEM_PREFIX), marker], |row| {
                    row.get(0)
                })?
                .collect()
        })
        .map_err(unavailable)?;
    let scope = Scope::Project(project.clone());
    let mut harnesses = std::collections::BTreeSet::new();
    for item in &items {
        let Some((harness, tail)) = item
            .strip_prefix(super::ITEM_PREFIX)
            .and_then(|rest| rest.strip_prefix(':'))
            .and_then(|rest| rest.split_once(':'))
        else {
            continue;
        };
        if Slot::ALL
            .iter()
            .any(|slot| tail == format!("project:{}:{}", project.as_str(), slot.as_str()))
        {
            harnesses.insert(harness.to_owned());
        }
    }
    assert!(harnesses.len() <= FORGOTTEN_HARNESSES_MAX);
    for harness in &harnesses {
        for slot in Slot::ALL {
            versioned::retire(tx, &item(harness, &scope, slot)?)?;
        }
    }
    Ok(())
}

fn exists(db: &Connection) -> Result<bool, EngineError> {
    table(db, "model_custom")
}

fn table(db: &Connection, name: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )
    .map_err(unavailable)
}
