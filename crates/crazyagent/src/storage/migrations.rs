use super::error;
use super::settings::SettingsSources;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod landing_step;
mod settings_step;
#[cfg(test)]
mod tests;

/// The folder beside engine.db that holds one folder per upgrade backup, each with a full
/// engine.db copy and the settings texts kept with it, and the dated copies a restore keeps
/// ([`keep_replaced`]). The sandbox names it among the engine secrets, so no granted root may
/// hold or sit inside it.
pub(crate) const BACKUPS: &str = "backups";

/// How many upgrade backups stay on disk. Older ones are removed once a new one exists.
pub(crate) const BACKUPS_KEPT: usize = 5;

/// The newest engine.db version a 2.4 engine writes: 2.4.2 ships the steps up to 11. An
/// upgrade from at most this version to a newer one is a 2.5 upgrade of 2.4 data.
const V24_VERSION_MAX: i64 = 11;
const _: () = assert!(STEPS.len() as i64 + 1 >= V24_VERSION_MAX);

/// How many days the first backup a 2.5 upgrade took from a 2.4 database stays outside the
/// BACKUPS_KEPT rotation, so the owner can still go back to 2.4 in that time.
const PINNED_DAYS: u32 = 30;

/// The file a backup folder holds once its copy passed the check made before any step ran.
const CHECKED: &str = "checked";

/// The most tables the copy check counts. engine.db holds a few hundred.
const TABLES_MAX: usize = 4_096;

/// Checks a fresh backup copy against the live database before any step runs. Inputs: the
/// live connection and the copy's path. Output: Ok, or the refusal that stops the upgrade.
type CopyCheck = fn(&Connection, &Path) -> Result<(), EngineError>;

/// The database file name inside a backup folder.
pub(crate) const DATABASE: &str = "engine.db";

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
    Step {
        to: 4,
        run: master_sessions_parked,
    },
    Step {
        to: 5,
        run: widen_file_trash_kinds,
    },
    Step {
        to: 6,
        run: sweep_orphan_project_plugins,
    },
    Step {
        to: 7,
        run: widen_hook_kinds,
    },
    Step {
        to: 8,
        run: nest_team_agents,
    },
    Step {
        to: 9,
        run: widen_trash_for_account_files,
    },
    Step {
        to: 10,
        run: usage_counts_by_model,
    },
    Step {
        to: 11,
        run: widen_delete_file_names,
    },
    Step {
        to: 12,
        run: app_plugin_store,
    },
    Step {
        to: 13,
        run: worktrees_on_the_log,
    },
    Step {
        to: 14,
        run: settings_step::settings_on_the_log,
    },
    Step {
        to: 15,
        run: landing_step::landing_on_the_log,
    },
];

/// Step 13 (v2.5 L6): the worktrees aggregate goes on the state log. Worktree rows written
/// before the log reach replay through one snapshot run, appended through the open's proof
/// since no engine host runs during an upgrade; a store with no worktree row gets none.
/// Input: the open upgrade transaction. Output: Ok or the log's refusal.
fn worktrees_on_the_log(tx: &Transaction<'_>) -> Result<(), EngineError> {
    let at_ms = crate::state_log::replay::Clock::System.now_ms();
    let opening = super::opening::Opening::new(at_ms);
    crate::worktrees::log::first_run(tx, opening)?;
    assert!(
        !tx.is_autocommit(),
        "a step leaves the upgrade transaction open"
    );
    Ok(())
}

/// The widest engine file name step 11 lets `projects_delete_files` hold, kept as that step
/// shipped: a wider record needs a step of its own, which the check below demands at build
/// time once [`crate::projects_home::RECORDED_NAME_BYTES_MAX`] grows (G1 code r1 F1).
const STEP_11_NAME_BYTES_MAX: usize = 96;
const _: () = assert!(
    crate::projects_home::RECORDED_NAME_BYTES_MAX == STEP_11_NAME_BYTES_MAX,
    "a wider delete record needs a migration step that widens projects_delete_files"
);

/// Step 12 (V250-H2): the app plugin store's trust records, switches and secret bindings.
/// An upgrade from 2.4 makes the three empty tables under its pinned backup; a new database
/// gets them when the engine opens it. Input: the open upgrade transaction. Output: Ok, or
/// `app_plugins_unavailable`.
fn app_plugin_store(tx: &Transaction<'_>) -> Result<(), EngineError> {
    crate::app_plugins::store::initialize(tx)
}

/// Step 11 (RV-DS241-152): a delete records Overview and roadmap status run names of up to
/// [`STEP_11_NAME_BYTES_MAX`] bytes, so `projects_delete_files` must hold them. An existing
/// table is rebuilt with the wider check, keeping its rows; a missing one gets the current
/// schema when the engine opens it. Input: the open upgrade transaction. Output: any SQLite
/// failure.
fn widen_delete_file_names(tx: &Transaction<'_>) -> Result<(), EngineError> {
    if !has_table(tx, "projects_delete_files")? {
        return Ok(());
    }
    tx.execute_batch(&format!(
        "CREATE TABLE projects_delete_files_new (
            name TEXT PRIMARY KEY CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND {STEP_11_NAME_BYTES_MAX}),
            inode INTEGER NOT NULL
         );
         INSERT INTO projects_delete_files_new SELECT name, inode FROM projects_delete_files;
         DROP TABLE projects_delete_files;
         ALTER TABLE projects_delete_files_new RENAME TO projects_delete_files;"
    ))
    .map_err(error)
}

