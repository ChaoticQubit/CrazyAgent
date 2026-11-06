use super::usage_tests::{IN_FLIGHT, Usage};
use super::*;
use crate::client_controller::OwnerController;
use crate::plugins::{Scope, ServerSpec};
use crate::projects_home::AddMode;
use crate::protocol::ProjectId;

const SWEPT_FILES: usize = 40_000;
const PATIENT_WAIT_MS: u64 = 30_000;
const SWEEP_TIME_MAX: Duration = Duration::from_secs(60);
const LAPSE_WAIT_MS: u64 = 250;
const PUBLISHED_WAIT: Duration = Duration::from_secs(5);

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

/// Waits until storage has answered every job queued before this call, so a change it ran
/// after the owner stopped waiting is committed. Input: the engine.
async fn storage_settled(owner: &Usage) {
    let (send, settled) = oneshot::channel();
    owner
        .runtime()
        .engine
        .storage
        .sender
        .send(crate::storage::Job::Setup(
            crate::storage::setup::Command::Sweep,
            send,
        ))
        .await
        .unwrap();
    tokio::time::timeout(SWEEP_TIME_MAX, settled)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// The ids of the stored MCP servers of one name, read once storage has settled.
/// Inputs: the engine and the name. Output: the ids.
async fn stored_servers(owner: &Usage, name: &str) -> Vec<String> {
    storage_settled(owner).await;
    let db = rusqlite::Connection::open_with_flags(
        owner.engine_root.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.prepare("SELECT id FROM plugin_items WHERE kind='server' AND name=?1")
        .unwrap()
        .query_map([name], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Whether the engine admits an owner client's capability now: the check every owner
/// request passes before it reaches storage. Inputs: the runtime and the client.
/// Output: the answer.
fn admitted(runtime: &OwnerRuntimeHandle, client: &OwnerClient) -> bool {
    let mut packet = [0; 37];
    packet[..4].copy_from_slice(b"CAO1");
    packet[4] = match client.kind {
        OwnerClientKind::Tui => 0,
        OwnerClientKind::Desktop => 1,
    };
    packet[5..].copy_from_slice(&client.token);
    runtime.authority.authenticate(&packet).is_ok()
}

/// The TUI client's registration as the engine holds it in memory. Input: the runtime.
/// Output: the registration.
fn tui_registration(runtime: &OwnerRuntimeHandle) -> Registration {
    runtime
        .authority
        .slots
        .lock()
        .unwrap()
        .iter()
        .find(|slot| slot.registration.kind == OwnerClientKind::Tui)
        .unwrap()
        .registration
        .clone()
}

/// Waits until the engine holds a TUI epoch in memory. Inputs: the runtime and the epoch.
/// Output: whether it did within `PUBLISHED_WAIT`.
async fn published(runtime: &OwnerRuntimeHandle, epoch: i64) -> bool {
    let deadline = tokio::time::Instant::now() + PUBLISHED_WAIT;
    while runtime.epoch(OwnerClientKind::Tui).unwrap() != epoch {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// A fresh TUI client that reads the capability file the engine holds now, without
/// asking the engine anything. Input: the engine. Output: the client.
fn fresh_client(owner: &Usage) -> OwnerClient {
    OwnerClient::attach(
        &owner.client.root.path,
        OwnerClientKind::Tui,
        Duration::from_secs(20),
    )
    .unwrap()
}

/// Revokes the TUI client and returns what storage committed, reconciling the same control
/// when the answer outlasts a short owner wait. Inputs: the engine and the operation name.
/// Output: the committed reply.
async fn revoke_settled(owner: &Usage, name: &str) -> OwnerControlReply {
    let runtime = owner.runtime();
    let control = revocation(runtime, OwnerClientKind::Tui, name);
    match runtime.control(control.clone()).await {
        Ok(reply) => reply,
        Err(error) => {
            assert_eq!(
                error,
                failure("owner_outcome_unknown_reconcile_same_identity")
            );
            storage_settled(owner).await;
            runtime.reconcile_control(control).await.unwrap().unwrap()
        }
    }
}

/// The owner control that reactivates the TUI client at an epoch. Inputs: the operation
/// name and the expected epoch. Output: the control.
fn reactivation(name: &str, expected_epoch: i64) -> OwnerControl {
    OwnerControl {
        operation_id: RequestId::new(name).unwrap(),
        target: OwnerClientKind::Tui,
        expected_epoch,
        decision: OwnerDecision::Reactivate,
    }
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
    let _disk = super::usage_tests::busy_storage().await;
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
        Vec::new(),
    ));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut added).await.is_err(),
        "the change waits behind the busy storage"
    );
    assert!(
        runtime.engine.owner_egress.try_write().is_err(),
        "the change holds the owner barrier while it waits"
    );
    let control = revocation(runtime, OwnerClientKind::Tui, "revoke-during-plugin-change");
    let mut revoke = Box::pin(runtime.control(control.clone()));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut revoke).await.is_err(),
        "a revocation waits for the change already in flight"
    );
    let answered = tokio::time::timeout(SWEEP_TIME_MAX, added).await.unwrap();
    let revoked = tokio::time::timeout(SWEEP_TIME_MAX, revoke).await.unwrap();
    tokio::time::timeout(SWEEP_TIME_MAX, sweep)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!swept.exists());
    let stored = stored_servers(&owner, "held-server").await;
    let id = match answered {
        Ok(server) => {
            assert_eq!(
                stored,
                std::slice::from_ref(&server.id),
                "the confirmed change is the one storage holds"
            );
            server.id
        }
        Err(error) if error.0 == "plugins_outcome_unknown" => {
            assert_eq!(
                stored.len(),
                1,
                "storage kept the change the owner wait could not confirm: {stored:?}"
            );
            stored[0].clone()
        }
        Err(error) => panic!("the held change answered {}", error.0),
    };
    match revoked {
        Ok(reply) => assert!(!reply.active),
        Err(error) if error.0 == "owner_outcome_unknown_reconcile_same_identity" => {
            let reconciled = runtime.reconcile_control(control).await.unwrap();
            assert!(
                reconciled.is_some_and(|reply| !reply.active),
                "storage holds the revocation the owner wait could not confirm"
            );
        }
        Err(error) => panic!("the revocation answered {}", error.0),
    }

    assert!(
        owner.client.plugin_server(id).await.is_err(),
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
                overrides: Vec::new(),
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
    runtime
        .authority
        .publish(registration, runtime.authority.next_control())
        .unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_that_outlasts_the_owner_wait_refuses_the_client_at_once() {
    let _disk = super::usage_tests::busy_storage().await;
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let runtime = owner.runtime();
    assert!(
        admitted(runtime, &owner.client),
        "the client starts admitted"
    );
    let epoch = runtime.epoch(OwnerClientKind::Tui).unwrap();
    let (swept, sweep) = occupy_storage(&owner);
    let lapsed = runtime
        .control(revocation(runtime, OwnerClientKind::Tui, "revoke-lapsed"))
        .await;
    assert_eq!(
        lapsed.map(|reply| reply.active),
        Err(failure("owner_outcome_unknown_reconcile_same_identity")),
        "the revocation outlasts the owner wait"
    );
    assert!(
        !admitted(runtime, &owner.client),
        "the revoked client is refused right after the outcome-unknown answer"
    );

    tokio::time::timeout(SWEEP_TIME_MAX, sweep)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!swept.exists());
    storage_settled(&owner).await;
    assert!(
        published(runtime, epoch + 1).await,
        "memory follows the revocation storage committed"
    );
    assert!(!tui_registration(runtime).active);
    assert!(!admitted(runtime, &owner.client));
    assert!(!admitted(runtime, &fresh_client(&owner)));
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reactivation_that_outlasts_the_owner_wait_admits_the_client_once_storage_settles() {
    let _disk = super::usage_tests::busy_storage().await;
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let runtime = owner.runtime();
    let revoked = revoke_settled(&owner, "revoke-first").await;
    assert!(!revoked.active);
    assert!(!admitted(runtime, &owner.client));
    let (swept, sweep) = occupy_storage(&owner);
    let lapsed = runtime
        .control(reactivation("reactivate-lapsed", revoked.owner_epoch))
        .await;
    assert_eq!(
        lapsed.map(|reply| reply.active),
        Err(failure("owner_outcome_unknown_reconcile_same_identity")),
        "the reactivation outlasts the owner wait"
    );

    tokio::time::timeout(SWEEP_TIME_MAX, sweep)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!swept.exists());
    storage_settled(&owner).await;
    assert!(
        published(runtime, revoked.owner_epoch + 1).await,
        "memory follows the reactivation storage committed"
    );
    assert!(tui_registration(runtime).active);
    assert!(
        admitted(runtime, &fresh_client(&owner)),
        "the reactivated client is admitted once storage settles"
    );
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_publication_never_replaces_a_newer_registration_or_reopens_a_closed_one() {
    let owner = Usage::start().await;
    let runtime = owner.runtime();
    let older = tui_registration(runtime);
    assert!(older.active);
    let revoked = runtime
        .control(revocation(runtime, OwnerClientKind::Tui, "revoke-newer"))
        .await
        .unwrap();
    assert!(!revoked.active);

    runtime
        .authority
        .publish(older, runtime.authority.next_control())
        .unwrap();
    assert_eq!(
        runtime.epoch(OwnerClientKind::Tui).unwrap(),
        revoked.owner_epoch,
        "an older late settle does not replace the newer registration"
    );
    assert!(!tui_registration(runtime).active);
    assert!(!admitted(runtime, &fresh_client(&owner)));

    let mut reopened = tui_registration(runtime);
    reopened.active = true;
    runtime
        .authority
        .publish(reopened, runtime.authority.next_control())
        .unwrap();
    assert!(
        !tui_registration(runtime).active,
        "a settle of the same epoch never reopens a closed slot"
    );
    assert!(!admitted(runtime, &fresh_client(&owner)));
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reactivation_sent_before_a_lapsed_revocation_never_reopens_the_slot() {
    let _disk = super::usage_tests::busy_storage().await;
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let runtime = owner.runtime();
    let revoked = revoke_settled(&owner, "revoke-before-both").await;
    assert!(!revoked.active);
    let (swept, sweep) = occupy_storage(&owner);
    let reactivated = runtime
        .control(reactivation(
            "reactivate-before-revoke",
            revoked.owner_epoch,
        ))
        .await;
    assert_eq!(
        reactivated.map(|reply| reply.active),
        Err(failure("owner_outcome_unknown_reconcile_same_identity")),
        "the reactivation outlasts the owner wait"
    );
    let lapsed = runtime
        .control(revocation(
            runtime,
            OwnerClientKind::Tui,
            "revoke-after-reactivate",
        ))
        .await;
    assert_eq!(
        lapsed.map(|reply| reply.active),
        Err(failure("owner_outcome_unknown_reconcile_same_identity")),
        "the revocation outlasts the owner wait"
    );
    assert!(!admitted(runtime, &owner.client));

    tokio::time::timeout(SWEEP_TIME_MAX, sweep)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!swept.exists());
    storage_settled(&owner).await;
    assert!(
        published(runtime, revoked.owner_epoch + 1).await,
        "memory takes the epoch storage committed"
    );
    assert!(
        !tui_registration(runtime).active,
        "a reactivation sent before the revocation never reopens the slot"
    );
    assert!(
        !admitted(runtime, &fresh_client(&owner)),
        "the client the revocation refused stays refused"
    );
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_storage_refuses_still_refuses_the_client_until_a_newer_reactivation() {
    let owner = Usage::start().await;
    let runtime = owner.runtime();
    let epoch = runtime.epoch(OwnerClientKind::Tui).unwrap();
    let mut stale = revocation(runtime, OwnerClientKind::Tui, "revoke-stale");
    stale.expected_epoch = epoch + 1;
    assert_eq!(
        runtime.control(stale).await.map(|reply| reply.active),
        Err(failure("owner_control_stale")),
        "storage refuses the revocation"
    );
    assert!(
        !admitted(runtime, &owner.client),
        "a revocation storage refused still refuses the client"
    );

    let reactivated = runtime
        .control(reactivation("reactivate-after-refusal", epoch))
        .await
        .unwrap();
    assert!(reactivated.active);
    assert_eq!(runtime.epoch(OwnerClientKind::Tui).unwrap(), epoch + 1);
    assert!(
        admitted(runtime, &fresh_client(&owner)),
        "a newer committed reactivation admits the client again"
    );
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_that_never_reaches_storage_still_refuses_the_client() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let runtime = owner.runtime();
    let held = runtime.engine.owner_egress.read().await;
    assert_eq!(
        runtime
            .control(revocation(runtime, OwnerClientKind::Tui, "revoke-held"))
            .await
            .map(|reply| reply.active),
        Err(failure("owner_control_not_submitted")),
        "the revocation never gets the owner barrier"
    );
    drop(held);
    assert!(
        !admitted(runtime, &owner.client),
        "a revocation that never reached storage still refuses the client"
    );
    owner.stop().await;
}
