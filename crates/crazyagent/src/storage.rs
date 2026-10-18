use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::protocol::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use tokio::sync::{mpsc, oneshot};

pub(crate) enum Job {
    Apply(WireRequest, oneshot::Sender<Result<WireReply, EngineError>>),
    Config(EffectiveConfig, oneshot::Sender<Result<(), EngineError>>),
    Close(oneshot::Sender<()>),
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
        let (sender, mut receiver) = mpsc::channel(config.settings.queue_capacity);
        let path = path.to_path_buf();
        let (ready_send, ready_receive) = oneshot::channel();
        std::thread::Builder::new()
            .name("crazyagent-sqlite".into())
            .spawn(move || {
                let (mut connection, lock) = match open_database(&path, readonly) {
                    Ok(database) => {
                        let _ = ready_send.send(Ok(()));
                        database
                    }
                    Err(e) => {
                        let _ = ready_send.send(Err(e));
                        return;
                    }
                };
                let mut config = config;
                while let Some(job) = receiver.blocking_recv() {
                    match job {
                        Job::Apply(request, reply) => {
                            let _ = reply.send(apply(&mut connection, &config, &path, request));
                        }
                        Job::Config(next, reply) => {
                            let result = store_config(&connection, &next);
                            if result.is_ok() {
                                config = next;
                            }
                            let _ = reply.send(result);
                        }
                        Job::Close(reply) => {
                            drop(connection);
                            drop(lock);
                            let _ = reply.send(());
                            break;
                        }
                    }
                }
            })
            .map_err(error)?;
        ready_receive.await.map_err(error)??;
        Ok(Self { sender })
    }
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn open_database(path: &Path, readonly: bool) -> Result<(Connection, std::fs::File), EngineError> {
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
        initialize_schema(&connection)?;
        crate::coordination::initialize(&connection)?;
        crate::memory::initialize(&connection)?;
        crate::assets::initialize(&connection)?;
        crate::worktrees::initialize(&connection)?;
        crate::worktrees::reconcile(&connection)?;
    }
    assert!(connection.is_autocommit());
    assert_eq!(
        std::fs::symlink_metadata(path).map_err(error)?.ino(),
        metadata.ino()
    );
    Ok((connection, lock))
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
        .map_err(error)?;
    assert_eq!(lock.metadata().map_err(error)?.ino(), lock_metadata.ino());
    assert_eq!(lock.metadata().map_err(error)?.dev(), lock_metadata.dev());
    Ok(lock)
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
            CREATE TABLE IF NOT EXISTS events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);")
        .map_err(error)?;
    assert!(connection.is_autocommit());
    Ok(())
}

fn apply(
    connection: &mut Connection,
    config: &EffectiveConfig,
    path: &Path,
    request: WireRequest,
) -> Result<WireReply, EngineError> {
    let mut bytes = serde_json::to_vec(&request).map_err(error)?;
    if matches!(
        request.command,
        Command::Knowledge { .. } | Command::Assets { .. }
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
    ) || crate::coordination::is_mutation(&request.command)
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
            || matches!(&request.command, Command::Assets { action } if !crate::assets::is_mutation(action));
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

fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    path: &Path,
    command: &Command,
    revision: u64,
) -> Result<Outcome, EngineError> {
    match command {
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
    let event = Event {
        cursor: Cursor::new(revision),
        request_id: request.request_id.clone(),
        client_id: request.client_id.clone(),
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
