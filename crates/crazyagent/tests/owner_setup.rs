#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::config::{AccountPool, ModelAccessEdit, ProviderQuotaPolicy};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, OwnerClientKind, OwnerData, OwnerMutation, OwnerRead, OwnerResponse,
    OwnerResult,
};
use crazyagent::master_session::{MasterBackend, MasterOwnerAction};
use crazyagent::owner_profile::{AnswerDetail, Initiative, OwnerProfile, WorkingStyle};
use crazyagent::protocol::{CredentialRef, ProjectId, ProjectSpec};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;

const MIXED: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["codex-1", "codex-2", "claude-1", "api-1", "local-1"]
connections = [
  { id = "codex-a", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-1", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile-a" } },
  { id = "codex-b", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-2", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile-b" } },
  { id = "claude-a", provider = "anthropic", harness = "claude", protocol = "native", account_ref = "claude-1", endpoint = { kind = "native" }, model = { id = "native-default", author = "anthropic" }, auth = { method = "native_managed", profile_ref = "claude-profile-a" } },
  { id = "api-a", provider = "openrouter", harness = "rcli", protocol = "openai-chat", account_ref = "api-1", endpoint = { kind = "http", url = "https://openrouter.ai/api/v1" }, model = { id = "some/model", author = "openrouter" }, auth = { method = "api_key", credential_ref = "api-key-ref" } },
  { id = "local-a", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-1", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [
  { id = "pool-mixed", name = "Everything", members = ["claude-a", "api-a", "codex-a", "codex-b", "local-a"], projects = [], roles = [], fallback_allowed = false },
  { id = "pool-claude", members = ["claude-a"], projects = [], roles = [], fallback_allowed = false },
  { id = "pool-subscriptions", members = ["claude-a", "codex-b"], projects = [], roles = [], fallback_allowed = false },
]
"#;

struct Case {
    root: tempfile::TempDir,
    global: PathBuf,
    service: Option<EngineService>,
    stopped: watch::Receiver<bool>,
    _stop: watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-owner-setup-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, MIXED).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = watch::channel(false);
        let mut case = Self {
            root,
            global,
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
            &self.global,
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

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn applied(response: OwnerResponse) -> OwnerResult {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            operation.result
        }
        other => panic!("actual owner receipt required: {other:?}"),
    }
}

async fn edit(controller: &mut OwnerController, edit: ModelAccessEdit) -> OwnerResult {
    let reply = controller
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools,
            cursor: None,
            limit: 1,
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessPage { source_hash, .. } = &reply.data else {
        panic!("actual pool page required");
    };
    controller
        .begin_mutation(
            OwnerMutation::ModelAccess {
                expected_config_id: reply.config_id.clone(),
                expected_source_hash: source_hash.clone(),
                edit,
            },
            1,
        )
        .unwrap();
    applied(controller.send_pending().await.unwrap())
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
    assert_eq!(
        applied(controller.send_pending().await.unwrap()),
        OwnerResult::Applied
    );
    project
}

fn pool(id: &str, members: &[&str], projects: &[&str]) -> AccountPool {
    AccountPool {
        id: reference(id),
        name: None,
        members: members.iter().map(|m| reference(m)).collect(),
        projects: projects.iter().map(|p| reference(p)).collect(),
        roles: if projects.is_empty() {
            vec![]
        } else {
            vec![reference("project_leader")]
        },
        fallback_allowed: false,
        provider_quota_policy: ProviderQuotaPolicy::default(),
    }
}

fn profile() -> OwnerProfile {
    OwnerProfile {
        name: "Ada Lovelace".into(),
        role: "Founder".into(),
        bio: "Builds developer tools.".into(),
        preferred_stack: vec!["Rust".into(), "TypeScript".into()],
        working_style: WorkingStyle {
            detail: AnswerDetail::Terse,
            initiative: Initiative::AskFirst,
        },
    }
}

