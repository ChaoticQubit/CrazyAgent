use super::*;
use crate::config::SETTINGS_VERSION;
use crate::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, OwnerData, OwnerMutation, OwnerOperation, OwnerRead,
    OwnerReadReply, OwnerRequest, OwnerResult, Started,
};
use crate::memory::library::{LibraryAction, MemoryScope};
use crate::protocol::{PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId};
use crate::storage::{Job, Storage};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio::sync::oneshot;

const GLOBAL: &str = r#"version = 1
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

const PROJECT: &str = "version = 1\nqueue_capacity = 3\n";

const NOTE: &str = "Keep this note across engine upgrades";

/// A private engine root with real settings files, like the engine's own data folder.
struct Root {
    dir: tempfile::TempDir,
}

impl Root {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ca-mig-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = Self { dir };
        for (path, text) in [(root.global(), GLOBAL), (root.project(), PROJECT)] {
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        root
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn database(&self) -> PathBuf {
        self.path().join("engine.db")
    }

    fn global(&self) -> PathBuf {
        self.path().join("global.toml")
    }

    fn project(&self) -> PathBuf {
        self.path().join("project.toml")
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database()).unwrap()
    }

    async fn open(&self) -> Result<Storage, EngineError> {
        Storage::open_with_settings_sources(&self.database(), &self.global(), &self.project())
            .await
            .map(|(storage, _)| storage)
    }

    /// The backup folders, oldest first; anything not named like a backup is left out.
    fn backups(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.path().join(BACKUPS)) else {
            return Vec::new();
        };
        let mut found: Vec<(String, PathBuf)> = entries
            .map(|entry| entry.unwrap().path())
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?.to_owned();
                Some((backup_time(&name)?.to_owned(), path))
            })
            .collect();
        found.sort();
        found.into_iter().map(|(_, path)| path).collect()
    }
}

async fn job<T>(
    storage: &Storage,
    build: impl FnOnce(oneshot::Sender<Result<T, EngineError>>) -> Job,
) -> T {
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(build(send))
        .unwrap_or_else(|_| panic!("storage queue unavailable"));
    receive.await.unwrap().unwrap()
}

async fn owner_read(storage: &Storage, started: &Started, query: OwnerRead) -> OwnerReadReply {
    let admission = started.owners[0].admission.clone();
    job(storage, |send| Job::OwnerRead(admission, query, send)).await
}

async fn apply(storage: &Storage, started: &Started, id: &str, mutation: OwnerMutation) {
    let basis = owner_read(storage, started, OwnerRead::Capabilities).await;
    let admission = started.owners[0].admission.clone();
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: basis.revision,
        operation: OwnerOperation::Apply { mutation },
    };
    let applied = job(storage, |send| Job::OwnerApply(admission, request, send)).await;
    assert_eq!(applied.result, OwnerResult::Applied, "{id}");
}

async fn close(storage: Storage) {
    job(&storage, Job::Close).await;
}

/// Builds the database a 0.1 engine leaves behind: a project, a memory and a pool
/// written through the real storage actor, then `PRAGMA user_version` back at the
/// unset 0, which 0.1 never changed.
async fn legacy(root: &Root) {
    let storage = root.open().await.unwrap();
    let started = job(&storage, Job::OwnerStart).await;
    apply(
        &storage,
        &started,
        "create-migrated-project",
        OwnerMutation::CreateProject {
            project_id: ProjectId::new("configured-project").unwrap(),
            spec: ProjectSpec {
                name: "Migrated project".into(),
                repositories: vec![],
            },
        },
    )
    .await;
    apply(
        &storage,
        &started,
        "remember-migrated-note",
        OwnerMutation::Memory {
            action: LibraryAction::Add {
                scope: MemoryScope::Global,
                text: NOTE.into(),
            },
        },
    )
    .await;
    close(storage).await;
    assert!(
        !root.path().join(BACKUPS).exists(),
        "a new database needs no backup"
    );
    let db = root.connection();
    assert_eq!(version(&db).unwrap(), latest(STEPS));
    db.execute_batch("PRAGMA user_version = 0").unwrap();
}

