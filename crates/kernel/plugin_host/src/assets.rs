use kernel_common::hash;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};

/// Where an asset applies: every project, or one project by its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    Global,
    Project(String),
}

/// What an asset file holds. `NativePlugin` is named but never admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Rules,
    Skill,
    ConfigurationText,
    NativePlugin,
}

/// One asset file offered for admission: its id, unique name within its scope, scope,
/// kind and absolute path. Its serde shape is the engine's stored and wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    pub kind: Kind,
    pub path: PathBuf,
}

/// An admitted source with the (device, inode) of the file it was admitted as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Admitted {
    source: Source,
    device: u64,
    inode: u64,
}

/// One asset as read into a snapshot: its source, the SHA-256 of its bytes and the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Content {
    pub source: Source,
    pub content_hash: String,
    pub bytes: Vec<u8>,
}

/// The assets one project and config saw at one time, with the hash over all three.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub id: String,
    pub project_id: String,
    pub config_id: String,
    pub content_hash: String,
    pub assets: Vec<Content>,
}

impl Snapshot {
    /// Asserts the snapshot's hashes and bounds hold and that it holds no native plugin.
    fn assert_consistent(&self) {
        assert_eq!(
            self.content_hash,
            snapshot_hash(&self.project_id, &self.config_id, &self.assets)
        );
        assert!(self.assets.len() <= ASSET_SOURCES_MAX);
        for asset in &self.assets {
            assert_eq!(asset.content_hash, hash(&asset.bytes));
            assert_ne!(asset.source.kind, Kind::NativePlugin);
        }
    }
}

/// Why an asset action did not apply. `Refused` is an answer the caller reports as a
/// refusal with its code; `Failed` is a store or encoding fault the caller raises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Refused(String),
    Failed(String),
}

/// A store or encoding fault as `Failed`. Input: the fault. Output: the failure.
fn failed(value: impl std::fmt::Display) -> Failure {
    Failure::Failed(value.to_string())
}

/// The most sources a project's snapshot selects, and so the most an admission may allow.
const ASSET_SOURCES_MAX: usize = 128;

/// Engine facts an admission needs: whether a project-scoped source's project exists
/// (unused for a global source), the byte limit per asset and the source count limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmitFacts {
    pub project_exists: bool,
    pub asset_bytes_max: usize,
    pub asset_sources_max: usize,
}

/// Engine facts a snapshot needs: the principal's project, the policy config id and its
/// limits, and the frame bound.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotFacts<'a> {
    pub project_id: &'a str,
    pub config_id: &'a str,
    pub asset_bytes_max: usize,
    pub records_max: usize,
    pub frame_bytes_max: usize,
}

/// Engine facts a read needs: the principal's project and its task's config id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFacts<'a> {
    pub project_id: &'a str,
    pub task_config_id: &'a str,
}

/// Creates the asset tables when absent. Input: the connection. Output: done, or the
/// database's refusal text.
pub fn initialize(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS asset_sources
        (id TEXT PRIMARY KEY, scope TEXT NOT NULL, name TEXT NOT NULL, body TEXT NOT NULL,
        revoked INTEGER NOT NULL DEFAULT 0, UNIQUE(scope,name));
        CREATE TABLE IF NOT EXISTS asset_snapshots (id TEXT PRIMARY KEY, body TEXT NOT NULL);",
        )
        .map_err(|e| e.to_string())
}

/// Fails with `knowledge_record_capacity` when a table already holds its maximum rows.
/// Inputs: the transaction, a fixed table name and the maximum. Output: done or failure.
fn below(tx: &Transaction<'_>, table: &str, maximum: usize) -> Result<(), Failure> {
    let count: i64 = tx
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .map_err(failed)?;
    if count >= maximum as i64 {
        return Err(failed("knowledge_record_capacity"));
    }
    assert!(count < maximum as i64);
    assert!(!tx.is_autocommit());
    Ok(())
}

