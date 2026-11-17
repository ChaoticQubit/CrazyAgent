use super::Scenario;
use super::e2e_support::Owner;
use super::launch::Team;
use super::provider::as_owner;
use super::workflow::{PROJECT, attached, engine, private_folder, recorded, reply, seeded};
use crazyagent::agent_git::Layout;
use crazyagent::coord::settings::{CoordSettings, RoleSkills, SkillRole};
use crazyagent::coord::tool_switches::BUILTIN_TOOLS;
use crazyagent::launch_context::{self, team_tools_section};
use crazyagent::plugins::Scope;
use crazyagent::protocol::ProjectId;
use crazyagent::sandbox::policy::AgentAccess;
use crazyagent::team::{AgentRole, DEPTH_DEFAULT, TeamAction};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};

/// The owner's own instructions the role scenarios try to save for one team agent.
const AGENT_TEXT: &str = "Answer in short paragraphs.\n";

/// The skills the launch scenarios add app-wide, each from its own SKILL.md folder.
const SKILLS: [&str; 2] = ["release-notes", "code-review"];

/// The harnesses a team agent launch installs for.
const HARNESSES: [&str; 2] = ["claude-code", "codex"];

/// The largest file whose text a launch capture keeps; a larger or non-UTF-8 file is kept
/// by its path alone.
const TEXT_BYTES_MAX: u64 = 64 * 1024;

/// The deepest folder a launch capture walks below an agent home.
const DEPTH_MAX: usize = 8;

/// Lists the agent role scenarios: the skills each team role gets, what a team agent
/// received before any launch, the team tools section of a master's or sub-agent's
/// instructions, and what each team role's launch receives.
/// Output: the scenarios in the order the snapshot row runs them.
pub(super) fn scenarios() -> Vec<Scenario> {
    let role = |name: &'static str, capture: fn(&Path) -> Result<Value, String>| Scenario {
        kind: "role",
        name,
        capture,
    };
    vec![
        role("skills-default", skills_default),
        role("skills-chosen-per-role", skills_chosen_per_role),
        role("skills-refused", skills_refused),
        role("instructions-before-launch", instructions_before_launch),
        role("agent-text-without-agent", agent_text_without_agent),
        role("tools-section-plain", tools_section_plain),
        role("tools-section-own-clone-catch-up", tools_section_own_clone),
        role("tools-section-tools-off", tools_section_tools_off),
        role("launch-all-approved", launch_all_approved),
        role("launch-chosen-per-role", launch_chosen_per_role),
        role("policy-master-live", policy_master_live),
        role(
            "policy-sub-agent-default-depth",
            policy_sub_agent_default_depth,
        ),
        role("policy-sub-agent-depth-1", policy_sub_agent_depth_1),
    ]
}

/// Runs one scenario's steps on a seeded [`super::workflow::PROJECT`] of a fresh engine.
/// Inputs: the scratch folder and the steps, which get the owner and the project.
/// Output: the capture.
fn on_project<F, Fut>(scratch: &Path, steps: F) -> Result<Value, String>
where
    F: FnOnce(Owner, ProjectId) -> Fut,
    Fut: Future<Output = (Owner, Result<Value, String>)>,
{
    let engine = engine(scratch)?;
    let scratch = scratch.to_path_buf();
    attached(&engine, |mut owner| async move {
        match seeded(&mut owner, &scratch).await {
            Ok(project) => steps(owner, project).await,
            Err(problem) => (owner, Err(problem)),
        }
    })
}

/// Reads a project's coordination settings from its team status, as the Team screen does.
/// Inputs: the owner and the project. Output: the settings, or what the engine answered.
async fn coord_settings(owner: &mut Owner, project: &ProjectId) -> Result<CoordSettings, String> {
    let status = owner.controller.team_status(project.clone()).await;
    status
        .map_err(|error| error.0)?
        .coordination
        .map(|view| view.settings)
        .ok_or_else(|| "the team status has no coordination view".to_owned())
}

/// Reads which skills each team role gets. Inputs: the owner and the project.
/// Output: the role skills as JSON.
async fn role_skills(owner: &mut Owner, project: &ProjectId) -> Result<Value, String> {
    let settings = coord_settings(owner, project).await?;
    serde_json::to_value(settings.role_skills).map_err(|error| error.to_string())
}

