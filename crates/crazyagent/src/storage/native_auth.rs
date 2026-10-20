use super::*;
use crate::engine::owner::native_auth::{
    Command, FACT_BYTES_MAX, OPERATIONS_MAX, REQUEST_BYTES_MAX, decode_fact, encode_fact,
};

pub(crate) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS native_auth_operations (
            id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 128),
            profile_key TEXT NOT NULL CHECK(length(profile_key)=64),
            generation INTEGER NOT NULL CHECK(generation>0),
            phase TEXT NOT NULL CHECK(phase IN ('starting','awaiting_owner','checking_account',
                'authenticated_observed','unauthenticated_observed','cancelling','unavailable','uncertain')),
            request BLOB NOT NULL CHECK(length(request) BETWEEN 1 AND 8192),
            fact BLOB NOT NULL CHECK(length(fact) BETWEEN 1 AND 16384),
            UNIQUE(profile_key,generation)
        );
        CREATE INDEX IF NOT EXISTS native_auth_profile_phase
        ON native_auth_operations(profile_key,phase);
        CREATE TRIGGER IF NOT EXISTS native_auth_operations_capacity
        BEFORE INSERT ON native_auth_operations
        WHEN (SELECT COUNT(*) FROM native_auth_operations)>=1024
        BEGIN SELECT RAISE(ABORT,'native_auth_operation_capacity'); END;",
    )
    .map_err(error)
}

pub(crate) fn execute(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    admission: &Admission,
    command: Command,
) -> Result<NativeAuthReply, EngineError> {
    command.validate()?;
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let connection = validate_scope(&tx, config, sources, admission, &command)?;
    if let Some(fact) = existing(&tx, connection, admission, &command)? {
        tx.commit().map_err(error)?;
        return Ok(reply(admission, &command, fact));
    }
    validate_method(connection, &command)?;
    let key = profile_key(connection)?;
    ensure_capacity(&tx)?;
    ensure_idle(&tx, &key)?;
    tx.commit().map_err(error)?;
    let scope = command.scope();
    let prepared = prepare_native_profile(
        db,
        config,
        sources,
        custody,
        &HostStamp {
            db_instance: admission.db_instance.clone(),
            engine_epoch: admission.engine_epoch.clone(),
        },
        (
            &scope.connection,
            &scope.expected_config_id,
            &scope.expected_source_hash,
        ),
    )?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let connection = validate_scope(&tx, config, sources, admission, &command)?;
    if connection != &prepared.binding().connection {
        return Err(error("native_profile_source_conflict"));
    }
    ensure_capacity(&tx)?;
    ensure_idle(&tx, &key)?;
    let fact = record(&tx, &key, prepared, &command)?;
    tx.commit().map_err(error)?;
    Ok(reply(admission, &command, fact))
}

fn validate_scope<'a>(
    tx: &Transaction<'_>,
    config: &'a EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    admission: &Admission,
    command: &Command,
) -> Result<&'a crate::config::ModelConnection, EngineError> {
    check_owner_admission(tx, admission)?;
    let scope = command.scope();
    if scope.expected_database_instance != admission.db_instance {
        return Err(error("native_auth_database_mismatch"));
    }
    let source_hash = settings::verified_source_hash(config, sources)
        .map_err(|_| error("native_profile_source_conflict"))?;
    if scope.expected_config_id != config.id || scope.expected_source_hash != source_hash {
        return Err(error("native_profile_source_conflict"));
    }
    native_profile_connection(config, &scope.connection)
}

fn validate_method(
    connection: &crate::config::ModelConnection,
    command: &Command,
) -> Result<(), EngineError> {
    let valid = match command {
        Command::Start(request) => match request.method {
            NativeAuthMethod::CodexBrowser | NativeAuthMethod::CodexDeviceCode => {
                connection.harness.as_str() == "codex"
            }
            NativeAuthMethod::ClaudeOfficial => connection.harness.as_str() == "claude",
        },
        Command::Logout(_) => matches!(connection.harness.as_str(), "codex" | "claude"),
        Command::Read(_) | Command::Cancel(_) => false,
    };
    if !valid {
        return Err(error("native_auth_method_mismatch"));
    }
    Ok(())
}

