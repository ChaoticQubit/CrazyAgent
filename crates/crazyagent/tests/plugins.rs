#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::EngineError;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    AccountSecret, OwnerClientKind, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::plugins::{Scope, ServerSpec, ServerStatus, ServerView, plain_problem};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use rusqlite::OptionalExtension;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::{Mutex, watch};

const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";
const FIXTURE: &str = include_str!("fixtures/mcp_fixture.py");
static ONE_CHECK: Mutex<()> = Mutex::const_new(());

/// Checks a server while no other test of this binary checks one. The engine caps server
/// checks per process, and every test here runs its own engine in this one process, so
/// parallel tests would take each other's places. Inputs: the controller and the server id.
/// Output: the checked server, or the engine's refusal.
async fn check(controller: &OwnerController, id: String) -> Result<ServerView, EngineError> {
    let _one = ONE_CHECK.lock().await;
    controller.check_mcp_server(id).await
}

struct Case {
    root: tempfile::TempDir,
    service: Option<EngineService>,
    stopped: watch::Receiver<bool>,
    _stop: watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        Self::with_settings(SETTINGS).await
    }

    async fn with_settings(settings: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-plugins-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, settings).unwrap();
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
    let folder = root.join("servers");
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join("mcp_fixture.py");
    fs::write(&path, FIXTURE).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path.display().to_string()
}

