use super::*;
use crate::config::{AccountPool, ModelAccessEdit};
use crate::engine::bootstrap::EngineService;
use crate::master_session::{
    MasterBackend, MasterOwnerAction, MasterReason, MasterSelection, MasterSessionView,
    MasterTurnPhase, MasterTurnView,
};
use crate::routing::{ResourceBudget, RouteRequest};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tokio::sync::watch;

const SETTINGS: &str = r#"
version = 1
timeout_ms = 2000
allowed_destinations = @DESTINATIONS@
[routing]
approved_accounts = @ACCOUNTS@
[routing.global_capacity]
slots = 1
cpu_millis = 1000
memory_mib = 512
processes = 4
output_bytes = 1048576
wall_time_ms = 60000
spend_microunits = 0
[routing.project_capacity]
slots = 1
cpu_millis = 1000
memory_mib = 512
processes = 4
output_bytes = 1048576
wall_time_ms = 60000
spend_microunits = 0
[routing.task_capacity]
slots = 1
cpu_millis = 1000
memory_mib = 512
processes = 4
output_bytes = 1048576
wall_time_ms = 60000
spend_microunits = 0
[model_access]
accounts = @ACCOUNTS@
[[model_access.connections]]
id = "claude-connection"
provider = "anthropic"
harness = "claude-code"
protocol = "native"
account_ref = "claude-account"
endpoint = { kind = "native" }
model = { id = "configured-claude-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "claude-profile" }
[[model_access.connections]]
id = "second-connection"
provider = "anthropic"
harness = "claude-code"
protocol = "native"
account_ref = "second-account"
endpoint = { kind = "native" }
model = { id = "configured-claude-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "second-profile" }
"#;

const SPARE: &str = r#"
[[model_access.connections]]
id = "spare-connection"
provider = "anthropic"
harness = "claude-code"
protocol = "native"
account_ref = "spare-account"
endpoint = { kind = "native" }
model = { id = "configured-claude-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "spare-profile" }
"#;

const POOL: &str = r#"
[[model_access.pools]]
id = "master-pool"
members = ["claude-connection", "second-connection"]
projects = ["p"]
roles = ["project_leader"]
fallback_allowed = false
"#;

const OTHER_POOL: &str = r#"
[[model_access.pools]]
id = "other-pool"
members = ["spare-connection"]
projects = ["q"]
roles = ["project_leader"]
fallback_allowed = false
"#;

const SANDBOX: [&str; 2] = ["claude-connection", "second-connection"];

/// Builds the settings file as the engine keeps it: a master pool of two Claude
/// connections, optionally a spare account in another project's pool (approved, with its
/// own target), and the given master sandbox destinations.
/// Input: whether the spare account is present and the destinations. Output: TOML.
fn settings(spare: bool, sandbox: &[&str]) -> String {
    let mut accounts = vec!["claude-account", "second-account"];
    let mut destinations = sandbox.to_vec();
    if spare {
        accounts.push("spare-account");
        destinations.push("spare-connection");
    }
    let list = |items: &[&str]| {
        let quoted: Vec<String> = items.iter().map(|item| format!("\"{item}\"")).collect();
        format!("[{}]", quoted.join(", "))
    };
    let head = SETTINGS
        .replace("@DESTINATIONS@", &list(&destinations))
        .replace("@ACCOUNTS@", &list(&accounts));
    let spare = if spare {
        [SPARE, POOL, OTHER_POOL].concat()
    } else {
        POOL.into()
    };
    [head, spare].concat()
}

fn request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

fn result(reply: &OwnerReply) -> OwnerResult {
    let OwnerResponse::Applied { operation } = &reply.response else {
        panic!("actual owner receipt missing: {reply:?}");
    };
    operation.result.clone()
}

fn conflict() -> OwnerResult {
    OwnerResult::Rejected {
        code: "master_source_conflict".into(),
    }
}

struct RebindCase {
    root: tempfile::TempDir,
    global: PathBuf,
    service: EngineService,
    owner: OwnerClient,
    _stop: watch::Sender<bool>,
}

impl RebindCase {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-mrb-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        write_settings(&global, &settings(true, &SANDBOX));
        let (service, owner, stop) = launch(root.path(), &global).await;
        let case = Self {
            root,
            global,
            service,
            owner,
            _stop: stop,
        };
        let create = request(
            "rebind-project",
            case.revision().await,
            OwnerOperation::Apply {
                mutation: OwnerMutation::CreateProject {
                    project_id: ProjectId::new("p").unwrap(),
                    spec: ProjectSpec {
                        name: "rebind project".into(),
                        repositories: vec![],
                    },
                },
            },
        );
        let created = case.owner.request(create).await.unwrap();
        assert_eq!(result(&created), OwnerResult::Applied);
        case
    }

    async fn read(&self, query: OwnerRead) -> OwnerReadReply {
        let reply = self
            .owner
            .request(request("rebind-read", 0, OwnerOperation::Read { query }))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner read failed: {reply:?}");
        };
        state
    }

    async fn revision(&self) -> u64 {
        self.read(OwnerRead::Capabilities).await.revision.value()
    }

    async fn basis(&self) -> (ConfigId, String) {
        let basis = self
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 16,
            })
            .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("actual model source missing");
        };
        (basis.config_id, source_hash)
    }

    async fn master(&self, id: &str, action: MasterOwnerAction) -> OwnerResult {
        let action = request(
            id,
            self.revision().await,
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster { action },
            },
        );
        result(&self.owner.request(action).await.unwrap())
    }

    async fn open(&self, id: &str) -> MasterSessionView {
        let (config, source) = self.basis().await;
        let target = CredentialRef::new("claude-connection").unwrap();
        let opened = self
            .master(
                id,
                MasterOwnerAction::Open {
                    project_id: ProjectId::new("p").unwrap(),
                    expected_config_id: config,
                    expected_source_hash: source,
                    selection: Box::new(MasterSelection {
                        backend: MasterBackend::Claude,
                        pool_ref: CredentialRef::new("master-pool").unwrap(),
                        target_profile_ref: target.clone(),
                        route: RouteRequest {
                            preferred_target: Some(target.clone()),
                            required_target: Some(target),
                            required_capabilities: vec!["planning".into(), "engine-tools".into()],
                            account_ref: CredentialRef::new("claude-account").unwrap(),
                            budget_ref: CredentialRef::new("planning-budget").unwrap(),
                            resources: ResourceBudget::unit(),
                        },
                    }),
                },
            )
            .await;
        assert_eq!(opened, OwnerResult::Applied);
        self.session().await
    }

    async fn session(&self) -> MasterSessionView {
        let state = self
            .read(OwnerRead::ProjectMaster {
                project_id: ProjectId::new("p").unwrap(),
            })
            .await;
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("actual master session missing");
        };
        *session
    }

    async fn submit(&self, session: &MasterSessionView, id: &str) -> OwnerResult {
        self.master(
            id,
            MasterOwnerAction::Submit {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                text: "Real synthetic input after a settings change.".into(),
            },
        )
        .await
    }

    async fn close(&self, session: &MasterSessionView, id: &str) -> OwnerResult {
        self.master(
            id,
            MasterOwnerAction::Close {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
            },
        )
        .await
    }

    async fn edit(&self, id: &str, edit: ModelAccessEdit) -> OwnerResult {
        let (config, source) = self.basis().await;
        let change = request(
            id,
            self.revision().await,
            OwnerOperation::Apply {
                mutation: OwnerMutation::ModelAccess {
                    expected_config_id: config,
                    expected_source_hash: source,
                    edit,
                },
            },
        );
        result(&self.owner.request(change).await.unwrap())
    }

    async fn remove_spare(&self, id: &str) -> [OwnerResult; 3] {
        let unassigned = self
            .edit(
                &format!("{id}-unassign"),
                ModelAccessEdit::ReplacePool {
                    pool: AccountPool {
                        id: reference("other-pool"),
                        name: None,
                        members: vec![reference("spare-connection")],
                        projects: vec![],
                        roles: vec![reference("project_leader")],
                        fallback_allowed: false,
                        provider_quota_policy: Default::default(),
                    },
                },
            )
            .await;
        let deleted = self
            .edit(
                &format!("{id}-delete-pool"),
                ModelAccessEdit::RemovePool {
                    id: reference("other-pool"),
                },
            )
            .await;
        let removed = self
            .edit(
                &format!("{id}-remove-account"),
                ModelAccessEdit::RemoveConnectedAccount {
                    id: reference("spare-connection"),
                },
            )
            .await;
        [unassigned, deleted, removed]
    }

    fn destinations(&self) -> Vec<CredentialRef> {
        let body = std::fs::read_to_string(&self.global).unwrap();
        let config: toml_edit::DocumentMut = body.parse().unwrap();
        config["allowed_destinations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| reference(value.as_str().unwrap()))
            .collect()
    }

    async fn turns(&self, session: &MasterSessionView) -> Vec<MasterTurnView> {
        let state = self
            .read(OwnerRead::ProjectMasterTurns {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                cursor: None,
                limit: 32,
            })
            .await;
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("actual turn page missing");
        };
        page.turns
    }

    async fn settled(&self, session: &MasterSessionView, id: &str) -> Option<MasterTurnView> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let turn = self
                    .turns(session)
                    .await
                    .into_iter()
                    .find(|turn| turn.id.as_str() == id);
                if let Some(turn) = turn
                    && turn.phase == MasterTurnPhase::Blocked
                {
                    return turn;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .ok()
    }

    async fn restart(&mut self, next: &str) {
        self.stop().await.unwrap();
        write_settings(&self.global, next);
        let (service, owner, stop) = launch(self.root.path(), &self.global).await;
        self.service = service;
        self.owner = owner;
        self._stop = stop;
    }

    async fn stop(&mut self) -> Result<(), EngineError> {
        tokio::time::timeout(Duration::from_secs(5), self.service.shutdown())
            .await
            .expect("actual owner shutdown exceeded five seconds")
    }
}

