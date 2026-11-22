use crate::StorageError;
use crate::migrations::{DATABASE, Texts, backup, error, forget_backup_failure, recorded, schema};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(all(test, unix))]
mod tests;

/// One forward engine.db schema step. `to` is the version the database reaches when
/// the upgrade commits; `run` makes the change inside the upgrade transaction and
/// must leave that transaction open.
pub struct Step {
    pub to: i64,
    pub run: StepRun,
}

/// The body of one schema step: it changes the database through the open upgrade
/// transaction and returns the cause when it cannot.
pub type StepRun = fn(&Transaction<'_>) -> Result<(), StorageError>;

/// The newest version the steps reach. Input: the ordered steps, which must be
/// numbered from 2 without gaps. Output: that version, or 1 when there are no steps.
fn latest(steps: &[Step]) -> i64 {
    let mut expected = 2;
    for step in steps {
        assert_eq!(
            step.to, expected,
            "migration steps are numbered from 2 without gaps"
        );
        expected += 1;
    }
    expected - 1
}

/// Reads the version of record. Input: the connection. Output: `PRAGMA user_version`.
pub(crate) fn version(db: &Connection) -> Result<i64, StorageError> {
    db.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(error)
}

/// Says where engine.db stands and refuses one written by a newer engine.
/// Input: the connection and the newest version this engine knows. Output: None for a
/// database with no schema yet, otherwise its version, where the unset 0 reads as 1.
fn standing(db: &Connection, latest: i64) -> Result<Option<i64>, StorageError> {
    let found = version(db)?;
    if found > latest {
        return Err(error(format!(
            "storage_newer_than_engine: the engine database is at version {found} and this \
             engine reads up to version {latest}; install the newer CrazyAgent that wrote it"
        )));
    }
    if found < 0 {
        return Err(error(format!(
            "storage_version_invalid: the engine database reports version {found}"
        )));
    }
    let empty = !db
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema)", [], |row| {
            row.get::<_, bool>(0)
        })
        .map_err(error)?;
    Ok((!empty).then_some(found.max(1)))
}

/// Refuses a read-only open of engine.db that a newer engine wrote or that still needs
/// an upgrade. Input: the read-only connection and the steps. Output: Ok when the
/// database is empty or at the newest version.
pub fn check(db: &Connection, steps: &[Step]) -> Result<(), StorageError> {
    let latest = latest(steps);
    match standing(db, latest)? {
        Some(at) if at < latest => Err(error(format!(
            "storage_upgrade_required: the engine database is at version {at}; open it \
             writable once so this engine can upgrade it to version {latest}"
        ))),
        _ => Ok(()),
    }
}

/// Brings engine.db to the newest version before anything else writes to it, after
/// forgetting a finished restore's record on unix. A database with no schema is stamped newest. An older one is backed up through
/// [`crate::migrations::backup`], whose copy check always runs, then every pending step runs
/// in one transaction. Inputs: the writable connection, its path, the settings texts to keep
/// beside the backup and the steps. Output: Ok at the newest version, or an error naming the
/// backup. A newer database is refused and never written.
pub fn upgrade(
    db: &mut Connection,
    path: &Path,
    texts: Texts<'_>,
    steps: &[Step],
) -> Result<(), StorageError> {
    #[cfg(unix)]
    crate::restore::forget_restore(path);
    upgrade_checked(db, path, texts, steps, SystemTime::now(), backup)
}

/// [`upgrade`] with a given time and backup, so tests reach a copy mismatch on purpose.
/// Turned on only by the `test-support` feature from crazyagent's dev-dependencies. Inputs:
/// as `upgrade`, plus the time and the backup. Output: as `upgrade`.
#[cfg(feature = "test-support")]
pub fn upgrade_at_for_test(
    db: &mut Connection,
    path: &Path,
    texts: Texts<'_>,
    steps: &[Step],
    now: SystemTime,
    take: Backup,
) -> Result<(), StorageError> {
    upgrade_checked(db, path, texts, steps, now, take)
}

