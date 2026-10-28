#![cfg(unix)]

use crazyagent::config::resolve_config;
use crazyagent::engine::Engine;
use crazyagent::protocol::*;
use crazyagent::worktrees::{LeaseState, REMOTE_LOCK, REMOTE_REFUSAL, WorktreeLease};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Output, Stdio};

fn git_run(dir: &Path, args: &[&str]) -> Output {
    owner_git_run(dir, Path::new("/dev/null"), args)
}

fn owner_git_run(dir: &Path, global: &Path, args: &[&str]) -> Output {
    crazyagent::spawn_gate::output(
        Process::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", global)
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = git_run(dir, args);
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

struct Project {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    remote: PathBuf,
}

fn project() -> Project {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let remote = root.join("remote.git");
    git(
        &root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("tracked"), "committed\n").unwrap();
    git(&repo, &["add", "tracked"]);
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
            "base",
        ],
    );
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    Project {
        _temporary: temporary,
        root,
        repo,
        remote,
    }
}

fn request(id: &str, revision: u64, command: Command) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("ide-remotes-test").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        command,
    }
}

async fn applied(engine: &Engine, id: &str, revision: u64, command: Command) {
    let reply = engine.apply(request(id, revision, command)).await.unwrap();
    assert_eq!(reply.outcome, Outcome::Applied, "{id}");
}

async fn leases(engine: &Engine) -> Vec<WorktreeLease> {
    let reply = engine
        .apply(request(
            "leases",
            0,
            Command::WorktreeLeases {
                task_id: TaskId::new("task").unwrap(),
            },
        ))
        .await
        .unwrap();
    let Outcome::Worktrees { leases } = reply.outcome else {
        panic!("leases required")
    };
    leases
}

/// Has a real engine admit the project's repository and give one task its agent worktree,
/// through the same commands the owner's app sends. Input: the project. Output: the open
/// engine and the Ready worktree's folder.
async fn agent_worktree(project: &Project) -> (Engine, PathBuf) {
    let config =
        resolve_config(&project.root.join("global"), &project.root.join("project")).unwrap();
    let engine = Engine::open(&project.root.join("db"), config.clone())
        .await
        .unwrap();
    let project_id = ProjectId::new("project").unwrap();
    let repository = project.repo.to_str().unwrap().to_owned();
    applied(
        &engine,
        "project",
        0,
        Command::CreateProject {
            project_id: project_id.clone(),
            spec: ProjectSpec {
                name: "Product".into(),
                repositories: vec![repository.clone()],
            },
        },
    )
    .await;
    applied(
        &engine,
        "pin",
        1,
        Command::PinTask {
            context: ExecutionContext {
                project_id: project_id.clone(),
                task_id: TaskId::new("task").unwrap(),
                config_id: config.id.clone(),
                snapshot_id: SnapshotId::new("snapshot").unwrap(),
                baseline_id: BaselineId::new("baseline").unwrap(),
                allowed_destinations: vec![],
                budget_ref: CredentialRef::new("budget").unwrap(),
            },
        },
    )
    .await;
    applied(
        &engine,
        "admit",
        2,
        Command::AdmitRepository {
            project_id,
            repository: repository.clone(),
        },
    )
    .await;
    applied(
        &engine,
        "allocate",
        3,
        Command::AllocateWorktree {
            task_id: TaskId::new("task").unwrap(),
            repository,
            owner_session: SessionId::new("owner").unwrap(),
            native_descendant: None,
        },
    )
    .await;
    let lease = leases(&engine).await.remove(0);
    assert_eq!(lease.state, LeaseState::Ready);
    (engine, PathBuf::from(lease.path))
}

