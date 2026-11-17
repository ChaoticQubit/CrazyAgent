use super::{Masks, Scenario, compare, normalise, refuse_private, snapshots_root, withheld};
use crate::e2e_support::{self, Engine, Owner};
use crazyagent::cleanup_settings::{CleanupMode, CleanupSettings};
use crazyagent::engine::owner::{AccountSecret, OwnerResponse, OwnerResult, SavedText};
use crazyagent::github::GitHubPromptAnswer;
use crazyagent::harnesses::leftovers::LeftoverMode;
use crazyagent::harnesses::update::{
    CheckWhen, InstallFromApp, OnlineLookup, UpdateMode, UpdateSettings,
};
use crazyagent::learning::MarkStep;
use crazyagent::master_frame_limit::{FrameSize, MasterFrameSettings};
use crazyagent::master_step_limit::{MasterStepSettings, StepLimit};
use crazyagent::master_time_limit::{MasterTimeSettings, TimeLimit};
use crazyagent::memory::library::MemoryScope;
use crazyagent::plugins::{Scope, ServerSpec};
use crazyagent::project_overview::Apply;
use crazyagent::protocol::ProjectId;
use crazyagent::provider_retry::ProviderRetry;
use crazyagent::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crazyagent::roadmap_status::Refresh;
use crazyagent::sandbox::policy::AgentPolicy;
use crazyagent::sandbox_settings::SandboxSettings;
use crazyagent::setup::RefusedTool;
use crazyagent::team::agent_messages::{KEPT_CHOICES, MessageSettings, PAGE_CHOICES, ResultsShown};
use crazyagent::team::master_limits::{ROUND_CHOICES, RUN_CHOICES_SECS};
use crazyagent::team::wall::TerminalsPerWindow;
use crazyagent::team::{
    BUDGET_CHOICES, BackgroundAtEnd, BranchMode, CAP_CHOICES, CatchUpMain, ChildLanding,
    DEPTH_CHOICES, IDLE_CHOICES_SECS, LIMIT_CHOICES_SECS, LandingMode, MasterIdleStop, MasterLands,
    NEW_FOLDER_FILES_CHOICES, ReuseIdle, SwitchMode, TeamAction,
};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value, json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The project id the storage fixture creates through the engine.
const PROJECT: &str = "snapshot-p5";

/// The fake secret the fixture's MCP server is added with. It matches no credential shape,
/// and a dump that shows it anywhere fails, so a column holding a typed secret is caught.
const SENTINEL: &str = "SNAPSHOT-SENTINEL-NOT-A-SECRET";

/// The PATH the fixture engine runs with, so no tool the owner installed lands in a row.
const FIXTURE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The fixture owner's git identity, so git never derives one from this machine's user.
const OWNER_GIT: &str = "[user]\n\tname = Owner Example\n\temail = owner@example.invalid\n";

/// Columns the dump never shows, by table: the raw owner request and reply frames, which can
/// carry a secret the owner typed, and the hash and result frame kept for one request.
const SECRET_COLUMNS: [(&str, &str); 6] = [
    ("native_auth_operations", "fact"),
    ("native_auth_operations", "request"),
    ("operations", "reply"),
    ("operations", "request"),
    ("owner_operations", "request_hash"),
    ("owner_operations", "result"),
];

/// Words that mark a secret in a column name or a JSON key, matched as a whole `_` or `-`
/// part: such a column is left out of the dump and such a key's value shows as `<excluded>`.
const SECRET_WORDS: [&str; 10] = [
    "secret",
    "secrets",
    "password",
    "passphrase",
    "credential",
    "credentials",
    "cookie",
    "cookies",
    "bearer",
    "apikey",
];

/// Spellings that mark a secret anywhere inside a column name or a JSON key.
const SECRET_PHRASES: [&str; 7] = [
    "api_key",
    "private_key",
    "access_token",
    "refresh_token",
    "id_token",
    "auth_token",
    "client_secret",
];

/// The tables the settings fixture writes through its owner requests. A settings dump with
/// one of them empty fails, so a request the engine answered without storing is caught.
const SEEDED_TABLES: [&str; 46] = [
    "agent_cleanup_settings",
    "agent_quit_settings",
    "configured_master_frames",
    "configured_master_steps",
    "configured_master_time",
    "github_publish_prompt",
    "harness_update_settings",
    "learning_marks",
    "memory_library",
    "plugin_items",
    "project_overview",
    "project_setups",
    "project_agent_policy",
    "project_read_paths",
    "projects_home",
    "provider_retry_settings",
    "roadmap_status_settings",
    "sandbox_settings",
    "setup_refused_tool",
    "team_account_spread",
    "team_agent_branches",
    "team_background",
    "team_caps",
    "team_catch_up_main",
    "team_done_quiet",
    "team_finished_view",
    "team_land_on",
    "team_landing_mode",
    "team_leftovers",
    "team_master_branch",
    "team_master_idle_stop",
    "team_master_lands",
    "team_master_limits",
    "team_master_live_terminal",
    "team_master_switch",
    "team_merge_ask",
    "team_message_settings",
    "team_nesting",
    "team_new_folder_limit",
    "team_reask_empty",
    "team_reuse_idle",
    "team_settings",
    "team_time_limits",
    "team_token_budgets",
    "team_wall",
    "user_profile",
];

