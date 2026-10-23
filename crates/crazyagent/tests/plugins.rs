use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    AccountSecret, OwnerClientKind, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::plugins::{Scope, ServerSpec, ServerStatus, plain_problem};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";
const FIXTURE: &str = include_str!("fixtures/mcp_fixture.py");

struct Case {
    root: tempfile::TempDir,
    service: Option<EngineService>,
    stopped: watch::Receiver<bool>,
    _stop: watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-plugins-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, SETTINGS).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = watch::channel(false);
        let mut case = Self {
            root,
            service: None,
            stopped,
            _stop: stop,
        };
        case.start().await;
        case
    }

    async fn start(&mut self) {
        let service = EngineService::start_with_settings_sources(
            self.root.path(),
            "e.sock",
            &self.root.path().join("global.toml"),
            &self.root.path().join("project.toml"),
            &self.stopped,
        )
        .await
        .unwrap();
        self.service = Some(service);
    }

    async fn restart(&mut self) {
        self.service.take().unwrap().shutdown().await.unwrap();
        self.start().await;
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

fn skill_folder(root: &Path, folder: &str, name: &str) -> String {
    let path = root.join("picked").join(folder);
    fs::create_dir_all(path.join("scripts")).unwrap();
    fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Teaches {name}.\n---\n\nSteps.\n"),
    )
    .unwrap();
    fs::write(path.join("scripts/never.sh"), "touch ran\n").unwrap();
    path.display().to_string()
}

fn fixture(root: &Path) -> String {
    let path = root.join("mcp_fixture.py");
    fs::write(&path, FIXTURE).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path.display().to_string()
}

fn secret(value: &str) -> AccountSecret {
    AccountSecret::new(value.as_bytes().to_vec()).unwrap()
}

fn files_under(path: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() {
                found.extend(files_under(&child));
            } else {
                found.push(child);
            }
        }
    }
    found
}

#[tokio::test]
async fn skills_are_copied_into_the_engine_listed_switched_per_project_and_removed() {
    let mut case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "app").await;
    let root = case.root.path().to_path_buf();
    let release = controller
        .add_skill(
            Scope::Global,
            skill_folder(&root, "release", "release-notes"),
        )
        .await
        .unwrap();
    assert_eq!(release.name, "release-notes");
    assert_eq!(release.description, "Teaches release-notes.");
    assert_eq!(release.scope_label, "For every project");
    assert!(release.enabled);
    let stored = PathBuf::from(&release.folder);
    assert!(stored.starts_with(root.join("plugins/skills/global")));
    assert!(stored.join("SKILL.md").is_file());
    assert!(stored.join("scripts/never.sh").is_file());
    assert!(!root.join("picked/release/ran").exists());
    assert!(!stored.join("ran").exists());
    let linear = controller
        .add_skill(
            Scope::Global,
            skill_folder(&root, "linear", "linear-method"),
        )
        .await
        .unwrap();
    let local = controller
        .add_skill(
            Scope::Project {
                project_id: project.clone(),
            },
            skill_folder(&root, "style", "app-style"),
        )
        .await
        .unwrap();
    assert_eq!(local.scope_label, "This project only");

    let again = controller
        .add_skill(Scope::Global, skill_folder(&root, "copy", "release-notes"))
        .await
        .unwrap_err();
    assert_eq!(again.0, "skill_name_taken");
    fs::create_dir_all(root.join("picked/empty")).unwrap();
    let empty = controller
        .add_skill(
            Scope::Global,
            root.join("picked/empty").display().to_string(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        plain_problem(&empty.0),
        Some("This folder has no SKILL.md file.")
    );
    let unknown = controller
        .add_skill(
            Scope::Project {
                project_id: ProjectId::new("nope").unwrap(),
            },
            skill_folder(&root, "nope", "nope"),
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.0, "plugin_project_unknown");

    let global: Vec<String> = controller
        .plugin_skills(None)
        .await
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    assert_eq!(global, vec!["linear-method", "release-notes"]);
    controller
        .switch_plugin(project.clone(), linear.id.clone(), false)
        .await
        .unwrap();
    let here: Vec<(String, Option<bool>)> = controller
        .plugin_skills(Some(project.clone()))
        .await
        .unwrap()
        .into_iter()
        .map(|skill| (skill.name, skill.on_here))
        .collect();
    assert_eq!(
        here,
        vec![
            ("linear-method".into(), Some(false)),
            ("release-notes".into(), Some(true)),
            ("app-style".into(), None),
        ]
    );
    let effective = controller.project_plugins(project.clone()).await.unwrap();
    let names: Vec<&str> = effective.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["app-style", "release-notes"]);
    assert!(
        effective
            .skills
            .iter()
            .all(|s| s.folder.join("SKILL.md").is_file())
    );

    controller
        .set_skill_enabled(release.id.clone(), false)
        .await
        .unwrap();
    let effective = controller.project_plugins(project.clone()).await.unwrap();
    assert_eq!(effective.skills.len(), 1);
    assert_eq!(effective.skills[0].name, "app-style");

    controller.remove_skill(release.id.clone()).await.unwrap();
    assert!(!stored.exists());
    drop(controller);
    case.restart().await;
    let controller = case.controller().await;
    let after: Vec<(String, Option<bool>)> = controller
        .plugin_skills(Some(project.clone()))
        .await
        .unwrap()
        .into_iter()
        .map(|skill| (skill.name, skill.on_here))
        .collect();
    assert_eq!(
        after,
        vec![
            ("linear-method".into(), Some(false)),
            ("app-style".into(), None),
        ]
    );
    controller
        .switch_plugin(project.clone(), linear.id.clone(), true)
        .await
        .unwrap();
    let names: Vec<String> = controller
        .project_plugins(project)
        .await
        .unwrap()
        .skills
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["app-style", "linear-method"]);
    drop(controller);
    case.close().await;
}

