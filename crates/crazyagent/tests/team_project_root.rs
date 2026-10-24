#![cfg(target_os = "macos")]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::onboarding::project_id_for;
use crazyagent::protocol::CredentialRef;
use crazyagent::team::TeamAction;
use crazyagent::team::runtime::{TeamLimits, TeamRuntime};
use rusqlite::OptionalExtension;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const POOL: &str = "local-pool";
const PROJECT: &str = "demo-app";
const README: &str = "# demo-app\n\nThe owner's own code, which every agent must see.\n";
const CATALOG: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\"]\nprojects = [\"demo-app\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";

/// Runs git in `repo` with no global or system configuration.
/// Takes the folder and the arguments. Returns standard output; a failure fails the test.
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Project Root Test",
            "-c",
            "user.email=project-root@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The plain code of a rejected owner response, or `None` once applied.
fn rejection(response: &OwnerResponse) -> Option<String> {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            match &operation.result {
                OwnerResult::Applied => None,
                OwnerResult::Rejected { code } => Some(code.clone()),
                other => panic!("unexpected owner result: {other:?}"),
            }
        }
        OwnerResponse::Rejected { code } => Some(code.clone()),
        other => panic!("unexpected owner response: {other:?}"),
    }
}

/// Every `.git` entry below `folder`, without following links.
/// Takes the folder. Returns each path with whether it is a folder.
fn git_entries(folder: &Path) -> Vec<(PathBuf, bool)> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(next) = pending.pop() {
        assert!(found.len() + pending.len() < 10_000);
        for entry in fs::read_dir(&next).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if entry.file_name() == ".git" {
                found.push((entry.path(), kind.is_dir()));
            }
            if kind.is_dir() && !kind.is_symlink() {
                pending.push(entry.path());
            }
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_team_agent_of_an_onboarded_repo_works_on_the_owners_code() {
    let runtime = tempfile::Builder::new()
        .prefix("ca-root-")
        .tempdir_in("/private/tmp")
        .unwrap();
    fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let root = runtime.path().canonicalize().unwrap();
    let global = root.join("global.toml");
    fs::write(&global, CATALOG).unwrap();
    fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    let settings = root.join("project.toml");
    let repos = tempfile::Builder::new()
        .prefix("ca-root-repos-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let repo = repos.path().canonicalize().unwrap().join(PROJECT);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("README.md"), README).unwrap();
    fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let harness = repos.path().join("codex-stand-in");
    fs::write(&harness, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o700)).unwrap();

    let (_stop, stopped) = watch::channel(false);
    let mut service =
        EngineService::start_with_settings_sources(&root, "e.sock", &global, &settings, &stopped)
            .await
            .unwrap();
    let mut owner = OwnerController::attach(&root, OwnerClientKind::Tui, Duration::from_secs(5))
        .await
        .unwrap();
    let started = owner
        .start_onboarding(
            repo.to_str().unwrap().to_owned(),
            CredentialRef::new(POOL).unwrap(),
            1,
        )
        .await
        .unwrap();
    assert_eq!(rejection(&started), None, "{started:?}");
    let project = project_id_for(&repo).unwrap();
    let message = owner
        .team_action(
            TeamAction::Message {
                project_id: project.clone(),
                text: "Read the README and say what the project is.".into(),
            },
            2,
        )
        .await
        .unwrap();
    assert_eq!(rejection(&message), None, "{message:?}");

    let (database, state) = (root.join("engine.db"), root.join("team"));
    let access_files = (global.clone(), settings.clone());
    let mut team = tokio::task::spawn_blocking(move || {
        TeamRuntime::open(
            &database,
            &state,
            BTreeMap::from([("codex", harness)]),
            Box::new(move || {
                crazyagent::config::resolve_config(&access_files.0, &access_files.1)
                    .ok()?
                    .settings
                    .model_access
            }),
            TeamLimits::default(),
        )
        .unwrap()
    })
    .await
    .unwrap();
    let reader = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let (id, worktree) = loop {
        team = tokio::task::spawn_blocking(move || {
            team.tick().unwrap();
            team
        })
        .await
        .unwrap();
        let row: Option<(i64, Option<String>, String, Option<String>)> = reader
            .query_row(
                "SELECT id,worktree,status,error FROM team_agents WHERE project=?1 ORDER BY id LIMIT 1",
                [project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .unwrap();
        match row {
            Some((id, Some(worktree), ..)) => break (id, worktree),
            Some((_, None, status, problem)) if status == "failed" => {
                panic!("the agent did not get a worktree: {problem:?}")
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "no team agent started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    assert_eq!(worktree, format!(".crazyagent/worktrees/agent-{id}"));
    let copy = repo.join(&worktree);
    assert_eq!(
        fs::read_to_string(copy.join("README.md")).unwrap(),
        README,
        "the agent's worktree holds the owner's committed files"
    );
    assert!(copy.join("src/main.rs").is_file());
    assert_eq!(
        git(&copy, &["rev-parse", "HEAD"]),
        head,
        "the worktree starts at the owner's HEAD"
    );
    assert!(git(&repo, &["worktree", "list", "--porcelain"]).contains(copy.to_str().unwrap()));
    let state = repo.join(".crazyagent");
    let worktrees = state.join("worktrees");
    assert!(
        !state.join(".git").exists(),
        "no repository inside .crazyagent"
    );
    for (path, folder) in git_entries(&state) {
        assert!(
            !folder && path.parent().and_then(Path::parent) == Some(worktrees.as_path()),
            "only a worktree's .git file may sit under .crazyagent: {}",
            path.display()
        );
    }
    assert!(
        repo.join(".git").is_dir(),
        "the owner's repository is untouched"
    );

    tokio::task::spawn_blocking(move || team.shutdown().unwrap())
        .await
        .unwrap();
    drop(owner);
    tokio::time::timeout(Duration::from_secs(10), service.shutdown())
        .await
        .unwrap()
        .unwrap();
}
