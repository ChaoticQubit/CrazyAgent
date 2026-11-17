#![cfg(unix)]

use crazyagent::{config::resolve_config, engine::Engine, protocol::*};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

fn git(root: &Path, args: &[&str]) -> String {
    let output = crazyagent::spawn_gate::output(
        Process::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-C", root.to_str().unwrap()])
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "{:?}", output);
    String::from_utf8(output.stdout).unwrap()
}

fn repository(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir(&path).unwrap();
    git(&path, &["init", "-b", "main"]);
    std::fs::write(path.join("tracked"), "committed\n").unwrap();
    git(&path, &["add", "tracked"]);
    git(
        &path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid",
            "commit",
            "-m",
            "base",
        ],
    );
    path
}

fn request(id: &str, revision: u64, command: Command) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("worktree-test").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        command,
    }
}

/// Sends one request the way the engine's contract asks a client to: while it answers
/// outcome_unknown_retry_same_identity (its wait ended and the write may still commit),
/// the same request goes again, and storage replays a recorded mutation's reply. Gives up
/// after 30 s. Inputs: the engine and the request. Output: the reply.
async fn send(engine: &Engine, request: WireRequest) -> WireReply {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match engine.apply(request.clone()).await {
            Ok(reply) => return reply,
            Err(error)
                if error.0 == "outcome_unknown_retry_same_identity"
                    && std::time::Instant::now() < until => {}
            Err(error) => panic!("engine apply failed: {error:?}"),
        }
    }
}

async fn apply(engine: &Engine, id: &str, revision: u64, command: Command) -> WireReply {
    send(engine, request(id, revision, command)).await
}

/// Opens the engine, creates the project and pins its task, and starts the owner runtime the
/// way `serve` does at boot, so lease writes run under the store's engine host. Inputs: the
/// root and the repository. Output: the engine and its configuration.
async fn setup(root: &Path, repo: &Path) -> (Engine, crazyagent::config::EffectiveConfig) {
    let (engine, config) = setup_unstarted(root, repo).await;
    engine.start_owner_runtime().await.unwrap();
    (engine, config)
}

/// Opens the engine, creates the project and pins its task, with no owner runtime started.
/// Inputs: the root and the repository. Output: the engine and its configuration.
async fn setup_unstarted(
    root: &Path,
    repo: &Path,
) -> (Engine, crazyagent::config::EffectiveConfig) {
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.join("global"), &root.join("project")).unwrap();
    let engine = Engine::open(&root.join("db"), config.clone())
        .await
        .unwrap();
    assert_eq!(
        apply(
            &engine,
            "project",
            0,
            Command::CreateProject {
                project_id: ProjectId::new("project").unwrap(),
                spec: ProjectSpec {
                    name: "Product".into(),
                    repositories: vec![repo.to_str().unwrap().into()]
                },
            }
        )
        .await
        .outcome,
        Outcome::Applied
    );
    assert_eq!(
        apply(
            &engine,
            "pin",
            1,
            Command::PinTask {
                context: ExecutionContext {
                    project_id: ProjectId::new("project").unwrap(),
                    task_id: TaskId::new("task").unwrap(),
                    config_id: config.id.clone(),
                    snapshot_id: SnapshotId::new("snapshot").unwrap(),
                    baseline_id: BaselineId::new("baseline").unwrap(),
                    allowed_destinations: vec![],
                    budget_ref: CredentialRef::new("budget").unwrap(),
                }
            }
        )
        .await
        .outcome,
        Outcome::Applied
    );
    (engine, config)
}

fn allocate(repo: &Path, owner: &str, native: Option<&str>) -> Command {
    Command::AllocateWorktree {
        task_id: TaskId::new("task").unwrap(),
        repository: repo.to_str().unwrap().into(),
        owner_session: SessionId::new(owner).unwrap(),
        native_descendant: native.map(str::to_owned),
    }
}

