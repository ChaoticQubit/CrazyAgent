use super::*;

/// Makes a real FIFO at `path` with `mkfifo`. Input: the path.
fn fifo(path: &Path) {
    let output = crazyagent::spawn_gate::output(
        Process::new("/usr/bin/mkfifo")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test]
async fn a_worktree_head_naming_a_ref_of_its_own_folder_is_refused_at_once_for_every_lease() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repo = repository(&root, "repo");
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
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            root.join("detached").to_str().unwrap(),
        ],
    );
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
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Ready
    );

    let agent = admin_folder(&root.join("agent"));
    std::fs::create_dir_all(agent.join("refs").join("worktree")).unwrap();
    fifo(&agent.join("refs").join("worktree").join("stuck"));
    let head = agent.join("HEAD");
    let kept = std::fs::read_to_string(&head).unwrap();
    assert_eq!(kept, "ref: refs/heads/agent\n");
    std::fs::write(&head, "ref: refs/worktree/stuck\n").unwrap();
    let started = std::time::Instant::now();
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
        rejected("untrusted_git_entry:worktrees/agent/HEAD")
    );
    for planted in [
        "ref: refs/heads/../worktree/stuck\n",
        "ref: refs/bisect/bad\n",
        "ref: ORIG_HEAD\n",
        "ref: refs/heads/agent\nref: refs/worktree/stuck\n",
    ] {
        std::fs::write(&head, planted).unwrap();
        assert_eq!(
            leases(&engine).await[0].state,
            crazyagent::worktrees::LeaseState::Uncertain,
            "{planted}"
        );
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "the refusals waited on git: {:?}",
        started.elapsed()
    );
    std::fs::write(&head, &kept).unwrap();
    assert_eq!(
        leases(&engine).await[0].state,
        crazyagent::worktrees::LeaseState::Ready
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
            apply(
                &engine,
                "allocate",
                revision(&engine).await,
                allocate(&repo, "owner", None)
            )
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
        engine.start_owner_runtime().await.unwrap();
        assert_eq!(
            apply(
                &engine,
                "interrupted",
                revision(&engine).await,
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
                "checkouts::interrupted_allocation_is_retained",
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
        apply(
            &engine,
            "retry",
            revision(&engine).await,
            allocate(&repo, "other-owner", None)
        )
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
        apply(
            &engine,
            "other",
            revision(&engine).await,
            allocate(&repo, "other", None)
        )
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
            apply(
                &engine,
                "allocate",
                revision(&engine).await,
                allocate(&repo, "owner", None)
            )
            .await
            .outcome,
            rejected(expected),
            "{kind}"
        );
        let lease = leases(&engine).await.remove(0);
        assert_eq!(lease.state, crazyagent::worktrees::LeaseState::Uncertain);
        assert_eq!(std::fs::read(repo.join("tracked")).unwrap(), b"committed\n");
        assert!(matches!(
            apply(
                &engine,
                "second",
                revision(&engine).await,
                allocate(&repo, "other", None)
            )
            .await
            .outcome,
            Outcome::Rejected { .. }
        ));
        engine.close().await.unwrap();
    }
}

fn pin(task: &str, config: &crazyagent::config::EffectiveConfig) -> Command {
    Command::PinTask {
        context: ExecutionContext {
            project_id: ProjectId::new("project").unwrap(),
            task_id: TaskId::new(task).unwrap(),
            config_id: config.id.clone(),
            snapshot_id: SnapshotId::new("snapshot").unwrap(),
            baseline_id: BaselineId::new("baseline").unwrap(),
            allowed_destinations: vec![],
            budget_ref: CredentialRef::new("budget").unwrap(),
        },
    }
}

async fn leases_of(engine: &Engine, task: &str) -> Vec<crazyagent::worktrees::WorktreeLease> {
    let reply = apply(
        engine,
        &format!("leases-{task}"),
        0,
        Command::WorktreeLeases {
            task_id: TaskId::new(task).unwrap(),
        },
    )
    .await;
    let Outcome::Worktrees { leases } = reply.outcome else {
        panic!("leases required")
    };
    leases
}

fn volume(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).unwrap().dev()
}

fn set_devices(value: &mut serde_json::Value, device: u64) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                if key == "device" {
                    *field = serde_json::Value::from(device);
                } else {
                    set_devices(field, device);
                }
            }
        }
        serde_json::Value::Array(items) => {
            items.iter_mut().for_each(|item| set_devices(item, device))
        }
        _ => {}
    }
}

enum Renumber {
    Volume(u64),
    VolumeBesideTheLeaseDirectory(u64),
    LeaseDirectoryOnly(u64),
    Nothing,
}