/// Every row of every table as text, sorted, so whole databases can be compared.
fn rows(db: &Connection) -> BTreeMap<String, Vec<String>> {
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let mut statement = db.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
            let columns = statement.column_count();
            let mut values: Vec<String> = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| {
                            row.get::<_, rusqlite::types::Value>(index)
                                .map(|value| format!("{value:?}"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(|cells| cells.join("|"))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            values.sort();
            (table, values)
        })
        .collect()
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().mode() & 0o777
}

fn refused<T>(result: Result<T, EngineError>) -> String {
    match result {
        Ok(_) => panic!("the open must be refused"),
        Err(cause) => cause.0,
    }
}

fn probe(tx: &Transaction<'_>) -> Result<(), EngineError> {
    tx.execute_batch(
        "CREATE TABLE migration_probe (step INTEGER PRIMARY KEY); \
         INSERT INTO migration_probe VALUES (3);",
    )
    .map_err(error)
}

fn broken(tx: &Transaction<'_>) -> Result<(), EngineError> {
    tx.execute_batch("CREATE TABLE migration_broken (")
        .map_err(error)
}

fn escaping(tx: &Transaction<'_>) -> Result<(), EngineError> {
    tx.execute_batch("COMMIT; DELETE FROM projects; CREATE TABLE migration_escaped (a);")
        .map_err(error)
}

/// The production steps followed by test-only steps numbered after them.
fn steps(extra: &[StepRun]) -> Vec<Step> {
    let next = latest(STEPS) + 1;
    STEPS
        .iter()
        .map(|step| Step {
            to: step.to,
            run: step.run,
        })
        .chain((next..).zip(extra).map(|(to, run)| Step { to, run: *run }))
        .collect()
}

#[tokio::test]
async fn v1_database_upgrades_in_place_and_keeps_every_row() {
    let root = Root::new();
    legacy(&root).await;
    let before = rows(&root.connection());
    assert!(
        before["projects"]
            .iter()
            .any(|row| row.contains("configured-project"))
    );
    assert!(
        before["memory_library"]
            .iter()
            .any(|row| row.contains(NOTE))
    );
    let settings = (
        std::fs::read(root.global()).unwrap(),
        std::fs::read(root.project()).unwrap(),
    );

    close(root.open().await.unwrap()).await;

    let db = root.connection();
    assert_eq!(version(&db).unwrap(), latest(STEPS));
    assert_eq!(rows(&db), before);
    drop(db);
    let backups = root.backups();
    assert_eq!(backups.len(), 1);
    let backup = &backups[0];
    let name = backup.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with(&format!("1-to-{}-", latest(STEPS))),
        "{name}"
    );
    assert_eq!(mode(&root.path().join(BACKUPS)), 0o700);
    assert_eq!(mode(backup), 0o700);
    for file in [DATABASE, "global.toml", "project.toml"] {
        assert_eq!(mode(&backup.join(file)), 0o600, "{file}");
    }
    assert_eq!(
        std::fs::read(backup.join("global.toml")).unwrap(),
        settings.0
    );
    assert_eq!(
        std::fs::read(backup.join("project.toml")).unwrap(),
        settings.1
    );
    let copy = Connection::open(backup.join(DATABASE)).unwrap();
    assert_eq!(version(&copy).unwrap(), 0);
    assert_eq!(rows(&copy), before);
    drop(copy);

    let storage = root.open().await.unwrap();
    let started = job(&storage, Job::OwnerStart).await;
    let pools = owner_read(
        &storage,
        &started,
        OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools,
            cursor: None,
            limit: 10,
        },
    )
    .await;
    let OwnerData::ModelAccessPage { items, .. } = pools.data else {
        panic!("the pool page is missing after the upgrade");
    };
    assert!(items.iter().any(|item| matches!(
        item,
        ModelAccessRecord::Pool { pool } if pool.id.as_str() == "configured-pool"
    )));
    let memories = owner_read(
        &storage,
        &started,
        OwnerRead::Memories {
            scope: MemoryScope::Global,
            search: None,
            before: None,
            limit: 10,
        },
    )
    .await;
    let OwnerData::Memories { page } = memories.data else {
        panic!("the memory page is missing after the upgrade");
    };
    assert!(page.items.iter().any(|item| item.text == NOTE));
    close(storage).await;
    assert_eq!(
        root.backups().len(),
        1,
        "a database at the newest version is not backed up again"
    );
}