/// Lists the storage scenarios. Output: the scenarios in run order.
pub(super) fn scenarios() -> Vec<Scenario> {
    let scenario = |name: &'static str, capture: fn(&Path) -> Result<Value, String>| Scenario {
        kind: "storage",
        name,
        capture,
    };
    vec![scenario("fresh", fresh), scenario("settings", settings)]
}

/// The engine's first start: launched on a fixture HOME, attached to once as the TUI does,
/// then stopped. Input: unused. Output: the dump of its engine.db.
fn fresh(_scratch: &Path) -> Result<Value, String> {
    let stopped = seeded("ca-p5-fresh-", false)?;
    dump(&stopped.engine.path().join("engine.db"))
}

/// The upgrade fixture's state: a profile, a projects home, one project with its team
/// settings, sandbox policy, extra read folder and instructions text, the app-wide settings,
/// two memories and three plugins, one an MCP server holding the fake secret. Input: unused. Output: the
/// checked dump of its engine.db.
fn settings(_scratch: &Path) -> Result<Value, String> {
    let stopped = seeded("ca-p5-settings-", true)?;
    settled(dump(&stopped.engine.path().join("engine.db"))?)
}

/// A real engine stopped after its owner requests, with the fixture HOME and the scratch disk
/// its folders live on kept until the dump is read.
struct Stopped {
    engine: Engine,
    _home: tempfile::TempDir,
    _disk: tempfile::TempDir,
}

/// Starts the engine on a fixture HOME and a fixed PATH, attaches as the TUI, sends the
/// settings requests when asked, then stops it. Inputs: a scratch prefix and whether to seed
/// the settings. Output: the stopped engine, or the request that failed.
fn seeded(prefix: &str, with_settings: bool) -> Result<Stopped, String> {
    let home = e2e_support::scratch("ca-p5-home-");
    fs::write(home.path().join(".gitconfig"), OWNER_GIT).map_err(said)?;
    let disk = e2e_support::scratch("ca-p5-disk-");
    let projects = disk.path().join("Projects");
    fs::create_dir(&projects).map_err(said)?;
    let folders = plugin_folders(disk.path())?;
    let project = ProjectId::new(PROJECT).map_err(debug)?;
    let mut engine = Engine::new(prefix);
    engine.home = Some(home.path().to_path_buf());
    engine.env.push(("PATH", FIXTURE_PATH.into()));
    engine.launch();
    let seeding = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(said)?
        .block_on(async {
            let mut owner = Owner::attach(&engine).await;
            let done = if with_settings {
                seed(&mut owner, &projects, &folders, &project).await
            } else {
                Ok(())
            };
            owner.controller.detach();
            done
        });
    let _stderr = engine.stop();
    seeding?;
    Ok(Stopped {
        engine,
        _home: home,
        _disk: disk,
    })
}

/// The skill folder and the hooks plugin folder the fixture adds and the extra read folder it
/// gives the project's agents, written on the scratch disk.
struct Folders {
    skill: PathBuf,
    hooks: PathBuf,
    reference: PathBuf,
}

/// Writes one skill folder, one hooks plugin folder and one reference folder.
/// Input: the scratch disk. Output: the folders.
fn plugin_folders(disk: &Path) -> Result<Folders, String> {
    let reference = disk.join("tide-tables");
    fs::create_dir(&reference).map_err(said)?;
    let skill = disk.join("skills").join("tide-notes");
    fs::create_dir_all(&skill).map_err(said)?;
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: tide-notes\ndescription: Writes tide table notes.\n---\nRead the tide table, then write one note per tide.\n",
    )
    .map_err(said)?;
    let hooks = disk.join("tide-hooks");
    fs::create_dir_all(hooks.join(".claude-plugin")).map_err(said)?;
    fs::create_dir_all(hooks.join("hooks")).map_err(said)?;
    fs::write(
        hooks.join(".claude-plugin").join("plugin.json"),
        "{\"name\": \"tide-hooks\"}\n",
    )
    .map_err(said)?;
    fs::write(
        hooks.join("hooks").join("hooks.json"),
        "{\"hooks\": {\"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"/bin/echo checked\"}]}]}}\n",
    )
    .map_err(said)?;
    Ok(Folders {
        skill,
        hooks,
        reference,
    })
}

/// Sends every settings request in a fixed order, so two seedings give the same revisions.
/// Inputs: the attached owner, the projects home, the plugin folders and the project.
/// Output: nothing, or the request the engine did not store.
async fn seed(
    owner: &mut Owner,
    projects: &Path,
    folders: &Folders,
    project: &ProjectId,
) -> Result<(), String> {
    applied(
        "save_profile",
        owner.save_profile(e2e_support::profile()).await,
    )?;
    applied("set_home", owner.set_home(projects).await)?;
    applied(
        "create_project",
        owner.create_project(PROJECT, "Snapshot P5").await,
    )?;
    for (step, action) in team_actions(project) {
        let generation = owner.next();
        recorded(step, owner.controller.team_action(action, generation).await)?;
    }
    versioned_settings(owner, folders, project).await?;
    app_settings(owner).await?;
    project_settings(owner, project).await?;
    plugins(owner, folders, project).await
}

