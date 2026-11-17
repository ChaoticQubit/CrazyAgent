use super::Scenario;
use crate::e2e_support::{self, Engine, Owner};
use crazyagent::agent_git::Layout;
use crazyagent::engine::owner::{OwnerResponse, OwnerResult};
use crazyagent::landing::{
    self, AcceptOutcome, AgentKind, ChangeAgent, ChangeSetId, TurnEnd, TurnOutcome, Workspace,
};
use crazyagent::launch_context::{landing_rules, team_tools_section};
use crazyagent::plugins::{Scope, ServerSpec};
use crazyagent::protocol::ProjectId;
use crazyagent::team::{LandingMode, TeamAction};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

/// The project id every P4 fixture creates through the engine.
const PROJECT: &str = "snapshot-p4";

/// The fixture owner's git identity, which a landed commit is authored with.
const OWNER_GIT: &str = "[user]\n\tname = Owner Example\n\temail = owner@example.invalid\n";

/// The text the fixture repository starts with; the agent edits line two.
const README: &str = "one\ntwo\nthree\n";

/// One plugin the owner adds through the engine before it stops: a skill folder, a hooks
/// plugin folder, or a stdio MCP server, approved or only added.
pub(super) enum Plugin {
    Skill(PathBuf),
    Hooks(PathBuf),
    Server {
        name: &'static str,
        command: &'static str,
        approve: bool,
    },
}

/// A project the real engine set up through owner requests, stopped afterwards so a driver
/// reads its engine.db alone: the stopped engine (its root holds engine.db), the fixture
/// home, the scratch disk the fixture folders live on and the project id.
pub(super) struct Seeded {
    pub(super) engine: Engine,
    pub(super) _home: tempfile::TempDir,
    pub(super) disk: tempfile::TempDir,
    pub(super) project: ProjectId,
}

/// Starts the engine on a fixture HOME, sets up the project, its Landing setting when one is
/// given and the plugins `fixtures` writes on the scratch disk through owner requests only,
/// then stops it. Inputs: a scratch prefix, the fixture writer and the Landing setting, none
/// to keep the default. Output: the seeded project, or the step that failed.
pub(super) fn seeded(
    prefix: &str,
    fixtures: fn(&Path) -> Result<Vec<Plugin>, String>,
    landing: Option<LandingMode>,
) -> Result<Seeded, String> {
    let home = e2e_support::scratch("ca-p4-home-");
    fs::write(home.path().join(".gitconfig"), OWNER_GIT).map_err(text)?;
    let disk = e2e_support::scratch("ca-p4-disk-");
    let projects = disk.path().join("Projects");
    fs::create_dir(&projects).map_err(text)?;
    let plugins = fixtures(disk.path())?;
    let project = ProjectId::new(PROJECT).map_err(debug)?;
    let mut engine = Engine::new(prefix);
    engine.home = Some(home.path().to_path_buf());
    engine.launch();
    let seeding = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(text)?
        .block_on(async {
            let mut owner = Owner::attach(&engine).await;
            let done = seed(&mut owner, &projects, &project, landing, &plugins).await;
            owner.controller.detach();
            done
        });
    let _stderr = engine.stop();
    seeding?;
    Ok(Seeded {
        engine,
        _home: home,
        disk,
        project,
    })
}

/// Sends the owner requests that set up the project, its Landing setting and its plugins.
/// Inputs: the attached owner, the projects home, the project, the Landing setting and the
/// plugins. Output: nothing, or the request the engine did not apply.
async fn seed(
    owner: &mut Owner,
    projects: &Path,
    project: &ProjectId,
    landing: Option<LandingMode>,
    plugins: &[Plugin],
) -> Result<(), String> {
    applied(
        "save_profile",
        owner.save_profile(e2e_support::profile()).await,
    )?;
    applied("set_home", owner.set_home(projects).await)?;
    applied(
        "create_project",
        owner.create_project(PROJECT, "Snapshot P4").await,
    )?;
    if let Some(mode) = landing {
        let generation = owner.next();
        let action = TeamAction::LandingMode {
            project_id: project.clone(),
            mode,
        };
        match owner
            .controller
            .team_action(action, generation)
            .await
            .map_err(debug)?
        {
            OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied => {}
            other => return Err(format!("landing_mode: {other:?}")),
        }
    }
    let scope = || Scope::Project {
        project_id: project.clone(),
    };
    for plugin in plugins {
        match plugin {
            Plugin::Skill(folder) => {
                let view = owner
                    .controller
                    .add_skill(scope(), path_text(folder)?)
                    .await
                    .map_err(debug)?;
                if !view.enabled {
                    owner
                        .controller
                        .set_skill_enabled(view.id, true, view.version)
                        .await
                        .map_err(debug)?;
                }
            }
            Plugin::Hooks(folder) => {
                let hooks = owner
                    .controller
                    .add_hooks(scope(), path_text(folder)?)
                    .await
                    .map_err(debug)?;
                for hook in hooks.into_iter().filter(|hook| !hook.enabled) {
                    owner
                        .controller
                        .set_hook_enabled(hook.id, true)
                        .await
                        .map_err(debug)?;
                }
            }
            Plugin::Server {
                name,
                command,
                approve,
            } => {
                let spec = ServerSpec::Stdio {
                    command: (*command).to_owned(),
                    args: Vec::new(),
                };
                let view = owner
                    .controller
                    .add_mcp_server(scope(), (*name).to_owned(), spec.clone(), Vec::new())
                    .await
                    .map_err(debug)?;
                if *approve {
                    owner
                        .controller
                        .approve_mcp_server(view.id, spec)
                        .await
                        .map_err(debug)?;
                }
            }
        }
    }
    Ok(())
}