/// The engine's current revision, from a read's reply; lease writes move it more than once
/// (v2.5 L6), so requests after one read it. Input: the engine. Output: the revision.
async fn revision(engine: &Engine) -> u64 {
    let read = Command::WorktreeLeases {
        task_id: TaskId::new("task").unwrap(),
    };
    apply(engine, "revision", 0, read).await.revision.value()
}

async fn leases(engine: &Engine) -> Vec<crazyagent::worktrees::WorktreeLease> {
    let reply = apply(
        engine,
        "leases",
        0,
        Command::WorktreeLeases {
            task_id: TaskId::new("task").unwrap(),
        },
    )
    .await;
    let Outcome::Worktrees { leases } = reply.outcome else {
        panic!("leases required")
    };
    leases
}

#[tokio::test]
async fn dirty_work_is_preserved_and_writer_is_unique() {
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let repo = repository(&root, "repo");
    std::fs::write(repo.join("tracked"), b"owner dirty\0bytes").unwrap();
    std::fs::write(repo.join("untracked"), b"owner untracked\0bytes").unwrap();
    let (engine, config) = setup(&root, &repo).await;
    let unadmitted = apply(&engine, "unadmitted", 2, allocate(&repo, "owner", None)).await;
    assert!(matches!(unadmitted.outcome, Outcome::Rejected { .. }));
    assert_eq!(
        apply(
            &engine,
            "admit",
            2,
            Command::AdmitRepository {
                project_id: ProjectId::new("project").unwrap(),
                repository: repo.to_str().unwrap().into(),
            }
        )
        .await
        .outcome,
        Outcome::Applied
    );
    let operation = request(
        "allocate",
        revision(&engine).await,
        allocate(&repo, "owner", Some("native-child")),
    );
    let first = send(&engine, operation.clone()).await;
    assert_eq!(first.outcome, Outcome::Applied);
    assert_eq!(send(&engine, operation.clone()).await, first);
    let lease = leases(&engine).await.remove(0);
    assert_eq!(
        std::fs::read(Path::new(&lease.path).join("tracked")).unwrap(),
        b"committed\n"
    );
    assert_eq!(lease.native_descendant.as_deref(), Some("native-child"));
    std::fs::write(Path::new(&lease.path).join("tracked"), b"worker dirty").unwrap();
    std::fs::write(Path::new(&lease.path).join("scratch"), b"worker scratch").unwrap();
    let second = apply(
        &engine,
        "second",
        revision(&engine).await,
        allocate(&repo, "second-owner", None),
    )
    .await;
    assert!(matches!(second.outcome, Outcome::Rejected { .. }));
    engine.close().await.unwrap();
    let engine = Engine::open(&root.join("db"), config).await.unwrap();
    let recovered = leases(&engine).await.remove(0);
    assert_eq!(
        recovered.state,
        crazyagent::worktrees::LeaseState::RecoveryRequired
    );
    assert_eq!(send(&engine, operation).await, first);
    let release = apply(
        &engine,
        "release",
        revision(&engine).await,
        Command::ReleaseWorktree {
            task_id: lease.task_id.clone(),
            repository: lease.repository.clone(),
            owner_session: lease.owner_session.clone(),
        },
    )
    .await;
    assert!(matches!(release.outcome, Outcome::Rejected { .. }));
    assert_eq!(
        std::fs::read(repo.join("tracked")).unwrap(),
        b"owner dirty\0bytes"
    );
    assert_eq!(
        std::fs::read(repo.join("untracked")).unwrap(),
        b"owner untracked\0bytes"
    );
    assert_eq!(
        std::fs::read(Path::new(&lease.path).join("tracked")).unwrap(),
        b"worker dirty"
    );
    assert_eq!(
        std::fs::read(Path::new(&lease.path).join("scratch")).unwrap(),
        b"worker scratch"
    );
    engine.close().await.unwrap();
}

/// Builds the refusal outcome the engine returns for one reason code.
/// Takes the code. Returns `Outcome::Rejected` carrying it.
fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

