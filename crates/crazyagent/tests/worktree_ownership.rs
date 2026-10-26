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

async fn apply(engine: &Engine, id: &str, revision: u64, command: Command) -> WireReply {
    engine.apply(request(id, revision, command)).await.unwrap()
}

async fn setup(root: &Path, repo: &Path) -> (Engine, crazyagent::config::EffectiveConfig) {
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
        3,
        allocate(&repo, "owner", Some("native-child")),
    );
    let first = engine.apply(operation.clone()).await.unwrap();
    assert_eq!(first.outcome, Outcome::Applied);
    assert_eq!(engine.apply(operation.clone()).await.unwrap(), first);
    let lease = leases(&engine).await.remove(0);
    assert_eq!(
        std::fs::read(Path::new(&lease.path).join("tracked")).unwrap(),
        b"committed\n"
    );
    assert_eq!(lease.native_descendant.as_deref(), Some("native-child"));
    std::fs::write(Path::new(&lease.path).join("tracked"), b"worker dirty").unwrap();
    std::fs::write(Path::new(&lease.path).join("scratch"), b"worker scratch").unwrap();
    let second = apply(&engine, "second", 4, allocate(&repo, "second-owner", None)).await;
    assert!(matches!(second.outcome, Outcome::Rejected { .. }));
    engine.close().await.unwrap();
    let engine = Engine::open(&root.join("db"), config).await.unwrap();
    let recovered = leases(&engine).await.remove(0);
    assert_eq!(
        recovered.state,
        crazyagent::worktrees::LeaseState::RecoveryRequired
    );
    assert_eq!(engine.apply(operation).await.unwrap(), first);
    let release = apply(
        &engine,
        "release",
        4,
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
    let (first, second) = tokio::join!(
        engine.apply(request("one", 3, allocate(&repo, "owner-one", None))),
        engine.apply(request(
            "two",
            3,
            allocate(&repo, "owner-two", Some("native-two"))
        ))
    );
    let results = [first.unwrap(), second.unwrap()];
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
        apply(&engine, "third", 4, allocate(&repo, "owner-three", None))
            .await
            .outcome,
        rejected("writer_already_owned")
    );
    assert_eq!(leases(&engine).await.len(), 1);
    assert_eq!(
        apply(
            &engine,
            "pin-two",
            4,
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
        apply(&engine, "allocate-two", 5, command).await.outcome,
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
                apply(&engine, "allocate", 3, allocate(&repo, "owner", None))
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
        apply(&engine, "allocate", 3, allocate(&repo, "owner", None))
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
    let allocation = apply(&engine, "allocate", 3, allocate(&repo, "owner", None)).await;
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
        3,
        allocate(&root.join("alias"), "owner", None),
    )
    .await;
    assert_eq!(allocation.outcome, rejected("repository_not_admitted"));
    assert!(leases(&engine).await.is_empty());
    assert_eq!(
        apply(&engine, "allocate", 3, allocate(&repo, "owner", None))
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
        apply(&engine, "second", 4, allocate(&repo, "second", None))
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
                1 + index as u64,
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
            3,
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
            4,
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
            5,
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
        apply(&engine, "allocate", 3, allocate(&repo, "owner", None))
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

#[tokio::test]
async fn ordinary_remote_configuration_does_not_grant_network_authority() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://unreachable.invalid/product.git",
        ],
    );
    git(&repo, &["config", "branch.main.remote", "origin"]);
    git(&repo, &["config", "branch.main.merge", "refs/heads/main"]);
    let (engine, _) = setup(&root, &repo).await;
    admit(&engine, &repo).await;
    assert_eq!(
        apply(&engine, "allocate", 3, allocate(&repo, "owner", None))
            .await
            .outcome,
        Outcome::Applied
    );
    engine.close().await.unwrap();
}

