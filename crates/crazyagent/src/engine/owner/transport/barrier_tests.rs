use super::usage_tests::{IN_FLIGHT, Usage, hold_storage};
use super::*;
use crate::client_controller::OwnerController;
use crate::plugins::{Scope, ServerSpec};
use crate::projects_home::AddMode;
use crate::protocol::ProjectId;

const PATIENT_WAIT_MS: u64 = 30_000;
const SWEEP_TIME_MAX: Duration = Duration::from_secs(60);
const LAPSE_WAIT_MS: u64 = 250;
const QUEUED_WAIT_MS: u64 = 3000;
const CHANGE_CUT_MS: u64 = 1500;
const CHANGE_CLIENT_MS: u64 = 1200;
const REMOVAL_CUT_MS: u64 = 1000;
const REMOVAL_BARRIER_HELD: Duration = Duration::from_millis(600);
const PUBLISHED_WAIT: Duration = Duration::from_secs(5);
const SLOW_ENGINE_MS: u64 = 3000;
const FAST_CLIENT_MS: u64 = 1000;

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

/// Waits up to 5 s for the storage queue to hold exactly `count` jobs the worker has not
/// received yet. Inputs: the engine and the count. Output: whether it did.
async fn storage_queue_reaches(owner: &Usage, count: usize) -> bool {
    let sender = &owner.runtime().engine.storage.sender;
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while sender.max_capacity() - sender.capacity() != count {
        if tokio::time::Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    true
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
            assert_eq!(error, failure("access_change_pending"));
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
    let release = hold_storage(&runtime.engine.storage.sender);
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
    release.send(()).unwrap();
    let answered = tokio::time::timeout(SWEEP_TIME_MAX, added).await.unwrap();
    let revoked = tokio::time::timeout(SWEEP_TIME_MAX, revoke).await.unwrap();
    let stored = stored_servers(&owner, "held-server").await;
    let id = match answered {
        Ok(server) => {
            assert_eq!(
                stored,
                std::slice::from_ref(&server.id),
                "the confirmed change is the one storage holds"
            );
            Some(server.id)
        }
        Err(error) if error.0 == "plugins_outcome_unknown" => {
            assert_eq!(
                stored.len(),
                1,
                "storage kept the change the owner wait could not confirm: {stored:?}"
            );
            Some(stored[0].clone())
        }
        Err(error) if error.0 == "storage_busy_not_submitted" => {
            assert!(
                stored.is_empty(),
                "the withdrawn change never reached storage"
            );
            None
        }
        Err(error) => panic!("the held change answered {}", error.0),
    };
    match revoked {
        Ok(reply) => assert!(!reply.active),
        Err(error) if error.0 == "access_change_pending" => {
            storage_settled(&owner).await;
            let reconciled = runtime.reconcile_control(control).await.unwrap();
            assert!(
                reconciled.is_some_and(|reply| !reply.active),
                "storage holds the revocation the owner wait could not confirm"
            );
        }
        Err(error) => panic!("the revocation answered {}", error.0),
    }

    let read = match id {
        Some(id) => owner.client.plugin_server(id).await.map(|_| ()),
        None => owner.client.plugin_servers(None, 0).await.map(|_| ()),
    };
    assert!(
        read.is_err(),
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
        .owner_job(|reply| Job::OwnerControl(runtime.authority.host.clone(), control, None, reply))
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

/// A folder add whose replay check storage takes 1.3 s late, and whose plan then waits behind
/// a second hold, is refused storage_busy_not_submitted by the projects home route before the
/// exchange deadline cuts it: the late take puts the plan's storage bound past that cut, so
/// the owner hears the refusal rather than an unknown outcome, and nothing moves
/// (V243-STORAGE-BUSY-ROWS).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_add_whose_plan_storage_cannot_take_is_refused_unsent_and_moves_nothing() {
    let owner = Usage::start().await;
    let disk = tempfile::tempdir().unwrap();
    let place = disk.path().canonicalize().unwrap();
    let (home, source) = (place.join("projects"), place.join("sources").join("kept"));
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("main.rs"), "fn main() {}\n").unwrap();
    let mut controller = OwnerController::attach(
        owner.root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(20),
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

    let first = hold_storage(&owner.runtime().engine.storage.sender);
    assert!(
        storage_queue_reaches(&owner, 0).await,
        "storage took the first hold"
    );
    let mut answered = Box::pin(tokio::time::timeout(
        Duration::from_secs(20),
        controller.send_pending(),
    ));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut answered)
            .await
            .is_err(),
        "the replay check waits behind the first hold"
    );
    assert!(
        storage_queue_reaches(&owner, 1).await,
        "the replay check is queued behind the first hold"
    );
    let second = hold_storage(&owner.runtime().engine.storage.sender);
    assert!(
        storage_queue_reaches(&owner, 2).await,
        "the second hold is queued behind the replay check"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut answered)
            .await
            .is_err(),
        "the replay check still waits, so the plan is submitted late"
    );
    drop(first);
    let answered = answered.await.unwrap();
    drop(second);
    assert!(
        matches!(
            &answered,
            Ok(OwnerResponse::Rejected { code }) if code == "storage_busy_not_submitted"
        ),
        "the route answered the refusal itself: {answered:?}"
    );
    storage_settled(&owner).await;
    assert!(
        source.join("main.rs").is_file() && !home.join("kept").exists(),
        "a refused add moves nothing"
    );
    drop(controller);
    owner.stop().await;
}

