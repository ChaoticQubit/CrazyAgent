use super::*;
use crate::config::{ModelAuthentication, ModelConnection};
use crate::engine::bootstrap::NativeProfileUse;
use crate::engine::owner::account_profile::authorize_account_profile_scope;

pub(crate) enum Command {
    Reserve(AccountProfileRequest, bool),
    Apply(AccountProfileRequest, Option<AccountSecret>),
    Read(AccountProfileRequest),
    Capabilities(AccountProfileRequest),
    Finish(Box<Work>, bool),
    Abandon(AccountProfileRequest),
}

pub(crate) enum ResultValue {
    Accepted,
    Profile(AccountProfileReply, bool),
    Capabilities(AccountAuthCapabilitiesReply),
    Work(Box<Work>),
}

pub(crate) struct Work {
    pub(crate) admission: Admission,
    pub(crate) request: AccountProfileRequest,
    pub(crate) receipt: AccountProfileReceipt,
    pub(crate) guard: NativeProfileUse,
    pub(crate) secret: Option<AccountSecret>,
}

struct Record {
    request: Vec<u8>,
    owner_epoch: i64,
    engine_epoch: String,
    phase: String,
    receipt: Option<Vec<u8>>,
}

pub(crate) fn execute(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    admission: &Admission,
    command: Command,
) -> Result<ResultValue, EngineError> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS account_profile_operations (owner_id TEXT NOT NULL, request_id TEXT NOT NULL, owner_epoch INTEGER NOT NULL, engine_epoch TEXT NOT NULL, request BLOB NOT NULL, phase TEXT NOT NULL CHECK(phase IN ('reserved','running','committed','not_applied','uncertain')), receipt BLOB, PRIMARY KEY(owner_id,request_id))").map_err(error)?;
    let request = match &command {
        Command::Reserve(request, _)
        | Command::Apply(request, _)
        | Command::Read(request)
        | Command::Capabilities(request)
        | Command::Abandon(request) => request,
        Command::Finish(work, _) => &work.request,
    };
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, admission)?;
    settings::ensure_clear(&tx)?;
    let connection = connection(config, sources, admission, request)?.clone();
    if matches!(&command, Command::Capabilities(_)) {
        let reply = AccountAuthCapabilitiesReply {
            request_id: request.request_id.clone(),
            database_instance: admission.db_instance.clone(),
            engine_epoch: admission.engine_epoch.clone(),
            config_id: config.id.clone(),
            source_hash: request.scope.expected_source_hash.clone(),
            connection,
            managed: Vec::new(),
            configured: ProducerCapability::Unavailable {
                code: "account_profile_harness_unsupported".into(),
            },
        };
        tx.commit().map_err(error)?;
        return Ok(ResultValue::Capabilities(reply));
    }
    authorize_account_profile_scope(
        &request.scope,
        &admission.db_instance,
        &config.id,
        &request.scope.expected_source_hash,
        &connection,
    )?;
    let request_bytes = serde_json::to_vec(request).map_err(error)?;
    if request_bytes.len() > 8192 {
        return Err(error("account_profile_request_bounds"));
    }
    let recorded = record(&tx, admission, request)?;
    if let Some(record) = &recorded {
        if record.request != request_bytes || record.owner_epoch != admission.owner_epoch {
            return Err(error("account_profile_request_conflict"));
        }
        if record.phase == "committed" {
            let receipt: AccountProfileReceipt = serde_json::from_slice(
                record
                    .receipt
                    .as_deref()
                    .ok_or_else(|| error("account_profile_record_invalid"))?,
            )
            .map_err(|_| error("account_profile_record_invalid"))?;
            let reply = reply(
                admission,
                request,
                AccountProfileOutcome::ConfiguredOnly {
                    receipt: Box::new(receipt),
                    historical_replay: true,
                },
            );
            tx.commit().map_err(error)?;
            return Ok(ResultValue::Profile(reply, true));
        }
    }
    match command {
        Command::Read(request) => {
            let settled = recorded
                .as_ref()
                .is_some_and(|record| record.phase == "not_applied");
            if recorded
                .as_ref()
                .is_some_and(|record| record.phase != "not_applied")
            {
                return Err(error("account_profile_outcome_unknown"));
            }
            tx.commit().map_err(error)?;
            Ok(ResultValue::Profile(
                reply(admission, &request, AccountProfileOutcome::NotRecorded),
                settled,
            ))
        }
        Command::Reserve(request, has_secret) => {
            if recorded.is_some() {
                return Err(error("account_profile_outcome_unknown"));
            }
            if matches!(connection.auth, ModelAuthentication::ApiKey { .. }) != has_secret {
                return Err(error("account_profile_secret_required_or_unexpected"));
            }
            let count: i64 = tx
                .query_row(
                    "SELECT count(*) FROM account_profile_operations",
                    [],
                    |row| row.get(0),
                )
                .map_err(error)?;
            let count = u64::try_from(count).map_err(error)?;
            if count >= 1024 {
                return Err(error("account_profile_operations_full"));
            }
            tx.execute("INSERT INTO account_profile_operations(owner_id,request_id,owner_epoch,engine_epoch,request,phase) VALUES(?1,?2,?3,?4,?5,'reserved')",
                params![admission.owner_id,request.request_id.as_str(),admission.owner_epoch,admission.engine_epoch,request_bytes]).map_err(error)?;
            tx.commit().map_err(error)?;
            Ok(ResultValue::Accepted)
        }
        Command::Apply(request, secret) => {
            let record = recorded.ok_or_else(|| error("account_profile_reservation_missing"))?;
            if record.phase != "reserved" || record.engine_epoch != admission.engine_epoch {
                return Err(error("account_profile_outcome_unknown"));
            }
            if matches!(connection.auth, ModelAuthentication::ApiKey { .. }) != secret.is_some() {
                return Err(error("account_profile_secret_required_or_unexpected"));
            }
            tx.execute("UPDATE account_profile_operations SET phase='running' WHERE owner_id=?1 AND request_id=?2",
                params![admission.owner_id,request.request_id.as_str()]).map_err(error)?;
            let revision = owner_revision(&tx)?;
            tx.commit().map_err(error)?;
            let work = prepare(
                custody, admission, &request, config, connection, revision, secret,
            );
            match work {
                Ok(work) => Ok(ResultValue::Work(Box::new(work))),
                Err(failure) => {
                    db.execute("UPDATE account_profile_operations SET phase='uncertain' WHERE owner_id=?1 AND request_id=?2",
                        params![admission.owner_id,request.request_id.as_str()]).map_err(error)?;
                    Err(failure)
                }
            }
        }
        Command::Finish(work, succeeded) => {
            if &work.admission != admission || work.receipt.connection != connection {
                return Err(error("account_profile_actor_changed"));
            }
            let record = recorded.ok_or_else(|| error("account_profile_reservation_missing"))?;
            if record.phase != "running" || record.engine_epoch != admission.engine_epoch {
                return Err(error("account_profile_outcome_unknown"));
            }
            if !succeeded || work.guard.revalidate_retained().is_err() {
                tx.execute("UPDATE account_profile_operations SET phase='uncertain' WHERE owner_id=?1 AND request_id=?2",
                    params![admission.owner_id,work.request.request_id.as_str()]).map_err(error)?;
                tx.commit().map_err(error)?;
                return Err(error("account_profile_outcome_unknown"));
            }
            let bytes = serde_json::to_vec(&work.receipt).map_err(error)?;
            if bytes.len() > 16384 {
                return Err(error("account_profile_receipt_bounds"));
            }
            tx.execute("UPDATE account_profile_operations SET phase='committed',receipt=?3 WHERE owner_id=?1 AND request_id=?2",
                params![admission.owner_id,work.request.request_id.as_str(),bytes]).map_err(error)?;
            tx.commit().map_err(error)?;
            Ok(ResultValue::Profile(
                reply(
                    admission,
                    &work.request,
                    AccountProfileOutcome::ConfiguredOnly {
                        receipt: Box::new(work.receipt),
                        historical_replay: false,
                    },
                ),
                true,
            ))
        }
        Command::Abandon(request) => {
            if recorded
                .as_ref()
                .is_some_and(|record| record.phase == "reserved")
            {
                tx.execute("UPDATE account_profile_operations SET phase='not_applied' WHERE owner_id=?1 AND request_id=?2 AND phase='reserved'",
                    params![admission.owner_id,request.request_id.as_str()]).map_err(error)?;
            }
            tx.commit().map_err(error)?;
            Ok(ResultValue::Accepted)
        }
        Command::Capabilities(_) => unreachable!(),
    }
}

