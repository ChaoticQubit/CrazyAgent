use super::error;
use super::settings::SettingsSources;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use kernel_storage::StorageError;
use kernel_storage::migrations::V24_VERSION_MAX;
/// Forwards to kernel_storage::migrations::backup_failure until Task 3b repoints storage.rs
/// and deletes it (POD-B exception). Input and output as the kernel's.
pub use kernel_storage::migrations::backup_failure;
#[cfg(unix)]
use kernel_storage::migrations::{
    BACKUPS, DATABASE, PINNED_DAYS, backup_time, backups_folder, backups_in, check_copy, fill,
    mark_checked, pinned, private_file, schema, stamp, stamp_time,
};
pub(crate) use kernel_storage::upgrade::Step;
#[cfg(unix)]
use kernel_storage::upgrade::{restore, version};
#[cfg(unix)]
use rusqlite::OpenFlags;
use rusqlite::{Connection, Transaction};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::time::{Duration, SystemTime};

mod landing_step;
mod memory_step;
mod settings_step;
#[cfg(all(test, unix))]
mod tests;
mod worktrees_step;

const _: () = assert!(STEPS.len() as i64 + 1 >= V24_VERSION_MAX);

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
        run: worktrees_step::worktrees_on_the_log,
    },
    Step {
        to: 14,
        run: settings_step::settings_on_the_log,
    },
    Step {
        to: 15,
        run: memory_step::memory_on_the_log,
    },
    Step {
        to: 16,
        run: settings_step::sessions_on_the_log,
    },
    Step {
        to: 17,
        run: landing_step::landing_on_the_log,
    },
    Step {
        to: 18,
        run: settings_step::session_turns_on_the_log,
    },
];

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
fn app_plugin_store(tx: &Transaction<'_>) -> Result<(), StorageError> {
    crate::app_plugins::store::initialize(tx)?;
    Ok(())
}

/// Step 11 (RV-DS241-152): a delete records Overview and roadmap status run names of up to
/// [`STEP_11_NAME_BYTES_MAX`] bytes, so `projects_delete_files` must hold them. An existing
/// table is rebuilt with the wider check, keeping its rows; a missing one gets the current
/// schema when the engine opens it. Input: the open upgrade transaction. Output: any SQLite
/// failure.
fn widen_delete_file_names(tx: &Transaction<'_>) -> Result<(), StorageError> {
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
    .map_err(error)?;
    Ok(())
}