/// A New Project whose seed is done but whose record waits behind the owner barrier and busy
/// storage until the post-disk cut drops its wait still lands once storage is free: the cut
/// closes the exchange unanswered, the client reads unknown, which stays true, and the
/// project folder replaces its staging folder (V243-STORAGE-BUSY-ROWS, #629 F1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_project_record_its_cut_drops_lands_once_storage_is_free() {
    let owner = Usage::start().await;
    let runtime = owner.runtime();
    let disk = tempfile::tempdir().unwrap();
    let home = disk.path().canonicalize().unwrap().join("projects");
    std::fs::create_dir(&home).unwrap();
    let names = |staging: bool| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| match staging {
                true => name.starts_with(".crazyagent-new-"),
                false => !name.starts_with('.'),
            })
            .collect();
        names.sort();
        names
    };
    let mut controller = OwnerController::attach(
        owner.root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(20),
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
    controller.project_catalog(None, Vec::new()).await.unwrap();
    controller.read(OwnerRead::ProjectsHome).await.unwrap();
    controller
        .begin_mutation(
            OwnerMutation::SetupProject {
                project_id: ProjectId::new("books").unwrap(),
                spec: crate::protocol::ProjectSpec {
                    name: "Books".into(),
                    repositories: Vec::new(),
                },
                setup: crate::project_setup::ProjectSetup {
                    about: "Tracks the books I want to read.".into(),
                    tech_stack: vec!["rust".into()],
                    platforms: vec![crate::project_setup::Platform::Website],
                    research: Vec::new(),
                    pool: None,
                    team: None,
                    documents: Vec::new(),
                },
            },
            2,
        )
        .unwrap();
    let visible = names(false);

    let first = runtime.engine.owner_egress.write().await;
    let mut answered = Box::pin(tokio::time::timeout(
        Duration::from_secs(20),
        controller.send_pending(),
    ));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut answered)
            .await
            .is_err(),
        "the planned project waits for the owner barrier at its grant check"
    );
    let release = hold_storage(&owner.runtime().engine.storage.sender);
    assert!(
        storage_queue_reaches(&owner, 0).await,
        "storage took the hold"
    );
    let mut again = Box::pin(runtime.engine.owner_egress.write());
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut again).await.is_err(),
        "the test waits for the barrier behind the grant check"
    );
    drop(first);
    let second = again.await;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), &mut answered)
            .await
            .is_err(),
        "the seeded project waits for the barrier the test holds"
    );
    assert_eq!(names(true).len(), 1, "the seed ran into its staging folder");
    drop(second);
    assert!(
        storage_queue_reaches(&owner, 1).await,
        "the record is queued behind the hold"
    );
    let answered = answered.await.unwrap();
    assert!(
        matches!(
            &answered,
            Err(error) if error.0 == "owner_outcome_unknown_reconcile_same_identity"
        ),
        "the cut closed the exchange unanswered, which the client reads as unknown: {answered:?}"
    );
    assert_eq!(names(false), visible, "the record has not landed yet");

    drop(release);
    storage_settled(&owner).await;
    assert!(
        names(true).is_empty() && names(false).len() == visible.len() + 1,
        "the record landed once storage was free, so the project folder replaced its staging \
         folder: staging {:?}, visible {:?}",
        names(true),
        names(false)
    );
    drop(controller);
    owner.stop().await;
}

