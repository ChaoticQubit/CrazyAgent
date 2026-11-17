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

/// Creates the saved accounts table, so the start sweep can check a key against it.
/// Input: the engine connection. Output: any SQLite failure.
pub(crate) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(error)
}

pub(crate) enum Command {
    Add(Box<ModelConnection>, AccountDetails),
    Rename(CredentialRef, String),
    Remove(CredentialRef),
    List(Option<CredentialRef>, usize),
    Get(CredentialRef),
    /// A new key replaced an account's key: the account and its key reference.
    KeyReplaced(CredentialRef, CredentialRef),
    Held,
}

pub(crate) struct Listed {
    pub(crate) connection: ModelConnection,
    pub(crate) details: AccountDetails,
    pub(crate) sign_in: Option<NativeAuthPhase>,
    pub(crate) launchable: bool,
    pub(crate) rejected: bool,
    pub(crate) rechecking: bool,
}

pub(crate) enum ResultValue {
    Account(Box<Listed>),
    Removed,
    Page(Vec<Listed>, Option<CredentialRef>),
    Held(Vec<CredentialRef>),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    connection: ModelConnection,
    details: AccountDetails,
}

/// The folder beside the engine database that holds account keys, one 0600 file per key
/// named by its credential reference.
pub(crate) const ACCOUNT_KEYS: &str = "account-keys";

/// The folder beside the engine database that holds the keys the owner entered for app
/// plugins, one 0600 file per key named by its reference.
pub(crate) const PLUGIN_KEYS: &str = "plugin-keys";

/// The owner-only key folders beside the engine database, which no agent sandbox may read:
/// account keys and app plugin keys.
pub(crate) const KEY_FOLDERS: [&str; 2] = [ACCOUNT_KEYS, PLUGIN_KEYS];

/// Deletes one account key file from the engine root's owner-only key folder, for the start
/// sweep of keys a committed removal left behind. Inputs: the engine root and the key's
/// credential reference. Output: Ok when the file is gone, including when it or the folder
/// was already missing; `account_key_store_unsafe` for a folder that is not this user's 0700
/// directory.
#[cfg(unix)]
pub(crate) fn remove_key_file(root: &Path, name: &str) -> Result<(), EngineError> {
    remove_key_in(root, ACCOUNT_KEYS, name)
}

/// Deletes one key file from one of the engine root's owner-only key folders. Inputs: the
/// engine root, a folder from [`KEY_FOLDERS`] and the key name. Output: Ok when the file is
/// gone, including when it or the folder was already missing; `account_key_store_unsafe`
/// for a folder that is not this user's 0700 directory.
#[cfg(unix)]
pub(crate) fn remove_key_in(root: &Path, folder: &str, name: &str) -> Result<(), EngineError> {
    use rustix::fs::AtFlags;
    let Some(directory) = key_folder_in(root, folder, name)? else {
        return Ok(());
    };
    match rustix::fs::unlinkat(&directory, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(_) => Err(error("account_key_store_unavailable")),
    }
}

/// Opens one of the engine root's key folders for one key name, refusing a name that is not
/// a plain credential reference. Inputs: the engine root, a folder from [`KEY_FOLDERS`] and
/// the key name. Output: the folder, none when it does not exist,
/// `account_key_reference_invalid`, or `account_key_store_unsafe` for a folder that is not
/// this user's 0700 directory.
#[cfg(unix)]
fn key_folder_in(
    root: &Path,
    folder: &str,
    name: &str,
) -> Result<Option<std::fs::File>, EngineError> {
    if CredentialRef::new(name).is_err() || name.starts_with('.') {
        return Err(error("account_key_reference_invalid"));
    }
    open_key_folder(root, folder)
}

/// Opens one of the key folders of the engine root or of a restore's dated copy. Inputs: the
/// engine root or the copy, and a folder from [`KEY_FOLDERS`]. Output: the folder, none when
/// it does not exist, or `account_key_store_unsafe` for a folder that is not this user's 0700
/// directory.
#[cfg(unix)]
fn open_key_folder(root: &Path, folder: &str) -> Result<Option<std::fs::File>, EngineError> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::MetadataExt;
    assert!(KEY_FOLDERS.contains(&folder), "a known key folder");
    let directory = match rustix::fs::open(
        root.join(folder),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => std::fs::File::from(fd),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(error("account_key_store_unavailable")),
    };
    let metadata = directory.metadata().map_err(error)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(error("account_key_store_unsafe"));
    }
    Ok(Some(directory))
}