/// Step 10 (CODEBURN-COUNT): usage rows gain the model and estimated dollars. A database that
/// never opened the usage store gets the new shape when the store first opens.
/// Input: the open upgrade transaction. Output: done, or the store's failure.
fn usage_counts_by_model(tx: &Transaction<'_>) -> Result<(), StorageError> {
    if !has_table(tx, "usage_tokens")? {
        return Ok(());
    }
    crate::usage::capture::migrate_counts(tx)?;
    Ok(())
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
fn widen_file_trash_kinds(tx: &Transaction<'_>) -> Result<(), StorageError> {
    if !has_table(tx, "file_trash")? {
        return Ok(super::trash::initialize(tx)?);
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
    .map_err(error)?;
    Ok(())
}

/// Step 8 (N1): a sub-agent can lead workers of its own, so team tasks and agents record
/// the lead that started them.
fn nest_team_agents(tx: &Transaction<'_>) -> Result<(), StorageError> {
    crate::team::add_parent_columns(tx)?;
    Ok(())
}

/// Step 9: a removed account's sign-in files and generated SDK home go through the file
/// trash, so `file_trash` must take their two kinds. An existing table is rebuilt with the
/// current check, keeping its rows; a missing one gets the current schema when the engine
/// opens it. Input: the transaction. Output: any SQLite failure.
fn widen_trash_for_account_files(tx: &Transaction<'_>) -> Result<(), StorageError> {
    if has_table(tx, "file_trash")? {
        tx.execute_batch(super::trash::WIDEN).map_err(error)?;
    }
    Ok(())
}

/// Step 7 (H2): hooks are plugin items, and their stored folder copies go through the file
/// trash, so `plugin_items` must take the `hook` kind and `file_trash` the `hook_folder`
/// kind. Each existing table is rebuilt with the current check, keeping its rows; a missing
/// one gets the current schema when the engine opens it.
fn widen_hook_kinds(tx: &Transaction<'_>) -> Result<(), StorageError> {
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
fn sweep_orphan_project_plugins(tx: &Transaction<'_>) -> Result<(), StorageError> {
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
fn widen_empty_team_plans(tx: &Transaction<'_>) -> Result<(), StorageError> {
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
fn master_sessions_parked(tx: &Transaction<'_>) -> Result<(), StorageError> {
    super::master_session::allow_parked(tx)?;
    crate::master_sessions::add_ownership_columns(tx)?;
    Ok(())
}

/// Step 2: makes `PRAGMA user_version` the database version of record. owner_meta
/// keeps its fixed `schema_version = 1` column as the owner row layout, so every query
/// that filters on it keeps working. Refuses an owner_meta row of another layout.
fn adopt_user_version(tx: &Transaction<'_>) -> Result<(), StorageError> {
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
        return Err(error("owner_meta_layout_unknown").into());
    }
    Ok(())
}

/// Refuses a read-only open of engine.db that a newer engine wrote or that still needs
/// an upgrade, through [`kernel_storage::upgrade::check`]. Input: the read-only connection
/// and the steps. Output: Ok when the database is empty or at the newest version.
pub(crate) fn check(db: &Connection, steps: &[Step]) -> Result<(), EngineError> {
    Ok(kernel_storage::upgrade::check(db, steps)?)
}

/// Brings engine.db to the newest version before anything else writes to it, through
/// [`kernel_storage::upgrade::upgrade`], after forgetting a finished restore's record. A
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
    #[cfg(unix)]
    forget_restore(path);
    Ok(kernel_storage::upgrade::upgrade(
        db,
        path,
        &|| settings_texts(settings),
        steps,
    )?)
}

/// The settings texts a backup keeps beside engine.db, read from the sources when there are
/// any. Input: the sources. Output: their file names and texts, or none.
fn settings_texts(
    settings: Option<&SettingsSources>,
) -> Result<Vec<(&'static str, String)>, StorageError> {
    settings.map_or(Ok(Vec::new()), |sources| {
        sources
            .backup_texts()
            .map_err(|cause| StorageError(cause.0))
    })
}

/// The name a restore gives the dated copy it keeps of the data it replaces (DEC-192), before
/// `-<UTC time>`. It holds no `-to-`, so neither the rotation nor the pin ever counts it.
#[cfg(unix)]
const REPLACED: &str = "replaced";

/// The code a restore starts with once no pinned 2.4 backup is left, after PINNED_DAYS.
#[cfg(unix)]
pub(crate) const RESTORE_UNAVAILABLE: &str = "storage_restore_unavailable";
/// The code a restore starts with while a team agent or a research run still runs.
#[cfg(unix)]
pub(crate) const RESTORE_AGENTS_RUNNING: &str = "storage_restore_agents_running";
/// The code a restore starts with while another connection holds engine.db.
#[cfg(unix)]
pub(crate) const RESTORE_BUSY: &str = "storage_restore_busy";
/// The code any other failed restore starts with.
#[cfg(unix)]
pub(crate) const RESTORE_FAILED: &str = "storage_restore_failed";

/// Until when the owner can still restore the pinned 2.4 backup of the engine at `root`, read
/// without creating anything. Inputs: the engine root and the time now. Output: the moment the
/// pin ends, or None when no pinned backup is left.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
fn sweep_failed(folder: &str, cause: &EngineError) {
    let code = cause.0.split(':').next().unwrap_or_default();
    eprintln!("restore_key_sweep_failed folder={folder} cause={code}");
}

/// What a restore's sweep calls the replaced copy's database in its log.
#[cfg(unix)]
const REPLACED_DATABASE: &str = "replaced-database";

/// Opens the database of the dated copy of the replaced 2.5 data without following a link:
/// read-write like `shape`, so a WAL copy with no -wal or -shm file opens on Apple's SQLite,
/// then `query_only`, so the sweep can never write the kept copy. Input: the copy's folder.
/// Output: the connection, or the failure.
#[cfg(unix)]
fn replaced_database(copy: &Path) -> Result<Connection, EngineError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let opened = Connection::open_with_flags(copy.join(DATABASE), flags).map_err(error)?;
    opened
        .execute_batch("PRAGMA query_only = ON")
        .map_err(error)?;
    Ok(opened)
}

/// What a restore's sweep calls the generated SDK homes and native sign-ins in its log.
#[cfg(unix)]
const PROFILES: &str = "account-profiles";

/// Settles each generated SDK home and native profile slot the replaced 2.5 copy names that
/// `named` does not keep: one a saved account of the copy names moves into the same folder
/// of that dated copy ([`crate::engine::bootstrap::move_aside`]), and one only the copy's
/// trash still notes, of an account the owner removed in 2.5, is deleted through the start
/// sweep's own removal ([`super::trash::delete`]). Every keep test runs before the first
/// change, and a failed one keeps that one in place and does not stop the others. Inputs: the
/// engine root, the copy's folder and database, and the keep test by kind and name. Output:
/// how many were settled, or the copy's or keep test's failure, or the first failed change.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
const SWEEP_NOTES_MAX: i64 = 10_000;

/// Whether a restored app plugin binding holds a plugin key by its file name; a database
/// with no binding table, as every 2.4 copy, holds none. Inputs: the restored connection
/// and the name. Output: the answer, or any SQLite failure.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
fn restore_failed(cause: impl std::fmt::Display) -> EngineError {
    error(format!("{RESTORE_FAILED}: {cause}"))
}

/// Opens engine.db and holds it alone until the connection closes: exclusive locking mode
/// and an exclusive transaction, with no wait, as the probe in changes.rs does. Any other
/// connection, in this process or another, refuses the restore, and none can open while it
/// runs. Input: the database path. Output: the connection, or `storage_restore_busy`.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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
    let made = fill(db, &target, &|| settings_texts(settings))
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
#[cfg(unix)]
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

/// The private file beside engine.db that says how the owner's last restore stands, so a
/// client can say so while no engine answers: `pending`, `restored <copy>` or
/// `failed <code>`.
#[cfg(unix)]
pub(crate) const RESTORE_RECORD_FILE: &str = "restore-record";
/// The record of a restore the engine stops for.
#[cfg(unix)]
pub(crate) const RECORD_PENDING: &str = "pending";
/// The first word of the record of a restore that brought the 2.4 data back.
#[cfg(unix)]
pub(crate) const RECORD_RESTORED: &str = "restored";
/// The first word of the record of a restore that changed nothing.
#[cfg(unix)]
pub(crate) const RECORD_FAILED: &str = "failed";
/// The most a restore record holds.
#[cfg(unix)]
const RESTORE_RECORD_BYTES_MAX: usize = 512;

/// Replaces the restore record of the engine at `root` with one line, written private and
/// synced. Inputs: the root and the line. Output: Ok once it is on disk.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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