/// Settings with one local model connection the owner configured at
/// `http://127.0.0.1:<port>/v1`, as for LM Studio or vLLM. Input: the port. Output: the
/// global settings text.
fn local_model_settings(port: u16) -> String {
    format!(
        "{SETTINGS}\n[model_access]\naccounts = [\"local-account\"]\nconnections = [{{ id = \"local-connection\", provider = \"openai-compatible\", harness = \"rcli\", protocol = \"openai-chat\", account_ref = \"local-account\", endpoint = {{ kind = \"http\", url = \"http://127.0.0.1:{port}/v1\" }}, model = {{ id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }}, auth = {{ method = \"none\" }} }}]\npools = [{{ id = \"local-pool\", members = [\"local-connection\"], projects = [], roles = [], fallback_allowed = false }}]\n"
    )
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
        .switch_plugin(project.clone(), linear.id.clone(), false, linear.version)
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
        .set_skill_enabled(release.id.clone(), false, release.version)
        .await
        .unwrap();
    let effective = controller.project_plugins(project.clone()).await.unwrap();
    assert_eq!(effective.skills.len(), 1);
    assert_eq!(effective.skills[0].name, "app-style");

    let version = shown_version(&controller, &release.id, None).await;
    controller
        .remove_skill(release.id.clone(), version)
        .await
        .unwrap();
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
        .switch_plugin(project.clone(), linear.id.clone(), true, linear.version + 1)
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

/// Wiring test: tests/fixtures/mcp_fixture.py stands in for an MCP server and invents its
/// tool list, so this checks approval, secrets and what agents are given. Real harnesses
/// offered the approved server: launch_context_live.rs
/// real_codex_and_claude_receive_the_approved_skill_server_and_context.
/// No test checks a real third-party MCP server: none is installed or approved.
#[tokio::test]
async fn a_skill_description_with_hidden_characters_is_refused_and_never_reaches_agents() {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "app").await;
    let root = case.root.path().to_path_buf();
    let hidden = root.join("picked").join("hidden");
    fs::create_dir_all(&hidden).unwrap();
    fs::write(
        hidden.join("SKILL.md"),
        "---\nname: fmt\ndescription: Formats changed files.\u{e0041}\u{e0042}\n---\n\nSteps.\n",
    )
    .unwrap();
    let refused = controller
        .add_skill(Scope::Global, hidden.display().to_string())
        .await
        .unwrap_err();
    assert_eq!(refused.0, "skill_description_hidden");
    assert!(plain_problem(&refused.0).is_some());

    let added = controller
        .add_skill(Scope::Global, skill_folder(&root, "fmt", "fmt"))
        .await
        .unwrap();
    assert_eq!(added.refused, None);
    let effective = controller.project_plugins(project.clone()).await.unwrap();
    assert!(effective.skills.iter().any(|skill| skill.name == "fmt"));
    let stored = PathBuf::from(&added.folder).join("SKILL.md");
    fs::set_permissions(&stored, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(
        &stored,
        "---\nname: fmt\ndescription: Teaches fmt.\u{e0041}\u{e0042}\n---\n\nSteps.\n",
    )
    .unwrap();
    let effective = controller.project_plugins(project.clone()).await.unwrap();
    assert!(
        effective.skills.iter().all(|skill| skill.name != "fmt"),
        "a stored skill whose SKILL.md hides text is not given to agents"
    );
    let listed = controller.plugin_skills(None).await.unwrap();
    let fmt = listed.iter().find(|skill| skill.name == "fmt").unwrap();
    assert_eq!(
        fmt.refused.as_deref(),
        Some("skill_description_hidden"),
        "the skill list says why agents no longer get it"
    );
    controller
        .remove_skill(fmt.id.clone(), fmt.version)
        .await
        .unwrap();
    assert!(
        controller
            .plugin_skills(None)
            .await
            .unwrap()
            .iter()
            .all(|skill| skill.name != "fmt"),
        "the refused skill can still be removed"
    );
    case.close().await;
}

#[tokio::test]
async fn wiring_mcp_servers_wait_for_approval_and_agents_get_only_approved_ones() {
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

    let checked = check(&controller, server.id.clone()).await.unwrap();
    assert_eq!(checked.tools, vec!["echo", "authorized"]);
    assert_eq!(checked.status, ServerStatus::Approved);
    assert_eq!(checked.problem, None);

    let changed = controller
        .update_mcp_server(server.id.clone(), other.clone(), checked.version)
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
        .set_mcp_server_enabled(server.id.clone(), false, changed.version)
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
    let on = controller
        .set_mcp_server_enabled(server.id.clone(), true, off.version)
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
    let broken = check(&controller, broken.id.clone()).await.unwrap();
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
        .switch_plugin(project.clone(), server.id.clone(), false, on.version)
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
            .switch_plugin(project.clone(), remote.id.clone(), false, remote.version)
            .await
            .unwrap_err()
            .0,
        "plugin_switch_global_only"
    );

    let version = shown_version(&controller, &server.id, None).await;
    controller
        .remove_mcp_server(server.id.clone(), version)
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
    let version = shown_version(&controller, &remote.id, Some(project.clone())).await;
    controller
        .remove_mcp_server(remote.id, version)
        .await
        .unwrap();
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

/// A checked server runs inside the agent boundary: it still answers with its own secret,
/// but it can neither write outside its own check folders nor read another server's saved
/// secret in the engine folder.
#[tokio::test]
async fn a_server_check_cannot_write_outside_or_read_another_servers_secret() {
    let case = Case::new().await;
    let controller = case.controller().await;
    let root = case.root.path().to_path_buf();
    controller
        .add_mcp_server(
            Scope::Global,
            "other".into(),
            ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: vec![],
            },
            vec![("OTHER_TOKEN".into(), secret("other-value"))],
        )
        .await
        .unwrap();
    let saved = files_under(&root.join("plugins/secrets"));
    assert_eq!(saved.len(), 1);
    let marker = root.join("escaped");
    let spec = ServerSpec::Stdio {
        command: fixture(&root),
        args: vec![
            "escape".into(),
            marker.display().to_string(),
            saved[0].display().to_string(),
        ],
    };
    let server = controller
        .add_mcp_server(
            Scope::Global,
            "escape".into(),
            spec.clone(),
            vec![("FIXTURE_TOKEN".into(), secret("dummy-value"))],
        )
        .await
        .unwrap();
    controller
        .approve_mcp_server(server.id.clone(), spec)
        .await
        .unwrap();
    let checked = check(&controller, server.id.clone()).await.unwrap();
    assert!(
        !marker.exists(),
        "the checked server must not write outside its boundary"
    );
    assert_eq!(checked.tools, vec!["echo", "authorized"]);
    assert_eq!(checked.problem, None);
    let folder = root.join("mcp-checks").join(&server.id);
    assert!(folder.join("home").is_dir());
    let version = shown_version(&controller, &server.id, None).await;
    controller
        .remove_mcp_server(server.id.clone(), version)
        .await
        .unwrap();
    assert!(
        !folder.exists(),
        "a removed server leaves no check folder behind"
    );
    case.close().await;
}