fn connection<'a>(
    config: &'a EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    admission: &Admission,
    request: &AccountProfileRequest,
) -> Result<&'a ModelConnection, EngineError> {
    if request.scope.expected_database_instance != admission.db_instance {
        return Err(error("native_auth_database_mismatch"));
    }
    let hash = settings::verified_source_hash(config, sources)?;
    if request.scope.expected_config_id != config.id || request.scope.expected_source_hash != hash {
        return Err(error("account_profile_config_changed"));
    }
    let catalog = config
        .settings
        .model_access
        .as_ref()
        .ok_or_else(|| error("account_profile_connection_changed"))?;
    let selected = catalog
        .connections
        .iter()
        .find(|item| item.id == request.scope.connection)
        .ok_or_else(|| error("account_profile_connection_changed"))?;
    if !catalog.accounts.contains(&selected.account_ref) {
        return Err(error("account_profile_connection_changed"));
    }
    Ok(selected)
}

fn record(
    tx: &Transaction<'_>,
    admission: &Admission,
    request: &AccountProfileRequest,
) -> Result<Option<Record>, EngineError> {
    tx.query_row("SELECT request,owner_epoch,engine_epoch,phase,receipt FROM account_profile_operations WHERE owner_id=?1 AND request_id=?2",
        params![admission.owner_id,request.request_id.as_str()], |row| Ok(Record {
            request: row.get(0)?, owner_epoch: row.get(1)?, engine_epoch: row.get(2)?,
            phase: row.get(3)?, receipt: row.get(4)?,
        })).optional().map_err(error)
}