async fn admit(engine: &Engine, repo: &Path) {
    assert_eq!(
        apply(
            engine,
            "admit",
            2,
            Command::AdmitRepository {
                project_id: ProjectId::new("project").unwrap(),
                repository: repo.to_str().unwrap().into(),
            }
        )
        .await
        .outcome,
        Outcome::Applied
    );
}

#[tokio::test]
async fn concurrent_requests_have_one_writer_and_independent_task_branches() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    let (engine, config) = setup(&root, &repo).await;
    admit(&engine, &repo).await;
    let at = revision(&engine).await;
    let (first, second) = tokio::join!(
        send(
            &engine,
            request("one", at, allocate(&repo, "owner-one", None))
        ),
        send(
            &engine,
            request("two", at, allocate(&repo, "owner-two", Some("native-two")))
        )
    );
    let results = [first, second];
    assert_eq!(
        results
            .iter()
            .filter(|r| r.outcome == Outcome::Applied)
            .count(),
        1
    );
    let loser = results
        .iter()
        .find(|r| r.outcome != Outcome::Applied)
        .unwrap();
    assert_eq!(loser.outcome, rejected("stale_revision"));
    assert_eq!(
        apply(
            &engine,
            "third",
            revision(&engine).await,
            allocate(&repo, "owner-three", None)
        )
        .await
        .outcome,
        rejected("writer_already_owned")
    );
    assert_eq!(leases(&engine).await.len(), 1);
    assert_eq!(
        apply(
            &engine,
            "pin-two",
            revision(&engine).await,
            Command::PinTask {
                context: ExecutionContext {
                    project_id: ProjectId::new("project").unwrap(),
                    task_id: TaskId::new("second-task").unwrap(),
                    config_id: config.id,
                    snapshot_id: SnapshotId::new("second-snapshot").unwrap(),
                    baseline_id: BaselineId::new("baseline").unwrap(),
                    allowed_destinations: vec![],
                    budget_ref: CredentialRef::new("budget").unwrap(),
                }
            }
        )
        .await
        .outcome,
        Outcome::Applied
    );
    let mut command = allocate(&repo, "third-owner", None);
    if let Command::AllocateWorktree { task_id, .. } = &mut command {
        *task_id = TaskId::new("second-task").unwrap();
    }
    assert_eq!(
        apply(&engine, "allocate-two", revision(&engine).await, command)
            .await
            .outcome,
        Outcome::Applied
    );
    let listing = git(&repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(listing.matches("branch refs/heads/crazyagent/").count(), 2);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn hostile_config_filters_and_attributes_never_execute() {
    for hostile in ["filter", "include", "fsmonitor", "attributes", "alternate"] {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let repo = repository(&root, "repo");
        let marker = root.join("executed");
        let command = format!("touch {}", marker.display());
        match hostile {
            "filter" => {
                git(&repo, &["config", "filter.hostile.smudge", &command]);
                std::fs::write(repo.join(".git/info/attributes"), "* filter=hostile\n").unwrap();
            }
            "include" => {
                let included = root.join("included");
                std::fs::write(
                    &included,
                    format!("[filter \"hostile\"]\n\tsmudge = {command}\n"),
                )
                .unwrap();
                std::fs::write(repo.join(".git/info/attributes"), "* filter=hostile\n").unwrap();
                git(
                    &repo,
                    &["config", "include.path", included.to_str().unwrap()],
                );
            }
            "fsmonitor" => {
                git(&repo, &["config", "core.fsmonitor", &command]);
            }
            "attributes" => {
                std::fs::write(repo.join(".gitattributes"), "* filter=hostile\n").unwrap();
                git(&repo, &["add", ".gitattributes"]);
                git(
                    &repo,
                    &[
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@invalid",
                        "commit",
                        "-m",
                        "attributes",
                    ],
                );
            }
            "alternate" => {
                std::fs::write(
                    repo.join(".git/objects/info/alternates"),
                    "/outside/objects\n",
                )
                .unwrap();
            }
            _ => unreachable!(),
        };
        let (engine, _) = setup(&root, &repo).await;
        let admission = apply(
            &engine,
            "admit",
            2,
            Command::AdmitRepository {
                project_id: ProjectId::new("project").unwrap(),
                repository: repo.to_str().unwrap().into(),
            },
        )
        .await;
        let result = if admission.outcome == Outcome::Applied {
            (
                "allocation",
                apply(
                    &engine,
                    "allocate",
                    revision(&engine).await,
                    allocate(&repo, "owner", None),
                )
                .await
                .outcome,
            )
        } else {
            ("admission", admission.outcome)
        };
        assert!(!marker.exists(), "hostile {hostile} executed");
        let expected = match hostile {
            "attributes" => ("allocation", Outcome::Applied),
            "alternate" => (
                "admission",
                rejected("unsupported_shared_or_partial_git_metadata"),
            ),
            "filter" => (
                "admission",
                rejected("repository_config_refused:filter_driver"),
            ),
            "include" => ("admission", rejected("repository_config_refused:include")),
            "fsmonitor" => (
                "admission",
                rejected("repository_config_refused:core_fsmonitor"),
            ),
            _ => unreachable!(),
        };
        assert_eq!(result, expected, "hostile {hostile}");
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn hooks_are_disabled_and_config_changes_after_admission_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    let marker = root.join("hook-executed");
    for name in ["post-checkout", "reference-transaction"] {
        let path = repo.join(".git/hooks").join(name);
        std::fs::write(&path, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let (engine, _) = setup(&root, &repo).await;
    admit(&engine, &repo).await;
    assert_eq!(
        apply(
            &engine,
            "allocate",
            revision(&engine).await,
            allocate(&repo, "owner", None)
        )
        .await
        .outcome,
        Outcome::Applied
    );
    assert!(!marker.exists());
    engine.close().await.unwrap();
    let other = root.join("other-engine");
    std::fs::create_dir(&other).unwrap();
    let (engine, _) = setup(&other, &repo).await;
    admit(&engine, &repo).await;
    git(
        &repo,
        &[
            "config",
            "core.fsmonitor",
            &format!("touch {}", marker.display()),
        ],
    );
    let allocation = apply(
        &engine,
        "allocate",
        revision(&engine).await,
        allocate(&repo, "owner", None),
    )
    .await;
    assert!(matches!(allocation.outcome, Outcome::Rejected { .. }));
    assert!(!marker.exists());
    engine.close().await.unwrap();
}

#[tokio::test]
async fn outside_paths_symlinks_and_metadata_swaps_fail_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    let outside = repository(&root, "outside");
    let (engine, config) = setup(&root, &repo).await;
    let admission = apply(
        &engine,
        "outside",
        2,
        Command::AdmitRepository {
            project_id: ProjectId::new("project").unwrap(),
            repository: outside.to_str().unwrap().into(),
        },
    )
    .await;
    assert_eq!(admission.outcome, rejected("repository_not_in_project"));
    admit(&engine, &repo).await;
    std::os::unix::fs::symlink(&repo, root.join("alias")).unwrap();
    let allocation = apply(
        &engine,
        "alias",
        revision(&engine).await,
        allocate(&root.join("alias"), "owner", None),
    )
    .await;
    assert_eq!(allocation.outcome, rejected("repository_not_admitted"));
    assert!(leases(&engine).await.is_empty());
    assert_eq!(
        apply(
            &engine,
            "allocate",
            revision(&engine).await,
            allocate(&repo, "owner", None)
        )
        .await
        .outcome,
        Outcome::Applied
    );
    let lease = leases(&engine).await.remove(0);
    engine.close().await.unwrap();
    std::fs::rename(&lease.path, root.join("preserved-worktree")).unwrap();
    std::os::unix::fs::symlink(&outside, &lease.path).unwrap();
    let engine = Engine::open(&root.join("db"), config).await.unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Uncertain
    );
    assert_eq!(
        std::fs::read(outside.join("tracked")).unwrap(),
        b"committed\n"
    );
    assert_eq!(
        std::fs::read(root.join("preserved-worktree/tracked")).unwrap(),
        b"committed\n"
    );
    assert!(matches!(
        apply(
            &engine,
            "second",
            revision(&engine).await,
            allocate(&repo, "second", None)
        )
        .await
        .outcome,
        Outcome::Rejected { .. }
    ));
    engine.close().await.unwrap();
}

#[tokio::test]
async fn multiple_repositories_require_individual_admission() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    let other = repository(&root, "other");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.join("global"), &root.join("project")).unwrap();
    let engine = Engine::open(&root.join("db"), config.clone())
        .await
        .unwrap();
    engine.start_owner_runtime().await.unwrap();
    assert_eq!(
        apply(
            &engine,
            "project",
            0,
            Command::CreateProject {
                project_id: ProjectId::new("project").unwrap(),
                spec: ProjectSpec {
                    name: "Product".into(),
                    repositories: vec![
                        repo.to_str().unwrap().into(),
                        other.to_str().unwrap().into()
                    ],
                }
            }
        )
        .await
        .outcome,
        Outcome::Applied
    );
    for (index, repository) in [&repo, &other].iter().enumerate() {
        assert_eq!(
            apply(
                &engine,
                &format!("admit-{index}"),
                revision(&engine).await,
                Command::AdmitRepository {
                    project_id: ProjectId::new("project").unwrap(),
                    repository: repository.to_str().unwrap().into(),
                }
            )
            .await
            .outcome,
            Outcome::Applied
        );
    }
    assert_eq!(
        apply(
            &engine,
            "pin",
            revision(&engine).await,
            Command::PinTask {
                context: ExecutionContext {
                    project_id: ProjectId::new("project").unwrap(),
                    task_id: TaskId::new("task").unwrap(),
                    config_id: config.id,
                    snapshot_id: SnapshotId::new("snapshot").unwrap(),
                    baseline_id: BaselineId::new("baseline").unwrap(),
                    allowed_destinations: vec![],
                    budget_ref: CredentialRef::new("budget").unwrap(),
                }
            }
        )
        .await
        .outcome,
        Outcome::Applied
    );
    assert_eq!(
        apply(
            &engine,
            "allocate-one",
            revision(&engine).await,
            allocate(&repo, "owner-one", None)
        )
        .await
        .outcome,
        Outcome::Applied
    );
    assert_eq!(
        apply(
            &engine,
            "allocate-two",
            revision(&engine).await,
            allocate(&other, "owner-two", None)
        )
        .await
        .outcome,
        Outcome::Applied
    );
    let leases = leases(&engine).await;
    assert_eq!(leases.len(), 2);
    assert_ne!(leases[0].path, leases[1].path);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn lease_status_detects_replacement_without_restart() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    let (engine, _) = setup(&root, &repo).await;
    admit(&engine, &repo).await;
    assert_eq!(
        apply(
            &engine,
            "allocate",
            revision(&engine).await,
            allocate(&repo, "owner", None)
        )
        .await
        .outcome,
        Outcome::Applied
    );
    let lease = leases(&engine).await.remove(0);
    std::fs::rename(
        Path::new(&lease.path).join(".git"),
        Path::new(&lease.path).join("preserved-git-pointer"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        repo.join(".git/config"),
        Path::new(&lease.path).join(".git"),
    )
    .unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Uncertain
    );
    engine.close().await.unwrap();
}

