use super::*;
use crate::config::{edit_model_access, resolve_config_bytes};
#[cfg(unix)]
use rustix::fs::{AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
#[cfg(unix)]
use std::io::{Read, Write};
use std::path::PathBuf;

pub(super) const SETTINGS_JOURNAL_MAX: usize = 5 * crate::config::CONFIG_BYTES_MAX;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileState {
    identity: Option<(u64, u64)>,
    bytes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceState {
    source_id: String,
    global: FileState,
    project: FileState,
}

impl SourceState {
    fn hash(&self) -> Result<String, EngineError> {
        Ok(crate::memory::hash(
            &serde_json::to_vec(self).map_err(error)?,
        ))
    }

    fn config(&self) -> Result<EffectiveConfig, EngineError> {
        resolve_config_bytes(&self.global.bytes, &self.project.bytes).map_err(|e| error(e.0))
    }
}

struct SourceFile {
    #[cfg(unix)]
    parent: std::fs::File,
    path: PathBuf,
    identity: (u64, u64),
    name: OsString,
}

#[cfg(unix)]
fn private(metadata: &std::fs::Metadata, directory: bool) -> Result<(), EngineError> {
    if metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        }
    {
        return Err(error("settings_metadata_invalid"));
    }
    Ok(())
}

#[cfg(unix)]
fn identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

impl SourceFile {
    /// Refuses to open a settings source without Unix owner, mode and inode checks.
    /// Takes the source path, unused.
    /// Returns `settings_unavailable` until Windows has its own owner-only file checks.
    #[cfg(not(unix))]
    fn open(_path: &Path) -> Result<Self, EngineError> {
        Err(error("settings_unavailable"))
    }

    #[cfg(unix)]
    fn open(path: &Path) -> Result<Self, EngineError> {
        let name = path
            .file_name()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| error("settings_path_invalid"))?
            .to_owned();
        let path = std::fs::canonicalize(
            path.parent()
                .ok_or_else(|| error("settings_path_invalid"))?,
        )
        .map_err(error)?;
        let parent = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&path)
            .map_err(error)?;
        let metadata = parent.metadata().map_err(error)?;
        private(&metadata, true)?;
        let source = Self {
            parent,
            path,
            identity: identity(&metadata),
            name,
        };
        source.verify()?;
        source.read()?;
        Ok(source)
    }

    #[cfg(unix)]
    fn verify(&self) -> Result<(), EngineError> {
        let metadata = std::fs::symlink_metadata(&self.path).map_err(error)?;
        private(&metadata, true)?;
        if identity(&metadata) != self.identity {
            return Err(error("settings_parent_replaced"));
        }
        Ok(())
    }

    /// Refuses to read a file beside a settings source without directory-relative opens.
    /// Takes the file name, unused.
    /// Returns `settings_unavailable` until Windows has its own owner-only file checks.
    #[cfg(not(unix))]
    fn read_name(&self, _name: &std::ffi::OsStr) -> Result<FileState, EngineError> {
        Err(error("settings_unavailable"))
    }

    #[cfg(unix)]
    fn read_name(&self, name: &std::ffi::OsStr) -> Result<FileState, EngineError> {
        self.verify()?;
        let fd = match rustix::fs::openat(
            &self.parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => {
                return Ok(FileState {
                    identity: None,
                    bytes: String::new(),
                });
            }
            Err(e) => return Err(error(e)),
        };
        let mut file = std::fs::File::from(fd);
        let metadata = file.metadata().map_err(error)?;
        private(&metadata, false)?;
        if metadata.len() > crate::config::CONFIG_BYTES_MAX as u64 {
            return Err(error("settings_file_bounds"));
        }
        let mut bytes = String::new();
        (&mut file)
            .take(crate::config::CONFIG_BYTES_MAX as u64 + 1)
            .read_to_string(&mut bytes)
            .map_err(error)?;
        if bytes.len() > crate::config::CONFIG_BYTES_MAX {
            return Err(error("settings_file_bounds"));
        }
        let after = file.metadata().map_err(error)?;
        if identity(&metadata) != identity(&after)
            || metadata.len() != after.len()
            || metadata.mtime() != after.mtime()
            || metadata.mtime_nsec() != after.mtime_nsec()
        {
            return Err(error("settings_source_changed"));
        }
        let named =
            rustix::fs::statat(&self.parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
        if (named.st_dev as u64, named.st_ino) != identity(&metadata) {
            return Err(error("settings_source_replaced"));
        }
        assert!(bytes.len() <= crate::config::CONFIG_BYTES_MAX);
        Ok(FileState {
            identity: Some(identity(&metadata)),
            bytes,
        })
    }

    fn read(&self) -> Result<FileState, EngineError> {
        self.read_name(&self.name)
    }
}