/// Step 10 (CODEBURN-COUNT): usage rows gain the model and estimated dollars. A database that
/// never opened the usage store gets the new shape when the store first opens.
/// Input: the open upgrade transaction. Output: done, or the store's failure.
fn usage_counts_by_model(tx: &Transaction<'_>) -> Result<(), EngineError> {
    if !has_table(tx, "usage_tokens")? {
        return Ok(());
    }
    crate::usage::capture::migrate_counts(tx)
}

/// Says whether a table of that name exists. Inputs: the connection or open transaction and
/// the name. Output: true when `sqlite_schema` holds a table row for it.
fn has_table(db: &Connection, name: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )
    .map_err(error)
}

/// Step 5 (PP-2): a removed project's skill store folder is now queued in the transactional
/// file trash, as its secrets already are, so the `skill_folder` kind must pass the check.
/// An existing `file_trash` is rebuilt with the wider check, keeping its pending rows; a
/// database that never had the table gets it from the current schema, which already lists
/// the kind. Runs before step 6, which queues that kind for orphaned projects.
fn widen_file_trash_kinds(tx: &Transaction<'_>) -> Result<(), EngineError> {
    if !has_table(tx, "file_trash")? {
        return super::trash::initialize(tx);
    }
    tx.execute_batch(
        "CREATE TABLE file_trash_new (
            kind TEXT NOT NULL CHECK(kind IN ('account_key','mcp_secret','mcp_checks','skill_folder')),
            name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 128),
            PRIMARY KEY(kind, name)
         ) WITHOUT ROWID;
         INSERT INTO file_trash_new SELECT kind, name FROM file_trash;
         DROP TABLE file_trash;
         ALTER TABLE file_trash_new RENAME TO file_trash;",
    )
    .map_err(error)
}

/// Step 8 (N1): a sub-agent can lead workers of its own, so team tasks and agents record
/// the lead that started them.
fn nest_team_agents(tx: &Transaction<'_>) -> Result<(), EngineError> {
    crate::team::add_parent_columns(tx)
}

/// Step 9: a removed account's sign-in files and generated SDK home go through the file
/// trash, so `file_trash` must take their two kinds. An existing table is rebuilt with the
/// current check, keeping its rows; a missing one gets the current schema when the engine
/// opens it. Input: the transaction. Output: any SQLite failure.
fn widen_trash_for_account_files(tx: &Transaction<'_>) -> Result<(), EngineError> {
    if has_table(tx, "file_trash")? {
        tx.execute_batch(super::trash::WIDEN).map_err(error)?;
    }
    Ok(())
}

/// Step 7 (H2): hooks are plugin items, and their stored folder copies go through the file
/// trash, so `plugin_items` must take the `hook` kind and `file_trash` the `hook_folder`
/// kind. Each existing table is rebuilt with the current check, keeping its rows; a missing
/// one gets the current schema when the engine opens it.
fn widen_hook_kinds(tx: &Transaction<'_>) -> Result<(), EngineError> {
    if has_table(tx, "plugin_items")? {
        tx.execute_batch(super::plugins::WIDEN_ITEMS)
            .map_err(error)?;
    }
    if has_table(tx, "file_trash")? {
        tx.execute_batch(super::trash::WIDEN).map_err(error)?;
    }
    Ok(())
}

/// Step 6 (PP-1): clears plugin rows that removals before `remove_project` held them left
/// behind, which a reused id would otherwise inherit. For every project scope present in
/// `plugin_items` or `plugin_switches` with no `projects` row, `remove_project` drops the
/// rows and queues the stale secrets, check folders and skill folder for the post-start
/// sweep. Runs after step 5, which widened `file_trash`. A database without the plugin
/// tables yet has no rows to sweep.
fn sweep_orphan_project_plugins(tx: &Transaction<'_>) -> Result<(), EngineError> {
    if !has_table(tx, "plugin_items")? {
        return Ok(());
    }
    for id in orphan_project_ids(tx)? {
        let project = ProjectId::new(id).map_err(error)?;
        super::plugins::remove_project(tx, &project)?;
    }
    Ok(())
}

/// The project ids that own plugin rows or switches but have no `projects` row, each once.
/// Input: the transaction, whose `plugin_items`, `plugin_switches` and `projects` tables
/// exist. Output: the orphaned ids.
fn orphan_project_ids(tx: &Transaction<'_>) -> Result<Vec<String>, EngineError> {
    let mut statement = tx
        .prepare(
            "SELECT id FROM (
                 SELECT substr(scope, 9) AS id FROM plugin_items WHERE scope LIKE 'project:%'
                 UNION
                 SELECT project AS id FROM plugin_switches)
             WHERE id NOT IN (SELECT id FROM projects)",
        )
        .map_err(error)?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    Ok(ids)
}

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

/// Step 4: master sessions can be parked and resumed, and team tasks and agents record
/// the session that owns them (DEC-096 addendum 4b).
fn master_sessions_parked(tx: &Transaction<'_>) -> Result<(), EngineError> {
    super::master_session::allow_parked(tx)?;
    crate::master_sessions::add_ownership_columns(tx)
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
    forget_restore(path);
    upgrade_at(db, path, settings, steps, SystemTime::now(), check_copy)
}