/// Every team setting an owner request sets without a read version, each away from its
/// default where it has one, with values taken from the engine's own choice lists.
/// Input: the project. Output: each request with its step name.
fn team_actions(project: &ProjectId) -> Vec<(&'static str, TeamAction)> {
    let id = || project.clone();
    vec![
        (
            "sub_agent_depth",
            TeamAction::SubAgentDepth {
                project_id: id(),
                depth: DEPTH_CHOICES[1],
            },
        ),
        (
            "child_landing",
            TeamAction::ChildLanding {
                project_id: id(),
                mode: ChildLanding::Owner,
            },
        ),
        (
            "agent_branches",
            TeamAction::AgentBranches {
                project_id: id(),
                mode: BranchMode::Detached,
            },
        ),
        (
            "master_branch",
            TeamAction::MasterBranch {
                project_id: id(),
                branch: Some("tide-lead".to_owned()),
            },
        ),
        (
            "land_on",
            TeamAction::LandOn {
                project_id: id(),
                branch: Some("tide-main".to_owned()),
            },
        ),
        (
            "background_at_end",
            TeamAction::BackgroundAtEnd {
                project_id: id(),
                choice: BackgroundAtEnd::Grace { secs: 120 },
            },
        ),
        (
            "leftovers_at_end",
            TeamAction::LeftoversAtEnd {
                project_id: id(),
                mode: LeftoverMode::Keep,
            },
        ),
        (
            "ask_before_merge",
            TeamAction::AskBeforeMerge {
                project_id: id(),
                on: true,
            },
        ),
        (
            "master_lands",
            TeamAction::MasterLands {
                project_id: id(),
                mode: MasterLands::Waits,
            },
        ),
        (
            "reask_empty_reply",
            TeamAction::ReaskEmptyReply {
                project_id: id(),
                on: true,
            },
        ),
        (
            "done_quiet",
            TeamAction::DoneQuiet {
                project_id: id(),
                secs: Some(IDLE_CHOICES_SECS[1]),
            },
        ),
        (
            "master_idle_stop",
            TeamAction::MasterIdleStop {
                project_id: id(),
                choice: MasterIdleStop::OneHour,
            },
        ),
        (
            "master_live_terminal",
            TeamAction::MasterLiveTerminal {
                project_id: id(),
                on: true,
            },
        ),
        (
            "catch_up_main",
            TeamAction::CatchUpMain {
                project_id: id(),
                mode: CatchUpMain::Off,
            },
        ),
        (
            "reuse_idle",
            TeamAction::ReuseIdle {
                project_id: id(),
                mode: ReuseIdle::On,
            },
        ),
        (
            "landing_mode",
            TeamAction::LandingMode {
                project_id: id(),
                mode: LandingMode::Merge,
            },
        ),
        (
            "new_folder_limit",
            TeamAction::NewFolderLimit {
                project_id: id(),
                files: NEW_FOLDER_FILES_CHOICES[2],
            },
        ),
        (
            "terminals_per_window",
            TeamAction::TerminalsPerWindow {
                project_id: id(),
                per_window: TerminalsPerWindow::Four,
            },
        ),
        (
            "message_settings",
            TeamAction::MessageSettings {
                project_id: id(),
                settings: MessageSettings {
                    keep_master_messages: true,
                    kept_per_project: KEPT_CHOICES[1],
                    progress_shown: false,
                    results: ResultsShown::Full,
                    owner_items_shown: true,
                    page_size: PAGE_CHOICES[0],
                },
            },
        ),
        (
            "max_harnesses",
            TeamAction::MaxHarnesses {
                project_id: id(),
                max: Some(CAP_CHOICES[2]),
            },
        ),
        (
            "max_teammates",
            TeamAction::MaxTeammates {
                project_id: id(),
                max: Some(CAP_CHOICES[4]),
            },
        ),
        (
            "master_rounds",
            TeamAction::MasterRounds {
                project_id: id(),
                max: Some(ROUND_CHOICES[2]),
            },
        ),
        (
            "master_run_time",
            TeamAction::MasterRunTime {
                project_id: id(),
                secs: Some(RUN_CHOICES_SECS[1]),
            },
        ),
        (
            "token_budget",
            TeamAction::TokenBudget {
                project_id: id(),
                tokens: Some(BUDGET_CHOICES[1]),
            },
        ),
        (
            "spread_accounts",
            TeamAction::SpreadAccounts {
                project_id: id(),
                on: true,
            },
        ),
    ]
}

/// Sends the settings that carry the version of a stored item. Each is that item's first
/// write, so version 0, except the four team settings that share one item and so take 0 to 3
/// in order (team.rs `settings_write`): keep warm, time limits, the finished agents view and
/// the harness switch mode; then the sandbox settings, the agent policy, the extra read folder
/// and the app-wide and project instructions text. Inputs: the attached owner, the fixture
/// folders and the project. Output: nothing, or the request the engine did not store.
async fn versioned_settings(
    owner: &mut Owner,
    folders: &Folders,
    project: &ProjectId,
) -> Result<(), String> {
    let id = || project.clone();
    let team = [
        (
            "keep_warm",
            TeamAction::KeepWarm {
                project_id: id(),
                on: false,
                version: 0,
            },
        ),
        (
            "time_limits",
            TeamAction::TimeLimits {
                project_id: id(),
                turn_secs: Some(LIMIT_CHOICES_SECS[0]),
                session_secs: Some(LIMIT_CHOICES_SECS[2]),
                idle_secs: Some(IDLE_CHOICES_SECS[0]),
                version: 1,
            },
        ),
        (
            "finished_agents",
            TeamAction::FinishedAgents {
                project_id: id(),
                show: true,
                version: 2,
            },
        ),
        (
            "on_harness_switch",
            TeamAction::OnHarnessSwitch {
                project_id: id(),
                mode: SwitchMode::Fresh,
                version: 3,
            },
        ),
    ];
    for (step, action) in team {
        let generation = owner.next();
        recorded(step, owner.controller.team_action(action, generation).await)?;
    }
    let generation = owner.next();
    recorded(
        "sandbox_settings",
        owner
            .controller
            .set_sandbox_settings(SandboxSettings::default(), 0, generation)
            .await,
    )?;
    let generation = owner.next();
    let policy = AgentPolicy {
        ide_git_hooks: true,
        ..AgentPolicy::default()
    };
    recorded(
        "agent_policy",
        owner
            .controller
            .set_project_agent_policy(id(), 0, policy, generation)
            .await,
    )?;
    let generation = owner.next();
    recorded(
        "read_paths",
        owner
            .controller
            .set_project_read_paths(id(), 0, vec![path_text(&folders.reference)?], generation)
            .await,
    )?;
    for (step, scope) in [
        ("app_process_text", None),
        ("project_process_text", Some(id())),
    ] {
        let saved = owner
            .controller
            .save_process_text(scope, "Write one note per tide.".to_owned(), 0, None)
            .await
            .map_err(|error| format!("{step}: {error:?}"))?;
        if !matches!(saved, SavedText::Saved { .. }) {
            return Err(format!(
                "{step}: the engine answered that the text changed since"
            ));
        }
    }
    Ok(())
}

