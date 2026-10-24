#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerMutation, OwnerResponse, OwnerResult};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::projects_home::{
    CRAZYAGENT_DIRECTORY, EXCLUDES, FoundState, LAYOUT, Registered, destination, folder_name,
    forgotten, plain_problem, plan_trash, slug, state_folder,
};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn git(folder: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(folder)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn one_folder_name_rule_covers_new_projects_and_intake() {
    for (name, folder) in [
        ("Reading List", Some("reading-list")),
        ("  My   App!! v2 ", Some("my-app-v2")),
        ("CrazyAgent", Some("crazyagent")),
        ("../../etc", Some("etc")),
        ("café au lait", Some("caf-au-lait")),
        ("日本語", None),
        ("---", None),
        ("", None),
        ("CON", None),
        ("lpt1", None),
    ] {
        assert_eq!(slug(name).as_deref(), folder, "{name}");
        if let Some(folder) = folder {
            folder_name(folder).unwrap();
        }
    }
    let long = slug(&"word ".repeat(40)).unwrap();
    assert!(long.len() <= 64 && !long.ends_with('-'), "{long}");
    folder_name(&long).unwrap();

    for name in ["notes", "My Project", "v1.2", "a_b"] {
        folder_name(name).unwrap();
    }
    for name in [
        "",
        ".hidden",
        "trailing.",
        "trailing ",
        "a/b",
        "a\\b",
        "a:b",
        "tab\there",
        "nul",
        "Com3.txt",
        &"x".repeat(256),
    ] {
        assert_eq!(
            folder_name(name).unwrap_err().0,
            "home_folder_name_invalid",
            "{name}"
        );
    }
    assert!(plain_problem("home_folder_name_invalid").is_some());

    let temp = tempfile::tempdir_in("/private/tmp").unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir(home.join("taken")).unwrap();
    assert_eq!(destination(&home, "free").unwrap(), home.join("free"));
    assert_eq!(
        destination(&home, "taken").unwrap_err().0,
        "home_destination_exists"
    );
    assert_eq!(
        destination(&home, ".hidden").unwrap_err().0,
        "home_folder_name_invalid"
    );
    assert_eq!(names(&home), ["taken"]);
}

#[test]
fn the_state_folder_is_crazyagent_when_present_and_never_a_link() {
    let temp = tempfile::tempdir_in("/private/tmp").unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home_project = root.join("home-project");
    fs::create_dir_all(home_project.join(CRAZYAGENT_DIRECTORY)).unwrap();
    assert_eq!(
        state_folder(&home_project),
        home_project.join(CRAZYAGENT_DIRECTORY)
    );
    let legacy = root.join("legacy");
    fs::create_dir_all(legacy.join("context")).unwrap();
    assert_eq!(state_folder(&legacy), legacy);
    let linked = root.join("linked");
    fs::create_dir(&linked).unwrap();
    symlink(&root, linked.join(CRAZYAGENT_DIRECTORY)).unwrap();
    assert_eq!(state_folder(&linked), linked);
    let file = root.join("file-state");
    fs::create_dir(&file).unwrap();
    fs::write(file.join(CRAZYAGENT_DIRECTORY), "x").unwrap();
    assert_eq!(state_folder(&file), file);
}

#[test]
fn only_plain_unused_top_level_home_folders_can_be_deleted() {
    let temp = tempfile::tempdir_in("/private/tmp").unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    for folder in [
        home.join("free"),
        home.join("used"),
        home.join("free").join("inner"),
        home.join(".hidden"),
        root.join("outside"),
    ] {
        fs::create_dir_all(folder).unwrap();
    }
    fs::write(home.join("notes.txt"), "x").unwrap();
    symlink(home.join("free"), home.join("pointer")).unwrap();
    let registered = [Registered {
        project_id: ProjectId::new("used").unwrap(),
        name: "Used".into(),
        folder: home.join("used"),
        onboarding: false,
    }];
    let text = |path: PathBuf| path.to_str().unwrap().to_owned();

    assert_eq!(
        plan_trash(&home, &text(home.join("free")), &registered).unwrap(),
        home.join("free")
    );
    for (path, code) in [
        (text(home.join("used")), "home_trash_still_set_up"),
        (text(home.join("free").join("inner")), "home_trash_outside"),
        (text(home.join(".hidden")), "home_trash_outside"),
        (text(root.join("outside")), "home_trash_outside"),
        (text(home.clone()), "home_trash_outside"),
        ("/".to_owned(), "home_trash_outside"),
        (text(home.join("pointer")), "home_trash_not_folder"),
        (text(home.join("notes.txt")), "home_trash_not_folder"),
        (text(home.join("absent")), "home_trash_missing"),
        ("relative".to_owned(), "home_trash_invalid"),
        (
            format!("{}/../home/free", text(home.clone())),
            "home_trash_invalid",
        ),
    ] {
        assert_eq!(
            plan_trash(&home, &path, &registered).unwrap_err().0,
            code,
            "{path}"
        );
        assert!(plain_problem(code).is_some(), "{code}");
    }
    assert_eq!(
        names(&home),
        [".hidden", "free", "notes.txt", "pointer", "used"]
    );
    assert!(forgotten("/x/reading-list").contains("/x/reading-list"));
}

struct Engine {
    root: tempfile::TempDir,
    service: Option<EngineService>,
    _stop: tokio::sync::watch::Sender<bool>,
}

impl Engine {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-new-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, "timeout_ms = 5000\n").unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        Self {
            root,
            service: Some(service),
            _stop: stop,
        }
    }

    async fn stop(mut self) {
        self.service.take().unwrap().shutdown().await.unwrap();
    }
}

