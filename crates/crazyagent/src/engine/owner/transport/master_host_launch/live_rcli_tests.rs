use crate::accounts::{AccountDraft, Provider, SignInMethod};
use crate::client_controller::OwnerController;
use crate::config::{
    AccountPool, ModelAccessEdit, ModelConnection, ModelEndpoint, ProviderQuotaPolicy,
};
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{
    AccountProfileOutcome, AccountProfileRequest, AccountSecret, ModelAccessCollection,
    ModelAccessRecord, NativeAuthScope, OwnerClientKind, OwnerData, OwnerMutation, OwnerRead,
    OwnerResponse, OwnerResult,
};
use crate::master_session::{
    MasterBackend, MasterOwnerAction, MasterSessionPhase, MasterTurnPhase, MasterTurnView,
};
use crate::project_setup::{HarnessSlot, ProjectSetup, TeamPlan};
use crate::protocol::{CredentialRef, ProjectId, ProjectSpec, RequestId, SessionId};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PROJECT: &str = "rcli-live";
const RCLI: &str = "CRAZYAGENT_RCLI";
const LOCAL_URL: &str = "http://127.0.0.1:11434/v1";
const MODEL: &str = "qwen2.5:0.5b";
const WAIT: Duration = Duration::from_secs(10);
const TURN: Duration = Duration::from_secs(300);
const ASK: &str = "Reply with only the word READY.";
const OPENAI_MODEL: &str = "gpt-4o-mini";

/// A new owner-only folder under `/tmp`. Input: its name's prefix. Output: the folder.
fn private_dir(prefix: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// The bundled RCLI host this run registers, as the engine finds it: the file
/// `CRAZYAGENT_RCLI` names, else `crazyagent-rcli` on PATH. Input: none. Output: its path,
/// or a panic with the plain BLOCKED message.
fn rcli_host() -> PathBuf {
    let host = crate::paths::RCLI_EXECUTABLE;
    std::env::var_os(RCLI)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH")?)
                .map(|folder| folder.join(host))
                .find(|path| path.is_file())
        })
        .filter(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "BLOCKED: no RCLI host: {host} is not on PATH and {RCLI} names no file; the app bundles it at /Applications/CrazyAgent.app/Contents/MacOS/{host}"
            )
        })
}

/// Checks that the engine recorded one owner change. Input: its reply. Output: none.
fn applied(response: OwnerResponse) {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            assert_eq!(operation.result, OwnerResult::Applied)
        }
        other => panic!("the owner change must be recorded: {other:?}"),
    }
}

/// Sends one owner change from a fresh view, as the app's clients do. Inputs: the
/// controller and the change. Output: none; it panics unless the engine applied it.
async fn change(controller: &mut OwnerController, mutation: OwnerMutation) {
    controller.read(OwnerRead::Capabilities).await.unwrap();
    controller.begin_mutation(mutation, 1).unwrap();
    applied(controller.send_pending().await.unwrap());
}

/// Creates the project's pool from one account, assigned to the project for its project
/// leader, as the owner does on the pools page. Inputs: the controller, the project and
/// the account. Output: none.
async fn assign_pool(
    controller: &mut OwnerController,
    project: &ProjectId,
    account: &CredentialRef,
) {
    let pools = controller
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools,
            cursor: None,
            limit: 1,
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessPage { source_hash, .. } = &pools.data else {
        panic!("pool page required");
    };
    change(
        controller,
        OwnerMutation::ModelAccess {
            expected_config_id: pools.config_id.clone(),
            expected_source_hash: source_hash.clone(),
            edit: ModelAccessEdit::CreatePool {
                pool: AccountPool {
                    id: CredentialRef::new("pool-rcli-live").unwrap(),
                    name: None,
                    members: vec![account.clone()],
                    projects: vec![CredentialRef::new(project.as_str()).unwrap()],
                    roles: vec![CredentialRef::new(crate::config::LEADER_ROLE).unwrap()],
                    fallback_allowed: false,
                    provider_quota_policy: ProviderQuotaPolicy::default(),
                },
            },
        },
    )
    .await;
}

