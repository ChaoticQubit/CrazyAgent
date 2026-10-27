use super::*;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClient, OwnerClientKind};
use crate::protocol::ProjectId;
use crate::team::cleanup::Removal;
use rusqlite::params;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

const OWNER_TIMEOUT_MS: u64 = 250;
const AGENT: i64 = 7;
const FOLDER_FILES: usize = 40_000;

/// Runs git as the owner would in a test: an isolated home, no system config, in `dir`.
/// Inputs: the owner home, the folder and the arguments. Output: none; a failure panics.
fn git(home: &Path, dir: &Path, args: &[&str]) {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Makes the project repository with one commit, its state folder ignored as the
/// runtime keeps it. Inputs: the folder to make it in and the owner home. Output: the
/// repository.
fn repository(under: &Path, home: &Path) -> PathBuf {
    let repo = under.join("repo");
    std::fs::create_dir_all(repo.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
    git(home, &repo, &["init", "-q", "--template=", "-b", "main"]);
    git(home, &repo, &["config", "user.name", "Repo Owner"]);
    git(
        home,
        &repo,
        &["config", "user.email", "owner@example.invalid"],
    );
    std::fs::write(repo.join("README"), b"one\n").unwrap();
    std::fs::write(repo.join(".gitignore"), b"/.crazyagent/\ntarget/\n").unwrap();
    git(home, &repo, &["add", "README", ".gitignore"]);
    git(home, &repo, &["commit", "-q", "-m", "Start"]);
    repo
}

/// Adds one finished sub-agent as the runtime leaves it: its worktree under the project's
/// state folder with its start recorded, an agent folder of `FOLDER_FILES` files, its
/// sandbox profile and its rows. Inputs: the engine root, the repository, the owner home
/// and the project. Output: the worktree.
fn finished_agent(engine_root: &Path, repo: &Path, home: &Path, project: &ProjectId) -> PathBuf {
    let worktree = crate::projects_home::state_folder(repo)
        .join("worktrees")
        .join(format!("agent-{AGENT}"));
    let git_dir = repo.join(".git/worktrees").join(format!("agent-{AGENT}"));
    git(
        home,
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let db = crate::landing::open(engine_root).unwrap();
    crate::team::initialize(&db).unwrap();
    crate::landing::record_worktree_start(&db, repo, &worktree, &git_dir).unwrap();
    let team = engine_root.join("team");
    let cache = team
        .join("agents")
        .join(format!("agent-{AGENT}"))
        .join("home/cache");
    std::fs::create_dir_all(&cache).unwrap();
    for index in 0..FOLDER_FILES {
        std::fs::write(cache.join(index.to_string()), b"x").unwrap();
    }
    std::fs::create_dir_all(team.join("profiles")).unwrap();
    std::fs::write(
        team.join("profiles").join(format!("agent-{AGENT}.sb")),
        b"(version 1)\n",
    )
    .unwrap();
    let now = crate::team::now_ms();
    db.execute(
        "INSERT INTO team_agents(id,project,role,harness,tools,connection,status,created_ms,heartbeat_ms,stopped_ms)
         VALUES (?1,?2,'sub','claude-code',0,'claude-main','stopped',?3,?3,?4)",
        params![AGENT, project.as_str(), now - 5000, now - 1000],
    )
    .unwrap();
    db.execute(
        "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
        params![
            AGENT,
            project.as_str(),
            repo.to_str().unwrap(),
            worktree.to_str().unwrap(),
            git_dir.to_str().unwrap()
        ],
    )
    .unwrap();
    worktree
}

/// A removal that runs past the owner exchange deadline still answers the owner with what
/// it removed, rather than the connection closing while the folders go on being deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removal_slower_than_the_owner_deadline_answers_with_the_agents_it_removed() {
    let root = tempfile::Builder::new()
        .prefix("ca-cleanup-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    std::fs::write(
        &global,
        format!("version = 1\ntimeout_ms = {OWNER_TIMEOUT_MS}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (shutdown, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let client = OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
        .await
        .unwrap()
        .0;
    let engine_root = service
        .owner_runtime()
        .unwrap()
        .authority
        .root
        .path
        .parent()
        .unwrap()
        .to_path_buf();
    let work = root.path().join("work");
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = repository(&work, &home);
    let project = ProjectId::new("project-1").unwrap();
    let worktree = finished_agent(&engine_root, &repo, &home, &project);
    let folder = engine_root
        .join("team/agents")
        .join(format!("agent-{AGENT}"));

    let started = std::time::Instant::now();
    let removed = client
        .remove_finished_agents(project.clone(), vec![AGENT])
        .await
        .expect("a removal past the owner deadline must still answer");
    assert!(
        started.elapsed() > Duration::from_millis(OWNER_TIMEOUT_MS),
        "the removal must outlast the owner deadline for this test to show anything"
    );
    assert_eq!(
        removed,
        Removal {
            agents: vec![AGENT],
            kept: None
        }
    );
    assert!(!worktree.exists());
    assert!(!folder.exists());
    let db = crate::landing::open(&engine_root).unwrap();
    let recorded: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM team_worktrees WHERE agent=?1",
            [AGENT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 0);

    drop(client);
    tokio::time::timeout(Duration::from_secs(10), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    drop(shutdown);
}