fn write_settings(global: &Path, body: &str) {
    std::fs::write(global, body).unwrap();
    std::fs::set_permissions(global, std::fs::Permissions::from_mode(0o600)).unwrap();
}

async fn launch(root: &Path, global: &Path) -> (EngineService, OwnerClient, watch::Sender<bool>) {
    let (stop, stopped) = watch::channel(false);
    let service = EngineService::start_with_settings_sources(
        root,
        "e.sock",
        global,
        &root.join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let owner = OwnerClient::discover(root, OwnerClientKind::Tui, Duration::from_secs(3))
        .await
        .unwrap()
        .0;
    (service, owner, stop)
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

#[tokio::test]
async fn a_send_after_an_account_outside_the_master_pool_is_removed_continues_the_same_conversation()
 {
    let mut case = RebindCase::start().await;
    let session = case.open("rebind-open").await;
    let first = case.submit(&session, "rebind-first").await;
    let first_turn = case.settled(&session, "rebind-first").await;
    let (before, _) = case.basis().await;
    let targets = case.destinations();
    let removed = case.remove_spare("rebind").await;
    let (after, _) = case.basis().await;
    let remaining = case.destinations();
    let second = case.submit(&session, "rebind-second").await;
    let second_turn = case.settled(&session, "rebind-second").await;
    let added = case
        .edit(
            "rebind-add-extra",
            ModelAccessEdit::AddAccount {
                account_ref: reference("extra-account"),
            },
        )
        .await;
    let third = case.submit(&session, "rebind-third").await;
    let third_turn = case.settled(&session, "rebind-third").await;
    let current = case.session().await;
    let turns = case.turns(&session).await;
    case.stop().await.unwrap();
    assert_eq!(first, OwnerResult::Applied);
    assert_eq!(
        first_turn.as_ref().and_then(|turn| turn.reason),
        Some(MasterReason::ReadinessUnknown)
    );
    assert_eq!(
        removed,
        [
            OwnerResult::Applied,
            OwnerResult::Applied,
            OwnerResult::Applied
        ]
    );
    assert_ne!(
        before, after,
        "removing the spare account changes the settings"
    );
    assert!(targets.contains(&reference("spare-connection")));
    assert!(
        !remaining.contains(&reference("spare-connection")),
        "the engine drops the removed account's own target with it"
    );
    assert_eq!(
        second,
        OwnerResult::Applied,
        "an account outside the master's pool does not end the conversation"
    );
    assert_eq!(
        second_turn.and_then(|turn| turn.reason),
        Some(MasterReason::ReadinessUnknown),
        "the turn reaches the sign-in check under the new settings, not a source conflict"
    );
    assert_eq!(added, OwnerResult::Applied);
    assert_eq!(third, OwnerResult::Applied);
    assert_eq!(
        third_turn.and_then(|turn| turn.reason),
        Some(MasterReason::ReadinessUnknown)
    );
    assert_eq!(current.session_id, session.session_id);
    assert_eq!(current.config_id, session.config_id);
    assert_eq!(current.rebound.len(), 2);
    assert_eq!(current.rebound[0].config_id, after);
    assert_eq!(turns.len(), 3);
}

#[tokio::test]
async fn a_send_after_a_master_pool_member_is_removed_asks_for_a_new_conversation() {
    let mut case = RebindCase::start().await;
    let session = case.open("pool-open").await;
    let narrowed = case
        .edit(
            "pool-narrow",
            ModelAccessEdit::ReplacePool {
                pool: AccountPool {
                    id: reference("master-pool"),
                    name: None,
                    members: vec![reference("claude-connection")],
                    projects: vec![reference("p")],
                    roles: vec![reference("project_leader")],
                    fallback_allowed: false,
                    provider_quota_policy: Default::default(),
                },
            },
        )
        .await;
    let removed = case
        .edit(
            "pool-remove-second",
            ModelAccessEdit::RemoveConnectedAccount {
                id: reference("second-connection"),
            },
        )
        .await;
    let refused = case.submit(&session, "pool-stale-send").await;
    let ended = case.close(&session, "pool-close").await;
    let fresh = case.open("pool-reopen").await;
    let sent = case.submit(&fresh, "pool-fresh-send").await;
    case.stop().await.unwrap();
    assert_eq!(narrowed, OwnerResult::Applied);
    assert_eq!(removed, OwnerResult::Applied);
    assert_eq!(refused, conflict());
    assert_eq!(ended, OwnerResult::Applied);
    assert_ne!(fresh.session_id, session.session_id);
    assert!(fresh.rebound.is_empty());
    assert_eq!(sent, OwnerResult::Applied);
}

#[tokio::test]
async fn a_send_after_the_sandbox_changes_asks_for_a_new_conversation() {
    let mut case = RebindCase::start().await;
    let session = case.open("sandbox-open").await;
    case.restart(&settings(true, &["second-connection"])).await;
    let refused = case.submit(&session, "sandbox-stale-send").await;
    let ended = case.close(&session, "sandbox-close").await;
    let (config, source) = case.basis().await;
    let reopened = case
        .master(
            "sandbox-reopen",
            MasterOwnerAction::Open {
                project_id: session.project_id.clone(),
                expected_config_id: config,
                expected_source_hash: source,
                selection: Box::new(session.selection.clone()),
            },
        )
        .await;
    case.stop().await.unwrap();
    assert_eq!(refused, conflict());
    assert_eq!(ended, OwnerResult::Applied);
    assert_eq!(
        reopened,
        OwnerResult::Rejected {
            code: "master_scope_or_budget_denied".into()
        },
        "a new conversation names the lost sandbox instead of running without it"
    );
}

#[tokio::test]
async fn a_send_after_a_restart_asks_for_a_new_conversation_even_when_only_settings_outside_it_changed()
 {
    let mut case = RebindCase::start().await;
    let session = case.open("approval-open").await;
    let added = case
        .edit(
            "approval-add-extra",
            ModelAccessEdit::AddAccount {
                account_ref: reference("extra-account"),
            },
        )
        .await;
    case.restart(&settings(false, &SANDBOX)).await;
    let sent = case.submit(&session, "approval-send").await;
    let current = case.session().await;
    case.stop().await.unwrap();
    assert_eq!(added, OwnerResult::Applied);
    assert_eq!(
        sent,
        conflict(),
        "a conversation opened before a restart never continues, even when the master's settings match"
    );
    assert_eq!(current.session_id, session.session_id);
    assert!(current.rebound.is_empty());
}

#[tokio::test]
async fn a_conversation_rebound_as_often_as_allowed_then_asks_for_a_new_one() {
    let mut case = RebindCase::start().await;
    let session = case.open("limit-open").await;
    let mut results = Vec::new();
    for round in 0..=crate::master_session::REBINDS_MAX {
        let added = case
            .edit(
                &format!("limit-add-{round}"),
                ModelAccessEdit::AddAccount {
                    account_ref: reference(&format!("extra-account-{round}")),
                },
            )
            .await;
        assert_eq!(added, OwnerResult::Applied);
        let id = format!("limit-send-{round}");
        let sent = case.submit(&session, &id).await;
        if sent == OwnerResult::Applied {
            assert!(case.settled(&session, &id).await.is_some());
        }
        results.push(sent);
    }
    let current = case.session().await;
    case.stop().await.unwrap();
    let (last, kept) = results.split_last().unwrap();
    assert!(kept.iter().all(|sent| *sent == OwnerResult::Applied));
    assert_eq!(*last, conflict());
    assert_eq!(current.rebound.len(), crate::master_session::REBINDS_MAX);
}

#[tokio::test]
async fn a_rebound_session_keeps_its_bindings_in_order_and_admits_only_its_own_attempts() {
    let mut case = RebindCase::start().await;
    let session = case.open("record-open").await;
    let removed = case.remove_spare("record").await;
    let sent = case.submit(&session, "record-send").await;
    let current = case.session().await;
    case.stop().await.unwrap();
    assert_eq!(
        removed,
        [
            OwnerResult::Applied,
            OwnerResult::Applied,
            OwnerResult::Applied
        ]
    );
    assert_eq!(sent, OwnerResult::Applied);
    assert_eq!(current.rebound.len(), 1);
    let now = current.rebound[0].clone();
    assert!(current.rebinds_valid());
    assert_eq!(
        current.binding(),
        (&now.config_id, now.source_hash.as_str())
    );
    assert!(now.at.value() > session.version.value());
    assert!(now.at.value() <= current.version.value());
    assert!(current.admits_attempt(&now.config_id, true));
    assert!(current.admits_attempt(&now.config_id, false));
    assert!(
        current.admits_attempt(&session.config_id, false),
        "a turn finished before the change stays readable"
    );
    assert!(
        !current.admits_attempt(&session.config_id, true),
        "a live turn runs only under the current settings"
    );
    assert!(!current.admits_attempt(&ConfigId::new("0".repeat(64)).unwrap(), false));
    let tampered = |change: &dyn Fn(&mut MasterSessionView)| {
        let mut copy = current.clone();
        change(&mut copy);
        copy.rebinds_valid()
    };
    assert!(!tampered(
        &|copy| copy.selection.backend = MasterBackend::Rcli
    ));
    assert!(!tampered(
        &|copy| copy.rebound[0].at = copy.owner.revision_after.clone()
    ));
    assert!(!tampered(
        &|copy| copy.rebound[0].at = Cursor::new(copy.version.value() + 1)
    ));
    assert!(!tampered(&|copy| copy
        .rebound
        .push(copy.rebound[0].clone())));
    assert!(!tampered(
        &|copy| copy.rebound[0].source_hash = "not-a-hash".into()
    ));
    assert!(!tampered(&|copy| {
        copy.rebound[0].config_id = copy.config_id.clone();
        copy.rebound[0].source_hash = copy.source_hash.clone();
    }));
    assert!(!tampered(&|copy| {
        copy.rebound = vec![now.clone(); crate::master_session::REBINDS_MAX + 1]
    }));
}
