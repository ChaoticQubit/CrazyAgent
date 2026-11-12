use super::settings::{COUNTED_TOOL_BYTES_MAX, COUNTED_TOOLS_MAX, Nudges};
use super::store::{AgentRow, agent, state};
use super::{AgentId, MessageId, MessageKind, NewMessage, Party, TOOL_BYTES_MAX};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most agents the Process page lists, newest first.
pub const AGENTS_SHOWN_MAX: usize = 48;
/// The most running agents one adoption check reads.
pub const AGENTS_CHECKED_MAX: usize = 64;
/// The most reminders one adoption check sends.
pub const NUDGES_PER_TICK_MAX: usize = 16;
/// How long an agent works with no graphify and no context-mode call before it is
/// flagged (Q18), in ms.
pub const UNGROUNDED_MS: i64 = 10 * 60_000;
/// What the FirstEdit nudges tell an agent whose first file edit came before any
/// code-graph query (DEC-111 R4): the hook's answer where hooks run, the engine's note for
/// an agent without them.
pub const FIRST_EDIT_REMINDER: &str = "You are editing before any code-graph query. Run graphify query first if this touches code you have not mapped.";
/// The longest shell command text read for graphify or a skill read, in bytes.
pub const COMMAND_BYTES_MAX: usize = 16 * 1024;
/// What the clients show in place of receipts for an agent whose launch fires no hooks
/// (DEC-104 bare-hooks), instead of zeros or a blank.
pub const HOOKLESS_WORDS: &str = "unavailable for this account kind";
/// The most bytes one adoption view takes once encoded. Newer agents are kept first; the
/// older ones past it are counted in `Adoption::hidden`.
pub const VIEW_BYTES_MAX: usize = crate::protocol::FRAME_BYTES_MAX / 2;
/// The most skill columns one view names: every skill a project's agents can receive,
/// global and the project's own.
pub const SKILLS_SHOWN_MAX: usize = 2 * crate::plugins::SKILLS_PER_SCOPE_MAX;
/// The most skills one agent called that its project has not enabled, listed by name.
pub const OTHER_SKILLS_MAX: usize = 16;
/// The stored-name prefix of one skill's calls, kept beside the harness's own `Skill` count
/// so each skill counts by name (F7).
pub const SKILL_PREFIX: &str = "skill:";
/// The longest skill name a stored skill call keeps, in bytes.
pub const SKILL_NAME_BYTES_MAX: usize = TOOL_BYTES_MAX - SKILL_PREFIX.len();
const HEADER_BYTES_MAX: usize = 64
    + COUNTED_TOOLS_MAX * (COUNTED_TOOL_BYTES_MAX + 4)
    + SKILLS_SHOWN_MAX * (crate::plugins::skill_folder::NAME_BYTES_MAX + 4);
const AGENT_BYTES_MAX: usize = 512
    + COUNTED_TOOLS_MAX * 11
    + SKILLS_SHOWN_MAX * 11
    + OTHER_SKILLS_MAX * (SKILL_NAME_BYTES_MAX + 32);
const TOOLS_PER_AGENT_MAX: usize = 256;
const LABEL_BYTES_MAX: usize = 64;
const EDIT_TOOLS: [&str; 5] = ["Edit", "Write", "MultiEdit", "NotebookEdit", "apply_patch"];
pub(crate) const SHELL_TOOLS: [&str; 6] = [
    "Bash",
    "shell",
    "shell_command",
    "exec_command",
    "local_shell_call",
    "container.exec",
];
const GRAPH_TOOL: &str = "graphify";
const CONTEXT_PREFIX: &str = "ctx_";
const NUDGE_KEY: &str = "adoption-";

