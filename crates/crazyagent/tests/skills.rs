#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::plugins::{Scope, plain_problem};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::skills;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";

struct Case {
    root: tempfile::TempDir,
    service: Option<EngineService>,
    _stop: watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-skills-")
            .tempdir_in(fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, SETTINGS).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = watch::channel(false);
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

    async fn controller(&self) -> OwnerController {
        OwnerController::attach(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(5),
        )
        .await
        .unwrap()
    }

    async fn close(mut self) {
        self.service.take().unwrap().shutdown().await.unwrap();
    }
}

async fn create_project(controller: &mut OwnerController, id: &str) -> ProjectId {
    let project = ProjectId::new(id).unwrap();
    controller.read(OwnerRead::Capabilities).await.unwrap();
    controller
        .begin_mutation(
            OwnerMutation::CreateProject {
                project_id: project.clone(),
                spec: ProjectSpec {
                    name: format!("Project {id}"),
                    repositories: vec![],
                },
            },
            1,
        )
        .unwrap();
    match controller.send_pending().await.unwrap() {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            assert_eq!(operation.result, OwnerResult::Applied)
        }
        other => panic!("actual project receipt required: {other:?}"),
    }
    project
}

fn skill_folder(root: &Path, name: &str, description: &str, body: &str) -> String {
    let path = root.join("picked").join(name);
    fs::create_dir_all(path.join("references")).unwrap();
    fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}"),
    )
    .unwrap();
    fs::write(path.join("references/guide.md"), "Guide.\n").unwrap();
    path.display().to_string()
}

#[tokio::test]
async fn the_catalog_lists_each_skill_with_its_line_source_and_both_switches() {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "app").await;
    let other = create_project(&mut controller, "other").await;
    let root = case.root.path().to_path_buf();
    let graph_source = skill_folder(
        &root,
        "graph",
        "Builds a code graph for questions. Use it before reading source.",
        "Steps.\n",
    );
    let graph = controller
        .add_skill(Scope::Global, graph_source.clone())
        .await
        .unwrap();
    let notes = controller
        .add_skill(
            Scope::Global,
            skill_folder(&root, "notes", "Writes release notes.", "Steps.\n"),
        )
        .await
        .unwrap();
    let local = controller
        .add_skill(
            Scope::Project {
                project_id: project.clone(),
            },
            skill_folder(&root, "style", "House style for this app.", "Steps.\n"),
        )
        .await
        .unwrap();

    let app_level = skills::catalog(&controller, None).await.unwrap();
    let rows: Vec<(&str, &str, bool, Option<bool>)> = app_level
        .iter()
        .map(|skill| {
            (
                skill.name.as_str(),
                skill.line.as_str(),
                skill.enabled,
                skill.on_here,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("graph", "Builds a code graph for questions.", true, None),
            ("notes", "Writes release notes.", true, None),
        ]
    );
    assert_eq!(app_level[0].source, graph_source);

    controller
        .set_skill_enabled(notes.id.clone(), false)
        .await
        .unwrap();
    controller
        .switch_plugin(project.clone(), graph.id.clone(), false)
        .await
        .unwrap();
    let here = skills::catalog(&controller, Some(project.clone()))
        .await
        .unwrap();
    let rows: Vec<(&str, bool, Option<bool>)> = here
        .iter()
        .map(|skill| (skill.name.as_str(), skill.enabled, skill.on_here))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("graph", true, Some(false)),
            ("notes", false, Some(true)),
            ("style", true, None),
        ]
    );
    let elsewhere: Vec<String> = skills::catalog(&controller, Some(other.clone()))
        .await
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    assert_eq!(elsewhere, vec!["graph", "notes"]);

    let effective = controller.project_plugins(project.clone()).await.unwrap();
    let given: Vec<&str> = effective
        .skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    assert_eq!(
        given,
        vec!["style"],
        "only skills on at both levels reach agents"
    );
    controller
        .switch_plugin(project.clone(), graph.id.clone(), true)
        .await
        .unwrap();
    controller
        .set_skill_enabled(notes.id.clone(), true)
        .await
        .unwrap();
    let effective = controller.project_plugins(project.clone()).await.unwrap();
    let given: Vec<&str> = effective
        .skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    assert_eq!(given, vec!["graph", "notes", "style"]);
    assert_eq!(local.line, "House style for this app.");
    case.close().await;
}