/// Marks an admitted source revoked. Inputs: the transaction and the source id. Output:
/// done, `asset_source_missing` when no live source has the id, or the store's failure.
pub fn revoke(tx: &Transaction<'_>, source_id: &str) -> Result<(), Failure> {
    let count = tx
        .execute(
            "UPDATE asset_sources SET revoked=1 WHERE id=?1 AND revoked=0",
            [source_id],
        )
        .map_err(failed)?;
    if count == 0 {
        return Err(Failure::Refused("asset_source_missing".into()));
    }
    assert_eq!(count, 1);
    assert!(!tx.is_autocommit());
    Ok(())
}

/// Refuses to open an asset file where the Unix no-follow walk from `/` does not exist.
/// Takes the path, unused. Returns `asset_source_unavailable`.
#[cfg(not(unix))]
fn open_regular(_path: &Path) -> Result<std::fs::File, String> {
    Err("asset_source_unavailable".into())
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

/// Opens an asset file by a no-follow walk from `/`, one component at a time, and
/// accepts only a regular file with one link owned by this user. Input: the absolute
/// path. Output: the open file, or the refusal text.
#[cfg(unix)]
fn open_regular(path: &Path) -> Result<std::fs::File, String> {
    use rustix::fs::{Mode, OFlags, open, openat};
    if !path.is_absolute() || path.as_os_str().len() > 4096 {
        return Err("asset_path_denied".into());
    }
    let mut components = path.components().peekable();
    if components.next() != Some(Component::RootDir) {
        return Err("asset_path_denied".into());
    }
    let mut descriptor = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?;
    let mut depth = 0;
    while let Some(component) = components.next() {
        depth += 1;
        if depth > 128 {
            return Err("asset_path_denied".into());
        }
        let Component::Normal(name) = component else {
            return Err("asset_path_denied".into());
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if components.peek().is_some() {
            flags |= OFlags::DIRECTORY;
        }
        descriptor = openat(&descriptor, name, flags, Mode::empty())
            .map_err(|_| String::from("asset_path_denied"))?;
    }
    let file = std::fs::File::from(descriptor);
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::getuid().as_raw()
    {
        return Err("asset_path_denied".into());
    }
    assert!(metadata.is_file());
    assert!(depth <= 128);
    Ok(file)
}

/// Admits an asset file: checks its name, kind, project and size, pins the (device,
/// inode) of the file it opened and stores the source. Inputs: the transaction, the
/// engine facts and the source. Output: done, the refusal, or the store's failure.
pub fn admit(tx: &Transaction<'_>, facts: &AdmitFacts, source: &Source) -> Result<(), Failure> {
    assert!(
        facts.asset_sources_max <= ASSET_SOURCES_MAX,
        "asset sources stay within the selection limit"
    );
    if source.name.is_empty()
        || source.name.len() > 128
        || !source
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(Failure::Refused("invalid_asset_name".into()));
    }
    if source.kind == Kind::NativePlugin {
        return Err(Failure::Refused("native_plugin_unavailable".into()));
    }
    if matches!(source.scope, Scope::Project(_)) && !facts.project_exists {
        return Err(Failure::Refused("project_missing".into()));
    }
    let file = open_regular(&source.path).map_err(Failure::Refused)?;
    let metadata = file.metadata().map_err(failed)?;
    if metadata.len() > facts.asset_bytes_max as u64 {
        return Err(Failure::Refused("asset_bytes_capacity".into()));
    }
    below(tx, "asset_sources", facts.asset_sources_max)?;
    let (device, inode) = file_identity(&metadata);
    let admitted = Admitted {
        source: source.clone(),
        device,
        inode,
    };
    tx.execute(
        "INSERT INTO asset_sources(id,scope,name,body) VALUES (?1,?2,?3,?4)",
        params![
            source.id,
            serde_json::to_string(&source.scope).map_err(failed)?,
            source.name,
            serde_json::to_string(&admitted).map_err(failed)?
        ],
    )
    .map_err(failed)?;
    assert!(metadata.is_file());
    assert!(!tx.is_autocommit());
    Ok(())
}

/// The SHA-256 over the serde text of `(project, config, assets)`, the engine's snapshot
/// hash. Inputs: the ids and the assets. Output: 64 lowercase hex digits.
fn snapshot_hash(project: &str, config: &str, assets: &[Content]) -> String {
    hash(&serde_json::to_vec(&(project, config, assets)).unwrap())
}

/// The live sources a project sees, one per name, a project source over a global one of
/// the same name, in name order. Inputs: the transaction and the project. Output: the
/// sources, `asset_source_revoked` when a selected one is revoked, or the store's text.
fn selected_sources(tx: &Transaction<'_>, project: &str) -> Result<Vec<Admitted>, String> {
    let error = |e: rusqlite::Error| e.to_string();
    let mut statement = tx
        .prepare(
            "SELECT body,revoked FROM asset_sources WHERE scope IN (?1,?2) ORDER BY id LIMIT ?3",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![
                serde_json::to_string(&Scope::Global).map_err(|e| e.to_string())?,
                serde_json::to_string(&Scope::Project(project.to_owned()))
                    .map_err(|e| e.to_string())?,
                ASSET_SOURCES_MAX as i64
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
        )
        .map_err(error)?;
    let mut selected: BTreeMap<String, (Admitted, bool)> = BTreeMap::new();
    for row in rows {
        let (body, revoked) = row.map_err(error)?;
        let source: Admitted = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        if matches!(source.source.scope, Scope::Project(_))
            || !selected.contains_key(&source.source.name)
        {
            selected.insert(source.source.name.clone(), (source, revoked));
        }
    }
    let mut sources = Vec::new();
    for (source, revoked) in selected.into_values() {
        if revoked {
            return Err("asset_source_revoked".into());
        }
        sources.push(source);
    }
    assert!(sources.len() <= ASSET_SOURCES_MAX);
    assert!(
        sources.iter().all(|s| s.source.scope == Scope::Global
            || s.source.scope == Scope::Project(project.to_owned()))
    );
    Ok(sources)
}

/// Refuses to read an asset where its Unix change times cannot be checked. Takes the
/// admitted source, the opened file, its metadata and the limit, all unused. Returns
/// `asset_source_unavailable`.
#[cfg(not(unix))]
fn content(
    _source: &Admitted,
    _file: &std::fs::File,
    _before: &std::fs::Metadata,
    _maximum: usize,
) -> Result<Content, String> {
    Err("asset_source_unavailable".into())
}

/// Reads an admitted asset file already opened for a snapshot, whose identity the
/// snapshot has checked, refusing a file that changed while it was read. Takes the
/// admitted source, the opened file, its metadata taken before the read and the byte
/// limit. Returns its content, `asset_bytes_capacity`, or the read or change refusal.
#[cfg(unix)]
fn content(
    source: &Admitted,
    file: &std::fs::File,
    before: &std::fs::Metadata,
    maximum: usize,
) -> Result<Content, String> {
    if before.len() > maximum as u64 {
        return Err("asset_bytes_capacity".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let after = file.metadata().map_err(|e| e.to_string())?;
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
        return Err("asset_changed_during_snapshot".into());
    }
    assert!(bytes.len() <= maximum);
    assert_eq!(before.ino(), after.ino());
    Ok(Content {
        source: source.source.clone(),
        content_hash: hash(&bytes),
        bytes,
    })
}

/// Opens every selected source and refuses the set when any was `replaced`. Input: the
/// sources. Output: each source's open file and its metadata, `asset_source_replaced`, or
/// the open refusal.
fn opened(sources: &[Admitted]) -> Result<Vec<(std::fs::File, std::fs::Metadata)>, String> {
    let mut files = Vec::with_capacity(sources.len());
    for source in sources {
        let file = open_regular(&source.source.path)?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        files.push((file, metadata));
    }
    let live: Vec<(u64, u64)> = files.iter().map(|(_, m)| file_identity(m)).collect();
    if replaced(sources, &live) {
        return Err("asset_source_replaced".into());
    }
    assert_eq!(files.len(), sources.len());
    assert!(files.len() <= ASSET_SOURCES_MAX);
    Ok(files)
}

/// Checks every source's pinned identity against its live one in one call to the file
/// identity rule, so one volume renumbered at mount is accepted while files spread over
/// other volumes are not. Inputs: the sources and their live (device, inode) pairs, in the
/// same order. Output: true when any source no longer names its pinned file.
fn replaced(sources: &[Admitted], live: &[(u64, u64)]) -> bool {
    let pinned: Vec<(u64, u64)> = sources.iter().map(|s| (s.device, s.inode)).collect();
    !kernel_common::file_identity::same_files(&pinned, live)
}

/// Takes a snapshot of the assets a project sees under a config and stores it. Inputs:
/// the transaction, the engine facts and the snapshot id. Output: done, the refusal, or
/// the store's failure.
pub fn snapshot(
    tx: &Transaction<'_>,
    facts: &SnapshotFacts<'_>,
    snapshot_id: &str,
) -> Result<(), Failure> {
    assert!(
        facts.frame_bytes_max > 2048,
        "a frame holds a snapshot's envelope"
    );
    let sources = selected_sources(tx, facts.project_id).map_err(Failure::Refused)?;
    let files = opened(&sources).map_err(Failure::Refused)?;
    let mut assets = Vec::with_capacity(sources.len());
    for (source, (file, before)) in sources.iter().zip(&files) {
        assets
            .push(content(source, file, before, facts.asset_bytes_max).map_err(Failure::Refused)?);
    }
    let snapshot = Snapshot {
        id: snapshot_id.to_owned(),
        project_id: facts.project_id.to_owned(),
        config_id: facts.config_id.to_owned(),
        content_hash: snapshot_hash(facts.project_id, facts.config_id, &assets),
        assets,
    };
    let body = serde_json::to_string(&snapshot).map_err(failed)?;
    if body.len() > facts.frame_bytes_max - 2048 {
        return Err(Failure::Refused("asset_snapshot_capacity".into()));
    }
    below(tx, "asset_snapshots", facts.records_max)?;
    snapshot.assert_consistent();
    tx.execute(
        "INSERT INTO asset_snapshots VALUES (?1,?2)",
        params![snapshot_id, body],
    )
    .map_err(failed)?;
    assert!(!tx.is_autocommit());
    Ok(())
}

/// Reads a stored snapshot for a principal whose project and task config took it, while
/// every source in it is still admitted. Inputs: the transaction, the engine facts and the
/// snapshot id. Output: the snapshot, the refusal, or the store's failure.
pub fn read(
    tx: &Transaction<'_>,
    facts: &ReadFacts<'_>,
    snapshot_id: &str,
) -> Result<Snapshot, Failure> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM asset_snapshots WHERE id=?1",
            [snapshot_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(failed)?;
    let Some(body) = body else {
        return Err(Failure::Refused("asset_snapshot_missing".into()));
    };
    let snapshot: Snapshot = serde_json::from_str(&body).map_err(failed)?;
    if snapshot.project_id != facts.project_id || snapshot.config_id != facts.task_config_id {
        return Err(Failure::Refused("asset_scope_denied".into()));
    }
    for asset in &snapshot.assets {
        let live: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM asset_sources WHERE id=?1 AND revoked=0)",
                [asset.source.id.as_str()],
                |r| r.get(0),
            )
            .map_err(failed)?;
        if !live {
            return Err(Failure::Refused("asset_source_revoked".into()));
        }
    }
    snapshot.assert_consistent();
    assert_eq!(snapshot.id, snapshot_id);
    Ok(snapshot)
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod tests;
