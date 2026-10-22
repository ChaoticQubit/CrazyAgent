use crate::config::EffectiveConfig;
use crate::engine::owner::native_auth as owner_auth;
use crate::engine::owner::*;
use crate::engine::{EngineError, NativeProfileCustody};
use crate::model_gateway::{self, ConnectionState, Observation};
use crate::protocol::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

mod master_bootstrap;
pub(crate) mod account_profile;
mod master_session;
pub(crate) use master_session::{
    MasterRuntimeCommand, MasterRuntimeObservation, MasterRuntimeResult, PreparedMasterTurn,
    validate_configured_initial_conversation_receipt,
};
mod native_auth;
#[cfg(test)]
mod native_auth_custody_tests;
#[cfg(test)]
pub(crate) use native_auth::NativeAuthRetirementCheckpoint;
pub(crate) use native_auth::{NativeAuthBegin, NativeAuthDispatch, NativeAuthWork};
#[cfg(test)]
mod native_auth_host_tests;
mod settings;
pub(crate) mod worker_dispatch;

pub(crate) enum Job {
    AccountProfile(
        Admission,
        account_profile::Command,
        oneshot::Sender<Result<account_profile::ResultValue, EngineError>>,
    ),
    MasterRuntime(
        Box<MasterRuntimeCommand>,
        oneshot::Sender<Result<MasterRuntimeResult, EngineError>>,
    ),
    NativeAuthDispatch(
        Box<NativeAuthWork>,
        oneshot::Sender<Result<NativeAuthDispatch, EngineError>>,
    ),
    NativeAuthCustodyCheck(
        Arc<NativeAuthDispatch>,
        oneshot::Sender<Result<NativeAuthFact, EngineError>>,
    ),
    NativeAuthCustodyObserve(
        crate::engine::owner::transport::native_auth_channel::NativeAuthCustodyObservation,
        oneshot::Sender<Result<NativeAuthFact, EngineError>>,
    ),
    NativeAuthCustodyRetire(
        crate::engine::owner::transport::native_auth_channel::NativeAuthCustodyRetirement,
        oneshot::Sender<Result<NativeAuthFact, EngineError>>,
    ),
    NativeAuthCustodyLost(
        Arc<NativeAuthDispatch>,
        oneshot::Sender<Result<NativeAuthFact, EngineError>>,
    ),
    NativeAuth(
        Admission,
        owner_auth::Command,
        oneshot::Sender<Result<NativeAuthBegin, EngineError>>,
    ),
    NativeAuthFinish(
        Admission,
        Box<NativeAuthWork>,
        oneshot::Sender<Result<NativeAuthReply, EngineError>>,
    ),
    PrepareNativeProfile(
        HostStamp,
        CredentialRef,
        ConfigId,
        String,
        oneshot::Sender<Result<PreparedNativeProfile, EngineError>>,
    ),
    GatewayObserve(
        HostStamp,
        Box<Observation>,
        oneshot::Sender<Result<(), EngineError>>,
    ),
    GatewayConnectionState(
        HostStamp,
        CredentialRef,
        oneshot::Sender<Result<ConnectionState, EngineError>>,
    ),
    WorkerDispatch(
        Box<worker_dispatch::DispatchJob>,
        oneshot::Sender<Result<worker_dispatch::DispatchResult, EngineError>>,
    ),
    OwnerStart(oneshot::Sender<Result<Started, EngineError>>),
    OwnerStop(HostStamp, oneshot::Sender<Result<(), EngineError>>),
    OwnerControl(
        HostStamp,
        OwnerControl,
        oneshot::Sender<Result<(OwnerControlReply, Registration), EngineError>>,
    ),
    OwnerControlReconcile(
        HostStamp,
        OwnerControl,
        oneshot::Sender<Result<Option<OwnerControlReply>, EngineError>>,
    ),
    OwnerApply(
        Admission,
        OwnerRequest,
        oneshot::Sender<Result<OwnerApplied, EngineError>>,
    ),
    OwnerRead(
        Admission,
        OwnerRead,
        oneshot::Sender<Result<OwnerReadReply, EngineError>>,
    ),
    OwnerReconcile(
        Admission,
        RequestId,
        Option<String>,
        oneshot::Sender<Result<OwnerResponse, EngineError>>,
    ),
    Apply(WireRequest, oneshot::Sender<Result<WireReply, EngineError>>),
    Config(EffectiveConfig, oneshot::Sender<Result<(), EngineError>>),
    Close(oneshot::Sender<Result<(), EngineError>>),
}

pub(crate) struct Storage {
    pub(crate) sender: mpsc::Sender<Job>,
}

impl Storage {
    pub(crate) async fn open(
        path: &Path,
        config: EffectiveConfig,
        readonly: bool,
    ) -> Result<Self, EngineError> {
        Ok(Self::open_inner(path, config, readonly, None).await?.0)
    }

    pub(crate) async fn open_with_settings_sources(
        path: &Path,
        global: &Path,
        project: &Path,
    ) -> Result<(Self, EffectiveConfig), EngineError> {
        let sources = settings::SettingsSources::open(global, project)?;
        let config = sources.config()?;
        Self::open_inner(path, config, false, Some(sources)).await
    }