/// Saves one role skills choice the way the Team screen does: the coordination settings it
/// read with only the role skills replaced; the client checks the settings before sending.
/// Inputs: the owner, the project and the choice.
/// Output: the change's result, or the plain code the client or the engine refused with.
async fn save_role_skills(
    owner: &mut Owner,
    project: &ProjectId,
    chosen: RoleSkills,
) -> Result<Value, String> {
    let mut settings = coord_settings(owner, project).await?;
    settings.role_skills = chosen;
    let action = TeamAction::CoordSettings {
        project_id: project.clone(),
        settings,
    };
    let generation = owner.next();
    recorded(owner.controller.team_action(action, generation).await)
}

/// The skills each team role of a new project gets before the owner picks any.
/// Input: the scratch folder. Output: the role skills.
fn skills_default(scratch: &Path) -> Result<Value, String> {
    on_project(scratch, |mut owner, project| async move {
        let captured = role_skills(&mut owner, &project)
            .await
            .map(|read| json!({ "role_skills": read }));
        (owner, captured)
    })
}

/// One choice per role saved and read back: All approved skills for the master, two
/// chosen skills for a lead and no skills for a worker. Input: the scratch folder.
/// Output: the save's reply and the role skills read after it.
fn skills_chosen_per_role(scratch: &Path) -> Result<Value, String> {
    on_project(scratch, |mut owner, project| async move {
        let chosen = RoleSkills {
            master: None,
            lead: Some(vec!["release-notes".into(), "code-review".into()]),
            worker: Some(vec![]),
        };
        let captured: Result<Value, String> = async {
            let saved = save_role_skills(&mut owner, &project, chosen).await?;
            let read = role_skills(&mut owner, &project).await?;
            Ok(json!({ "saved": saved, "read": read }))
        }
        .await;
        (owner, captured)
    })
}

/// Two role skills choices the client refuses before sending, a name that is not a skill
/// folder name and a name chosen twice for one role, and the role skills read after them,
/// unchanged. Input: the scratch folder. Output: both replies and the read.
fn skills_refused(scratch: &Path) -> Result<Value, String> {
    on_project(scratch, |mut owner, project| async move {
        let unsafe_name = RoleSkills {
            lead: Some(vec!["Release Notes".into()]),
            ..RoleSkills::default()
        };
        let named_twice = RoleSkills {
            worker: Some(vec!["code-review".into(), "code-review".into()]),
            ..RoleSkills::default()
        };
        let captured: Result<Value, String> = async {
            let unsafe_name = save_role_skills(&mut owner, &project, unsafe_name).await?;
            let named_twice = save_role_skills(&mut owner, &project, named_twice).await?;
            let read = role_skills(&mut owner, &project).await?;
            Ok(json!({ "unsafe_name": unsafe_name, "named_twice": named_twice, "read": read }))
        }
        .await;
        (owner, captured)
    })
}

/// What the engine says a team agent received at its last launch when no agent has been
/// launched: for the project's newest master, for agent id 0 and for agent id 1.
/// Input: the scratch folder. Output: the three replies.
fn instructions_before_launch(scratch: &Path) -> Result<Value, String> {
    on_project(scratch, |owner, project| async move {
        let controller = &owner.controller;
        let newest_master = controller.agent_instructions(project.clone(), None).await;
        let agent_zero = controller
            .agent_instructions(project.clone(), Some(0))
            .await;
        let agent_one = controller.agent_instructions(project, Some(1)).await;
        let captured = reply(newest_master).and_then(|newest_master| {
            let agent_zero = reply(agent_zero)?;
            let agent_one = reply(agent_one)?;
            Ok(json!({
                "newest_master": newest_master,
                "agent_zero": agent_zero,
                "agent_one": agent_one,
            }))
        });
        (owner, captured)
    })
}

/// The owner's own instructions for team agent 1 of a project that never launched one: the
/// read and a first save. Input: the scratch folder. Output: both replies.
fn agent_text_without_agent(scratch: &Path) -> Result<Value, String> {
    on_project(scratch, |owner, project| async move {
        let read = owner.controller.agent_text(project.clone(), 1).await;
        let saved = owner
            .controller
            .save_agent_text(project, 1, AGENT_TEXT.into(), 0, None)
            .await;
        let captured = reply(read)
            .and_then(|read| reply(saved).map(|saved| json!({ "read": read, "saved": saved })));
        (owner, captured)
    })
}