#[tokio::test]
async fn newer_database_is_refused_and_left_byte_for_byte() {
    let root = Root::new();
    legacy(&root).await;
    let newer = latest(STEPS) + 1;
    root.connection()
        .execute_batch(&format!("PRAGMA user_version = {newer}"))
        .unwrap();
    let bytes = std::fs::read(root.database()).unwrap();

    let writable = refused(root.open().await);
    assert!(
        writable.starts_with("storage_newer_than_engine:"),
        "{writable}"
    );
    assert!(writable.contains(&format!("version {newer}")), "{writable}");
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);

    let config = crate::config::resolve_config(&root.global(), &root.project()).unwrap();
    let readonly = refused(Storage::open(&root.database(), config, true).await);
    assert!(
        readonly.starts_with("storage_newer_than_engine:"),
        "{readonly}"
    );
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);
    assert!(!root.path().join(BACKUPS).exists());
    assert_eq!(version(&root.connection()).unwrap(), newer);
}

#[tokio::test]
async fn readonly_open_refuses_a_database_that_needs_an_upgrade() {
    let root = Root::new();
    legacy(&root).await;
    let bytes = std::fs::read(root.database()).unwrap();
    let config = crate::config::resolve_config(&root.global(), &root.project()).unwrap();

    let readonly = refused(Storage::open(&root.database(), config, true).await);

    assert!(
        readonly.starts_with("storage_upgrade_required:"),
        "{readonly}"
    );
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);
    assert!(!root.path().join(BACKUPS).exists());
}

#[tokio::test]
async fn every_pending_step_runs_in_order_from_any_older_version() {
    let root = Root::new();
    legacy(&root).await;
    let before = rows(&root.connection());
    let steps = steps(&[probe]);
    let sources = SettingsSources::open(&root.global(), &root.project()).unwrap();
    let mut db = root.connection();
    let probed = |db: &Connection| -> Vec<i64> {
        db.prepare("SELECT step FROM migration_probe")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };

    upgrade(&mut db, &root.database(), Some(&sources), &steps).unwrap();

    assert_eq!(version(&db).unwrap(), 3);
    assert_eq!(probed(&db), vec![3]);
    let mut after = rows(&db);
    assert_eq!(after.remove("migration_probe").unwrap().len(), 1);
    assert_eq!(after, before);
    let backups = root.backups();
    assert_eq!(backups.len(), 1);
    let first = backups[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(first.starts_with("1-to-3-"), "{first}");
    assert_eq!(
        std::fs::read(backups[0].join("global.toml")).unwrap(),
        std::fs::read(root.global()).unwrap()
    );

    upgrade(&mut db, &root.database(), Some(&sources), &steps).unwrap();
    assert_eq!(root.backups().len(), 1, "nothing is pending, so no backup");

    db.execute_batch("DROP TABLE migration_probe; PRAGMA user_version = 2")
        .unwrap();
    upgrade(&mut db, &root.database(), Some(&sources), &steps).unwrap();
    assert_eq!(version(&db).unwrap(), 3);
    assert_eq!(probed(&db), vec![3]);
    let names: Vec<String> = root
        .backups()
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&first));
    assert!(
        names.iter().any(|name| name.starts_with("2-to-3-")),
        "{names:?}"
    );
}

#[tokio::test]
async fn failing_step_rolls_back_and_names_the_backup() {
    let root = Root::new();
    legacy(&root).await;
    let before = rows(&root.connection());
    let sources = SettingsSources::open(&root.global(), &root.project()).unwrap();
    let mut db = root.connection();

    let failed = upgrade(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe, broken]),
    )
    .unwrap_err()
    .0;

    let backups = root.backups();
    assert_eq!(backups.len(), 1);
    assert!(
        failed.starts_with("storage_migration_failed: step 4 failed:"),
        "{failed}"
    );
    assert!(failed.contains("the database is unchanged"), "{failed}");
    assert!(
        failed.ends_with(&format!("backup at {}", backups[0].display())),
        "{failed}"
    );
    assert_eq!(version(&db).unwrap(), 0);
    assert_eq!(rows(&db), before);
    let copy = Connection::open(backups[0].join(DATABASE)).unwrap();
    assert_eq!(version(&copy).unwrap(), 0);
    assert_eq!(rows(&copy), before);
    drop(copy);

    upgrade(&mut db, &root.database(), Some(&sources), &steps(&[probe])).unwrap();
    assert_eq!(version(&db).unwrap(), 3);
}

