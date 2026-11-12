#![cfg(target_os = "macos")]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::plugins::{Scope, ServerSpec, ServerStatus};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::sandbox::policy::{AgentNetwork, AgentPolicy};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tokio::sync::watch;

const SETTINGS: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n";
const FIXTURE: &str = include_str!("fixtures/mcp_network_fixture.py");
const UNWRITTEN_POLICY_VERSION: u64 = 0;

struct Case {
    root: tempfile::TempDir,
    service: Option<EngineService>,
    _stopped: watch::Receiver<bool>,
    _stop: watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-agent-network-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, SETTINGS).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &root.path().join("global.toml"),
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        Self {
            root,
            service: Some(service),
            _stopped: stopped,
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

async fn set_network(controller: &mut OwnerController, project: ProjectId, network: AgentNetwork) {
    controller.read(OwnerRead::Capabilities).await.unwrap();
    controller
        .begin_mutation(
            OwnerMutation::SetProjectAgentPolicy {
                project_id: project,
                version: UNWRITTEN_POLICY_VERSION,
                policy: AgentPolicy {
                    network,
                    ..AgentPolicy::default()
                },
            },
            2,
        )
        .unwrap();
    match controller.send_pending().await.unwrap() {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            assert_eq!(operation.result, OwnerResult::Applied)
        }
        other => panic!("actual policy receipt required: {other:?}"),
    }
}

fn fixture(root: &Path, port: u16) -> ServerSpec {
    let folder = root.join("servers");
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join("mcp_network_fixture.py");
    fs::write(&path, FIXTURE).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    ServerSpec::Stdio {
        command: path.display().to_string(),
        args: vec![port.to_string()],
    }
}

async fn checked_tools(network: AgentNetwork) -> (Vec<String>, ServerStatus, Option<String>) {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "app").await;
    set_network(&mut controller, project.clone(), network).await;
    let listener = crazyagent::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let spec = fixture(case.root.path(), port);
    let server = controller
        .add_mcp_server(
            Scope::Project {
                project_id: project.clone(),
            },
            "netcheck".into(),
            spec,
            vec![],
        )
        .await
        .unwrap();
    let checked = controller
        .check_mcp_server(server.id.clone())
        .await
        .unwrap();
    drop(listener);
    case.close().await;
    (checked.tools, checked.status, checked.problem)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_open_network_a_local_port_server_starts_and_checks_ok() {
    let (tools, _status, problem) = checked_tools(AgentNetwork::Open).await;
    assert_eq!(tools, vec!["connected"], "{problem:?}");
    assert_eq!(problem, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_https_only_a_local_port_server_is_refused_by_the_check_and_cannot_connect() {
    let (tools, _status, _problem) = checked_tools(AgentNetwork::Https).await;
    assert_eq!(tools, vec!["refused"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_network_a_server_reaches_nothing() {
    let (tools, _status, _problem) = checked_tools(AgentNetwork::None).await;
    assert_eq!(tools, vec!["refused"]);
}