/// Turns an owner result into nothing when applied. Inputs: the request's name and its result.
/// Output: nothing, or the name with the result the engine gave.
fn applied(step: &str, result: OwnerResult) -> Result<(), String> {
    if result == OwnerResult::Applied {
        Ok(())
    } else {
        Err(format!("{step}: {result:?}"))
    }
}

/// Prints an error through its Debug form. Input: the error. Output: the text.
pub(super) fn debug(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

/// Prints an error through its Display form. Input: the error. Output: the text.
pub(super) fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Reads a path as UTF-8 text for a request. Input: the path. Output: the text.
pub(super) fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "a fixture path is not UTF-8".to_owned())
}

/// Writes no plugin fixture. Input: the scratch disk, unused. Output: no plugins.
pub(super) fn no_plugins(_disk: &Path) -> Result<Vec<Plugin>, String> {
    Ok(Vec::new())
}

/// Lists the landing scenarios. Output: the scenarios in run order.
pub(super) fn scenarios() -> Vec<Scenario> {
    let scenario = |name: &'static str, capture: fn(&Path) -> Result<Value, String>| Scenario {
        kind: "landing",
        name,
        capture,
    };
    vec![
        scenario("rules-text", rules_text),
        scenario("settings-defaults", settings_defaults),
        scenario("accept-squash", accept_squash),
        scenario("accept-merge", accept_merge),
        scenario("accept-edited-message", accept_edited_message),
        scenario("accept-conflict", accept_conflict),
        scenario("accept-into-free-branch", accept_into_free_branch),
    ]
}

/// Captures the landing text agents get per git layout and the team tools section for each
/// catch-up-main and own-clone pair. Input: the scratch folder, unused.
/// Output: each text as its lines.
fn rules_text(_scratch: &Path) -> Result<Value, String> {
    let mut sections = Vec::new();
    for catch_up_main in [true, false] {
        for own_clone in [true, false] {
            sections.push(json!({
                "catch_up_main": catch_up_main,
                "own_clone": own_clone,
                "text": lines(&team_tools_section(&[], catch_up_main, own_clone)),
            }));
        }
    }
    Ok(json!({
        "landing_rules": {
            "linked": lines(landing_rules(Layout::Linked)),
            "clone": lines(landing_rules(Layout::Clone)),
        },
        "team_tools": sections,
    }))
}

/// Captures each landing setting a new project starts with, as the owner's team status
/// shows it. Input: the scratch folder, unused. Output: the setting names, `master_lands`
/// null when the status shows none.
fn settings_defaults(_scratch: &Path) -> Result<Value, String> {
    let mut seeded = seeded("ca-p4-settings-", no_plugins, None)?;
    let project = seeded.project.clone();
    let status = again(&mut seeded, async move |owner| {
        owner.controller.team_status(project).await.map_err(debug)
    })?;
    Ok(json!({
        "landing": status.landing_mode.name(),
        "catch_up_main": status.catch_up_main.name(),
        "master_lands": status.master_lands.map(|mode| mode.name()),
    }))
}

/// Starts the stopped engine again on the same root and HOME, as the app's next launch does,
/// attaches as the TUI, runs one owner step, then stops the engine. Inputs: the seeded
/// project and the step. Output: the step's answer.
fn again<T>(
    seeded: &mut Seeded,
    step: impl AsyncFnOnce(&mut Owner) -> Result<T, String>,
) -> Result<T, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(text)?;
    seeded.engine.launch();
    let answer = runtime.block_on(async {
        let mut owner = Owner::attach(&seeded.engine).await;
        let answer = step(&mut owner).await;
        owner.controller.detach();
        answer
    });
    let _stderr = seeded.engine.stop();
    answer
}

/// What the owner's Accept came to: the Landing setting the team status shows, the message
/// Changes proposed and the engine's decision.
struct Accepted {
    landing: &'static str,
    proposed: String,
    outcome: AcceptOutcome,
}