pub(crate) struct SettingsSources {
    global: SourceFile,
    project: SourceFile,
    #[cfg(unix)]
    lease: std::fs::File,
    #[cfg(unix)]
    lease_name: OsString,
    #[cfg(unix)]
    source_id: String,
}

impl SettingsSources {
    /// Refuses to hold the writable settings sources without a Unix file lease.
    /// Takes the global and project paths, unused.
    /// Returns `settings_unavailable` until Windows has its own owner-only file checks.
    #[cfg(not(unix))]
    pub(super) fn open(_global: &Path, _project: &Path) -> Result<Self, EngineError> {
        Err(error("settings_unavailable"))
    }

    #[cfg(unix)]
    pub(super) fn open(global: &Path, project: &Path) -> Result<Self, EngineError> {
        let global = SourceFile::open(global)?;
        let project = SourceFile::open(project)?;
        if global.path.join(&global.name) == project.path.join(&project.name) {
            return Err(error("settings_sources_overlap"));
        }
        let source_id = crate::memory::hash(
            &serde_json::to_vec(&(
                global.path.join(&global.name),
                global.identity,
                project.path.join(&project.name),
                project.identity,
            ))
            .map_err(error)?,
        );
        let lease_name = OsString::from(format!(
            ".crazyagent-settings-{}.lock",
            crate::memory::hash(global.name.as_encoded_bytes())
        ));
        let fd = rustix::fs::openat(
            &global.parent,
            &lease_name,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(error)?;
        let lease = std::fs::File::from(fd);
        private(&lease.metadata().map_err(error)?, false)?;
        rustix::fs::flock(&lease, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| error("settings_writer_busy"))?;
        let sources = Self {
            global,
            project,
            lease,
            lease_name,
            source_id,
        };
        sources.snapshot()?.config()?;
        Ok(sources)
    }

    /// Reads the held global and project settings for an upgrade backup.
    /// Output: the backup file name and text of each settings file that exists;
    /// a file that does not exist yet is left out.
    #[cfg(unix)]
    pub(super) fn backup_texts(&self) -> Result<Vec<(&'static str, String)>, EngineError> {
        let state = self.snapshot()?;
        Ok([
            ("global.toml", state.global),
            ("project.toml", state.project),
        ]
        .into_iter()
        .filter(|(_, file)| file.identity.is_some())
        .map(|(name, file)| (name, file.bytes))
        .collect())
    }

    pub(super) fn retained_paths(&self) -> (PathBuf, PathBuf) {
        (
            self.global.path.join(&self.global.name),
            self.project.path.join(&self.project.name),
        )
    }

    /// Refuses to read the held settings sources without a Unix file lease.
    /// Takes the held sources.
    /// Returns `settings_unavailable` until Windows has its own owner-only file checks.
    #[cfg(not(unix))]
    fn snapshot(&self) -> Result<SourceState, EngineError> {
        Err(error("settings_unavailable"))
    }

    #[cfg(unix)]
    fn snapshot(&self) -> Result<SourceState, EngineError> {
        let named = self.global.read_name(&self.lease_name)?;
        if named.identity != Some(identity(&self.lease.metadata().map_err(error)?)) {
            return Err(error("settings_lease_replaced"));
        }
        Ok(SourceState {
            source_id: self.source_id.clone(),
            global: self.global.read()?,
            project: self.project.read()?,
        })
    }

    pub(super) fn config(&self) -> Result<EffectiveConfig, EngineError> {
        self.snapshot()?.config()
    }

    /// Refuses to publish a settings change without directory-relative renames.
    /// Takes the database and pending change, both unused.
    /// Returns `settings_unavailable` until Windows has its own owner-only file checks.
    #[cfg(not(unix))]
    fn publish(&self, _db: &Connection, _pending: &mut Pending) -> Result<(), EngineError> {
        Err(error("settings_unavailable"))
    }

    #[cfg(unix)]
    fn publish(&self, db: &Connection, pending: &mut Pending) -> Result<(), EngineError> {
        let current = self.snapshot()?;
        if current.source_id != pending.old.source_id || current.project != pending.old.project {
            return Err(error("settings_source_conflict"));
        }
        if current.global.bytes == pending.new_global
            && current.global.identity == pending.new_identity
            && pending.new_identity.is_some()
        {
            self.global.parent.sync_all().map_err(error)?;
            return Ok(());
        }
        if current.global != pending.old.global {
            return Err(error("settings_source_conflict"));
        }
        let name = OsString::from(&pending.temporary);
        if pending.new_identity.is_none() {
            let fd = match rustix::fs::openat(
                &self.global.parent,
                &name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            ) {
                Ok(fd) => Some(fd),
                Err(rustix::io::Errno::EXIST) => None,
                Err(e) => return Err(error(e)),
            };
            if let Some(fd) = fd {
                let mut file = std::fs::File::from(fd);
                file.write_all(pending.new_global.as_bytes())
                    .map_err(error)?;
                file.sync_all().map_err(error)?;
            }
            let temporary = self.global.read_name(&name)?;
            if temporary.identity.is_none() || temporary.bytes != pending.new_global {
                return Err(error("settings_temporary_conflict"));
            }
            let fd = rustix::fs::openat(
                &self.global.parent,
                &name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(error)?;
            let file = std::fs::File::from(fd);
            if Some(identity(&file.metadata().map_err(error)?)) != temporary.identity {
                return Err(error("settings_temporary_conflict"));
            }
            file.sync_all().map_err(error)?;
            self.global.parent.sync_all().map_err(error)?;
            pending.new_identity = temporary.identity;
            save_pending(db, pending)?;
        }
        let temporary = self.global.read_name(&name)?;
        if temporary.identity != pending.new_identity || temporary.bytes != pending.new_global {
            return Err(error("settings_temporary_conflict"));
        }
        if self.snapshot()? != pending.old {
            return Err(error("settings_source_conflict"));
        }
        rustix::fs::renameat(
            &self.global.parent,
            &name,
            &self.global.parent,
            &self.global.name,
        )
        .map_err(error)?;
        self.global.parent.sync_all().map_err(error)?;
        let published = self.snapshot()?;
        if published.global.identity != pending.new_identity
            || published.global.bytes != pending.new_global
            || published.project != pending.old.project
        {
            return Err(error("settings_publication_unknown"));
        }
        assert!(pending.new_identity.is_some());
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    request: OwnerRequest,
    admission: Admission,
    revision: u64,
    old: SourceState,
    new_global: String,
    temporary: String,
    new_identity: Option<(u64, u64)>,
}

#[cfg(unix)]
fn save_pending(db: &Connection, pending: &Pending) -> Result<(), EngineError> {
    let body = serde_json::to_vec(pending).map_err(error)?;
    if body.len() > SETTINGS_JOURNAL_MAX {
        return Err(error("settings_journal_bounds"));
    }
    db.execute("INSERT INTO settings_publication(singleton,body) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET body=excluded.body", [body]).map_err(error)?;
    assert!(db.is_autocommit());
    Ok(())
}

fn load_pending(db: &Connection) -> Result<Option<Pending>, EngineError> {
    let row: Option<Option<Vec<u8>>> = db.query_row(&format!("SELECT CASE WHEN length(body)<={SETTINGS_JOURNAL_MAX} THEN body END FROM settings_publication WHERE singleton=1"), [], |r| r.get(0)).optional().map_err(error)?;
    row.map(|body| {
        serde_json::from_slice(&body.ok_or_else(|| error("settings_journal_bounds"))?)
            .map_err(|_| error("settings_journal_invalid"))
    })
    .transpose()
}

pub(super) fn pending(db: &Connection) -> Result<bool, EngineError> {
    Ok(load_pending(db)?.is_some())
}

pub(super) fn ensure_clear(db: &Connection) -> Result<(), EngineError> {
    if pending(db)? {
        return Err(error("settings_publication_pending"));
    }
    Ok(())
}

pub(super) fn reconcile_pending(
    db: &Connection,
    a: &Admission,
    id: &RequestId,
    hash: Option<&str>,
) -> Result<bool, EngineError> {
    if let Some(pending) = load_pending(db)?
        && pending.admission.owner_id == a.owner_id
        && pending.request.request_id == *id
    {
        if let Some(hash) = hash
            && hash != pending.request.mutation_hash()?
        {
            return Err(error("operation_identity_conflict"));
        }
        return Ok(true);
    }
    Ok(false)
}

pub(super) fn recover(
    db: &mut Connection,
    config: &mut EffectiveConfig,
    sources: Option<&SettingsSources>,
) -> Result<(), EngineError> {
    let Some(mut pending) = load_pending(db)? else {
        return Ok(());
    };
    let sources = sources.ok_or_else(|| error("settings_recovery_requires_sources"))?;
    pending.request.validate()?;
    let OwnerOperation::Apply {
        mutation:
            OwnerMutation::ModelAccess {
                expected_config_id,
                expected_source_hash,
                edit,
            },
    } = &pending.request.operation
    else {
        return Err(error("settings_journal_invalid"));
    };
    let old_config = pending.old.config()?;
    let registered = super::accounts::pool_materialization(db, &old_config, edit)?;
    let (new_global, next) = edit_model_access(
        &pending.old.global.bytes,
        &pending.old.project.bytes,
        edit,
        &registered,
    )
    .map_err(|e| error(e.0))?;
    if &old_config.id != expected_config_id
        || pending.old.hash()? != *expected_source_hash
        || new_global != pending.new_global
        || pending.request.expected_revision.value() != pending.revision
        || pending.temporary
            != format!(
                ".crazyagent-publication-{}",
                pending.request.mutation_hash()?
            )
    {
        return Err(error("settings_journal_invalid"));
    }
    let valid_admission: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM owner_meta m JOIN owner_identities i ON i.owner_id=?1 WHERE m.singleton=1 AND m.db_instance=?2 AND i.owner_epoch>=?3)",
        params![pending.admission.owner_id, pending.admission.db_instance, pending.admission.owner_epoch], |r| r.get(0)).map_err(error)?;
    if !valid_admission
        || !is_hex_id(&pending.admission.engine_epoch)
        || pending.admission.owner_epoch < 1
    {
        return Err(error("settings_journal_admission_invalid"));
    }
    let revision: i64 = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |r| r.get(0))
        .map_err(error)?;
    if u64::try_from(revision).map_err(error)? != pending.revision {
        return Err(error("settings_pending_revision_conflict"));
    }
    sources.publish(db, &mut pending)?;
    finalize(db, &pending, &next)?;
    *config = next;
    Ok(())
}