    async fn open_inner(
        path: &Path,
        config: EffectiveConfig,
        readonly: bool,
        mut sources: Option<settings::SettingsSources>,
    ) -> Result<(Self, EffectiveConfig), EngineError> {
        let (sender, mut receiver) = mpsc::channel(config.settings.queue_capacity);
        let path = path.to_path_buf();
        let (ready_send, ready_receive) = oneshot::channel();
        std::thread::Builder::new()
            .name("crazyagent-sqlite".into())
            .spawn(move || {
                let OpenedDatabase {
                    mut connection,
                    lock,
                    custody,
                } = match open_actor_database(&path, readonly) {
                    Ok(database) => database,
                    Err(e) => {
                        drop(sources.take());
                        let _ = ready_send.send(Err(e));
                        return;
                    }
                };
                let mut custody = Some(custody);
                let mut config = config;
                if !readonly
                    && let Err(e) =
                        settings::recover(&mut connection, &mut config, sources.as_ref())
                {
                    drop(connection);
                    drop(custody.take());
                    drop(lock);
                    drop(sources.take());
                    let _ = ready_send.send(Err(e));
                    return;
                }
                let _ = ready_send.send(Ok(config.clone()));
                let mut owner_runtime = None;
                while let Some(job) = receiver.blocking_recv() {
                    match job {
                        Job::AccountProfile(admission, command, reply) => {
                            let result = if readonly {
                                Err(error("account_profile_readonly"))
                            } else {
                                account_profile::execute(&mut connection, &config, sources.as_ref(),
                                    custody.as_mut().unwrap(), &admission, command)
                            };
                            let _ = reply.send(result);
                        }
                        Job::NativeAuthDispatch(work, reply) => {
                            let result = if readonly {
                                Err(error("native_auth_readonly"))
                            } else {
                                native_auth::dispatch_native_auth(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    custody.as_mut().unwrap(),
                                    *work,
                                )
                            };
                            let _ = reply.send(result);
                        }
                        Job::NativeAuthCustodyCheck(dispatch, reply) => {
                            let _ = reply.send(native_auth::check_native_auth_custody(
                                &mut connection,
                                &config,
                                sources.as_ref(),
                                &dispatch,
                            ));
                        }
                        Job::NativeAuthCustodyObserve(observation, reply) => {
                            let result = if readonly {
                                Err(error("native_auth_readonly"))
                            } else {
                                native_auth::observe_native_auth_custody(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    observation,
                                )
                            };
                            let _ = reply.send(result);
                        }
                        Job::NativeAuthCustodyRetire(retirement, reply) => {
                            let result = if readonly {
                                Err(error("native_auth_readonly"))
                            } else {
                                native_auth::finish_native_auth_custody(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    retirement,
                                )
                            };
                            let _ = reply.send(result);
                        }
                        Job::NativeAuthCustodyLost(dispatch, reply) => {
                            let result = if readonly {
                                Err(error("native_auth_readonly"))
                            } else {
                                native_auth::lose_native_auth_custody(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    &dispatch,
                                )
                            };
                            let _ = reply.send(result);
                        }
                        Job::NativeAuth(admission, command, reply) => {
                            let result = if readonly {
                                Err(error("native_auth_readonly"))
                            } else {
                                native_auth::execute(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    custody.as_mut().unwrap(),
                                    &admission,
                                    command,
                                )
                            };
                            if let Err(Ok(begun)) = reply.send(result)
                                && let Some(work) = begun.work
                            {
                                let _ = finish_native_auth_without_host(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    custody.as_mut().unwrap(),
                                    &admission,
                                    work,
                                );
                            }
                        }
                        Job::NativeAuthFinish(admission, work, reply) => {
                            let result = if readonly {
                                Err(error("native_auth_readonly"))
                            } else {
                                finish_native_auth_without_host(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    custody.as_mut().unwrap(),
                                    &admission,
                                    *work,
                                )
                            };
                            let _ = reply.send(result);
                        }
                        Job::PrepareNativeProfile(host, id, config_id, source_hash, reply) => {
                            let result = if readonly {
                                Err(error("native_profile_readonly"))
                            } else {
                                prepare_native_profile(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    custody.as_mut().unwrap(),
                                    &host,
                                    (&id, &config_id, &source_hash),
                                )
                            };
                            let _ = reply.send(result);
                        }
                        Job::GatewayObserve(host, observation, reply) => {
                            let result = if readonly {
                                Err(error("gateway_readonly"))
                            } else {
                                gateway_observe(&mut connection, &config, &host, &observation)
                            };
                            let _ = reply.send(result);
                        }
                        Job::GatewayConnectionState(host, id, reply) => {
                            let _ = reply.send(gateway_connection_state(
                                &mut connection,
                                &config,
                                &host,
                                &id,
                            ));
                        }
                        Job::MasterRuntime(command, reply) => {
                            let result = if readonly {
                                Err(error("master_runtime_readonly"))
                            } else {
                                master_session::runtime(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    custody.as_mut().unwrap(),
                                    &path,
                                    *command,
                                )
                            };
                            let _ = reply.send(result);
                        }
                        Job::WorkerDispatch(job, reply) => {
                            let result = if readonly {
                                Err(error("dispatch_readonly"))
                            } else {
                                worker_dispatch::execute(&mut connection, &config, *job)
                            };
                            let _ = reply.send(result);
                        }
                        Job::OwnerStart(reply) => {
                            let result = if readonly || owner_runtime.is_some() {
                                Err(error("owner_runtime_unavailable_or_busy"))
                            } else {
                                owner_start(&mut connection).and_then(|started| {
                                    worker_dispatch::restart(&mut connection, &config)?;
                                    Ok(started)
                                })
                            };
                            if let Ok(started) = &result {
                                owner_runtime = Some(started.host.clone());
                            }
                            let _ = reply.send(result);
                        }
                        Job::OwnerStop(stamp, reply) => {
                            let result = owner_stop(&mut connection, &stamp);
                            if result.is_ok() {
                                owner_runtime = None;
                            }
                            let _ = reply.send(result);
                        }
                        Job::OwnerControl(stamp, control, reply) => {
                            let _ = reply.send(owner_control(&mut connection, &stamp, &control));
                        }
                        Job::OwnerControlReconcile(stamp, control, reply) => {
                            let _ = reply.send(owner_control_reconcile(
                                &mut connection,
                                &stamp,
                                &control,
                            ));
                        }
                        Job::OwnerApply(admission, request, reply) => {
                            if matches!(
                                &request.operation,
                                OwnerOperation::Apply {
                                    mutation: OwnerMutation::ModelAccess { .. }
                                }
                            ) {
                                let _ = reply.send(settings::apply(
                                    &mut connection,
                                    &mut config,
                                    sources.as_ref(),
                                    &admission,
                                    &request,
                                ));
                                continue;
                            }
                            if let Err(e) = settings::ensure_clear(&connection) {
                                let _ = reply.send(Err(e));
                                continue;
                            }
                            if matches!(
                                &request.operation,
                                OwnerOperation::Apply {
                                    mutation: OwnerMutation::ProjectMaster {
                                        action: crate::master_session::MasterOwnerAction::RegisterRcliProfile { .. }
                                    }
                                }
                            ) {
                                let result = match owner_runtime.as_ref() {
                                    Some(host) => master_session::apply_rcli_profile_registration(
                                        &mut connection,
                                        &config,
                                        sources.as_ref(),
                                        custody.as_mut().unwrap(),
                                        host,
                                        &admission,
                                        &request,
                                    ),
                                    None => Err(error("owner_runtime_stale")),
                                };
                                let _ = reply.send(result);
                                continue;
                            }
                            if matches!(
                                &request.operation,
                                OwnerOperation::Apply {
                                    mutation: OwnerMutation::ProjectMaster { .. }
                                }
                            ) {
                                let result = match owner_runtime.as_ref() {
                                    Some(host) => master_session::apply(
                                        &mut connection,
                                        &config,
                                        sources.as_ref(),
                                        custody.as_mut().unwrap(),
                                        host,
                                        &admission,
                                        &request,
                                    ),
                                    None => Err(error("owner_runtime_stale")),
                                };
                                let _ = reply.send(result);
                                continue;
                            }
                            let _ = reply.send(owner_apply(
                                &mut connection,
                                &config,
                                &path,
                                &admission,
                                &request,
                            ));
                        }
                        Job::OwnerRead(admission, query, reply) => {
                            if matches!(
                                &query,
                                OwnerRead::ProjectMaster { .. }
                                    | OwnerRead::ProjectMasterTurns { .. }
                            ) {
                                let _ = reply.send(master_session::read(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    &admission,
                                    &query,
                                ));
                                continue;
                            }
                            if matches!(
                                query,
                                OwnerRead::ModelAccessPage { .. }
                                    | OwnerRead::ModelAccessRecord { .. }
                            ) {
                                let _ = reply.send(settings::read(
                                    &mut connection,
                                    &config,
                                    sources.as_ref(),
                                    &admission,
                                    &query,
                                ));
                                continue;
                            }
                            if matches!(query, OwnerRead::Capabilities) && sources.is_some() {
                                let result =
                                    owner_read(&mut connection, &config, &path, &admission, &query)
                                        .and_then(|mut state| {
                                            if let OwnerData::Capabilities {
                                                settings_persistence,
                                                ..
                                            } = &mut state.data
                                            {
                                                *settings_persistence =
                                                    !settings::pending(&connection)?;
                                            }
                                            Ok(state)
                                        });
                                let _ = reply.send(result);
                                continue;
                            }
                            let _ = reply.send(owner_read(
                                &mut connection,
                                &config,
                                &path,
                                &admission,
                                &query,
                            ));
                        }
                        Job::OwnerReconcile(admission, id, hash, reply) => {
                            let _ = reply.send(owner_reconcile(
                                &mut connection,
                                &admission,
                                &id,
                                hash.as_deref(),
                            ));
                        }
                        Job::Apply(request, reply) => {
                            if let Err(e) = settings::ensure_clear(&connection) {
                                let _ = reply.send(Err(e));
                                continue;
                            }
                            let _ = reply.send(apply(&mut connection, &config, &path, request));
                        }
                        Job::Config(next, reply) => {
                            let result = settings::ensure_clear(&connection).and_then(|()| {
                                if sources.is_some() {
                                    Err(error("settings_sources_host_fixed"))
                                } else {
                                    store_config(&connection, &next)
                                }
                            });
                            if result.is_ok() {
                                config = next;
                            }
                            let _ = reply.send(result);
                        }
                        Job::Close(reply) => {
                            if let Some(stamp) = &owner_runtime {
                                let _ = owner_stop(&mut connection, stamp);
                            }
                            drop(connection);
                            drop(custody.take());
                            let released = release_database_lock(lock);
                            drop(sources.take());
                            let _ = reply.send(released);
                            break;
                        }
                    }
                }
            })
            .map_err(|e| error(format!("database_worker_spawn_failed: {e}")))?;
        let config = ready_receive.await.map_err(error)??;
        Ok((Self { sender }, config))
    }
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// Opens private engine storage and initializes additive authority schemas.
/// Takes a database path and read-only mode and returns connection ownership.
/// Rejects unsafe filesystem metadata and competing engine ownership.
#[cfg(test)]
fn open_database(path: &Path, readonly: bool) -> Result<(Connection, std::fs::File), EngineError> {
    let opened = open_actor_database(path, readonly)?;
    Ok((opened.connection, opened.lock))
}

struct OpenedDatabase {
    connection: Connection,
    lock: std::fs::File,
    custody: Result<NativeProfileCustody, EngineError>,
}

fn open_actor_database(path: &Path, readonly: bool) -> Result<OpenedDatabase, EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| error("database parent missing"))?;
    let metadata = std::fs::symlink_metadata(parent).map_err(error)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(error("database requires private owner directory"));
    }
    if !readonly {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(error(e)),
        }
    }
    let metadata = std::fs::symlink_metadata(path).map_err(error)?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(error("database must be a private owner file"));
    }
    let lock = lock_database(path)?;
    let flags = if readonly {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let canonical_path = std::fs::canonicalize(parent).map_err(error)?.join(
        path.file_name()
            .ok_or_else(|| error("database name missing"))?,
    );
    let connection = Connection::open_with_flags(
        canonical_path,
        flags | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(error)?;
    connection
        .busy_timeout(std::time::Duration::from_millis(250))
        .map_err(error)?;
    if !readonly {
        initialize_authority_schemas(&connection)?;
    }
    assert!(connection.is_autocommit());
    assert_eq!(
        std::fs::symlink_metadata(path).map_err(error)?.ino(),
        metadata.ino()
    );
    let custody = NativeProfileCustody::capture(path, &metadata);
    Ok(OpenedDatabase {
        connection,
        lock,
        custody,
    })
}

fn initialize_authority_schemas(connection: &Connection) -> Result<(), EngineError> {
    initialize_schema(connection)?;
    crate::coordination::initialize(connection)?;
    crate::memory::initialize(connection)?;
    crate::assets::initialize(connection)?;
    crate::verification::initialize(connection)?;
    crate::worktrees::initialize(connection)?;
    crate::worktrees::reconcile(connection)?;
    worker_dispatch::initialize(connection)?;
    model_gateway::initialize(connection)?;
    master_session::initialize(connection)?;
    native_auth::initialize(connection)?;
    master_bootstrap::initialize(connection)
}

fn finish_native_auth_without_host(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    admission: &Admission,
    work: NativeAuthWork,
) -> Result<NativeAuthReply, EngineError> {
    let expected = work.fact().clone();
    let result = native_auth::finish_native_auth_without_effect(
        db, config, sources, custody, admission, work,
    );
    if result.is_err() {
        let host = HostStamp {
            db_instance: admission.db_instance.clone(),
            engine_epoch: admission.engine_epoch.clone(),
        };
        let _ = native_auth::publish_native_auth(
            db,
            config,
            sources,
            &host,
            &expected,
            native_auth::NativeAuthHostUpdate::CustodyUncertain,
        );
    }
    result
}

fn prepare_native_profile(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    custody: &mut Result<NativeProfileCustody, EngineError>,
    host: &HostStamp,
    expected: (&CredentialRef, &ConfigId, &str),
) -> Result<PreparedNativeProfile, EngineError> {
    settings::ensure_clear(db)?;
    let tx = db.transaction().map_err(error)?;
    check_owner_host(&tx, host)?;
    let source_hash = settings::verified_source_hash(config, sources)
        .map_err(|_| error("native_profile_source_conflict"))?;
    if &config.id != expected.1 || source_hash != expected.2 {
        return Err(error("native_profile_source_conflict"));
    }
    let connection = native_profile_connection(config, expected.0)?;
    let (custody_id, key) = custody
        .as_mut()
        .map_err(|error| error.clone())?
        .prepare(host, connection)?;
    if settings::verified_source_hash(config, sources)
        .map_err(|_| error("native_profile_source_conflict"))?
        != source_hash
    {
        return Err(error("native_profile_source_conflict"));
    }
    let binding = NativeProfileBinding {
        database_instance: host.db_instance.clone(),
        engine_epoch: host.engine_epoch.clone(),
        config_id: config.id.clone(),
        source_hash,
        at_revision: Cursor::new(owner_revision(&tx)?),
        connection: connection.clone(),
        custody_id,
    };
    tx.commit().map_err(error)?;
    let crate::config::ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
        return Err(error("native_profile_not_managed"));
    };
    Ok(NativeProfileCustody::receipt(
        binding,
        expected.0,
        profile_ref,
        &key,
    ))
}

fn native_profile_connection<'a>(
    config: &'a EffectiveConfig,
    id: &CredentialRef,
) -> Result<&'a crate::config::ModelConnection, EngineError> {
    use crate::config::{ModelAuthentication, ModelEndpoint};
    let catalog = config
        .settings
        .model_access
        .as_ref()
        .ok_or_else(|| error("native_profile_connection_missing"))?;
    let connection = catalog
        .connections
        .iter()
        .find(|item| &item.id == id)
        .ok_or_else(|| error("native_profile_connection_missing"))?;
    let ModelAuthentication::NativeManaged { profile_ref } = &connection.auth else {
        return Err(error("native_profile_not_managed"));
    };
    if !matches!(connection.endpoint, ModelEndpoint::Native {})
        || connection.protocol.as_str() != "native"
        || !catalog.accounts.contains(&connection.account_ref)
    {
        return Err(error("native_profile_not_managed"));
    }
    for alias in &catalog.connections {
        if matches!(&alias.auth, ModelAuthentication::NativeManaged { profile_ref: other } if other == profile_ref)
            && (alias.account_ref != connection.account_ref
                || alias.provider != connection.provider
                || alias.harness != connection.harness
                || alias.protocol != connection.protocol)
        {
            return Err(error("native_profile_binding_conflict"));
        }
    }
    Ok(connection)
}

fn gateway_observe(
    db: &mut Connection,
    config: &EffectiveConfig,
    host: &HostStamp,
    observation: &Observation,
) -> Result<(), EngineError> {
    settings::ensure_clear(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_host(&tx, host)?;
    model_gateway::observe(&tx, config, observation)?;
    tx.commit().map_err(error)
}

fn gateway_connection_state(
    db: &mut Connection,
    config: &EffectiveConfig,
    host: &HostStamp,
    id: &CredentialRef,
) -> Result<ConnectionState, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_host(&tx, host)?;
    let connection = config
        .settings
        .model_access
        .as_ref()
        .and_then(|catalog| {
            catalog
                .connections
                .iter()
                .find(|connection| &connection.id == id)
        })
        .ok_or_else(|| error("gateway_connection_missing"))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| error("gateway_time_bounds"))?
        .as_millis();
    let now_ms = u64::try_from(now_ms)
        .ok()
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| error("gateway_time_bounds"))?;
    let state = model_gateway::connection_state(&tx, connection, now_ms)?;
    tx.commit().map_err(error)?;
    Ok(state)
}