/// A removed server's check home can hold a package manager's whole install, as an `npx`
/// server's does. The removal still answers Done within the owner's wait, its check folder
/// is gone from its path when it does, and what the check wrote is deleted after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_whose_check_home_holds_many_files_answers_its_removal_at_once() {
    let case = Case::new().await;
    let controller = case.controller().await;
    let root = case.root.path().to_path_buf();
    let server = controller
        .add_mcp_server(
            Scope::Global,
            "packages".into(),
            ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: vec![],
            },
            Vec::new(),
        )
        .await
        .unwrap();
    let checks = root.join("mcp-checks");
    let folder = checks.join(&server.id);
    for package in 0..100 {
        let installed = folder
            .join("home/.npm/_cacache")
            .join(format!("package-{package}"));
        fs::create_dir_all(&installed).unwrap();
        for file in 0..1000 {
            fs::File::create(installed.join(format!("file-{file}"))).unwrap();
        }
    }

    controller
        .remove_mcp_server(server.id.clone(), server.version)
        .await
        .expect("a committed removal answers Done however much its check home holds");
    assert!(
        !folder.exists(),
        "the removed server's check folder is gone from its path when the removal answers"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let left: Vec<String> = fs::read_dir(&checks)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(&server.id))
            .collect();
        if left.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "what the removed server's check wrote must be deleted: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(controller);
    case.close().await;
}

/// A checked server gets the open network agents get, but while the owner's local model
/// server is down it cannot listen on that configured endpoint's port and answer the
/// prompts sent there; another loopback port stays free for it to bind.
#[tokio::test]
async fn a_server_check_cannot_listen_on_a_configured_local_model_port() {
    let (kept, other) = {
        let first = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        let second = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        (
            first.local_addr().unwrap().port(),
            second.local_addr().unwrap().port(),
        )
    };
    let case = Case::with_settings(&local_model_settings(kept)).await;
    let controller = case.controller().await;
    let root = case.root.path().to_path_buf();
    for (name, port, first) in [("kept", kept, "refused"), ("other", other, "listened")] {
        let spec = ServerSpec::Stdio {
            command: fixture(&root),
            args: vec!["listen".into(), port.to_string()],
        };
        let server = controller
            .add_mcp_server(Scope::Global, name.into(), spec.clone(), vec![])
            .await
            .unwrap();
        controller
            .approve_mcp_server(server.id.clone(), spec)
            .await
            .unwrap();
        let checked = check(&controller, server.id.clone()).await.unwrap();
        assert_eq!(checked.problem, None, "{name}");
        assert_eq!(
            checked.tools,
            vec![first, "anonymous"],
            "{name} port {port}: a check listens only where the owner's model server does not"
        );
    }
    case.close().await;
}

/// OWNER-PORTS-FAIL-OPEN (rv-secreview IDE-6 note): a settings file that stops reading
/// after the engine started leaves the configured local model port unknown, so the check
/// is refused with `owner_ports_settings_unreadable`, shown in its own sentence, and never
/// listens on that port.
#[tokio::test]
async fn a_server_check_with_unreadable_settings_is_refused_not_run_without_kept_ports() {
    let kept = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let case = Case::with_settings(&local_model_settings(kept)).await;
    let controller = case.controller().await;
    let root = case.root.path().to_path_buf();
    let spec = ServerSpec::Stdio {
        command: fixture(&root),
        args: vec!["listen".into(), kept.to_string()],
    };
    let server = controller
        .add_mcp_server(Scope::Global, "kept".into(), spec.clone(), vec![])
        .await
        .unwrap();
    controller
        .approve_mcp_server(server.id.clone(), spec)
        .await
        .unwrap();
    fs::write(root.join("global.toml"), "[[[\n").unwrap();
    let checked = check(&controller, server.id.clone()).await.unwrap();
    assert!(
        !checked.tools.iter().any(|tool| tool == "listened"),
        "port {kept}: {:?}",
        checked.tools
    );
    assert_eq!(
        checked.problem.as_deref(),
        Some(
            "CrazyAgent can't read its settings file, so it did not run the check. Fix the file and check again."
        )
    );
    case.close().await;
}

