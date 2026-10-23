use super::*;
use crate::engine::bootstrap::NativeProfileUsePurpose;
use crate::engine::owner::native_auth::{
    CodexAuthObservation, Command, FACT_BYTES_MAX, OPERATIONS_MAX, REQUEST_BYTES_MAX,
    decode_fact, encode_fact,
};
use crate::engine::owner::transport::native_auth_channel::{
    NativeAuthCustodyObservation, NativeAuthCustodyRetirement,
};

impl Clone for Command {
    fn clone(&self) -> Self {
        match self {
            Self::Start(value) => Self::Start(value.clone()),
            Self::Read(value) => Self::Read(value.clone()),
            Self::Cancel(value) => Self::Cancel(value.clone()),
            Self::Logout(value) => Self::Logout(value.clone()),
        }
    }
}

pub(crate) struct NativeAuthBegin {
    pub(crate) reply: NativeAuthReply,
    pub(crate) work: Option<NativeAuthWork>,
}

pub(crate) struct NativeAuthWork {
    fact: NativeAuthFact,
    admission: Admission,
    command: Command,
    physical: NativeAuthPhysical,
}

struct NativeAuthPhysical {
    file: std::fs::File,
    path: std::path::PathBuf,
    device: u64,
    inode: u64,
    bytes: u64,
}

impl NativeAuthPhysical {
    fn capture(db: &Connection, fact: &NativeAuthFact) -> Result<Self, EngineError> {
        let database = Path::new(
            db.path()
                .ok_or_else(|| error("native_profile_custody_unsafe"))?,
        );
        let path = database
            .parent()
            .ok_or_else(|| error("native_profile_custody_unsafe"))?
            .join("native-profiles")
            .join(profile_key(&fact.binding.connection)?)
            .join("identity.json");
        let file = std::fs::File::from(
            rustix::fs::open(
                &path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map_err(error)?,
        );
        let metadata = file.metadata().map_err(error)?;
        let physical = Self {
            file,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
        };
        physical.verify()?;
        Ok(physical)
    }

    fn verify(&self) -> Result<(), EngineError> {
        let metadata = self.file.metadata().map_err(error)?;
        let named = std::fs::symlink_metadata(&self.path).map_err(error)?;
        if !metadata.is_file()
            || !named.is_file()
            || self.inode == 0
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || named.dev() != self.device
            || named.ino() != self.inode
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
            || named.uid() != metadata.uid()
            || named.mode() != metadata.mode()
            || named.nlink() != 1
            || metadata.len() != self.bytes
            || self.bytes == 0
        {
            return Err(error("native_profile_identity_changed"));
        }
        Ok(())
    }
}

impl NativeAuthWork {
    pub(crate) fn fact(&self) -> &NativeAuthFact {
        &self.fact
    }
}

pub(crate) struct NativeAuthDispatch {
    work: NativeAuthWork,
}

impl NativeAuthDispatch {
    pub(crate) fn fact(&self) -> &NativeAuthFact {
        self.work.fact()
    }