#[tokio::test]
async fn the_owner_profile_is_absent_on_first_run_then_saved_updated_and_durable() {
    let mut case = Case::new().await;
    let mut controller = case.controller().await;
    assert_eq!(controller.owner_profile().await.unwrap(), None);
    let saved = profile();
    assert_eq!(
        applied(
            controller
                .save_owner_profile(saved.clone(), 1)
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    assert_eq!(
        controller.owner_profile().await.unwrap(),
        Some(saved.clone())
    );
    let mut updated = saved.clone();
    updated.role = "CTO".into();
    updated.working_style.detail = AnswerDetail::Detailed;
    assert_eq!(
        applied(
            controller
                .save_owner_profile(updated.clone(), 2)
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    let mut blank = updated.clone();
    blank.name = " ".into();
    assert_eq!(
        controller.save_owner_profile(blank, 3).await.unwrap_err().0,
        "owner_profile_name"
    );
    drop(controller);
    case.restart().await;
    let mut controller = case.controller().await;
    assert_eq!(controller.owner_profile().await.unwrap(), Some(updated));
    drop(controller);
    case.close().await;
}

#[tokio::test]
async fn the_pool_list_reads_plainly_and_pool_limits_hold_in_the_engine() {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let pools = controller.pool_summaries().await.unwrap();
    let rows: Vec<(&str, &str)> = pools
        .iter()
        .map(|p| (p.name.as_str(), p.summary.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "Everything",
                "2 ChatGPT, 1 Claude, 1 API key, 1 Local model"
            ),
            ("pool-claude", "1 Claude"),
            ("pool-subscriptions", "1 ChatGPT, 1 Claude"),
        ]
    );
    let status: Vec<&str> = pools[0]
        .members
        .iter()
        .map(|m| m.status_label.as_str())
        .collect();
    assert_eq!(
        status,
        vec![
            "Not signed in",
            "Set up",
            "Not signed in",
            "Not signed in",
            "Set up"
        ]
    );
    let sixteen: Vec<String> = (0..16).map(|index| format!("x{index}")).collect();
    let mut too_big = pool("pool-mixed", &[], &[]);
    too_big.members = sixteen.iter().map(|m| reference(m)).collect();
    assert_eq!(
        edit(
            &mut controller,
            ModelAccessEdit::ReplacePool { pool: too_big }
        )
        .await,
        OwnerResult::Rejected {
            code: "pool_member_limit".into()
        }
    );
    assert_eq!(
        crazyagent::config::plain_problem("pool_member_limit"),
        Some("A pool can hold up to 15 accounts")
    );
    let project = create_project(&mut controller, "pool-project").await;
    let assigned = pool("pool-claude", &["claude-a"], &[project.as_str()]);
    assert_eq!(
        edit(
            &mut controller,
            ModelAccessEdit::ReplacePool { pool: assigned }
        )
        .await,
        OwnerResult::Applied
    );
    assert_eq!(
        edit(
            &mut controller,
            ModelAccessEdit::RemovePool {
                id: reference("pool-claude")
            }
        )
        .await,
        OwnerResult::Rejected {
            code: "pool_in_use".into()
        }
    );
    assert_eq!(
        edit(
            &mut controller,
            ModelAccessEdit::RemovePool {
                id: reference("pool-subscriptions")
            }
        )
        .await,
        OwnerResult::Applied
    );
    let after = controller.pool_summaries().await.unwrap();
    assert_eq!(after.len(), 2);
    assert_eq!(after[1].project_count, 1);
    drop(controller);
    case.close().await;
}

#[tokio::test]
async fn a_named_pool_is_renamed_and_its_mixed_members_change_durably() {
    let mut case = Case::new().await;
    let mut controller = case.controller().await;
    let mut team = pool("pool-team", &["codex-a", "api-a"], &[]);
    team.name = Some("Team".into());
    assert_eq!(
        edit(
            &mut controller,
            ModelAccessEdit::CreatePool { pool: team.clone() }
        )
        .await,
        OwnerResult::Applied
    );
    team.name = Some("Core".into());
    team.members = vec![
        reference("api-a"),
        reference("local-a"),
        reference("claude-a"),
    ];
    assert_eq!(
        edit(&mut controller, ModelAccessEdit::ReplacePool { pool: team }).await,
        OwnerResult::Applied
    );
    drop(controller);
    case.restart().await;
    let mut controller = case.controller().await;
    let pools = controller.pool_summaries().await.unwrap();
    let rows: Vec<(&str, &str, &str)> = pools
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str(), p.summary.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "pool-mixed",
                "Everything",
                "2 ChatGPT, 1 Claude, 1 API key, 1 Local model"
            ),
            ("pool-claude", "pool-claude", "1 Claude"),
            (
                "pool-subscriptions",
                "pool-subscriptions",
                "1 ChatGPT, 1 Claude"
            ),
            ("pool-team", "Core", "1 Claude, 1 API key, 1 Local model"),
        ]
    );
    let shared = pools
        .iter()
        .filter(|p| p.members.iter().any(|m| m.id.as_str() == "claude-a"))
        .count();
    assert_eq!(shared, 4);
    let mut blank = pool("pool-team", &["api-a"], &[]);
    blank.name = Some("x".repeat(65));
    assert_eq!(
        edit(
            &mut controller,
            ModelAccessEdit::ReplacePool { pool: blank }
        )
        .await,
        OwnerResult::Rejected {
            code: "pool_name_bounds".into()
        }
    );
    drop(controller);
    case.close().await;
}