/// Sends the app-wide settings requests: cleanup, quit, provider retry, the three master
/// limits, the refused-tool choice, one learning mark, harness updates and a global memory.
/// Input: the attached owner. Output: nothing, or the request the engine did not store.
async fn app_settings(owner: &mut Owner) -> Result<(), String> {
    let generation = owner.next();
    recorded(
        "cleanup_settings",
        owner
            .controller
            .set_cleanup_settings(
                CleanupSettings {
                    mode: CleanupMode::AfterDays,
                    days: 14,
                },
                generation,
            )
            .await,
    )?;
    let generation = owner.next();
    recorded(
        "quit_settings",
        owner
            .controller
            .set_quit_settings(
                QuitSettings {
                    on_quit: OnQuit::KeepRunning,
                    left_running: LeftRunning::AfterHours,
                    hours: 12,
                },
                generation,
            )
            .await,
    )?;
    let generation = owner.next();
    recorded(
        "provider_retry",
        owner
            .controller
            .set_provider_retry(ProviderRetry::StopAtOnce, generation)
            .await,
    )?;
    let generation = owner.next();
    recorded(
        "master_frame_settings",
        owner
            .controller
            .set_master_frame_settings(
                MasterFrameSettings {
                    limit: FrameSize::Kib256,
                },
                generation,
            )
            .await,
    )?;
    let generation = owner.next();
    recorded(
        "master_step_settings",
        owner
            .controller
            .set_master_step_settings(
                MasterStepSettings {
                    limit: StepLimit::Steps100,
                },
                generation,
            )
            .await,
    )?;
    let generation = owner.next();
    recorded(
        "master_time_settings",
        owner
            .controller
            .set_master_time_settings(
                MasterTimeSettings {
                    limit: TimeLimit::Minutes30,
                },
                generation,
            )
            .await,
    )?;
    let generation = owner.next();
    recorded(
        "refused_tool",
        owner
            .controller
            .set_refused_tool(RefusedTool::CommandOnly, generation)
            .await,
    )?;
    let generation = owner.next();
    let mark = owner
        .controller
        .set_learning_mark(MarkStep::Process, true, generation)
        .await
        .map_err(|error| format!("learning_mark: {error:?}"))?
        .ok_or("learning_mark: the engine already held the mark")?;
    recorded("learning_mark", Ok::<OwnerResponse, String>(mark))?;
    let update = UpdateSettings {
        mode: UpdateMode::CheckOnly,
        when: CheckWhen::OnButton,
        online: OnlineLookup::Off,
        install: InstallFromApp::Off,
    };
    let stored = owner
        .controller
        .harness_set_update_settings(update)
        .await
        .map_err(|error| format!("harness_update_settings: {error:?}"))?;
    if stored != update {
        return Err(format!("harness_update_settings: stored {stored:?}"));
    }
    let generation = owner.next();
    recorded(
        "global_memory",
        owner
            .controller
            .add_memory(
                MemoryScope::Global,
                "Tide tables are read at dawn.".to_owned(),
                generation,
            )
            .await,
    )
}

/// Sends the project settings requests outside the team: roadmap and overview refresh, the
/// overview apply choice, the publish question answer and a project memory.
/// Inputs: the attached owner and the project. Output: nothing, or the refused request.
async fn project_settings(owner: &mut Owner, project: &ProjectId) -> Result<(), String> {
    owner
        .controller
        .roadmap_set_refresh(project.clone(), Refresh::RefreshOnly)
        .await
        .map_err(|error| format!("roadmap_refresh: {error:?}"))?;
    owner
        .controller
        .overview_set_refresh(project.clone(), Refresh::Off)
        .await
        .map_err(|error| format!("overview_refresh: {error:?}"))?;
    owner
        .controller
        .overview_set_apply(project.clone(), Apply::WhenUnedited)
        .await
        .map_err(|error| format!("overview_apply: {error:?}"))?;
    owner
        .controller
        .github_publish_prompt_set(project.clone(), GitHubPromptAnswer::Never)
        .await
        .map_err(|error| format!("publish_prompt: {error:?}"))?;
    let generation = owner.next();
    recorded(
        "project_memory",
        owner
            .controller
            .add_memory(
                MemoryScope::Project {
                    project_id: project.clone(),
                },
                "Buoy alerts go to the harbour channel.".to_owned(),
                generation,
            )
            .await,
    )
}