/// [`upgrade`] at a given time with a given copy check, which tests replace to reach a
/// mismatch on purpose. Inputs: as `upgrade`, plus the time that stamps the backup and ages
/// the pinned one, and the check the copy must pass before any step. Output: as `upgrade`.
fn upgrade_at(
    db: &mut Connection,
    path: &Path,
    settings: Option<&SettingsSources>,
    steps: &[Step],
    now: SystemTime,
    check: CopyCheck,
) -> Result<(), EngineError> {
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
    let made = backup(db, path, settings, (from, latest), now, check);
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

/// The name a restore gives the dated copy it keeps of the data it replaces (DEC-192), before
/// `-<UTC time>`. It holds no `-to-`, so neither the rotation nor the pin ever counts it.
const REPLACED: &str = "replaced";

/// The code a restore starts with once no pinned 2.4 backup is left, after PINNED_DAYS.
pub(crate) const RESTORE_UNAVAILABLE: &str = "storage_restore_unavailable";
/// The code a restore starts with while a team agent or a research run still runs.
pub(crate) const RESTORE_AGENTS_RUNNING: &str = "storage_restore_agents_running";
/// The code a restore starts with while another connection holds engine.db.
pub(crate) const RESTORE_BUSY: &str = "storage_restore_busy";
/// The code any other failed restore starts with.
pub(crate) const RESTORE_FAILED: &str = "storage_restore_failed";

/// Until when the owner can still restore the pinned 2.4 backup of the engine at `root`, read
/// without creating anything. Inputs: the engine root and the time now. Output: the moment the
/// pin ends, or None when no pinned backup is left.
pub(crate) fn restorable_until(root: &Path, now: SystemTime) -> Option<SystemTime> {
    let folder = root.join(BACKUPS);
    let metadata = std::fs::symlink_metadata(&folder).ok()?;
    if !metadata.is_dir() || metadata.uid() != rustix::process::getuid().as_raw() {
        return None;
    }
    let found = backups_in(&folder).ok()?;
    let index = pinned(&folder, &found, now)?;
    let until = stamp_time(&found[index].0)? + Duration::from_secs(86_400) * PINNED_DAYS;
    assert!(until > now);
    Some(until)
}

/// Brings back the pinned 2.4 backup the owner asked for (L5, DEC-192), at the time now.
/// Inputs: the database path and the settings sources. Output: as [`restore_pinned_at`].
pub(crate) fn restore_pinned(
    path: &Path,
    settings: Option<&SettingsSources>,
) -> Result<PathBuf, EngineError> {
    restore_pinned_at(path, settings, SystemTime::now())
}

/// [`restore_pinned`] at a given time, which ages the pin. It holds engine.db alone for its
/// whole run, refuses while an agent runs, keeps one dated 0700 copy of the data it replaces,
/// then restores the pinned copy and checks every table against it. Inputs: the database
/// path, the settings sources and the time. Output: the folder of the copy it kept.
fn restore_pinned_at(
    path: &Path,
    settings: Option<&SettingsSources>,
    now: SystemTime,
) -> Result<PathBuf, EngineError> {
    let root = path
        .parent()
        .ok_or_else(|| restore_failed("the database has no folder"))?;
    let folder = backups_folder(root)?;
    let found = backups_in(&folder)?;
    let index = pinned(&folder, &found, now).ok_or_else(|| error(RESTORE_UNAVAILABLE))?;
    let source = folder.join(&found[index].1);
    let mut db = alone(path)?;
    if agents_running(&db)? {
        return Err(error(RESTORE_AGENTS_RUNNING));
    }
    let kept = keep_replaced(&db, &folder, settings, now)?;
    let pinned_copy = source.join(DATABASE);
    let restored = shape(&pinned_copy).and_then(|(version_pinned, schema_pinned)| {
        restore(&mut db, &source, version_pinned, &schema_pinned)?;
        check_copy(&db, &pinned_copy)?;
        assert_eq!(version(&db)?, version_pinned);
        Ok(())
    });
    if let Err(cause) = restored {
        return Err(drop_unneeded(&db, &kept, cause));
    }
    sweep_unbound_keys(&db, root, settings, &kept);
    assert!(kept.starts_with(&folder));
    Ok(kept)
}

/// Settles the owner's secrets the restored 2.4 rows no longer name (V250-L5-N5, N5B): each
/// plugin key no restored app plugin binding holds, so every one for a 2.4 copy, and each
/// account key, generated SDK home and native profile slot neither a restored account nor
/// the live settings name, as the start sweep judges ([`super::trash::referenced`]). One a
/// row of the dated copy of the replaced 2.5 data names moves into the same folder of that
/// copy (DEC-192), where it stays with the data that names it; one no row of it names, left
/// by a removal, is deleted as removals delete it. No settings, settings that cannot be read,
/// or a copy that cannot be opened keep every account's files in place. A failure is logged
/// by what it swept and its code and never fails the checked restore. Inputs: the restored
/// connection, still held alone, the engine root, the settings sources and the copy's folder.
fn sweep_unbound_keys(
    db: &Connection,
    root: &Path,
    settings: Option<&SettingsSources>,
    copy: &Path,
) {
    use super::accounts::{ACCOUNT_KEYS, PLUGIN_KEYS, settle_unkept_in};
    use super::trash::{ACCOUNT_KEY, referenced};
    let replaced = match replaced_database(copy) {
        Ok(replaced) => replaced,
        Err(cause) => return sweep_failed(REPLACED_DATABASE, &cause),
    };
    let mut swept = vec![(
        PLUGIN_KEYS,
        settle_unkept_in(
            root,
            PLUGIN_KEYS,
            copy,
            |name| plugin_key_bound(db, name),
            |name| plugin_key_bound(&replaced, name),
        ),
    )];
    match settings.map(SettingsSources::config) {
        Some(Ok(config)) => {
            let catalog = config.settings.model_access.as_ref();
            let named = |kind: &str, name: &str| referenced(db, catalog, kind, name);
            let keys = settle_unkept_in(
                root,
                ACCOUNT_KEYS,
                copy,
                |name| named(ACCOUNT_KEY, name),
                |name| referenced(&replaced, None, ACCOUNT_KEY, name),
            );
            swept.push((ACCOUNT_KEYS, keys));
            let profiles = sweep_replaced_profiles(root, copy, &replaced, named);
            swept.push((PROFILES, profiles));
        }
        Some(Err(cause)) => swept.push((ACCOUNT_KEYS, Err(cause))),
        None => {}
    }
    for (folder, result) in swept {
        if let Err(cause) = result {
            sweep_failed(folder, &cause);
        }
    }
}

/// Logs one failed part of a restore's sweep by name and by the code before its first `:`,
/// never a key name or a path. Inputs: what was swept and the failure.
fn sweep_failed(folder: &str, cause: &EngineError) {
    let code = cause.0.split(':').next().unwrap_or_default();
    eprintln!("restore_key_sweep_failed folder={folder} cause={code}");
}

/// What a restore's sweep calls the replaced copy's database in its log.
const REPLACED_DATABASE: &str = "replaced-database";

/// Opens the database of the dated copy of the replaced 2.5 data read-only, without
/// following a link. Input: the copy's folder. Output: the connection, or the failure.
fn replaced_database(copy: &Path) -> Result<Connection, EngineError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    Connection::open_with_flags(copy.join(DATABASE), flags).map_err(error)
}