fn finalize(
    db: &mut Connection,
    pending: &Pending,
    next: &EffectiveConfig,
) -> Result<OwnerApplied, EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    if owner_revision(&tx)? != pending.revision {
        return Err(error("settings_pending_revision_conflict"));
    }
    let revision = pending
        .revision
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(|| error("revision_exhausted"))?;
    store_config(&tx, next)?;
    let request = &pending.request;
    let hash = request.mutation_hash()?;
    let provenance = OwnerProvenance {
        origin: "native_owner".into(),
        owner_id: pending.admission.owner_id.clone(),
        operation_id: request.request_id.clone(),
        request_hash: hash,
        engine_epoch: pending.admission.engine_epoch.clone(),
        admitted_epoch: pending.admission.owner_epoch,
        resulting_epoch: pending.admission.owner_epoch,
        revision_before: Cursor::new(pending.revision),
        revision_after: Cursor::new(revision),
        event_cursor: Some(Cursor::new(revision)),
    };
    persist_event(
        &tx,
        next,
        &WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new(format!("owner:{}", pending.admission.owner_id))
                .map_err(error)?,
            request_id: request.request_id.clone(),
            expected_revision: request.expected_revision.clone(),
            command: match &request.operation {
                OwnerOperation::Apply { mutation } => mutation.command()?,
                _ => return Err(error("settings_journal_invalid")),
            },
        },
        revision,
    )?;
    owner_record(
        &tx,
        &provenance,
        &Sha256::digest(serde_json::to_vec(request).map_err(error)?),
        &serde_json::to_vec(&OwnerResult::Applied).map_err(error)?,
    )?;
    tx.execute("DELETE FROM settings_publication WHERE singleton=1", [])
        .map_err(error)?;
    tx.commit().map_err(error)?;
    assert_eq!(revision, pending.revision + 1);
    Ok(OwnerApplied {
        result: OwnerResult::Applied,
        provenance,
        historical_replay: false,
        current_revision: Cursor::new(revision),
    })
}