/// Accepts one change set the way the owner does in Changes: with the engine started again,
/// reads the team status and the Changes list, then sends Accept with the head the list
/// shows, into `target` when given. Inputs: the seeded project, the change set, the owner's
/// message and the target branch. Output: what the Accept came to, or the refusal.
fn owner_accept(
    seeded: &mut Seeded,
    set: ChangeSetId,
    message: Option<&str>,
    target: Option<&str>,
) -> Result<Accepted, String> {
    let project = seeded.project.clone();
    let message = message.map(str::to_owned);
    let target = target.map(str::to_owned);
    again(seeded, async move |owner| {
        let status = owner
            .controller
            .team_status(project.clone())
            .await
            .map_err(debug)?;
        let view = owner
            .controller
            .change_sets(project)
            .await
            .map_err(debug)?
            .into_iter()
            .find(|view| view.id == set)
            .ok_or("the owner's Changes list does not show the change set")?;
        let outcome = match target {
            None => {
                owner
                    .controller
                    .accept_changes(set, view.head.clone(), message)
                    .await
            }
            Some(target) => {
                owner
                    .controller
                    .accept_changes_into(set, view.head.clone(), message, Some(target))
                    .await
            }
        }
        .map_err(debug)?;
        Ok(Accepted {
            landing: status.landing_mode.name(),
            proposed: view.message,
            outcome,
        })
    })
}

/// Makes the fixture owner repository on the scratch disk with the folder agents work in.
/// Input: the seeded project. Output: the repository's checkout.
fn checkout(seeded: &Seeded) -> Result<PathBuf, String> {
    let checkout = seeded.disk.path().join("repo");
    e2e_support::repository(&checkout, "README", README);
    fs::write(checkout.join(".git/info/exclude"), ".crazyagent/\n").map_err(text)?;
    fs::create_dir_all(checkout.join(".crazyagent/worktrees")).map_err(text)?;
    Ok(checkout)
}

/// Runs one agent turn the way the engine records it, through the public landing API, since
/// no owner request runs an agent: the agent's clone on its claimed branch, an edit of README
/// line two plus a new file committed there, and the turn's end. Inputs: the store, the
/// project and the owner checkout. Output: the change set the turn left.
fn agent_change(
    db: &Connection,
    project: &ProjectId,
    checkout: &Path,
) -> Result<ChangeSetId, String> {
    let worktree = checkout.join(".crazyagent/worktrees/agent-1");
    let branch = landing::claim_task_branch(db, checkout, &worktree, "tide-fix").map_err(debug)?;
    landing::add_agent_clone_on(checkout, &worktree, None, &branch).map_err(debug)?;
    let git_dir = crazyagent::agent_git::git_dir(&worktree);
    landing::record_worktree_start(db, checkout, &worktree, &git_dir).map_err(debug)?;
    fs::write(worktree.join("README"), "one\ntwo tides\nthree\n").map_err(text)?;
    fs::write(worktree.join("TIDES"), "high 06:10\nlow 12:25\n").map_err(text)?;
    e2e_support::git(&worktree, &["add", "-A"]);
    e2e_support::git(&worktree, &["commit", "--quiet", "-m", "Agent: tide fix"]);
    let workspace = Workspace {
        project: project.clone(),
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "1".into(),
            label: "Agent 1".into(),
        },
        repository: checkout.to_path_buf(),
        worktree,
        git_dir,
    };
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Fix the tide table".into(),
    };
    landing::record_turn_end(db, &workspace, &turn)
        .map_err(debug)?
        .ok_or_else(|| "the turn left no change set".to_owned())
}

/// Records one agent change set on the stopped engine's store, closing the store before the
/// engine starts again. Inputs: the seeded project and the owner checkout. Output: the set.
fn recorded_change(seeded: &Seeded, checkout: &Path) -> Result<ChangeSetId, String> {
    let db = landing::open(seeded.engine.path()).map_err(debug)?;
    agent_change(&db, &seeded.project, checkout)
}

/// Names the decision an Accept came to, as the owner sees it.
/// Input: the outcome. Output: the landed commit and branch, or the conflicting files.
fn decision(outcome: &AcceptOutcome) -> Value {
    match outcome {
        AcceptOutcome::Landed { commit, branch } => {
            json!({"landed": {"commit": commit, "branch": branch}})
        }
        AcceptOutcome::Conflict { files } => json!({ "conflict": files }),
    }
}

