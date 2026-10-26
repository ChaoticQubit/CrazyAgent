use super::*;
use crate::accounts::AccountDetails;
use crate::config::{ModelAccessEdit, ModelAuthentication, ModelConnection};
use crate::engine::owner::native_auth::NativeAuthPhase;
use serde::{Deserialize, Serialize};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS accounts (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE CHECK(length(id) BETWEEN 1 AND 128),
    body BLOB NOT NULL CHECK(length(body) <= 16384)
)";
const REGISTRY_MAX: i64 = 1_000_000;

pub(crate) enum Command {
    Add(Box<ModelConnection>, AccountDetails),
    Rename(CredentialRef, String),
    Remove(CredentialRef),
    List(Option<CredentialRef>, usize),
    Get(CredentialRef),
}

pub(crate) struct Listed {
    pub(crate) connection: ModelConnection,
    pub(crate) details: AccountDetails,
    pub(crate) sign_in: Option<NativeAuthPhase>,
    pub(crate) launchable: bool,
}

pub(crate) enum ResultValue {
    Account(Box<Listed>),
    Removed(Box<ModelConnection>),
    Page(Vec<Listed>, Option<CredentialRef>),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    connection: ModelConnection,
    details: AccountDetails,
}

fn encode(record: &Record) -> Result<Vec<u8>, EngineError> {
    let body = serde_json::to_vec(record).map_err(error)?;
    if body.len() > 16384 {
        return Err(error("account_record_bounds"));
    }
    Ok(body)
}

fn decode(id: &str, body: &[u8]) -> Result<Record, EngineError> {
    let record: Record =
        serde_json::from_slice(body).map_err(|_| error("account_record_invalid"))?;
    if record.connection.id.as_str() != id {
        return Err(error("account_record_invalid"));
    }
    Ok(record)
}

fn registry_exists(db: &Connection) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='accounts')",
        [],
        |row| row.get(0),
    )
    .map_err(error)
}

fn registered(db: &Connection, id: &CredentialRef) -> Result<Option<(i64, Record)>, EngineError> {
    if !registry_exists(db)? {
        return Ok(None);
    }
    db.query_row(
        "SELECT seq, body FROM accounts WHERE id=?1",
        [id.as_str()],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
    )
    .optional()
    .map_err(error)?
    .map(|(seq, body)| decode(id.as_str(), &body).map(|record| (seq, record)))
    .transpose()
}

/// Lists saved accounts that a pool edit names but the config does not hold yet, so the
/// same settings publication can add them before saving the pool. Inputs: database, the
/// current config and the edit. Output: the connections to add, in member order.
pub(super) fn pool_materialization(
    db: &Connection,
    config: &EffectiveConfig,
    edit: &ModelAccessEdit,
) -> Result<Vec<ModelConnection>, EngineError> {
    let pool = match edit {
        ModelAccessEdit::CreatePool { pool } | ModelAccessEdit::ReplacePool { pool } => pool,
        _ => return Ok(Vec::new()),
    };
    let held = config
        .settings
        .model_access
        .as_ref()
        .map(|catalog| &catalog.connections);
    let mut found = Vec::new();
    for member in &pool.members {
        if held.is_some_and(|connections| connections.iter().any(|c| c.id == *member)) {
            continue;
        }
        if let Some((_, record)) = registered(db, member)? {
            found.push(record.connection);
        }
    }
    assert!(found.len() <= pool.members.len());
    Ok(found)
}