/// A checked server cannot swap its own check folder for a link that would widen its next
/// check: the engine refuses a check folder that is not the plain folder it made.
#[tokio::test]
async fn a_server_check_folder_swapped_for_a_link_is_refused() {
    let case = Case::new().await;
    let controller = case.controller().await;
    let root = case.root.path().to_path_buf();
    let target = root.join("planted-target");
    fs::create_dir(&target).unwrap();
    let spec = ServerSpec::Stdio {
        command: fixture(&root),
        args: vec!["plant".into(), target.display().to_string()],
    };
    let server = controller
        .add_mcp_server(
            Scope::Global,
            "plant".into(),
            spec.clone(),
            vec![("FIXTURE_TOKEN".into(), secret("dummy-value"))],
        )
        .await
        .unwrap();
    controller
        .approve_mcp_server(server.id.clone(), spec)
        .await
        .unwrap();
    let first = check(&controller, server.id.clone()).await.unwrap();
    assert_eq!(first.tools, vec!["echo", "authorized"]);
    let home = root.join("mcp-checks").join(&server.id).join("home");
    #[cfg(target_os = "macos")]
    assert!(
        fs::symlink_metadata(&home).unwrap().is_dir(),
        "the check sandbox keeps the server from swapping its home for a link"
    );
    assert!(
        !target.join("planted").exists(),
        "the first check never writes outside its own folders"
    );
    if !fs::symlink_metadata(&home).unwrap().is_symlink() {
        fs::remove_dir_all(&home).unwrap();
        std::os::unix::fs::symlink(&target, &home).unwrap();
    }
    let second = check(&controller, server.id.clone()).await.unwrap();
    assert!(
        !target.join("planted").exists(),
        "a later check must not write through the link"
    );
    assert_eq!(second.problem.as_deref(), Some("The server can't start."));
    case.close().await;
}

fn stored_version(root: &Path, id: &str) -> Option<i64> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    let tables: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'versioned_items'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if tables == 0 {
        return None;
    }
    db.query_row(
        "SELECT version FROM versioned_items WHERE item = ?1",
        [format!("plugin:{id}")],
        |row| row.get(0),
    )
    .optional()
    .unwrap()
}

/// Returns the version the plugin screens show for one skill or server.
/// Inputs: the controller, the item id and the project whose view lists it, none for global.
/// Output: the item's current version.
async fn shown_version(controller: &OwnerController, id: &str, project: Option<ProjectId>) -> u64 {
    let skills = controller.plugin_skills(project.clone()).await.unwrap();
    if let Some(skill) = skills.iter().find(|skill| skill.id == id) {
        return skill.version;
    }
    let servers = controller.plugin_servers(project).await.unwrap();
    servers
        .iter()
        .find(|server| server.id == id)
        .expect("a listed plugin")
        .version
}