/// Adds the project's three plugins: the skill folder and the hooks folder, each switched on,
/// and a stdio MCP server added with the fake secret, then approved.
/// Inputs: the attached owner, the folders and the project. Output: nothing, or the refusal.
async fn plugins(owner: &mut Owner, folders: &Folders, project: &ProjectId) -> Result<(), String> {
    let scope = || Scope::Project {
        project_id: project.clone(),
    };
    let skill = owner
        .controller
        .add_skill(scope(), path_text(&folders.skill)?)
        .await
        .map_err(|error| format!("add_skill: {error:?}"))?;
    if !skill.enabled {
        owner
            .controller
            .set_skill_enabled(skill.id, true, skill.version)
            .await
            .map_err(|error| format!("set_skill_enabled: {error:?}"))?;
    }
    let hooks = owner
        .controller
        .add_hooks(scope(), path_text(&folders.hooks)?)
        .await
        .map_err(|error| format!("add_hooks: {error:?}"))?;
    for hook in hooks.into_iter().filter(|hook| !hook.enabled) {
        owner
            .controller
            .set_hook_enabled(hook.id, true)
            .await
            .map_err(|error| format!("set_hook_enabled: {error:?}"))?;
    }
    let spec = ServerSpec::Stdio {
        command: "/bin/cat".to_owned(),
        args: Vec::new(),
    };
    let secret = AccountSecret::new(SENTINEL.as_bytes().to_vec()).map_err(debug)?;
    let server = owner
        .controller
        .add_mcp_server(
            scope(),
            "tide-server".to_owned(),
            spec.clone(),
            vec![("TIDE_KEY".to_owned(), secret)],
        )
        .await
        .map_err(|error| format!("add_mcp_server: {error:?}"))?;
    owner
        .controller
        .approve_mcp_server(server.id, spec)
        .await
        .map_err(|error| format!("approve_mcp_server: {error:?}"))?;
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

/// Turns an owner change's answer into nothing when the engine recorded and applied it.
/// Inputs: the request's name and the answer. Output: nothing, or the name with the answer.
fn recorded<E: std::fmt::Debug>(
    step: &str,
    response: Result<OwnerResponse, E>,
) -> Result<(), String> {
    match response.map_err(|error| format!("{step}: {error:?}"))? {
        OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied => Ok(()),
        other => Err(format!("{step}: {other:?}")),
    }
}

/// Checks a settings dump: every seeded table holds a row and the fake secret shows nowhere.
/// Input: the dump. Output: the same dump, or the empty tables or the leak found.
fn settled(dump: Value) -> Result<Value, String> {
    let empty: Vec<&str> = SEEDED_TABLES
        .into_iter()
        .filter(|name| {
            table_entry(&dump, name)
                .and_then(|entry| entry["row_count"].as_i64())
                .is_none_or(|count| count == 0)
        })
        .collect();
    if !empty.is_empty() {
        return Err(format!("seeded tables hold no row: {}", empty.join(", ")));
    }
    if serde_json::to_string(&dump)
        .map_err(said)?
        .contains(SENTINEL)
    {
        return Err(
            "the dump shows the fake MCP secret: a secret column is not excluded".to_owned(),
        );
    }
    Ok(dump)
}

/// Dumps every table of an engine.db as a list sorted by name, so no table name is a key the
/// normalisation reads: its CREATE text with its indexes and triggers, its row count, the
/// columns left out as secret, and its other columns' rows in a stable order. Reads a scratch
/// copy, so nothing is written next to the file. Input: the engine.db path. Output: the dump,
/// or why it could not be read.
pub(super) fn dump(db: &Path) -> Result<Value, String> {
    let (_scratch, connection) = scratch_copy(db)?;
    let probe = Masks::new();
    let mut tables = Vec::new();
    for (name, sql) in schema(&connection, "table")? {
        tables.push(table(&connection, &probe, &name, &sql)?);
    }
    let views: Vec<Value> = schema(&connection, "view")?
        .into_iter()
        .map(|(_, sql)| lines(&sql))
        .collect();
    Ok(json!({"tables": tables, "views": views}))
}

/// Finds one table's entry in a dump. Inputs: the dump and the table name. Output: the
/// entry, or none when the engine.db has no such table.
fn table_entry<'a>(dump: &'a Value, name: &str) -> Option<&'a Value> {
    dump["tables"]
        .as_array()?
        .iter()
        .find(|entry| entry["table"] == name)
}

/// Copies an engine.db and the WAL beside it, when there is one, into a new scratch folder and
/// opens the copy there, so SQLite reads the WAL pages too and writes nothing beside the
/// original. Input: the engine.db path. Output: the scratch folder, which must outlive the
/// connection, and the connection.
fn scratch_copy(db: &Path) -> Result<(tempfile::TempDir, Connection), String> {
    let scratch = tempfile::tempdir().map_err(said)?;
    let copy = scratch.path().join("engine.db");
    fs::copy(db, &copy).map_err(said)?;
    let wal = wal_path(db);
    if wal.exists() {
        fs::copy(&wal, wal_path(&copy)).map_err(said)?;
    }
    let connection =
        Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(said)?;
    Ok((scratch, connection))
}