/// Runs one Settings > Accounts command in the storage actor. Saved accounts live in the
/// `accounts` table; the config holds only accounts routing needs (subscriptions and pool
/// members). Inputs: database, live config, writable sources, owner admission, command.
/// Output: the changed or listed accounts with their latest managed sign-in phase.
pub(super) fn execute(
    db: &mut Connection,
    config: &mut EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    admission: &Admission,
    command: Command,
) -> Result<ResultValue, EngineError> {
    db.execute_batch(SCHEMA).map_err(error)?;
    match command {
        Command::List(after, limit) => list(db, config, admission, after.as_ref(), limit),
        Command::Get(id) => {
            let tx = db.transaction().map_err(error)?;
            check_owner_admission(&tx, admission)?;
            let (connection, details) =
                find(&tx, config, &id)?.ok_or_else(|| error("account_not_found"))?;
            Ok(ResultValue::Account(Box::new(listed(
                &tx, admission, connection, details,
            )?)))
        }
        Command::Add(connection, details) => {
            {
                let tx = db.transaction().map_err(error)?;
                check_owner_admission(&tx, admission)?;
                let count: i64 = tx
                    .query_row("SELECT count(*) FROM accounts", [], |row| row.get(0))
                    .map_err(error)?;
                if count >= REGISTRY_MAX {
                    return Err(error("accounts_full"));
                }
            }
            if matches!(connection.auth, ModelAuthentication::NativeManaged { .. }) {
                publish(
                    db,
                    config,
                    sources,
                    admission,
                    ModelAccessEdit::AddConnectedAccount {
                        connection: (*connection).clone(),
                    },
                )?;
            }
            let record = Record {
                connection: *connection,
                details,
            };
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(error)?;
            check_owner_admission(&tx, admission)?;
            tx.execute(
                "INSERT INTO accounts(id, body) VALUES (?1, ?2)",
                params![record.connection.id.as_str(), encode(&record)?],
            )
            .map_err(|_| error("account_exists"))?;
            let account = listed(&tx, admission, record.connection, record.details)?;
            tx.commit().map_err(error)?;
            Ok(ResultValue::Account(Box::new(account)))
        }
        Command::Rename(id, name) => {
            let name = crate::accounts::account_name(&name)?;
            if name.is_empty() {
                return Err(error("account_name_invalid"));
            }
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(error)?;
            check_owner_admission(&tx, admission)?;
            let (connection, mut details) =
                find(&tx, config, &id)?.ok_or_else(|| error("account_not_found"))?;
            details.name = name;
            let record = Record {
                connection,
                details,
            };
            tx.execute(
                "INSERT INTO accounts(id, body) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
                params![id.as_str(), encode(&record)?],
            )
            .map_err(error)?;
            let account = listed(&tx, admission, record.connection, record.details)?;
            tx.commit().map_err(error)?;
            Ok(ResultValue::Account(Box::new(account)))
        }
        Command::Remove(id) => {
            let (connection, _) = {
                let tx = db.transaction().map_err(error)?;
                check_owner_admission(&tx, admission)?;
                find(&tx, config, &id)?.ok_or_else(|| error("account_not_found"))?
            };
            if held(config, &id) {
                publish(
                    db,
                    config,
                    sources,
                    admission,
                    ModelAccessEdit::RemoveConnectedAccount { id: id.clone() },
                )?;
            }
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(error)?;
            check_owner_admission(&tx, admission)?;
            tx.execute("DELETE FROM accounts WHERE id=?1", [id.as_str()])
                .map_err(error)?;
            tx.commit().map_err(error)?;
            Ok(ResultValue::Removed(Box::new(connection)))
        }
    }
}

fn held(config: &EffectiveConfig, id: &CredentialRef) -> bool {
    config
        .settings
        .model_access
        .as_ref()
        .is_some_and(|catalog| catalog.connections.iter().any(|c| c.id == *id))
}

fn fallback(connection: &ModelConnection) -> AccountDetails {
    let provider = crate::accounts::provider_of(connection);
    AccountDetails {
        name: crate::accounts::provider_name(provider).into(),
        provider,
    }
}

fn find(
    db: &Connection,
    config: &EffectiveConfig,
    id: &CredentialRef,
) -> Result<Option<(ModelConnection, AccountDetails)>, EngineError> {
    let current = config
        .settings
        .model_access
        .as_ref()
        .and_then(|catalog| catalog.connections.iter().find(|c| c.id == *id).cloned());
    Ok(match (registered(db, id)?, current) {
        (Some((_, record)), Some(connection)) => Some((connection, record.details)),
        (Some((_, record)), None) => Some((record.connection, record.details)),
        (None, Some(connection)) => {
            let details = fallback(&connection);
            Some((connection, details))
        }
        (None, None) => None,
    })
}

fn publish(
    db: &mut Connection,
    config: &mut EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    admission: &Admission,
    edit: ModelAccessEdit,
) -> Result<(), EngineError> {
    let request = {
        let tx = db.transaction().map_err(error)?;
        check_owner_admission(&tx, admission)?;
        let revision = owner_revision(&tx)?;
        let source_hash = settings::verified_source_hash(config, sources)?;
        OwnerRequest {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            request_id: RequestId::new(crate::engine::owner::random_id()?).map_err(error)?,
            expected_revision: Cursor::new(revision),
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::ModelAccess {
                    expected_config_id: config.id.clone(),
                    expected_source_hash: source_hash,
                    edit,
                },
            },
        }
    };
    let applied = settings::apply(db, config, sources, admission, &request)?;
    if let OwnerResult::Rejected { code } = applied.result {
        return Err(error(code));
    }
    Ok(())
}