/// The longest account key the store holds, as the owner transport writes it.
#[cfg(unix)]
const KEY_BYTES_MAX: u64 = 8192;

/// Reads one saved account key so the engine can hand it to its key relay, never to an agent
/// (DEC-141 PA-4). Inputs: the engine root and the key's credential reference. Output: the
/// key, none when it was never saved, or `account_key_store_unsafe` for a key file that is not
/// this user's single-link 0600 file; a link in its place is `account_key_store_unavailable`.
#[cfg(unix)]
pub(crate) fn read_key_file(
    root: &Path,
    credential: &CredentialRef,
) -> Result<Option<crate::credentials::SecretString>, EngineError> {
    read_key_in(root, ACCOUNT_KEYS, credential)
}

/// Reads one saved key from one of the engine root's owner-only key folders into engine
/// memory only. Inputs: the engine root, a folder from [`KEY_FOLDERS`] and the key's
/// reference. Output: the key, none when it was never saved, or the refusals of
/// [`read_key_file`].
#[cfg(unix)]
pub(crate) fn read_key_in(
    root: &Path,
    folder: &str,
    credential: &CredentialRef,
) -> Result<Option<crate::credentials::SecretString>, EngineError> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let Some(directory) = key_folder_in(root, folder, credential.as_str())? else {
        return Ok(None);
    };
    let file = match rustix::fs::openat(
        &directory,
        credential.as_str(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => std::fs::File::from(fd),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(error("account_key_store_unavailable")),
    };
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.len() > KEY_BYTES_MAX
    {
        return Err(error("account_key_store_unsafe"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let read = file.take(KEY_BYTES_MAX + 1).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > KEY_BYTES_MAX {
        crate::credentials::wipe(&mut bytes);
        return Err(error("account_key_store_unavailable"));
    }
    crate::credentials::SecretString::new(bytes)
        .map(Some)
        .map_err(|_| error("account_key_store_unsafe"))
}

/// Whether a key is saved in one of the engine root's owner-only key folders, without
/// reading it. Inputs: the engine root, a folder from [`KEY_FOLDERS`] and the key's
/// reference. Output: true for a regular file, false when it or the folder is missing, or
/// the folder's refusal.
#[cfg(unix)]
pub(crate) fn key_saved_in(
    root: &Path,
    folder: &str,
    credential: &CredentialRef,
) -> Result<bool, EngineError> {
    let Some(directory) = key_folder_in(root, folder, credential.as_str())? else {
        return Ok(false);
    };
    match rustix::fs::statat(
        &directory,
        credential.as_str(),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => {
            Ok(rustix::fs::FileType::from_raw_mode(stat.st_mode)
                == rustix::fs::FileType::RegularFile)
        }
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(_) => Err(error("account_key_store_unavailable")),
    }
}

/// Saves or replaces one key in one of the engine root's owner-only key folders, creating
/// the 0700 folder when missing. The key goes to a staged 0600 file renamed over the old
/// one, so no file ever holds a partial key. Inputs: the engine root, a folder from
/// [`KEY_FOLDERS`], the key's reference and the key bytes, which are wiped. Output: Ok with
/// the folder synced; on failure the old key stays and nothing is staged.
#[cfg(unix)]
pub(crate) fn replace_key_in(
    root: &Path,
    folder: &str,
    credential: &CredentialRef,
    mut key: Vec<u8>,
) -> Result<(), EngineError> {
    let saved = replace_staged(root, folder, credential, &key);
    crate::credentials::wipe(&mut key);
    saved
}

/// The disk work of [`replace_key_in`]: checks the key's length, makes the folder, writes
/// and syncs a staged file and renames it over the key. Inputs: as there, with the key
/// borrowed. Output: Ok once the folder is synced; a failed write or rename removes the
/// staged file and is `account_key_store_unavailable`.
#[cfg(unix)]
fn replace_staged(
    root: &Path,
    folder: &str,
    credential: &CredentialRef,
    key: &[u8],
) -> Result<(), EngineError> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    if key.is_empty() || key.len() as u64 > KEY_BYTES_MAX {
        return Err(error("account_key_store_unavailable"));
    }
    assert!(KEY_FOLDERS.contains(&folder), "a known key folder");
    match rustix::fs::mkdir(root.join(folder), Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(_) => return Err(error("account_key_store_unavailable")),
    }
    let directory = key_folder_in(root, folder, credential.as_str())?
        .ok_or_else(|| error("account_key_store_unavailable"))?;
    let staged = format!("{STAGED_KEY}{}", crate::engine::owner::random_id()?);
    let mut file = std::fs::File::from(
        rustix::fs::openat(
            &directory,
            staged.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| error("account_key_store_unavailable"))?,
    );
    let written = std::io::Write::write_all(&mut file, key).and_then(|()| file.sync_all());
    if written.is_err()
        || rustix::fs::renameat(&directory, staged.as_str(), &directory, credential.as_str())
            .is_err()
    {
        let _ = rustix::fs::unlinkat(&directory, staged.as_str(), AtFlags::empty());
        return Err(error("account_key_store_unavailable"));
    }
    directory
        .sync_all()
        .map_err(|_| error("account_key_store_unavailable"))
}

/// The name prefix of a key copy a replace stages before renaming it over its key; the
/// account key writer stages under the same prefix.
#[cfg(unix)]
const STAGED_KEY: &str = ".replace-";

/// The most staged key copies one engine start deletes from one key folder; any more wait
/// for the next start.
#[cfg(unix)]
const STAGED_SWEEP_MAX: usize = 64;

/// Deletes the staged key copies a crash between a replace's write and its rename left in
/// one of the engine root's owner-only key folders, at most [`STAGED_SWEEP_MAX`]. The engine
/// runs it at start, before any replace can stage one. Inputs: the engine root and a folder
/// from [`KEY_FOLDERS`]. Output: Ok once those found are gone or the folder is missing, the
/// folder's refusal, or `account_key_store_unavailable`.
#[cfg(unix)]
pub(crate) fn remove_staged_in(root: &Path, folder: &str) -> Result<(), EngineError> {
    let Some(directory) = open_key_folder(root, folder)? else {
        return Ok(());
    };
    let mut staged = Vec::new();
    for entry in rustix::fs::Dir::read_from(&directory)
        .map_err(|_| error("account_key_store_unavailable"))?
    {
        let entry = entry.map_err(|_| error("account_key_store_unavailable"))?;
        if entry
            .file_name()
            .to_bytes()
            .starts_with(STAGED_KEY.as_bytes())
        {
            staged.push(entry.file_name().to_owned());
            if staged.len() == STAGED_SWEEP_MAX {
                break;
            }
        }
    }
    assert!(staged.len() <= STAGED_SWEEP_MAX);
    for name in &staged {
        match rustix::fs::unlinkat(&directory, name.as_c_str(), rustix::fs::AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => {}
            Err(_) => return Err(error("account_key_store_unavailable")),
        }
    }
    directory
        .sync_all()
        .map_err(|_| error("account_key_store_unavailable"))
}

/// The most entries one restore reads from one key folder; any more stay, and the restore
/// logs `restore_key_sweep_bounded`.
#[cfg(unix)]
const KEY_SWEEP_MAX: usize = 10_000;

/// Moves, after a restore to a 2.4 copy (V250-L5-N5), every key in one of the engine root's
/// owner-only key folders that `kept` does not keep into the same-named new 0700 folder of
/// the restore's dated copy, so that copy holds the keys its data names (DEC-192), and
/// deletes the staged copies. It reads at most [`KEY_SWEEP_MAX`] entries and leaves a name
/// that is no plain reference alone. A key whose move fails stays where it is and does not
/// stop the others; a failed keep test stops the sweep before any key moves. Inputs: the engine
/// root, a folder from [`KEY_FOLDERS`], the dated copy's folder and the keep test by key
/// name. Output: how many keys moved, or the folder's refusal, the keep test's failure or the
/// first failed move.
#[cfg(unix)]
pub(crate) fn move_unkept_in(
    root: &Path,
    folder: &str,
    copy: &Path,
    kept: impl Fn(&str) -> Result<bool, EngineError>,
) -> Result<usize, EngineError> {
    let mut failed = remove_staged_in(root, folder).err();
    let Some(directory) = open_key_folder(root, folder)? else {
        return failed.map_or(Ok(0), Err);
    };
    let mut names = Vec::new();
    let mut read = 0;
    for entry in rustix::fs::Dir::read_from(&directory)
        .map_err(|_| error("account_key_store_unavailable"))?
    {
        if read == KEY_SWEEP_MAX {
            eprintln!("restore_key_sweep_bounded folder={folder}");
            break;
        }
        read += 1;
        let entry = entry.map_err(|_| error("account_key_store_unavailable"))?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if !name.starts_with('.') && CredentialRef::new(name).is_ok() {
            names.push(name.to_owned());
        }
    }
    assert!(names.len() <= read && read <= KEY_SWEEP_MAX);
    let mut unkept = Vec::new();
    for name in names {
        if !kept(name.as_str())? {
            unkept.push(name);
        }
    }
    if unkept.is_empty() {
        return failed.map_or(Ok(0), Err);
    }
    let into = match copy_key_folder(copy, folder) {
        Ok(made) => made,
        Err(cause) => return Err(failed.unwrap_or(cause)),
    };
    let mut moved = 0;
    for name in &unkept {
        match rustix::fs::renameat(&directory, name.as_str(), &into, name.as_str()) {
            Ok(()) => moved += 1,
            Err(_) => {
                failed.get_or_insert(error("account_key_store_unavailable"));
            }
        }
    }
    assert!(moved <= unkept.len());
    if moved > 0 && (into.sync_all().is_err() || directory.sync_all().is_err()) {
        failed.get_or_insert(error("account_key_store_unavailable"));
    }
    failed.map_or(Ok(moved), Err)
}

/// Makes and opens one key folder in a restore's dated copy, which must not hold it yet, as
/// this user's 0700 directory, with the copy synced so the new folder lasts. Inputs: the
/// dated copy's folder and a folder from [`KEY_FOLDERS`]. Output: the new folder, the
/// [`open_key_folder`] refusals, or `account_key_store_unavailable`.
#[cfg(unix)]
fn copy_key_folder(copy: &Path, folder: &str) -> Result<std::fs::File, EngineError> {
    rustix::fs::mkdir(copy.join(folder), rustix::fs::Mode::from_raw_mode(0o700))
        .map_err(|_| error("account_key_store_unavailable"))?;
    let made =
        open_key_folder(copy, folder)?.ok_or_else(|| error("account_key_store_unavailable"))?;
    std::fs::File::open(copy)
        .and_then(|parent| parent.sync_all())
        .map_err(|_| error("account_key_store_unavailable"))?;
    Ok(made)
}

/// Refuses to read an account key where the Unix owner-only key folder does not exist.
/// Inputs: the engine root and credential, both unused. Output: `account_key_store_unavailable`.
#[cfg(not(unix))]
pub(crate) fn read_key_file(
    _root: &Path,
    _credential: &CredentialRef,
) -> Result<Option<crate::credentials::SecretString>, EngineError> {
    Err(error("account_key_store_unavailable"))
}

/// Refuses to read a key where the Unix owner-only key folders do not exist. Inputs: the
/// engine root, folder and reference, all unused. Output: `account_key_store_unavailable`.
#[cfg(not(unix))]
pub(crate) fn read_key_in(
    _root: &Path,
    _folder: &str,
    _credential: &CredentialRef,
) -> Result<Option<crate::credentials::SecretString>, EngineError> {
    Err(error("account_key_store_unavailable"))
}

/// Refuses to check for a key where the Unix owner-only key folders do not exist. Inputs:
/// the engine root, folder and reference, all unused. Output: `account_key_store_unavailable`.
#[cfg(not(unix))]
pub(crate) fn key_saved_in(
    _root: &Path,
    _folder: &str,
    _credential: &CredentialRef,
) -> Result<bool, EngineError> {
    Err(error("account_key_store_unavailable"))
}

/// Refuses to save a key where the Unix owner-only key folders do not exist; the key is
/// wiped. Inputs: the engine root, folder and reference, unused, and the key. Output:
/// `account_key_store_unavailable`.
#[cfg(not(unix))]
pub(crate) fn replace_key_in(
    _root: &Path,
    _folder: &str,
    _credential: &CredentialRef,
    mut key: Vec<u8>,
) -> Result<(), EngineError> {
    crate::credentials::wipe(&mut key);
    Err(error("account_key_store_unavailable"))
}

/// Refuses to delete a key where the Unix owner-only key folders do not exist. Inputs: the
/// engine root, folder and key name, all unused. Output: `account_key_store_unavailable`.
#[cfg(not(unix))]
pub(crate) fn remove_key_in(_root: &Path, _folder: &str, _name: &str) -> Result<(), EngineError> {
    Err(error("account_key_store_unavailable"))
}

/// The display name and API-key reference of one saved account, for an app plugin secret
/// bound to that account's key. Inputs: the engine connection and the account id. Output:
/// none for an unknown account; the key reference is none for an account that does not
/// sign in with an API key.
pub(crate) fn api_key_account(
    db: &Connection,
    id: &CredentialRef,
) -> Result<Option<(String, Option<CredentialRef>)>, EngineError> {
    Ok(registered(db, id)?.map(|(_, record)| {
        let key = match record.connection.auth {
            ModelAuthentication::ApiKey { credential_ref } => Some(credential_ref),
            _ => None,
        };
        (record.details.name, key)
    }))
}

#[cfg(all(test, unix))]
#[path = "account_key_tests.rs"]
mod account_key_tests;

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
    initialize(db)?;
    match command {
        Command::List(after, limit) => list(db, config, admission, after.as_ref(), limit),
        Command::Held => {
            let tx = db.transaction().map_err(error)?;
            check_owner_admission(&tx, admission)?;
            let held = config
                .settings
                .model_access
                .as_ref()
                .map(|catalog| catalog.connections.iter().map(|c| c.id.clone()).collect())
                .unwrap_or_default();
            Ok(ResultValue::Held(held))
        }
        Command::Get(id) => {
            let tx = db.transaction().map_err(error)?;
            check_owner_admission(&tx, admission)?;
            let (connection, details) =
                find(&tx, config, &id)?.ok_or_else(|| error("account_not_found"))?;
            Ok(ResultValue::Account(Box::new(listed(
                &tx, admission, connection, details,
            )?)))
        }
        Command::KeyReplaced(id, credential) => {
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(error)?;
            check_owner_admission(&tx, admission)?;
            let Some((connection, details)) = find(&tx, config, &id)? else {
                super::trash::put(&tx, super::trash::ACCOUNT_KEY, credential.as_str())?;
                tx.commit().map_err(error)?;
                #[cfg(unix)]
                settle_removed(
                    db,
                    config.settings.model_access.as_ref(),
                    super::trash::ACCOUNT_KEY,
                    credential.as_str(),
                );
                return Err(error("account_not_found"));
            };
            if !matches!(connection.auth, ModelAuthentication::ApiKey { .. }) {
                return Err(error("account_key_not_allowed"));
            }
            super::master_session::lift_account_holds(&tx, &connection)?;
            let account = listed(&tx, admission, connection, details)?;
            assert!(!account.rejected);
            tx.commit().map_err(error)?;
            Ok(ResultValue::Account(Box::new(account)))
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
            #[cfg(unix)]
            let saved = registered(&tx, &id)?.map(|(_, record)| record.connection.auth);
            tx.execute("DELETE FROM accounts WHERE id=?1", [id.as_str()])
                .map_err(error)?;
            crate::usage::forget_limits(&tx, &id)?;
            #[cfg(not(unix))]
            if let ModelAuthentication::ApiKey { credential_ref } = &connection.auth {
                super::trash::put(&tx, super::trash::ACCOUNT_KEY, credential_ref.as_str())?;
            }
            #[cfg(unix)]
            let left: std::collections::BTreeSet<(&str, &str)> = account_files(&connection.auth)
                .into_iter()
                .chain(saved.iter().flat_map(account_files))
                .collect();
            #[cfg(unix)]
            for (kind, name) in &left {
                super::trash::put(&tx, kind, name)?;
            }
            tx.commit().map_err(error)?;
            #[cfg(unix)]
            for (kind, name) in &left {
                settle_removed(db, config.settings.model_access.as_ref(), kind, name);
            }
            Ok(ResultValue::Removed)
        }
    }
}

/// Names the files an account's authentication leaves to delete once nothing names it: an
/// API key account's key and generated SDK home, or a native account's sign-in.
/// Input: the authentication. Output: each trash kind with its reference, none for others.
#[cfg(unix)]
fn account_files(auth: &ModelAuthentication) -> Vec<(&'static str, &str)> {
    match auth {
        ModelAuthentication::ApiKey { credential_ref } => vec![
            (super::trash::ACCOUNT_KEY, credential_ref.as_str()),
            (super::trash::GENERATED_HOME, credential_ref.as_str()),
        ],
        ModelAuthentication::NativeManaged { profile_ref } => {
            vec![(super::trash::NATIVE_SIGN_IN, profile_ref.as_str())]
        }
        _ => Vec::new(),
    }
}

/// Names the files a settings change leaves no connection naming: each file of a connection
/// before the change that no connection after it names with the same kind and reference.
/// Inputs: the catalogs before and after. Output: those kinds with their references, once each.
#[cfg(unix)]
pub(super) fn dropped_files(
    before: Option<&crate::config::ModelAccess>,
    after: Option<&crate::config::ModelAccess>,
) -> std::collections::BTreeSet<(&'static str, String)> {
    let files = |catalog: Option<&crate::config::ModelAccess>| {
        catalog
            .into_iter()
            .flat_map(|access| &access.connections)
            .flat_map(|connection| account_files(&connection.auth))
            .map(|(kind, name)| (kind, name.to_owned()))
            .collect::<std::collections::BTreeSet<_>>()
    };
    files(before).difference(&files(after)).cloned().collect()
}

/// Deletes a removed account's files, or a dropped connection's, straight after the change
/// commits; what cannot go now stays noted for the next start. Inputs: the connection, the
/// catalog left after the change, the kind and the reference.
#[cfg(unix)]
pub(super) fn settle_removed(
    db: &Connection,
    catalog: Option<&crate::config::ModelAccess>,
    kind: &str,
    name: &str,
) {
    let settled = db
        .path()
        .and_then(|path| std::path::Path::new(path).parent())
        .ok_or_else(|| error("account_file_root_unknown"))
        .and_then(|root| super::trash::settle(db, root, catalog, kind, name));
    if let Err(problem) = settled {
        eprintln!("file_trash_not_removed kind={kind} cause={}", problem.0);
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

/// Takes a project Delete everything removed out of every pool in the saved settings, as
/// choosing no pool for it does, so no pool lists it or counts it toward its project cap
/// (rv-sec2 bgjobs-3b F2). A delete that kept the project's records keeps its pool too.
/// Inputs: the database, the config, the settings sources, the owner's admission and the
/// project. Output: Ok once no pool lists a removed project, else the error.
pub(super) fn forget_pool_project(
    db: &mut Connection,
    config: &mut EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    admission: &Admission,
    project: &ProjectId,
) -> Result<(), EngineError> {
    let kept: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if kept {
        return Ok(());
    }
    let reference = CredentialRef::new(project.as_str()).map_err(error)?;
    let pooled = |config: &EffectiveConfig| {
        config.settings.model_access.as_ref().is_some_and(|access| {
            access
                .pools
                .iter()
                .any(|pool| pool.projects.contains(&reference))
        })
    };
    if !pooled(&*config) {
        return Ok(());
    }
    publish(
        db,
        config,
        sources,
        admission,
        ModelAccessEdit::AssignProjectPool {
            project: reference.clone(),
            pool: None,
        },
    )?;
    assert!(
        !pooled(&*config),
        "a published no-pool edit leaves no pool listing the project"
    );
    Ok(())
}

/// Gives a demo just made exactly the pool its Create picked when another pool still lists
/// an earlier demo of the same id, so that pool never serves the new one (rv-sa241 LOW-1);
/// when the picked pool can't take it, the demo leaves every pool. With no such pool it
/// changes nothing and the client assigns the pool as before. Inputs: the database, the
/// config, the settings sources, the owner's admission, the demo and the picked pool.
/// Output: Ok, else the error, with the demo in no pool.
pub(super) fn settle_demo_pool(
    db: &mut Connection,
    config: &mut EffectiveConfig,
    sources: Option<&settings::SettingsSources>,
    admission: &Admission,
    project: &ProjectId,
    pool: &CredentialRef,
) -> Result<(), EngineError> {
    let reference = CredentialRef::new(project.as_str()).map_err(error)?;
    let listing = |config: &EffectiveConfig| -> Vec<CredentialRef> {
        config
            .settings
            .model_access
            .as_ref()
            .map(|access| {
                access
                    .pools
                    .iter()
                    .filter(|listed| listed.projects.contains(&reference))
                    .map(|listed| listed.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    if listing(&*config).iter().all(|listed| listed == pool) {
        return Ok(());
    }
    let assigned = publish(
        db,
        config,
        sources,
        admission,
        ModelAccessEdit::AssignProjectPool {
            project: reference.clone(),
            pool: Some(pool.clone()),
        },
    );
    if let Err(problem) = assigned {
        if !listing(&*config).is_empty() {
            publish(
                db,
                config,
                sources,
                admission,
                ModelAccessEdit::AssignProjectPool {
                    project: reference.clone(),
                    pool: None,
                },
            )?;
        }
        assert!(
            listing(&*config).is_empty(),
            "a refused assign leaves the demo in no pool"
        );
        return Err(problem);
    }
    assert_eq!(listing(&*config), std::slice::from_ref(pool));
    Ok(())
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
    let more = skip + items.len() < unregistered.len() || rows.len() > wanted;
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
    let next = if more {
        items.last().map(|item| item.connection.id.clone())
    } else {
        None
    };
    assert!(items.len() <= limit);
    Ok(ResultValue::Page(items, next))
}

/// Lists one account with its latest sign-in, whether the provider rejected it, whether it
/// passes the launch check every master, team and research turn uses on this engine run, so
/// status never shows an account as signed in when a launch or the master would refuse it,
/// and whether a sign-in that fails only that check waits for this run's re-check (DEC-108);
/// a re-check that cannot be read counts as none, so one account never fails the list.
/// Inputs: the transaction, the admitted owner, the connection and its details.
/// Output: the listed account.
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
    let rejected = crate::model_gateway::sign_in_rejected(tx, &connection)?;
    let host = HostStamp {
        db_instance: admission.db_instance.clone(),
        engine_epoch: admission.engine_epoch.clone(),
    };
    let launchable = sign_in == Some(NativeAuthPhase::AuthenticatedObserved)
        && !rejected
        && crate::storage::native_sign_in(tx, &host, &connection, None).is_ok();
    assert!(!launchable || sign_in == Some(NativeAuthPhase::AuthenticatedObserved));
    let rechecking = sign_in == Some(NativeAuthPhase::AuthenticatedObserved)
        && !launchable
        && !rejected
        && crate::storage::sign_in_rechecking(tx, &host, &connection).unwrap_or(false);
    assert!(!(launchable && rechecking));
    Ok(Listed {
        connection,
        details,
        sign_in,
        launchable,
        rejected,
        rechecking,
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
    let Some(bytes) = row else {
        return Ok(None);
    };
    let phase = crate::engine::owner::native_auth::decode_fact(&bytes)?.phase;
    if super::native_auth::profile_fenced(tx, &key)? {
        return Ok(Some(phase.releasing()));
    }
    Ok(Some(phase))
}