/// The team tools section of a master's or sub-agent's instructions with every team tool
/// on, "Catch up with main" off and no clone of its own, from the public builder the
/// engine calls at launch. Input: the scratch folder, unused. Output: the section.
fn tools_section_plain(_scratch: &Path) -> Result<Value, String> {
    Ok(json!({ "section": team_tools_section(&[], false, false) }))
}

/// The team tools section of a sub-agent in its own clone while "Catch up with main" is
/// on, from the same public builder. Input: the scratch folder, unused.
/// Output: the section.
fn tools_section_own_clone(_scratch: &Path) -> Result<Value, String> {
    Ok(json!({ "section": team_tools_section(&[], true, true) }))
}

/// The team tools section of a sub-agent in its own clone with the first two built-in team
/// tools switched off, from the same public builder. Input: the scratch folder, unused.
/// Output: the tools left out and the section.
fn tools_section_tools_off(_scratch: &Path) -> Result<Value, String> {
    let off: Vec<String> = BUILTIN_TOOLS
        .iter()
        .take(2)
        .map(|tool| tool.0.to_owned())
        .collect();
    Ok(json!({ "off": off, "section": team_tools_section(&off, false, true) }))
}

/// Every team role's launch with the two app-wide skills and no role skills choice, so
/// each role gets All approved skills. Input: the scratch folder. Output: see [`launches`].
fn launch_all_approved(scratch: &Path) -> Result<Value, String> {
    launches(scratch, None)
}

/// Every team role's launch after the owner chose All approved skills for the master, one
/// approved skill and one name that is not approved for a lead, and no skills for a worker.
/// Input: the scratch folder. Output: see [`launches`].
fn launch_chosen_per_role(scratch: &Path) -> Result<Value, String> {
    let chosen = RoleSkills {
        master: None,
        lead: Some(vec!["release-notes".into(), "tide-notes".into()]),
        worker: Some(vec![]),
    };
    launches(scratch, Some(chosen))
}

/// Seeds the project with [`SKILLS`] and, when given, a role skills choice, all through
/// owner requests, stops the engine, then reads its engine.db read-only through the public
/// launch context calls the team runtime and the master conversation make.
/// Inputs: the scratch folder and the choice. Output: the choice's save and what each
/// launch receives (see [`read_launches`]).
fn launches(scratch: &Path, chosen: Option<RoleSkills>) -> Result<Value, String> {
    let mut engine = engine(scratch)?;
    let folders = skill_folders(scratch)?;
    let at = scratch.to_path_buf();
    let saved = attached(&engine, |mut owner| async move {
        let captured: Result<Value, String> = async {
            let project = seeded(&mut owner, &at).await?;
            for folder in folders {
                let added = owner.controller.add_skill(Scope::Global, folder).await;
                added.map_err(|error| error.0)?;
            }
            match chosen {
                Some(chosen) => save_role_skills(&mut owner, &project, chosen).await,
                None => Ok(Value::Null),
            }
        }
        .await;
        (owner, captured)
    })?;
    engine.stop();
    let db = Connection::open_with_flags(
        engine.path().join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| error.to_string())?;
    let project = ProjectId::new(PROJECT).map_err(|error| error.to_string())?;
    let mut captured = read_launches(scratch, &db, &project)?;
    captured["role_skills_saved"] = saved;
    Ok(captured)
}

/// Reads what each launch receives from the stopped engine's database: the instructions for
/// a linked worktree and an own clone, whether every role gets the same instructions, the
/// team index a master with tools off adds, the master conversation's startup instruction,
/// and each role's skills and launches. Inputs: the scratch folder, the read-only
/// connection and the project. Output: the capture.
fn read_launches(scratch: &Path, db: &Connection, project: &ProjectId) -> Result<Value, String> {
    let master = launch_context::for_team_role(db, project, Layout::Linked, SkillRole::Master)
        .map_err(|error| error.0)?;
    let linked = master.instructions().to_owned();
    let indexed = master
        .with_team_index(db, project)
        .map_err(|error| error.0)?;
    let index = indexed
        .instructions()
        .strip_prefix(linked.as_str())
        .ok_or("the team index is not added after the instructions")?
        .to_owned();
    let clone = launch_context::for_team_role(db, project, Layout::Clone, SkillRole::Worker)
        .map_err(|error| error.0)?;
    let mut same = true;
    let mut roles = Vec::new();
    for role in SkillRole::ALL {
        let context = launch_context::for_team_role(db, project, Layout::Linked, role)
            .map_err(|error| error.0)?;
        same &= context.instructions() == linked;
        roles.push(role_launch(scratch, &context, role)?);
    }
    Ok(json!({
        "instructions": {
            "linked": lines(&linked),
            "clone": lines(clone.instructions()),
            "same_for_every_role": same,
        },
        "master_team_index": lines(&index),
        "master_conversation": master_conversation(db, project)?,
        "roles": roles,
    }))
}