/// A plugin change storage cannot take in time is refused storage_busy_not_submitted to a
/// client whose wait ends before the engine's exchange deadline, as a desktop client's fixed
/// wait does under a longer owner timeout, or any wait whose connect outlasts the engine's
/// reply room. The client waits past that deadline, so it hears the refusal, not
/// plugins_outcome_unknown, and the change never lands (V243-STORAGE-BUSY-ROWS, #629 r2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_change_storage_cannot_take_is_refused_to_a_client_whose_wait_ends_first() {
    let owner = Usage::start_waiting(CHANGE_CUT_MS).await;
    let client = OwnerClient::discover(
        owner.root.path(),
        OwnerClientKind::Tui,
        Duration::from_millis(CHANGE_CLIENT_MS),
    )
    .await
    .unwrap()
    .0;
    let release = hold_storage(&owner.runtime().engine.storage.sender);
    assert!(
        storage_queue_reaches(&owner, 0).await,
        "storage took the hold"
    );
    let started = tokio::time::Instant::now();
    let added = client
        .add_mcp_server(
            Scope::Global,
            "late-server".into(),
            ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: Vec::new(),
            },
            Vec::new(),
        )
        .await;
    let waited = started.elapsed();
    assert_eq!(
        added.map(|server| server.id).map_err(|error| error.0),
        Err("storage_busy_not_submitted".to_owned()),
        "the client heard the engine's refusal after {waited:?}"
    );
    assert!(
        waited >= Duration::from_millis(CHANGE_CLIENT_MS),
        "the refusal came at the engine's cut, past the client's own timeout: {waited:?}"
    );
    drop(release);
    assert!(
        stored_servers(&owner, "late-server").await.is_empty(),
        "the refused change never landed"
    );
    owner.stop().await;
}