#[tokio::test]
async fn stale_skill_write_is_refused() {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "app").await;
    let root = case.root.path().to_path_buf();
    let skill = controller
        .add_skill(
            Scope::Global,
            skill_folder(&root, "release", "release-notes"),
        )
        .await
        .unwrap();
    assert_eq!(skill.version, 0);
    assert!(skill.enabled);

    let future = controller
        .set_skill_enabled(skill.id.clone(), false, 1)
        .await
        .unwrap_err();
    assert_eq!(future.0, "stale_revision");
    assert_eq!(stored_version(&root, &skill.id), None);
    let missing = controller
        .set_skill_enabled("skill-missing".into(), false, 7)
        .await
        .unwrap_err();
    assert_eq!(missing.0, "skill_not_found");

    let off = controller
        .set_skill_enabled(skill.id.clone(), false, 0)
        .await
        .unwrap();
    assert!(!off.enabled);
    assert_eq!(off.version, 1);
    assert_eq!(stored_version(&root, &skill.id), Some(1));
    let stale = controller
        .set_skill_enabled(skill.id.clone(), true, 0)
        .await
        .unwrap_err();
    assert_eq!(stale.0, "stale_revision");
    let listed = controller.plugin_skills(None).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].enabled);
    assert_eq!(listed[0].version, 1);

    let stale_switch = controller
        .switch_plugin(project.clone(), skill.id.clone(), false, 0)
        .await
        .unwrap_err();
    assert_eq!(stale_switch.0, "stale_revision");
    let here = controller
        .plugin_skills(Some(project.clone()))
        .await
        .unwrap();
    assert_eq!(here[0].on_here, Some(true));
    assert_eq!(here[0].version, 1);
    controller
        .switch_plugin(project.clone(), skill.id.clone(), false, 1)
        .await
        .unwrap();
    let here = controller
        .plugin_skills(Some(project.clone()))
        .await
        .unwrap();
    assert_eq!(here[0].on_here, Some(false));
    assert_eq!(here[0].version, 2);
    let on = controller
        .set_skill_enabled(skill.id.clone(), true, 2)
        .await
        .unwrap();
    assert!(on.enabled);
    assert_eq!(on.version, 3);

    let stale_remove = controller
        .remove_skill(skill.id.clone(), 2)
        .await
        .unwrap_err();
    assert_eq!(stale_remove.0, "stale_revision");
    let kept = controller.plugin_skills(None).await.unwrap();
    assert_eq!(kept.len(), 1);
    assert!(Path::new(&kept[0].folder).join("SKILL.md").is_file());
    assert_eq!(stored_version(&root, &skill.id), Some(3));
    controller.remove_skill(skill.id.clone(), 3).await.unwrap();
    assert_eq!(stored_version(&root, &skill.id), None);
    drop(controller);
    case.close().await;
}

#[tokio::test]
async fn stale_mcp_server_write_is_refused() {
    let case = Case::new().await;
    let controller = case.controller().await;
    let root = case.root.path().to_path_buf();
    let command = fixture(&root);
    let server = controller
        .add_mcp_server(
            Scope::Global,
            "fixture".into(),
            ServerSpec::Stdio {
                command: command.clone(),
                args: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(server.version, 0);
    let other = ServerSpec::Stdio {
        command: command.clone(),
        args: vec!["--other".into()],
    };

    let invalid = controller
        .update_mcp_server(
            server.id.clone(),
            ServerSpec::Http {
                url: "http://example.com/mcp".into(),
            },
            5,
        )
        .await
        .unwrap_err();
    assert_eq!(invalid.0, "server_url_invalid");
    let future = controller
        .update_mcp_server(server.id.clone(), other.clone(), 1)
        .await
        .unwrap_err();
    assert_eq!(future.0, "stale_revision");
    assert_eq!(stored_version(&root, &server.id), None);

    let changed = controller
        .update_mcp_server(server.id.clone(), other.clone(), 0)
        .await
        .unwrap();
    assert_eq!(changed.version, 1);
    assert_eq!(changed.runs, format!("{command} --other"));
    let stale_update = controller
        .update_mcp_server(
            server.id.clone(),
            ServerSpec::Stdio {
                command: command.clone(),
                args: vec![],
            },
            0,
        )
        .await
        .unwrap_err();
    assert_eq!(stale_update.0, "stale_revision");
    let stale_off = controller
        .set_mcp_server_enabled(server.id.clone(), false, 0)
        .await
        .unwrap_err();
    assert_eq!(stale_off.0, "stale_revision");
    let listed = controller.plugin_servers(None).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].runs, format!("{command} --other"));
    assert_ne!(listed[0].status_label, "Off");
    assert_eq!(listed[0].version, 1);

    let off = controller
        .set_mcp_server_enabled(server.id.clone(), false, 1)
        .await
        .unwrap();
    assert_eq!(off.status_label, "Off");
    assert_eq!(off.version, 2);
    assert_eq!(stored_version(&root, &server.id), Some(2));

    let stale_remove = controller
        .remove_mcp_server(server.id.clone(), 1)
        .await
        .unwrap_err();
    assert_eq!(stale_remove.0, "stale_revision");
    assert_eq!(controller.plugin_servers(None).await.unwrap().len(), 1);
    assert_eq!(stored_version(&root, &server.id), Some(2));
    controller
        .remove_mcp_server(server.id.clone(), 2)
        .await
        .unwrap();
    assert_eq!(stored_version(&root, &server.id), None);
    drop(controller);
    case.close().await;
}