#[tokio::test]
async fn mcp_servers_wait_for_approval_and_agents_get_only_approved_ones() {
    let mut case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "app").await;
    let root = case.root.path().to_path_buf();
    let command = fixture(&root);
    let spec = ServerSpec::Stdio {
        command: command.clone(),
        args: vec![],
    };
    let server = controller
        .add_mcp_server(
            Scope::Global,
            "fixture".into(),
            spec.clone(),
            vec![("FIXTURE_TOKEN".into(), secret("dummy-value"))],
        )
        .await
        .unwrap();
    assert_eq!(server.status, ServerStatus::WaitingForApproval);
    assert_eq!(server.status_label, "Waiting for approval");
    assert_eq!(server.runs, command);
    assert_eq!(server.secrets.len(), 1);
    assert_eq!(server.secrets[0].name, "FIXTURE_TOKEN");
    assert!(server.secrets[0].saved);
    let secrets = files_under(&root.join("plugins/secrets"));
    assert_eq!(secrets.len(), 1);
    assert_eq!(
        fs::metadata(&secrets[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read(&secrets[0]).unwrap(), b"dummy-value");
    let listed = serde_json::to_string(&controller.plugin_servers(None).await.unwrap()).unwrap();
    assert!(!listed.contains("dummy-value"));
    for name in ["engine.db", "engine.db-wal"] {
        if let Ok(bytes) = fs::read(root.join(name)) {
            assert!(
                !bytes.windows(11).any(|w| w == b"dummy-value"),
                "the secret must not reach {name}"
            );
        }
    }
    assert!(
        controller
            .project_plugins(project.clone())
            .await
            .unwrap()
            .servers
            .is_empty()
    );

    let other = ServerSpec::Stdio {
        command: command.clone(),
        args: vec!["normal".into()],
    };
    assert_eq!(
        controller
            .approve_mcp_server(server.id.clone(), other.clone())
            .await
            .unwrap_err()
            .0,
        "server_changed"
    );
    let approved = controller
        .approve_mcp_server(server.id.clone(), spec.clone())
        .await
        .unwrap();
    assert_eq!(approved.status_label, "Approved");
    let effective = controller.project_plugins(project.clone()).await.unwrap();
    assert_eq!(effective.servers.len(), 1);
    assert_eq!(effective.servers[0].name, "fixture");
    assert_eq!(effective.servers[0].spec, spec);
    assert_eq!(effective.servers[0].secrets[0].name, "FIXTURE_TOKEN");

    let checked = controller
        .check_mcp_server(server.id.clone())
        .await
        .unwrap();
    assert_eq!(checked.tools, vec!["echo", "authorized"]);
    assert_eq!(checked.status, ServerStatus::Approved);
    assert_eq!(checked.problem, None);

    let changed = controller
        .update_mcp_server(server.id.clone(), other.clone())
        .await
        .unwrap();
    assert_eq!(changed.status_label, "Waiting for approval");
    assert!(changed.tools.is_empty());
    assert!(
        controller
            .project_plugins(project.clone())
            .await
            .unwrap()
            .servers
            .is_empty()
    );
    controller
        .approve_mcp_server(server.id.clone(), other.clone())
        .await
        .unwrap();
    let off = controller
        .set_mcp_server_enabled(server.id.clone(), false)
        .await
        .unwrap();
    assert_eq!(off.status_label, "Off");
    assert!(
        controller
            .project_plugins(project.clone())
            .await
            .unwrap()
            .servers
            .is_empty()
    );
    controller
        .set_mcp_server_enabled(server.id.clone(), true)
        .await
        .unwrap();

    let refused = controller
        .add_mcp_server(
            Scope::Global,
            "plain".into(),
            ServerSpec::Http {
                url: "http://example.com/mcp".into(),
            },
            vec![],
        )
        .await
        .unwrap_err();
    assert_eq!(refused.0, "server_url_invalid");
    let remote_spec = ServerSpec::Http {
        url: "https://mcp.example.invalid/mcp".into(),
    };
    let remote = controller
        .add_mcp_server(
            Scope::Project {
                project_id: project.clone(),
            },
            "remote".into(),
            remote_spec.clone(),
            vec![("Authorization".into(), secret("Bearer dummy-token"))],
        )
        .await
        .unwrap();
    controller
        .approve_mcp_server(remote.id.clone(), remote_spec.clone())
        .await
        .unwrap();
    let broken_spec = ServerSpec::Stdio {
        command: "/nonexistent/mcp-server".into(),
        args: vec![],
    };
    let broken = controller
        .add_mcp_server(Scope::Global, "broken".into(), broken_spec.clone(), vec![])
        .await
        .unwrap();
    controller
        .approve_mcp_server(broken.id.clone(), broken_spec)
        .await
        .unwrap();
    let broken = controller
        .check_mcp_server(broken.id.clone())
        .await
        .unwrap();
    assert_eq!(broken.status_label, "Can't start");
    assert_eq!(
        broken.problem.as_deref(),
        Some("Can't start: command not found.")
    );
    let names: Vec<String> = controller
        .project_plugins(project.clone())
        .await
        .unwrap()
        .servers
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["fixture", "remote"]);
    let elsewhere = create_project(&mut controller, "other").await;
    let names: Vec<String> = controller
        .project_plugins(elsewhere)
        .await
        .unwrap()
        .servers
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["fixture"]);
    controller
        .switch_plugin(project.clone(), server.id.clone(), false)
        .await
        .unwrap();
    let names: Vec<String> = controller
        .project_plugins(project.clone())
        .await
        .unwrap()
        .servers
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["remote"]);
    assert_eq!(
        controller
            .switch_plugin(project.clone(), remote.id.clone(), false)
            .await
            .unwrap_err()
            .0,
        "plugin_switch_global_only"
    );

    controller
        .remove_mcp_server(server.id.clone())
        .await
        .unwrap();
    assert_eq!(files_under(&root.join("plugins/secrets")).len(), 1);
    drop(controller);
    case.restart().await;
    let controller = case.controller().await;
    let servers: Vec<(String, String)> = controller
        .plugin_servers(Some(project.clone()))
        .await
        .unwrap()
        .into_iter()
        .map(|s| (s.name, s.status_label))
        .collect();
    assert_eq!(
        servers,
        vec![
            ("broken".into(), "Can't start".into()),
            ("remote".into(), "Approved".into()),
        ]
    );
    controller.remove_mcp_server(remote.id).await.unwrap();
    assert!(files_under(&root.join("plugins/secrets")).is_empty());
    drop(controller);
    case.close().await;
}