/// What a restore's sweep calls the generated SDK homes and native sign-ins in its log.
const PROFILES: &str = "account-profiles";

/// Settles each generated SDK home and native profile slot the replaced 2.5 copy names that
/// `named` does not keep: one a saved account of the copy names moves into the same folder
/// of that dated copy ([`crate::engine::bootstrap::move_aside`]), and one only the copy's
/// trash still notes, of an account the owner removed in 2.5, is deleted through the start
/// sweep's own removal ([`super::trash::delete`]). Every keep test runs before the first
/// change, and a failed one keeps that one in place and does not stop the others. Inputs: the
/// engine root, the copy's folder and database, and the keep test by kind and name. Output:
/// how many were settled, or the copy's or keep test's failure, or the first failed change.
fn sweep_replaced_profiles(
    root: &Path,
    copy: &Path,
    replaced: &Connection,
    named: impl Fn(&str, &str) -> Result<bool, EngineError>,
) -> Result<usize, EngineError> {
    let mut unkept = Vec::new();
    for (kind, name, saved) in replaced_profiles(replaced)? {
        if !named(kind, name.as_str())? {
            unkept.push((kind, name, saved));
        }
    }
    let mut failed = None;
    let mut settled = 0;
    for (kind, name, saved) in &unkept {
        let gone = if *saved {
            let home = *kind == super::trash::GENERATED_HOME;
            crate::protocol::CredentialRef::new(name.as_str())
                .map_err(|_| error("account_key_reference_invalid"))
                .and_then(|reference| {
                    crate::engine::bootstrap::move_aside(root, copy, &reference, home)
                })
        } else {
            super::trash::delete(root, kind, name)
        };
        match gone {
            Ok(()) => settled += 1,
            Err(cause) => {
                failed.get_or_insert(cause);
            }
        }
    }
    assert!(settled <= unkept.len());
    failed.map_or(Ok(settled), Err)
}