#[tokio::test]
async fn step_that_ends_the_transaction_is_restored_from_the_backup() {
    let root = Root::new();
    legacy(&root).await;
    let before = rows(&root.connection());
    let sources = SettingsSources::open(&root.global(), &root.project()).unwrap();
    let mut db = root.connection();

    let failed = upgrade(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe, escaping]),
    )
    .unwrap_err()
    .0;

    let backups = root.backups();
    assert_eq!(backups.len(), 1);
    assert!(
        failed.starts_with(
            "storage_migration_failed: step 4 failed: the step ended the upgrade transaction;"
        ),
        "{failed}"
    );
    assert!(
        failed.contains("the database was restored from the backup"),
        "{failed}"
    );
    assert!(
        failed.ends_with(&format!("backup at {}", backups[0].display())),
        "{failed}"
    );
    assert_eq!(version(&db).unwrap(), 0);
    assert_eq!(rows(&db), before);
    drop(db);
    drop(sources);

    close(root.open().await.unwrap()).await;
    assert_eq!(version(&root.connection()).unwrap(), latest(STEPS));
    assert_eq!(rows(&root.connection()), before);
}

#[tokio::test]
async fn only_the_newest_five_backups_are_kept() {
    let root = Root::new();
    legacy(&root).await;
    let folder = root.path().join(BACKUPS);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    for day in 1..=6 {
        let old = folder.join(format!("1-to-2-2000010{day}T000000000Z"));
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join(DATABASE), b"old").unwrap();
    }
    std::fs::create_dir(folder.join("keep-me")).unwrap();
    std::fs::write(folder.join("1-to-2-notes.txt"), b"owner notes").unwrap();

    close(root.open().await.unwrap()).await;

    let names: Vec<String> = root
        .backups()
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), BACKUPS_KEPT, "{names:?}");
    assert_eq!(names[0], "1-to-2-20000103T000000000Z");
    assert!(names[BACKUPS_KEPT - 1].starts_with(&format!("1-to-{}-", latest(STEPS))));
    assert!(!folder.join("1-to-2-20000101T000000000Z").exists());
    assert!(!folder.join("1-to-2-20000102T000000000Z").exists());
    assert!(folder.join("keep-me").is_dir());
    assert_eq!(
        std::fs::read(folder.join("1-to-2-notes.txt")).unwrap(),
        b"owner notes"
    );
}

#[tokio::test]
async fn newer_settings_file_is_refused_before_the_database_opens() {
    let root = Root::new();
    std::fs::write(
        root.global(),
        format!("version = {}\n", SETTINGS_VERSION + 1),
    )
    .unwrap();

    let refused = refused(root.open().await);

    assert!(refused.contains("settings_newer_than_engine:"), "{refused}");
    assert!(!root.database().exists());
}

#[test]
#[should_panic(expected = "migration steps are numbered from 2 without gaps")]
fn steps_must_be_numbered_from_two_without_gaps() {
    latest(&[Step { to: 3, run: probe }]);
}

#[test]
fn backup_names_use_a_sortable_utc_stamp() {
    assert_eq!(stamp(UNIX_EPOCH).unwrap(), "19700101T000000000Z");
    assert_eq!(
        stamp(UNIX_EPOCH + Duration::from_secs(951_868_800)).unwrap(),
        "20000301T000000000Z"
    );
    assert_eq!(
        stamp(UNIX_EPOCH + Duration::from_millis(1_709_251_199_999)).unwrap(),
        "20240229T235959999Z"
    );
    assert!(stamp(UNIX_EPOCH + Duration::from_secs(253_402_300_800)).is_err());
    assert_eq!(
        backup_time("1-to-2-20240229T235959999Z"),
        Some("20240229T235959999Z")
    );
    for other in [
        "keep-me",
        "1-to-2-",
        "x-to-2-20240229T235959999Z",
        "1-to-2-20240229T235959999",
        "1-to-2-2024022XT235959999Z",
        "1-to-2-2024022\u{e9}T35959999Z",
    ] {
        assert_eq!(backup_time(other), None, "{other}");
    }
}