fn lock_database(path: &Path) -> Result<std::fs::File, EngineError> {
    let lock_path = path.with_extension("engine-lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&lock_path)
        .map_err(error)?;
    let lock_metadata = lock.metadata().map_err(error)?;
    if !lock_metadata.is_file()
        || lock_metadata.uid() != rustix::process::getuid().as_raw()
        || lock_metadata.mode() & 0o077 != 0
        || lock_metadata.nlink() != 1
    {
        return Err(error("invalid_engine_lock_file"));
    }
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|e| error(format!("database_lock_acquire_failed: {e}")))?;
    assert_eq!(lock.metadata().map_err(error)?.ino(), lock_metadata.ino());
    assert_eq!(lock.metadata().map_err(error)?.dev(), lock_metadata.dev());
    Ok(lock)
}

fn release_database_lock(lock: std::fs::File) -> Result<(), EngineError> {
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::Unlock)
        .map_err(|e| error(format!("database_lock_release_failed: {e}")))
}

fn initialize_schema(connection: &Connection) -> Result<(), EngineError> {
    assert!(connection.is_autocommit());
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
            INSERT OR IGNORE INTO state VALUES (1,0);
            CREATE TABLE IF NOT EXISTS operations (client TEXT NOT NULL, id TEXT NOT NULL,
                request BLOB NOT NULL, reply BLOB NOT NULL, PRIMARY KEY(client,id));
            CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS configs (id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS settings_publication (singleton INTEGER PRIMARY KEY CHECK(singleton=1), body BLOB NOT NULL CHECK(length(body)<=262144));")
        .map_err(error)?;
    assert!(connection.is_autocommit());
    Ok(())
}

/// Applies one revision-checked operation through the storage transaction.
/// Takes current config, storage path and an identified request.
/// Returns a durable reply while preserving replay and sensitive input hashes.
fn apply(
    connection: &mut Connection,
    config: &EffectiveConfig,
    path: &Path,
    request: WireRequest,
) -> Result<WireReply, EngineError> {
    if matches!(
        request.command,
        Command::OwnerConversation { .. } | Command::OwnerModelAccess { .. }
    ) {
        return Err(error("owner_authentication_required"));
    }
    if request.client_id.as_str().starts_with("owner:") {
        return Err(error("reserved_owner_identity"));
    }
    let mut bytes = serde_json::to_vec(&request).map_err(error)?;
    if matches!(
        request.command,
        Command::Knowledge { .. }
            | Command::Assets { .. }
            | Command::ProposeBaseline { .. }
            | Command::OwnerVerification { .. }
    ) {
        bytes = format!("sha256:{}", crate::memory::hash(&bytes)).into_bytes();
    }
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let previous: Option<(Vec<u8>, Vec<u8>)> = transaction
        .query_row(
            "SELECT request,reply FROM operations WHERE client=?1 AND id=?2",
            params![request.client_id.as_str(), request.request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let mutation = matches!(
        request.command,
        Command::CreateProject { .. }
            | Command::PinTask { .. }
            | Command::MigrateTask { .. }
            | Command::AdmitRepository { .. }
            | Command::AllocateWorktree { .. }
            | Command::ReleaseWorktree { .. }
    ) || crate::verification::is_mutation(&request.command)
        || crate::coordination::is_mutation(&request.command)
        || matches!(&request.command, Command::Knowledge { action } if crate::memory::is_mutation(action))
        || matches!(&request.command, Command::Assets { action } if crate::assets::is_mutation(action));
    let replay = previous.is_some();
    if let Some((original, reply)) = previous {
        if original != bytes {
            return Err(error("operation_identity_conflict"));
        }
        if mutation {
            return serde_json::from_slice(&reply).map_err(error);
        }
    }
    let revision: i64 = transaction
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(error)?;
    let revision = u64::try_from(revision).map_err(error)?;
    let outcome = if mutation && request.expected_revision.value() != revision {
        Outcome::Rejected {
            code: "stale_revision".into(),
        }
    } else {
        execute(&transaction, config, path, &request.command, revision)?
    };
    let (transaction, outcome) = if outcome == Outcome::Applied
        && let Command::AllocateWorktree {
            task_id,
            repository,
            ..
        } = &request.command
    {
        transaction.commit().map_err(error)?;
        let result = crate::worktrees::provision(connection, task_id, repository);
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(error)?;
        let outcome = match result {
            Ok(()) => Outcome::Applied,
            Err(e) => Outcome::Rejected { code: e.0 },
        };
        (transaction, outcome)
    } else {
        (transaction, outcome)
    };
    let changed = mutation && outcome == Outcome::Applied;
    let next = if changed {
        revision
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| error("revision_exhausted"))?
    } else {
        revision
    };
    let reply = WireReply {
        request_id: request.request_id.clone(),
        revision: Cursor::new(next),
        outcome,
    };
    finish_operation(
        transaction,
        config,
        &request,
        bytes,
        reply,
        replay,
        revision,
    )
}

/// Atomically records an operation reply, revision and retained event.
/// Takes an open transaction and its bounded operation outcome.
/// Sensitive authority reads are reevaluated rather than cached as replies.
fn finish_operation(
    transaction: Transaction<'_>,
    config: &EffectiveConfig,
    request: &WireRequest,
    bytes: Vec<u8>,
    reply: WireReply,
    replay: bool,
    revision: u64,
) -> Result<WireReply, EngineError> {
    let next = reply.revision.value();
    let changed = reply.outcome == Outcome::Applied;
    let frame = encode_frame(&reply).map_err(error)?;
    if changed {
        persist_event(&transaction, config, request, next)?;
    }
    if !replay {
        let sensitive_read = matches!(&request.command, Command::Knowledge { action } if !crate::memory::is_mutation(action))
            || matches!(&request.command, Command::Assets { action } if !crate::assets::is_mutation(action))
            || matches!(&request.command, Command::OwnerVerification { .. } if !crate::verification::is_mutation(&request.command));
        let reply_bytes = if sensitive_read { &[][..] } else { &frame[4..] };
        transaction
            .execute(
                "INSERT INTO operations VALUES (?1,?2,?3,?4)",
                params![
                    request.client_id.as_str(),
                    request.request_id.as_str(),
                    bytes,
                    reply_bytes
                ],
            )
            .map_err(error)?;
    }
    assert_eq!(next > revision, changed);
    assert!(frame.len() <= FRAME_BYTES_MAX + 4);
    transaction.commit().map_err(error)?;
    Ok(reply)
}

/// Dispatches a typed command inside the engine storage transaction.
/// Takes current configuration, command and storage context.
/// Returns domain outcomes without granting worker-wire owner authority.
fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    path: &Path,
    command: &Command,
    revision: u64,
) -> Result<Outcome, EngineError> {
    match command {
        Command::OwnerModelAccess { .. } => Ok(Outcome::Rejected {
            code: "settings_read_only".into(),
        }),
        Command::OwnerConversation { mutation } => {
            crate::conversation::apply(tx, mutation, revision)
        }
        Command::ProposeBaseline { .. } | Command::OwnerVerification { .. } => {
            crate::verification::execute(tx, config, command)
        }
        Command::Knowledge { action } => crate::memory::execute(tx, config, action, revision),
        Command::Assets { action } => crate::assets::execute(tx, config, action),
        Command::CreateProject { project_id, spec } => create_project(tx, project_id, spec),
        Command::Snapshot => {
            let mut outcome = snapshot(tx)?;
            if let Outcome::Snapshot { tasks, .. } = &mut outcome {
                for context in tasks {
                    context
                        .allowed_destinations
                        .retain(|d| !config.settings.revoked_destinations.contains(d));
                }
            }
            Ok(outcome)
        }
        Command::Events { after } => events(tx, after.value(), revision),
        Command::PinTask { context } => pin_task(tx, config, context),
        Command::MigrateTask { task_id, config_id } => migrate_task(tx, config, task_id, config_id),
        Command::TaskContext { task_id } => {
            let Some(mut context) = task_context(tx, task_id)? else {
                return Ok(Outcome::Rejected {
                    code: "task_missing".into(),
                });
            };
            context
                .allowed_destinations
                .retain(|d| !config.settings.revoked_destinations.contains(d));
            Ok(Outcome::Task { context })
        }
        Command::TaskConfig { task_id } => {
            let Some(context) = task_context(tx, task_id)? else {
                return Ok(Outcome::Rejected {
                    code: "task_missing".into(),
                });
            };
            let body: String = tx
                .query_row(
                    "SELECT body FROM configs WHERE id=?1",
                    [context.config_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(error)?;
            Ok(Outcome::Configuration {
                config: Box::new(serde_json::from_str(&body).map_err(error)?),
            })
        }
        Command::AdmitRepository {
            project_id,
            repository,
        } => crate::worktrees::admit(tx, project_id, repository),
        Command::AllocateWorktree { .. } => crate::worktrees::reserve(
            tx,
            &std::fs::canonicalize(
                path.parent()
                    .ok_or_else(|| error("runtime_parent_missing"))?,
            )
            .map_err(error)?,
            command,
        ),
        Command::WorktreeLeases { task_id } => crate::worktrees::leases(tx, task_id),
        Command::ReleaseWorktree {
            task_id,
            repository,
            owner_session,
        } => crate::worktrees::release(tx, task_id, repository, owner_session),
        _ => crate::coordination::execute(tx, config, command, revision),
    }
}

fn create_project(
    tx: &Transaction<'_>,
    project_id: &ProjectId,
    spec: &ProjectSpec,
) -> Result<Outcome, EngineError> {
    let stored = StoredProject {
        project_id: project_id.clone(),
        spec: spec.clone(),
    };
    let count = tx
        .execute(
            "INSERT OR IGNORE INTO projects VALUES (?1,?2)",
            params![
                project_id.as_str(),
                serde_json::to_string(&stored).map_err(error)?
            ],
        )
        .map_err(error)?;
    if count == 0 {
        return Ok(Outcome::Rejected {
            code: "project_exists".into(),
        });
    }
    enforce_snapshot_bound(tx)?;
    assert_eq!(count, 1);
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn snapshot(tx: &Transaction<'_>) -> Result<Outcome, EngineError> {
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

fn enforce_snapshot_bound(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let snapshot = snapshot(tx)?;
    let bytes = serde_json::to_vec(&snapshot).map_err(error)?;
    if bytes.len() > FRAME_BYTES_MAX - 1024 {
        return Err(error("state_capacity"));
    }
    assert!(bytes.len() <= FRAME_BYTES_MAX - 1024);
    Ok(())
}

fn events(tx: &Transaction<'_>, after: u64, revision: u64) -> Result<Outcome, EngineError> {
    let first: Option<i64> = tx
        .query_row("SELECT MIN(cursor) FROM events", [], |row| row.get(0))
        .map_err(error)?;
    if after > revision || first.is_some_and(|first| after.saturating_add(1) < first as u64) {
        return Ok(Outcome::ResyncRequired);
    }
    let mut statement = tx
        .prepare("SELECT body FROM events WHERE cursor>?1 ORDER BY cursor LIMIT 128")
        .map_err(error)?;
    let rows = statement
        .query_map([after as i64], |row| row.get::<_, String>(0))
        .map_err(error)?;
    let events = rows
        .map(|row| serde_json::from_str(&row.map_err(error)?).map_err(error))
        .collect::<Result<Vec<Event>, _>>()?;
    assert!(events.len() <= 128);
    assert!(
        events
            .windows(2)
            .all(|w| w[0].cursor.value() < w[1].cursor.value())
    );
    Ok(Outcome::Events { events })
}

fn persist_event(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    request: &WireRequest,
    revision: u64,
) -> Result<(), EngineError> {
    persist_event_ids(
        tx,
        config,
        &request.request_id,
        &request.client_id,
        revision,
    )
}

fn persist_event_ids(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    request_id: &RequestId,
    client_id: &ClientId,
    revision: u64,
) -> Result<(), EngineError> {
    let event = Event {
        cursor: Cursor::new(revision),
        request_id: request_id.clone(),
        client_id: client_id.clone(),
    };
    tx.execute("UPDATE state SET revision=?1 WHERE id=1", [revision as i64])
        .map_err(error)?;
    tx.execute(
        "INSERT INTO events VALUES (?1,?2)",
        params![
            revision as i64,
            serde_json::to_string(&event).map_err(error)?
        ],
    )
    .map_err(error)?;
    tx.execute(
        "DELETE FROM events WHERE cursor<=?1",
        [revision.saturating_sub(config.settings.events_retained as u64) as i64],
    )
    .map_err(error)?;
    assert!(revision > 0);
    assert_eq!(event.cursor.value(), revision);
    Ok(())
}

pub(crate) fn task_context(
    tx: &Transaction<'_>,
    task_id: &TaskId,
) -> Result<Option<ExecutionContext>, EngineError> {
    let value: Option<String> = tx
        .query_row(
            "SELECT body FROM tasks WHERE id=?1",
            [task_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    value
        .map(|value| serde_json::from_str(&value).map_err(error))
        .transpose()
}

fn pin_task(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    context: &ExecutionContext,
) -> Result<Outcome, EngineError> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [context.project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !exists
        || context.config_id != config.id
        || context.allowed_destinations.iter().any(|d| {
            !config.settings.allowed_destinations.contains(d)
                || config.settings.revoked_destinations.contains(d)
        })
    {
        return Ok(Outcome::Rejected {
            code: "context_not_authorized".into(),
        });
    }
    let count = tx
        .execute(
            "INSERT OR IGNORE INTO tasks VALUES (?1,?2)",
            params![
                context.task_id.as_str(),
                serde_json::to_string(context).map_err(error)?
            ],
        )
        .map_err(error)?;
    if count == 0 {
        return Ok(Outcome::Rejected {
            code: "task_exists".into(),
        });
    }
    store_config(tx, config)?;
    enforce_snapshot_bound(tx)?;
    assert_eq!(count, 1);
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn migrate_task(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    task_id: &TaskId,
    config_id: &ConfigId,
) -> Result<Outcome, EngineError> {
    if config_id != &config.id {
        return Ok(Outcome::Rejected {
            code: "config_not_current".into(),
        });
    }
    let Some(mut context) = task_context(tx, task_id)? else {
        return Ok(Outcome::Rejected {
            code: "task_missing".into(),
        });
    };
    context.config_id = config_id.clone();
    store_config(tx, config)?;
    context.allowed_destinations = config
        .settings
        .allowed_destinations
        .iter()
        .filter(|d| !config.settings.revoked_destinations.contains(d))
        .cloned()
        .collect();
    tx.execute(
        "UPDATE tasks SET body=?1 WHERE id=?2",
        params![
            serde_json::to_string(&context).map_err(error)?,
            task_id.as_str()
        ],
    )
    .map_err(error)?;
    enforce_snapshot_bound(tx)?;
    assert_eq!(&context.config_id, config_id);
    assert!(context.allowed_destinations.len() <= 64);
    Ok(Outcome::Applied)
}

pub(crate) fn store_config(
    connection: &Connection,
    config: &EffectiveConfig,
) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT OR IGNORE INTO configs VALUES (?1,?2)",
            params![
                config.id.as_str(),
                serde_json::to_string(config).map_err(error)?
            ],
        )
        .map_err(error)?;
    Ok(())
}

const OWNER_META: &str = "CREATE TABLE owner_meta (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 1),
    db_instance TEXT NOT NULL CHECK(length(db_instance) = 64),
    engine_epoch TEXT NOT NULL CHECK(length(engine_epoch) = 64),
    runtime_state TEXT NOT NULL CHECK(runtime_state IN ('offline', 'active'))
)";
const OWNER_IDENTITIES: &str = "CREATE TABLE owner_identities (
    owner_id TEXT NOT NULL PRIMARY KEY CHECK(length(owner_id) = 64),
    client_kind TEXT NOT NULL UNIQUE CHECK(client_kind IN ('tui', 'desktop')),
    owner_epoch INTEGER NOT NULL CHECK(owner_epoch >= 1),
    status TEXT NOT NULL CHECK(status IN ('active', 'revoked'))
)";
const OWNER_OPERATIONS: &str = "CREATE TABLE owner_operations (
    origin TEXT NOT NULL CHECK(origin IN ('native_owner', 'trusted_host')),
    owner_id TEXT NOT NULL,
    operation_id TEXT NOT NULL CHECK(length(operation_id) BETWEEN 1 AND 128),
    request_hash BLOB NOT NULL CHECK(length(request_hash) = 32),
    engine_epoch TEXT NOT NULL CHECK(length(engine_epoch) = 64),
    admitted_epoch INTEGER NOT NULL CHECK(admitted_epoch >= 1),
    resulting_epoch INTEGER NOT NULL CHECK(resulting_epoch >= admitted_epoch),
    revision_before INTEGER NOT NULL CHECK(revision_before >= 0),
    revision_after INTEGER NOT NULL CHECK(revision_after >= revision_before),
    result BLOB NOT NULL CHECK(length(result) BETWEEN 1 AND 4096),
    event_cursor INTEGER CHECK(event_cursor IS NULL OR event_cursor >= 1),
    PRIMARY KEY(origin, owner_id, operation_id)
)";
const OWNER_EVENT: &str = "CREATE UNIQUE INDEX owner_operation_event ON owner_operations(event_cursor) WHERE event_cursor IS NOT NULL";

fn owner_schema(tx: &Transaction<'_>) -> Result<bool, EngineError> {
    let expected = [
        ("owner_meta", OWNER_META),
        ("owner_identities", OWNER_IDENTITIES),
        ("owner_operations", OWNER_OPERATIONS),
        ("owner_operation_event", OWNER_EVENT),
    ];
    let mut stmt = tx.prepare("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND (name GLOB 'owner_*' OR tbl_name IN ('owner_meta','owner_identities','owner_operations')) ORDER BY name").map_err(error)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    if rows.is_empty() {
        return Ok(false);
    }
    if rows.len() != expected.len() {
        return Err(error("owner_schema_unknown"));
    }
    for (name, sql) in rows {
        let original = expected
            .iter()
            .find(|(n, _)| *n == name)
            .ok_or_else(|| error("owner_schema_unknown"))?
            .1;
        if sql.trim_end_matches(';') != original {
            return Err(error("owner_schema_unknown"));
        }
    }
    Ok(true)
}

fn owner_start(db: &mut Connection) -> Result<Started, EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    if !owner_schema(&tx)? {
        for sql in [OWNER_META, OWNER_IDENTITIES, OWNER_OPERATIONS, OWNER_EVENT] {
            tx.execute_batch(sql).map_err(error)?;
        }
        tx.execute(
            "INSERT INTO owner_meta VALUES (1,1,?1,?2,'offline')",
            params![random_id()?, random_id()?],
        )
        .map_err(error)?;
        for kind in [OwnerClientKind::Tui, OwnerClientKind::Desktop] {
            tx.execute(
                "INSERT INTO owner_identities VALUES (?1,?2,1,'active')",
                params![random_id()?, kind.name()],
            )
            .map_err(error)?;
        }
    }
    crate::conversation::initialize(&tx)?;
    let count: i64 = tx
        .query_row("SELECT COUNT(*) FROM owner_meta", [], |r| r.get(0))
        .map_err(error)?;
    let (version, instance, old_engine, state): (i64, String, String, String) = tx.query_row("SELECT schema_version,db_instance,engine_epoch,runtime_state FROM owner_meta WHERE singleton=1", [], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(error)?;
    if count != 1
        || version != 1
        || !is_hex_id(&instance)
        || !is_hex_id(&old_engine)
        || !matches!(state.as_str(), "active" | "offline")
    {
        return Err(error("owner_metadata_invalid"));
    }
    let count: i64 = tx
        .query_row("SELECT COUNT(*) FROM owner_identities", [], |r| r.get(0))
        .map_err(error)?;
    if count != 2 {
        return Err(error("owner_registration_invalid"));
    }
    let host = HostStamp {
        db_instance: instance,
        engine_epoch: random_id()?,
    };
    tx.execute(
        "UPDATE owner_meta SET engine_epoch=?1,runtime_state='active' WHERE singleton=1",
        [&host.engine_epoch],
    )
    .map_err(error)?;
    let owners = [
        owner_registration(&tx, &host, OwnerClientKind::Tui)?,
        owner_registration(&tx, &host, OwnerClientKind::Desktop)?,
    ];
    assert_ne!(owners[0].admission.owner_id, owners[1].admission.owner_id);
    tx.commit().map_err(error)?;
    Ok(Started { host, owners })
}

fn check_owner_host(tx: &Transaction<'_>, stamp: &HostStamp) -> Result<(), EngineError> {
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_meta WHERE singleton=1 AND schema_version=1 AND db_instance=?1 AND engine_epoch=?2 AND runtime_state='active')", params![stamp.db_instance, stamp.engine_epoch], |r|r.get(0)).map_err(error)?;
    if !valid {
        return Err(error("owner_runtime_stale"));
    }
    Ok(())
}

fn owner_registration(
    tx: &Transaction<'_>,
    host: &HostStamp,
    kind: OwnerClientKind,
) -> Result<Registration, EngineError> {
    let (id, epoch, status): (String, i64, String) = tx
        .query_row(
            "SELECT owner_id,owner_epoch,status FROM owner_identities WHERE client_kind=?1",
            [kind.name()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(error)?;
    if !is_hex_id(&id) || epoch < 1 || !matches!(status.as_str(), "active" | "revoked") {
        return Err(error("owner_registration_invalid"));
    }
    Ok(Registration {
        kind,
        admission: Admission {
            db_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            owner_id: id,
            owner_epoch: epoch,
        },
        active: status == "active",
    })
}

fn check_owner_admission(tx: &Transaction<'_>, a: &Admission) -> Result<(), EngineError> {
    check_owner_host(
        tx,
        &HostStamp {
            db_instance: a.db_instance.clone(),
            engine_epoch: a.engine_epoch.clone(),
        },
    )?;
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_identities WHERE owner_id=?1 AND owner_epoch=?2 AND status='active')", params![a.owner_id,a.owner_epoch], |r|r.get(0)).map_err(error)?;
    if !valid {
        return Err(error("owner_grant_stale_or_revoked"));
    }
    Ok(())
}

fn owner_stop(db: &mut Connection, host: &HostStamp) -> Result<(), EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_host(&tx, host)?;
    tx.execute(
        "UPDATE owner_meta SET runtime_state='offline' WHERE singleton=1",
        [],
    )
    .map_err(error)?;
    tx.commit().map_err(error)
}

fn owner_revision(tx: &Transaction<'_>) -> Result<u64, EngineError> {
    let n: i64 = tx
        .query_row("SELECT revision FROM state WHERE id=1", [], |r| r.get(0))
        .map_err(error)?;
    u64::try_from(n).map_err(error)
}

fn owner_record(
    tx: &Transaction<'_>,
    provenance: &OwnerProvenance,
    hash: &[u8],
    result: &[u8],
) -> Result<(), EngineError> {
    assert_eq!(hash.len(), 32);
    if result.is_empty() || result.len() > 4096 {
        return Err(error("owner_result_bounds"));
    }
    tx.execute(
        "INSERT INTO owner_operations VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            provenance.origin,
            provenance.owner_id,
            provenance.operation_id.as_str(),
            hash,
            provenance.engine_epoch,
            provenance.admitted_epoch,
            provenance.resulting_epoch,
            provenance.revision_before.value() as i64,
            provenance.revision_after.value() as i64,
            result,
            provenance.event_cursor.as_ref().map(|c| c.value() as i64)
        ],
    )
    .map_err(error)?;
    assert!(!tx.is_autocommit());
    Ok(())
}

pub(crate) fn owner_lookup(
    tx: &Transaction<'_>,
    origin: &str,
    owner: &str,
    id: &RequestId,
    expected: Option<&str>,
) -> Result<Option<(OwnerProvenance, Vec<u8>)>, EngineError> {
    let row = tx.query_row("SELECT request_hash,engine_epoch,admitted_epoch,resulting_epoch,revision_before,revision_after,CASE WHEN length(result)<=4096 THEN result END,event_cursor FROM owner_operations WHERE origin=?1 AND owner_id=?2 AND operation_id=?3", params![origin,owner,id.as_str()], |r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,Option<Vec<u8>>>(6)?,r.get::<_,Option<i64>>(7)?))).optional().map_err(error)?;
    let Some((hash, engine, admitted, resulting, before, after, result, event)) = row else {
        return Ok(None);
    };
    let hex = hash.iter().map(|n| format!("{n:02x}")).collect::<String>();
    if !is_hex_id(&hex)
        || !is_hex_id(&engine)
        || admitted < 1
        || resulting < admitted
        || before < 0
        || after < before
        || event.is_some_and(|n| n < 1)
    {
        return Err(error("owner_ledger_invalid"));
    }
    if expected.is_some_and(|s| s != hex) {
        return Err(error("operation_identity_conflict"));
    }
    let provenance = OwnerProvenance {
        origin: origin.into(),
        owner_id: owner.into(),
        operation_id: id.clone(),
        request_hash: hex,
        engine_epoch: engine,
        admitted_epoch: admitted,
        resulting_epoch: resulting,
        revision_before: Cursor::new(before as u64),
        revision_after: Cursor::new(after as u64),
        event_cursor: event.map(|n| Cursor::new(n as u64)),
    };
    Ok(Some((
        provenance,
        result.ok_or_else(|| error("owner_result_bounds"))?,
    )))
}