/// The generated SDK homes and native profile slots the replaced 2.5 copy names, once each:
/// those of its saved accounts ([`super::accounts::profile_files`]), marked saved, then those
/// only its trash still notes. Input: the copy's database. Output: each kind with its name
/// and whether a saved account names it, or the copy's failure.
fn replaced_profiles(copy: &Connection) -> Result<Vec<(&'static str, String, bool)>, EngineError> {
    use super::trash::{GENERATED_HOME, NATIVE_SIGN_IN};
    use std::collections::BTreeSet;
    let saved: BTreeSet<(&'static str, String)> =
        super::accounts::profile_files(copy)?.into_iter().collect();
    let mut noted = BTreeSet::new();
    if has_table(copy, "file_trash")? {
        let rows = copy
            .prepare("SELECT kind, name FROM file_trash WHERE kind IN (?1, ?2) LIMIT ?3")
            .map_err(error)?
            .query_map(
                rusqlite::params![GENERATED_HOME, NATIVE_SIGN_IN, SWEEP_NOTES_MAX],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        for (kind, name) in rows {
            let kind = if kind == GENERATED_HOME {
                GENERATED_HOME
            } else {
                NATIVE_SIGN_IN
            };
            if !saved.contains(&(kind, name.clone())) {
                noted.insert((kind, name));
            }
        }
    }
    Ok(saved
        .into_iter()
        .map(|(kind, name)| (kind, name, true))
        .chain(noted.into_iter().map(|(kind, name)| (kind, name, false)))
        .collect())
}

/// The most trash notes a restore's sweep reads from the replaced copy.
const SWEEP_NOTES_MAX: i64 = 10_000;

/// Whether a restored app plugin binding holds a plugin key by its file name; a database
/// with no binding table, as every 2.4 copy, holds none. Inputs: the restored connection
/// and the name. Output: the answer, or any SQLite failure.
fn plugin_key_bound(db: &Connection, name: &str) -> Result<bool, EngineError> {
    if !has_table(db, "app_plugin_secrets")? {
        return Ok(false);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_plugin_secrets WHERE source='key' AND reference=?1)",
        [name],
        |row| row.get(0),
    )
    .map_err(error)
}

/// Settles the copy a failed restore kept: it is removed when the live database still holds
/// exactly its rows, so retries leave no copies outside the rotation, and kept otherwise,
/// since it may then hold the only whole 2.5 data. Inputs: the connection, the copy folder and
/// the failure. Output: `storage_restore_failed` naming the cause and whether the copy stays.
fn drop_unneeded(db: &Connection, kept: &Path, cause: EngineError) -> EngineError {
    let unchanged = check_copy(db, &kept.join(DATABASE)).is_ok();
    if unchanged && std::fs::remove_dir_all(kept).is_ok() {
        return restore_failed(cause.0);
    }
    restore_failed(format!(
        "{}; the replaced data stays in {}",
        cause.0,
        kept.display()
    ))
}

/// Builds a restore error. Input: the cause. Output: `storage_restore_failed` with it.
fn restore_failed(cause: impl std::fmt::Display) -> EngineError {
    error(format!("{RESTORE_FAILED}: {cause}"))
}

/// Opens engine.db and holds it alone until the connection closes: exclusive locking mode
/// and an exclusive transaction, with no wait, as the probe in changes.rs does. Any other
/// connection, in this process or another, refuses the restore, and none can open while it
/// runs. Input: the database path. Output: the connection, or `storage_restore_busy`.
fn alone(path: &Path) -> Result<Connection, EngineError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let db = Connection::open_with_flags(path, flags).map_err(restore_failed)?;
    db.busy_timeout(Duration::ZERO).map_err(restore_failed)?;
    match db.execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;") {
        Ok(()) => {}
        Err(rusqlite::Error::SqliteFailure(cause, _))
            if matches!(
                cause.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            return Err(error(RESTORE_BUSY));
        }
        Err(cause) => return Err(restore_failed(cause)),
    }
    assert!(db.is_autocommit());
    let mode: String = db
        .query_row("PRAGMA locking_mode", [], |row| row.get(0))
        .map_err(restore_failed)?;
    assert_eq!(mode, "exclusive");
    Ok(db)
}

/// Whether a team agent or a research run still runs, as engine.db records it: a team_agents
/// row, masters included, that is not stopped or failed, or a research_jobs row running. A
/// table that does not exist holds none. Input: the connection. Output: the answer.
pub(crate) fn agents_running(db: &Connection) -> Result<bool, EngineError> {
    let checks = [
        (
            "team_agents",
            "SELECT COUNT(*) FROM team_agents WHERE status NOT IN ('stopped', 'failed')",
        ),
        (
            "research_jobs",
            "SELECT COUNT(*) FROM research_jobs WHERE status = 'running'",
        ),
    ];
    for (table, query) in checks {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(restore_failed)?;
        if !exists {
            continue;
        }
        let running: i64 = db
            .query_row(query, [], |row| row.get(0))
            .map_err(restore_failed)?;
        assert!(running >= 0);
        if running > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Keeps one dated copy of the data a restore replaces (DEC-192): engine.db through the
/// SQLite backup API and the settings sources, in a new 0700 `replaced-<UTC time>` folder
/// under backups/, checked and marked as an upgrade backup is. Inputs: the connection, the
/// backups folder, the settings sources and the time. Output: the folder; a copy that fails
/// its check is removed.
fn keep_replaced(
    db: &Connection,
    folder: &Path,
    settings: Option<&SettingsSources>,
    now: SystemTime,
) -> Result<PathBuf, EngineError> {
    let taken = stamp(now).map_err(|cause| restore_failed(cause.0))?;
    let name = format!("{REPLACED}-{taken}");
    assert!(backup_time(&name).is_none());
    let target = folder.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .map_err(restore_failed)?;
    let made = fill(db, &target, settings)
        .and_then(|()| check_copy(db, &target.join(DATABASE)))
        .and_then(|()| mark_checked(folder, &target));
    if let Err(cause) = made {
        let _ = std::fs::remove_dir_all(&target);
        return Err(restore_failed(cause.0));
    }
    Ok(target)
}

/// The version and schema rows of the pinned copy, which the restored database must read.
/// The copy is opened as `check_copy` opens it. Input: the copy's path. Output: both, or
/// `storage_restore_failed` for a copy that is not 2.4 data.
fn shape(copy: &Path) -> Result<(i64, Vec<String>), EngineError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let opened = Connection::open_with_flags(copy, flags).map_err(restore_failed)?;
    let found = version(&opened)?;
    if found > V24_VERSION_MAX {
        return Err(restore_failed("the pinned copy is not 2.4 data"));
    }
    Ok((found, schema(&opened)?))
}

/// Builds a backup error. Input: the cause. Output: `storage_backup_failed` with it.
fn backup_failed(cause: impl std::fmt::Display) -> EngineError {
    error(format!("{BACKUP_FAILED}: {cause}"))
}

/// Copies engine.db through the SQLite online backup API, and the settings files, into
/// `<root>/backups/<from>-to-<to>-<UTC time>/` (folders 0700, files 0600), checks the copy,
/// marks it checked, then prunes to the pinned backup and the newest BACKUPS_KEPT others.
/// Inputs: the connection, its path, the settings sources, both versions, the time and the
/// copy check. Output: the new backup folder; a copy that fails its check is removed.
fn backup(
    db: &Connection,
    path: &Path,
    settings: Option<&SettingsSources>,
    (from, to): (i64, i64),
    now: SystemTime,
    check: CopyCheck,
) -> Result<PathBuf, EngineError> {
    assert!(from < to);
    let root = path
        .parent()
        .ok_or_else(|| backup_failed("the database has no folder"))?;
    let folder = backups_folder(root)?;
    let target = folder.join(format!("{from}-to-{to}-{}", stamp(now)?));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .map_err(backup_failed)?;
    let made = fill(db, &target, settings)
        .and_then(|()| check(db, &target.join(DATABASE)))
        .and_then(|()| mark_checked(&folder, &target));
    if let Err(cause) = made {
        let _ = std::fs::remove_dir_all(&target);
        return Err(cause);
    }
    prune(&folder, now)?;
    Ok(target)
}

/// Checks a backup copy before any step runs: SQLite's integrity check must pass, the copy
/// must hold engine.db's schema, and every table must hold as many rows as in engine.db.
/// The copy is opened without create, by the same path `fill` wrote it to, which may pass a
/// linked engine root. Inputs: the live connection and the copy's path.
/// Output: Ok, or `storage_backup_failed` naming the gap.
fn check_copy(db: &Connection, copy: &Path) -> Result<(), EngineError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let opened = Connection::open_with_flags(copy, flags).map_err(backup_failed)?;
    let integrity: String = opened
        .query_row("PRAGMA integrity_check(1)", [], |row| row.get(0))
        .map_err(backup_failed)?;
    if integrity != "ok" {
        return Err(backup_failed(format!(
            "the backup copy failed its integrity check: {integrity}"
        )));
    }
    let schemas = schema(&opened).and_then(|copied| Ok((copied, schema(db)?)));
    let (copied, live) = schemas.map_err(|cause| backup_failed(cause.0))?;
    if copied != live {
        return Err(backup_failed("the backup copy holds another schema"));
    }
    for table in tables(db)? {
        let live = count(db, &table)?;
        let copied = count(&opened, &table)?;
        if copied != live {
            return Err(backup_failed(format!(
                "the backup copy holds {copied} rows in {table} where engine.db holds {live}"
            )));
        }
    }
    Ok(())
}