/// Names the WAL file SQLite keeps beside a database. Input: the database path.
/// Output: the path with `-wal` appended.
fn wal_path(db: &Path) -> PathBuf {
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// Lists one kind of schema item with its CREATE text, by name, leaving out SQLite's own.
/// Inputs: the connection and `table` or `view`. Output: each item's name and text.
fn schema(connection: &Connection, kind: &str) -> Result<Vec<(String, String)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT name, sql FROM sqlite_master WHERE type = ?1 AND sql IS NOT NULL AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        )
        .map_err(said)?;
    let items = statement
        .query_map([kind], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(said)?;
    items.collect::<Result<Vec<_>, _>>().map_err(said)
}

/// Dumps one table. Inputs: the connection, a mask probe, the table's name and CREATE text.
/// Output: the table's schema, row count, excluded columns and rows.
fn table(connection: &Connection, probe: &Masks, name: &str, sql: &str) -> Result<Value, String> {
    let mut attached = connection
        .prepare(
            "SELECT sql FROM sqlite_master WHERE tbl_name = ?1 AND type IN ('index', 'trigger') AND sql IS NOT NULL ORDER BY type, name",
        )
        .map_err(said)?;
    let attached: Vec<Value> = attached
        .query_map([name], |row| row.get::<_, String>(0))
        .map_err(said)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(said)?
        .iter()
        .map(|text| lines(text))
        .collect();
    let mut listed = connection
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .map_err(said)?;
    let columns: Vec<String> = listed
        .query_map([name], |row| row.get::<_, String>(0))
        .map_err(said)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(said)?;
    let (shown, excluded): (Vec<String>, Vec<String>) = columns
        .into_iter()
        .partition(|column| !secret_column(name, column));
    let count: i64 = connection
        .query_row(
            &format!("SELECT COUNT(*) FROM {}", quoted(name)),
            [],
            |row| row.get(0),
        )
        .map_err(said)?;
    let mut rows = Vec::new();
    if !shown.is_empty() {
        let list: Vec<String> = shown.iter().map(|column| quoted(column)).collect();
        let mut select = connection
            .prepare(&format!("SELECT {} FROM {}", list.join(", "), quoted(name)))
            .map_err(said)?;
        let mut found = select.query([]).map_err(said)?;
        while let Some(row) = found.next().map_err(said)? {
            let mut cells = Map::new();
            for (index, column) in shown.iter().enumerate() {
                cells.insert(
                    column.clone(),
                    cell(probe, row.get_ref(index).map_err(said)?)?,
                );
            }
            rows.push(Value::Object(cells));
        }
    }
    assert!(shown.is_empty() || usize::try_from(count).is_ok_and(|count| count == rows.len()));
    Ok(json!({
        "table": name,
        "schema": lines(sql),
        "attached": attached,
        "excluded": excluded,
        "row_count": count,
        "rows": stable(rows)?,
    }))
}

/// Shows one cell: an integer or real as a number, text as a string or, when it holds a JSON
/// object or array, as that JSON; a blob as its UTF-8 text the same way, else its byte count.
/// Inputs: a mask probe and the cell. Output: the cell as JSON.
fn cell(probe: &Masks, value: ValueRef<'_>) -> Result<Value, String> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(number) => Value::from(number),
        ValueRef::Real(number) => serde_json::Number::from_f64(number)
            .map(Value::Number)
            .ok_or("a REAL cell is not a finite number")?,
        ValueRef::Text(bytes) => text_cell(probe, &String::from_utf8_lossy(bytes))?,
        ValueRef::Blob(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => json!({"blob": text_cell(probe, text)?}),
            Err(_) => json!({"blob_bytes": bytes.len()}),
        },
    })
}

/// Shows one text cell, parsed when it holds a JSON object or array. Inputs: a mask probe and
/// the text. Output: the parsed and tidied JSON, or the text as a string.
fn text_cell(probe: &Masks, text: &str) -> Result<Value, String> {
    match serde_json::from_str::<Value>(text) {
        Ok(parsed @ (Value::Object(_) | Value::Array(_))) => tidy(probe, parsed),
        _ => Ok(Value::from(text)),
    }
}

/// Readies JSON from a cell for the comparer: a secret-named key's value becomes `<excluded>`,
/// and an object with a key the normalisation would mask becomes a stable list of key and
/// value pairs. Inputs: a mask probe and the JSON. Output: the tidied JSON.
fn tidy(probe: &Masks, value: Value) -> Result<Value, String> {
    match value {
        Value::Array(items) => items
            .into_iter()
            .map(|item| tidy(probe, item))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(map) => {
            let masked_key = map.keys().any(|key| probe.clone().text(key) != *key);
            let mut tidied = Map::new();
            for (key, item) in map {
                let item = if secret_name(&key) {
                    Value::from("<excluded>")
                } else {
                    tidy(probe, item)?
                };
                tidied.insert(key, item);
            }
            if masked_key {
                let pairs = tidied
                    .into_iter()
                    .map(|(key, value)| json!({"key": key, "value": value}))
                    .collect();
                stable(pairs).map(Value::Array)
            } else {
                Ok(Value::Object(tidied))
            }
        }
        other => Ok(other),
    }
}

/// Orders values by their normalised text alone, keeping the read order between equal ones,
/// so a random id never decides the order and the id numbering stays the same between runs.
/// Input: the values. Output: the values in stable order.
fn stable(values: Vec<Value>) -> Result<Vec<Value>, String> {
    let mut keyed = values
        .into_iter()
        .map(|value| Ok((normalise(&value)?, value)))
        .collect::<Result<Vec<(String, Value)>, String>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(keyed.into_iter().map(|(_, value)| value).collect())
}