fn owner_apply(
    db: &mut Connection,
    config: &EffectiveConfig,
    path: &Path,
    a: &Admission,
    request: &OwnerRequest,
) -> Result<OwnerApplied, EngineError> {
    request.validate()?;
    let OwnerOperation::Apply { mutation } = &request.operation else {
        return Err(error("owner_mutation_required"));
    };
    let command = mutation.command()?;
    let hash_text = request.mutation_hash()?;
    let hash = Sha256::digest(serde_json::to_vec(request).map_err(error)?);
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, a)?;
    let revision = owner_revision(&tx)?;
    if let Some((provenance, bytes)) = owner_lookup(
        &tx,
        "native_owner",
        &a.owner_id,
        &request.request_id,
        Some(&hash_text),
    )? {
        let result = serde_json::from_slice(&bytes).map_err(error)?;
        return Ok(OwnerApplied {
            result,
            provenance,
            historical_replay: true,
            current_revision: Cursor::new(revision),
        });
    }
    let outcome = if request.expected_revision.value() != revision {
        Outcome::Rejected {
            code: "stale_revision".into(),
        }
    } else {
        execute(&tx, config, path, &command, revision)?
    };
    let (result, changed) = match outcome {
        Outcome::Applied => {
            let result = if let Command::OwnerVerification {
                action: crate::verification::OwnerVerification::Verify { attempt_id, .. },
            } = &command
            {
                OwnerResult::VerificationUnavailable {
                    attempt_id: attempt_id.clone(),
                }
            } else {
                OwnerResult::Applied
            };
            (result, true)
        }
        Outcome::Rejected { code } => (OwnerResult::Rejected { code }, false),
        _ => return Err(error("owner_mutation_result_unavailable")),
    };
    let next = if changed {
        revision
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| error("revision_exhausted"))?
    } else {
        revision
    };
    if changed {
        let wire = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new(format!("owner:{}", a.owner_id)).map_err(error)?,
            request_id: request.request_id.clone(),
            expected_revision: request.expected_revision.clone(),
            command,
        };
        persist_event(&tx, config, &wire, next)?;
    }
    let provenance = OwnerProvenance {
        origin: "native_owner".into(),
        owner_id: a.owner_id.clone(),
        operation_id: request.request_id.clone(),
        request_hash: hash_text,
        engine_epoch: a.engine_epoch.clone(),
        admitted_epoch: a.owner_epoch,
        resulting_epoch: a.owner_epoch,
        revision_before: Cursor::new(revision),
        revision_after: Cursor::new(next),
        event_cursor: changed.then(|| Cursor::new(next)),
    };
    owner_record(
        &tx,
        &provenance,
        &hash,
        &serde_json::to_vec(&result).map_err(error)?,
    )?;
    tx.commit().map_err(error)?;
    assert_eq!(next > revision, changed);
    Ok(OwnerApplied {
        result,
        provenance,
        historical_replay: false,
        current_revision: Cursor::new(next),
    })
}