/// Sets the pooled account up for agents as the app does once it is in a pool: the engine
/// generates the RCLI host's home for its keyless endpoint. Inputs: the controller and the
/// account. Output: none; it panics unless the engine configured it.
async fn ready_for_agents(controller: &mut OwnerController, account: &CredentialRef) {
    let record = controller
        .read(OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Connections,
            id: account.clone(),
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessRecord { source_hash, .. } = &record.data else {
        panic!("connection record required");
    };
    let request = AccountProfileRequest {
        request_id: RequestId::new("rcli-live-profile").unwrap(),
        scope: NativeAuthScope {
            expected_database_instance: controller.identity().database_instance().to_owned(),
            connection: account.clone(),
            expected_config_id: record.config_id.clone(),
            expected_source_hash: source_hash.clone(),
        },
    };
    let reply = controller
        .configure_account_profile(request, None, 1)
        .await
        .unwrap();
    assert!(
        matches!(reply.outcome, AccountProfileOutcome::ConfiguredOnly { .. }),
        "the local account is set up for agents: {:?}",
        reply.outcome
    );
}

/// The project's one master turn as the engine holds it. Inputs: the controller, the
/// project and the session. Output: the turn, or None before it is queued.
async fn turn(
    controller: &mut OwnerController,
    project: &ProjectId,
    session: &SessionId,
) -> Option<MasterTurnView> {
    let state = controller
        .read(OwnerRead::ProjectMasterTurns {
            project_id: project.clone(),
            session_id: session.clone(),
            cursor: None,
            limit: 1,
        })
        .await
        .unwrap();
    let OwnerData::ProjectMasterTurns { page } = state.data else {
        panic!("master turns read required");
    };
    page.turns.into_iter().next()
}

/// Plants the generated home a keyed account's failed setup left before DEC-150 addendum
/// 9a: exactly the engine's old config naming the builtin provider id, and no key.
/// Inputs: the engine root and the account's connection. Output: the home's path.
fn legacy_home(root: &Path, connection: &ModelConnection) -> PathBuf {
    let profile = crate::config::configured_profile_ref(connection).unwrap();
    let homes = root.join("configured-sdk-homes");
    let home = homes.join(format!("{:x}", Sha256::digest(profile.as_str().as_bytes())));
    let sdk = home.join(".backboard");
    std::fs::create_dir_all(&sdk).unwrap();
    for directory in [&homes, &home, &sdk] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let ModelEndpoint::Http { url } = &connection.endpoint else {
        panic!("a keyed RCLI account has an HTTP endpoint");
    };
    let config = sdk.join("config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "model": { "provider": connection.provider.as_str(), "model": connection.model.id },
            "providers": [{
                "id": connection.provider.as_str(), "name": "Configured provider",
                "protocol": connection.protocol.as_str(), "baseUrl": url,
                "auth": { "type": "apiKey" }
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    home
}

/// Sets the live project up with a team plan naming RCLI as its master and the given agents,
/// as the owner does in the app. Inputs: the controller, the project and its agents.
/// Output: none; it panics unless the engine applied it.
async fn rcli_team_project(
    controller: &mut OwnerController,
    project: &ProjectId,
    agents: &[HarnessSlot],
) {
    applied(
        controller
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "RCLI live".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Answers questions about a small notes app.".into(),
                    tech_stack: vec![],
                    platforms: vec![],
                    research: vec![],
                    pool: None,
                    team: Some(TeamPlan {
                        master: "rcli".into(),
                        master_tools: true,
                        sub_harnesses: agents.to_vec(),
                        min_agents: 1,
                        max_agents: 2,
                    }),
                    documents: vec![],
                },
                2,
            )
            .await
            .unwrap(),
    );
}

/// Opens the project's default master, checks it runs on RCLI, sends one question and waits
/// for that turn to end. Inputs: the controller and the project. Output: the ended turn.
async fn first_rcli_turn(controller: &mut OwnerController, project: &ProjectId) -> MasterTurnView {
    let open = crate::tui::default_master_open(controller, project)
        .await
        .unwrap();
    let MasterOwnerAction::Open { selection, .. } = &open else {
        panic!("open action required");
    };
    assert_eq!(
        selection.backend,
        MasterBackend::Rcli,
        "the default master runs on the team plan's RCLI"
    );
    change(controller, OwnerMutation::ProjectMaster { action: open }).await;
    let state = controller
        .read(OwnerRead::ProjectMaster {
            project_id: project.clone(),
        })
        .await
        .unwrap();
    let OwnerData::ProjectMaster {
        session: Some(session),
    } = state.data
    else {
        panic!("the default Open opened no master session");
    };
    assert_eq!(session.phase, MasterSessionPhase::Configured);
    assert_eq!(session.selection.backend, MasterBackend::Rcli);

    change(
        controller,
        OwnerMutation::ProjectMaster {
            action: MasterOwnerAction::Submit {
                project_id: project.clone(),
                session_id: session.session_id.clone(),
                text: ASK.into(),
            },
        },
    )
    .await;
    let deadline = Instant::now() + TURN;
    loop {
        let seen = turn(controller, project, &session.session_id).await;
        if let Some(turn) = seen.as_ref().filter(|turn| {
            matches!(
                turn.phase,
                MasterTurnPhase::Succeeded
                    | MasterTurnPhase::Failed
                    | MasterTurnPhase::Cancelled
                    | MasterTurnPhase::Uncertain
            )
        }) {
            break turn.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the RCLI master's turn ends within {} s: {:?}",
            TURN.as_secs(),
            seen.map(|turn| (turn.phase, turn.reason, turn.detail, turn.cause))
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// N-D30, live: a project whose team plan names RCLI as its master and Claude Code for its
/// agents, with a keyless local Ollama account set up for agents in its pool as the owner
/// does in the app, opens its default master on RCLI, and the bundled RCLI host answers a
/// real master turn with the local model's reply while the plan keeps its agents on Claude
/// Code. BLOCKED without the RCLI host or the local model server. Runs in a live window
/// only.
#[test]
#[ignore = "live qualification: needs crazyagent-rcli on PATH or named by CRAZYAGENT_RCLI, and ollama serving qwen2.5:0.5b"]
fn a_real_rcli_master_answers_a_turn_on_a_team_plan_project() {
    let rcli = rcli_host();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = private_dir("ca-live-rcli-");
    let home = private_dir("ca-live-rcli-home-");
    runtime.block_on(async {
        let global = root.path().join("g.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("p.toml"),
            &stopped,
        )
        .await
        .unwrap();
        service.register_rcli_executable(&rcli).unwrap();
        let mut controller = OwnerController::attach(root.path(), OwnerClientKind::Desktop, WAIT)
            .await
            .unwrap();
        let models = controller.local_models(LOCAL_URL.into()).await;
        if !models
            .as_ref()
            .is_ok_and(|models| models.iter().any(|model| model == MODEL))
        {
            panic!(
                "BLOCKED: no local model server: ollama must serve {MODEL} on 127.0.0.1:11434: {models:?}"
            );
        }
        let projects = home.path().join("Projects");
        std::fs::create_dir(&projects).unwrap();
        applied(
            controller
                .set_projects_home(projects.to_str().unwrap().into(), 1)
                .await
                .unwrap(),
        );
        let account = controller
            .add_account(
                AccountDraft {
                    provider: Provider::Local,
                    method: SignInMethod::Url,
                    name: "Local model".into(),
                    url: None,
                    model: Some(MODEL.into()),
                },
                None,
            )
            .await
            .unwrap()
            .id;
        let project = ProjectId::new(PROJECT).unwrap();
        let agents = vec![HarnessSlot {
            harness: "claude-code".into(),
            agents: 1,
            tools: true,
        }];
        rcli_team_project(&mut controller, &project, &agents).await;
        assign_pool(&mut controller, &project, &account).await;
        ready_for_agents(&mut controller, &account).await;

        let ended = first_rcli_turn(&mut controller, &project).await;
        println!(
            "real rcli master turn: phase={:?} reason={:?} detail={:?} cause={:?} reply={:?}",
            ended.phase,
            ended.reason,
            ended.detail,
            ended.cause,
            ended.reply.as_ref().map(|reply| &reply.text)
        );
        assert_eq!(ended.phase, MasterTurnPhase::Succeeded, "{:?}", ended.cause);
        assert!(
            ended
                .reply
                .as_ref()
                .is_some_and(|reply| !reply.text.trim().is_empty()),
            "the RCLI master answers with the local model's reply"
        );
        let setup = controller.project_setup(project.clone()).await.unwrap().unwrap();
        assert_eq!(
            (setup.team.master.as_str(), &setup.team.sub_harnesses),
            ("rcli", &agents),
            "the team plan keeps its agents on Claude Code"
        );
        drop(controller);
        tokio::time::timeout(WAIT, service.shutdown())
            .await
            .unwrap()
            .unwrap();
    });
}

/// KY-M1, live: a keyed OpenAI account saved before DEC-150 addendum 9a, pooled for a
/// project whose team plan names RCLI as its master, with the home its failed setup left
/// (the engine's old config, no key), launches its RCLI master on the first turn with no
/// owner action. The key is synthetic, so the provider refuses the turn after the launch.
/// BLOCKED without the RCLI host. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs crazyagent-rcli on PATH or named by CRAZYAGENT_RCLI, and sends a synthetic key to the OpenAI API"]
fn a_pooled_keyed_rcli_account_saved_before_the_builtin_id_fix_launches_its_master() {
    let rcli = rcli_host();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = private_dir("ca-live-rcli-keyed-");
    let home = private_dir("ca-live-rcli-keyed-home-");
    runtime.block_on(async {
        let global = root.path().join("g.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("p.toml"),
            &stopped,
        )
        .await
        .unwrap();
        service.register_rcli_executable(&rcli).unwrap();
        let mut controller = OwnerController::attach(root.path(), OwnerClientKind::Desktop, WAIT)
            .await
            .unwrap();
        let projects = home.path().join("Projects");
        std::fs::create_dir(&projects).unwrap();
        applied(
            controller
                .set_projects_home(projects.to_str().unwrap().into(), 1)
                .await
                .unwrap(),
        );
        let account = controller
            .add_account(
                AccountDraft {
                    provider: Provider::OpenaiApi,
                    method: SignInMethod::ApiKey,
                    name: "OpenAI key".into(),
                    url: None,
                    model: Some(OPENAI_MODEL.into()),
                },
                Some(AccountSecret::new(b"synthetic-key-not-a-real-key".to_vec()).unwrap()),
            )
            .await
            .unwrap()
            .id;
        let record = controller
            .read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: account.clone(),
            })
            .await
            .unwrap();
        let OwnerData::ModelAccessRecord {
            record: ModelAccessRecord::Connection { connection },
            ..
        } = record.data
        else {
            panic!("connection record required");
        };
        let generated = legacy_home(root.path(), &connection);
        let project = ProjectId::new(PROJECT).unwrap();
        let agents = vec![HarnessSlot {
            harness: "claude-code".into(),
            agents: 1,
            tools: true,
        }];
        rcli_team_project(&mut controller, &project, &agents).await;
        assign_pool(&mut controller, &project, &account).await;

        let ended = first_rcli_turn(&mut controller, &project).await;
        println!(
            "pre-fix keyed rcli master turn: phase={:?} reason={:?} cause={:?} launched={}",
            ended.phase,
            ended.reason,
            ended.cause,
            ended.runtime.is_some()
        );
        assert!(
            !matches!(
                ended.cause.as_deref(),
                Some(
                    "rcli_master_key_setup_required"
                        | "rcli_master_profile_mismatch"
                        | "rcli_profile_configuration_invalid"
                        | "native_profile_custody_unsafe"
                )
            ),
            "the engine sets the pre-fix account up before the launch: {:?}",
            ended.cause
        );
        assert!(
            ended.runtime.is_some(),
            "the RCLI master launches: {:?}",
            (ended.phase, ended.cause)
        );
        assert!(
            generated.join(".backboard").join("keys.json").is_file(),
            "the engine wrote the saved key into the account's generated home"
        );
        drop(controller);
        tokio::time::timeout(WAIT, service.shutdown())
            .await
            .unwrap()
            .unwrap();
    });
}