/// Says whether a table's column is left out of the dump. Inputs: the table and column names.
/// Output: true for a listed frame column or a secret-named column.
fn secret_column(table: &str, column: &str) -> bool {
    SECRET_COLUMNS.contains(&(table, column)) || secret_name(column)
}

/// Says whether a column name or JSON key names a secret. Input: the name. Output: true when
/// a `_` or `-` part is a secret word or the name holds a secret phrase.
fn secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower
        .split(['_', '-'])
        .any(|part| SECRET_WORDS.contains(&part))
        || SECRET_PHRASES.iter().any(|phrase| lower.contains(phrase))
}

/// Quotes an SQL identifier. Input: the name. Output: the name in double quotes.
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Splits text into its lines, for a readable golden. Input: the text. Output: the lines.
fn lines(text: &str) -> Value {
    Value::from(text.lines().map(str::to_owned).collect::<Vec<_>>())
}

/// Gives a folder's path as text. Input: the folder. Output: the path, or why it is not UTF-8.
fn path_text(folder: &Path) -> Result<String, String> {
    folder
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| "a fixture folder path is not UTF-8".to_owned())
}

/// Prints an error through its Display form. Input: the error. Output: the text.
fn said(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Prints an error through its Debug form. Input: the error. Output: the text.
fn debug(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

/// The committed 2.4.2 upgrade fixture. Output: tests/fixtures/v242-engine.db below this crate.
fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v242-engine.db")
}

/// Refuses private text with this machine's HOME and USER and with the passwd entry's home
/// and name, the sources the engine reads. A user name that is also a snapshot kind, such as a
/// farm account named runner, is not checked, as the runner's failure lines do.
/// Input: the text. Output: nothing, or the kind of private text found.
fn private_text(text: &str) -> Result<(), String> {
    let owner = nix::unistd::User::from_uid(nix::unistd::getuid())
        .map_err(said)?
        .ok_or("this user has no passwd entry")?;
    let personal = |name: &str| !super::KINDS.contains(&name);
    let home = std::env::var("HOME").ok();
    let user = std::env::var("USER").ok().filter(|user| personal(user));
    let name = Some(owner.name.as_str()).filter(|name| personal(name));
    refuse_private(text, home.as_deref(), user.as_deref())?;
    refuse_private(text, owner.dir.to_str(), name)
}

/// Refuses private text in every cell of every table of an engine.db, the excluded columns
/// too, with blobs read as UTF-8 where they hold it. Input: the engine.db path. Output:
/// nothing, or the table and the kind of private text found.
fn private_cells(db: &Path) -> Result<(), String> {
    let (_scratch, connection) = scratch_copy(db)?;
    for (name, _) in schema(&connection, "table")? {
        let mut select = connection
            .prepare(&format!("SELECT * FROM {}", quoted(&name)))
            .map_err(said)?;
        let width = select.column_count();
        let mut found = select.query([]).map_err(said)?;
        let mut text = String::new();
        while let Some(row) = found.next().map_err(said)? {
            for index in 0..width {
                match row.get_ref(index).map_err(said)? {
                    ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                        text.push_str(&String::from_utf8_lossy(bytes));
                        text.push('\n');
                    }
                    ValueRef::Null | ValueRef::Integer(_) | ValueRef::Real(_) => {}
                }
            }
        }
        private_text(&text).map_err(|kind| format!("table {name}: {kind}"))?;
    }
    Ok(())
}

/// Compares the committed upgrade fixture's dump with the settings golden. When the fixture
/// is missing and `write` is set, seeds a fresh engine, requires its dump to equal the golden,
/// a stop that left no WAL pages (the fixture is one file), and no private text in any cell or
/// in the raw bytes, then writes the fixture and fails so a writing run is never green. Input: the write switch. Output: nothing when the fixture
/// matches, else why not.
fn fixture_matches(write: bool) -> Result<(), String> {
    let golden = fs::read(snapshots_root().join("storage/settings.json"))
        .map_err(|error| format!("the storage/settings golden: {error}; capture it first"))?;
    let fixture = fixture_path();
    if fixture.exists() {
        return compare(&golden, &normalise(&settled(dump(&fixture)?)?)?);
    }
    if !write {
        return Err(
            "no upgrade fixture yet; write it with SNAPSHOT_V242_WRITE=1 on the 2.4.2 release commit"
                .to_owned(),
        );
    }
    let stopped = seeded("ca-p5-fixture-", true)?;
    let db = stopped.engine.path().join("engine.db");
    compare(&golden, &normalise(&settled(dump(&db)?)?)?)?;
    if fs::metadata(wal_path(&db)).is_ok_and(|meta| meta.len() > 0) {
        return Err(
            "the stopped engine left WAL pages beside engine.db, so the fixture would not be one file"
                .to_owned(),
        );
    }
    private_cells(&db)?;
    let bytes = fs::read(&db).map_err(said)?;
    private_text(&String::from_utf8_lossy(&bytes)).map_err(|kind| format!("raw bytes: {kind}"))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&fixture)
        .map_err(said)?;
    file.write_all(&bytes).map_err(said)?;
    Err("v242-engine.db written; review it, then run without SNAPSHOT_V242_WRITE".to_owned())
}