    pub(crate) fn admission(&self) -> &Admission {
        &self.work.admission
    }
}

pub(crate) fn dispatch_native_auth(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    work: NativeAuthWork,
) -> Result<NativeAuthDispatch, EngineError> {
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let dispatch = NativeAuthDispatch { work };
    let fact = validate_dispatch(&tx, config, sources, &dispatch)?;
    if !matches!(
        fact.phase,
        NativeAuthPhase::Starting | NativeAuthPhase::Cancelling
    ) {
        return Err(error("native_auth_recovery_required"));
    }
    let host = dispatch_host(&dispatch);
    let purpose = custody_purpose(&fact);
    let guard = custody
        .as_mut()
        .map_err(|error| error.clone())?
        .acquire_use(
            &host,
            &fact.binding,
            &fact.operation_id,
            fact.generation,
            purpose,
        )?;
    guard.revalidate(
        &host,
        &fact.binding,
        &fact.operation_id,
        fact.generation,
        purpose,
    )?;
    validate_dispatch(&tx, config, sources, &dispatch)?;
    tx.commit().map_err(error)?;
    drop(guard);
    assert!(db.is_autocommit());
    Ok(dispatch)
}

pub(crate) fn check_native_auth_custody(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    dispatch: &NativeAuthDispatch,
) -> Result<NativeAuthFact, EngineError> {
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let fact = validate_dispatch(&tx, config, sources, dispatch)?;
    if !matches!(
        fact.phase,
        NativeAuthPhase::Starting | NativeAuthPhase::Cancelling | NativeAuthPhase::AwaitingOwner
            | NativeAuthPhase::CheckingAccount | NativeAuthPhase::AuthenticatedObserved
            | NativeAuthPhase::UnauthenticatedObserved | NativeAuthPhase::Failed | NativeAuthPhase::Cancelled
    ) {
        return Err(error("native_auth_recovery_required"));
    }
    tx.commit().map_err(error)?;
    Ok(fact)
}

pub(crate) fn observe_native_auth_custody(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    observation: NativeAuthCustodyObservation,
) -> Result<NativeAuthFact, EngineError> {
    settings::ensure_clear(db)?;
    observation.validate_session()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let mut fact = validate_dispatch(&tx, config, sources, observation.dispatch())?;
    observation.apply(&mut fact)?;
    store_fact(&tx, &fact)?;
    let stored = validate_dispatch(&tx, config, sources, observation.dispatch())?;
    assert_eq!(stored, fact);
    observation.validate_session()?;
    tx.commit().map_err(error)?;
    assert!(db.is_autocommit());
    Ok(fact)
}

fn dispatch_host(dispatch: &NativeAuthDispatch) -> HostStamp {
    HostStamp {
        db_instance: dispatch.admission().db_instance.clone(),
        engine_epoch: dispatch.admission().engine_epoch.clone(),
    }
}

fn custody_purpose(fact: &NativeAuthFact) -> NativeProfileUsePurpose {
    match fact.action {
        NativeAuthAction::Logout => NativeProfileUsePurpose::Logout,
        NativeAuthAction::SignIn { .. } => NativeProfileUsePurpose::Authentication,
    }
}

fn validate_dispatch(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    dispatch: &NativeAuthDispatch,
) -> Result<NativeAuthFact, EngineError> {
    dispatch.work.physical.verify()?;
    let admission = dispatch.admission();
    validate_scope(tx, config, sources, admission, &dispatch.work.command)?;
    let fact = validate_continuation(
        tx,
        config,
        sources,
        &dispatch_host(dispatch),
        dispatch.fact(),
    )?;
    let (owner_id, owner_epoch, request): (String, i64, Vec<u8>) = tx
        .query_row(
            "SELECT f.owner_id,f.owner_epoch,o.request FROM native_auth_fences f
             JOIN native_auth_operations o ON o.id=f.operation_id WHERE f.operation_id=?1",
            [fact.operation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(error)?;
    if owner_id != admission.owner_id || owner_epoch != admission.owner_epoch {
        return Err(error("owner_grant_stale_or_revoked"));
    }
    if request != dispatch.work.command.request_bytes()? {
        return Err(error("native_auth_operation_scope_conflict"));
    }
    Ok(fact)
}

pub(crate) fn lose_native_auth_custody(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    dispatch: &NativeAuthDispatch,
) -> Result<NativeAuthFact, EngineError> {
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let mut fact = validate_dispatch(&tx, config, sources, dispatch)?;
    apply_update(&mut fact, NativeAuthHostUpdate::CustodyUncertain)?;
    store_fact(&tx, &fact)?;
    tx.commit().map_err(error)?;
    Ok(fact)
}

thread_local! {
    static RETIREMENT_WAIT: std::cell::RefCell<Option<(Arc<NativeAuthCustodyRetirement>, std::time::Instant)>> = const { std::cell::RefCell::new(None) };
}

fn wait_for_custody_commit(_: i32) -> bool {
    let live = RETIREMENT_WAIT.with_borrow(|state| {
        state.as_ref().is_some_and(|(retirement, started)| {
            retirement.session_is_live()
                && started.elapsed() < std::time::Duration::from_millis(250)
        })
    });
    if live {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    live && RETIREMENT_WAIT.with_borrow(|state| {
        state
            .as_ref()
            .is_some_and(|(retirement, _)| retirement.session_is_live())
    })
}

pub(crate) fn finish_native_auth_custody(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    retirement: NativeAuthCustodyRetirement,
) -> Result<NativeAuthFact, EngineError> {
    let retirement = Arc::new(retirement);
    db.busy_handler(Some(wait_for_custody_commit))
        .map_err(error)?;
    RETIREMENT_WAIT.with_borrow_mut(|state| {
        assert!(state.is_none());
        *state = Some((retirement.clone(), std::time::Instant::now()));
    });
    let result = commit_native_auth_custody(db, config, sources, &retirement);
    RETIREMENT_WAIT.with_borrow_mut(|state| {
        state.take();
    });
    db.busy_timeout(std::time::Duration::from_millis(250))
        .map_err(error)?;
    result
}

fn commit_native_auth_custody(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    retirement: &NativeAuthCustodyRetirement,
) -> Result<NativeAuthFact, EngineError> {
    settings::ensure_clear(db)?;
    retirement.validate_session()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let dispatch = retirement.dispatch();
    let mut fact = validate_dispatch(&tx, config, sources, dispatch)?;
    let valid = if retirement.after_child_reaped() {
        matches!(fact.phase, NativeAuthPhase::AuthenticatedObserved
            | NativeAuthPhase::UnauthenticatedObserved | NativeAuthPhase::Failed | NativeAuthPhase::Cancelled)
    } else {
        matches!(fact.phase, NativeAuthPhase::Starting | NativeAuthPhase::Cancelling)
    };
    if !valid {
        return Err(error("native_auth_recovery_required"));
    }
    if !retirement.after_child_reaped() {
        apply_update(&mut fact, NativeAuthHostUpdate::Unavailable)?;
    }
    store_fact(&tx, &fact)?;
    validate_dispatch(&tx, config, sources, dispatch)?;
    retirement.validate_session()?;
    let deleted = tx
        .execute(
            "DELETE FROM native_auth_fences WHERE operation_id=?1 AND generation=?2",
            params![
                fact.operation_id.as_str(),
                i64::try_from(fact.generation)
                    .map_err(|_| error("native_auth_generation_bounds"))?
            ],
        )
        .map_err(error)?;
    assert_eq!(deleted, 1);
    #[cfg(test)]
    retirement_checkpoint::before_commit(retirement)?;
    retirement.validate_session()?;
    tx.commit().map_err(error)?;
    assert!(db.is_autocommit());
    Ok(fact)
}

pub(crate) enum NativeAuthHostUpdate {
    CustodyUncertain,
    Unavailable,
    Codex(CodexAuthObservation),
}

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
        BEGIN SELECT RAISE(ABORT,'native_auth_operation_capacity'); END;
        CREATE TABLE IF NOT EXISTS native_auth_fences (
            profile_key TEXT PRIMARY KEY NOT NULL CHECK(length(profile_key)=64),
            operation_id TEXT UNIQUE NOT NULL REFERENCES native_auth_operations(id)
                CHECK(length(operation_id) BETWEEN 1 AND 128),
            generation INTEGER NOT NULL CHECK(generation>0),
            db_instance TEXT NOT NULL CHECK(length(db_instance)=64),
            engine_epoch TEXT NOT NULL CHECK(length(engine_epoch)=64),
            custody_id TEXT NOT NULL CHECK(length(custody_id)=64),
            owner_id TEXT NOT NULL CHECK(length(owner_id)=64),
            owner_epoch INTEGER NOT NULL CHECK(owner_epoch>0)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS native_auth_one_flow ON native_auth_fences((1));",
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
) -> Result<NativeAuthBegin, EngineError> {
    let reply = match command {
        Command::Read(request) => read_native_auth(db, config, sources, admission, &request)?,
        Command::Cancel(request) => {
            let observed = read_native_auth(
                db,
                config,
                sources,
                admission,
                &NativeAuthQuery {
                    request_id: request.request_id.clone(),
                    operation_id: request.operation_id.clone(),
                    scope: request.scope.clone(),
                },
            )?;
            cancel_native_auth(db, config, sources, admission, &request, &observed.fact)?
        }
        command @ (Command::Start(_) | Command::Logout(_)) => {
            return begin_native_auth(db, config, sources, custody, admission, command);
        }
    };
    Ok(NativeAuthBegin { reply, work: None })
}

pub(crate) fn begin_native_auth(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    admission: &Admission,
    command: Command,
) -> Result<NativeAuthBegin, EngineError> {
    command.validate()?;
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let connection = validate_scope(&tx, config, sources, admission, &command)?;
    if let Some(fact) = existing(&tx, connection, admission, &command)? {
        tx.commit().map_err(error)?;
        return Ok(NativeAuthBegin {
            reply: reply(admission, &command, fact),
            work: None,
        });
    }
    validate_method(connection, &command)?;
    let key = profile_key(connection)?;
    ensure_capacity(&tx)?;
    ensure_idle(&tx, &key)?;
    tx.commit().map_err(error)?;
    let host = HostStamp {
        db_instance: admission.db_instance.clone(),
        engine_epoch: admission.engine_epoch.clone(),
    };
    let prepared = prepare_requested_profile(db, config, sources, custody, &host, &command)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let connection = validate_scope(&tx, config, sources, admission, &command)?;
    if connection != &prepared.binding().connection {
        return Err(error("native_profile_source_conflict"));
    }
    ensure_capacity(&tx)?;
    ensure_idle(&tx, &key)?;
    let generation = next_generation(&tx, &key)?;
    let purpose = if matches!(command, Command::Logout(_)) {
        NativeProfileUsePurpose::Logout
    } else {
        NativeProfileUsePurpose::Authentication
    };
    let guard = custody
        .as_mut()
        .map_err(|error| error.clone())?
        .acquire_use(
            &host,
            prepared.binding(),
            command.operation_id(),
            generation,
            purpose,
        )?;
    let fact = record(&tx, &key, prepared, &command, generation)?;
    let inserted = tx.execute(
        "INSERT INTO native_auth_fences
         (profile_key,operation_id,generation,db_instance,engine_epoch,custody_id,owner_id,owner_epoch)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            key,
            fact.operation_id.as_str(),
            i64::try_from(generation).map_err(|_| error("native_auth_generation_bounds"))?,
            admission.db_instance,
            admission.engine_epoch,
            fact.binding.custody_id,
            admission.owner_id,
            admission.owner_epoch
        ],
    ).map_err(error)?;
    assert_eq!(inserted, 1);
    let physical = NativeAuthPhysical::capture(&tx, &fact)?;
    guard.revalidate(
        &host,
        &fact.binding,
        &fact.operation_id,
        fact.generation,
        purpose,
    )?;
    validate_scope(&tx, config, sources, admission, &command)?;
    tx.commit().map_err(error)?;
    drop(guard);
    assert!(db.is_autocommit());
    let reply = reply(admission, &command, fact.clone());
    Ok(NativeAuthBegin {
        reply,
        work: Some(NativeAuthWork {
            fact,
            admission: admission.clone(),
            command,
            physical,
        }),
    })
}

pub(crate) fn read_native_auth(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    admission: &Admission,
    request: &NativeAuthQuery,
) -> Result<NativeAuthReply, EngineError> {
    let command = Command::Read(request.clone());
    command.validate()?;
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let connection = validate_scope(&tx, config, sources, admission, &command)?;
    let fact = existing(&tx, connection, admission, &command)?
        .ok_or_else(|| error("native_auth_operation_missing"))?;
    tx.commit().map_err(error)?;
    Ok(reply(admission, &command, fact))
}

pub(crate) fn cancel_native_auth(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    admission: &Admission,
    request: &NativeAuthCancel,
    expected: &NativeAuthFact,
) -> Result<NativeAuthReply, EngineError> {
    let command = Command::Cancel(request.clone());
    command.validate()?;
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let connection = validate_scope(&tx, config, sources, admission, &command)?;
    if command.operation_id() != &expected.operation_id
        || connection != &expected.binding.connection
    {
        return Err(error("native_auth_operation_scope_conflict"));
    }
    let mut fact = load_fact(&tx, &expected.operation_id)?;
    if !unfinished(fact.phase) && fact.phase != NativeAuthPhase::Uncertain {
        return Err(error("native_auth_not_active"));
    }
    let host = HostStamp {
        db_instance: admission.db_instance.clone(),
        engine_epoch: admission.engine_epoch.clone(),
    };
    validate_continuation(&tx, config, sources, &host, expected)?;
    if fact.phase == NativeAuthPhase::Uncertain {
        return Err(error("native_auth_recovery_required"));
    }
    fact.phase = NativeAuthPhase::Cancelling;
    fact.code = None;
    store_fact(&tx, &fact)?;
    tx.commit().map_err(error)?;
    Ok(reply(admission, &command, fact))
}

pub(crate) fn publish_native_auth(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    host: &HostStamp,
    expected: &NativeAuthFact,
    update: NativeAuthHostUpdate,
) -> Result<NativeAuthFact, EngineError> {
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let mut fact = validate_continuation(&tx, config, sources, host, expected)?;
    apply_update(&mut fact, update)?;
    store_fact(&tx, &fact)?;
    tx.commit().map_err(error)?;
    Ok(fact)
}

pub(crate) fn finish_native_auth_without_effect(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    admission: &Admission,
    work: NativeAuthWork,
) -> Result<NativeAuthReply, EngineError> {
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, admission)?;
    if admission != &work.admission {
        return Err(error("owner_grant_stale_or_revoked"));
    }
    let host = HostStamp {
        db_instance: admission.db_instance.clone(),
        engine_epoch: admission.engine_epoch.clone(),
    };
    let mut fact = validate_continuation(&tx, config, sources, &host, work.fact())?;
    if !matches!(
        fact.phase,
        NativeAuthPhase::Starting | NativeAuthPhase::Cancelling | NativeAuthPhase::Unavailable
    ) {
        return Err(error("native_auth_recovery_required"));
    }
    let purpose = match fact.action {
        NativeAuthAction::Logout => NativeProfileUsePurpose::Logout,
        NativeAuthAction::SignIn { .. } => NativeProfileUsePurpose::Authentication,
    };
    let guard = custody
        .as_mut()
        .map_err(|error| error.clone())?
        .acquire_use(
            &host,
            &fact.binding,
            &fact.operation_id,
            fact.generation,
            purpose,
        )?;
    apply_update(&mut fact, NativeAuthHostUpdate::Unavailable)?;
    store_fact(&tx, &fact)?;
    guard.revalidate(
        &host,
        &fact.binding,
        &fact.operation_id,
        fact.generation,
        purpose,
    )?;
    validate_continuation(&tx, config, sources, &host, work.fact())?;
    let deleted = tx
        .execute(
            "DELETE FROM native_auth_fences WHERE operation_id=?1 AND generation=?2",
            params![
                fact.operation_id.as_str(),
                i64::try_from(fact.generation)
                    .map_err(|_| error("native_auth_generation_bounds"))?
            ],
        )
        .map_err(error)?;
    assert_eq!(deleted, 1);
    tx.commit().map_err(error)?;
    drop(guard);
    assert!(db.is_autocommit());
    Ok(reply(admission, &work.command, fact))
}

fn load_fact(tx: &Transaction<'_>, id: &RequestId) -> Result<NativeAuthFact, EngineError> {
    let row: Option<(Vec<u8>, String, String, i64)> = tx
        .query_row(
            "SELECT fact,phase,profile_key,generation FROM native_auth_operations WHERE id=?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(error)?;
    let (bytes, phase, key, generation) =
        row.ok_or_else(|| error("native_auth_operation_missing"))?;
    let fact = decode_fact(&bytes)?;
    if &fact.operation_id != id
        || phase != stored_phase(fact.phase)
        || key != profile_key(&fact.binding.connection)?
        || i64::try_from(fact.generation).ok() != Some(generation)
    {
        return Err(error("native_auth_fact_invalid"));
    }
    Ok(fact)
}

fn validate_continuation(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    host: &HostStamp,
    expected: &NativeAuthFact,
) -> Result<NativeAuthFact, EngineError> {
    check_owner_host(tx, host)?;
    let fact = load_fact(tx, &expected.operation_id)?;
    if fact.binding != expected.binding
        || fact.generation != expected.generation
        || fact.action != expected.action
        || fact.binding.database_instance != host.db_instance
        || fact.binding.engine_epoch != host.engine_epoch
    {
        return Err(error("native_auth_operation_scope_conflict"));
    }
    let source_hash = settings::verified_source_hash(config, sources)
        .map_err(|_| error("native_profile_source_conflict"))?;
    if fact.binding.config_id != config.id
        || fact.binding.source_hash != source_hash
        || native_profile_connection(config, &fact.binding.connection.id)?
            != &fact.binding.connection
    {
        return Err(error("native_profile_source_conflict"));
    }
    let origin: Option<(String, i64)> = tx.query_row(
        "SELECT owner_id,owner_epoch FROM native_auth_fences WHERE profile_key=?1 AND operation_id=?2
         AND generation=?3 AND db_instance=?4 AND engine_epoch=?5 AND custody_id=?6",
        params![profile_key(&fact.binding.connection)?, fact.operation_id.as_str(),
            i64::try_from(fact.generation).map_err(|_| error("native_auth_generation_bounds"))?,
            host.db_instance, host.engine_epoch, fact.binding.custody_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(error)?;
    let (owner_id, owner_epoch) = origin.ok_or_else(|| error("native_auth_recovery_required"))?;
    check_owner_admission(
        tx,
        &Admission {
            db_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            owner_id,
            owner_epoch,
        },
    )?;
    Ok(fact)
}

fn apply_update(
    fact: &mut NativeAuthFact,
    update: NativeAuthHostUpdate,
) -> Result<(), EngineError> {
    match update {
        NativeAuthHostUpdate::Codex(observation) => observation.apply(fact)?,
        NativeAuthHostUpdate::CustodyUncertain => {
            fact.phase = NativeAuthPhase::Uncertain;
            fact.code = Some("native_auth_recovery_required".into());
        }
        NativeAuthHostUpdate::Unavailable => {
            if fact.phase == NativeAuthPhase::Uncertain {
                return Err(error("native_auth_recovery_required"));
            }
            fact.phase = NativeAuthPhase::Unavailable;
            fact.code = Some("native_auth_host_unavailable".into());
        }
    }
    Ok(())
}

fn store_fact(tx: &Transaction<'_>, fact: &NativeAuthFact) -> Result<(), EngineError> {
    let updated = tx
        .execute(
            "UPDATE native_auth_operations SET fact=?1,phase=?2 WHERE id=?3 AND generation=?4",
            params![
                encode_fact(fact)?,
                stored_phase(fact.phase),
                fact.operation_id.as_str(),
                i64::try_from(fact.generation)
                    .map_err(|_| error("native_auth_generation_bounds"))?
            ],
        )
        .map_err(error)?;
    assert_eq!(updated, 1);
    Ok(())
}

fn stored_phase(phase: NativeAuthPhase) -> &'static str {
    match phase {
        NativeAuthPhase::Failed | NativeAuthPhase::Cancelled => "unavailable",
        _ => phase.name(),
    }
}

fn prepare_requested_profile(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    host: &HostStamp,
    command: &Command,
) -> Result<PreparedNativeProfile, EngineError> {
    let scope = command.scope();
    prepare_native_profile(
        db,
        config,
        sources,
        custody,
        host,
        (
            &scope.connection,
            &scope.expected_config_id,
            &scope.expected_source_hash,
        ),
    )
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
            NativeAuthMethod::CodexBrowser | NativeAuthMethod::CodexDeviceCode
            | NativeAuthMethod::CodexExisting => connection.harness.as_str() == "codex",
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
    let row: Option<Vec<u8>> = tx
        .query_row(
            "SELECT request FROM native_auth_operations WHERE id=?1",
            [command.operation_id().as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(request) = row else {
        return match command {
            Command::Read(_) | Command::Cancel(_) => Err(error("native_auth_operation_missing")),
            Command::Start(_) | Command::Logout(_) => Ok(None),
        };
    };
    if command.action().is_some() && request != command.request_bytes()? {
        return Err(error("native_auth_operation_conflict"));
    }
    let mut fact = load_fact(tx, command.operation_id())?;
    let scope = command.scope();
    if &fact.operation_id != command.operation_id()
        || fact.binding.database_instance != admission.db_instance
        || &fact.binding.connection != connection
        || fact.binding.config_id != scope.expected_config_id
        || fact.binding.source_hash != scope.expected_source_hash
    {
        return Err(error("native_auth_operation_scope_conflict"));
    }
    let fenced: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM native_auth_fences WHERE operation_id=?1)",
            [fact.operation_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if (unfinished(fact.phase) || fenced) && fact.binding.engine_epoch != admission.engine_epoch {
        fact.phase = NativeAuthPhase::Uncertain;
        fact.code = Some("native_auth_recovery_required".into());
        store_fact(tx, &fact)?;
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
            "SELECT EXISTS(SELECT 1 FROM native_auth_fences WHERE profile_key=?1)
             OR EXISTS(SELECT 1 FROM native_auth_operations WHERE profile_key=?1
             AND phase IN ('starting','awaiting_owner','checking_account','cancelling','uncertain'))",
            [key],
            |row| row.get(0),
        )
        .map_err(error)?;
    if pending {
        return Err(error("native_auth_recovery_required"));
    }
    let busy: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM native_auth_fences)
         OR EXISTS(SELECT 1 FROM native_auth_operations
         WHERE phase IN ('starting','awaiting_owner','checking_account','cancelling','uncertain'))",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if busy {
        return Err(error("native_auth_host_busy"));
    }
    Ok(())
}

fn next_generation(tx: &Transaction<'_>, key: &str) -> Result<u64, EngineError> {
    let previous: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(generation),0) FROM native_auth_operations WHERE profile_key=?1",
            [key],
            |row| row.get(0),
        )
        .map_err(error)?;
    let generation = previous
        .checked_add(1)
        .filter(|generation| *generation > 0)
        .ok_or_else(|| error("native_auth_generation_bounds"))?;
    u64::try_from(generation).map_err(|_| error("native_auth_generation_bounds"))
}

fn record(
    tx: &Transaction<'_>,
    key: &str,
    prepared: PreparedNativeProfile,
    command: &Command,
    generation: u64,
) -> Result<NativeAuthFact, EngineError> {
    let sql_generation =
        i64::try_from(generation).map_err(|_| error("native_auth_generation_bounds"))?;
    let fact = NativeAuthFact {
        operation_id: command.operation_id().clone(),
        action: command
            .action()
            .ok_or_else(|| error("native_auth_operation_missing"))?,
        binding: prepared.binding().clone(),
        generation,
        phase: NativeAuthPhase::Starting,
        code: None,
    };
    let request = command.request_bytes()?;
    let bytes = encode_fact(&fact)?;
    assert!(request.len() <= REQUEST_BYTES_MAX && bytes.len() <= FACT_BYTES_MAX);
    assert!(generation > 0);
    let inserted = tx
        .execute(
            "INSERT INTO native_auth_operations(id,profile_key,generation,request,fact,phase)
         VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                fact.operation_id.as_str(),
                key,
                sql_generation,
                request,
                bytes,
                stored_phase(fact.phase)
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
        claude_challenge: None,
        account: None,
    }
}

#[cfg(test)]
pub(crate) use retirement_checkpoint::NativeAuthRetirementCheckpoint;

#[cfg(test)]
mod retirement_checkpoint {
    use super::{EngineError, NativeAuthCustodyRetirement, NativeAuthFact, error};
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex, mpsc};
    use std::time::Duration;
    use tokio::sync::oneshot;

    type Scope = (String, String, String, u64);

    struct Waiter {
        reached: oneshot::Sender<()>,
        released: mpsc::Receiver<()>,
        bound: Duration,
    }

    struct Entry {
        registration: Arc<()>,
        waiter: Option<Waiter>,
    }

    static CHECKPOINTS: LazyLock<Mutex<HashMap<Scope, Entry>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) struct NativeAuthRetirementCheckpoint {
        scope: Scope,
        registration: Arc<()>,
        reached: Option<oneshot::Receiver<()>>,
        release: Option<mpsc::SyncSender<()>>,
    }