/// Reads a branch as git keeps it: each commit's parents, author, subject and body, newest
/// first, and every file at its tip with its text. Inputs: the checkout and the branch.
/// Output: the branch view.
fn branch_view(checkout: &Path, branch: &str) -> Value {
    let log = stdout(
        checkout,
        &["log", "--format=%P%x1f%an <%ae>%x1f%s%x1f%b%x1e", branch],
    );
    let commits: Vec<Value> = log
        .split('\x1e')
        .map(|entry| entry.trim_start_matches('\n'))
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let parts: Vec<&str> = entry.split('\x1f').collect();
            let part = |index: usize| parts.get(index).copied().unwrap_or("");
            json!({
                "parents": part(0).split_whitespace().collect::<Vec<_>>(),
                "author": part(1),
                "subject": part(2),
                "body": lines(part(3).trim_end()),
            })
        })
        .collect();
    let files: Vec<Value> = stdout(checkout, &["ls-tree", "-r", "--name-only", branch])
        .lines()
        .map(|file| {
            let shown = stdout(checkout, &["show", &format!("{branch}:{file}")]);
            json!({"path": file, "text": lines(&shown)})
        })
        .collect();
    json!({"branch": branch, "commits": commits, "files": files})
}

/// Runs git in a folder and reads what it printed. Inputs: the folder and the arguments.
/// Output: standard output as text.
fn stdout(folder: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&e2e_support::git(folder, args).stdout).into_owned()
}

/// Splits text into its lines for a golden. Input: the text. Output: the lines.
pub(super) fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Accepts one agent change set through the owner's Changes request under a Landing setting,
/// with the owner's message if given, and captures the decision and the owner's branch.
/// Inputs: the fixture prefix, the Landing setting (none keeps the default) and the message.
/// Output: the Landing the status shows, the proposed message, the decision and the branch.
fn accept_under(
    prefix: &str,
    landing: Option<LandingMode>,
    message: Option<&str>,
) -> Result<Value, String> {
    let mut seeded = seeded(prefix, no_plugins, landing)?;
    let checkout = checkout(&seeded)?;
    let set = recorded_change(&seeded, &checkout)?;
    let accepted = owner_accept(&mut seeded, set, message, None)?;
    Ok(json!({
        "landing": accepted.landing,
        "proposed_message": lines(&accepted.proposed),
        "decision": decision(&accepted.outcome),
        "owner": branch_view(&checkout, "main"),
        "owner_status": lines(&stdout(&checkout, &["status", "--porcelain"])),
    }))
}

/// Accept under the Landing a new project starts with. Input: unused.
/// Output: see `accept_under`.
fn accept_squash(_scratch: &Path) -> Result<Value, String> {
    accept_under("ca-p4-squash-", None, None)
}

/// Accept under Landing Merge, set by the owner's request. Input: unused.
/// Output: see `accept_under`.
fn accept_merge(_scratch: &Path) -> Result<Value, String> {
    accept_under("ca-p4-merge-", Some(LandingMode::Merge), None)
}

/// Accept under the default Landing with the owner's own message. Input: unused.
/// Output: see `accept_under`.
fn accept_edited_message(_scratch: &Path) -> Result<Value, String> {
    accept_under(
        "ca-p4-message-",
        None,
        Some("Tide table fix, as the owner wrote it"),
    )
}

/// Accepts a change set whose README line the owner also changed after the agent started,
/// and captures the conflict and the owner's untouched branch and files. Input: unused.
/// Output: the decision, the owner's branch and its working state.
fn accept_conflict(_scratch: &Path) -> Result<Value, String> {
    let mut seeded = seeded("ca-p4-conflict-", no_plugins, None)?;
    let checkout = checkout(&seeded)?;
    let set = recorded_change(&seeded, &checkout)?;
    fs::write(checkout.join("README"), "one\ntwo owners\nthree\n").map_err(text)?;
    e2e_support::git(&checkout, &["commit", "--quiet", "-am", "Owner: line two"]);
    let accepted = owner_accept(&mut seeded, set, None, None)?;
    Ok(json!({
        "decision": decision(&accepted.outcome),
        "owner": branch_view(&checkout, "main"),
        "owner_status": lines(&stdout(&checkout, &["status", "--porcelain"])),
    }))
}

/// Accepts a change set into a branch checked out nowhere, which moves only that branch,
/// and captures both branches and the owner's working state. Input: unused.
/// Output: the decision and both branches.
fn accept_into_free_branch(_scratch: &Path) -> Result<Value, String> {
    let mut seeded = seeded("ca-p4-free-", no_plugins, None)?;
    let checkout = checkout(&seeded)?;
    e2e_support::git(&checkout, &["branch", "release"]);
    let set = recorded_change(&seeded, &checkout)?;
    let accepted = owner_accept(&mut seeded, set, None, Some("release"))?;
    Ok(json!({
        "decision": decision(&accepted.outcome),
        "release": branch_view(&checkout, "release"),
        "owner": branch_view(&checkout, "main"),
        "owner_status": lines(&stdout(&checkout, &["status", "--porcelain"])),
    }))
}