/// An account removal that waits for the owner barrier inside its serve bound, then behind
/// busy storage, is answered access_change_pending before that bound cuts it: its storage
/// job is decided by the serve's own cut, not by a bound counted from its later submit, so
/// the owner hears the pending removal, not accounts_outcome_unknown, and it lands once
/// storage is free (V243-STORAGE-BUSY-ROWS, #629 r2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_removal_held_at_the_barrier_then_behind_storage_is_pending_before_its_cut() {
    let owner = Usage::start_waiting(REMOVAL_CUT_MS).await;
    let runtime = owner.runtime();
    let listener = crate::spawn_gate::bind_tcp("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let added = owner
        .client
        .add_account(
            crate::accounts::AccountDraft {
                provider: crate::accounts::Provider::OpenaiCompatible,
                method: crate::accounts::SignInMethod::Url,
                name: "Held removal".into(),
                url: Some(url),
                model: Some("lab-model".into()),
            },
            Some(
                crate::engine::owner::AccountSecret::new(
                    format!("sk-held-removal-{}", std::process::id()).into_bytes(),
                )
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    let held = runtime.engine.owner_egress.write().await;
    let release = hold_storage(&owner.runtime().engine.storage.sender);
    assert!(
        storage_queue_reaches(&owner, 0).await,
        "storage took the hold"
    );
    let mut removed = Box::pin(owner.client.remove_account(added.id.clone()));
    assert!(
        tokio::time::timeout(REMOVAL_BARRIER_HELD, &mut removed)
            .await
            .is_err(),
        "the removal waits for the owner barrier inside its serve bound"
    );
    drop(held);
    let removed = tokio::time::timeout(Duration::from_secs(10), removed)
        .await
        .unwrap();
    assert_eq!(
        removed.map(|_| ()).map_err(|error| error.0),
        Err("access_change_pending".to_owned()),
        "the owner hears the pending removal, decided before the serve's cut"
    );
    drop(release);
    storage_settled(&owner).await;
    assert!(
        owner
            .client
            .list_accounts(None, 10)
            .await
            .unwrap()
            .accounts
            .is_empty(),
        "the pending removal landed once storage was free"
    );
    owner.stop().await;
}

/// Discovers a TUI owner client whose own timeout is far below the engine's, as the TUI's
/// fixed timeout is below a raised timeout_ms. Input: the engine. Output: the client, which
/// learns the engine's timeout from the locator it discovers through.
async fn fast_client(owner: &Usage) -> OwnerClient {
    OwnerClient::discover(
        owner.root.path(),
        OwnerClientKind::Tui,
        Duration::from_millis(FAST_CLIENT_MS),
    )
    .await
    .unwrap()
    .0
}

/// A plugin change behind busy storage, sent by a client whose timeout is a third of the
/// engine's, hears the engine's refusal at the engine's own cut: the client waits its own
/// timeout plus the engine's, not its own timeout twice, so it never says
/// plugins_outcome_unknown for a change refused unsent (V243-PLUGINS-CLIENT-TIMEOUT-BELOW-ENGINE).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_change_behind_busy_storage_is_refused_to_a_client_far_below_the_engine() {
    let owner = Usage::start_waiting(SLOW_ENGINE_MS).await;
    let client = fast_client(&owner).await;
    let release = hold_storage(&owner.runtime().engine.storage.sender);
    assert!(
        storage_queue_reaches(&owner, 0).await,
        "storage took the hold"
    );
    let started = tokio::time::Instant::now();
    let added = client
        .add_mcp_server(
            Scope::Global,
            "slow-engine-server".into(),
            ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: Vec::new(),
            },
            Vec::new(),
        )
        .await;
    let waited = started.elapsed();
    assert_eq!(
        added.map(|server| server.id).map_err(|error| error.0),
        Err("storage_busy_not_submitted".to_owned()),
        "the client heard the engine's refusal after {waited:?}"
    );
    assert!(
        waited >= Duration::from_millis(2 * FAST_CLIENT_MS),
        "the refusal came past twice the client's own timeout: {waited:?}"
    );
    drop(release);
    assert!(
        stored_servers(&owner, "slow-engine-server")
            .await
            .is_empty(),
        "the refused change never landed"
    );
    owner.stop().await;
}