const _: () = assert!(VIEW_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX / 2);
const _: () = assert!(HEADER_BYTES_MAX + AGENT_BYTES_MAX <= VIEW_BYTES_MAX);
const _: () = assert!(crate::plugins::skill_folder::NAME_BYTES_MAX <= SKILL_NAME_BYTES_MAX);
const _: () = assert!(COUNTED_TOOL_BYTES_MAX <= TOOL_BYTES_MAX);
const _: () = assert!(NUDGES_PER_TICK_MAX <= AGENTS_CHECKED_MAX);

/// A workflow habit an agent missed (Q18), shown red on the Process page and the graph:
/// editing files with no declared work, or working ten minutes with no code-graph query
/// and no context-mode call. Each earns that agent one reminder under the TenMinutes nudges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Flag {
    #[serde(rename = "edits_without_start_work")]
    EditsWithoutWork,
    #[serde(rename = "no_graphify_or_context_mode")]
    Ungrounded,
}

impl Flag {
    pub const ALL: [Flag; 2] = [Flag::EditsWithoutWork, Flag::Ungrounded];

    /// The flag's stored name, the same as its wire name. Takes the flag. Returns the name.
    pub fn name(self) -> &'static str {
        match self {
            Flag::EditsWithoutWork => "edits_without_start_work",
            Flag::Ungrounded => "no_graphify_or_context_mode",
        }
    }

    /// The flag in the owner's words, as the Process page shows it. Takes the flag.
    /// Returns the words.
    pub fn words(self) -> &'static str {
        match self {
            Flag::EditsWithoutWork => "Edits without start_work",
            Flag::Ungrounded => "10+ min without graphify or context-mode",
        }
    }

    /// The one reminder the engine sends an agent for this flag, as an Engine note.
    /// Takes the flag. Returns the note's text.
    pub fn reminder(self) -> &'static str {
        match self {
            Flag::EditsWithoutWork => {
                "CrazyAgent reminder: you are editing files without declared work. Call start_work with a title and the files or folders you will change, so other agents see your claim and overlapping edits are caught."
            }
            Flag::Ungrounded => {
                "CrazyAgent reminder: you have worked 10 minutes without a code-graph query or a context-mode call. Query the graph first (graphify query \"<question>\") and run large reads and command output through ctx_execute or ctx_batch_execute."
            }
        }
    }

    fn key(self, agent: AgentId) -> String {
        format!("{NUDGE_KEY}{}-{agent}", self.name())
    }
}

/// One agent's row on the Process page: who it is, whether it still runs, its count for
/// each counted-tools entry in the settings' order, its calls of each enabled skill in the
/// view's skill order (zero included), the skills it called that the project has not
/// enabled, its graphify and `ctx_*` calls, the flags it has now, the flags it was already
/// reminded of and whether its launch fires no hooks, whose calls the usage scan counts
/// from its transcript instead (OQ-8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAdoption {
    pub agent: AgentId,
    pub role: String,
    pub harness: String,
    pub active: bool,
    pub counts: Vec<u32>,
    #[serde(default)]
    pub skills: Vec<u32>,
    #[serde(default)]
    pub other_skills: Vec<SkillCount>,
    #[serde(default)]
    pub graphify: u32,
    #[serde(default)]
    pub context_mode: u32,
    pub flags: Vec<Flag>,
    pub reminded: Vec<Flag>,
    #[serde(default)]
    pub hookless: bool,
}

/// One skill an agent called by name, with how many times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCount {
    pub name: String,
    pub count: u32,
}

/// A project's adoption view: its nudges option, the counted-tools entries that name
/// the count columns, the skills its agents receive (sorted by name), its newest agents, at
/// most `AGENTS_SHOWN_MAX`, and how many older agents the view's byte limit left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adoption {
    pub nudges: Nudges,
    pub counted: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    pub agents: Vec<AgentAdoption>,
    #[serde(default)]
    pub hidden: u32,
}