fn result(response: OwnerResponse) -> OwnerResult {
    match response {
        OwnerResponse::Applied { operation } => operation.result,
        other => panic!("owner mutation must be recorded: {other:?}"),
    }
}

fn rejected(code: &str) -> OwnerResult {
    OwnerResult::Rejected { code: code.into() }
}

fn setup() -> ProjectSetup {
    ProjectSetup {
        about: "Tracks the books I want to read.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Website],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![],
    }
}

fn spec(name: &str) -> ProjectSpec {
    ProjectSpec {
        name: name.into(),
        repositories: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_project_lands_in_the_home_as_a_ready_git_repository() {
    let engine = Engine::start().await;
    let place = tempfile::tempdir_in("/private/tmp").unwrap();
    let home = place.path().canonicalize().unwrap().join("projects");
    fs::create_dir(&home).unwrap();
    let mut owner = OwnerController::attach(
        engine.root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let mut generation = 0;
    let mut next = || {
        generation += 1;
        generation
    };
    let reading = ProjectId::new("reading").unwrap();

    let response = owner
        .setup_project(reading.clone(), spec("Reading List"), setup(), next())
        .await
        .unwrap();
    assert_eq!(result(response), rejected("projects_home_unset"));
    assert_eq!(
        plain_problem("projects_home_unset"),
        Some("Choose a projects home first.")
    );
    assert!(!engine.root.path().join("projects").join("reading").exists());

    let response = owner
        .set_projects_home(home.to_str().unwrap().into(), next())
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let response = owner
        .setup_project(reading.clone(), spec("Reading List"), setup(), next())
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);

    let folder = home.join("reading-list");
    assert_eq!(names(&home), ["reading-list"]);
    let state = folder.join(CRAZYAGENT_DIRECTORY);
    for name in LAYOUT {
        assert!(state.join(name).is_dir(), "{name}");
    }
    assert!(state.join("context").join("brief.md").is_file());
    assert!(!folder.join("context").exists() && !folder.join("worktrees").exists());
    assert_eq!(git(&folder, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert_eq!(
        git(&folder, &["status", "--porcelain", "--untracked-files=all"]),
        ""
    );
    let exclude = fs::read_to_string(folder.join(".git").join("info").join("exclude")).unwrap();
    for line in EXCLUDES {
        assert_eq!(
            exclude.lines().filter(|present| *present == line).count(),
            1
        );
    }

    let found = owner.found_projects().await.unwrap();
    let entry = found
        .iter()
        .find(|entry| entry.name == "reading-list")
        .unwrap();
    assert_eq!(entry.state, FoundState::Ready);
    assert_eq!(entry.project_id.as_ref(), Some(&reading));
    assert!(entry.git && entry.known);
    let context = owner.project_context(reading.clone()).await.unwrap();
    assert_eq!(context.folder, state.to_str().unwrap());
    assert!(
        context
            .items
            .iter()
            .any(|item| item.path == "context/brief.md")
    );

    fs::create_dir(home.join("garden")).unwrap();
    fs::write(home.join("garden").join("keep.txt"), "mine").unwrap();
    let garden = ProjectId::new("garden").unwrap();
    let response = owner
        .setup_project(garden.clone(), spec("Garden"), setup(), next())
        .await
        .unwrap();
    assert_eq!(result(response), rejected("home_destination_exists"));
    assert!(plain_problem("home_destination_exists").is_some());
    assert_eq!(names(&home), ["garden", "reading-list"]);
    assert_eq!(names(&home.join("garden")), ["keep.txt"]);
    let response = owner
        .setup_project(garden.clone(), spec("日本"), setup(), next())
        .await
        .unwrap();
    assert_eq!(result(response), rejected("home_destination_exists"));
    let symbols = ProjectId::new("symbols").unwrap();
    let response = owner
        .setup_project(symbols, spec("Symbols & Signs!"), setup(), next())
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    assert_eq!(names(&home), ["garden", "reading-list", "symbols-signs"]);

    let response = owner.remove_project(reading.clone(), next()).await.unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    assert!(state.join("context").join("brief.md").is_file());
    let found = owner.found_projects().await.unwrap();
    let entry = found
        .iter()
        .find(|entry| entry.name == "reading-list")
        .unwrap();
    assert_eq!(entry.state, FoundState::Found);
    assert!(entry.project_id.is_none() && entry.known);

    let symbols_folder = home.join("symbols-signs");
    owner.found_projects().await.unwrap();
    owner
        .begin_mutation(
            OwnerMutation::DeleteProjectFolder {
                path: symbols_folder.to_str().unwrap().into(),
            },
            next(),
        )
        .unwrap();
    assert_eq!(
        result(owner.send_pending().await.unwrap()),
        rejected("home_trash_still_set_up")
    );
    assert!(symbols_folder.is_dir());

    owner.found_projects().await.unwrap();
    owner
        .begin_mutation(
            OwnerMutation::DeleteProjectFolder {
                path: folder.to_str().unwrap().into(),
            },
            next(),
        )
        .unwrap();
    assert_eq!(
        result(owner.send_pending().await.unwrap()),
        OwnerResult::Applied
    );
    assert!(!folder.exists());
    assert_eq!(names(&home), ["garden", "symbols-signs"]);

    owner.found_projects().await.unwrap();
    owner
        .begin_mutation(
            OwnerMutation::DeleteProjectFolder {
                path: place.path().to_str().unwrap().into(),
            },
            next(),
        )
        .unwrap();
    assert_eq!(
        result(owner.send_pending().await.unwrap()),
        rejected("home_trash_outside")
    );
    assert!(home.is_dir());

    engine.stop().await;
}
