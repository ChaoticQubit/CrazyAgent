#![cfg(target_os = "macos")]

mod e2e_support;
mod golden_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::landing::{AcceptOutcome, AgentKind, ChangeState, FileStatus};
use crazyagent::protocol::CredentialRef;
use crazyagent::team::TeamAction;
use e2e_support::scratch;
use golden_support::{Golden, git_text, landed, owner_repo, silent_provider, wait_until};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const FLOW: &str = "accept-changes";
const STAND_IN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden/codex");
const POOL: &str = "local-pool";
const IDENTITY: [&str; 2] = ["Golden Owner", "golden@example.invalid"];
const TASK: &str = "Write golden.md with one line about apples.";
const FILE: &str = "golden.md";
const LINE: &str = "Apples are red.\n";
const SET_TIME: Duration = Duration::from_secs(120);
const STEPS: [&str; 8] = [
    "start the engine",
    "make the repository a project",
    "hand the task to Codex",
    "wait for the change set",
    "read the diff",
    "accept the change set",
    "find the change on main",
    "stop the engine",
];

/// The engine's settings: one keyless OpenAI-compatible account on a port that takes
/// connections and answers none, in the one pool the project's agents run on.
/// Input: that port. Output: the global.toml text.
fn global(port: u16) -> String {
    format!(
        r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]

[[model_access.connections]]
id = "local-connection"
provider = "openai-compatible"
harness = "rcli"
protocol = "openai-chat"
account_ref = "local-account"
endpoint = {{ kind = "http", url = "http://127.0.0.1:{port}/v1" }}
model = {{ id = "golden-model", author = "openai-compatible" }}
auth = {{ method = "none" }}

[[model_access.pools]]
id = "{POOL}"
members = ["local-connection"]
projects = []
roles = []
fallback_allowed = false
"#
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_accepts_an_agents_change_set_as_one_commit_of_their_own_on_main() {
    let stand_in = Path::new(STAND_IN);
    let mode = fs::metadata(stand_in).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "the Codex stand-in is kept as 0755");
    let repos = scratch("ca-golden-repos-");
    let repo = repos.path().join("fruit-stand");
    let start = owner_repo(&repo, Some(IDENTITY));
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

    let controller = &golden.owner.controller;
    let id = &project;
    let pending = async || {
        let sets = controller.change_sets(id.clone()).await.unwrap();
        sets.into_iter()
            .find(|set| set.state == ChangeState::Pending)
    };
    let set = wait_until("the agent's change set", SET_TIME, pending).await;
    assert_eq!(
        set.agent.kind,
        AgentKind::Sub,
        "a sub-agent's turn opened it"
    );
    golden.step(STEPS[3]);

    let controller = &golden.owner.controller;
    let diff = controller
        .change_diff(set.id.clone(), set.head.clone(), 0)
        .await;
    let diff = diff.unwrap();
    let added = diff.files.iter().find(|file| file.path == FILE);
    assert_eq!(
        added.map(|file| file.status),
        Some(FileStatus::Added),
        "the diff adds the agent's file: {:?}",
        diff.files
    );
    assert!(diff.text.contains("+Apples are red."), "{}", diff.text);
    golden.step(STEPS[4]);

    let controller = &golden.owner.controller;
    let outcome = controller
        .accept_changes(set.id.clone(), set.head.clone(), None)
        .await;
    assert!(
        matches!(&outcome, Ok(AcceptOutcome::Landed { branch, .. }) if branch == "main"),
        "the set lands on main: {outcome:?}"
    );
    let Ok(AcceptOutcome::Landed { commit, .. }) = outcome else {
        unreachable!()
    };
    golden.step(STEPS[5]);

    assert_eq!(
        git_text(&repo, &["rev-parse", "HEAD"]),
        commit,
        "main holds the commit"
    );
    let parent = git_text(&repo, &["rev-parse", &format!("{commit}^")]);
    assert_eq!(
        parent, start,
        "one squash commit on top of the owner's branch"
    );
    let author = git_text(&repo, &["log", "-1", "--format=%an <%ae>"]);
    let owner = format!("{} <{}>", IDENTITY[0], IDENTITY[1]);
    assert_eq!(author, owner, "the owner's own Git identity authored it");
    let porcelain = git_text(&repo, &["status", "--porcelain"]);
    assert_eq!(porcelain, "", "the owner's checkout matches the commit");
    let written = fs::read_to_string(repo.join(FILE)).ok();
    assert_eq!(
        written.as_deref(),
        Some(LINE),
        "the agent's file is on main"
    );
    let sets = golden.owner.controller.change_sets(project).await.unwrap();
    let state = sets
        .iter()
        .find(|item| item.id == set.id)
        .map(|item| item.state);
    assert_eq!(state, Some(ChangeState::Accepted), "the set reads accepted");
    golden.step(STEPS[6]);

    golden.engine.stop();
    golden.step(STEPS[7]);

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