/// Names a tool call the way adoption counts it: an MCP tool by its own name without the
/// server, a shell command that runs graphify as `graphify`, one that reads a `SKILL.md`
/// as `Skill`, any other call by its name. Takes the harness's tool name and its input
/// (the parsed arguments). Returns the name for `record_tool`, none when it is not 1 to
/// `TOOL_BYTES_MAX` bytes of letters, digits, `_`, `-`, `.` or `:`.
pub fn tool_key(name: &str, input: &Value) -> Option<String> {
    let key = match name.strip_prefix("mcp__").unwrap_or(name).rsplit_once("__") {
        Some((server, _)) if server.contains(GRAPH_TOOL) => GRAPH_TOOL,
        Some((_, tool)) => tool,
        None => match shell_command(name, input) {
            Some(command) if runs_graphify(&command) => GRAPH_TOOL,
            Some(command) if command.contains("SKILL.md") => "Skill",
            _ => name,
        },
    };
    let plain = !key.is_empty()
        && key.len() <= TOOL_BYTES_MAX
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte));
    plain.then(|| key.to_owned())
}

/// The command text of a shell tool call, at most `COMMAND_BYTES_MAX` bytes: Claude's
/// `Bash` and Codex's `shell_command` give it as `command`, `exec_command` as `cmd`, and
/// `shell` and `local_shell_call` as an argument list. Takes the tool name and input.
/// Returns the text, none for another tool.
fn shell_command(name: &str, input: &Value) -> Option<String> {
    if !SHELL_TOOLS.contains(&name) {
        return None;
    }
    let given = match &input["command"] {
        Value::Null => &input["cmd"],
        given => given,
    };
    let mut text = match given {
        Value::String(text) => text.clone(),
        Value::Array(words) => words
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    if text.len() > COMMAND_BYTES_MAX {
        let mut end = COMMAND_BYTES_MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    assert!(text.len() <= COMMAND_BYTES_MAX);
    Some(text)
}

/// Names the skill one tool call used, the way adoption counts it per skill: Claude's
/// `Skill` call by the skill it names, a shell command that reads `<folder>/SKILL.md` by
/// the folder's name. Takes the harness's tool name and its input. Returns `skill:` and the
/// name, none for another call or a name that is not 1 to `SKILL_NAME_BYTES_MAX` bytes of
/// letters, digits, `_`, `-`, `.` or `:`.
pub fn skill_key(name: &str, input: &Value) -> Option<String> {
    let skill = if name == "Skill" {
        input["skill"].as_str()?.to_owned()
    } else {
        let command = shell_command(name, input)?;
        command
            .split(|c: char| c.is_whitespace() || ";&|()'\"`".contains(c))
            .find_map(|word| word.strip_suffix("/SKILL.md"))?
            .rsplit('/')
            .next()?
            .to_owned()
    };
    let plain = !skill.is_empty()
        && skill.len() <= SKILL_NAME_BYTES_MAX
        && skill
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte));
    let key = plain.then(|| format!("{SKILL_PREFIX}{skill}"))?;
    assert!(key.len() <= TOOL_BYTES_MAX);
    Some(key)
}

fn runs_graphify(command: &str) -> bool {
    command
        .split(|c: char| c.is_whitespace() || ";&|()'\"`".contains(c))
        .any(|word| word.rsplit('/').next() == Some(GRAPH_TOOL))
}

