use super::error;
use super::settings::SettingsSources;
use crate::engine::EngineError;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
mod tests;

/// The folder beside engine.db that holds one folder per upgrade backup.
pub(crate) const BACKUPS: &str = "backups";

/// How many upgrade backups stay on disk. Older ones are removed once a new one exists.
pub(crate) const BACKUPS_KEPT: usize = 5;

/// The database file name inside a backup folder.
const DATABASE: &str = "engine.db";

/// One forward engine.db schema step. `to` is the version the database reaches when
/// the upgrade commits; `run` makes the change inside the upgrade transaction and
/// must leave that transaction open.
pub(crate) struct Step {
    pub(crate) to: i64,
    pub(crate) run: StepRun,
}

/// The body of one schema step: it changes the database through the open upgrade
/// transaction and returns the cause when it cannot.
pub(crate) type StepRun = fn(&Transaction<'_>) -> Result<(), EngineError>;

/// Every engine.db schema step, oldest first, numbered from 2 without gaps. Version 1
/// is the shape engines up to 0.1 wrote, which never set `PRAGMA user_version`. A step
/// must accept that a table open creates lazily, such as owner_meta, may be absent.
pub(crate) const STEPS: &[Step] = &[
    Step {
        to: 2,
        run: adopt_user_version,
    },
    Step {
        to: 3,
        run: widen_empty_team_plans,
    },
];

/// Step 3 (DEC-096 addendum 3): an empty sub-harness list now means any harness, so a saved
/// plan that lists none and allows at most one agent, the old "master only" default, gets the
/// team maximum. Runs once with the upgrade, so an owner who later sets one agent keeps it.
/// A body that is not valid JSON is left as it is.
fn widen_empty_team_plans(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let setups: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='project_setups')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !setups {
        return Ok(());
    }
    tx.execute(
        "UPDATE project_setups SET body=json_set(body,'$.team.max_agents',?1)
         WHERE CASE WHEN json_valid(body)
             THEN json_type(body,'$.team.sub_harnesses')='array'
                 AND json_array_length(body,'$.team.sub_harnesses')=0
                 AND json_type(body,'$.team.max_agents')='integer'
                 AND json_extract(body,'$.team.max_agents')<=1
             ELSE 0 END",
        [crate::project_setup::TEAM_AGENTS_MAX],
    )
    .map_err(error)?;
    Ok(())
}

/// Step 2: makes `PRAGMA user_version` the database version of record. owner_meta
/// keeps its fixed `schema_version = 1` column as the owner row layout, so every query
/// that filters on it keeps working. Refuses an owner_meta row of another layout.
fn adopt_user_version(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let owner_meta: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='owner_meta')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !owner_meta {
        return Ok(());
    }
    let other: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM owner_meta WHERE schema_version IS NOT 1",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if other != 0 {
        return Err(error("owner_meta_layout_unknown"));
    }
    Ok(())
}

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
fn version(db: &Connection) -> Result<i64, EngineError> {
    db.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(error)
}