    impl NativeAuthRetirementCheckpoint {
        pub(crate) fn arm(
            fact: &NativeAuthFact,
            wait_bound: Duration,
        ) -> Result<Self, EngineError> {
            if wait_bound.is_zero() || wait_bound > Duration::from_secs(2) {
                return Err(error("native_auth_test_checkpoint_bound"));
            }
            let scope = scope(fact);
            let registration = Arc::new(());
            let (entered, reached) = oneshot::channel();
            let (release, released) = mpsc::sync_channel(1);
            let mut checkpoints = CHECKPOINTS
                .lock()
                .map_err(|_| error("native_auth_test_checkpoint_poisoned"))?;
            if checkpoints.contains_key(&scope) {
                return Err(error("native_auth_test_checkpoint_duplicate"));
            }
            if checkpoints.len() >= 16 {
                return Err(error("native_auth_test_checkpoint_capacity"));
            }
            checkpoints.insert(
                scope.clone(),
                Entry {
                    registration: registration.clone(),
                    waiter: Some(Waiter {
                        reached: entered,
                        released,
                        bound: wait_bound,
                    }),
                },
            );
            Ok(Self {
                scope,
                registration,
                reached: Some(reached),
                release: Some(release),
            })
        }

        pub(crate) async fn reached(&mut self) -> Result<(), EngineError> {
            self.reached
                .take()
                .ok_or_else(|| error("native_auth_test_checkpoint_consumed"))?
                .await
                .map_err(|_| error("native_auth_test_checkpoint_closed"))
        }