async fn open_master(
    controller: &mut OwnerController,
    pool_id: &str,
    members: &[&str],
    project: &str,
) -> MasterOwnerAction {
    let project = create_project(controller, project).await;
    assert_eq!(
        edit(
            controller,
            ModelAccessEdit::ReplacePool {
                pool: pool(pool_id, members, &[project.as_str()])
            }
        )
        .await,
        OwnerResult::Applied
    );
    let action = crazyagent::tui::default_master_open(controller, &project)
        .await
        .unwrap();
    controller.read(OwnerRead::Capabilities).await.unwrap();
    controller
        .begin_mutation(
            OwnerMutation::ProjectMaster {
                action: action.clone(),
            },
            1,
        )
        .unwrap();
    assert_eq!(
        applied(controller.send_pending().await.unwrap()),
        OwnerResult::Applied
    );
    action
}

#[tokio::test]
async fn a_mixed_pool_assigned_to_a_project_selects_an_eligible_master_member() {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let mixed = ["claude-a", "api-a", "codex-a", "codex-b", "local-a"];
    let MasterOwnerAction::Open { selection, .. } =
        open_master(&mut controller, "pool-mixed", &mixed, "mixed-project").await
    else {
        panic!("open action required");
    };
    assert_eq!(selection.backend, MasterBackend::Rcli);
    assert_eq!(selection.pool_ref.as_str(), "pool-mixed");
    assert!(["api-1", "local-1"].contains(&selection.route.account_ref.as_str()));
    let MasterOwnerAction::Open { selection, .. } = open_master(
        &mut controller,
        "pool-subscriptions",
        &["claude-a", "codex-b"],
        "subscription-project",
    )
    .await
    else {
        panic!("open action required");
    };
    assert_eq!(selection.backend, MasterBackend::Codex);
    assert_eq!(selection.route.account_ref.as_str(), "codex-2");
    assert_eq!(selection.target_profile_ref.as_str(), "codex-b");
    drop(controller);
    case.close().await;
}

#[tokio::test]
async fn a_signed_in_style_claude_account_can_be_the_project_master() {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let MasterOwnerAction::Open { selection, .. } = open_master(
        &mut controller,
        "pool-claude",
        &["claude-a"],
        "claude-project",
    )
    .await
    else {
        panic!("open action required");
    };
    assert_eq!(selection.backend, MasterBackend::Claude);
    assert_eq!(selection.route.account_ref.as_str(), "claude-1");
    assert_eq!(selection.target_profile_ref.as_str(), "claude-a");
    drop(controller);
    case.close().await;
}