#[tokio::test]
async fn a_server_whose_secret_cannot_be_saved_is_not_left_behind() {
    let case = Case::new().await;
    let controller = case.controller().await;
    let root = case.root.path().to_path_buf();
    let spec = ServerSpec::Stdio {
        command: "/usr/bin/true".into(),
        args: vec![],
    };
    let duplicate = controller
        .add_mcp_server(
            Scope::Global,
            "twice".into(),
            spec.clone(),
            vec![
                ("TOKEN".into(), secret("first")),
                ("TOKEN".into(), secret("second")),
            ],
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate.0, "server_secret_duplicate");
    assert!(plain_problem(&duplicate.0).is_some());
    assert!(controller.plugin_servers(None).await.unwrap().is_empty());

    fs::create_dir(root.join("plugins")).unwrap();
    fs::set_permissions(root.join("plugins"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(root.join("plugins/secrets")).unwrap();
    fs::set_permissions(
        root.join("plugins/secrets"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let unsafe_store = controller
        .add_mcp_server(
            Scope::Global,
            "tools".into(),
            spec.clone(),
            vec![("TOKEN".into(), secret("dummy-value"))],
        )
        .await
        .unwrap_err();
    assert_eq!(unsafe_store.0, "plugin_store_unsafe");
    assert!(controller.plugin_servers(None).await.unwrap().is_empty());
    assert!(files_under(&root.join("plugins/secrets")).is_empty());

    fs::set_permissions(
        root.join("plugins/secrets"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let added = controller
        .add_mcp_server(
            Scope::Global,
            "tools".into(),
            spec,
            vec![("TOKEN".into(), secret("dummy-value"))],
        )
        .await
        .unwrap();
    assert!(added.secrets[0].saved);
    drop(controller);
    case.close().await;
}