/// The flags one agent's habits raise now (Q18). Edits by an agent that never started or
/// owned work raise the first. A running agent with tools that worked `UNGROUNDED_MS`
/// since its start, busy or calling tools, with no graphify and no `ctx_*` call, raises
/// the second, whether or not its launch fires hooks (OQ-8). Takes the database, the agent,
/// its row, its counts by stored tool name and the time in ms. Returns the flags in
/// `Flag::ALL` order.
pub(super) fn flags(
    db: &Connection,
    id: AgentId,
    row: &AgentRow,
    used: &[(String, i64)],
    now: i64,
) -> Result<Vec<Flag>, EngineError> {
    let (tools, declared): (bool, bool) = db
        .query_row(
            "SELECT a.tools,
             EXISTS(SELECT 1 FROM coord_audit WHERE by_kind='agent' AND by_agent=a.id
                    AND action='start')
             OR EXISTS(SELECT 1 FROM coord_work WHERE owner_agent=a.id)
             FROM team_agents a WHERE a.id=?1",
            [id],
            |found| Ok((found.get(0)?, found.get(1)?)),
        )
        .map_err(state)?;
    let called = |tool: &dyn Fn(&str) -> bool| {
        used.iter()
            .any(|(name, count)| *count > 0 && tool(super::presence::short(name)))
    };
    let edited = called(&|name| EDIT_TOOLS.contains(&name));
    let grounded = called(&|name| name == GRAPH_TOOL || name.starts_with(CONTEXT_PREFIX));
    let busy = row.status == "working" || called(&|_| true);
    let worked = now.saturating_sub(row.created) >= UNGROUNDED_MS;
    let mut raised = Vec::new();
    if edited && !declared {
        raised.push(Flag::EditsWithoutWork);
    }
    if row.live() && tools && busy && worked && !grounded {
        raised.push(Flag::Ungrounded);
    }
    assert!(raised.len() <= Flag::ALL.len());
    Ok(raised)
}

/// An agent's counts by stored tool name, at most `TOOLS_PER_AGENT_MAX` names. Takes the
/// database and the agent. Returns the names with their counts, by name.
pub(super) fn used(db: &Connection, id: AgentId) -> Result<Vec<(String, i64)>, EngineError> {
    let mut statement = db
        .prepare("SELECT tool,count FROM coord_tool_use WHERE agent=?1 ORDER BY tool LIMIT ?2")
        .map_err(state)?;
    let limit = i64::try_from(TOOLS_PER_AGENT_MAX).map_err(state)?;
    let rows = statement
        .query_map(params![id, limit], |found| {
            Ok((found.get(0)?, found.get(1)?))
        })
        .map_err(state)?;
    let used: Vec<(String, i64)> = rows.collect::<Result<_, _>>().map_err(state)?;
    assert!(used.len() <= TOOLS_PER_AGENT_MAX);
    Ok(used)
}

/// Sums an agent's calls for each counted-tools entry, matched as the snapshot matches
/// them. Takes its counts by stored tool name and the entries. Returns one count per entry.
fn counted(used: &[(String, i64)], entries: &[String]) -> Vec<u32> {
    let sums: Vec<u32> = entries
        .iter()
        .map(|entry| {
            let total = used
                .iter()
                .filter(|(tool, _)| super::presence::counts(entry, tool))
                .map(|(_, count)| (*count).max(0))
                .fold(0i64, i64::saturating_add);
            u32::try_from(total).unwrap_or(u32::MAX)
        })
        .collect();
    assert_eq!(sums.len(), entries.len());
    sums
}

/// Sums an agent's calls of each enabled skill, a call matching a skill by its whole name or
/// by the part after its last `:` (a plugin's namespaced skill), and lists the other skills
/// it called, at most `OTHER_SKILLS_MAX`, by name. Takes its counts by stored tool name and
/// the enabled skills. Returns one count per enabled skill and the others.
fn skill_counts(used: &[(String, i64)], skills: &[String]) -> (Vec<u32>, Vec<SkillCount>) {
    let calls: Vec<(&str, i64)> = used
        .iter()
        .filter_map(|(tool, count)| Some((tool.strip_prefix(SKILL_PREFIX)?, (*count).max(0))))
        .collect();
    let names = |call: &str, skill: &str| call == skill || call.rsplit(':').next() == Some(skill);
    let sum = |pick: &dyn Fn(&str) -> bool| {
        let total = calls
            .iter()
            .filter(|(call, _)| pick(call))
            .map(|(_, count)| *count)
            .fold(0i64, i64::saturating_add);
        u32::try_from(total).unwrap_or(u32::MAX)
    };
    let enabled: Vec<u32> = skills
        .iter()
        .map(|skill| sum(&|call| names(call, skill)))
        .collect();
    let others: Vec<SkillCount> = calls
        .iter()
        .filter(|(call, count)| *count > 0 && !skills.iter().any(|skill| names(call, skill)))
        .take(OTHER_SKILLS_MAX)
        .map(|(call, count)| SkillCount {
            name: (*call).to_owned(),
            count: u32::try_from(*count).unwrap_or(u32::MAX),
        })
        .collect();
    assert_eq!(enabled.len(), skills.len());
    assert!(others.len() <= OTHER_SKILLS_MAX);
    (enabled, others)
}