pub(super) fn apply(
    db: &mut Connection,
    config: &mut EffectiveConfig,
    sources: Option<&SettingsSources>,
    a: &Admission,
    request: &OwnerRequest,
) -> Result<OwnerApplied, EngineError> {
    request.validate()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, a)?;
    let revision = owner_revision(&tx)?;
    let hash = request.mutation_hash()?;
    if let Some((provenance, result)) = owner_lookup(
        &tx,
        "native_owner",
        &a.owner_id,
        &request.request_id,
        Some(&hash),
    )? {
        return Ok(OwnerApplied {
            result: serde_json::from_slice(&result).map_err(error)?,
            provenance,
            historical_replay: true,
            current_revision: Cursor::new(revision),
        });
    }
    if pending(&tx)? {
        return Err(error("settings_publication_pending"));
    }
    let OwnerOperation::Apply {
        mutation:
            OwnerMutation::ModelAccess {
                expected_config_id,
                expected_source_hash,
                edit,
            },
    } = &request.operation
    else {
        return Err(error("owner_mutation_required"));
    };
    let prepared = (|| {
        let sources = sources.ok_or_else(|| error("settings_read_only"))?;
        if request.expected_revision.value() != revision {
            return Err(error("stale_revision"));
        }
        if expected_config_id != &config.id {
            return Err(error("stale_config"));
        }
        let old = sources.snapshot()?;
        if old.hash()? != *expected_source_hash || old.config()?.id != config.id {
            return Err(error("settings_source_conflict"));
        }
        let registered = super::accounts::pool_materialization(&tx, config, edit)?;
        let (new_global, next) =
            edit_model_access(&old.global.bytes, &old.project.bytes, edit, &registered)
                .map_err(|e| error(e.0))?;
        if revision == i64::MAX as u64 {
            return Err(error("revision_exhausted"));
        }
        Ok((
            sources,
            Pending {
                request: request.clone(),
                admission: a.clone(),
                revision,
                old,
                new_global,
                temporary: format!(".crazyagent-publication-{hash}"),
                new_identity: None,
            },
            next,
        ))
    })();
    match prepared {
        Err(e) => {
            drop(tx);
            reject(db, a, request, e.0)
        }
        Ok((sources, mut pending, next)) => {
            let body = serde_json::to_vec(&pending).map_err(error)?;
            if body.len() > SETTINGS_JOURNAL_MAX {
                drop(tx);
                return reject(db, a, request, "settings_journal_bounds".into());
            }
            store_config(&tx, config)?;
            tx.execute("INSERT INTO settings_publication VALUES(1,?1)", [body])
                .map_err(error)?;
            tx.commit().map_err(error)?;
            sources.publish(db, &mut pending)?;
            let result = finalize(db, &pending, &next)?;
            *config = next;
            Ok(result)
        }
    }
}

