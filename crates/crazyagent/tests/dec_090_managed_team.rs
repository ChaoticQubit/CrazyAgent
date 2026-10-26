#![cfg(unix)]

use crazyagent::config::resolve_config;
use crazyagent::project_setup::load_setup;
use crazyagent::protocol::{Outcome, ProjectId};
use crazyagent::team::{TaskStatus, TeamAction, execute, launch_harness, status};
use rusqlite::{Connection, TransactionBehavior};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RUN_LIMIT: Duration = Duration::from_secs(1200);

/// The owner's running engine, its two settings files and the project named by
/// `variable` whose team runs a tools-on `harness` sub-agent on a signed-in managed
/// account. Input: the variable and the launch harness. Output: those paths and the
/// project id, or a panic with the plain BLOCKED message and the steps that unblock it.
fn live(variable: &str, harness: &str) -> (PathBuf, PathBuf, PathBuf, String) {
    let blocked = || -> ! {
        panic!(
            "BLOCKED: no signed-in managed {harness} account with a tools-on team. Start CrazyAgent's engine with --writable-settings, --codex-executable and --claude-executable, sign in to a managed {harness} account, assign its pool to a project whose team has a tools-on {harness} sub-agent, then run: CRAZYAGENT_MANAGED_RUNTIME=<engine runtime folder> CRAZYAGENT_MANAGED_GLOBAL_TOML=<its global settings file> CRAZYAGENT_MANAGED_PROJECT_TOML=<its project settings file> {variable}=<that project id> cargo test --locked --offline -p crazyagent --test dec_090_managed_team"
        )
    };
    let path = |name: &str| {
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| blocked()))
            .canonicalize()
            .unwrap_or_else(|_| panic!("BLOCKED: {name} does not name an existing path"))
    };
    let runtime = path("CRAZYAGENT_MANAGED_RUNTIME");
    assert!(
        runtime.join("engine.db").is_file(),
        "BLOCKED: CRAZYAGENT_MANAGED_RUNTIME has no engine.db"
    );
    let global = path("CRAZYAGENT_MANAGED_GLOBAL_TOML");
    let settings = path("CRAZYAGENT_MANAGED_PROJECT_TOML");
    let project = std::env::var(variable).unwrap_or_else(|_| blocked());
    (runtime, global, settings, project)
}

/// Hands one shell task to a tools-on managed sub-agent of the owner's running engine
/// and requires the agent to run it on its signed-in account and report its output.
/// Input: the project variable and the launch harness.
fn managed_agent_completes_a_tool_turn(variable: &str, harness: &str) {
    let (runtime, global, settings, project) = live(variable, harness);
    let access = resolve_config(&global, &settings)
        .unwrap()
        .settings
        .model_access
        .unwrap_or_else(|| panic!("BLOCKED: the engine settings configure no model access"));
    let mut database = Connection::open(runtime.join("engine.db")).unwrap();
    database.busy_timeout(Duration::from_secs(5)).unwrap();
    let project_id = ProjectId::new(&project).unwrap();
    let nonce = format!(
        "DEC090-OK-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let prompt = format!(
        "Use your shell to run this command in your working folder: printf {nonce} > dec-090-check.txt && cat dec-090-check.txt. Then reply with exactly what it printed."
    );
    let tx = database
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let stored = load_setup(&tx, &project_id)
        .unwrap()
        .unwrap_or_else(|| panic!("BLOCKED: {variable} names no project in the engine"));
    let slot = stored
        .team
        .sub_harnesses
        .iter()
        .find(|slot| slot.tools && launch_harness(&slot.harness) == Some(harness))
        .unwrap_or_else(|| {
            panic!("BLOCKED: the project's team has no tools-on {harness} sub-agent")
        })
        .harness
        .clone();
    let outcome = execute(
        &tx,
        &access,
        &TeamAction::Delegate {
            project_id: project_id.clone(),
            harness: Some(slot),
            prompt: prompt.clone(),
        },
    )
    .unwrap();
    assert_eq!(outcome, Outcome::Applied);
    tx.commit().unwrap();
    let deadline = Instant::now() + RUN_LIMIT;
    let task = loop {
        let task = status(&database, &stored)
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.prompt == prompt)
            .expect("the delegated task is listed");
        if matches!(
            task.status,
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled
        ) {
            break task;
        }
        assert!(
            Instant::now() < deadline,
            "task {} is still {}: {:?}",
            task.id,
            task.status_label,
            task.problem
        );
        std::thread::sleep(Duration::from_secs(1));
    };
    assert_eq!(task.status, TaskStatus::Done, "{:?}", task.problem);
    assert!(
        task.result
            .as_deref()
            .is_some_and(|text| text.contains(&nonce)),
        "{:?}",
        task.result
    );
}

#[test]
fn a_tools_on_codex_agent_completes_a_turn_on_its_signed_in_managed_account() {
    managed_agent_completes_a_tool_turn("CRAZYAGENT_DEC090_CODEX_PROJECT", "codex");
}

#[test]
fn a_tools_on_claude_agent_completes_a_turn_on_its_signed_in_managed_account() {
    managed_agent_completes_a_tool_turn("CRAZYAGENT_DEC090_CLAUDE_PROJECT", "claude-code");
}