fn owner_reconcile(
    db: &mut Connection,
    a: &Admission,
    id: &RequestId,
    hash: Option<&str>,
) -> Result<OwnerResponse, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    let revision = Cursor::new(owner_revision(&tx)?);
    if settings::reconcile_pending(&tx, a, id, hash)? {
        return Ok(OwnerResponse::OutcomeUnknown {
            operation_id: id.clone(),
        });
    }
    Ok(
        match owner_lookup(&tx, "native_owner", &a.owner_id, id, hash)? {
            Some((provenance, bytes)) => OwnerResponse::Committed {
                operation: OwnerApplied {
                    result: serde_json::from_slice(&bytes).map_err(error)?,
                    provenance,
                    historical_replay: true,
                    current_revision: revision,
                },
            },
            None => OwnerResponse::NotRecordedAtBarrier { revision },
        },
    )
}

fn owner_control(
    db: &mut Connection,
    host: &HostStamp,
    control: &OwnerControl,
) -> Result<(OwnerControlReply, Registration), EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_host(&tx, host)?;
    let registration = owner_registration(&tx, host, control.target)?;
    let a = &registration.admission;
    let bytes = serde_json::to_vec(control).map_err(error)?;
    let hash_text = crate::memory::hash(&bytes);
    if let Some((_, result)) = owner_lookup(
        &tx,
        "trusted_host",
        &a.owner_id,
        &control.operation_id,
        Some(&hash_text),
    )? {
        return Ok((
            serde_json::from_slice(&result).map_err(error)?,
            registration,
        ));
    }
    if control.expected_epoch != a.owner_epoch {
        return Err(error("owner_control_stale"));
    }
    let next = a.owner_epoch.checked_add(1);
    if next.is_none() && control.decision == OwnerDecision::Reactivate {
        return Err(error("owner_epoch_exhausted"));
    }
    let reply = OwnerControlReply {
        owner_id: a.owner_id.clone(),
        owner_epoch: next.unwrap_or(i64::MAX),
        active: control.decision == OwnerDecision::Reactivate,
        epoch_exhausted_revoked: next.is_none(),
    };
    tx.execute(
        "UPDATE owner_identities SET owner_epoch=?1,status=?2 WHERE owner_id=?3",
        params![
            reply.owner_epoch,
            if reply.active { "active" } else { "revoked" },
            reply.owner_id
        ],
    )
    .map_err(error)?;
    let revision = owner_revision(&tx)?;
    let provenance = OwnerProvenance {
        origin: "trusted_host".into(),
        owner_id: a.owner_id.clone(),
        operation_id: control.operation_id.clone(),
        request_hash: hash_text,
        engine_epoch: host.engine_epoch.clone(),
        admitted_epoch: a.owner_epoch,
        resulting_epoch: reply.owner_epoch,
        revision_before: Cursor::new(revision),
        revision_after: Cursor::new(revision),
        event_cursor: None,
    };
    owner_record(
        &tx,
        &provenance,
        &Sha256::digest(&bytes),
        &serde_json::to_vec(&reply).map_err(error)?,
    )?;
    let registration = owner_registration(&tx, host, control.target)?;
    tx.commit().map_err(error)?;
    Ok((reply, registration))
}

fn owner_control_reconcile(
    db: &mut Connection,
    host: &HostStamp,
    control: &OwnerControl,
) -> Result<Option<OwnerControlReply>, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_host(&tx, host)?;
    let registration = owner_registration(&tx, host, control.target)?;
    let hash = crate::memory::hash(&serde_json::to_vec(control).map_err(error)?);
    owner_lookup(
        &tx,
        "trusted_host",
        &registration.admission.owner_id,
        &control.operation_id,
        Some(&hash),
    )?
    .map(|(_, b)| serde_json::from_slice(&b).map_err(error))
    .transpose()
}

fn owner_read(
    db: &mut Connection,
    config: &EffectiveConfig,
    path: &Path,
    a: &Admission,
    query: &OwnerRead,
) -> Result<OwnerReadReply, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    let revision = owner_revision(&tx)?;
    let data = match query {
        OwnerRead::WorkerDispatch { dispatch_id } => OwnerData::WorkerDispatch {
            dispatch: worker_dispatch::load(&tx, dispatch_id)?.map(|record| Box::new(record.view)),
        },
        OwnerRead::Settings => {
            let names: Vec<_> = config.descriptors.keys().cloned().collect();
            assert!(!names.is_empty());
            assert!(config.validate().is_ok());
            assert!(names.len() <= 25 + usize::from(config.settings.model_access.is_some()));
            OwnerData::Settings { names }
        }
        OwnerRead::Setting { name } => read_owner_setting(config, name)?,
        OwnerRead::ConversationHistory {
            project_id,
            cursor,
            limit,
        } => crate::conversation::history(
            &tx,
            &config.id,
            a,
            project_id,
            cursor.as_ref(),
            *limit,
            revision,
        )?,
        OwnerRead::CurrentBrief { project_id } => {
            crate::conversation::current_brief(&tx, project_id)?
        }
        OwnerRead::Page {
            view,
            cursor,
            limit,
        } => read_owner_page(&tx, config, a, *view, cursor.as_ref(), *limit, revision)?,
        OwnerRead::Baseline { baseline_id } => read_owner_baseline(&tx, baseline_id)?,
        OwnerRead::Capabilities => OwnerData::Capabilities {
            authenticated_owner_transport: true,
            native_worker_admission: false,
            human_consent_proven: false,
            native_input: false,
            merge_execution: false,
            settings_persistence: false,
        },
        _ => {
            let command = query
                .command()?
                .ok_or_else(|| error("owner_read_required"))?;
            OwnerData::Domain {
                outcome: Box::new(execute(&tx, config, path, &command, revision)?),
            }
        }
    };
    let reply = OwnerReadReply {
        revision: Cursor::new(revision),
        config_id: config.id.clone(),
        data,
    };
    if serde_json::to_vec(&reply).map_err(error)?.len() > FRAME_BYTES_MAX - 1024 {
        return Err(error("owner_response_bounds"));
    }
    Ok(reply)
}

fn read_owner_setting(config: &EffectiveConfig, name: &str) -> Result<OwnerData, EngineError> {
    let descriptor = config
        .descriptors
        .get(name)
        .ok_or_else(|| error("owner_setting_not_found"))?;
    assert_eq!(descriptor.name, name);
    assert!(config.validate().is_ok());
    assert!(config.descriptors.len() <= 25 + usize::from(config.settings.model_access.is_some()));
    Ok(OwnerData::Setting {
        descriptor: Box::new(descriptor.clone()),
    })
}