fn reject(
    db: &mut Connection,
    a: &Admission,
    request: &OwnerRequest,
    code: String,
) -> Result<OwnerApplied, EngineError> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_admission(&tx, a)?;
    let revision = owner_revision(&tx)?;
    let result = OwnerResult::Rejected { code };
    let provenance = OwnerProvenance {
        origin: "native_owner".into(),
        owner_id: a.owner_id.clone(),
        operation_id: request.request_id.clone(),
        request_hash: request.mutation_hash()?,
        engine_epoch: a.engine_epoch.clone(),
        admitted_epoch: a.owner_epoch,
        resulting_epoch: a.owner_epoch,
        revision_before: Cursor::new(revision),
        revision_after: Cursor::new(revision),
        event_cursor: None,
    };
    owner_record(
        &tx,
        &provenance,
        &Sha256::digest(serde_json::to_vec(request).map_err(error)?),
        &serde_json::to_vec(&result).map_err(error)?,
    )?;
    tx.commit().map_err(error)?;
    Ok(OwnerApplied {
        result,
        provenance,
        historical_replay: false,
        current_revision: Cursor::new(revision),
    })
}

pub(super) fn verified_source_hash(
    config: &EffectiveConfig,
    sources: Option<&SettingsSources>,
) -> Result<String, EngineError> {
    match sources {
        Some(sources) => {
            let snapshot = sources.snapshot()?;
            if snapshot.config()?.id != config.id {
                return Err(error("settings_source_conflict"));
            }
            snapshot.hash()
        }
        None => Ok(config.id.as_str().to_owned()),
    }
}

pub(super) fn verified_source_hash_at(
    config: &EffectiveConfig,
    global: &Path,
    project: &Path,
) -> Result<String, EngineError> {
    let global = SourceFile::open(global)?;
    let project = SourceFile::open(project)?;
    if global.path.join(&global.name) == project.path.join(&project.name) {
        return Err(error("settings_sources_overlap"));
    }
    let source_id = crate::memory::hash(
        &serde_json::to_vec(&(
            global.path.join(&global.name),
            global.identity,
            project.path.join(&project.name),
            project.identity,
        ))
        .map_err(error)?,
    );
    let snapshot = SourceState {
        source_id,
        global: global.read()?,
        project: project.read()?,
    };
    if snapshot.config()?.id != config.id {
        return Err(error("settings_source_conflict"));
    }
    snapshot.hash()
}

