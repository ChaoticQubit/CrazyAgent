use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::memory::{capacity, error, hash, principal, reject};
use crate::protocol::*;
#[cfg(unix)]
use rusqlite::Connection;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetScope {
    Global,
    Project(ProjectId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Rules,
    Skill,
    ConfigurationText,
    NativePlugin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSource {
    pub id: RequestId,
    pub name: String,
    pub scope: AssetScope,
    pub kind: AssetKind,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmittedSource {
    source: AssetSource,
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetContent {
    pub source: AssetSource,
    pub content_hash: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSnapshot {
    pub id: SnapshotId,
    pub project_id: ProjectId,
    pub config_id: ConfigId,
    pub content_hash: String,
    pub assets: Vec<AssetContent>,
}

impl AssetSnapshot {
    fn assert_consistent(&self) {
        assert_eq!(
            self.content_hash,
            snapshot_hash(&self.project_id, &self.config_id, &self.assets)
        );
        assert!(self.assets.len() <= 128);
        for asset in &self.assets {
            assert_eq!(asset.content_hash, hash(&asset.bytes));
            assert_ne!(asset.source.kind, AssetKind::NativePlugin);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum AssetAction {
    Admit {
        source: AssetSource,
    },
    Revoke {
        source_id: RequestId,
    },
    Snapshot {
        principal_id: RequestId,
        snapshot_id: SnapshotId,
    },
    Read {
        principal_id: RequestId,
        snapshot_id: SnapshotId,
    },
}

#[cfg(unix)]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS asset_sources
        (id TEXT PRIMARY KEY, scope TEXT NOT NULL, name TEXT NOT NULL, body TEXT NOT NULL,
        revoked INTEGER NOT NULL DEFAULT 0, UNIQUE(scope,name));
        CREATE TABLE IF NOT EXISTS asset_snapshots (id TEXT PRIMARY KEY, body TEXT NOT NULL);",
        )
        .map_err(error)
}

pub(crate) fn is_mutation(action: &AssetAction) -> bool {
    !matches!(action, AssetAction::Read { .. })
}

pub(crate) fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    action: &AssetAction,
) -> Result<Outcome, EngineError> {
    match action {
        AssetAction::Admit { source } => admit(tx, config, source),
        AssetAction::Revoke { source_id } => {
            let count = tx
                .execute(
                    "UPDATE asset_sources SET revoked=1 WHERE id=?1 AND revoked=0",
                    [source_id.as_str()],
                )
                .map_err(error)?;
            if count == 0 {
                return reject("asset_source_missing");
            }
            assert_eq!(count, 1);
            assert!(!tx.is_autocommit());
            Ok(Outcome::Applied)
        }
        AssetAction::Snapshot {
            principal_id,
            snapshot_id,
        } => snapshot(tx, config, principal_id, snapshot_id),
        AssetAction::Read {
            principal_id,
            snapshot_id,
        } => read(tx, principal_id, snapshot_id),
    }
}

/// Refuses to open an asset file where the Unix no-follow walk from `/` does not exist.
/// Takes the path, unused. Returns `asset_source_unavailable`.
#[cfg(not(unix))]
fn open_regular(_path: &Path) -> Result<std::fs::File, EngineError> {
    Err(error("asset_source_unavailable"))
}

/// The device and inode that pin an admitted asset file, so a later read detects a
/// replaced file. Takes its metadata. Returns the pair.
#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

/// Reports no file identity where the platform has none; `open_regular` refuses there
/// before any identity is recorded. Takes the metadata, unused. Returns zeros.
#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

#[cfg(unix)]
fn open_regular(path: &Path) -> Result<std::fs::File, EngineError> {
    use rustix::fs::{Mode, OFlags, open, openat};
    if !path.is_absolute() || path.as_os_str().len() > 4096 {
        return Err(error("asset_path_denied"));
    }
    let mut components = path.components().peekable();
    if components.next() != Some(Component::RootDir) {
        return Err(error("asset_path_denied"));
    }
    let mut descriptor = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(error)?;
    let mut depth = 0;
    while let Some(component) = components.next() {
        depth += 1;
        if depth > 128 {
            return Err(error("asset_path_denied"));
        }
        let Component::Normal(name) = component else {
            return Err(error("asset_path_denied"));
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if components.peek().is_some() {
            flags |= OFlags::DIRECTORY;
        }
        descriptor = openat(&descriptor, name, flags, Mode::empty())
            .map_err(|_| error("asset_path_denied"))?;
    }
    let file = std::fs::File::from(descriptor);
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::getuid().as_raw()
    {
        return Err(error("asset_path_denied"));
    }
    assert!(metadata.is_file());
    assert!(depth <= 128);
    Ok(file)
}

fn admit(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    source: &AssetSource,
) -> Result<Outcome, EngineError> {
    if source.name.is_empty()
        || source.name.len() > 128
        || !source
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return reject("invalid_asset_name");
    }
    if source.kind == AssetKind::NativePlugin {
        return reject("native_plugin_unavailable");
    }
    if let AssetScope::Project(project) = &source.scope {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                [project.as_str()],
                |r| r.get(0),
            )
            .map_err(error)?;
        if !exists {
            return reject("project_missing");
        }
    }
    let file = match open_regular(&source.path) {
        Ok(f) => f,
        Err(e) => return reject(&e.0),
    };
    let metadata = file.metadata().map_err(error)?;
    if metadata.len() > config.settings.knowledge.asset_bytes_max as u64 {
        return reject("asset_bytes_capacity");
    }
    capacity(
        tx,
        "asset_sources",
        config.settings.knowledge.asset_sources_max,
    )?;
    let (device, inode) = file_identity(&metadata);
    let admitted = AdmittedSource {
        source: source.clone(),
        device,
        inode,
    };
    tx.execute(
        "INSERT INTO asset_sources(id,scope,name,body) VALUES (?1,?2,?3,?4)",
        params![
            source.id.as_str(),
            serde_json::to_string(&source.scope).map_err(error)?,
            source.name,
            serde_json::to_string(&admitted).map_err(error)?
        ],
    )
    .map_err(error)?;
    assert!(metadata.is_file());
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn snapshot_hash(project: &ProjectId, config: &ConfigId, assets: &[AssetContent]) -> String {
    hash(&serde_json::to_vec(&(project, config, assets)).unwrap())
}

fn selected_sources(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<Vec<AdmittedSource>, EngineError> {
    let mut statement = tx
        .prepare(
            "SELECT body,revoked FROM asset_sources WHERE scope IN (?1,?2) ORDER BY id LIMIT 128",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![
                serde_json::to_string(&AssetScope::Global).map_err(error)?,
                serde_json::to_string(&AssetScope::Project(project.clone())).map_err(error)?
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
        )
        .map_err(error)?;
    let mut selected: BTreeMap<String, (AdmittedSource, bool)> = BTreeMap::new();
    for row in rows {
        let (body, revoked) = row.map_err(error)?;
        let source: AdmittedSource = serde_json::from_str(&body).map_err(error)?;
        if matches!(source.source.scope, AssetScope::Project(_))
            || !selected.contains_key(&source.source.name)
        {
            selected.insert(source.source.name.clone(), (source, revoked));
        }
    }
    let mut sources = Vec::new();
    for (source, revoked) in selected.into_values() {
        if revoked {
            return Err(error("asset_source_revoked"));
        }
        sources.push(source);
    }
    assert!(sources.len() <= 128);
    assert!(sources.iter().all(|s| s.source.scope == AssetScope::Global
        || s.source.scope == AssetScope::Project(project.clone())));
    Ok(sources)
}

/// Refuses to read an asset where its Unix identity and change times cannot be checked.
/// Takes the admitted source and limit, both unused. Returns `asset_source_unavailable`.
#[cfg(not(unix))]
fn content(_source: &AdmittedSource, _maximum: usize) -> Result<AssetContent, EngineError> {
    Err(error("asset_source_unavailable"))
}

#[cfg(unix)]
fn content(source: &AdmittedSource, maximum: usize) -> Result<AssetContent, EngineError> {
    let file = open_regular(&source.source.path)?;
    let before = file.metadata().map_err(error)?;
    if before.ino() != source.inode || before.dev() != source.device {
        return Err(error("asset_source_replaced"));
    }
    if before.len() > maximum as u64 {
        return Err(error("asset_bytes_capacity"));
    }
    let mut bytes = Vec::new();
    (&file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    let after = file.metadata().map_err(error)?;
    if before.len() != after.len()
        || after.len() != bytes.len() as u64
        || bytes.len() > maximum
        || (
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        )
    {
        return Err(error("asset_changed_during_snapshot"));
    }
    assert!(bytes.len() <= maximum);
    assert_eq!(before.ino(), after.ino());
    Ok(AssetContent {
        source: source.source.clone(),
        content_hash: hash(&bytes),
        bytes,
    })
}

fn snapshot(
    tx: &Transaction<'_>,
    current: &EffectiveConfig,
    principal_id: &RequestId,
    snapshot_id: &SnapshotId,
) -> Result<Outcome, EngineError> {
    let p = match principal(tx, principal_id) {
        Ok(p) => p,
        Err(e) => return reject(&e.0),
    };
    let config = crate::memory::policy(tx, &p, current)?;
    let sources = match selected_sources(tx, &p.project_id) {
        Ok(s) => s,
        Err(e) => return reject(&e.0),
    };
    let mut assets = Vec::new();
    for source in sources {
        let asset = match content(&source, config.settings.knowledge.asset_bytes_max) {
            Ok(a) => a,
            Err(e) => return reject(&e.0),
        };
        assets.push(asset);
    }
    let snapshot = AssetSnapshot {
        id: snapshot_id.clone(),
        project_id: p.project_id.clone(),
        config_id: config.id.clone(),
        content_hash: snapshot_hash(&p.project_id, &config.id, &assets),
        assets,
    };
    let body = serde_json::to_string(&snapshot).map_err(error)?;
    if body.len() > FRAME_BYTES_MAX - 2048 {
        return reject("asset_snapshot_capacity");
    }
    capacity(tx, "asset_snapshots", config.settings.knowledge.records_max)?;
    snapshot.assert_consistent();
    tx.execute(
        "INSERT INTO asset_snapshots VALUES (?1,?2)",
        params![snapshot_id.as_str(), body],
    )
    .map_err(error)?;
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn read(
    tx: &Transaction<'_>,
    principal_id: &RequestId,
    snapshot_id: &SnapshotId,
) -> Result<Outcome, EngineError> {
    let p = match principal(tx, principal_id) {
        Ok(p) => p,
        Err(e) => return reject(&e.0),
    };
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM asset_snapshots WHERE id=?1",
            [snapshot_id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(body) = body else {
        return reject("asset_snapshot_missing");
    };
    let snapshot: AssetSnapshot = serde_json::from_str(&body).map_err(error)?;
    let context =
        crate::storage::task_context(tx, &p.task_id)?.ok_or_else(|| error("task_missing"))?;
    if snapshot.project_id != p.project_id || snapshot.config_id != context.config_id {
        return reject("asset_scope_denied");
    }
    for asset in &snapshot.assets {
        let live: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM asset_sources WHERE id=?1 AND revoked=0)",
                [asset.source.id.as_str()],
                |r| r.get(0),
            )
            .map_err(error)?;
        if !live {
            return reject("asset_source_revoked");
        }
    }
    snapshot.assert_consistent();
    assert_eq!(snapshot.id, *snapshot_id);
    Ok(Outcome::Assets {
        snapshot: Box::new(snapshot),
    })
}