/// The startup instruction of the engine's own project master conversation for each master
/// tools setting: on (the default team plan's) with no team tool switched off, and off.
/// No model access settings are given. Inputs: the read-only connection and the project.
/// Output: both texts as lines.
fn master_conversation(db: &Connection, project: &ProjectId) -> Result<Value, String> {
    let tx = db
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    let none_off: &[String] = &[];
    let tools_off =
        launch_context::for_master(&tx, project, None, None).map_err(|error| error.0)?;
    let tools_on =
        launch_context::for_master(&tx, project, Some(none_off), None).map_err(|error| error.0)?;
    drop(tx);
    Ok(json!({ "tools_off": lines(&tools_off), "tools_on": lines(&tools_on) }))
}

/// Installs one team role's launch context into a fresh private home for each harness and
/// tools switch, and keeps the home, the harness arguments, the denied tools, every file
/// written and the skill folders among them. The seed stores no MCP server, so no launch
/// reads the context-mode install state. Inputs: the scratch folder, the context and the
/// role. Output: the role, its skills and its launches.
fn role_launch(
    scratch: &Path,
    context: &launch_context::AgentContext,
    role: SkillRole,
) -> Result<Value, String> {
    let skills: Vec<&str> = context
        .plugins()
        .skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    let mut launches = Vec::new();
    for harness in HARNESSES {
        for tools in [false, true] {
            let home = launch_home(scratch, &format!("{role:?}-{harness}-{tools}"))?;
            let installed = context
                .install(harness, &home, tools)
                .map_err(|error| error.0)?;
            let arguments: Vec<String> = installed
                .arguments()
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();
            let launch = json!({
                "harness": harness,
                "tools": tools,
                "home": home.to_string_lossy(),
                "arguments": arguments,
                "denied_tools": installed.denied_tools(),
                "files": written(&home, &home, 0)?,
                "skills_written": skills_written(&home, 0)?,
            });
            drop(installed);
            launches.push(launch);
        }
    }
    Ok(json!({ "role": role, "skills": skills, "launches": launches }))
}

/// Writes one SKILL.md folder per name in [`SKILLS`] below the scratch folder, as the owner
/// picks them in Add skill. Input: the scratch folder. Output: the folders as text.
fn skill_folders(scratch: &Path) -> Result<Vec<String>, String> {
    let picked = private_folder(scratch, "picked")?;
    let mut folders = Vec::with_capacity(SKILLS.len());
    for name in SKILLS {
        let folder = private_folder(&picked, name)?;
        let text =
            format!("---\nname: {name}\ndescription: Teaches {name}.\n---\n\nSteps for {name}.\n");
        std::fs::write(folder.join("SKILL.md"), text).map_err(|error| error.to_string())?;
        let folder = folder.to_str().ok_or("a skill folder is not UTF-8")?;
        folders.push(folder.to_owned());
    }
    Ok(folders)
}

/// Makes a fresh 0700 agent home below the scratch folder, by its real path, as a launch
/// requires. Inputs: the scratch folder and the home's name. Output: the home.
fn launch_home(scratch: &Path, name: &str) -> Result<PathBuf, String> {
    let homes = scratch.join("homes");
    if !homes.is_dir() {
        private_folder(scratch, "homes")?;
    }
    let home = private_folder(&homes, name)?;
    home.canonicalize().map_err(|error| error.to_string())
}

/// Lists a folder's entries by path, sorted. Input: the folder. Output: the paths.
fn sorted_entries(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(folder).map_err(|error| error.to_string())?;
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    paths.sort();
    Ok(paths)
}