pub(super) fn read(
    db: &mut Connection,
    config: &EffectiveConfig,
    sources: Option<&SettingsSources>,
    a: &Admission,
    query: &OwnerRead,
) -> Result<OwnerReadReply, EngineError> {
    let tx = db.transaction().map_err(error)?;
    check_owner_admission(&tx, a)?;
    if pending(&tx)? {
        return Err(error("settings_publication_pending"));
    }
    let revision = owner_revision(&tx)?;
    let source_hash = verified_source_hash(config, sources)?;
    let writable = sources.is_some();
    let (collection, record_id, cursor, limit) = match query {
        OwnerRead::ModelAccessPage {
            collection,
            cursor,
            limit,
        } => (*collection, None, cursor.as_ref(), *limit),
        OwnerRead::ModelAccessRecord { collection, id } => (*collection, Some(id), None, 1),
        _ => return Err(error("model_access_read_required")),
    };
    if !(1..=16).contains(&limit) {
        return Err(error("model_access_page_limit"));
    }
    if let Some(cursor) = cursor
        && (cursor.db_instance != a.db_instance
            || cursor.owner_id != a.owner_id
            || cursor.engine_epoch != a.engine_epoch
            || cursor.owner_epoch != a.owner_epoch
            || cursor.config_id != config.id
            || cursor.at_revision.value() != revision
            || cursor.source_hash != source_hash
            || cursor.collection != collection)
    {
        return Ok(OwnerReadReply {
            revision: Cursor::new(revision),
            config_id: config.id.clone(),
            data: OwnerData::ResyncRequired,
        });
    }
    let catalog = config.settings.model_access.as_ref();
    let mut records: Vec<ModelAccessRecord> = match (collection, catalog) {
        (ModelAccessCollection::Accounts, Some(c)) => c
            .accounts
            .iter()
            .cloned()
            .map(|account_ref| ModelAccessRecord::Account { account_ref })
            .collect(),
        (ModelAccessCollection::Connections, Some(c)) => c
            .connections
            .iter()
            .cloned()
            .map(|connection| ModelAccessRecord::Connection { connection })
            .collect(),
        (ModelAccessCollection::Pools, Some(c)) => c
            .pools
            .iter()
            .cloned()
            .map(|pool| ModelAccessRecord::Pool { pool })
            .collect(),
        (_, None) => Vec::new(),
    };
    assert!(records.len() <= 64);
    records.sort_by(|a, b| a.id().cmp(b.id()));
    let data = if let Some(id) = record_id {
        let record = records
            .into_iter()
            .find(|r| r.id() == id)
            .ok_or_else(|| error("model_access_record_not_found"))?;
        OwnerData::ModelAccessRecord {
            source_hash,
            writable,
            record,
        }
    } else {
        records.retain(|r| cursor.is_none_or(|c| r.id() > &c.after_id));
        let remaining = records.len();
        let mut items = Vec::new();
        for record in records.into_iter().take(usize::from(limit)) {
            items.push(record);
            if serde_json::to_vec(&items).map_err(error)?.len() > 30000 {
                items.pop();
                break;
            }
        }
        if items.is_empty() && remaining != 0 {
            return Err(error("model_access_record_bounds"));
        }
        let next = if items.len() < remaining {
            Some(ModelAccessCursor {
                db_instance: a.db_instance.clone(),
                owner_id: a.owner_id.clone(),
                engine_epoch: a.engine_epoch.clone(),
                owner_epoch: a.owner_epoch,
                config_id: config.id.clone(),
                at_revision: Cursor::new(revision),
                source_hash: source_hash.clone(),
                collection,
                after_id: items.last().unwrap().id().clone(),
            })
        } else {
            None
        };
        OwnerData::ModelAccessPage {
            collection,
            source_hash,
            writable,
            items,
            next,
        }
    };
    let reply = OwnerReadReply {
        revision: Cursor::new(revision),
        config_id: config.id.clone(),
        data,
    };
    if serde_json::to_vec(&reply).map_err(error)?.len() > 32768 {
        return Err(error("model_access_record_bounds"));
    }
    Ok(reply)
}

#[cfg(all(test, unix))]
mod source_access_tests;