fn reply(
    admission: &Admission,
    request: &AccountProfileRequest,
    outcome: AccountProfileOutcome,
) -> AccountProfileReply {
    AccountProfileReply {
        request_id: request.request_id.clone(),
        database_instance: admission.db_instance.clone(),
        engine_epoch: admission.engine_epoch.clone(),
        outcome,
    }
}

/// Returns the custody profile reference that account setup generates for an RCLI connection.
/// Input: the configured connection. Output: its API-key credential reference, or a stable
/// digest-derived reference for an unauthenticated local endpoint; other auth is rejected.
pub(super) fn configured_profile_ref(
    connection: &ModelConnection,
) -> Result<CredentialRef, EngineError> {
    crate::config::configured_profile_ref(connection)
        .ok_or_else(|| error("account_profile_auth_unsupported"))
}

fn prepare(
    custody: &mut Result<NativeProfileCustody, EngineError>,
    admission: &Admission,
    request: &AccountProfileRequest,
    config: &EffectiveConfig,
    connection: ModelConnection,
    revision: u64,
    secret: Option<AccountSecret>,
) -> Result<Work, EngineError> {
    let profile = configured_profile_ref(&connection)?;
    let custody = custody.as_mut().map_err(|failure| error(&failure.0))?;
    let host = HostStamp {
        db_instance: admission.db_instance.clone(),
        engine_epoch: admission.engine_epoch.clone(),
    };
    let metadata =
        custody.prepare_configured_rcli_profile(&host, &connection, &profile, secret.as_ref())?;
    let binding = NativeProfileBinding {
        database_instance: admission.db_instance.clone(),
        engine_epoch: admission.engine_epoch.clone(),
        config_id: config.id.clone(),
        source_hash: request.scope.expected_source_hash.clone(),
        at_revision: Cursor::new(revision),
        connection: connection.clone(),
        custody_id: metadata.custody_id.clone(),
    };
    let guard = custody.acquire_configured_rcli_use(
        &host,
        &binding,
        &profile,
        &request.request_id,
        metadata.profile_generation,
    )?;
    let receipt = AccountProfileReceipt {
        config_id: config.id.clone(),
        source_hash: request.scope.expected_source_hash.clone(),
        connection,
        profile_ref: profile,
        custody_id: metadata.custody_id,
        profile_config_sha256: metadata.profile_config_sha256,
        profile_generation: metadata.profile_generation,
    };
    Ok(Work {
        admission: admission.clone(),
        request: request.clone(),
        receipt,
        guard,
        secret,
    })
}
