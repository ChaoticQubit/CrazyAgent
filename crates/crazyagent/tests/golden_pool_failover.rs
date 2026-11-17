#![cfg(target_os = "macos")]

mod e2e_support;
mod golden_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::protocol::CredentialRef;
use crazyagent::team::{AgentRole, TaskStatus, TeamAction};
use e2e_support::scratch;
use golden_support::{Golden, landed, owner_repo, silent_provider, wait_until};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const FLOW: &str = "pool-failover";
const STAND_IN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden/codex");
const POOL: &str = "local-pool";
const LIMITED: &str = "limited-connection";
const READY: &str = "ready-connection";
const TASK: &str = "Write golden.md with one line about apples.";
const ANSWER: &str = "Wrote golden.md.";
const TASK_TIME: Duration = Duration::from_secs(180);
const STEPS: [&str; 6] = [
    "start the engine",
    "make the repository a project",
    "hand the task to Codex",
    "wait for the task to end",
    "find it run on the next member",
    "stop the engine",
];

/// The engine's settings: one pool of two keyless OpenAI-compatible accounts on a port that
/// takes connections and answers none, the limited one listed first and marked by its URL
/// path, which the Codex stand-in reads. Input: that port. Output: the global.toml text.
fn global(port: u16) -> String {
    format!(
        r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["limited-account", "ready-account"]

[[model_access.connections]]
id = "{LIMITED}"
provider = "openai-compatible"
harness = "rcli"
protocol = "openai-chat"
account_ref = "limited-account"
endpoint = {{ kind = "http", url = "http://127.0.0.1:{port}/limited/v1" }}
model = {{ id = "golden-model", author = "openai-compatible" }}
auth = {{ method = "none" }}

[[model_access.connections]]
id = "{READY}"
provider = "openai-compatible"
harness = "rcli"
protocol = "openai-chat"
account_ref = "ready-account"
endpoint = {{ kind = "http", url = "http://127.0.0.1:{port}/v1" }}
model = {{ id = "golden-model", author = "openai-compatible" }}
auth = {{ method = "none" }}

[[model_access.pools]]
id = "{POOL}"
members = ["{LIMITED}", "{READY}"]
projects = []
roles = []
fallback_allowed = false
"#
    )
}

/// Whether a task has ended, by its status; a task its account's rate limit sent back to the
/// queue has not.
/// Input: the status. Output: true once it is no longer queued, waiting or running.
fn ended(status: TaskStatus) -> bool {
    !matches!(
        status,
        TaskStatus::Queued | TaskStatus::NeedsPool | TaskStatus::Running
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_whose_first_pool_member_hits_its_rate_limit_ends_done_on_the_next_member() {
    let stand_in = Path::new(STAND_IN);
    let mode = fs::metadata(stand_in).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "the Codex stand-in is kept as 0755");
    let repos = scratch("ca-golden-repos-");
    let repo = repos.path().join("fruit-stand");
    owner_repo(&repo, None);
    let mut golden = Golden::start_team(FLOW, &global(silent_provider()), stand_in).await;

    let pool = CredentialRef::new(POOL).unwrap();
    let project = golden.onboard(&repo, &pool).await;
    golden.step(STEPS[1]);

    let generation = golden.owner.next();
    let delegate = TeamAction::Delegate {
        project_id: project.clone(),
        harness: Some("codex".into()),
        prompt: TASK.into(),
    };
    let handed = golden
        .owner
        .controller
        .team_action(delegate, generation)
        .await;
    assert_eq!(landed(handed.unwrap()), OwnerResult::Applied);
    golden.step(STEPS[2]);

    let controller = &mut golden.owner.controller;
    let id = &project;
    let done = async || {
        let status = controller.team_status(id.clone()).await.unwrap();
        let mut tasks = status.tasks.into_iter();
        tasks.find(|task| task.prompt == TASK && ended(task.status))
    };
    let task = wait_until("the task to end", TASK_TIME, done).await;
    golden.step(STEPS[3]);

    assert_eq!(
        (task.status, task.attempts),
        (TaskStatus::Done, 2),
        "the rate limit sent the task back once and the second try finished it: {task:?}"
    );
    let result = task.result.as_deref().unwrap_or_default();
    assert!(
        result.contains(ANSWER),
        "the owner hears the answer: {result:?}"
    );
    let status = golden.owner.controller.team_status(project).await.unwrap();
    let agent = status
        .agents
        .iter()
        .find(|agent| Some(agent.id) == task.agent);
    let agent = agent.expect("the task names the agent that ran it");
    assert_eq!(
        (agent.role, agent.harness.as_str(), agent.account.as_deref()),
        (AgentRole::Sub, "codex", Some(READY)),
        "the second try ran on the next pool member"
    );
    golden.step(STEPS[4]);

    golden.engine.stop();
    golden.step(STEPS[5]);

    let record = golden.finish();
    let steps: Option<Vec<&str>> = record
        .as_ref()
        .and_then(|record| record["steps"].as_array())
        .map(|steps| {
            steps
                .iter()
                .filter_map(|step| step["step"].as_str())
                .collect()
        });
    let outcome = record
        .as_ref()
        .and_then(|record| record["outcome"].as_str());
    assert_eq!(
        (steps, outcome),
        (Some(STEPS.to_vec()), Some("passed")),
        "the run record names each step in order"
    );
}