#[tokio::test]
async fn a_skill_detail_reads_the_whole_skill_md_its_files_and_the_project_switch() {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "app").await;
    let other = create_project(&mut controller, "other").await;
    let root = case.root.path().to_path_buf();
    let body = "Use \"quotes\" and \\ paths. Read é.\n".repeat(2500);
    let big = controller
        .add_skill(
            Scope::Global,
            skill_folder(&root, "design", "Designs front ends.", &body),
        )
        .await
        .unwrap();
    let local = controller
        .add_skill(
            Scope::Project {
                project_id: project.clone(),
            },
            skill_folder(&root, "style", "House style.", "Short.\n"),
        )
        .await
        .unwrap();

    let detail = controller.skill_detail(big.id.clone(), None).await.unwrap();
    let stored = fs::read_to_string(Path::new(&big.folder).join("SKILL.md")).unwrap();
    assert!(stored.len() > 3 * skills::PAGE_TEXT_BYTES_MAX);
    assert_eq!(detail.text, stored);
    assert!(detail.text.ends_with(&body));
    assert_eq!(detail.skill.name, "design");
    assert_eq!(detail.skill.on_here, None);
    let files: Vec<&str> = detail.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(files, vec!["SKILL.md", "references/guide.md"]);
    assert_eq!(detail.files[0].bytes, stored.len() as u64);
    assert_eq!(detail.files_more, 0);

    controller
        .switch_plugin(project.clone(), big.id.clone(), false)
        .await
        .unwrap();
    let here = controller
        .skill_detail(big.id.clone(), Some(project.clone()))
        .await
        .unwrap();
    assert_eq!(here.skill.on_here, Some(false));
    assert!(here.skill.enabled);
    let mine = controller
        .skill_detail(local.id.clone(), Some(project.clone()))
        .await
        .unwrap();
    assert_eq!(
        mine.text,
        "---\nname: style\ndescription: House style.\n---\n\nShort.\n"
    );

    let hidden = controller
        .skill_detail(local.id.clone(), Some(other.clone()))
        .await
        .unwrap_err();
    assert_eq!(hidden.0, "skill_not_found");
    let global_view = controller
        .skill_detail(local.id.clone(), None)
        .await
        .unwrap_err();
    assert_eq!(global_view.0, "skill_not_found");
    controller.remove_skill(big.id.clone()).await.unwrap();
    let removed = controller
        .skill_detail(big.id.clone(), None)
        .await
        .unwrap_err();
    assert_eq!(plain_problem(&removed.0), Some("This skill was removed."));
    let bad = controller
        .skill_detail("Not An Id".into(), None)
        .await
        .unwrap_err();
    assert_eq!(bad.0, "plugin_not_found");
    case.close().await;
}