/// Reads the admin folder a worktree's `.git` link names. Takes the worktree path.
/// Returns the folder inside the repository's `.git/worktrees`.
fn admin_folder(worktree: &Path) -> PathBuf {
    let link = std::fs::read_to_string(worktree.join(".git")).unwrap();
    PathBuf::from(link.strip_prefix("gitdir: ").unwrap().trim_end())
}

#[tokio::test]
async fn entries_agents_may_create_mark_only_the_lease_they_touch() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    let (engine, _) = setup(&root, &repo).await;
    admit(&engine, &repo).await;
    assert_eq!(
        apply(
            &engine,
            "allocate",
            revision(&engine).await,
            allocate(&repo, "owner", None)
        )
        .await
        .outcome,
        Outcome::Applied
    );
    let lease = leases(&engine).await.remove(0);
    assert_eq!(lease.state, crazyagent::worktrees::LeaseState::Ready);

    let blobs = root.join("blobs");
    std::fs::create_dir(&blobs).unwrap();
    let mut names = Vec::new();
    for index in 0..9000 {
        std::fs::write(blobs.join(index.to_string()), format!("loose {index}\n")).unwrap();
        names.push(format!("../blobs/{index}"));
    }
    let mut hash = vec!["hash-object", "-w"];
    hash.extend(names.iter().map(String::as_str));
    let written = git(&repo, &hash);
    assert_eq!(written.lines().count(), 9000);
    let first = written.lines().next().unwrap().to_owned();
    let fan = repo.join(".git/objects").join(&first[..2]);
    assert!(fan.join(&first[2..]).is_file());

    let outside = root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.join(".git/objects/zz")).unwrap();
    std::os::unix::fs::symlink(&outside, fan.join("tmp_obj_left")).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "agent",
            root.join("agent").to_str().unwrap(),
        ],
    );
    let agent = admin_folder(&root.join("agent"));
    std::os::unix::fs::symlink(&outside, agent.join("planted")).unwrap();
    std::fs::hard_link(agent.join("index"), agent.join("index-link")).unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Ready
    );

    let planted = fan.join("f".repeat(first.len() - 2));
    std::os::unix::fs::symlink(outside.join("object"), &planted).unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Uncertain
    );
    let readmission = apply(
        &engine,
        "readmit",
        revision(&engine).await,
        Command::AdmitRepository {
            project_id: ProjectId::new("project").unwrap(),
            repository: repo.to_str().unwrap().into(),
        },
    )
    .await;
    assert_eq!(
        readmission.outcome,
        rejected(&format!(
            "untrusted_git_entry:objects/{}/{}",
            &first[..2],
            "f".repeat(first.len() - 2)
        ))
    );
    std::fs::remove_file(&planted).unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Ready
    );

    let own = admin_folder(Path::new(&lease.path));
    std::os::unix::fs::symlink(&outside, own.join("planted")).unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Uncertain
    );
    std::fs::remove_file(own.join("planted")).unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Ready
    );
    engine.close().await.unwrap();
}

