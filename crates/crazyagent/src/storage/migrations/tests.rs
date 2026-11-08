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

/// The rows a legacy database holds after an engine start: every row it had, plus the mark
/// that the start brought the fixture project's activity history in, which every engine
/// start does once per project (Task 10 backfill, 581da0a23). Input: the rows before the
/// start. Output: the rows the start must leave.
fn started(mut rows: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    let marks = rows.get_mut("project_activity_backfill").unwrap();
    assert!(marks.is_empty());
    marks.push(format!(
        "{:?}",
        rusqlite::types::Value::Text("configured-project".into())
    ));
    rows
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
    tx.execute_batch(&format!(
        "CREATE TABLE migration_probe (step INTEGER PRIMARY KEY); \
         INSERT INTO migration_probe VALUES ({});",
        latest(STEPS) + 1
    ))
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
    assert_eq!(rows(&db), started(before.clone()));
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

    let newest = latest(STEPS);
    let probe_step = newest + 1;
    upgrade(&mut db, &root.database(), Some(&sources), &steps).unwrap();

    assert_eq!(version(&db).unwrap(), probe_step);
    assert_eq!(probed(&db), vec![probe_step]);
    let mut after = rows(&db);
    assert_eq!(after.remove("migration_probe").unwrap().len(), 1);
    assert_eq!(after, before);
    let backups = root.backups();
    assert_eq!(backups.len(), 1);
    let first = backups[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(first.starts_with(&format!("1-to-{probe_step}-")), "{first}");
    assert_eq!(
        std::fs::read(backups[0].join("global.toml")).unwrap(),
        std::fs::read(root.global()).unwrap()
    );

    upgrade(&mut db, &root.database(), Some(&sources), &steps).unwrap();
    assert_eq!(root.backups().len(), 1, "nothing is pending, so no backup");

    db.execute_batch(&format!(
        "DROP TABLE migration_probe; PRAGMA user_version = {newest}"
    ))
    .unwrap();
    upgrade(&mut db, &root.database(), Some(&sources), &steps).unwrap();
    assert_eq!(version(&db).unwrap(), probe_step);
    assert_eq!(probed(&db), vec![probe_step]);
    let names: Vec<String> = root
        .backups()
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&first));
    assert!(
        names
            .iter()
            .any(|name| name.starts_with(&format!("{newest}-to-{probe_step}-"))),
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
        failed.starts_with(&format!(
            "storage_migration_failed: step {} failed:",
            latest(STEPS) + 2
        )),
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
    assert_eq!(version(&db).unwrap(), latest(STEPS) + 1);
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
        failed.starts_with(&format!(
            "storage_migration_failed: step {} failed: the step ended the upgrade transaction;",
            latest(STEPS) + 2
        )),
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
    assert_eq!(rows(&root.connection()), started(before));
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

/// A setup body as engines before DEC-096 addendum 3 saved it.
/// Takes the project, the sub-harness list as JSON and the team maximum. Returns the body.
fn setup_body(project: &str, sub: &str, max: u8) -> String {
    format!(
        r#"{{"project_id":"{project}","name":"P","about":"","tech_stack":[],"platforms":[],"research":[],"pool":null,"team":{{"master":"codex","master_tools":true,"sub_harnesses":{sub},"min_agents":1,"max_agents":{max}}},"documents":[],"folder":"/tmp/{project}","created_ms":1,"revision":3}}"#
    )
}

#[test]
fn an_old_empty_team_plan_of_one_agent_widens_once_to_the_team_maximum() {
    let root = Root::new();
    let mut db = root.connection();
    crate::project_setup::initialize(&db).unwrap();
    let chosen = r#"[{"harness":"codex","agents":1,"tools":true}]"#;
    for (project, body) in [
        ("old-solo", setup_body("old-solo", "[]", 1)),
        ("chosen", setup_body("chosen", chosen, 2)),
        ("lowered", setup_body("lowered", "[]", 3)),
        ("broken", "not json".to_owned()),
    ] {
        db.execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            [project, body.as_str()],
        )
        .unwrap();
    }
    db.execute_batch("PRAGMA user_version = 2").unwrap();
    let before = setups_or_text(&db);

    upgrade(&mut db, &root.database(), None, STEPS).unwrap();

    assert_eq!(version(&db).unwrap(), latest(STEPS));
    let after = setups_or_text(&db);
    let mut widened: serde_json::Value = serde_json::from_str(&before["old-solo"]).unwrap();
    widened["team"]["max_agents"] = crate::project_setup::TEAM_AGENTS_MAX.into();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&after["old-solo"]).unwrap(),
        widened
    );
    for kept in ["chosen", "lowered", "broken"] {
        assert_eq!(after[kept], before[kept], "{kept}");
    }
    let read: crate::project_setup::StoredSetup = serde_json::from_str(&after["old-solo"]).unwrap();
    assert!(read.team.validate().is_ok());

    db.execute(
        "UPDATE project_setups SET body=?1 WHERE project='old-solo'",
        [setup_body("old-solo", "[]", 1)],
    )
    .unwrap();
    upgrade(&mut db, &root.database(), None, STEPS).unwrap();
    let again: serde_json::Value = serde_json::from_str(&setups_or_text(&db)["old-solo"]).unwrap();
    assert_eq!(
        again["team"]["max_agents"], 1,
        "the owner's later choice of one agent survives the next engine start"
    );
}