/// Lists every file a launch wrote below a folder, sorted, with its text when it is UTF-8
/// and at most [`TEXT_BYTES_MAX`] bytes, else a null text; a link is kept without its
/// target, which may name this Mac's tools. Inputs: the home, the folder and its depth
/// below the home. Output: one entry per file, its path relative to the home.
fn written(home: &Path, folder: &Path, depth: usize) -> Result<Vec<Value>, String> {
    if depth > DEPTH_MAX {
        return Err(format!("a launch wrote folders deeper than {DEPTH_MAX}"));
    }
    let mut files = Vec::new();
    for path in sorted_entries(folder)? {
        let relative = path.strip_prefix(home).map_err(|error| error.to_string())?;
        let relative = relative.to_string_lossy().into_owned();
        let kind = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if kind.file_type().is_symlink() {
            files.push(json!({ "path": relative, "link": true }));
        } else if kind.is_dir() {
            files.extend(written(home, &path, depth + 1)?);
        } else {
            let shown = (kind.len() <= TEXT_BYTES_MAX)
                .then(|| std::fs::read_to_string(&path).ok())
                .flatten();
            let text = shown.as_deref().map(lines);
            files.push(json!({ "path": relative, "text": text }));
        }
    }
    Ok(files)
}

/// Names the skill folders a launch wrote: each folder or link below the home holding a
/// SKILL.md, sorted, once per place it was written. Inputs: the folder and its depth below
/// the home. Output: the folder names.
fn skills_written(folder: &Path, depth: usize) -> Result<Vec<String>, String> {
    if depth > DEPTH_MAX {
        return Err(format!("a launch wrote folders deeper than {DEPTH_MAX}"));
    }
    let mut names = Vec::new();
    for path in sorted_entries(folder)? {
        let kind = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        let place = kind.is_dir() || kind.file_type().is_symlink();
        if place && path.join("SKILL.md").is_file() {
            let name = path.file_name().ok_or("a skill folder has no name")?;
            names.push(name.to_string_lossy().into_owned());
        }
        if kind.is_dir() {
            names.extend(skills_written(&path, depth + 1)?);
        }
    }
    names.sort();
    Ok(names)
}

/// Splits a text into its lines for a readable golden. Input: the text. Output: the lines.
fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// The task the sub-agent scenarios hand straight to a sub-agent.
const DELEGATED: &str = "Write the release notes for the tide sensor.";

/// The master's launch when the owner turns the live master on, on a Codex team at the
/// Sandboxed level with tools off: its arguments, variable names, home files and the
/// Seatbelt profile the engine installed, through the P2 launch helper. Input: the scratch
/// folder. Output: the capture.
fn policy_master_live(scratch: &Path) -> Result<Value, String> {
    let team = Team::start(scratch, AgentAccess::Sandboxed, false)?;
    let project = team.project.clone();
    let version = as_owner(&team.engine, |mut owner| async move {
        let status = owner.controller.team_status(project).await;
        let version = status
            .map(|status| status.settings_version)
            .map_err(|error| error.0);
        (owner, version)
    })?;
    team.act(TeamAction::LiveMaster {
        project_id: team.project.clone(),
        on: true,
        version,
    })?;
    Ok(json!({ "launch": team.launched(AgentRole::Master)? }))
}

/// A sub-agent's launch for a task the owner hands it straight, under the default
/// Sub-agent depth; a sub-agent whose own depth is below the setting launches as a lead,
/// else as a worker (team/runtime.rs:10048). Input: the scratch folder.
/// Output: the depth and the capture.
fn policy_sub_agent_default_depth(scratch: &Path) -> Result<Value, String> {
    let team = Team::start(scratch, AgentAccess::Sandboxed, false)?;
    team.act(delegated(&team))?;
    let launch = team.launched(AgentRole::Sub)?;
    Ok(json!({ "sub_agent_depth": DEPTH_DEFAULT, "launch": launch }))
}

/// A sub-agent's launch for a task the owner hands it straight after the owner set
/// Sub-agent depth to 1, where no sub-agent leads. Input: the scratch folder.
/// Output: the depth and the capture.
fn policy_sub_agent_depth_1(scratch: &Path) -> Result<Value, String> {
    let team = Team::start(scratch, AgentAccess::Sandboxed, false)?;
    team.act(TeamAction::SubAgentDepth {
        project_id: team.project.clone(),
        depth: 1,
    })?;
    team.act(delegated(&team))?;
    let launch = team.launched(AgentRole::Sub)?;
    Ok(json!({ "sub_agent_depth": 1, "launch": launch }))
}

/// The owner's Delegate action for [`DELEGATED`] on the team's own harness choice.
/// Input: the team. Output: the action.
fn delegated(team: &Team) -> TeamAction {
    TeamAction::Delegate {
        project_id: team.project.clone(),
        harness: None,
        prompt: DELEGATED.to_owned(),
    }
}