fn rewrite_rows(database: &Path, renumber: Renumber, uncertain: bool) {
    let db = rusqlite::Connection::open(database).unwrap();
    for table in ["repository_admissions", "worktree_leases"] {
        let rows: Vec<(i64, String)> = db
            .prepare(&format!("SELECT rowid, body FROM {table}"))
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(!rows.is_empty(), "{table}");
        for (rowid, body) in rows {
            let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
            let lease = table == "worktree_leases";
            match renumber {
                Renumber::Volume(device) => set_devices(&mut value, device),
                Renumber::VolumeBesideTheLeaseDirectory(device) => {
                    let kept = value.get("directory").cloned();
                    set_devices(&mut value, device);
                    if let Some(kept) = kept.filter(|_| lease) {
                        value["directory"] = kept;
                    }
                }
                Renumber::LeaseDirectoryOnly(device) if lease => {
                    set_devices(&mut value["directory"], device)
                }
                Renumber::LeaseDirectoryOnly(_) | Renumber::Nothing => {}
            }
            if lease && uncertain {
                value["lease"]["state"] = serde_json::Value::from("Uncertain");
            }
            db.execute(
                &format!("UPDATE {table} SET body=?2 WHERE rowid=?1"),
                rusqlite::params![rowid, serde_json::to_string(&value).unwrap()],
            )
            .unwrap();
        }
    }
}

async fn ready_lease(
    root: &Path,
) -> (
    PathBuf,
    crazyagent::config::EffectiveConfig,
    crazyagent::worktrees::WorktreeLease,
) {
    let repo = repository(root, "repo");
    let (engine, config) = setup(root, &repo).await;
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
    std::fs::write(Path::new(&lease.path).join("scratch"), b"worker scratch").unwrap();
    engine.close().await.unwrap();
    (repo, config, lease)
}

#[tokio::test]
async fn a_reboot_that_renumbers_the_volume_keeps_leases_and_admissions() {
    use crazyagent::worktrees::LeaseState;
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let (repo, config, lease) = ready_lease(&root).await;
    let live = volume(&repo);
    assert_eq!(volume(Path::new(&lease.path)), live);
    rewrite_rows(&root.join("db"), Renumber::Volume(live ^ 1), true);

    let engine = Engine::open(&root.join("db"), config.clone())
        .await
        .unwrap();
    engine.start_owner_runtime().await.unwrap();
    assert_eq!(leases(&engine).await[0].state, LeaseState::RecoveryRequired);
    assert_eq!(
        apply(
            &engine,
            "pin-second",
            revision(&engine).await,
            pin("second-task", &config)
        )
        .await
        .outcome,
        Outcome::Applied
    );
    let second = Command::AllocateWorktree {
        task_id: TaskId::new("second-task").unwrap(),
        repository: repo.to_str().unwrap().into(),
        owner_session: SessionId::new("second-owner").unwrap(),
        native_descendant: None,
    };
    assert_eq!(
        apply(&engine, "allocate-second", revision(&engine).await, second)
            .await
            .outcome,
        Outcome::Applied
    );
    let second_lease = leases_of(&engine, "second-task").await.remove(0);
    assert_eq!(second_lease.state, LeaseState::Ready);
    engine.close().await.unwrap();

    rewrite_rows(&root.join("db"), Renumber::Volume(live ^ 2), false);
    let engine = Engine::open(&root.join("db"), config).await.unwrap();
    assert_eq!(leases(&engine).await[0].state, LeaseState::RecoveryRequired);
    assert_eq!(
        leases_of(&engine, "second-task").await[0].state,
        LeaseState::RecoveryRequired
    );
    engine.close().await.unwrap();
    assert_eq!(
        std::fs::read(Path::new(&lease.path).join("scratch")).unwrap(),
        b"worker scratch"
    );
    assert_eq!(std::fs::read(repo.join("tracked")).unwrap(), b"committed\n");
    assert!(Path::new(&second_lease.path).join("tracked").is_file());
}

#[tokio::test]
async fn a_mixed_device_lease_record_is_still_refused() {
    use crazyagent::worktrees::LeaseState;
    for (renumber, uncertain) in [
        (Renumber::LeaseDirectoryOnly as fn(u64) -> Renumber, false),
        (
            Renumber::VolumeBesideTheLeaseDirectory as fn(u64) -> Renumber,
            true,
        ),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let (repo, config, lease) = ready_lease(&root).await;
        rewrite_rows(&root.join("db"), renumber(volume(&repo) ^ 1), uncertain);
        let engine = Engine::open(&root.join("db"), config).await.unwrap();
        assert_eq!(
            leases(&engine).await[0].state,
            LeaseState::Uncertain,
            "uncertain: {uncertain}"
        );
        engine.close().await.unwrap();
        assert_eq!(
            std::fs::read(Path::new(&lease.path).join("scratch")).unwrap(),
            b"worker scratch"
        );
    }
}

/// One change a lease row makes to a lease worktree before the engine opens again, such as
/// a tracked file removed or an index rewritten. Input: the lease worktree.
type Damage = dyn Fn(&Path);