/// The tables of a database, by name. Input: the connection. Output: at most TABLES_MAX
/// names, or `storage_backup_failed` when it holds more.
fn tables(db: &Connection) -> Result<Vec<String>, EngineError> {
    let mut statement = db
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .map_err(backup_failed)?;
    let names = statement
        .query_map([], |row| row.get(0))
        .map_err(backup_failed)?
        .take(TABLES_MAX + 1)
        .collect::<Result<Vec<String>, _>>()
        .map_err(backup_failed)?;
    if names.len() > TABLES_MAX {
        return Err(backup_failed(format!(
            "engine.db holds more than {TABLES_MAX} tables"
        )));
    }
    Ok(names)
}

/// The rows one table holds. Inputs: the connection and a table name from its schema.
/// Output: the count.
fn count(db: &Connection, table: &str) -> Result<i64, EngineError> {
    let quoted = table.replace('"', "\"\"");
    let rows: i64 = db
        .query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), [], |row| {
            row.get(0)
        })
        .map_err(backup_failed)?;
    assert!(rows >= 0);
    Ok(rows)
}

/// Records that a backup copy passed its check and makes the record durable: the `checked`
/// file, then the backup folder and the backups folder synced, so a crash after this point
/// still finds the backup checked. Inputs: the backups folder and the new backup.
/// Output: Ok once synced.
fn mark_checked(folder: &Path, target: &Path) -> Result<(), EngineError> {
    assert!(target.starts_with(folder));
    private_file(&target.join(CHECKED), b"")?;
    for synced in [target, folder] {
        std::fs::File::open(synced)
            .and_then(|handle| handle.sync_all())
            .map_err(backup_failed)?;
    }
    Ok(())
}

/// Creates `<root>/backups` when missing, or makes a folder of this user there private
/// again when an older build or a copy left it open to others (F36), and checks it. A link,
/// a file or a folder of another user is refused. Input: the engine root. Output: the
/// backups folder path.
fn backups_folder(root: &Path) -> Result<PathBuf, EngineError> {
    let folder = root.join(BACKUPS);
    match std::fs::DirBuilder::new().mode(0o700).create(&folder) {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(cause) => return Err(backup_failed(cause)),
    }
    let unsafe_folder = || {
        error(format!(
            "{BACKUP_UNSAFE}: {} must be a private folder of this user",
            folder.display()
        ))
    };
    let metadata = std::fs::symlink_metadata(&folder).map_err(backup_failed)?;
    if !metadata.is_dir() || metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(unsafe_folder());
    }
    if metadata.mode() & 0o077 != 0 {
        make_private(&folder, &metadata).map_err(|_| unsafe_folder())?;
        eprintln!("storage_backup_folder_made_private");
    }
    let checked = std::fs::symlink_metadata(&folder).map_err(backup_failed)?;
    if !checked.is_dir()
        || checked.uid() != rustix::process::getuid().as_raw()
        || checked.mode() & 0o077 != 0
        || (checked.dev(), checked.ino()) != (metadata.dev(), metadata.ino())
    {
        return Err(unsafe_folder());
    }
    Ok(folder)
}