/// v2.5 L6: an engine host ends with its storage worker. A started engine dropped without
/// close leaves no host in the process, so its store reopens here and the open sweep, whose
/// opening fence refuses while a host of the store runs, marks the Ready lease
/// RecoveryRequired. Gives up after 30 s with the last open error.
#[tokio::test]
async fn a_started_engine_dropped_without_close_leaves_no_host_and_its_store_reopens() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    let (engine, config) = setup(&root, &repo).await;
    admit(&engine, &repo).await;
    let allocation = allocate(&repo, "owner", None);
    let at = revision(&engine).await;
    assert_eq!(
        apply(&engine, "allocate", at, allocation).await.outcome,
        Outcome::Applied
    );
    drop(engine);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let engine = loop {
        match Engine::open(&root.join("db"), config.clone()).await {
            Ok(engine) => break engine,
            Err(error) if std::time::Instant::now() < until => {
                assert!(!error.0.is_empty());
                tokio::task::yield_now().await;
            }
            Err(error) => panic!("the dropped engine's store did not reopen: {error:?}"),
        }
    };
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::RecoveryRequired
    );
    engine.close().await.unwrap();
}

/// Sends one wire request through a service's client and asserts it applied. Inputs: the
/// client, the id, the expected revision and the command. Output: the reply's revision.
async fn wired(client: &crazyagent::engine::Client, id: &str, at: u64, command: Command) -> u64 {
    let reply = client.request(request(id, at, command)).await.unwrap();
    assert_eq!(reply.outcome, Outcome::Applied, "{id}");
    assert!(reply.revision.value() > at, "{id} moves the revision");
    reply.revision.value()
}