#[tokio::test]
async fn tool_switches_round_trip_through_the_owner_socket() {
    use crazyagent::coord::tool_switches::{BUILTIN_TOOLS, Switch};
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "app").await;
    let root = case.root.path().to_path_buf();

    let builtins = controller.tool_switches(None, None).await.unwrap();
    assert_eq!(builtins.len(), BUILTIN_TOOLS.len());
    assert!(
        builtins
            .iter()
            .all(|view| view.builtin && view.global == Switch::On && view.here.is_none())
    );
    let off = controller
        .set_tool_switch(None, None, "start_work".into(), Switch::Off)
        .await
        .unwrap();
    let start = off.iter().find(|view| view.tool == "start_work").unwrap();
    assert_eq!(start.global, Switch::Off);
    let on = controller
        .set_tool_switch(Some(project.clone()), None, "start_work".into(), Switch::On)
        .await
        .unwrap();
    let start = on.iter().find(|view| view.tool == "start_work").unwrap();
    assert_eq!((start.global, start.here), (Switch::Off, Some(Switch::On)));

    second_client_reads(&case, &project).await;

    let restored = controller
        .restore_tool_switches(Some(project.clone()), None)
        .await
        .unwrap();
    assert!(restored.iter().all(|view| view.here.is_none()));
    let start = restored
        .iter()
        .find(|view| view.tool == "start_work")
        .unwrap();
    assert_eq!(start.global, Switch::Off);

    let server = controller
        .add_mcp_server(
            Scope::Global,
            "fixture".into(),
            ServerSpec::Stdio {
                command: fixture(&root),
                args: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    let checked = check(&controller, server.id.clone()).await.unwrap();
    assert_eq!(checked.tools, vec!["echo", "anonymous"]);
    let listed = controller
        .tool_switches(Some(project.clone()), Some(server.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|view| view.tool.as_str())
            .collect::<Vec<_>>(),
        ["echo", "anonymous"]
    );
    let echo_off = controller
        .set_tool_switch(
            Some(project.clone()),
            Some(server.id.clone()),
            "echo".into(),
            Switch::Off,
        )
        .await
        .unwrap();
    assert_eq!(echo_off[0].here, Some(Switch::Off));
    for (server_id, tool, switch, code) in [
        (
            Some(server.id.clone()),
            "echo",
            Switch::Hidden,
            "tool_delete_builtin_only",
        ),
        (None, "nope", Switch::Off, "tool_unknown"),
        (
            Some("Bad_Id".to_owned()),
            "echo",
            Switch::Off,
            "server_not_found",
        ),
        (
            Some("absent-1".to_owned()),
            "echo",
            Switch::Off,
            "server_not_found",
        ),
    ] {
        assert_eq!(
            controller
                .set_tool_switch(Some(project.clone()), server_id, tool.into(), switch)
                .await
                .unwrap_err()
                .0,
            code,
            "{tool}"
        );
        assert!(plain_problem(code).is_some(), "{code}");
    }
    case.close().await;
}

/// Reads the built-in tools of a project through a second owner client, so the switches the
/// first client set are stored engine state, not a client cache. Inputs: the case and project.
/// Output: none; panics when the project read differs.
async fn second_client_reads(case: &Case, project: &ProjectId) {
    use crazyagent::coord::tool_switches::Switch;
    let reader = case.controller().await;
    let views = reader
        .tool_switches(Some(project.clone()), None)
        .await
        .unwrap();
    let start = views.iter().find(|view| view.tool == "start_work").unwrap();
    assert_eq!((start.global, start.here), (Switch::Off, Some(Switch::On)));
}