/// Reads every saved setup body as stored text, by project.
fn setups_or_text(db: &Connection) -> BTreeMap<String, String> {
    db.prepare("SELECT project, body FROM project_setups")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn an_upgrade_sweeps_plugins_left_by_projects_that_are_gone() {
    let root = Root::new();
    let mut db = root.connection();
    db.execute_batch(
        "CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL);
         CREATE TABLE plugin_items (
             id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 64),
             kind TEXT NOT NULL CHECK(kind IN ('skill','server')),
             scope TEXT NOT NULL CHECK(length(scope) BETWEEN 1 AND 160),
             name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 64),
             body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 65536),
             UNIQUE(kind, scope, name));
         CREATE TABLE plugin_switches (
             project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
             item TEXT NOT NULL CHECK(length(item) BETWEEN 1 AND 64),
             PRIMARY KEY(project, item));
         CREATE TABLE file_trash (
             kind TEXT NOT NULL CHECK(kind IN ('account_key','mcp_secret','mcp_checks')),
             name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 128),
             PRIMARY KEY(kind, name)) WITHOUT ROWID;",
    )
    .unwrap();

    db.execute("INSERT INTO projects VALUES ('live','{}')", [])
        .unwrap();
    db.execute(
        "INSERT INTO file_trash VALUES ('mcp_secret','pending-secret')",
        [],
    )
    .unwrap();
    for row in [
        ["sk-ghost", "skill", "project:ghost", "notes", "{}"],
        ["sk-live", "skill", "project:live", "guide", "{}"],
    ] {
        db.execute("INSERT INTO plugin_items VALUES (?1,?2,?3,?4,?5)", row)
            .unwrap();
    }
    db.execute(
        "INSERT INTO plugin_switches VALUES ('ghost','sw-ghost')",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO plugin_switches VALUES ('live','sw-live')", [])
        .unwrap();

    let ghost_folder = root
        .path()
        .join("plugins")
        .join("skills")
        .join("project")
        .join("ghost");
    let live_folder = root
        .path()
        .join("plugins")
        .join("skills")
        .join("project")
        .join("live");
    std::fs::create_dir_all(&ghost_folder).unwrap();
    std::fs::create_dir_all(&live_folder).unwrap();

    db.execute_batch("PRAGMA user_version = 4").unwrap();

    upgrade(&mut db, &root.database(), None, STEPS).unwrap();

    assert_eq!(version(&db).unwrap(), latest(STEPS));

    let ghost_items: i64 = db
        .query_row(
            "SELECT count(*) FROM plugin_items WHERE scope='project:ghost'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ghost_items, 0, "the orphaned project's rows are swept");
    let ghost_switches: i64 = db
        .query_row(
            "SELECT count(*) FROM plugin_switches WHERE project='ghost'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ghost_switches, 0, "the orphaned switches are swept");

    let live_items: i64 = db
        .query_row(
            "SELECT count(*) FROM plugin_items WHERE scope='project:live'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live_items, 1, "a live project keeps its plugins");

    let kept: i64 = db
        .query_row(
            "SELECT count(*) FROM file_trash WHERE kind='mcp_secret' AND name='pending-secret'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        kept, 1,
        "step 5 rebuilds file_trash and keeps its pending notes"
    );
    let queued: i64 = db
        .query_row(
            "SELECT count(*) FROM file_trash WHERE kind='skill_folder' AND name='ghost'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        queued, 1,
        "the orphaned skill folder is queued for the sweep"
    );

    crate::storage::trash::sweep(&db, root.path()).unwrap();
    assert!(
        !ghost_folder.exists(),
        "the orphaned skill folder is reclaimed"
    );
    assert!(
        live_folder.exists(),
        "a live project keeps its skill folder"
    );
}

#[test]
fn an_upgrade_from_six_lets_items_hold_hooks_and_the_trash_hold_hook_folders_keeping_every_row() {
    let root = Root::new();
    let mut db = root.connection();
    db.execute_batch(
        "CREATE TABLE plugin_items (
             id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 64),
             kind TEXT NOT NULL CHECK(kind IN ('skill','server')),
             scope TEXT NOT NULL CHECK(length(scope) BETWEEN 1 AND 160),
             name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 64),
             body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 65536),
             UNIQUE(kind, scope, name));
         CREATE TABLE file_trash (
             kind TEXT NOT NULL CHECK(kind IN ('account_key','mcp_secret','mcp_checks','skill_folder')),
             name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 128),
             PRIMARY KEY(kind, name)) WITHOUT ROWID;",
    )
    .unwrap();
    for row in [
        ["sk-guide", "skill", "global", "guide", "{\"on\":true}"],
        ["sv-notes", "server", "project:live", "notes", "{}"],
    ] {
        db.execute("INSERT INTO plugin_items VALUES (?1,?2,?3,?4,?5)", row)
            .unwrap();
    }
    db.execute(
        "INSERT INTO file_trash VALUES ('skill_folder','pending-folder')",
        [],
    )
    .unwrap();
    let hook = "INSERT INTO plugin_items VALUES ('hk-start','hook','global','start','{}')";
    let hook_folder = "INSERT INTO file_trash VALUES ('hook_folder','hk-start')";
    assert!(db.execute(hook, []).is_err(), "version 6 refuses a hook");
    assert!(db.execute(hook_folder, []).is_err());
    let before = rows(&db);
    db.execute_batch("PRAGMA user_version = 6").unwrap();

    upgrade(&mut db, &root.database(), None, STEPS).unwrap();

    assert_eq!(version(&db).unwrap(), latest(STEPS));
    let after = rows(&db);
    for table in ["plugin_items", "file_trash"] {
        assert_eq!(after[table], before[table], "{table} keeps every row");
    }
    db.execute(hook, []).unwrap();
    db.execute(hook_folder, []).unwrap();
    let hooks: i64 = db
        .query_row(
            "SELECT count(*) FROM plugin_items WHERE kind='hook'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hooks, 1);
    assert!(
        db.execute(
            "INSERT INTO plugin_items VALUES ('xx','widget','global','x','{}')",
            [],
        )
        .is_err(),
        "the widened check still refuses an unknown kind"
    );
}