/// Sets one folder of this user to 0700 through a handle opened without following a link,
/// only when it is still the folder that was checked. Inputs: the folder and its checked
/// metadata. Output: done, or the refusal or I/O error.
fn make_private(folder: &Path, checked: &std::fs::Metadata) -> Result<(), EngineError> {
    use rustix::fs::{Mode, OFlags};
    let handle = std::fs::File::from(
        rustix::fs::open(
            folder,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(error)?,
    );
    let opened = handle.metadata().map_err(error)?;
    if !opened.is_dir()
        || (opened.dev(), opened.ino()) != (checked.dev(), checked.ino())
        || opened.uid() != rustix::process::getuid().as_raw()
    {
        return Err(error(BACKUP_UNSAFE));
    }
    rustix::fs::fchmod(&handle, Mode::RWXU).map_err(error)
}

/// The code a backup refused for an unsafe backups folder starts with.
const BACKUP_UNSAFE: &str = "storage_backup_unsafe";
/// The code any other failed backup starts with.
const BACKUP_FAILED: &str = "storage_backup_failed";
/// The private file beside engine.db that names why the last upgrade backup failed, so a
/// client can say so while no engine answers (F36). A successful backup removes it.
pub(crate) const BACKUP_FAILURE_FILE: &str = "backup-failed";

/// Keeps a durable record of how an upgrade backup ended: a failure writes its code into
/// [`BACKUP_FAILURE_FILE`], a success removes the file. Writing the record never changes
/// the result. Inputs: the database path and the backup's result. Output: the same result.
fn recorded(path: &Path, result: Result<PathBuf, EngineError>) -> Result<PathBuf, EngineError> {
    let Some(root) = path.parent() else {
        return result;
    };
    let record = root.join(BACKUP_FAILURE_FILE);
    match &result {
        Ok(_) => forget_backup_failure(path),
        Err(cause) => {
            let code = if cause.0.starts_with(BACKUP_UNSAFE) {
                BACKUP_UNSAFE
            } else {
                BACKUP_FAILED
            };
            let _ = std::fs::remove_file(&record);
            if private_file(&record, code.as_bytes()).is_err() {
                eprintln!("storage_backup_record_unwritten");
            }
        }
    }
    result
}

/// Removes the record of a failed upgrade backup once the database needs no backup or one
/// was made. Input: the database path. Output: none; a record that cannot be removed is
/// logged.
fn forget_backup_failure(path: &Path) {
    let Some(root) = path.parent() else {
        return;
    };
    match std::fs::remove_file(root.join(BACKUP_FAILURE_FILE)) {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => eprintln!("storage_backup_record_kept"),
    }
}

/// Reads why the last upgrade backup of the engine at `root` failed, without an engine and
/// without following a link: only a private regular file of this user holding one known
/// code counts. Input: the engine root. Output: `storage_backup_unsafe` or
/// `storage_backup_failed`, or none.
pub fn backup_failure(root: &Path) -> Option<&'static str> {
    use std::io::Read;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(root.join(BACKUP_FAILURE_FILE))
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return None;
    }
    let mut text = String::new();
    file.take(64).read_to_string(&mut text).ok()?;
    [BACKUP_UNSAFE, BACKUP_FAILED]
        .into_iter()
        .find(|code| text == *code)
}

/// The private file beside engine.db that says how the owner's last restore stands, so a
/// client can say so while no engine answers: `pending`, `restored <copy>` or
/// `failed <code>`.
pub(crate) const RESTORE_RECORD_FILE: &str = "restore-record";
/// The record of a restore the engine stops for.
pub(crate) const RECORD_PENDING: &str = "pending";
/// The first word of the record of a restore that brought the 2.4 data back.
pub(crate) const RECORD_RESTORED: &str = "restored";
/// The first word of the record of a restore that changed nothing.
pub(crate) const RECORD_FAILED: &str = "failed";
/// The most a restore record holds.
const RESTORE_RECORD_BYTES_MAX: usize = 512;

/// Replaces the restore record of the engine at `root` with one line, written private and
/// synced. Inputs: the root and the line. Output: Ok once it is on disk.
pub(crate) fn write_restore_record(root: &Path, line: &str) -> Result<(), EngineError> {
    assert!(!line.is_empty());
    assert!(line.len() <= RESTORE_RECORD_BYTES_MAX);
    let record = root.join(RESTORE_RECORD_FILE);
    match std::fs::remove_file(&record) {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
        Err(cause) => return Err(restore_failed(cause)),
    }
    private_file(&record, line.as_bytes()).map_err(|cause| restore_failed(cause.0))
}

/// Reads the restore record of the engine at `root`, without an engine and without following
/// a link: only a private regular file of this user counts. Input: the root. Output: its line,
/// or none.
pub(crate) fn read_restore_record(root: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(root.join(RESTORE_RECORD_FILE))
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return None;
    }
    let mut text = String::new();
    let limit = u64::try_from(RESTORE_RECORD_BYTES_MAX).ok()?;
    file.take(limit).read_to_string(&mut text).ok()?;
    assert!(text.len() <= RESTORE_RECORD_BYTES_MAX);
    Some(text)
}

