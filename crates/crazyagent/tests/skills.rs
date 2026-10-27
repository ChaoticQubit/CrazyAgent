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