fn reminded(db: &Connection, project: &ProjectId, id: AgentId) -> Result<Vec<Flag>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT idem_key FROM coord_messages WHERE project=?1 AND from_kind='engine'
             AND to_kind='agent' AND to_agent=?2 AND substr(idem_key,1,length(?3))=?3",
        )
        .map_err(state)?;
    let rows = statement
        .query_map(params![project.as_str(), id, NUDGE_KEY], |found| {
            found.get::<_, String>(0)
        })
        .map_err(state)?;
    let keys: Vec<String> = rows.collect::<Result<_, _>>().map_err(state)?;
    Ok(Flag::ALL
        .into_iter()
        .filter(|flag| keys.contains(&flag.key(id)))
        .collect())
}

/// Reads a project's adoption view for the Process page: the counted-tools entries from
/// its settings, the skills its agents receive and its newest agents with their counts,
/// per-skill counts, graphify and `ctx_*` counts, flags and reminders. Reads only. Takes
/// the database, the project, the names of the skills its agents receive and the time in
/// ms. Returns the view, at most `VIEW_BYTES_MAX` once encoded: older agents past it are
/// left out and counted.
pub fn view(
    db: &Connection,
    project: &ProjectId,
    skills: &[String],
    now: i64,
) -> Result<Adoption, EngineError> {
    let settings = super::settings::read(db, project)?;
    assert!(settings.counted_tools.len() <= COUNTED_TOOLS_MAX);
    let mut skills: Vec<String> = skills
        .iter()
        .filter(|skill| {
            !skill.is_empty()
                && skill.len() <= crate::plugins::skill_folder::NAME_BYTES_MAX
                && skill
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte))
        })
        .take(SKILLS_SHOWN_MAX)
        .cloned()
        .collect();
    skills.sort();
    skills.dedup();
    let grounding = [GRAPH_TOOL.to_owned(), format!("{CONTEXT_PREFIX}*")];
    let ids: Vec<(AgentId, String, bool)> = {
        let mut statement = db
            .prepare(
                "SELECT id,harness,hookless FROM team_agents WHERE project=?1
                 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(state)?;
        let limit = i64::try_from(AGENTS_SHOWN_MAX).map_err(state)?;
        let rows = statement
            .query_map(params![project.as_str(), limit], |found| {
                Ok((found.get(0)?, found.get(1)?, found.get(2)?))
            })
            .map_err(state)?;
        rows.collect::<Result<_, _>>().map_err(state)?
    };
    let label = |text: &str| {
        let plain = text.len() <= LABEL_BYTES_MAX && !text.chars().any(char::is_control);
        if plain {
            text.to_owned()
        } else {
            String::new()
        }
    };
    let mut agents = Vec::with_capacity(ids.len());
    for (id, harness, hookless) in ids {
        let Some(row) = agent(db, id)? else {
            continue;
        };
        let used = used(db, id)?;
        let (skill_calls, other_skills) = skill_counts(&used, &skills);
        let [graphify, context_mode] = counted(&used, &grounding)[..] else {
            unreachable!("one count per grounding entry");
        };
        agents.push(AgentAdoption {
            agent: id,
            role: label(&row.role),
            harness: label(&harness),
            active: row.live(),
            counts: counted(&used, &settings.counted_tools),
            skills: skill_calls,
            other_skills,
            graphify,
            context_mode,
            flags: flags(db, id, &row, &used, now)?,
            reminded: reminded(db, project, id)?,
            hookless,
        });
    }
    assert!(agents.len() <= AGENTS_SHOWN_MAX);
    let mut view = Adoption {
        nudges: settings.nudges,
        counted: settings.counted_tools,
        skills,
        agents: Vec::new(),
        hidden: 0,
    };
    let mut bytes = serde_json::to_vec(&view).map_err(state)?.len();
    assert!(bytes <= HEADER_BYTES_MAX);
    for agent in agents {
        let size = serde_json::to_vec(&agent).map_err(state)?.len() + 1;
        if view.hidden == 0 && bytes + size <= VIEW_BYTES_MAX {
            bytes += size;
            view.agents.push(agent);
        } else {
            view.hidden = view.hidden.saturating_add(1);
        }
    }
    assert!(bytes <= VIEW_BYTES_MAX);
    Ok(view)
}

