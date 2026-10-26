#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::projects_home::{AddMode, FoundProject, FoundState, ProjectsHome};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Engine {
    root: tempfile::TempDir,
    service: Option<EngineService>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl Engine {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-home-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, "timeout_ms = 5000\n").unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, _) = tokio::sync::watch::channel(false);
        let mut engine = Self {
            root,
            service: None,
            stop,
        };
        engine.boot().await;
        engine
    }

    async fn boot(&mut self) {
        assert!(self.service.is_none());
        let stopped = self.stop.subscribe();
        self.service = Some(
            EngineService::start_with_settings_sources(
                self.root.path(),
                "e.sock",
                &self.root.path().join("global.toml"),
                &self.root.path().join("project.toml"),
                &stopped,
            )
            .await
            .unwrap(),
        );
    }

    async fn restart(&mut self) {
        let mut service = self.service.take().unwrap();
        tokio::time::timeout(Duration::from_secs(10), service.shutdown())
            .await
            .unwrap()
            .unwrap();
        self.boot().await;
    }

    async fn owner(&self) -> OwnerController {
        OwnerController::attach(
            self.root.path(),
            OwnerClientKind::Desktop,
            Duration::from_secs(10),
        )
        .await
        .unwrap()
    }

    async fn stop(mut self) {
        let mut service = self.service.take().unwrap();
        service.shutdown().await.unwrap();
    }
}

struct Owner {
    controller: OwnerController,
    generation: u64,
}

impl Owner {
    async fn apply(&mut self, mutation: OwnerMutation) -> OwnerResult {
        self.controller.read(OwnerRead::ProjectsHome).await.unwrap();
        self.generation += 1;
        self.controller
            .begin_mutation(mutation, self.generation)
            .unwrap();
        let result = match self.controller.send_pending().await.unwrap() {
            OwnerResponse::Applied { operation } => {
                assert!(!operation.historical_replay);
                operation.result
            }
            other => panic!("owner mutation must be recorded: {other:?}"),
        };
        assert!(self.controller.pending().is_none());
        result
    }

    async fn home(&mut self) -> ProjectsHome {
        match self
            .controller
            .read(OwnerRead::ProjectsHome)
            .await
            .unwrap()
            .data
        {
            OwnerData::ProjectsHome { home } => home,
            other => panic!("{other:?}"),
        }
    }

    async fn found(&mut self) -> Vec<FoundProject> {
        match self
            .controller
            .read(OwnerRead::FoundProjects)
            .await
            .unwrap()
            .data
        {
            OwnerData::FoundProjects { projects } => projects,
            other => panic!("{other:?}"),
        }
    }

    async fn found_until(&mut self, want: impl Fn(&[FoundProject]) -> bool) -> Vec<FoundProject> {
        let start = Instant::now();
        loop {
            let found = self.found().await;
            if want(&found) {
                return found;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "{found:?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn size(&mut self, path: &Path) -> Result<u64, String> {
        match self
            .controller
            .read(OwnerRead::FolderSize {
                path: path.to_str().unwrap().into(),
            })
            .await
        {
            Ok(state) => match state.data {
                OwnerData::FolderSize { bytes } => Ok(bytes),
                other => panic!("{other:?}"),
            },
            Err(e) => Err(e.0),
        }
    }
}

fn named<'a>(found: &'a [FoundProject], name: &str) -> Option<&'a FoundProject> {
    found.iter().find(|entry| entry.name == name)
}

fn rejected(code: &str) -> OwnerResult {
    OwnerResult::Rejected { code: code.into() }
}