/// The backup an upgrade takes before any step: [`upgrade`] always passes
/// [`crate::migrations::backup`], which runs the real copy check.
pub type Backup =
    fn(&Connection, &Path, Texts<'_>, (i64, i64), SystemTime) -> Result<PathBuf, StorageError>;

/// [`upgrade`] at a given time with a given backup. Inputs: as `upgrade`, plus the time that
/// stamps the backup and ages the pinned one, and the backup taken before any step. Output:
/// as `upgrade`.
fn upgrade_checked(
    db: &mut Connection,
    path: &Path,
    texts: Texts<'_>,
    steps: &[Step],
    now: SystemTime,
    take: Backup,
) -> Result<(), StorageError> {
    assert!(db.is_autocommit());
    let latest = latest(steps);
    let found = version(db)?;
    let from = match standing(db, latest)? {
        Some(from) if from < latest => from,
        _ => {
            forget_backup_failure(path);
            return stamp_version(db, found, latest);
        }
    };
    let before = schema(db)?;
    let made = take(db, path, texts, (from, latest), now);
    let backup = recorded(path, made)?;
    let failure = match run(db, steps, from, latest) {
        Ok(()) => {
            assert!(db.is_autocommit());
            assert_eq!(version(db)?, latest);
            return Ok(());
        }
        Err(failure) => failure,
    };
    Err(recover(db, &backup, found, &before, failure))
}

/// Sets the version of record without running steps, when it differs.
/// Inputs: the connection, the version found and the newest version. Output: Ok once
/// the database reads the newest version.
fn stamp_version(db: &Connection, found: i64, latest: i64) -> Result<(), StorageError> {
    if found != latest {
        db.execute_batch(&format!("PRAGMA user_version = {latest}"))
            .map_err(error)?;
    }
    assert_eq!(version(db)?, latest);
    Ok(())
}

/// A step that failed: its version, the cause, and whether the step ended the upgrade
/// transaction itself, so its changes may already be committed.
struct Failure {
    step: i64,
    cause: String,
    escaped: bool,
}

/// Runs every step above `from` and stamps `latest`, all in one immediate transaction.
/// Inputs: the connection, the steps and both versions. Output: Ok once committed, or
/// the failed step. The transaction rolls back on any failure it still holds.
fn run(db: &mut Connection, steps: &[Step], from: i64, latest: i64) -> Result<(), Failure> {
    let failed = |step: i64| {
        move |cause: rusqlite::Error| Failure {
            step,
            cause: cause.to_string(),
            escaped: false,
        }
    };
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(failed(from + 1))?;
    for step in steps.iter().filter(|step| step.to > from) {
        tx.authorizer(Some(keeps_the_transaction))
            .map_err(failed(step.to))?;
        let result = (step.run)(&tx);
        tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .map_err(failed(step.to))?;
        if tx.is_autocommit() {
            return Err(Failure {
                step: step.to,
                cause: result.err().map_or_else(
                    || "the step ended the upgrade transaction".to_owned(),
                    |cause| cause.0,
                ),
                escaped: true,
            });
        }
        result.map_err(|cause| Failure {
            step: step.to,
            cause: cause.0,
            escaped: false,
        })?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {latest}"))
        .map_err(failed(latest))?;
    tx.commit().map_err(failed(latest))
}

/// The authorizer a step runs under: it refuses every transaction and savepoint statement
/// before it runs, so no step can commit, roll back, begin or release and leave the upgrade
/// transaction, and allows everything else. Input: the statement's action. Output: the verdict.
fn keeps_the_transaction(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Transaction { .. } | AuthAction::Savepoint { .. } => Authorization::Deny,
        _ => Authorization::Allow,
    }
}

/// Returns engine.db to its state before the upgrade and builds the refusal. A step
/// that failed inside the transaction is rolled back already; a database that changed
/// anyway is restored from the backup. Inputs: the connection, the backup folder, the
/// version and schema found before, and the failure. Output: `storage_migration_failed`.
fn recover(
    db: &mut Connection,
    backup: &Path,
    found: i64,
    before: &[String],
    failure: Failure,
) -> StorageError {
    let unchanged = !failure.escaped
        && db.is_autocommit()
        && version(db).ok() == Some(found)
        && schema(db).ok().as_deref() == Some(before);
    let outcome = if unchanged {
        "the database is unchanged".to_owned()
    } else {
        match restore(db, backup, found, before) {
            Ok(()) => "the database was restored from the backup".to_owned(),
            Err(cause) => format!("restoring the backup failed ({})", cause.0),
        }
    };
    error(format!(
        "storage_migration_failed: step {} failed: {}; {outcome}; backup at {}",
        failure.step,
        failure.cause,
        backup.display()
    ))
}

/// Copies the backup over the live database through the SQLite backup API, then checks
/// that it reads the version and schema found before the upgrade. Inputs: the connection,
/// the backup folder, that version and schema. Output: Ok once restored.
pub(crate) fn restore(
    db: &mut Connection,
    backup: &Path,
    found: i64,
    before: &[String],
) -> Result<(), StorageError> {
    if !db.is_autocommit() {
        db.execute_batch("ROLLBACK").map_err(error)?;
    }
    db.restore(
        rusqlite::MAIN_DB,
        backup.join(DATABASE),
        None::<fn(rusqlite::backup::Progress)>,
    )
    .map_err(error)?;
    if version(db)? != found || schema(db)? != before {
        return Err(error("storage_restore_mismatch"));
    }
    Ok(())
}