/// Sends each running agent of a project one Engine note per flag it raised and was never
/// reminded of, at most `NUDGES_PER_TICK_MAX` a call, under the project's TenMinutes
/// nudges; under FirstEdit it sends `first_edits` instead, and under Off nothing. The
/// note's idempotency key is the flag and the agent, so a reminder is sent once whatever
/// the retries. Takes the database, the project and the time in ms. Returns Ok or the
/// first refusal.
pub fn tick(db: &Connection, project: &ProjectId, now: i64) -> Result<(), EngineError> {
    match super::settings::read(db, project)?.nudges {
        Nudges::Off => return Ok(()),
        Nudges::FirstEdit => return first_edits(db, project, now),
        Nudges::TenMinutes => {}
    }
    let ids: Vec<AgentId> = {
        let mut statement = db
            .prepare(
                "SELECT id FROM team_agents WHERE project=?1 AND tools=1
                 AND status NOT IN ('stopped','failed') ORDER BY id DESC LIMIT ?2",
            )
            .map_err(state)?;
        let limit = i64::try_from(AGENTS_CHECKED_MAX).map_err(state)?;
        let rows = statement
            .query_map(params![project.as_str(), limit], |found| found.get(0))
            .map_err(state)?;
        rows.collect::<Result<_, _>>().map_err(state)?
    };
    let mut sent = 0;
    for id in ids {
        let Some(row) = agent(db, id)? else {
            continue;
        };
        let raised = flags(db, id, &row, &used(db, id)?, now)?;
        if raised.is_empty() {
            continue;
        }
        let earlier = reminded(db, project, id)?;
        for flag in raised.into_iter().filter(|flag| !earlier.contains(flag)) {
            if sent == NUDGES_PER_TICK_MAX {
                return Ok(());
            }
            super::send(
                db,
                NewMessage {
                    project: project.clone(),
                    from: Party::Engine,
                    to: Party::Agent(id),
                    kind: MessageKind::Note,
                    question: None,
                    work: None,
                    reply_to: None,
                    body: flag.reminder().to_owned(),
                    idempotency_key: Some(flag.key(id)),
                },
                now,
            )?;
            sent += 1;
        }
    }
    assert!(sent <= NUDGES_PER_TICK_MAX);
    Ok(())
}