#[tokio::test]
async fn push_from_an_agent_worktree_is_refused_with_the_plain_message() {
    let project = project();
    let (engine, worktree) = agent_worktree(&project).await;
    let pointer = std::fs::read_to_string(worktree.join(".git")).unwrap();
    let metadata = PathBuf::from(pointer.strip_prefix("gitdir: ").unwrap().trim_end());
    assert_eq!(
        std::fs::read(metadata.join("config.worktree")).unwrap(),
        REMOTE_LOCK
    );
    let remote = project.remote.to_str().unwrap();
    let attempts: [&[&str]; 6] = [
        &["push", "origin", "HEAD:refs/heads/agent-work"],
        &["push", "-u", "origin", "HEAD"],
        &["push", remote, "HEAD:refs/heads/agent-work"],
        &["fetch", "origin"],
        &["pull", "origin", "main"],
        &["ls-remote", "https://example.invalid/product.git"],
    ];
    for args in attempts {
        let output = git_run(&worktree, args);
        let printed = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{args:?} worked in the agent worktree"
        );
        assert!(printed.contains(REMOTE_REFUSAL), "{args:?}: {printed}");
    }
    assert_eq!(
        git(&project.remote, &["for-each-ref", "--format=%(refname)"]),
        ""
    );
    assert_eq!(leases(&engine).await.remove(0).state, LeaseState::Ready);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn push_from_the_project_folder_is_unaffected() {
    let project = project();
    let (engine, worktree) = agent_worktree(&project).await;
    assert_eq!(
        git(
            &project.repo,
            &["config", "--local", "--get", "extensions.worktreeConfig"]
        )
        .trim(),
        "true"
    );
    assert_eq!(
        git(&project.repo, &["remote", "get-url", "--push", "origin"]).trim(),
        project.remote.to_str().unwrap()
    );
    git(&project.repo, &["push", "-q", "origin", "main"]);
    git(&project.repo, &["fetch", "-q", "origin"]);
    assert_eq!(
        git(&project.remote, &["rev-parse", "refs/heads/main"]),
        git(&project.repo, &["rev-parse", "main"])
    );
    assert!(!git_run(&worktree, &["fetch", "origin"]).status.success());
    assert_eq!(leases(&engine).await.remove(0).state, LeaseState::Ready);
    engine.close().await.unwrap();
}

/// Reads the Git metadata folder a linked worktree's `.git` file points to.
fn metadata_of(worktree: &Path) -> PathBuf {
    let pointer = std::fs::read_to_string(worktree.join(".git")).unwrap();
    PathBuf::from(pointer.strip_prefix("gitdir: ").unwrap().trim_end())
}