fn destination(root: &Path, repo: &Path) -> (PathBuf, String) {
    use sha2::{Digest, Sha256};
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{}\0task", repo.display()).as_bytes())
    );
    (
        root.join("worktrees").join(&digest),
        format!("crazyagent/{digest}"),
    )
}

#[tokio::test]
async fn preexisting_branch_and_path_are_preserved() {
    for conflict in ["branch", "path", "symlink"] {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let repo = repository(&root, "repo");
        let (engine, _) = setup(&root, &repo).await;
        admit(&engine, &repo).await;
        let (path, branch) = destination(&root, &repo);
        std::fs::create_dir(root.join("worktrees")).unwrap();
        std::fs::set_permissions(
            root.join("worktrees"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        match conflict {
            "branch" => {
                git(&repo, &["branch", &branch]);
            }
            "path" => {
                std::fs::create_dir(&path).unwrap();
                std::fs::write(path.join("owner-bytes"), b"preserve").unwrap();
            }
            "symlink" => {
                std::os::unix::fs::symlink(&repo, &path).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(matches!(
            apply(&engine, "allocate", 3, allocate(&repo, "owner", None))
                .await
                .outcome,
            Outcome::Rejected { .. }
        ));
        assert!(leases(&engine).await.is_empty());
        assert_eq!(std::fs::read(repo.join("tracked")).unwrap(), b"committed\n");
        if conflict == "path" {
            assert_eq!(
                std::fs::read(path.join("owner-bytes")).unwrap(),
                b"preserve"
            );
        }
        if conflict == "branch" {
            assert_eq!(
                git(&repo, &["rev-parse", &branch]),
                git(&repo, &["rev-parse", "HEAD"])
            );
        }
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn interrupted_allocation_is_retained() {
    if let Some(root) = std::env::var_os("CRAZYAGENT_T6_INTERRUPT_ROOT") {
        let root = PathBuf::from(root);
        let config = resolve_config(&root.join("global"), &root.join("project")).unwrap();
        let engine = Engine::open(&root.join("db"), config).await.unwrap();
        assert_eq!(
            apply(
                &engine,
                "interrupted",
                3,
                allocate(&root.join("repo"), "owner", Some("native-child"))
            )
            .await
            .outcome,
            Outcome::Applied
        );
        engine.close().await.unwrap();
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    for index in 0..64 {
        std::fs::write(repo.join(format!("file-{index}")), vec![index as u8; 2048]).unwrap();
    }
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid",
            "commit",
            "-m",
            "bounded files",
        ],
    );
    let (engine, config) = setup(&root, &repo).await;
    admit(&engine, &repo).await;
    engine.close().await.unwrap();
    let mut child = crazyagent::spawn_gate::spawn(
        Process::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "interrupted_allocation_is_retained",
                "--nocapture",
            ])
            .env("CRAZYAGENT_T6_INTERRUPT_ROOT", &root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    let db = rusqlite::Connection::open_with_flags(
        root.join("db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut observed = false;
    for _ in 0..2000 {
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM worktree_leases", [], |row| row.get(0))
            .unwrap();
        if count == 1 {
            observed = true;
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "allocation exited before reservation was observed"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(observed, "durable reservation was not observed");
    let body: String = db
        .query_row("SELECT body FROM worktree_leases", [], |row| row.get(0))
        .unwrap();
    let record: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        record["lease"]["state"], "Reserved",
        "kill must interrupt provisioning, not follow completion"
    );
    drop(db);
    let engine = Engine::open(&root.join("db"), config).await.unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Uncertain
    );
    assert!(matches!(
        apply(&engine, "retry", 3, allocate(&repo, "other-owner", None))
            .await
            .outcome,
        Outcome::Rejected { .. }
    ));
    assert_eq!(std::fs::read(repo.join("tracked")).unwrap(), b"committed\n");
    engine.close().await.unwrap();
}

#[tokio::test]
async fn missing_branch_and_replaced_repository_are_uncertain() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    let (engine, config) = setup(&root, &repo).await;
    admit(&engine, &repo).await;
    assert_eq!(
        apply(&engine, "allocate", 3, allocate(&repo, "owner", None))
            .await
            .outcome,
        Outcome::Applied
    );
    let lease = leases(&engine).await.remove(0);
    git(
        &repo,
        &["update-ref", "-d", &format!("refs/heads/{}", lease.branch)],
    );
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Uncertain
    );
    engine.close().await.unwrap();
    std::fs::rename(repo.join(".git"), root.join("preserved-metadata")).unwrap();
    let replacement = repository(&root, "replacement");
    std::os::unix::fs::symlink(replacement.join(".git"), repo.join(".git")).unwrap();
    let engine = Engine::open(&root.join("db"), config).await.unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Uncertain
    );
    assert!(matches!(
        apply(&engine, "other", 4, allocate(&repo, "other", None))
            .await
            .outcome,
        Outcome::Rejected { .. }
    ));
    assert_eq!(
        std::fs::read(Path::new(&lease.path).join("tracked")).unwrap(),
        b"committed\n"
    );
    engine.close().await.unwrap();
}

fn commit(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

fn nested_text(module: usize, section: usize, file: usize) -> String {
    format!("module {module} section {section} file {file}\n")
}

fn large_bytes() -> Vec<u8> {
    (0..50 * 1024 * 1024)
        .map(|index: usize| (index.wrapping_mul(31) % 251) as u8)
        .collect()
}

const LFS_POINTER: &str = concat!(
    "version https://git-lfs.github.com/spec/v1\n",
    "oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n",
    "size 12345\n"
);

#[tokio::test]
async fn a_real_size_repository_gets_a_ready_lease_with_its_links_large_file_and_lfs_pointer() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
    for module in 0..16 {
        for section in 0..8 {
            let folder = repo.join(format!("src/m{module}/s{section}"));
            std::fs::create_dir_all(&folder).unwrap();
            for file in 0..16 {
                std::fs::write(
                    folder.join(format!("f{file}.txt")),
                    nested_text(module, section, file),
                )
                .unwrap();
            }
        }
    }
    let deep = repo.join("deep/a/b/c/d/e/f/g/h/i/j/k/l");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("leaf.txt"), b"deep leaf\n").unwrap();
    std::fs::write(repo.join("big.bin"), large_bytes()).unwrap();
    std::fs::write(repo.join("AGENTS.md"), b"agent rules\n").unwrap();
    std::os::unix::fs::symlink("AGENTS.md", repo.join("CLAUDE.md")).unwrap();
    std::fs::create_dir(repo.join("docs")).unwrap();
    std::os::unix::fs::symlink("../src/m0", repo.join("docs/current")).unwrap();
    std::fs::write(root.join("outside-file"), b"outside the lease\n").unwrap();
    std::fs::create_dir(repo.join("links")).unwrap();
    std::os::unix::fs::symlink("../../outside-file", repo.join("links/escape")).unwrap();
    std::os::unix::fs::symlink(root.join("outside-file"), repo.join("links/absolute")).unwrap();
    std::fs::write(
        repo.join(".gitattributes"),
        "*.psd filter=lfs diff=lfs merge=lfs -text\n",
    )
    .unwrap();
    std::fs::create_dir(repo.join("art")).unwrap();
    std::fs::write(repo.join("art/cover.psd"), LFS_POINTER).unwrap();
    std::fs::create_dir(repo.join("bin")).unwrap();
    std::fs::write(repo.join("bin/run.sh"), b"#!/bin/sh\necho run\n").unwrap();
    std::fs::set_permissions(
        repo.join("bin/run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit(&repo, "real size");
    std::fs::write(root.join("global"), "version = 1\ntimeout_ms = 30000\n").unwrap();
    let (engine, _) = setup(&root, &repo).await;
    admit(&engine, &repo).await;
    assert_eq!(
        apply(&engine, "allocate", 3, allocate(&repo, "owner", None))
            .await
            .outcome,
        Outcome::Applied
    );
    let lease = leases(&engine).await.remove(0);
    assert_eq!(lease.state, crazyagent::worktrees::LeaseState::Ready);
    let path = Path::new(&lease.path);
    for module in 0..16 {
        for section in 0..8 {
            for file in 0..16 {
                assert_eq!(
                    std::fs::read_to_string(
                        path.join(format!("src/m{module}/s{section}/f{file}.txt"))
                    )
                    .unwrap(),
                    nested_text(module, section, file)
                );
            }
        }
    }
    assert_eq!(
        std::fs::read(path.join("deep/a/b/c/d/e/f/g/h/i/j/k/l/leaf.txt")).unwrap(),
        b"deep leaf\n"
    );
    assert!(std::fs::read(path.join("big.bin")).unwrap() == large_bytes());
    assert_eq!(
        std::fs::read_link(path.join("CLAUDE.md")).unwrap(),
        Path::new("AGENTS.md")
    );
    assert_eq!(
        std::fs::read(path.join("CLAUDE.md")).unwrap(),
        b"agent rules\n"
    );
    assert_eq!(
        std::fs::read_link(path.join("docs/current")).unwrap(),
        Path::new("../src/m0")
    );
    assert!(path.join("docs/current/s0/f0.txt").is_file());
    for link in ["links/escape", "links/absolute"] {
        assert!(
            std::fs::symlink_metadata(path.join(link)).is_err(),
            "{link} must not reach outside the lease"
        );
    }
    assert_eq!(
        git(
            path,
            &["ls-files", "-v", "--", "links/escape", "links/absolute"]
        ),
        "S links/absolute\nS links/escape\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("art/cover.psd")).unwrap(),
        LFS_POINTER
    );
    assert_ne!(
        std::fs::metadata(path.join("bin/run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o100,
        0
    );
    assert_eq!(git(path, &["status", "--porcelain"]), "");
    assert_eq!(
        std::fs::read(root.join("outside-file")).unwrap(),
        b"outside the lease\n"
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn submodule_entries_and_content_git_would_convert_retain_ownership_without_checkout() {
    for kind in ["submodule", "conversion"] {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let repo = repository(&root, "repo");
        let head = git(&repo, &["rev-parse", "HEAD"]);
        match kind {
            "submodule" => {
                git(
                    &repo,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("160000,{},vendor/lib", head.trim_end()),
                    ],
                );
                git(
                    &repo,
                    &[
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@invalid",
                        "commit",
                        "-q",
                        "-m",
                        "submodule",
                    ],
                );
            }
            "conversion" => {
                std::fs::write(
                    repo.join(".gitattributes"),
                    "*.txt working-tree-encoding=UTF-16LE\n",
                )
                .unwrap();
                let wide = "hello\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>();
                std::fs::write(repo.join("notes.txt"), wide).unwrap();
                commit(&repo, "conversion");
            }
            _ => unreachable!(),
        }
        let (engine, _) = setup(&root, &repo).await;
        admit(&engine, &repo).await;
        let expected = match kind {
            "submodule" => "unsupported_or_unsafe_tree_entry",
            "conversion" => "worktree_content_differs_from_commit",
            _ => unreachable!(),
        };
        assert_eq!(
            apply(&engine, "allocate", 3, allocate(&repo, "owner", None))
                .await
                .outcome,
            rejected(expected),
            "{kind}"
        );
        let lease = leases(&engine).await.remove(0);
        assert_eq!(lease.state, crazyagent::worktrees::LeaseState::Uncertain);
        assert_eq!(std::fs::read(repo.join("tracked")).unwrap(), b"committed\n");
        assert!(matches!(
            apply(&engine, "second", 3, allocate(&repo, "other", None))
                .await
                .outcome,
            Outcome::Rejected { .. }
        ));
        engine.close().await.unwrap();
    }
}