/// Step 8 (N1) gives the team tables of a version 7 database a `parent` column, keeping each
/// row with no lead; a database with no team tables upgrades with none made, and a second
/// engine start adds nothing.
#[test]
fn migration_8_adds_parent_to_team_tasks_and_agents() {
    let root = Root::new();
    let mut db = root.connection();
    db.execute_batch(
        "CREATE TABLE team_tasks (id INTEGER PRIMARY KEY, project TEXT NOT NULL, prompt TEXT NOT NULL);
         CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT NOT NULL, role TEXT NOT NULL);
         INSERT INTO team_tasks(id,project,prompt) VALUES (3,'notes','Write the notes page');
         INSERT INTO team_agents(id,project,role) VALUES (5,'notes','sub');
         PRAGMA user_version = 7;",
    )
    .unwrap();

    upgrade(&mut db, &root.database(), None, STEPS).unwrap();

    assert_eq!(version(&db).unwrap(), latest(STEPS));
    for (table, id) in [("team_tasks", 3), ("team_agents", 5)] {
        let parent: Option<i64> = db
            .query_row(
                &format!("SELECT parent FROM {table} WHERE id=?1"),
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(parent, None, "{table} keeps its row with no lead");
    }
    db.execute("UPDATE team_agents SET parent=5 WHERE id=5", [])
        .unwrap();
    db.execute_batch("PRAGMA user_version = 7").unwrap();
    upgrade(&mut db, &root.database(), None, STEPS).unwrap();
    let columns: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('team_agents') WHERE name='parent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 1, "the step adds the column once");
    let parent: Option<i64> = db
        .query_row("SELECT parent FROM team_agents WHERE id=5", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(parent, Some(5), "a second run keeps the recorded lead");

    let bare = Root::new();
    let mut empty = bare.connection();
    empty.execute_batch("PRAGMA user_version = 7").unwrap();
    upgrade(&mut empty, &bare.database(), None, STEPS).unwrap();
    assert_eq!(version(&empty).unwrap(), latest(STEPS));
    let tables: i64 = empty
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('team_tasks','team_agents')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0, "the step makes no team table of its own");
}