/// A served engine a row drives: the service and its stop sender, its wire and owner
/// clients, the quit form, and the revision after the setup's writes.
struct Served {
    service: crazyagent::engine::bootstrap::EngineService,
    stop: tokio::sync::watch::Sender<bool>,
    client: crazyagent::engine::Client,
    owner: crazyagent::engine::owner::OwnerClient,
    form: crazyagent::client_controller::OwnerController,
    at: u64,
}

/// Starts a service on the root and attaches the quit form before any write, then creates
/// the project, pins its task and admits the repository over the wire. Inputs: the root,
/// the repository and the configuration. Output: the served engine.
async fn served(root: &Path, repo: &Path, config: &crazyagent::config::EffectiveConfig) -> Served {
    use crazyagent::engine::owner::OwnerClientKind;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let service =
        crazyagent::engine::bootstrap::EngineService::start(root, "e.sock", config, &stopped)
            .await
            .unwrap();
    let timeout = std::time::Duration::from_secs(10);
    let (client, owner, _) = crazyagent::engine::Client::new(root.join("e.sock"), timeout)
        .unwrap()
        .with_owner(OwnerClientKind::Tui)
        .await
        .unwrap();
    let form = crazyagent::client_controller::OwnerController::attach(
        root,
        OwnerClientKind::Desktop,
        timeout,
    )
    .await
    .unwrap();
    let project = ProjectId::new("project").unwrap();
    let spec = ProjectSpec {
        name: "Product".into(),
        repositories: vec![repo.to_str().unwrap().into()],
    };
    let created = Command::CreateProject {
        project_id: project.clone(),
        spec,
    };
    let at = wired(&client, "project", 0, created).await;
    let context = ExecutionContext {
        project_id: project.clone(),
        task_id: TaskId::new("task").unwrap(),
        config_id: config.id.clone(),
        snapshot_id: SnapshotId::new("snapshot").unwrap(),
        baseline_id: BaselineId::new("baseline").unwrap(),
        allowed_destinations: vec![],
        budget_ref: CredentialRef::new("budget").unwrap(),
    };
    let at = wired(&client, "pin", at, Command::PinTask { context }).await;
    let admission = Command::AdmitRepository {
        project_id: project,
        repository: repo.to_str().unwrap().into(),
    };
    let at = wired(&client, "admit", at, admission).await;
    Served {
        service,
        stop,
        client,
        owner,
        form,
        at,
    }
}