fn private_folder(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn found_skill(folder: &Path, name: &str, guide: &str) {
    fs::create_dir_all(folder.join("references")).unwrap();
    fs::write(
        folder.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: The {name} skill. More here.\n---\n\nSteps.\n"),
    )
    .unwrap();
    fs::write(folder.join("references/guide.md"), guide).unwrap();
}

#[tokio::test]
async fn only_ticked_skills_are_imported_on_without_private_files_and_a_differing_one_is_replaced()
{
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "app").await;
    let root = case.root.path().to_path_buf();
    let home = root.join("home");
    let scratch = root.join("scratch");
    private_folder(&scratch);
    let claude = home.join(".claude/skills");
    let notes = claude.join("notes");
    found_skill(&notes, "notes", "Guide.\n");
    fs::write(notes.join(".env"), "TOKEN=dummy-value").unwrap();
    fs::write(notes.join("api_key.txt"), "dummy-key").unwrap();
    found_skill(&claude.join("style"), "style", "Style.\n");
    let graph = home.join(".codex/skills/graph");
    found_skill(&graph, "graph", "Graph.\n");

    let list = skills::import::scan(
        &home,
        &skills::catalog(&controller, None).await.unwrap(),
        &scratch,
    )
    .unwrap();
    let names: Vec<&str> = list.found.iter().map(|found| found.name.as_str()).collect();
    assert_eq!(names, vec!["notes", "style", "graph"]);
    assert!(
        list.found
            .iter()
            .all(|found| found.state == skills::import::ImportState::New)
    );
    let ticked: Vec<(String, Option<String>)> = list
        .found
        .iter()
        .filter(|found| found.name != "style")
        .map(|found| (found.folder.clone(), None))
        .collect();
    let done = controller.import_skills(ticked).await.unwrap();
    assert_eq!(done.len(), 2);
    let first = done[0].1.as_ref().unwrap();
    assert_eq!(first.skill.name, "notes");
    assert_eq!(first.skipped, vec![".env", "api_key.txt"]);
    assert_eq!(first.skipped_total, 2);
    assert!(done[1].1.as_ref().unwrap().skipped.is_empty());

    let app = skills::catalog(&controller, None).await.unwrap();
    let rows: Vec<(&str, bool, &str)> = app
        .iter()
        .map(|skill| (skill.name.as_str(), skill.enabled, skill.source.as_str()))
        .collect();
    let notes_source = notes.display().to_string();
    let graph_source = graph.display().to_string();
    assert_eq!(
        rows,
        vec![
            ("graph", true, graph_source.as_str()),
            ("notes", true, notes_source.as_str())
        ]
    );
    let stored = Path::new(&app[1].folder);
    assert!(stored.join("references/guide.md").is_file());
    assert!(!stored.join(".env").exists());
    assert!(!stored.join("api_key.txt").exists());
    assert_eq!(
        fs::read_to_string(notes.join(".env")).unwrap(),
        "TOKEN=dummy-value",
        "the source folder is never changed"
    );
    let given = controller.project_plugins(project.clone()).await.unwrap();
    let given: Vec<&str> = given
        .skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    assert_eq!(
        given,
        vec!["graph", "notes"],
        "imported skills reach agents"
    );

    let notes_id = app[1].id.clone();
    let graph_id = app[0].id.clone();
    fs::write(notes.join("references/guide.md"), "Newer guide.\n").unwrap();
    let again = skills::import::scan(&home, &app, &scratch).unwrap();
    let states: Vec<(&str, &skills::import::ImportState)> = again
        .found
        .iter()
        .map(|found| (found.name.as_str(), &found.state))
        .collect();
    assert_eq!(
        states,
        vec![
            (
                "notes",
                &skills::import::ImportState::Differs {
                    id: notes_id.clone()
                }
            ),
            ("style", &skills::import::ImportState::New),
            (
                "graph",
                &skills::import::ImportState::Added {
                    id: graph_id.clone()
                }
            ),
        ]
    );

    controller
        .switch_plugin(project.clone(), notes_id.clone(), false)
        .await
        .unwrap();
    controller
        .set_skill_enabled(notes_id.clone(), false)
        .await
        .unwrap();
    let taken = controller
        .import_skills(vec![(notes_source.clone(), None)])
        .await
        .unwrap();
    assert_eq!(taken[0].1.as_ref().unwrap_err(), "skill_name_taken");
    let wrong = controller
        .import_skills(vec![(notes_source.clone(), Some(graph_id.clone()))])
        .await
        .unwrap();
    assert_eq!(
        wrong[0].1.as_ref().unwrap_err(),
        "skill_import_name_changed"
    );
    let replaced = controller
        .import_skills(vec![(notes_source.clone(), Some(notes_id.clone()))])
        .await
        .unwrap();
    let replaced = replaced[0].1.as_ref().unwrap();
    assert_eq!(replaced.skill.id, notes_id);
    assert!(replaced.skill.enabled);
    let here = skills::catalog(&controller, Some(project.clone()))
        .await
        .unwrap();
    let notes_here = here.iter().find(|skill| skill.id == notes_id).unwrap();
    assert_eq!(notes_here.on_here, Some(false), "project switches are kept");
    assert_eq!(
        fs::read_to_string(Path::new(&notes_here.folder).join("references/guide.md")).unwrap(),
        "Newer guide.\n"
    );
    assert!(!Path::new(&notes_here.folder).join(".env").exists());
    let graph_here = here.iter().find(|skill| skill.id == graph_id).unwrap();
    assert_eq!(
        fs::read_to_string(Path::new(&graph_here.folder).join("references/guide.md")).unwrap(),
        "Graph.\n"
    );

    let leaky = claude.join("leaky");
    found_skill(&leaky, "leaky", "Leaky.\n");
    fs::write(root.join("outside.md"), "secret").unwrap();
    std::os::unix::fs::symlink(root.join("outside.md"), leaky.join("leak.md")).unwrap();
    let heavy = claude.join("heavy");
    found_skill(&heavy, "heavy", "Heavy.\n");
    fs::write(
        heavy.join("big.bin"),
        vec![0u8; crazyagent::plugins::skill_folder::IMPORT_BYTES_MAX as usize + 1],
    )
    .unwrap();
    let refused = controller
        .import_skills(vec![
            (leaky.display().to_string(), None),
            (heavy.display().to_string(), None),
        ])
        .await
        .unwrap();
    assert_eq!(refused[0].1.as_ref().unwrap_err(), "skill_link_outside");
    assert_eq!(refused[1].1.as_ref().unwrap_err(), "skill_import_too_large");
    assert_eq!(
        plain_problem("skill_import_too_large"),
        Some("A skill brought in from your skills folders can be up to 2 MB.")
    );
    let names: Vec<String> = skills::catalog(&controller, None)
        .await
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    assert_eq!(names, vec!["graph", "notes"]);
    case.close().await;
}