fn read_owner_baseline(tx: &Transaction<'_>, id: &BaselineId) -> Result<OwnerData, EngineError> {
    let (body,approved,revoked,current): (Option<String>,i64,i64,bool) = tx.query_row("SELECT CASE WHEN length(CAST(b.body AS BLOB))<=32768 THEN b.body END,b.approved,b.revoked,EXISTS(SELECT 1 FROM verification_current c WHERE c.project=b.project AND c.baseline=b.id) FROM verification_baselines b WHERE b.id=?1", [id.as_str()], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(error)?;
    let body = body.ok_or_else(|| error("owner_baseline_bounds"))?;
    let draft = crate::verification::BaselineDraft::import(body.as_bytes()).map_err(error)?;
    if draft.identity().map_err(error)? != *id
        || !matches!(approved, 0 | 1)
        || !matches!(revoked, 0 | 1)
    {
        return Err(error("owner_baseline_invalid"));
    }
    Ok(OwnerData::Baseline {
        draft,
        approved: approved == 1,
        revoked: revoked == 1,
        current,
    })
}

fn read_owner_page(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    a: &Admission,
    view: OwnerView,
    cursor: Option<&OwnerPageCursor>,
    limit: u8,
    revision: u64,
) -> Result<OwnerData, EngineError> {
    if !(1..=32).contains(&limit) {
        return Err(error("owner_page_limit"));
    }
    if let Some(c) = cursor {
        if c.db_instance != a.db_instance
            || c.owner_id != a.owner_id
            || c.engine_epoch != a.engine_epoch
            || c.owner_epoch != a.owner_epoch
            || c.config_id != config.id
            || c.at_revision.value() != revision
            || c.view != view
        {
            return Ok(OwnerData::ResyncRequired);
        }
        RequestId::new(c.after_id.clone()).map_err(error)?;
    }
    let after = cursor.map_or("", |c| c.after_id.as_str());
    let sql = match view {
        OwnerView::Projects => {
            "SELECT CASE WHEN length(id)<=128 THEN id END,CASE WHEN length(CAST(body AS BLOB))<=65536 THEN body END FROM projects WHERE id>?1 ORDER BY id LIMIT ?2"
        }
        OwnerView::Tasks => {
            "SELECT CASE WHEN length(id)<=128 THEN id END,CASE WHEN length(CAST(body AS BLOB))<=65536 THEN body END FROM tasks WHERE id>?1 ORDER BY id LIMIT ?2"
        }
        OwnerView::Candidates => {
            "SELECT CASE WHEN length(id)<=128 THEN id END,CASE WHEN length(CAST(body AS BLOB))<=65536 THEN body END FROM verification_candidates WHERE id>?1 ORDER BY id LIMIT ?2"
        }
        OwnerView::Plans => {
            "SELECT CASE WHEN length(id)<=128 THEN id END,CASE WHEN length(task)<=128 THEN task END FROM coordination_plans WHERE id>?1 ORDER BY id LIMIT ?2"
        }
        OwnerView::Baselines => {
            "SELECT CASE WHEN length(b.id)<=128 THEN b.id END,CASE WHEN length(b.project)<=128 THEN b.project END,b.approved,b.revoked,EXISTS(SELECT 1 FROM verification_current c WHERE c.project=b.project AND c.baseline=b.id) FROM verification_baselines b WHERE b.id>?1 ORDER BY b.id LIMIT ?2"
        }
    };
    let mut stmt = tx.prepare(sql).map_err(error)?;
    let mut rows = stmt
        .query(params![after, i64::from(limit) + 1])
        .map_err(error)?;
    let mut items = Vec::new();
    let mut last = None;
    let mut more = false;
    while let Some(row) = rows.next().map_err(error)? {
        if items.len() == usize::from(limit) {
            more = true;
            break;
        }
        let id: String = row
            .get::<_, Option<String>>(0)
            .map_err(error)?
            .ok_or_else(|| error("owner_row_bounds"))?;
        let body: String = row
            .get::<_, Option<String>>(1)
            .map_err(error)?
            .ok_or_else(|| error("owner_row_bounds"))?;
        RequestId::new(id.clone()).map_err(error)?;
        let item = match view {
            OwnerView::Projects => {
                let p: StoredProject = serde_json::from_str(&body).map_err(error)?;
                if p.project_id.as_str() != id
                    || p.spec.name.len() > 256
                    || p.spec.repositories.len() > 64
                {
                    return Err(error("owner_row_invalid"));
                }
                OwnerSummary::Project {
                    project_id: p.project_id,
                    name: p.spec.name,
                    repository_count: p.spec.repositories.len(),
                }
            }
            OwnerView::Tasks => {
                let t: ExecutionContext = serde_json::from_str(&body).map_err(error)?;
                if t.task_id.as_str() != id {
                    return Err(error("owner_row_invalid"));
                }
                OwnerSummary::Task {
                    task_id: t.task_id,
                    project_id: t.project_id,
                    config_id: t.config_id,
                    baseline_id: t.baseline_id,
                    snapshot_id: t.snapshot_id,
                }
            }
            OwnerView::Plans => OwnerSummary::Plan {
                plan_id: PlanId::new(&id).map_err(error)?,
                task_id: TaskId::new(body).map_err(error)?,
            },
            OwnerView::Candidates => {
                let c: crate::verification::Candidate =
                    serde_json::from_str(&body).map_err(error)?;
                if c.id.as_str() != id {
                    return Err(error("owner_row_invalid"));
                }
                OwnerSummary::Candidate {
                    id: c.id,
                    task_id: c.task_id,
                    snapshot_id: c.snapshot_id,
                    environment_id: c.environment_id,
                    pinned_baseline_id: c.pinned_baseline_id,
                    config_id: c.config_id,
                }
            }
            OwnerView::Baselines => {
                let approved: i64 = row.get(2).map_err(error)?;
                let revoked: i64 = row.get(3).map_err(error)?;
                if !matches!(approved, 0 | 1) || !matches!(revoked, 0 | 1) {
                    return Err(error("owner_row_invalid"));
                }
                OwnerSummary::Baseline {
                    baseline_id: BaselineId::new(&id).map_err(error)?,
                    project_id: ProjectId::new(body).map_err(error)?,
                    approved: approved == 1,
                    revoked: revoked == 1,
                    current: row.get(4).map_err(error)?,
                }
            }
        };
        items.push(item);
        if serde_json::to_vec(&items).map_err(error)?.len() > FRAME_BYTES_MAX - 4096 {
            items.pop();
            if items.is_empty() {
                return Err(error("owner_row_bounds"));
            }
            more = true;
            break;
        }
        last = Some(id);
    }
    let next = if more {
        Some(OwnerPageCursor {
            db_instance: a.db_instance.clone(),
            owner_id: a.owner_id.clone(),
            engine_epoch: a.engine_epoch.clone(),
            owner_epoch: a.owner_epoch,
            config_id: config.id.clone(),
            at_revision: Cursor::new(revision),
            view,
            after_id: last.ok_or_else(|| error("owner_page_progress"))?,
        })
    } else {
        None
    };
    assert!(items.len() <= usize::from(limit));
    Ok(OwnerData::Page { items, next })
}

#[cfg(test)]
mod owner_actor_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn gateway_actor_schema_initialization_is_writable_only() {
        let (_root, path, _) = fixture();
        let (db, lock) = open_database(&path, false).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('model_gateway_observations','model_gateway_attempts')",
                [],
                |row| row.get::<_, i64>(0)
            ).unwrap(),
            2
        );
        db.execute_batch(
            "DROP TABLE model_gateway_attempts; DROP TABLE model_gateway_observations;",
        )
        .unwrap();
        drop(db);
        drop(lock);
        let (db, lock) = open_database(&path, true).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name LIKE 'model_gateway_%'",
                [],
                |row| row.get::<_, i64>(0)
            ).unwrap(),
            0
        );
        drop(db);
        drop(lock);
        let (db, _lock) = open_database(&path, false).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('model_gateway_observations','model_gateway_attempts')",
                [],
                |row| row.get::<_, i64>(0)
            ).unwrap(),
            2
        );
    }

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, EffectiveConfig) {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("db");
        let cfg =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        (root, path, cfg)
    }

    async fn start(storage: &Storage) -> Started {
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerStart(tx))
            .unwrap_or_else(|_| panic!("queue"));
        rx.await.unwrap().unwrap()
    }

    #[test]
    fn settings_reader_asserts_impossible_internal_metadata_corruption() {
        for case in 0..4 {
            let (_root, path, mut cfg) = fixture();
            let (mut db, _lock) = open_database(&path, false).unwrap();
            let started = owner_start(&mut db).unwrap();
            let owner = &started.owners[0].admission;
            let query = serde_json::from_value(serde_json::json!({"kind":"Settings"})).unwrap();
            assert!(owner_read(&mut db, &cfg, &path, owner, &query).is_ok());
            match case {
                0 => cfg.descriptors.clear(),
                1 | 3 => {
                    let descriptor = cfg.descriptors.values().next().unwrap().clone();
                    for index in 0..26 {
                        cfg.descriptors
                            .insert(format!("invalid-{index}"), descriptor.clone());
                    }
                }
                _ => {
                    cfg.descriptors.get_mut("timeout_ms").unwrap().name = "corrupt".into();
                }
            }
            let query = if case >= 2 {
                serde_json::from_value(serde_json::json!({"kind":"Setting","name":"timeout_ms"}))
                    .unwrap()
            } else {
                query
            };
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                owner_read(&mut db, &cfg, &path, owner, &query)
            }));
            assert!(
                outcome.is_err(),
                "internal corruption case {case} must trip an invariant"
            );
            let tx = db.transaction().unwrap();
            assert_eq!(owner_revision(&tx).unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn queued_owner_read_rechecks_epoch_after_revoke() {
        let (_root, path, cfg) = fixture();
        let storage = Storage::open(&path, cfg, false).await.unwrap();
        let started = start(&storage).await;
        let owner = started.owners[0].admission.clone();
        let control = OwnerControl {
            operation_id: RequestId::new("revoke").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: owner.owner_epoch,
            decision: OwnerDecision::Revoke,
        };
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerControl(started.host, control, tx))
            .unwrap_or_else(|_| panic!("queue"));
        let (read_tx, read_rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerRead(owner, OwnerRead::Capabilities, read_tx))
            .unwrap_or_else(|_| panic!("queue"));
        assert!(!rx.await.unwrap().unwrap().0.active);
        assert!(read_rx.await.unwrap().is_err());
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::Close(tx))
            .unwrap_or_else(|_| panic!("queue"));
        rx.await.unwrap().unwrap();
    }

    fn mutation(id: &str) -> OwnerRequest {
        OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::Coordination {
                    action: crate::coordination::OwnerAction::ApprovePlan {
                        plan_id: PlanId::new("missing").unwrap(),
                    },
                },
            },
        }
    }

    #[tokio::test]
    async fn fifo_apply_revoke_and_dropped_ack_have_real_actor_barriers() {
        let (_root, path, cfg) = fixture();
        use crate::verification::*;
        let draft = BaselineDraft {
            version: 1,
            project_id: ProjectId::new("p").unwrap(),
            origin: BaselineOrigin::Imported,
            requirements: "exact accepted checks".into(),
            checks: vec![
                CheckSpec {
                    id: RequestId::new("code").unwrap(),
                    kind: CheckKind::Code,
                    minimum_risk: Risk::Low,
                    content: b"code".to_vec(),
                },
                CheckSpec {
                    id: RequestId::new("integrity").unwrap(),
                    kind: CheckKind::TestIntegrity,
                    minimum_risk: Risk::Low,
                    content: b"integrity".to_vec(),
                },
            ],
            rules: vec![],
            retry_limit: 0,
        };
        let baseline = draft.identity().unwrap();
        {
            let (mut db, _lock) = open_database(&path, false).unwrap();
            for (n, command) in [
                Command::CreateProject {
                    project_id: draft.project_id.clone(),
                    spec: ProjectSpec {
                        name: "Project".into(),
                        repositories: vec![],
                    },
                },
                Command::ProposeBaseline { draft },
            ]
            .into_iter()
            .enumerate()
            {
                let req = WireRequest {
                    protocol_version: PROTOCOL_VERSION,
                    client_id: ClientId::new("host").unwrap(),
                    request_id: RequestId::new(format!("seed-{n}")).unwrap(),
                    expected_revision: Cursor::new(n as u64),
                    command,
                };
                assert_eq!(
                    apply(&mut db, &cfg, &path, req).unwrap().outcome,
                    Outcome::Applied
                );
            }
        }
        let storage = Storage::open(&path, cfg.clone(), false).await.unwrap();
        let started = start(&storage).await;
        let owner = started.owners[0].admission.clone();
        let req = OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("lost-ack").unwrap(),
            expected_revision: Cursor::new(2),
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::ApproveBaseline {
                    baseline_id: baseline.clone(),
                },
            },
        };
        let (tx, rx) = oneshot::channel();
        drop(rx);
        storage
            .sender
            .try_send(Job::OwnerApply(owner.clone(), req.clone(), tx))
            .unwrap_or_else(|_| panic!("queue"));
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerReconcile(
                owner.clone(),
                req.request_id.clone(),
                Some(req.mutation_hash().unwrap()),
                tx,
            ))
            .unwrap_or_else(|_| panic!("queue"));
        let recovered = rx.await.unwrap().unwrap();
        let OwnerResponse::Committed {
            operation: original,
        } = recovered
        else {
            panic!("committed")
        };
        assert_eq!(original.result, OwnerResult::Applied);
        let control = OwnerControl {
            operation_id: RequestId::new("lost-ack").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: 1,
            decision: OwnerDecision::Revoke,
        };
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerControl(started.host.clone(), control.clone(), tx))
            .unwrap_or_else(|_| panic!("queue"));
        let (late_tx, late_rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerApply(
                owner.clone(),
                mutation("after-revoke"),
                late_tx,
            ))
            .unwrap_or_else(|_| panic!("queue"));
        assert!(!rx.await.unwrap().unwrap().0.active);
        assert!(late_rx.await.unwrap().is_err());
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerReconcile(
                owner.clone(),
                req.request_id.clone(),
                None,
                tx,
            ))
            .unwrap_or_else(|_| panic!("queue"));
        assert!(rx.await.unwrap().is_err());
        let reactivate = OwnerControl {
            operation_id: RequestId::new("reactivate").unwrap(),
            expected_epoch: 2,
            decision: OwnerDecision::Reactivate,
            ..control.clone()
        };
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerControl(started.host, reactivate, tx))
            .unwrap_or_else(|_| panic!("queue"));
        assert!(rx.await.unwrap().unwrap().0.active);
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::Close(tx))
            .unwrap_or_else(|_| panic!("queue"));
        rx.await.unwrap().unwrap();
        let storage = Storage::open(&path, cfg, false).await.unwrap();
        let restarted = start(&storage).await;
        assert_eq!(restarted.owners[0].admission.owner_id, owner.owner_id);
        assert_ne!(restarted.host.engine_epoch, owner.engine_epoch);
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerApply(owner, req.clone(), tx))
            .unwrap_or_else(|_| panic!("queue"));
        assert!(rx.await.unwrap().is_err());
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerApply(
                restarted.owners[0].admission.clone(),
                req,
                tx,
            ))
            .unwrap_or_else(|_| panic!("queue"));
        let replay = rx.await.unwrap().unwrap();
        assert!(replay.historical_replay);
        assert_eq!(replay.provenance, original.provenance);
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerControlReconcile(restarted.host, control, tx))
            .unwrap_or_else(|_| panic!("queue"));
        assert!(!rx.await.unwrap().unwrap().unwrap().active);
        let (tx, rx) = oneshot::channel();
        storage
            .sender
            .try_send(Job::Close(tx))
            .unwrap_or_else(|_| panic!("queue"));
        rx.await.unwrap().unwrap();
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM owner_operations WHERE origin='native_owner'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM operations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT revision FROM state", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            db.query_row(
                "SELECT approved FROM verification_baselines WHERE id=?1",
                [baseline.as_str()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn owner_schema_is_additive_and_unknown_schema_is_preserved() {
        let (_root, path, _cfg) = fixture();
        let (mut db, _lock) = open_database(&path, false).unwrap();
        db.execute(
            "INSERT INTO operations VALUES ('owner:tui','legacy',x'01',x'02')",
            [],
        )
        .unwrap();
        let started = owner_start(&mut db).unwrap();
        assert_ne!(
            started.owners[0].admission.owner_id,
            started.owners[1].admission.owner_id
        );
        assert_eq!(
            db.query_row(
                "SELECT request FROM operations WHERE client='owner:tui'",
                [],
                |r| r.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
            vec![1]
        );
        owner_stop(&mut db, &started.host).unwrap();
        db.execute("UPDATE owner_meta SET schema_version=2", [])
            .unwrap_err();
        db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE owner_meta SET schema_version=2; PRAGMA ignore_check_constraints=OFF;").unwrap();
        assert!(owner_start(&mut db).is_err());
        assert_eq!(
            db.query_row("SELECT schema_version FROM owner_meta", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let other = tempfile::tempdir().unwrap();
        std::fs::set_permissions(other.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (mut conflict, _other_lock) = open_database(&other.path().join("db"), false).unwrap();
        conflict
            .execute("CREATE TABLE owner_meta (unknown TEXT)", [])
            .unwrap();
        conflict
            .execute("INSERT INTO owner_meta VALUES ('preserve')", [])
            .unwrap();
        assert!(owner_start(&mut conflict).is_err());
        assert_eq!(
            conflict
                .query_row("SELECT unknown FROM owner_meta", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "preserve"
        );
        assert_eq!(
            conflict
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name='owner_identities'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn internal_ledger_hash_shape_is_a_runtime_invariant() {
        let (_root, path, cfg) = fixture();
        let (mut db, _lock) = open_database(&path, false).unwrap();
        let started = owner_start(&mut db).unwrap();
        let result = owner_apply(
            &mut db,
            &cfg,
            &path,
            &started.owners[0].admission,
            &mutation("shape"),
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        owner_record(&tx, &result.provenance, &[0; 31], b"{}").unwrap();
    }

    #[test]
    fn epoch_exhaustion_revokes_and_cannot_reactivate() {
        let (_root, path, _cfg) = fixture();
        let (mut db, _lock) = open_database(&path, false).unwrap();
        let started = owner_start(&mut db).unwrap();
        db.execute(
            "UPDATE owner_identities SET owner_epoch=?1 WHERE client_kind='tui'",
            [i64::MAX],
        )
        .unwrap();
        let control = OwnerControl {
            operation_id: RequestId::new("exhausted").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: i64::MAX,
            decision: OwnerDecision::Revoke,
        };
        let (reply, _) = owner_control(&mut db, &started.host, &control).unwrap();
        assert!(!reply.active);
        assert!(reply.epoch_exhausted_revoked);
        assert_eq!(reply.owner_epoch, i64::MAX);
        let reactivate = OwnerControl {
            operation_id: RequestId::new("reactivate").unwrap(),
            decision: OwnerDecision::Reactivate,
            ..control.clone()
        };
        assert!(owner_control(&mut db, &started.host, &reactivate).is_err());
        assert!(
            !owner_control(&mut db, &started.host, &control)
                .unwrap()
                .0
                .active
        );
    }

    #[test]
    fn owner_ledger_abort_rolls_back_domain_effect_and_pages_bound_corrupt_body() {
        let (_root, path, cfg) = fixture();
        let (mut db, _lock) = open_database(&path, false).unwrap();
        let started = owner_start(&mut db).unwrap();
        let a = &started.owners[0].admission;
        db.execute(
            "INSERT INTO projects VALUES ('huge',?1)",
            ["x".repeat(FRAME_BYTES_MAX + 1)],
        )
        .unwrap();
        assert!(
            owner_read(
                &mut db,
                &cfg,
                &path,
                a,
                &OwnerRead::Page {
                    view: OwnerView::Projects,
                    cursor: None,
                    limit: 2
                }
            )
            .is_err()
        );
        db.execute("DELETE FROM projects WHERE id='huge'", [])
            .unwrap();
        use crate::verification::*;
        let draft = BaselineDraft {
            version: 1,
            project_id: ProjectId::new("p").unwrap(),
            origin: BaselineOrigin::Imported,
            requirements: "approved checks".into(),
            checks: vec![
                CheckSpec {
                    id: RequestId::new("code").unwrap(),
                    kind: CheckKind::Code,
                    minimum_risk: Risk::Low,
                    content: b"code".to_vec(),
                },
                CheckSpec {
                    id: RequestId::new("integrity").unwrap(),
                    kind: CheckKind::TestIntegrity,
                    minimum_risk: Risk::Low,
                    content: b"integrity".to_vec(),
                },
            ],
            rules: vec![],
            retry_limit: 0,
        };
        let baseline = draft.identity().unwrap();
        for (n, command) in [
            Command::CreateProject {
                project_id: draft.project_id.clone(),
                spec: ProjectSpec {
                    name: "Project".into(),
                    repositories: vec![],
                },
            },
            Command::ProposeBaseline { draft },
        ]
        .into_iter()
        .enumerate()
        {
            let input = WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("host").unwrap(),
                request_id: RequestId::new(format!("fixture-{n}")).unwrap(),
                expected_revision: Cursor::new(n as u64),
                command,
            };
            assert_eq!(
                apply(&mut db, &cfg, &path, input).unwrap().outcome,
                Outcome::Applied
            );
        }
        db.execute_batch("CREATE TRIGGER owner_abort BEFORE INSERT ON owner_operations BEGIN SELECT RAISE(ABORT,'real_sqlite_abort'); END;").unwrap();
        let before: i64 = db
            .query_row("SELECT COUNT(*) FROM owner_operations", [], |r| r.get(0))
            .unwrap();
        let approve = OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("aborted").unwrap(),
            expected_revision: Cursor::new(2),
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::ApproveBaseline {
                    baseline_id: baseline.clone(),
                },
            },
        };
        assert!(
            owner_apply(&mut db, &cfg, &path, a, &approve)
                .unwrap_err()
                .0
                .contains("real_sqlite_abort")
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM owner_operations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            before
        );
        assert_eq!(
            db.query_row("SELECT revision FROM state", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row(
                "SELECT approved FROM verification_baselines WHERE id=?1",
                [baseline.as_str()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}

#[cfg(test)]
mod knowledge_storage_tests {
    use super::*;
    use crate::memory::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn bounded_sqlite_full_rolls_back_knowledge_and_operation_acknowledgement() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("db");
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let (mut db, _lock) = open_database(&path, false).unwrap();
        let mut revision = 0;
        let mut sequence = 0;
        let project_id = ProjectId::new("p").unwrap();
        let task_id = TaskId::new("t").unwrap();
        let mut call = |db: &mut Connection, command| {
            sequence += 1;
            let reply = apply(
                db,
                &config,
                &path,
                WireRequest {
                    protocol_version: PROTOCOL_VERSION,
                    client_id: ClientId::new("host").unwrap(),
                    request_id: RequestId::new(format!("op-{sequence}")).unwrap(),
                    expected_revision: Cursor::new(revision),
                    command,
                },
            );
            if let Ok(reply) = &reply {
                revision = reply.revision.value();
            }
            reply
        };
        assert_eq!(
            call(
                &mut db,
                Command::CreateProject {
                    project_id: project_id.clone(),
                    spec: ProjectSpec {
                        name: "p".into(),
                        repositories: vec![]
                    }
                }
            )
            .unwrap()
            .outcome,
            Outcome::Applied
        );
        assert_eq!(
            call(
                &mut db,
                Command::PinTask {
                    context: ExecutionContext {
                        project_id: project_id.clone(),
                        task_id: task_id.clone(),
                        config_id: config.id.clone(),
                        snapshot_id: SnapshotId::new("s").unwrap(),
                        baseline_id: BaselineId::new("b").unwrap(),
                        allowed_destinations: vec![],
                        budget_ref: CredentialRef::new("budget").unwrap(),
                    }
                }
            )
            .unwrap()
            .outcome,
            Outcome::Applied
        );
        assert_eq!(
            call(
                &mut db,
                Command::Knowledge {
                    action: KnowledgeAction::Admit {
                        principal: Principal {
                            id: RequestId::new("writer").unwrap(),
                            project_id,
                            task_id,
                            session_id: SessionId::new("session").unwrap(),
                            role: KnowledgeRole::Contributor,
                        }
                    }
                }
            )
            .unwrap()
            .outcome,
            Outcome::Applied
        );
        let pages: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
        let bounded: i64 = db
            .query_row(&format!("PRAGMA max_page_count={pages}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(pages, bounded);
        let before: (i64, i64) = db
            .query_row(
                "SELECT revision,(SELECT COUNT(*) FROM operations) FROM state",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let result = call(
            &mut db,
            Command::Knowledge {
                action: KnowledgeAction::Capture {
                    principal_id: RequestId::new("writer").unwrap(),
                    activity: ActivityInput {
                        id: RequestId::new("full").unwrap(),
                        source_id: RequestId::new("source").unwrap(),
                        category: CaptureCategory::ToolOutput,
                        text: "x".repeat(config.settings.knowledge.excerpt_bytes_max),
                        gaps: vec![],
                    },
                },
            },
        );
        assert!(result.unwrap_err().0.contains("database or disk is full"));
        let after: (i64, i64) = db
            .query_row(
                "SELECT revision,(SELECT COUNT(*) FROM operations) FROM state",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM knowledge_activity", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM knowledge_sources", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(db.is_autocommit());
    }
}

#[cfg(test)]
mod database_lock_tests {
    use super::*;

    #[test]
    fn release_does_not_depend_on_a_stale_file_description_alias() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let lock = lock_database(&path).unwrap();
        let stale_alias = lock.try_clone().unwrap();
        assert!(lock_database(&path).is_err());

        release_database_lock(lock).unwrap();
        let new_owner = lock_database(&path).unwrap();
        drop(stale_alias);
        assert!(lock_database(&path).is_err());

        release_database_lock(new_owner).unwrap();
        let final_owner = lock_database(&path).unwrap();
        release_database_lock(final_owner).unwrap();
    }
}

#[cfg(test)]
pub(crate) use configured_conversation_fixture::{
    ConfiguredInitialConversationFixture, configured_initial_conversation_fixture,
};

#[cfg(test)]
mod configured_conversation_fixture {
    use super::*;
    use crate::master_session::{
        MasterBackend, MasterOwnerAction, MasterSelection, MasterSessionView,
        RcliConfiguredProfileProof,
    };
    use crate::model_gateway::AttemptRequest;
    use crate::routing::{ResourceBudget, RouteRequest};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    const CONFIG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["configured-profile"]
[routing]
approved_accounts = ["configured-account"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["configured-account"]
[[model_access.connections]]
id = "configured-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "configured-account"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "local-model", author = "local-author" }
auth = { method = "none" }
[[model_access.pools]]
id = "configured-pool"
members = ["configured-connection"]
projects = ["configured-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

    pub(crate) struct ConfiguredInitialConversationFixture {
        pub(crate) db: Connection,
        pub(crate) config: EffectiveConfig,
        pub(crate) request: AttemptRequest,
        pub(crate) proof: RcliConfiguredProfileProof,
        pub(crate) binding: NativeProfileBinding,
        pub(crate) generation: u64,
        pub(crate) host: HostStamp,
        pub(crate) session: MasterSessionView,
        pub(crate) database_path: PathBuf,
        pub(crate) global_path: PathBuf,
        pub(crate) profile_home: PathBuf,
        storage: Storage,
        root: tempfile::TempDir,
    }

    impl ConfiguredInitialConversationFixture {
        pub(crate) async fn close(self) {
            let (send, receive) = oneshot::channel();
            self.storage.sender.try_send(Job::OwnerStop(self.host, send))
                .unwrap_or_else(|_| panic!("actual configured fixture owner stop queue unavailable"));
            receive.await.unwrap().unwrap();
            let (send, receive) = oneshot::channel();
            self.storage.sender.try_send(Job::Close(send))
                .unwrap_or_else(|_| panic!("actual configured fixture actor close queue unavailable"));
            receive.await.unwrap().unwrap();
            self.db.close().map_err(|(_, error)| error).unwrap();
            assert!(self.root.path().is_dir());
        }
    }

    fn reference(value: &str) -> CredentialRef {
        CredentialRef::new(value).unwrap()
    }

    fn private_file(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    async fn read(storage: &Storage, started: &Started, query: OwnerRead) -> OwnerReadReply {
        let (send, receive) = oneshot::channel();
        storage.sender.try_send(Job::OwnerRead(started.owners[0].admission.clone(), query, send))
            .unwrap_or_else(|_| panic!("actual configured fixture owner read queue unavailable"));
        receive.await.unwrap().unwrap()
    }

    async fn apply(storage: &Storage, started: &Started, id: &str, mutation: OwnerMutation) {
        let basis = read(storage, started, OwnerRead::Capabilities).await;
        let (send, receive) = oneshot::channel();
        storage.sender.try_send(Job::OwnerApply(started.owners[0].admission.clone(), OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: basis.revision,
            operation: OwnerOperation::Apply { mutation },
        }, send)).unwrap_or_else(|_| panic!("actual configured fixture owner mutation queue unavailable"));
        assert_eq!(receive.await.unwrap().unwrap().result, OwnerResult::Applied);
    }

    pub(crate) async fn configured_initial_conversation_fixture() -> ConfiguredInitialConversationFixture {
        let root = tempfile::Builder::new().prefix("ca-cgf-").tempdir_in("/private/tmp").unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global_path = root.path().join("global.toml");
        private_file(&global_path, CONFIG.as_bytes());
        let profile_home = root.path().join("configured-home");
        std::fs::create_dir(&profile_home).unwrap();
        std::fs::set_permissions(&profile_home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let sdk = profile_home.join(".backboard");
        std::fs::create_dir(&sdk).unwrap();
        std::fs::set_permissions(&sdk, std::fs::Permissions::from_mode(0o700)).unwrap();
        private_file(&sdk.join("config.json"), &serde_json::to_vec(&serde_json::json!({
            "model": { "provider": "local-provider", "model": "local-model" },
            "providers": [{"id": "local-provider", "name": "Local provider", "protocol": "openai-chat",
                "baseUrl": "http://127.0.0.1:11434/v1", "auth": { "type": "none" }}]
        })).unwrap());
        let database_path = root.path().join("engine.db");
        let (storage, config) = Storage::open_with_settings_sources(
            &database_path, &global_path, &root.path().join("project.toml"),
        ).await.unwrap();
        let (send, receive) = oneshot::channel();
        storage.sender.try_send(Job::OwnerStart(send))
            .unwrap_or_else(|_| panic!("actual configured fixture owner start queue unavailable"));
        let started = receive.await.unwrap().unwrap();
        let project = ProjectId::new("configured-project").unwrap();
        apply(&storage, &started, "create-configured-project", OwnerMutation::CreateProject {
            project_id: project.clone(),
            spec: ProjectSpec { name: "Configured conversation gateway".into(), repositories: vec![] },
        }).await;
        let basis = read(&storage, &started, OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools, cursor: None, limit: 1,
        }).await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("actual configured fixture source basis missing");
        };
        apply(&storage, &started, "open-configured-master", OwnerMutation::ProjectMaster {
            action: MasterOwnerAction::Open {
                project_id: project.clone(), expected_config_id: basis.config_id,
                expected_source_hash: source_hash,
                selection: MasterSelection {
                    backend: MasterBackend::Rcli, pool_ref: reference("configured-pool"),
                    target_profile_ref: reference("configured-profile"),
                    route: RouteRequest {
                        preferred_target: Some(reference("configured-profile")),
                        required_target: Some(reference("configured-profile")),
                        required_capabilities: vec![], account_ref: reference("configured-account"),
                        budget_ref: reference("conversation-budget"), resources: ResourceBudget::unit(),
                    },
                },
            },
        }).await;
        let state = read(&storage, &started, OwnerRead::ProjectMaster { project_id: project.clone() }).await;
        let OwnerData::ProjectMaster { session: Some(session) } = state.data else {
            panic!("actual configured fixture committed session missing");
        };
        apply(&storage, &started, "submit-configured-conversation", OwnerMutation::ProjectMaster {
            action: MasterOwnerAction::Submit {
                project_id: project.clone(), session_id: session.session_id.clone(),
                text: "Describe this project without using tools.".into(),
            },
        }).await;
        let state = read(&storage, &started, OwnerRead::ProjectMaster { project_id: project.clone() }).await;
        let OwnerData::ProjectMaster { session: Some(session) } = state.data else {
            panic!("actual configured fixture queued session missing");
        };
        let (send, receive) = oneshot::channel();
        storage.sender.try_send(Job::MasterRuntime(Box::new(MasterRuntimeCommand::RegisterRcliProfile {
            host: started.host.clone(), project_id: project.clone(),
            session_id: session.session_id.clone(), configured_home: profile_home.clone(),
        }), send)).unwrap_or_else(|_| panic!("actual configured fixture registration queue unavailable"));
        let MasterRuntimeResult::ConfiguredProfile(proof) = receive.await.unwrap().unwrap() else {
            panic!("actual configured fixture persisted registration missing");
        };
        let proof = *proof;
        assert_eq!(proof.database_instance, started.host.db_instance);
        assert_eq!(proof.engine_epoch, started.host.engine_epoch);
        assert_eq!(proof.config_id, config.id);
        assert_eq!(proof.source_hash, session.source_hash);
        assert_eq!(proof.profile_ref, session.selection.target_profile_ref);
        assert!(proof.generation > 0);
        let binding = NativeProfileBinding {
            database_instance: proof.database_instance.clone(), engine_epoch: proof.engine_epoch.clone(),
            config_id: proof.config_id.clone(), source_hash: proof.source_hash.clone(),
            at_revision: proof.at_revision.clone(), connection: proof.connection.clone(),
            custody_id: proof.custody_id.clone(),
        };
        let request = AttemptRequest {
            id: RequestId::new("configured-gateway-first-attempt").unwrap(),
            task_id: session.bootstrap_task_id.clone(), project_id: project,
            role: crate::coordination::Role::ProjectLeader,
            pool_ref: session.selection.pool_ref.clone(),
            target_profile_ref: session.selection.target_profile_ref.clone(),
            route: session.selection.route.clone(), previous_attempt: None,
        };
        let generation = proof.generation;
        let db = Connection::open_with_flags(&database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(db.query_row("SELECT COUNT(*) FROM model_gateway_attempts", [],
            |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM native_auth_operations", [],
            |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM model_gateway_observations", [],
            |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert!(!profile_home.join(".backboard/keys.json").exists());
        ConfiguredInitialConversationFixture {
            db, config, request, proof, binding, generation, host: started.host, session,
            database_path, global_path, profile_home, storage, root,
        }
    }
}