/// Prints a failure only when it holds no private text. Input: the failure. Output: the text,
/// or what kind of private text was withheld.
fn shown(failure: &str) -> String {
    let home = std::env::var("HOME").ok();
    let user = std::env::var("USER")
        .ok()
        .filter(|user| !super::KINDS.contains(&user.as_str()));
    withheld(failure, home.as_deref(), user.as_deref())
}

/// Builds a scratch SQLite file, not an engine.db, with a frame table, a plugin table whose
/// JSON bodies hold a secret key and an id-keyed map, and a settings table, its rows written
/// in the given order. Inputs: the folder, the file name, the plugin rows' order and the
/// harness cap. Output: the file's path.
fn sample(folder: &Path, file: &str, reversed: bool, harnesses: i64) -> PathBuf {
    let path = folder.join(file);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE operations (client TEXT NOT NULL, id TEXT NOT NULL, request BLOB NOT NULL, reply BLOB NOT NULL);
             CREATE TABLE plugin_items (id TEXT PRIMARY KEY, body TEXT NOT NULL);
             CREATE TABLE team_caps (project TEXT PRIMARY KEY, harnesses INTEGER, created_ms INTEGER);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO operations VALUES ('owner:tui', 'r1', ?1, x'00')",
            [SENTINEL.as_bytes()],
        )
        .unwrap();
    let mut bodies = vec![
        (
            "a",
            json!({"name": "a", "seen": {"3f2a9c1e5b7d4a608e1f2c3b4a5d6e7f": 1}}).to_string(),
        ),
        (
            "b",
            json!({"name": "b", "secrets": [SENTINEL], "api_key_ref": SENTINEL}).to_string(),
        ),
    ];
    if reversed {
        bodies.reverse();
    }
    for (id, body) in bodies {
        connection
            .execute(
                "INSERT INTO plugin_items VALUES (?1, ?2)",
                [id, body.as_str()],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO team_caps VALUES ('snapshot-p5', ?1, 1700000000000)",
            [harnesses],
        )
        .unwrap();
    path
}

#[test]
fn the_dump_leaves_out_secrets_orders_rows_stably_and_fails_on_one_changed_cell() {
    let scratch = tempfile::tempdir().unwrap();
    let first = dump(&sample(scratch.path(), "first.db", false, 3)).unwrap();
    let reordered = dump(&sample(scratch.path(), "reordered.db", true, 3)).unwrap();
    let changed = dump(&sample(scratch.path(), "changed.db", false, 4)).unwrap();
    assert!(!serde_json::to_string(&first).unwrap().contains(SENTINEL));
    let operations = table_entry(&first, "operations").unwrap();
    assert_eq!(operations["excluded"], json!(["request", "reply"]));
    assert_eq!(
        operations["rows"],
        json!([{"client": "owner:tui", "id": "r1"}])
    );
    let plugin_items = table_entry(&first, "plugin_items").unwrap();
    assert_eq!(plugin_items["row_count"], json!(2));
    let rows = plugin_items["rows"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|row| row["id"] == json!(id)).unwrap();
    assert_eq!(
        row("a")["body"]["seen"],
        json!([{"key": "3f2a9c1e5b7d4a608e1f2c3b4a5d6e7f", "value": 1}])
    );
    assert_eq!(row("b")["body"]["secrets"], json!("<excluded>"));
    assert_eq!(row("b")["body"]["api_key_ref"], json!("<excluded>"));
    assert_eq!(rows[0]["id"], json!("b"), "api_key_ref sorts before name");
    assert_eq!(normalise(&first), normalise(&reordered));
    assert!(normalise(&changed).unwrap().contains("\"harnesses\": 4"));
    assert_ne!(normalise(&first), normalise(&changed));
}

#[test]
fn the_dump_reads_rows_still_in_the_wal_beside_the_file_and_writes_nothing_there() {
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("engine.db");
    let writer = Connection::open(&path).unwrap();
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let pages: i64 = writer
        .query_row("PRAGMA wal_autocheckpoint=0", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pages, 0);
    writer
        .execute_batch(
            "CREATE TABLE team_caps (project TEXT PRIMARY KEY, harnesses INTEGER);
             INSERT INTO team_caps VALUES ('snapshot-p5', 3);",
        )
        .unwrap();
    let wal = scratch.path().join("engine.db-wal");
    let before = fs::read(&wal).unwrap();
    assert!(!before.is_empty());
    let dumped = dump(&path).unwrap();
    assert_eq!(
        table_entry(&dumped, "team_caps").unwrap()["rows"],
        json!([{"harnesses": 3, "project": "snapshot-p5"}])
    );
    assert_eq!(fs::read(&wal).unwrap(), before);
    drop(writer);
}

#[test]
fn the_upgrade_fixture_dumps_to_the_settings_golden() {
    let write = std::env::var_os("SNAPSHOT_V242_WRITE").is_some();
    let result =
        fixture_matches(write).map_err(|failure| shown(&format!("storage/fixture: {failure}")));
    assert_eq!(result, Ok(()));
}

#[test]
fn the_committed_upgrade_fixture_holds_no_private_text() {
    let fixture = fixture_path();
    assert!(
        fixture.exists(),
        "no upgrade fixture yet; write it with SNAPSHOT_V242_WRITE=1 on the 2.4.2 release commit"
    );
    assert_eq!(
        private_cells(&fixture).map_err(|failure| shown(&failure)),
        Ok(())
    );
    let bytes = fs::read(&fixture).unwrap();
    assert_eq!(
        private_text(&String::from_utf8_lossy(&bytes)).map_err(|failure| shown(&failure)),
        Ok(())
    );
}