#[tokio::test]
async fn agent_worktrees_made_before_the_lock_are_locked_once_at_start_and_the_owners_are_not() {
    let project = project();
    let (engine, lease) = agent_worktree(&project).await;
    engine.close().await.unwrap();
    std::fs::remove_file(metadata_of(&lease).join("config.worktree")).unwrap();
    let agents = crazyagent::projects_home::state_folder(&project.repo).join("worktrees");
    std::fs::create_dir_all(&agents).unwrap();
    let team = agents.join("agent-7");
    let owner = project.root.join("owner-feature");
    for worktree in [&team, &owner] {
        git(
            &project.repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
    }
    std::fs::write(
        metadata_of(&team).join("config.worktree"),
        &REMOTE_LOCK[..REMOTE_LOCK.len() / 2],
    )
    .unwrap();
    assert!(git_run(&lease, &["fetch", "origin"]).status.success());
    let connection = rusqlite::Connection::open(project.root.join("db")).unwrap();
    assert_eq!(
        crazyagent::worktrees::lock_existing(&connection).unwrap(),
        2
    );
    for worktree in [&lease, &team] {
        assert_eq!(
            std::fs::read(metadata_of(worktree).join("config.worktree")).unwrap(),
            REMOTE_LOCK
        );
        let output = git_run(worktree, &["push", "origin", "HEAD:refs/heads/agent-work"]);
        assert!(!output.status.success(), "{worktree:?}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(REMOTE_REFUSAL),
            "{output:?}"
        );
    }
    assert!(!metadata_of(&owner).join("config.worktree").exists());
    git(
        &owner,
        &["push", "-q", "origin", "HEAD:refs/heads/owner-feature"],
    );
    assert_eq!(
        git(&project.remote, &["for-each-ref", "--format=%(refname)"]).trim(),
        "refs/heads/owner-feature"
    );
    assert_eq!(
        crazyagent::worktrees::lock_existing(&connection).unwrap(),
        2
    );
}

#[tokio::test]
async fn an_owner_rewrite_with_a_transport_allow_cannot_carry_an_agent_worktree_remote() {
    let project = project();
    let (engine, worktree) = agent_worktree(&project).await;
    let global = project.root.join("owner.gitconfig");
    std::fs::write(
        &global,
        format!(
            "[url \"{}/\"]\n\tinsteadOf = https://example.invalid/\n[protocol \"file\"]\n\tallow = always\n",
            project.root.display()
        ),
    )
    .unwrap();
    let rewritten = "https://example.invalid/remote.git";
    let control = owner_git_run(&project.repo, &global, &["ls-remote", rewritten]);
    assert!(control.status.success(), "{control:?}");
    let attempts: [&[&str]; 3] = [
        &["push", rewritten, "HEAD:refs/heads/agent-work"],
        &["fetch", rewritten],
        &["ls-remote", rewritten],
    ];
    for args in attempts {
        let output = owner_git_run(&worktree, &global, args);
        let printed = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{args:?} worked in the agent worktree"
        );
        assert!(
            printed.contains("transport 'file' not allowed"),
            "{args:?}: {printed}"
        );
    }
    assert_eq!(
        git(&project.remote, &["for-each-ref", "--format=%(refname)"]),
        ""
    );
    assert_eq!(leases(&engine).await.remove(0).state, LeaseState::Ready);
    engine.close().await.unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn agent_sandbox_cannot_reach_owner_git_credentials() {
    use crazyagent::sandbox::{
        ProjectSandbox, SandboxError, SandboxNetwork, agent_variable_allowed,
    };
    use std::ffi::OsStr;

    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let owner_agent = root.join("owner-ssh-agent");
    let profiles = root.join("profiles");
    for folder in [&project, &home, &tmp, &owner_agent, &profiles] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let socket = owner_agent.join("agent.sock");
    let _listener = crazyagent::spawn_gate::bind_unix_std(&socket).unwrap();
    crazyagent::spawn_gate::connect_unix_std(&socket).unwrap();
    assert!(!agent_variable_allowed(
        OsStr::new("SSH_AUTH_SOCK"),
        socket.as_os_str()
    ));
    let owner = PathBuf::from(std::env::var_os("HOME").unwrap());
    let owner = owner.canonicalize().unwrap_or(owner);
    for (network, name) in [
        (SandboxNetwork::Open, "open.sb"),
        (SandboxNetwork::Only(Vec::new()), "closed.sb"),
    ] {
        let sandbox = ProjectSandbox::new(&project, &home, &tmp, network).unwrap();
        let profile = profiles.join(name);
        sandbox.install(&profile).unwrap();
        let secrets = sandbox.secret_paths();
        for credential in [
            ".ssh",
            ".git-credentials",
            ".config/git/credentials",
            ".config/gh",
        ] {
            let path = owner.join(credential);
            assert!(secrets.contains(&path), "{name}: {}", path.display());
            assert!(
                sandbox
                    .profile()
                    .contains(&format!("(subpath \"{}\")", path.display())),
                "{name}: {}",
                path.display()
            );
        }
        let mut connect = sandbox.command(&profile, Path::new("/usr/bin/nc"));
        connect
            .args(["-U", "-w", "2"])
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        assert!(
            !crazyagent::spawn_gate::output(&mut connect)
                .unwrap()
                .status
                .success(),
            "{name}: the agent connected to the owner's SSH agent socket"
        );
        let mut env = sandbox.command(&profile, Path::new("/usr/bin/env"));
        env.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let printed = crazyagent::spawn_gate::output(&mut env).unwrap();
        assert!(printed.status.success(), "{name}: {printed:?}");
        assert!(
            !String::from_utf8_lossy(&printed.stdout)
                .lines()
                .any(|line| line.starts_with("SSH_AUTH_SOCK=")),
            "{name}: the agent inherited SSH_AUTH_SOCK"
        );
        let mut harness = Process::new("/usr/bin/env");
        harness.env("SSH_AUTH_SOCK", &socket).current_dir(&project);
        assert!(
            matches!(
                sandbox.wrap(&profile, &harness),
                Err(SandboxError::Invalid(_))
            ),
            "{name}: a harness was handed SSH_AUTH_SOCK"
        );
    }
}
