use super::usage_tests::{IN_FLIGHT, Usage};
use super::*;
use crate::client_controller::OwnerController;
use crate::plugins::{Scope, ServerSpec};
use crate::projects_home::AddMode;
use crate::protocol::ProjectId;

const SWEPT_FILES: usize = 40_000;
const PATIENT_WAIT_MS: u64 = 30_000;
const SWEEP_TIME_MAX: Duration = Duration::from_secs(60);

/// The owner control that revokes one client kind at its current epoch.
/// Inputs: the runtime, the client kind and the operation name. Output: the control.
fn revocation(runtime: &OwnerRuntimeHandle, target: OwnerClientKind, name: &str) -> OwnerControl {
    OwnerControl {
        operation_id: RequestId::new(name).unwrap(),
        target,
        expected_epoch: runtime.epoch(target).unwrap(),
        decision: OwnerDecision::Revoke,
    }
}

/// Runs git for a test repository with no user or system configuration.
/// Inputs: the home folder, the repository folder and the arguments.
fn git(home: &Path, dir: &Path, args: &[&str]) {
    let output = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
}

/// Opens one real pending change set: an owner repository, an agent worktree with one new
/// file and the turn that recorded it. Input: the engine.
/// Output: the change set and its head.
fn pending_change_set(owner: &Usage) -> (crate::landing::ChangeSetId, String) {
    let root = owner.root.path();
    let (repo, home, worktree) = (root.join("repo"), root.join("home"), root.join("agent-1"));
    for folder in [&repo, &home] {
        std::fs::create_dir(folder).unwrap();
    }
    git(&home, &repo, &["init", "-q", "--template=", "-b", "main"]);
    std::fs::write(repo.join("README"), "one\n").unwrap();
    git(&home, &repo, &["add", "README"]);
    git(
        &home,
        &repo,
        &[
            "-c",
            "user.name=O",
            "-c",
            "user.email=o@example.invalid",
            "commit",
            "-q",
            "-m",
            "Start",
        ],
    );
    git(
        &home,
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    std::fs::write(worktree.join("NEW"), "new\n").unwrap();
    let db = crate::landing::open(&owner.engine_root).unwrap();
    let workspace = crate::landing::Workspace {
        project: ProjectId::new("project-1").unwrap(),
        agent: crate::landing::ChangeAgent {
            kind: crate::landing::AgentKind::Sub,
            id: "agent-1".into(),
            label: "Agent 1".into(),
        },
        repository: repo.clone(),
        worktree,
        git_dir: repo.join(".git/worktrees/agent-1"),
    };
    let turn = crate::landing::TurnEnd {
        outcome: crate::landing::TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Add a file".into(),
    };
    let id = crate::landing::record_turn_end(&db, &workspace, &turn)
        .unwrap()
        .unwrap();
    let head = crate::landing::list(&db, &workspace.project).unwrap()[0]
        .head
        .clone();
    (id, head)
}

/// Keeps the storage actor busy with a real sweep of many removed skill files.
/// Input: the engine. Output: the swept folder and the sweep's answer.
fn occupy_storage(owner: &Usage) -> (PathBuf, oneshot::Receiver<Result<(), EngineError>>) {
    let swept = owner
        .engine_root
        .join(crate::storage::plugins::STORE)
        .join("skills")
        .join("global")
        .join(".removed-busy-storage");
    std::fs::create_dir_all(&swept).unwrap();
    for index in 0..SWEPT_FILES {
        std::fs::write(swept.join(index.to_string()), b"x").unwrap();
    }
    let (send, sweep) = oneshot::channel();
    owner
        .runtime()
        .engine
        .storage
        .sender
        .try_send(crate::storage::Job::Setup(
            crate::storage::setup::Command::Sweep,
            send,
        ))
        .unwrap();
    (swept, sweep)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cleanup_removal_runs_without_the_owner_barrier_and_a_revoked_client_removes_nothing_after()
 {
    let owner = Usage::start().await;
    let runtime = owner.runtime();
    let held = crate::landing::open(&owner.engine_root).unwrap();
    crate::team::initialize(&held).unwrap();
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    let project = ProjectId::new("project-1").unwrap();
    let mut removal = Box::pin(
        owner
            .client
            .remove_finished_agents(project.clone(), vec![7]),
    );
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut removal).await.is_err(),
        "the removal waits behind the held write"
    );
    assert!(
        runtime.engine.owner_egress.try_write().is_ok(),
        "a removal in flight leaves the owner barrier to other requests and revocations"
    );
    held.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        removal.await.unwrap_err().0,
        "cleanup_agent_not_found",
        "the removal answers its own outcome"
    );
    drop(held);

    let revoked = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.control(revocation(
            runtime,
            OwnerClientKind::Tui,
            "revoke-after-cleanup",
        )),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!revoked.active);
    assert!(
        owner
            .client
            .remove_finished_agents(project.clone(), vec![7])
            .await
            .is_err(),
        "the revoked client removes nothing"
    );
    assert!(
        owner.client.finished_agents(project).await.is_err(),
        "the revoked client reads no more finished agents"
    );
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_set_discard_runs_without_the_owner_barrier_and_a_revoked_client_changes_nothing_after()
 {
    let owner = Usage::start().await;
    let runtime = owner.runtime();
    let (id, head) = pending_change_set(&owner);
    let held = crate::landing::open(&owner.engine_root).unwrap();
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut discard = Box::pin(owner.client.discard_changes(id.clone(), head));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut discard).await.is_err(),
        "the discard waits behind the held write"
    );
    assert!(
        runtime.engine.owner_egress.try_write().is_ok(),
        "a discard in flight leaves the owner barrier to other requests and revocations"
    );
    held.execute_batch("ROLLBACK").unwrap();
    discard
        .await
        .unwrap_or_else(|error| panic!("the held discard answered {}", error.0));
    drop(held);

    let revoked = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.control(revocation(
            runtime,
            OwnerClientKind::Tui,
            "revoke-after-discard",
        )),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!revoked.active);
    assert!(
        owner.client.restore_changes(id.clone()).await.is_err(),
        "the revoked client restores nothing"
    );
    let db = crate::landing::open(&owner.engine_root).unwrap();
    let project = ProjectId::new("project-1").unwrap();
    let state = crate::landing::list(&db, &project)
        .unwrap()
        .into_iter()
        .find(|set| set.id == id)
        .map(|set| set.state);
    assert_eq!(
        state,
        Some(crate::landing::ChangeState::Discarded),
        "the refused restore left the change set discarded"
    );
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_waits_for_a_plugin_change_in_flight_and_refuses_the_next() {
    let owner = Usage::start_waiting(PATIENT_WAIT_MS).await;
    let runtime = owner.runtime();
    let (swept, sweep) = occupy_storage(&owner);
    let mut added = Box::pin(owner.client.add_mcp_server(
        Scope::Global,
        "held-server".into(),
        ServerSpec::Stdio {
            command: "/usr/bin/true".into(),
            args: Vec::new(),
        },
    ));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut added).await.is_err(),
        "the change waits behind the busy storage"
    );
    assert!(
        runtime.engine.owner_egress.try_write().is_err(),
        "the change holds the owner barrier while it waits"
    );
    let mut revoke = Box::pin(runtime.control(revocation(
        runtime,
        OwnerClientKind::Tui,
        "revoke-during-plugin-change",
    )));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut revoke).await.is_err(),
        "a revocation waits for the change already in flight"
    );
    let server = tokio::time::timeout(SWEEP_TIME_MAX, added)
        .await
        .unwrap()
        .unwrap_or_else(|error| panic!("the held change answered {}", error.0));
    assert!(
        !tokio::time::timeout(SWEEP_TIME_MAX, revoke)
            .await
            .unwrap()
            .unwrap()
            .active
    );
    tokio::time::timeout(SWEEP_TIME_MAX, sweep)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!swept.exists());

    assert!(
        owner.client.plugin_server(server.id).await.is_err(),
        "the revoked client reads no more plugin state"
    );
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_add_whose_grant_is_revoked_after_its_plan_moves_nothing() {
    let owner = Usage::start().await;
    let runtime = owner.runtime();
    let disk = tempfile::tempdir().unwrap();
    let place = disk.path().canonicalize().unwrap();
    let (home, source) = (place.join("projects"), place.join("sources").join("kept"));
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("main.rs"), "fn main() {}\n").unwrap();
    let mut controller = OwnerController::attach(
        owner.root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    controller.read(OwnerRead::ProjectsHome).await.unwrap();
    controller
        .begin_mutation(
            OwnerMutation::SetProjectsHome {
                path: home.to_str().unwrap().into(),
            },
            1,
        )
        .unwrap();
    assert!(matches!(
        controller.send_pending().await.unwrap(),
        OwnerResponse::Applied { .. }
    ));
    controller.read(OwnerRead::ProjectsHome).await.unwrap();
    controller
        .begin_mutation(
            OwnerMutation::AddExistingFolder {
                source: source.to_str().unwrap().into(),
                mode: AddMode::Move,
            },
            2,
        )
        .unwrap();

    let barrier = runtime.engine.owner_egress.write().await;
    let mut added = Box::pin(controller.send_pending());
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut added).await.is_err(),
        "the add waits for the owner barrier after its plan"
    );
    assert!(
        source.join("main.rs").is_file(),
        "no disk work runs before the grant is checked again"
    );
    let control = revocation(runtime, OwnerClientKind::Desktop, "revoke-before-add");
    let (revoked, registration) = runtime
        .engine
        .owner_job(|reply| Job::OwnerControl(runtime.authority.host.clone(), control, reply))
        .await
        .unwrap();
    runtime.authority.publish(registration).unwrap();
    assert!(!revoked.active);
    drop(barrier);

    let answered = tokio::time::timeout(Duration::from_secs(10), added)
        .await
        .unwrap();
    assert!(
        !matches!(answered, Ok(OwnerResponse::Applied { .. })),
        "{answered:?}"
    );
    assert!(
        source.join("main.rs").is_file() && !home.join("kept").exists(),
        "an add whose grant was revoked moves nothing"
    );
    drop(controller);
    owner.stop().await;
}