fn list(
    db: &mut Connection,
    config: &EffectiveConfig,
    admission: &Admission,
    after: Option<&CredentialRef>,
    limit: usize,
) -> Result<ResultValue, EngineError> {
    if !(1..=crate::accounts::ACCOUNTS_PAGE_MAX).contains(&limit) {
        return Err(error("accounts_page_limit"));
    }
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, admission)?;
    let catalog = config.settings.model_access.clone().unwrap_or_default();
    let mut unregistered = Vec::new();
    for connection in &catalog.connections {
        if registered(&tx, &connection.id)?.is_none() {
            unregistered.push(connection);
        }
    }
    let (skip, after_seq) = match after {
        None => (0, 0),
        Some(id) => match unregistered.iter().position(|c| c.id == *id) {
            Some(index) => (index + 1, 0),
            None => (
                unregistered.len(),
                registered(&tx, id)?
                    .ok_or_else(|| error("accounts_cursor_stale"))?
                    .0,
            ),
        },
    };
    let mut items = Vec::with_capacity(limit);
    for connection in unregistered.iter().skip(skip).take(limit) {
        items.push(listed(
            &tx,
            admission,
            (*connection).clone(),
            fallback(connection),
        )?);
    }
    let wanted = limit - items.len();
    let mut more = skip + items.len() < unregistered.len();
    if wanted > 0 {
        let mut statement = tx
            .prepare("SELECT id, body FROM accounts WHERE seq>?1 ORDER BY seq LIMIT ?2")
            .map_err(error)?;
        let rows = statement
            .query_map(params![after_seq, wanted as i64 + 1], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        more = rows.len() > wanted;
        for (id, body) in rows.into_iter().take(wanted) {
            let record = decode(&id, &body)?;
            let connection = catalog
                .connections
                .iter()
                .find(|c| c.id == record.connection.id)
                .cloned()
                .unwrap_or(record.connection);
            items.push(listed(&tx, admission, connection, record.details)?);
        }
    }
    let next = if more {
        items.last().map(|item| item.connection.id.clone())
    } else {
        None
    };
    assert!(items.len() <= limit);
    Ok(ResultValue::Page(items, next))
}

/// Lists one account with its latest sign-in and whether that sign-in passes the launch
/// check every master, team and research turn uses on this engine run, so status never
/// shows an account as signed in when a launch would refuse it. Inputs: the transaction,
/// the admitted owner, the connection and its details. Output: the listed account.
fn listed(
    tx: &Transaction<'_>,
    admission: &Admission,
    connection: ModelConnection,
    details: AccountDetails,
) -> Result<Listed, EngineError> {
    let sign_in = match &connection.auth {
        ModelAuthentication::NativeManaged { profile_ref } => latest_sign_in(tx, profile_ref)?,
        _ => None,
    };
    let host = HostStamp {
        db_instance: admission.db_instance.clone(),
        engine_epoch: admission.engine_epoch.clone(),
    };
    let launchable = sign_in == Some(NativeAuthPhase::AuthenticatedObserved)
        && crate::storage::native_sign_in(tx, &host, &connection, None).is_ok();
    assert!(!launchable || sign_in == Some(NativeAuthPhase::AuthenticatedObserved));
    Ok(Listed {
        connection,
        details,
        sign_in,
        launchable,
    })
}

fn latest_sign_in(
    tx: &Connection,
    profile_ref: &CredentialRef,
) -> Result<Option<NativeAuthPhase>, EngineError> {
    let exists: bool = tx
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='native_auth_operations')", [], |row| row.get(0))
        .map_err(error)?;
    if !exists {
        return Ok(None);
    }
    let key = crate::memory::hash(profile_ref.as_str().as_bytes());
    let row: Option<Vec<u8>> = tx
        .query_row(
            "SELECT fact FROM native_auth_operations WHERE profile_key=?1 ORDER BY generation DESC LIMIT 1",
            [&key],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    row.map(|bytes| crate::engine::owner::native_auth::decode_fact(&bytes).map(|fact| fact.phase))
        .transpose()
}