/// The FirstEdit fallback for running agents without the hook path, a launch that fires no
/// hooks or a harness other than Claude Code: each one whose counted calls hold a file edit
/// and no code-graph query gets one Engine note with `FIRST_EDIT_REMINDER`, keyed by the
/// agent so it is sent once, at most `NUDGES_PER_TICK_MAX` a call. Takes the database, the
/// project and the time in ms. Returns Ok or the first refusal.
fn first_edits(db: &Connection, project: &ProjectId, now: i64) -> Result<(), EngineError> {
    let ids: Vec<AgentId> = {
        let mut statement = db
            .prepare(
                "SELECT id FROM team_agents WHERE project=?1 AND tools=1
                 AND status NOT IN ('stopped','failed')
                 AND (hookless=1 OR harness NOT IN ('claude-code','claude'))
                 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(state)?;
        let limit = i64::try_from(AGENTS_CHECKED_MAX).map_err(state)?;
        let rows = statement
            .query_map(params![project.as_str(), limit], |found| found.get(0))
            .map_err(state)?;
        rows.collect::<Result<_, _>>().map_err(state)?
    };
    let mut sent = 0;
    for id in ids {
        let used = used(db, id)?;
        let called = |tool: &dyn Fn(&str) -> bool| {
            used.iter()
                .any(|(name, count)| *count > 0 && tool(super::presence::short(name)))
        };
        if !called(&|name| EDIT_TOOLS.contains(&name)) || called(&|name| name == GRAPH_TOOL) {
            continue;
        }
        if first_edit_reminded(db, project, id)? {
            continue;
        }
        if sent == NUDGES_PER_TICK_MAX {
            break;
        }
        send_first_edit(db, project, id, now)?;
        sent += 1;
    }
    assert!(sent <= NUDGES_PER_TICK_MAX);
    Ok(())
}

/// Decides the FirstEdit reminder a native teammate's edit hands its session through the
/// relay (relay over the caller socket, OQ-7 B): under the FirstEdit nudges, for an agent
/// whose counted calls hold no code-graph query, once, recorded as the same Engine note
/// `first_edits` keys by the agent. Takes the database, the project, the agent and the time
/// in ms. Returns the note's id, to mark Delivered once its session got it, or None.
pub(super) fn first_edit_note(
    db: &Connection,
    project: &ProjectId,
    id: AgentId,
    now: i64,
) -> Result<Option<MessageId>, EngineError> {
    if super::settings::read(db, project)?.nudges != Nudges::FirstEdit {
        return Ok(None);
    }
    let graphed = used(db, id)?.iter().any(|(name, count)| {
        *count > 0 && tool_key(name, &Value::Null).as_deref() == Some(GRAPH_TOOL)
    });
    if graphed || first_edit_reminded(db, project, id)? {
        return Ok(None);
    }
    send_first_edit(db, project, id, now).map(Some)
}

/// Whether an agent got its FirstEdit reminder, by the Engine note keyed by the agent.
/// Takes the database, the project and the agent. Returns the answer.
fn first_edit_reminded(
    db: &Connection,
    project: &ProjectId,
    id: AgentId,
) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM coord_messages WHERE project=?1
         AND from_kind='engine' AND to_kind='agent' AND to_agent=?2 AND idem_key=?3)",
        params![project.as_str(), id, first_edit_key(id)],
        |found| found.get(0),
    )
    .map_err(state)
}

/// Sends an agent the Engine note with `FIRST_EDIT_REMINDER`, keyed by the agent so it is
/// sent once. Takes the database, the project, the agent and the time in ms. Returns the
/// note's id or the refusal.
fn send_first_edit(
    db: &Connection,
    project: &ProjectId,
    id: AgentId,
    now: i64,
) -> Result<MessageId, EngineError> {
    super::send(
        db,
        NewMessage {
            project: project.clone(),
            from: Party::Engine,
            to: Party::Agent(id),
            kind: MessageKind::Note,
            question: None,
            work: None,
            reply_to: None,
            body: FIRST_EDIT_REMINDER.to_owned(),
            idempotency_key: Some(first_edit_key(id)),
        },
        now,
    )
}

/// The idempotency key of an agent's FirstEdit reminder. Takes the agent. Returns the key.
fn first_edit_key(id: AgentId) -> String {
    format!("{NUDGE_KEY}first_edit-{id}")
}

#[cfg(all(test, unix))]
#[path = "adoption/tests.rs"]
mod tests;
