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
const SWEPT_FILES: usize = 40_000;
const LOCK_HELD: Duration = Duration::from_millis(1000);
const SKILL: &str = "---\nname: release-notes\ndescription: Writes release notes from merged changes.\n---\n\n# Release notes\n";

/// A real engine whose owner deadline is `OWNER_TIMEOUT_MS`, with an owner client that
/// waits as long as each request's own budget.
struct Owner {
    root: tempfile::TempDir,
    service: EngineService,
    _shutdown: tokio::sync::watch::Sender<bool>,
    client: OwnerClient,
    engine_root: PathBuf,
}

impl Owner {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-deadline-")
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
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let client =
            OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
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
        Self {
            root,
            service,
            _shutdown: shutdown,
            client,
            engine_root,
        }
    }

    /// Queues a real storage sweep of `SWEPT_FILES` files ahead of every later storage
    /// job, in a removed skill folder of the private plugin store made as the engine makes
    /// it. Output: the swept folder and the sweep's answer.
    fn occupy_storage(&self) -> (PathBuf, oneshot::Receiver<Result<(), EngineError>>) {
        let swept = crate::storage::plugins::store_folder(&self.engine_root, &["skills", "global"])
            .unwrap()
            .join(".removed-busy-storage");
        std::fs::create_dir(&swept).unwrap();
        for index in 0..SWEPT_FILES {
            std::fs::write(swept.join(index.to_string()), b"x").unwrap();
        }
        let (send, sweep) = oneshot::channel();
        self.service
            .owner_runtime()
            .unwrap()
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

    async fn stop(mut self) {
        drop(self.client);
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
        drop(self.root);
    }
}

/// Fails the test unless the request took longer than the owner deadline, so a pass
/// shows that it was answered past it. Input: when the request started.
fn outlasted_the_owner_deadline(started: std::time::Instant) {
    assert!(
        started.elapsed() > Duration::from_millis(OWNER_TIMEOUT_MS),
        "the request must outlast the owner deadline for this test to show anything"
    );
}

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
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, project.as_str(), repo);
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
    let _disk = super::usage_tests::busy_storage().await;
    let owner = Owner::start().await;
    let work = owner.root.path().join("work");
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = repository(&work, &home);
    let project = ProjectId::new("project-1").unwrap();
    let worktree = finished_agent(&owner.engine_root, &repo, &home, &project);
    let folder = owner
        .engine_root
        .join("team/agents")
        .join(format!("agent-{AGENT}"));

    let started = std::time::Instant::now();
    let removed = owner
        .client
        .remove_finished_agents(project.clone(), vec![AGENT])
        .await
        .expect("a removal past the owner deadline must still answer");
    outlasted_the_owner_deadline(started);
    assert_eq!(
        removed,
        Removal {
            agents: vec![AGENT],
            kept: None
        }
    );
    assert!(!worktree.exists());
    assert!(
        folder.exists(),
        "the agent's folder with its conversations stays as history"
    );
    let db = crate::landing::open(&owner.engine_root).unwrap();
    let recorded: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM team_worktrees WHERE agent=?1",
            [AGENT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 0);
    owner.stop().await;
}

/// A usage request that waits on the engine database past the owner exchange deadline,
/// here behind a real write lock another connection holds, still answers once it is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_usage_request_slower_than_the_owner_deadline_still_answers() {
    let owner = Owner::start().await;
    let lock = rusqlite::Connection::open(owner.engine_root.join("engine.db")).unwrap();
    crate::usage::initialize(&lock).unwrap();
    let asked_ms = crate::team::now_ms();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let held = std::thread::spawn(move || {
        std::thread::sleep(LOCK_HELD);
        lock.execute_batch("COMMIT").unwrap();
    });

    let started = std::time::Instant::now();
    owner
        .client
        .usage_refresh()
        .await
        .expect("a usage request past the owner deadline must still answer");
    outlasted_the_owner_deadline(started);
    held.join().unwrap();
    let db = crate::usage::open(&owner.engine_root).unwrap();
    let requested: i64 = db
        .query_row(
            "SELECT requested_ms FROM usage_refresh WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(requested >= asked_ms);
    owner.stop().await;
}

/// A skill import queued behind real storage work past the owner exchange deadline still
/// imports and answers with the skill, rather than the owner hearing the outcome is unknown
/// while storage adds it later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_skill_import_slower_than_the_owner_deadline_answers_with_the_skill() {
    let _disk = super::usage_tests::busy_storage().await;
    let owner = Owner::start().await;
    let source = owner.root.path().join("work/release-notes");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("SKILL.md"), SKILL).unwrap();
    let (swept, sweep) = owner.occupy_storage();

    let started = std::time::Instant::now();
    let imported = owner
        .client
        .import_skill(source.to_str().unwrap().to_owned(), None)
        .await
        .expect("an import past the owner deadline must still answer");
    outlasted_the_owner_deadline(started);
    assert_eq!(imported.skill.name, "release-notes");
    tokio::time::timeout(Duration::from_secs(60), sweep)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!swept.exists());
    owner.stop().await;
}

/// Only skill import scans and imports leave the owner exchange to be served under
/// `IMPORT_TIMEOUT`; every other plugins request, and one that does not parse, stays inline.
#[test]
fn only_skill_import_scans_and_imports_are_served_past_the_owner_deadline() {
    let body = |action: &str| format!(r#"{{"plugins":{action},"secret_present":false}}"#);
    assert!(plugins::imports(
        body(r#"{"kind":"import_scan"}"#).as_bytes()
    ));
    assert!(plugins::imports(
        body(r#"{"kind":"import_skill","folder":"/skills/release-notes","replace":null}"#)
            .as_bytes()
    ));
    assert!(!plugins::imports(
        body(r#"{"kind":"skills","project":null,"offset":0}"#).as_bytes()
    ));
    assert!(!plugins::imports(b"{}"));
}
