#![cfg(target_os = "macos")]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::onboarding::project_id_for;
use crazyagent::protocol::CredentialRef;
use crazyagent::sandbox::policy::{AgentAccess, AgentNetwork, AgentPolicy};
use crazyagent::team::TeamAction;
use crazyagent::team::runtime::{TeamLimits, TeamRuntime};
use rusqlite::OptionalExtension;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const POOL: &str = "named-pool";
const CATALOG: &str = "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"named-account\"]\n\n[[model_access.connections]]\nid = \"named-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"named-account\"\nendpoint = { kind = \"http\", url = \"https://models.crazyagent.test/v1\" }\nmodel = { id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"named-pool\"\nmembers = [\"named-connection\"]\nprojects = [\"net-none\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";

/// The harness stand-in: inside the agent's sandbox it fetches a page through the proxy
/// its variables name and again with every proxy skipped, writes both curl verdicts into
/// its home and exits, so no model is asked anything.
const STAND_IN: &str = "#!/bin/sh\nthrough=$(/usr/bin/curl -sS -o /dev/null --max-time 10 https://example.com 2>&1); through_exit=$?\ndirect=$(/usr/bin/curl -sS -o /dev/null --max-time 10 --noproxy '*' https://example.com 2>&1); direct_exit=$?\nprintf 'proxy=%s\\nthrough=%s\\nthrough_exit=%s\\ndirect=%s\\ndirect_exit=%s\\n' \"$HTTPS_PROXY\" \"$through\" \"$through_exit\" \"$direct\" \"$direct_exit\" > \"$HOME/network.part\"\nmv \"$HOME/network.part\" \"$HOME/network.txt\"\nexit 1\n";

/// Runs git in `repo` with no global or system configuration.
/// Takes the folder and the arguments. A failure fails the test.
fn git(repo: &Path, args: &[&str]) {
    let output = crazyagent::spawn_gate::output(
        Command::new("/usr/bin/git")
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=Network None Test",
                "-c",
                "user.email=network-none@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
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

/// An onboarded project the owner sets to Sandboxed with no network starts its master with
/// tools on its https endpoint account through the running engine. In the agent's sandbox
/// a fetch of a host that is not its model's gets the model proxy's 403, and the same fetch
/// with the proxy skipped cannot even resolve the host, since the profile reaches only the
/// proxy's port (S4b).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn under_no_network_an_agent_reaches_no_other_host_through_or_around_its_model_proxy() {
    let runtime = tempfile::Builder::new()
        .prefix("ca-none-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let root = runtime.path().canonicalize().unwrap();
    let global = root.join("global.toml");
    fs::write(&global, CATALOG).unwrap();
    fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    let settings = root.join("project.toml");
    let repos = tempfile::Builder::new()
        .prefix("ca-none-repos-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let repo = repos.path().canonicalize().unwrap().join("net-none");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# net-none\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let harness = repos.path().join("codex-stand-in");
    fs::write(&harness, STAND_IN).unwrap();
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
    let saved = owner.project_agent_policy(project.clone()).await.unwrap();
    let policy = AgentPolicy {
        access: AgentAccess::Sandboxed,
        toolchains: Vec::new(),
        network: AgentNetwork::None,
        ide_git_hooks: false,
        tool_cache: Default::default(),
        ide_agent_files: Default::default(),
        ..Default::default()
    };
    let set = owner
        .set_project_agent_policy(project.clone(), saved.version, policy, 2)
        .await
        .unwrap();
    assert_eq!(rejection(&set), None, "{set:?}");
    let message = owner
        .team_action(
            TeamAction::Message {
                project_id: project.clone(),
                text: "Read the README and say what the project is.".into(),
            },
            3,
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
    let deadline = Instant::now() + Duration::from_secs(90);
    let said = loop {
        team = tokio::task::spawn_blocking(move || {
            team.tick().unwrap();
            team
        })
        .await
        .unwrap();
        let row: Option<(i64, String, Option<String>)> = reader
            .query_row(
                "SELECT id,status,error FROM team_agents WHERE project=?1 ORDER BY id LIMIT 1",
                [project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .unwrap();
        if let Some((id, status, problem)) = row {
            let file = root
                .join("team/agents")
                .join(format!("agent-{id}"))
                .join("home/network.txt");
            if let Ok(text) = fs::read_to_string(&file) {
                break text;
            }
            assert!(
                status != "failed",
                "the agent failed before its harness wrote its verdicts: {problem:?}"
            );
        }
        assert!(Instant::now() < deadline, "the agent's harness never ran");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let line = |name: &str| {
        said.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("no {name} line: {said}"))
            .to_owned()
    };
    assert!(
        line("proxy").starts_with("http://127.0.0.1:"),
        "the harness was not pointed at its model proxy: {said}"
    );
    assert!(
        line("through").contains("403") && line("through_exit") == "56",
        "a fetch through the proxy reached a host that is not the model's: {said}"
    );
    assert!(
        line("direct").contains("resolve host") && line("direct_exit") != "0",
        "a fetch around the proxy left the sandbox: {said}"
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