fn existing(
    tx: &Transaction<'_>,
    connection: &crate::config::ModelConnection,
    admission: &Admission,
    command: &Command,
) -> Result<Option<NativeAuthFact>, EngineError> {
    let row: Option<(Vec<u8>, Vec<u8>, String)> = tx
        .query_row(
            "SELECT request,fact,phase FROM native_auth_operations WHERE id=?1",
            [command.operation_id().as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((request, bytes, phase)) = row else {
        return match command {
            Command::Read(_) | Command::Cancel(_) => Err(error("native_auth_operation_missing")),
            Command::Start(_) | Command::Logout(_) => Ok(None),
        };
    };
    if command.action().is_some() && request != command.request_bytes()? {
        return Err(error("native_auth_operation_conflict"));
    }
    let mut fact = decode_fact(&bytes)?;
    if phase != fact.phase.name() {
        return Err(error("native_auth_fact_invalid"));
    }
    let scope = command.scope();
    if &fact.operation_id != command.operation_id()
        || fact.binding.database_instance != admission.db_instance
        || &fact.binding.connection != connection
        || fact.binding.config_id != scope.expected_config_id
        || fact.binding.source_hash != scope.expected_source_hash
    {
        return Err(error("native_auth_operation_scope_conflict"));
    }
    if unfinished(fact.phase) && fact.binding.engine_epoch != admission.engine_epoch {
        fact.phase = NativeAuthPhase::Uncertain;
        fact.code = Some("native_auth_recovery_required".into());
        tx.execute(
            "UPDATE native_auth_operations SET fact=?1,phase=?2 WHERE id=?3",
            params![
                encode_fact(&fact)?,
                fact.phase.name(),
                fact.operation_id.as_str()
            ],
        )
        .map_err(error)?;
    }
    if matches!(command, Command::Cancel(_)) {
        return Err(error(
            if unfinished(fact.phase) || fact.phase == NativeAuthPhase::Uncertain {
                "native_auth_recovery_required"
            } else {
                "native_auth_not_active"
            },
        ));
    }
    Ok(Some(fact))
}

fn unfinished(phase: NativeAuthPhase) -> bool {
    matches!(
        phase,
        NativeAuthPhase::Starting
            | NativeAuthPhase::AwaitingOwner
            | NativeAuthPhase::CheckingAccount
            | NativeAuthPhase::Cancelling
    )
}

fn profile_key(connection: &crate::config::ModelConnection) -> Result<String, EngineError> {
    let crate::config::ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
        return Err(error("native_profile_not_managed"));
    };
    Ok(format!(
        "{:x}",
        Sha256::digest(profile_ref.as_str().as_bytes())
    ))
}

fn ensure_capacity(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let count: i64 = tx
        .query_row("SELECT COUNT(*) FROM native_auth_operations", [], |row| {
            row.get(0)
        })
        .map_err(error)?;
    if count >= OPERATIONS_MAX {
        return Err(error("native_auth_operation_capacity"));
    }
    assert!((0..OPERATIONS_MAX).contains(&count));
    Ok(())
}

fn ensure_idle(tx: &Transaction<'_>, key: &str) -> Result<(), EngineError> {
    let pending: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM native_auth_operations WHERE profile_key=?1
         AND phase IN ('starting','awaiting_owner','checking_account','cancelling','uncertain'))",
            [key],
            |row| row.get(0),
        )
        .map_err(error)?;
    if pending {
        return Err(error("native_auth_recovery_required"));
    }
    Ok(())
}

fn record(
    tx: &Transaction<'_>,
    key: &str,
    prepared: PreparedNativeProfile,
    command: &Command,
) -> Result<NativeAuthFact, EngineError> {
    let previous: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(generation),0) FROM native_auth_operations WHERE profile_key=?1",
            [key],
            |row| row.get(0),
        )
        .map_err(error)?;
    let generation = previous
        .checked_add(1)
        .ok_or_else(|| error("native_auth_generation_bounds"))?;
    let fact = NativeAuthFact {
        operation_id: command.operation_id().clone(),
        action: command
            .action()
            .ok_or_else(|| error("native_auth_operation_missing"))?,
        binding: prepared.binding().clone(),
        generation: u64::try_from(generation).map_err(error)?,
        phase: NativeAuthPhase::Unavailable,
        code: Some("native_auth_host_unavailable".into()),
    };
    let request = command.request_bytes()?;
    let bytes = encode_fact(&fact)?;
    assert!(request.len() <= REQUEST_BYTES_MAX && bytes.len() <= FACT_BYTES_MAX);
    assert!(generation > previous && fact.generation > 0);
    let inserted = tx
        .execute(
            "INSERT INTO native_auth_operations(id,profile_key,generation,request,fact,phase)
         VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                fact.operation_id.as_str(),
                key,
                generation,
                request,
                bytes,
                fact.phase.name()
            ],
        )
        .map_err(error)?;
    assert_eq!(inserted, 1);
    Ok(fact)
}

fn reply(admission: &Admission, command: &Command, fact: NativeAuthFact) -> NativeAuthReply {
    NativeAuthReply {
        request_id: command.request_id().clone(),
        database_instance: admission.db_instance.clone(),
        engine_epoch: admission.engine_epoch.clone(),
        fact,
        challenge: None,
        account: None,
    }
}