fn add(source: &Path, mode: AddMode) -> OwnerMutation {
    OwnerMutation::AddExistingFolder {
        source: source.to_str().unwrap().into(),
        mode,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_keeps_the_home_watches_it_and_records_each_add() {
    let disk = tempfile::tempdir().unwrap();
    let place = disk.path().canonicalize().unwrap();
    let home = place.join("projects");
    let sources = place.join("sources");
    let other = place.join("other-home");
    for folder in [&home, &sources, &other] {
        fs::create_dir(folder).unwrap();
    }
    let mut engine = Engine::start().await;
    let mut owner = Owner {
        controller: engine.owner().await,
        generation: 0,
    };

    assert_eq!(
        owner.home().await,
        ProjectsHome {
            path: None,
            watching: false
        }
    );
    assert!(owner.found().await.is_empty());
    assert_eq!(
        owner.apply(add(&sources, AddMode::Move)).await,
        rejected("projects_home_unset")
    );
    for (picked, code) in [
        (PathBuf::from("/"), "projects_home_unsafe"),
        (engine.root.path().to_path_buf(), "projects_home_unsafe"),
        (place.join("absent"), "projects_home_missing"),
    ] {
        assert_eq!(
            owner
                .apply(OwnerMutation::SetProjectsHome {
                    path: picked.to_str().unwrap().into(),
                })
                .await,
            rejected(code)
        );
    }
    assert_eq!(
        owner
            .apply(OwnerMutation::SetProjectsHome {
                path: disk.path().join("projects").to_str().unwrap().into(),
            })
            .await,
        OwnerResult::Applied
    );
    assert_eq!(
        owner.home().await,
        ProjectsHome {
            path: Some(home.to_str().unwrap().into()),
            watching: true
        }
    );

    fs::create_dir(home.join("dropped")).unwrap();
    let found = owner
        .found_until(|found| named(found, "dropped").is_some())
        .await;
    let dropped = named(&found, "dropped").unwrap();
    assert_eq!(dropped.state, FoundState::Found);
    assert_eq!(dropped.path, home.join("dropped").to_str().unwrap());
    assert!(!dropped.known && !dropped.git && dropped.project_id.is_none());
    assert!(!home.join("dropped").join(".crazyagent").exists());

    let moved = sources.join("moved");
    fs::create_dir_all(moved.join(".git")).unwrap();
    fs::write(moved.join("main.rs"), "fn main() {}\n").unwrap();
    assert_eq!(
        owner.apply(add(&moved, AddMode::Move)).await,
        OwnerResult::Applied
    );
    assert!(!moved.exists());
    assert_eq!(
        fs::read_to_string(home.join("moved").join("main.rs")).unwrap(),
        "fn main() {}\n"
    );
    let found = owner
        .found_until(|found| named(found, "moved").is_some())
        .await;
    assert!(named(&found, "moved").unwrap().git);

    let copied = sources.join("copied");
    fs::create_dir(&copied).unwrap();
    fs::write(copied.join("data.bin"), vec![9u8; 4096]).unwrap();
    fs::write(copied.join("notes.txt"), "abc").unwrap();
    assert_eq!(owner.size(&copied).await, Ok(4099));
    assert_eq!(
        owner.apply(add(&copied, AddMode::Copy)).await,
        OwnerResult::Applied
    );
    assert!(copied.join("data.bin").is_file());
    assert_eq!(
        fs::read(home.join("copied").join("data.bin")).unwrap(),
        vec![9u8; 4096]
    );

    for (source, mode, code) in [
        (home.join("dropped"), AddMode::Copy, "home_source_in_home"),
        (home.clone(), AddMode::Move, "home_source_in_home"),
        (copied.clone(), AddMode::Copy, "home_destination_exists"),
        (sources.join("absent"), AddMode::Move, "home_source_missing"),
        (PathBuf::from("/"), AddMode::Move, "home_source_unsafe"),
        (
            engine.root.path().to_path_buf(),
            AddMode::Copy,
            "home_source_unsafe",
        ),
    ] {
        assert_eq!(owner.apply(add(&source, mode)).await, rejected(code));
    }
    assert!(home.join("dropped").is_dir() && copied.is_dir());
    assert_eq!(
        owner.size(Path::new("/")).await,
        Err("folder_size_unsafe".into())
    );

    let setup = ProjectSetup {
        about: "Tracks reading lists.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Website],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![],
    };
    let spec = ProjectSpec {
        name: "Reading list".into(),
        repositories: vec![],
    };
    let project = ProjectId::new("reading").unwrap();
    owner.generation += 1;
    match owner
        .controller
        .setup_project(project.clone(), spec, setup, owner.generation)
        .await
        .unwrap()
    {
        OwnerResponse::Applied { operation } => assert_eq!(operation.result, OwnerResult::Applied),
        other => panic!("{other:?}"),
    }
    let folder = home.join("reading-list");
    assert!(folder.is_dir());
    let found = owner.found().await;
    assert!(named(&found, "Reading list").is_none());
    let ready = named(&found, "reading-list").unwrap();
    assert_eq!(ready.state, FoundState::Ready);
    assert_eq!(ready.project_id.as_ref(), Some(&project));
    fs::remove_dir_all(&folder).unwrap();
    let found = owner.found().await;
    let missing = named(&found, "Reading list").unwrap();
    assert_eq!(missing.state, FoundState::Missing);
    assert_eq!(missing.project_id.as_ref(), Some(&project));
    assert_eq!(missing.path, folder.to_str().unwrap());

    owner.controller.detach();
    engine.restart().await;
    owner.controller = engine.owner().await;
    assert_eq!(
        owner.home().await,
        ProjectsHome {
            path: Some(home.to_str().unwrap().into()),
            watching: true
        }
    );
    fs::create_dir(home.join("after-restart")).unwrap();
    owner
        .found_until(|found| {
            [
                "dropped",
                "moved",
                "copied",
                "after-restart",
                "Reading list",
            ]
            .iter()
            .all(|name| named(found, name).is_some())
        })
        .await;

    fs::create_dir(other.join("elsewhere")).unwrap();
    assert_eq!(
        owner
            .apply(OwnerMutation::SetProjectsHome {
                path: other.to_str().unwrap().into(),
            })
            .await,
        OwnerResult::Applied
    );
    assert!(home.join("moved").is_dir() && home.join("copied").is_dir());
    let found = owner
        .found_until(|found| named(found, "elsewhere").is_some())
        .await;
    assert!(named(&found, "moved").is_none() && named(&found, "dropped").is_none());
    assert_eq!(
        owner.home().await.path,
        Some(other.to_str().unwrap().into())
    );

    let third = place.join("third-home");
    fs::create_dir(&third).unwrap();
    assert_eq!(
        owner
            .apply(OwnerMutation::MoveProjectsHome {
                path: other.to_str().unwrap().into(),
                move_projects: true,
            })
            .await,
        rejected("home_move_same")
    );
    assert_eq!(
        owner
            .apply(OwnerMutation::MoveProjectsHome {
                path: third.to_str().unwrap().into(),
                move_projects: true,
            })
            .await,
        OwnerResult::Applied
    );
    assert!(third.join("elsewhere").is_dir() && !other.join("elsewhere").exists());
    assert_eq!(
        owner.home().await,
        ProjectsHome {
            path: Some(third.to_str().unwrap().into()),
            watching: true
        }
    );
    let moved_to = third.join("elsewhere");
    owner
        .found_until(|found| {
            named(found, "elsewhere").is_some_and(|entry| entry.path == moved_to.to_str().unwrap())
        })
        .await;
    assert_eq!(
        owner
            .apply(OwnerMutation::MoveProjectsHome {
                path: home.to_str().unwrap().into(),
                move_projects: false,
            })
            .await,
        OwnerResult::Applied
    );
    assert!(third.join("elsewhere").is_dir() && !home.join("elsewhere").exists());
    assert_eq!(owner.home().await.path, Some(home.to_str().unwrap().into()));

    engine.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_added_through_a_link_is_recorded_at_its_real_name() {
    let disk = tempfile::tempdir().unwrap();
    let place = disk.path().canonicalize().unwrap();
    let home = place.join("projects");
    let code = place.join("code");
    let links = place.join("links");
    for folder in [&home, &code, &links] {
        fs::create_dir(folder).unwrap();
    }
    for name in ["moved", "copied"] {
        fs::create_dir(code.join(name)).unwrap();
        fs::write(code.join(name).join("main.rs"), name).unwrap();
        std::os::unix::fs::symlink(code.join(name), links.join(format!("{name}-link"))).unwrap();
    }
    let engine = Engine::start().await;
    let mut owner = Owner {
        controller: engine.owner().await,
        generation: 0,
    };
    assert_eq!(
        owner
            .apply(OwnerMutation::SetProjectsHome {
                path: home.to_str().unwrap().into(),
            })
            .await,
        OwnerResult::Applied
    );
    for (name, mode) in [("moved", AddMode::Move), ("copied", AddMode::Copy)] {
        let source = format!("{}/", links.join(format!("{name}-link")).to_str().unwrap());
        assert_eq!(
            owner
                .apply(OwnerMutation::AddExistingFolder { source, mode })
                .await,
            OwnerResult::Applied,
            "{name}"
        );
        assert_eq!(
            fs::read_to_string(home.join(name).join("main.rs")).unwrap(),
            name
        );
        assert!(fs::symlink_metadata(home.join(format!("{name}-link"))).is_err());
    }
    assert!(fs::symlink_metadata(code.join("moved")).is_err());
    assert_eq!(
        fs::read_to_string(code.join("copied").join("main.rs")).unwrap(),
        "copied"
    );
    engine.stop().await;
}

fn move_home(path: &Path) -> OwnerMutation {
    OwnerMutation::MoveProjectsHome {
        path: path.to_str().unwrap().into(),
        move_projects: true,
    }
}

fn sql(database: &Path, statement: &str) {
    let db = rusqlite::Connection::open(database).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(statement).unwrap();
}

async fn unknown(owner: &mut Owner, to: &Path) {
    owner
        .controller
        .read(OwnerRead::ProjectsHome)
        .await
        .unwrap();
    owner.generation += 1;
    owner
        .controller
        .begin_mutation(move_home(to), owner.generation)
        .unwrap();
    let response = owner.controller.send_pending().await.unwrap();
    assert!(
        matches!(response, OwnerResponse::OutcomeUnknown { .. }),
        "{response:?}"
    );
    assert!(to.join("reading-list").is_dir() && to.join("plain").is_dir());
}

async fn home_until(owner: &mut Owner, home: &Path) {
    let start = Instant::now();
    while owner.home().await.path.as_deref() != home.to_str() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{}",
            home.display()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn ready_at(owner: &mut Owner, project: &ProjectId, folder: &Path) {
    let found = owner
        .found_until(|found| {
            found.iter().any(|entry| {
                entry.project_id.as_ref() == Some(project) && entry.path == folder.to_str().unwrap()
            })
        })
        .await;
    let entry = found
        .iter()
        .find(|entry| entry.project_id.as_ref() == Some(project))
        .unwrap();
    assert_eq!(entry.state, FoundState::Ready, "{found:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_home_move_whose_record_fails_still_lands_on_the_moved_folders() {
    let disk = tempfile::tempdir().unwrap();
    let place = disk.path().canonicalize().unwrap();
    let first = place.join("first");
    let second = place.join("second");
    let third = place.join("third");
    for folder in [&first, &second, &third] {
        fs::create_dir(folder).unwrap();
    }
    let mut engine = Engine::start().await;
    let mut owner = Owner {
        controller: engine.owner().await,
        generation: 0,
    };
    assert_eq!(
        owner
            .apply(OwnerMutation::SetProjectsHome {
                path: first.to_str().unwrap().into(),
            })
            .await,
        OwnerResult::Applied
    );
    let project = ProjectId::new("reading").unwrap();
    owner.generation += 1;
    let setup = ProjectSetup {
        about: "Tracks reading lists.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Website],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![],
    };
    let spec = ProjectSpec {
        name: "Reading list".into(),
        repositories: vec![],
    };
    match owner
        .controller
        .setup_project(project.clone(), spec, setup, owner.generation)
        .await
        .unwrap()
    {
        OwnerResponse::Applied { operation } => assert_eq!(operation.result, OwnerResult::Applied),
        other => panic!("{other:?}"),
    }
    fs::create_dir(first.join("plain")).unwrap();
    ready_at(&mut owner, &project, &first.join("reading-list")).await;

    let database = engine.root.path().join("engine.db");
    let abort = |event: &str| {
        sql(
            &database,
            &format!(
                "CREATE TRIGGER home_move_abort {event} BEGIN SELECT RAISE(ABORT, 'home_move_injected_abort'); END;"
            ),
        )
    };

    abort("BEFORE INSERT ON owner_operations");
    unknown(&mut owner, &second).await;
    sql(&database, "DROP TRIGGER home_move_abort;");
    home_until(&mut owner, &second).await;
    ready_at(&mut owner, &project, &second.join("reading-list")).await;
    owner.controller.detach();
    owner.controller = engine.owner().await;

    abort("BEFORE UPDATE ON projects_home");
    unknown(&mut owner, &third).await;
    owner.controller.detach();
    engine.restart().await;
    owner.controller = engine.owner().await;
    assert_eq!(owner.home().await.path.as_deref(), second.to_str());
    sql(&database, "DROP TRIGGER home_move_abort;");
    owner.controller.detach();
    engine.restart().await;
    owner.controller = engine.owner().await;
    home_until(&mut owner, &third).await;
    ready_at(&mut owner, &project, &third.join("reading-list")).await;

    assert_eq!(owner.apply(move_home(&first)).await, OwnerResult::Applied);
    assert!(first.join("reading-list").is_dir() && first.join("plain").is_dir());
    ready_at(&mut owner, &project, &first.join("reading-list")).await;
    engine.stop().await;
}

fn repositories(database: &Path, project: &ProjectId) -> Vec<String> {
    let db = rusqlite::Connection::open(database).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    let body: String = db
        .query_row(
            "SELECT body FROM projects WHERE id=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let stored: crazyagent::protocol::StoredProject = serde_json::from_str(&body).unwrap();
    stored.spec.repositories
}

#[tokio::test(flavor = "multi_thread")]
async fn a_home_move_points_the_project_repository_at_the_moved_folder() {
    let disk = tempfile::tempdir().unwrap();
    let place = disk.path().canonicalize().unwrap();
    let first = place.join("first");
    let second = place.join("second");
    let elsewhere = place.join("elsewhere");
    for folder in [&first, &second, &elsewhere] {
        fs::create_dir(folder).unwrap();
    }
    let engine = Engine::start().await;
    let mut owner = Owner {
        controller: engine.owner().await,
        generation: 0,
    };
    assert_eq!(
        owner
            .apply(OwnerMutation::SetProjectsHome {
                path: first.to_str().unwrap().into(),
            })
            .await,
        OwnerResult::Applied
    );
    let setup = ProjectSetup {
        about: "Tracks reading lists.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Website],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![],
    };
    let mut set_up = async |id: &str, name: &str, repositories: Vec<String>| {
        let project = ProjectId::new(id).unwrap();
        owner.generation += 1;
        let spec = ProjectSpec {
            name: name.into(),
            repositories,
        };
        match owner
            .controller
            .setup_project(project.clone(), spec, setup.clone(), owner.generation)
            .await
            .unwrap()
        {
            OwnerResponse::Applied { operation } => {
                assert_eq!(operation.result, OwnerResult::Applied)
            }
            other => panic!("{other:?}"),
        }
        project
    };
    let reading = set_up("reading", "Reading list", vec![]).await;
    let declared = set_up(
        "declared",
        "Declared repos",
        vec![
            first
                .join("declared-repos")
                .join("api")
                .to_str()
                .unwrap()
                .into(),
            elsewhere.to_str().unwrap().into(),
        ],
    )
    .await;
    ready_at(&mut owner, &reading, &first.join("reading-list")).await;
    let database = engine.root.path().join("engine.db");
    assert_eq!(
        repositories(&database, &reading),
        [first.join("reading-list").to_str().unwrap()]
    );

    assert_eq!(owner.apply(move_home(&second)).await, OwnerResult::Applied);
    ready_at(&mut owner, &reading, &second.join("reading-list")).await;

    assert_eq!(
        repositories(&database, &reading),
        [second.join("reading-list").to_str().unwrap()],
        "the master's workspace follows the moved project folder"
    );
    assert_eq!(
        repositories(&database, &declared),
        [
            second.join("declared-repos").join("api").to_str().unwrap(),
            elsewhere.to_str().unwrap(),
        ],
        "a repository inside the moved folder follows it, one outside stays"
    );
    engine.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_set_up_project_past_the_256th_keeps_its_folder_and_its_place_in_the_list() {
    let disk = tempfile::tempdir().unwrap();
    let home = disk.path().canonicalize().unwrap().join("projects");
    fs::create_dir(&home).unwrap();
    let engine = Engine::start().await;
    let mut owner = Owner {
        controller: engine.owner().await,
        generation: 0,
    };
    assert_eq!(
        owner
            .apply(OwnerMutation::SetProjectsHome {
                path: home.to_str().unwrap().into(),
            })
            .await,
        OwnerResult::Applied
    );
    let project = ProjectId::new("reading").unwrap();
    owner.generation += 1;
    let setup = ProjectSetup {
        about: "Tracks reading lists.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Website],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![],
    };
    let spec = ProjectSpec {
        name: "Reading list".into(),
        repositories: vec![],
    };
    match owner
        .controller
        .setup_project(project.clone(), spec, setup, owner.generation)
        .await
        .unwrap()
    {
        OwnerResponse::Applied { operation } => assert_eq!(operation.result, OwnerResult::Applied),
        other => panic!("{other:?}"),
    }
    let folder = home.join("reading-list");
    ready_at(&mut owner, &project, &folder).await;

    for n in 0..256 {
        fs::create_dir(home.join(format!("zz-{n:03}"))).unwrap();
    }
    let database = engine.root.path().join("engine.db");
    sql(
        &database,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 255)
             INSERT INTO project_setups
             SELECT printf('a%03d', i), json_set(s.body,
                 '$.project_id', printf('a%03d', i),
                 '$.name', printf('zz %03d', i),
                 '$.folder', '{}/' || printf('zz-%03d', i))
             FROM n, project_setups s WHERE s.project = 'reading';",
            home.display()
        ),
    );
    let count: i64 = rusqlite::Connection::open(&database)
        .unwrap()
        .query_row("SELECT count(*) FROM project_setups", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 257);

    let found = owner.found().await;
    let listed = named(&found, "reading-list").map(|entry| (entry.state, entry.project_id.clone()));
    let trashed = owner
        .apply(OwnerMutation::DeleteProjectFolder {
            path: folder.to_str().unwrap().into(),
        })
        .await;
    assert_eq!(
        (listed, trashed),
        (
            Some((FoundState::Ready, Some(project.clone()))),
            rejected("home_trash_still_set_up")
        )
    );
    assert!(folder.is_dir());
    assert_eq!(
        found
            .iter()
            .filter(|entry| entry.state == FoundState::Ready && entry.project_id.is_some())
            .count(),
        257
    );

    fs::remove_dir_all(&folder).unwrap();
    let found = owner.found().await;
    let missing = named(&found, "Reading list").unwrap();
    assert_eq!(missing.state, FoundState::Missing);
    assert_eq!(missing.project_id.as_ref(), Some(&project));
    assert_eq!(missing.path, folder.to_str().unwrap());
    engine.stop().await;
}