        pub(crate) fn release(&mut self) -> Result<(), EngineError> {
            self.release
                .take()
                .ok_or_else(|| error("native_auth_test_checkpoint_consumed"))?
                .send(())
                .map_err(|_| error("native_auth_test_checkpoint_closed"))
        }
    }

    impl Drop for NativeAuthRetirementCheckpoint {
        fn drop(&mut self) {
            drop(self.release.take());
            let mut checkpoints = CHECKPOINTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if checkpoints
                .get(&self.scope)
                .is_some_and(|entry| Arc::ptr_eq(&entry.registration, &self.registration))
            {
                checkpoints.remove(&self.scope);
            }
        }
    }

    pub(super) fn before_commit(
        retirement: &NativeAuthCustodyRetirement,
    ) -> Result<(), EngineError> {
        let waiter = {
            let mut checkpoints = CHECKPOINTS
                .lock()
                .map_err(|_| error("native_auth_test_checkpoint_poisoned"))?;
            let Some(entry) = checkpoints.get_mut(&scope(retirement.dispatch().fact())) else {
                return Ok(());
            };
            entry
                .waiter
                .take()
                .ok_or_else(|| error("native_auth_test_checkpoint_consumed"))?
        };
        waiter
            .reached
            .send(())
            .map_err(|_| error("native_auth_test_checkpoint_closed"))?;
        waiter.released.recv_timeout(waiter.bound).map_err(error)
    }

    fn scope(fact: &NativeAuthFact) -> Scope {
        (
            fact.binding.database_instance.as_str().to_owned(),
            fact.binding.engine_epoch.as_str().to_owned(),
            fact.operation_id.as_str().to_owned(),
            fact.generation,
        )
    }
}