/// Makes a Ready lease, lets `change` touch its worktree, stores the lease Uncertain with
/// `renumber` applied, opens the engine once more, and runs `after` once it is closed.
/// Inputs: the change and the after check, each given the lease worktree, and the renumber.
/// Output: the lease state the reopened engine reads.
async fn uncertain_after(
    change: impl FnOnce(&Path),
    renumber: fn(u64) -> Renumber,
    after: impl FnOnce(&Path),
) -> crazyagent::worktrees::LeaseState {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let (repo, config, lease) = ready_lease(&root).await;
    change(Path::new(&lease.path));
    rewrite_rows(&root.join("db"), renumber(volume(&repo) ^ 1), true);
    let engine = Engine::open(&root.join("db"), config).await.unwrap();
    let state = leases(&engine).await.remove(0).state;
    engine.close().await.unwrap();
    after(Path::new(&lease.path));
    state
}

/// LEASE-LEGACY-UNCERTAIN: an Uncertain lease whose worktree still matches its own Git
/// index, the checkout as made or with its agent's change staged, is checked again at the
/// next start and waits for recovery.
#[tokio::test]
async fn an_uncertain_lease_whose_worktree_matches_its_index_waits_for_recovery() {
    use crazyagent::worktrees::LeaseState;
    assert_eq!(
        uncertain_after(|_| {}, |_| Renumber::Nothing, |_| {}).await,
        LeaseState::RecoveryRequired,
        "the checkout as made"
    );
    let staged = |worktree: &Path| {
        std::fs::write(worktree.join("tracked"), "the agent's change\n").unwrap();
        git(worktree, &["add", "tracked"]);
    };
    assert_eq!(
        uncertain_after(staged, |_| Renumber::Nothing, |_| {}).await,
        LeaseState::RecoveryRequired,
        "the agent's change, staged"
    );
}

/// LEASE-LEGACY-UNCERTAIN, rv-sec3 N1 and rv-code4 #161 F1: a checkout that failed after
/// its folder was pinned (a tracked file missing or short, no Git index yet, or the index
/// lock of a cut-off Git writer beside a whole index) stays Uncertain at the next start, on
/// a plain volume and on a renumbered one, so it is never requalified on its pins alone.
#[tokio::test]
async fn a_failed_checkout_stays_uncertain_on_a_plain_and_a_renumbered_volume() {
    use crazyagent::worktrees::LeaseState;
    let missing = |worktree: &Path| std::fs::remove_file(worktree.join("tracked")).unwrap();
    let short = |worktree: &Path| std::fs::write(worktree.join("tracked"), "comm").unwrap();
    let unindexed =
        |worktree: &Path| std::fs::remove_file(admin_folder(worktree).join("index")).unwrap();
    let locked =
        |worktree: &Path| std::fs::write(admin_folder(worktree).join("index.lock"), "").unwrap();
    let damages: [(&str, &Damage); 4] = [
        ("missing", &missing),
        ("short", &short),
        ("no index", &unindexed),
        ("index.lock", &locked),
    ];
    for (damage, change) in damages {
        for (place, renumber) in [
            ("plain", (|_| Renumber::Nothing) as fn(u64) -> Renumber),
            ("renumbered", Renumber::Volume as fn(u64) -> Renumber),
        ] {
            assert_eq!(
                uncertain_after(change, renumber, |_| {}).await,
                LeaseState::Uncertain,
                "{damage} on a {place} volume"
            );
        }
    }
}

/// Rewrites a lease worktree's Git index with `read-tree` alone, as a checkout cut off
/// after `read-tree` leaves it, and checks that its entries hold no file stat data.
/// Input: the lease worktree.
fn index_from_read_tree(worktree: &Path) {
    git(worktree, &["read-tree", "HEAD"]);
    index_has_no_stat_data(worktree);
}

/// Checks that a lease worktree's Git index entry for `tracked` holds no file stat data,
/// so nothing has refreshed and rewritten that index since `read-tree`. Input: the lease
/// worktree.
fn index_has_no_stat_data(worktree: &Path) {
    let listed = git(worktree, &["ls-files", "--debug", "--", "tracked"]);
    assert!(listed.contains("size: 0\t"), "{listed}");
}

/// LEASE-LEGACY-UNCERTAIN, rv-sec3 #160 N4 and rv-code4 #161 F1: with an index from
/// `read-tree` alone, which holds no file stat data, the start check compares the files'
/// content without writing that index: a whole worktree waits for recovery, while a file
/// of the same size with other bytes, or a missing file, keeps the lease Uncertain.
#[tokio::test]
async fn an_uncertain_lease_whose_index_has_no_stat_data_is_checked_on_its_content() {
    use crazyagent::worktrees::LeaseState;
    let altered = |worktree: &Path| {
        index_from_read_tree(worktree);
        std::fs::write(worktree.join("tracked"), "COMMITTED\n").unwrap();
    };
    let missing = |worktree: &Path| {
        index_from_read_tree(worktree);
        std::fs::remove_file(worktree.join("tracked")).unwrap();
    };
    let cases: [(&str, &Damage, LeaseState); 3] = [
        ("whole", &index_from_read_tree, LeaseState::RecoveryRequired),
        ("same size, other bytes", &altered, LeaseState::Uncertain),
        ("missing", &missing, LeaseState::Uncertain),
    ];
    for (case, change, expected) in cases {
        assert_eq!(
            uncertain_after(change, |_| Renumber::Nothing, index_has_no_stat_data).await,
            expected,
            "{case}"
        );
    }
}