/// v2.5 L6 with V250-OWN-REVISION-RETRY (leader 03:53): the quit form reads its settings,
/// the engine then saves a lease through its own writes (an allocation's WorktreeReserved
/// and LeaseSaved), and the form's save is Applied after one resend under a new request id,
/// since the form's read holds no lease and reads the same again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_form_save_after_an_engine_lease_save_lands_after_one_resend() {
    use crazyagent::engine::owner::{OwnerMutation, OwnerResponse, OwnerResult};
    use crazyagent::quit_settings::{LeftRunning, OnQuit, QuitSettings};
    const MINE: QuitSettings = QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::Never,
        hours: 12,
    };
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let repo = repository(&root, "repo");
    let config = resolve_config(&root.join("global"), &root.join("project")).unwrap();
    let Served {
        mut service,
        stop,
        client,
        owner,
        mut form,
        at,
    } = served(&root, &repo, &config).await;
    assert_ne!(
        form.quit_settings().await.unwrap(),
        MINE,
        "the form starts from the defaults"
    );
    wired(&client, "allocate", at, allocate(&repo, "owner", None)).await;
    form.begin_mutation(OwnerMutation::SetQuitSettings { settings: MINE }, 1)
        .unwrap();
    let first = form.pending().unwrap().request().request_id.clone();
    let saved = form.send_pending().await.unwrap();
    let OwnerResponse::Applied { operation } = saved else {
        panic!("the form's save must land after one resend: {saved:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert_ne!(
        operation.provenance.operation_id, first,
        "the resend is its own request"
    );
    assert_eq!(form.quit_settings().await.unwrap(), MINE);
    drop(form);
    drop(owner);
    drop(client);
    service.shutdown().await.unwrap();
    assert!(!*stop.borrow(), "the row stops the service itself");
}

