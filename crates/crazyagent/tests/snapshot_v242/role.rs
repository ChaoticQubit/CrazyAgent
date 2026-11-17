use super::Scenario;
use super::e2e_support::Owner;
use super::workflow::{attached, engine, recorded, reply, seeded};
use crazyagent::coord::settings::{CoordSettings, RoleSkills};
use crazyagent::coord::tool_switches::BUILTIN_TOOLS;
use crazyagent::launch_context::team_tools_section;
use crazyagent::protocol::ProjectId;
use crazyagent::team::TeamAction;
use serde_json::{Value, json};
use std::future::Future;
use std::path::Path;

/// The owner's own instructions the role scenarios try to save for one team agent.
const AGENT_TEXT: &str = "Answer in short paragraphs.\n";

/// Lists the agent role scenarios: the skills each team role gets, what a team agent
/// received before any launch, and the team tools section of a master's or sub-agent's
/// instructions. Output: the scenarios in the order the snapshot row runs them.
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