/// An account removal behind busy storage, sent by a client whose timeout is a third of the
/// engine's, hears access_change_pending decided before the engine's cut: the client waits
/// the removal's bound at the engine's timeout plus its own, not the bound at its own
/// timeout, so it never says accounts_outcome_unknown, and the removal lands once storage
/// is free (V243-PLUGINS-CLIENT-TIMEOUT-BELOW-ENGINE).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_removal_behind_busy_storage_is_pending_to_a_client_far_below_the_engine() {
    let owner = Usage::start_waiting(SLOW_ENGINE_MS).await;
    let listener = crate::spawn_gate::bind_tcp("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let added = owner
        .client
        .add_account(
            crate::accounts::AccountDraft {
                provider: crate::accounts::Provider::OpenaiCompatible,
                method: crate::accounts::SignInMethod::Url,
                name: "Slow engine removal".into(),
                url: Some(url),
                model: Some("lab-model".into()),
            },
            Some(
                crate::engine::owner::AccountSecret::new(
                    format!("sk-slow-engine-{}", std::process::id()).into_bytes(),
                )
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    let client = fast_client(&owner).await;
    let release = hold_storage(&owner.runtime().engine.storage.sender);
    assert!(
        storage_queue_reaches(&owner, 0).await,
        "storage took the hold"
    );
    let started = tokio::time::Instant::now();
    let removed = client.remove_account(added.id.clone()).await;
    let waited = started.elapsed();
    assert_eq!(
        removed.map(|_| ()).map_err(|error| error.0),
        Err("access_change_pending".to_owned()),
        "the client heard the pending removal after {waited:?}"
    );
    assert!(
        waited >= Duration::from_millis(2 * FAST_CLIENT_MS),
        "the answer came past the removal's bound at the client's own timeout: {waited:?}"
    );
    drop(release);
    storage_settled(&owner).await;
    assert!(
        owner
            .client
            .list_accounts(None, 10)
            .await
            .unwrap()
            .accounts
            .is_empty(),
        "the pending removal landed once storage was free"
    );
    owner.stop().await;
}

/// RV-DS241-76: an owner request admitted while a revocation waits on the owner barrier,
/// and queued behind it, creates nothing once that revocation lapsed and closed the client:
/// the generic owner path checks the grant again under the barrier, as the foreground path
/// does. The request is sent halfway through the revocation's wait, so it is admitted
/// before the lapse and its own deadline outlasts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_request_queued_behind_a_lapsed_revocation_creates_nothing() {
    let owner = Usage::start_waiting(QUEUED_WAIT_MS).await;
    let runtime = owner.runtime();
    let read = owner
        .client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("queued-read").unwrap(),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::Capabilities,
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = read.response else {
        panic!("the revision read failed: {read:?}");
    };
    let holder = runtime.engine.owner_egress.write().await;
    let mut revoke = Box::pin(runtime.control(revocation(
        runtime,
        OwnerClientKind::Tui,
        "revoke-behind-a-holder",
    )));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut revoke).await.is_err(),
        "the revocation waits on the held barrier"
    );
    tokio::time::sleep(Duration::from_millis(QUEUED_WAIT_MS / 2)).await;
    let project = ProjectId::new("queued-project").unwrap();
    let mut created = Box::pin(owner.client.request(OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("queued-create").unwrap(),
        expected_revision: state.revision,
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::CreateProject {
                project_id: project.clone(),
                spec: ProjectSpec {
                    name: "queued".into(),
                    repositories: vec![],
                },
            },
        },
    }));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut created).await.is_err(),
        "the request is admitted and waits on the held barrier"
    );
    let lapsed = tokio::time::timeout(Duration::from_millis(QUEUED_WAIT_MS), revoke)
        .await
        .expect("the revocation lapses at its owner wait");
    assert!(lapsed.is_err(), "{lapsed:?}");
    assert!(!admitted(runtime, &owner.client));
    drop(holder);
    let answered = tokio::time::timeout(Duration::from_millis(QUEUED_WAIT_MS), created).await;
    storage_settled(&owner).await;
    let stored: i64 = rusqlite::Connection::open(owner.engine_root.join("engine.db"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM projects WHERE id=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored, 0,
        "the request admitted before the lapsed revocation created nothing: {answered:?}"
    );
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
    let release = hold_storage(&runtime.engine.storage.sender);
    let lapsed = runtime
        .control(revocation(runtime, OwnerClientKind::Tui, "revoke-lapsed"))
        .await;
    assert_eq!(
        lapsed.map(|reply| reply.active),
        Err(failure("access_change_pending")),
        "the revocation outlasts the owner wait and stays queued, never withdrawn"
    );
    assert!(
        !admitted(runtime, &owner.client),
        "the revoked client is refused right after the outcome-unknown answer"
    );

    release.send(()).unwrap();
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
async fn a_lapsed_reactivation_is_withdrawn_and_admits_no_one() {
    let _disk = super::usage_tests::busy_storage().await;
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let runtime = owner.runtime();
    let revoked = revoke_settled(&owner, "revoke-first").await;
    assert!(!revoked.active);
    assert!(!admitted(runtime, &owner.client));
    let release = hold_storage(&runtime.engine.storage.sender);
    let lapsed = runtime
        .control(reactivation("reactivate-lapsed", revoked.owner_epoch))
        .await;
    assert_eq!(
        lapsed.map(|reply| reply.active),
        Err(failure("storage_busy_not_submitted")),
        "the reactivation outlasts the owner wait and is withdrawn unrun"
    );
    release.send(()).unwrap();
    storage_settled(&owner).await;
    assert_eq!(
        runtime.epoch(OwnerClientKind::Tui).unwrap(),
        revoked.owner_epoch,
        "the withdrawn reactivation never reached storage"
    );
    assert!(!tui_registration(runtime).active);
    assert!(
        !admitted(runtime, &fresh_client(&owner)),
        "a withdrawn reactivation admits no one"
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

/// Storage is held for both controls, so each one outlasts its owner wait on any machine: one
/// sweep that had to outlast two waits ended inside the second on a fast hosted Linux disk,
/// and the revocation got storage's stale answer instead of the lapse (BL-DBLOCK).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reactivation_sent_before_a_lapsed_revocation_never_reopens_the_slot() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let runtime = owner.runtime();
    let revoked = revoke_settled(&owner, "revoke-before-both").await;
    assert!(!revoked.active);
    let release = hold_storage(&owner.runtime().engine.storage.sender);
    let reactivated = runtime
        .control(reactivation(
            "reactivate-before-revoke",
            revoked.owner_epoch,
        ))
        .await;
    assert_eq!(
        reactivated.map(|reply| reply.active),
        Err(failure("storage_busy_not_submitted")),
        "the reactivation outlasts the owner wait and is withdrawn"
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
        Err(failure("access_change_pending")),
        "the revocation outlasts the owner wait and stays queued"
    );
    assert!(!admitted(runtime, &owner.client));

    release.send(()).unwrap();
    storage_settled(&owner).await;
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

/// The database path the engine gave `Storage::open`, the key of every storage hold.
/// Input: the engine. Output: the path.
fn database(owner: &Usage) -> PathBuf {
    std::fs::canonicalize(owner.root.path())
        .unwrap()
        .join("engine.db")
}

/// The TUI identity as storage holds it, read through a connection of its own.
/// Input: the engine folder. Output: the epoch and the status.
fn stored_tui(root: &Path) -> (i64, String) {
    let db = rusqlite::Connection::open_with_flags(
        root.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.query_row(
        "SELECT owner_epoch,status FROM owner_identities WHERE client_kind='tui'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

/// Waits until the storage worker of `path` used up its death mark, so the next open of the
/// same database never meets it. Input: the database path. Output: whether it did within
/// `PUBLISHED_WAIT`.
async fn worker_died(path: &Path) -> bool {
    let deadline = tokio::time::Instant::now() + PUBLISHED_WAIT;
    while crate::storage::dying(path) {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// Waits until storage holds the TUI identity revoked one epoch past `epoch`.
/// Inputs: the engine folder and the epoch the revocation expected. Output: whether it
/// did within `PUBLISHED_WAIT`.
async fn stored_revoked(root: &Path, epoch: i64) -> bool {
    let deadline = tokio::time::Instant::now() + PUBLISHED_WAIT;
    while stored_tui(root) != (epoch + 1, "revoked".to_owned()) {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_that_never_gets_the_owner_barrier_is_pending_and_lands_while_the_engine_runs()
{
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let runtime = owner.runtime();
    let epoch = runtime.epoch(OwnerClientKind::Tui).unwrap();
    let held = runtime.engine.owner_egress.read().await;
    assert_eq!(
        runtime
            .control(revocation(
                runtime,
                OwnerClientKind::Tui,
                "revoke-barrier-kept"
            ))
            .await
            .map(|reply| reply.active),
        Err(failure("access_change_pending")),
        "a revocation that never gets the owner barrier is kept and pending"
    );
    assert!(!admitted(runtime, &owner.client));
    drop(held);
    assert!(
        stored_revoked(owner.root.path(), epoch).await,
        "the kept revocation lands while the engine runs"
    );
    assert!(!admitted(runtime, &fresh_client(&owner)));
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_the_full_queue_refuses_is_pending_and_lands_while_the_engine_runs() {
    let owner = Usage::start_settings(&format!(
        "timeout_ms = {LAPSE_WAIT_MS}\nqueue_capacity = 1\n"
    ))
    .await;
    let runtime = owner.runtime();
    let epoch = runtime.epoch(OwnerClientKind::Tui).unwrap();
    let hold = crate::storage::hold_claims(&database(&owner));
    let engine = runtime.engine.clone();
    let filler = tokio::spawn(async move {
        let (send, _answer) = oneshot::channel();
        engine
            .storage
            .submit(
                crate::storage::Job::Setup(crate::storage::setup::Command::Sweep, send),
                tokio::time::Instant::now() + Duration::from_millis(PATIENT_WAIT_MS),
                "owner_queue_not_submitted",
            )
            .await
            .map(|_| ())
    });
    tokio::time::sleep(IN_FLIGHT).await;
    assert_eq!(
        runtime
            .control(revocation(
                runtime,
                OwnerClientKind::Tui,
                "revoke-queue-kept"
            ))
            .await
            .map(|reply| reply.active),
        Err(failure("access_change_pending")),
        "a revocation the full queue refuses is kept and pending"
    );
    assert!(!admitted(runtime, &owner.client));
    drop(hold);
    filler.await.unwrap().unwrap();
    assert!(
        stored_revoked(owner.root.path(), epoch).await,
        "the kept revocation lands while the engine runs"
    );
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_pending_when_the_engine_dies_lands_at_the_next_open() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let runtime = owner.runtime();
    let epoch = runtime.epoch(OwnerClientKind::Tui).unwrap();
    let path = database(&owner);
    let hold = crate::storage::hold_claims(&path);
    assert_eq!(
        runtime
            .control(revocation(runtime, OwnerClientKind::Tui, "revoke-then-die"))
            .await
            .map(|reply| reply.active),
        Err(failure("access_change_pending")),
        "the revocation outlasts the owner wait and is pending"
    );
    crate::storage::die_at_claim(&path);
    drop(hold);
    assert!(
        worker_died(&path).await,
        "the storage worker dies at its claim"
    );
    let restarted = owner.restarted().await;
    assert_eq!(
        stored_tui(restarted.root.path()),
        (epoch + 1, "revoked".to_owned()),
        "the next open applies the revocation the dead engine never ran"
    );
    assert!(!tui_registration(restarted.runtime()).active);
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_refused_at_the_barrier_lands_at_the_next_open_when_the_engine_dies_mid_drain()
{
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let runtime = owner.runtime();
    let epoch = runtime.epoch(OwnerClientKind::Tui).unwrap();
    let path = database(&owner);
    let hold = crate::storage::hold_claims(&path);
    let held = runtime.engine.owner_egress.read().await;
    assert_eq!(
        runtime
            .control(revocation(
                runtime,
                OwnerClientKind::Tui,
                "revoke-drain-die"
            ))
            .await
            .map(|reply| reply.active),
        Err(failure("access_change_pending")),
        "a revocation that never gets the owner barrier is kept and pending"
    );
    drop(held);
    tokio::time::sleep(IN_FLIGHT).await;
    crate::storage::die_at_claim(&path);
    drop(hold);
    assert!(
        worker_died(&path).await,
        "the storage worker dies at the drain's claim"
    );
    let restarted = owner.restarted().await;
    assert_eq!(
        stored_tui(restarted.root.path()),
        (epoch + 1, "revoked".to_owned()),
        "the next open applies the kept revocation the drain never landed"
    );
    assert!(!tui_registration(restarted.runtime()).active);
    restarted.stop().await;
}

/// V243-NARROWING-DURABLE r2, pre-existing: an engine whose TUI client storage holds
/// revoked still starts, since its start probe asks only the clients it holds active, and
/// the revoked TUI stays refused while the Desktop client is admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_whose_tui_is_revoked_starts_and_refuses_the_tui() {
    let owner = Usage::start().await;
    let epoch = owner.runtime().epoch(OwnerClientKind::Tui).unwrap();
    assert!(!revoke_settled(&owner, "revoke-before-restart").await.active);
    let restarted = owner.restarted().await;
    assert_eq!(
        stored_tui(restarted.root.path()),
        (epoch + 1, "revoked".to_owned())
    );
    let runtime = restarted.runtime();
    assert!(!tui_registration(runtime).active);
    let root = &runtime.authority.root.path;
    let tui = OwnerClient::attach(root, OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    let desktop =
        OwnerClient::attach(root, OwnerClientKind::Desktop, Duration::from_secs(2)).unwrap();
    assert!(!admitted(runtime, &tui), "the revoked TUI stays refused");
    assert!(
        admitted(runtime, &desktop),
        "the active Desktop client is admitted"
    );
    restarted.stop().await;
}