/// The revision and kind of each logged worktrees event other than a snapshot run, in log
/// order. Input: the closed store's path. Output: the pairs.
fn logged(database: &Path) -> Vec<(i64, String)> {
    let db = rusqlite::Connection::open(database).unwrap();
    db.prepare(
        "SELECT revision, kind FROM state_log WHERE aggregate='worktrees'
         AND kind NOT IN ('SnapshotChunk','SnapshotDone') ORDER BY seq",
    )
    .unwrap()
    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// Asserts that replay of the closed store's log rebuilds both worktree tables as they
/// stand, inside a transaction that is rolled back. Input: the store's path. Output: none.
fn replays_to_live(database: &Path) {
    use crazyagent::state_log::replay::rows;
    let mut db = rusqlite::Connection::open(database).unwrap();
    let tables = |db: &rusqlite::Connection| {
        (
            rows(db, "repository_admissions").unwrap(),
            rows(db, "worktree_leases").unwrap(),
        )
    };
    let live = tables(&db);
    assert!(!live.0.is_empty() && !live.1.is_empty());
    let tx = db.transaction().unwrap();
    crazyagent::worktrees::log::rebuild(&tx).unwrap();
    assert_eq!(tables(&tx), live, "replay equals the live tables");
}

/// v2.5 L6 Q8 item 3: a wire admission appends RepositoryAdmitted through `own` and replies
/// one revision later; an allocation appends WorktreeReserved, then the checkout's
/// LeaseSaved, each through `own`, and replies one revision after the save. Replay of the
/// store rebuilds both worktree tables as they stand.
#[tokio::test]
async fn wire_lease_writes_are_logged_and_replay_to_the_live_tables() {
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let repo = repository(&root, "repo");
    let (engine, _config) = setup(&root, &repo).await;
    let admitted = apply(
        &engine,
        "admit",
        2,
        Command::AdmitRepository {
            project_id: ProjectId::new("project").unwrap(),
            repository: repo.to_str().unwrap().into(),
        },
    )
    .await;
    assert_eq!(admitted.outcome, Outcome::Applied);
    assert_eq!(admitted.revision.value(), 4);
    let allocated = apply(&engine, "allocate", 4, allocate(&repo, "owner", None)).await;
    assert_eq!(allocated.outcome, Outcome::Applied);
    assert_eq!(allocated.revision.value(), 7);
    engine.close().await.unwrap();
    let database = root.join("db");
    assert_eq!(
        logged(&database),
        [
            (3, "RepositoryAdmitted".to_owned()),
            (5, "WorktreeReserved".to_owned()),
            (6, "LeaseSaved".to_owned()),
        ]
    );
    replays_to_live(&database);
}

/// v2.5 L6 Q9: before an owner runtime starts, a wire admission is refused at its fence
/// (`owner_runtime_stale`) and writes nothing: once the runtime starts, the same request at
/// the same revision is applied.
#[tokio::test]
async fn a_wire_admission_before_an_owner_runtime_starts_is_refused_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let repo = repository(&root, "repo");
    let (engine, _config) = setup_unstarted(&root, &repo).await;
    let admit = request(
        "admit",
        2,
        Command::AdmitRepository {
            project_id: ProjectId::new("project").unwrap(),
            repository: repo.to_str().unwrap().into(),
        },
    );
    assert_eq!(
        engine.apply(admit.clone()).await.unwrap_err().0,
        "owner_runtime_stale"
    );
    engine.start_owner_runtime().await.unwrap();
    let reply = send(&engine, admit).await;
    assert_eq!(reply.outcome, Outcome::Applied);
    assert_eq!(reply.revision.value(), 4);
    engine.close().await.unwrap();
}

#[path = "worktree_ownership/checkouts.rs"]
mod checkouts;