/// Says where engine.db stands and refuses one written by a newer engine.
/// Input: the connection and the newest version this engine knows. Output: None for a
/// database with no schema yet, otherwise its version, where the unset 0 reads as 1.
fn standing(db: &Connection, latest: i64) -> Result<Option<i64>, EngineError> {
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
pub(crate) fn check(db: &Connection, steps: &[Step]) -> Result<(), EngineError> {
    let latest = latest(steps);
    match standing(db, latest)? {
        Some(at) if at < latest => Err(error(format!(
            "storage_upgrade_required: the engine database is at version {at}; open it \
             writable once so this engine can upgrade it to version {latest}"
        ))),
        _ => Ok(()),
    }
}

/// Brings engine.db to the newest version before anything else writes to it. A
/// database with no schema is stamped newest. An older one is backed up, then every
/// pending step runs in one transaction. Inputs: the writable connection, its path,
/// the settings sources to back up and the steps. Output: Ok at the newest version, or
/// an error naming the backup. A newer database is refused and never written.
pub(crate) fn upgrade(
    db: &mut Connection,
    path: &Path,
    settings: Option<&SettingsSources>,
    steps: &[Step],
) -> Result<(), EngineError> {
    assert!(db.is_autocommit());
    let latest = latest(steps);
    let found = version(db)?;
    let from = match standing(db, latest)? {
        Some(from) if from < latest => from,
        _ => return stamp_version(db, found, latest),
    };
    let before = schema(db)?;
    let backup = backup(db, path, settings, from, latest)?;
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
fn stamp_version(db: &Connection, found: i64, latest: i64) -> Result<(), EngineError> {
    if found != latest {
        db.execute_batch(&format!("PRAGMA user_version = {latest}"))
            .map_err(error)?;
    }
    assert_eq!(version(db)?, latest);
    Ok(())
}

/// The schema rows of engine.db, sorted, used to see whether a failed upgrade left
/// the database changed. Input: the connection. Output: one line per schema row.
fn schema(db: &Connection) -> Result<Vec<String>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT type || ' ' || name || ' ' || tbl_name || ' ' || coalesce(sql, '') \
             FROM sqlite_schema ORDER BY type, name",
        )
        .map_err(error)?;
    statement
        .query_map([], |row| row.get(0))
        .map_err(error)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(error)
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
        let result = (step.run)(&tx);
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
) -> EngineError {
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
/// that it reads the version and schema found before the upgrade. Inputs: the
/// connection, the backup folder, that version and schema. Output: Ok once restored.
fn restore(
    db: &mut Connection,
    backup: &Path,
    found: i64,
    before: &[String],
) -> Result<(), EngineError> {
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

/// Builds a backup error. Input: the cause. Output: `storage_backup_failed` with it.
fn backup_failed(cause: impl std::fmt::Display) -> EngineError {
    error(format!("storage_backup_failed: {cause}"))
}

/// Copies engine.db through the SQLite online backup API, and the settings files, into
/// `<root>/backups/<from>-to-<to>-<UTC time>/` (folders 0700, files 0600), then keeps
/// only the newest BACKUPS_KEPT backups. Inputs: the connection, its path, the settings
/// sources and both versions. Output: the new backup folder.
fn backup(
    db: &Connection,
    path: &Path,
    settings: Option<&SettingsSources>,
    from: i64,
    to: i64,
) -> Result<PathBuf, EngineError> {
    assert!(from < to);
    let root = path
        .parent()
        .ok_or_else(|| backup_failed("the database has no folder"))?;
    let folder = backups_folder(root)?;
    let target = folder.join(format!("{from}-to-{to}-{}", stamp(SystemTime::now())?));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .map_err(backup_failed)?;
    if let Err(cause) = fill(db, &target, settings) {
        let _ = std::fs::remove_dir_all(&target);
        return Err(cause);
    }
    prune(&folder)?;
    Ok(target)
}

/// Creates `<root>/backups` when missing and checks that it is a private folder of
/// this user. Input: the engine root. Output: the backups folder path.
fn backups_folder(root: &Path) -> Result<PathBuf, EngineError> {
    let folder = root.join(BACKUPS);
    match std::fs::DirBuilder::new().mode(0o700).create(&folder) {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(cause) => return Err(backup_failed(cause)),
    }
    let metadata = std::fs::symlink_metadata(&folder).map_err(backup_failed)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(error(format!(
            "storage_backup_unsafe: {} must be a private folder of this user",
            folder.display()
        )));
    }
    Ok(folder)
}

/// Writes the database and settings copies into a new backup folder.
/// Inputs: the connection, the folder and the settings sources. Output: Ok once every
/// copy is on disk.
fn fill(
    db: &Connection,
    target: &Path,
    settings: Option<&SettingsSources>,
) -> Result<(), EngineError> {
    let file = target.join(DATABASE);
    private_file(&file, b"")?;
    db.backup(rusqlite::MAIN_DB, &file, None)
        .map_err(backup_failed)?;
    if let Some(settings) = settings {
        for (name, text) in settings.backup_texts()? {
            private_file(&target.join(name), text.as_bytes())?;
        }
    }
    Ok(())
}

/// Creates a new file readable only by this user and writes it to disk.
/// Inputs: the path, which must not exist, and the bytes. Output: Ok once synced.
fn private_file(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(backup_failed)?;
    file.write_all(bytes).map_err(backup_failed)?;
    file.sync_all().map_err(backup_failed)
}

/// Removes all but the newest BACKUPS_KEPT backup folders. Only real folders named
/// `<from>-to-<to>-<UTC time>` count; anything else in the folder is left alone.
/// Input: the backups folder. Output: Ok once the older backups are gone.
fn prune(folder: &Path) -> Result<(), EngineError> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(backup_failed)? {
        let entry = entry.map_err(backup_failed)?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(time) = backup_time(&name).map(str::to_owned) else {
            continue;
        };
        if entry.file_type().map_err(backup_failed)?.is_dir() {
            found.push((time, name));
        }
    }
    found.sort();
    let excess = found.len().saturating_sub(BACKUPS_KEPT);
    for (_, name) in &found[..excess] {
        std::fs::remove_dir_all(folder.join(name)).map_err(backup_failed)?;
    }
    Ok(())
}

/// The UTC time part of a backup folder name `<from>-to-<to>-<time>`.
/// Input: a folder name. Output: the time, or None for any other name.
fn backup_time(name: &str) -> Option<&str> {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let (from, rest) = name.split_once("-to-")?;
    let (to, time) = rest.split_once('-')?;
    let shaped = time.len() == 19
        && time.is_ascii()
        && digits(&time[..8])
        && &time[8..9] == "T"
        && digits(&time[9..18])
        && &time[18..] == "Z";
    (digits(from) && digits(to) && shaped).then_some(time)
}

/// Formats a time as a UTC stamp such as `20261025T160700123Z`, which sorts in time
/// order. Input: the time. Output: the stamp, or an error outside the years 1970-9999.
fn stamp(time: SystemTime) -> Result<String, EngineError> {
    let since = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| backup_failed("the clock is before 1970"))?;
    let seconds = since.as_secs();
    let days =
        i64::try_from(seconds / 86_400).map_err(|_| backup_failed("the clock is out of range"))?;
    let (year, month, day) = civil(days);
    if !(1970..=9999).contains(&year) {
        return Err(backup_failed("the clock is out of range"));
    }
    let clock = seconds % 86_400;
    let text = format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}{:03}Z",
        clock / 3_600,
        clock % 3_600 / 60,
        clock % 60,
        since.subsec_millis()
    );
    assert_eq!(text.len(), 19);
    Ok(text)
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
/// Input: the days. Output: year, month 1-12 and day 1-31.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    assert!((1..=12).contains(&month) && (1..=31).contains(&day));
    (year, month, day)
}