/// Removes a pending or restored record as the engine starts, so a stale one never keeps a
/// client from starting the engine after a crash or a return to 2.5; a failed one stays for
/// the restore screen until the next request. Input: the database path. Output: none; a
/// record that cannot be removed is logged.
fn forget_restore(path: &Path) {
    let Some(root) = path.parent() else {
        return;
    };
    let Some(line) = read_restore_record(root) else {
        return;
    };
    if line.split(' ').next() == Some(RECORD_FAILED) {
        return;
    }
    if std::fs::remove_file(root.join(RESTORE_RECORD_FILE)).is_err() {
        eprintln!("storage_restore_record_kept");
    }
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

/// Removes the older backup folders: the pinned one stays, and of the others only the newest
/// BACKUPS_KEPT do. Only real folders named `<from>-to-<to>-<UTC time>` count; anything else
/// in the folder is left alone. Inputs: the backups folder and the time now.
/// Output: Ok once the older backups are gone.
fn prune(folder: &Path, now: SystemTime) -> Result<(), EngineError> {
    let found = backups_in(folder)?;
    let pinned = pinned(folder, &found, now);
    let rotating: Vec<&str> = found
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != pinned)
        .map(|(_, (_, name))| name.as_str())
        .collect();
    let excess = rotating.len().saturating_sub(BACKUPS_KEPT);
    for name in &rotating[..excess] {
        std::fs::remove_dir_all(folder.join(name)).map_err(backup_failed)?;
    }
    assert!(rotating.len() - excess <= BACKUPS_KEPT);
    Ok(())
}

/// The real backup folders, oldest first. Input: the backups folder. Output: each folder's
/// UTC time and name; links, files and other names are left out.
fn backups_in(folder: &Path) -> Result<Vec<(String, String)>, EngineError> {
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
    Ok(found)
}

/// The backup the rotation leaves alone and a restore brings back: the newest checked one a
/// 2.5 upgrade took from a 2.4 database, while its stamp is under PINNED_DAYS old, so 2.4
/// work done between a restore and the next upgrade is never lost to an older one. A stamp
/// ahead of the clock counts as young. Inputs: the backups folder, its backups oldest first
/// and the time now. Output: that backup's index, or None.
fn pinned(folder: &Path, found: &[(String, String)], now: SystemTime) -> Option<usize> {
    let kept_for = Duration::from_secs(86_400) * PINNED_DAYS;
    found.iter().rposition(|(time, name)| {
        let from_v24 = backup_versions(name)
            .is_some_and(|(from, to)| from <= V24_VERSION_MAX && to > V24_VERSION_MAX);
        let young = stamp_time(time)
            .is_some_and(|taken| now.duration_since(taken).map_or(true, |age| age < kept_for));
        let checked = std::fs::symlink_metadata(folder.join(name).join(CHECKED))
            .is_ok_and(|metadata| metadata.is_file());
        from_v24 && young && checked
    })
}

/// The versions in a backup folder name `<from>-to-<to>-<time>`. Input: a folder name.
/// Output: from and to, or None for any other name.
fn backup_versions(name: &str) -> Option<(i64, i64)> {
    backup_time(name)?;
    let (from, rest) = name.split_once("-to-")?;
    let (to, _) = rest.split_once('-')?;
    Some((from.parse().ok()?, to.parse().ok()?))
}

/// The UTC time part of a backup folder name `<from>-to-<to>-<time>`.
/// Input: a folder name. Output: the time, or None for any other name.
fn backup_time(name: &str) -> Option<&str> {
    let (from, rest) = name.split_once("-to-")?;
    let (to, time) = rest.split_once('-')?;
    (digits(from) && digits(to) && stamp_shaped(time)).then_some(time)
}

/// Whether text is one or more ASCII digits. Input: the text. Output: the answer.
fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether text has the shape of a backup stamp such as `20261025T160700123Z`.
/// Input: the text. Output: the answer; the date itself is not checked here.
fn stamp_shaped(time: &str) -> bool {
    time.len() == 19
        && time.is_ascii()
        && digits(&time[..8])
        && &time[8..9] == "T"
        && digits(&time[9..18])
        && &time[18..] == "Z"
}

/// Reads a backup stamp back as a time, the inverse of [`stamp`]. Input: the stamp.
/// Output: the time, or None for a stamp of another shape or a date that does not exist.
fn stamp_time(time: &str) -> Option<SystemTime> {
    if !stamp_shaped(time) {
        return None;
    }
    let field = |range: std::ops::Range<usize>| time[range].parse::<u32>().ok();
    let (year, month, day) = (i64::from(field(0..4)?), field(4..6)?, field(6..8)?);
    let (hour, minute, second, milli) = (
        field(9..11)?,
        field(11..13)?,
        field(13..15)?,
        field(15..18)?,
    );
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 59 {
        return None;
    }
    let count = days(year, month, day);
    if count < 0 || civil(count) != (year, month, day) {
        return None;
    }
    let seconds =
        u64::try_from(count).ok()? * 86_400 + u64::from(hour * 3_600 + minute * 60 + second);
    Some(UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_millis(u64::from(milli)))
}

/// The day count since 1970-01-01 of a proleptic Gregorian date, the inverse of [`civil`].
/// Inputs: year, month 1-12 and day 1-31. Output: the days, negative before 1970.
fn days(year: i64, month: u32, day: u32) -> i64 {
    assert!((1..=12).contains(&month));
    assert!((1..=31).contains(&day));
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let shifted = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * shifted + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
