use super::settings::{COUNTED_TOOL_BYTES_MAX, COUNTED_TOOLS_MAX};
use super::store::{AgentRow, agent, state};
use super::{AgentId, MessageKind, NewMessage, Party, TOOL_BYTES_MAX};
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
/// The longest shell command text read for graphify or a skill read, in bytes.
pub const COMMAND_BYTES_MAX: usize = 16 * 1024;
/// What the clients show in place of receipts and adoption counts for an agent whose
/// launch fires no hooks (DEC-104 bare-hooks), instead of zeros or a blank.
pub const HOOKLESS_WORDS: &str = "unavailable for this account kind";
/// The most bytes one adoption view takes once encoded.
pub const VIEW_BYTES_MAX: usize = AGENTS_SHOWN_MAX * (COUNTED_TOOLS_MAX * 11 + 256)
    + COUNTED_TOOLS_MAX * (COUNTED_TOOL_BYTES_MAX + 4)
    + 64;
const TOOLS_PER_AGENT_MAX: usize = 256;
const LABEL_BYTES_MAX: usize = 64;
const EDIT_TOOLS: [&str; 5] = ["Edit", "Write", "MultiEdit", "NotebookEdit", "apply_patch"];
const SHELL_TOOLS: [&str; 6] = [
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
const _: () = assert!(COUNTED_TOOL_BYTES_MAX <= TOOL_BYTES_MAX);
const _: () = assert!(NUDGES_PER_TICK_MAX <= AGENTS_CHECKED_MAX);

/// A workflow habit an agent missed (Q18), shown red on the Process page and the graph:
/// editing files with no declared work, or working ten minutes with no code-graph query
/// and no context-mode call. Each earns that agent one reminder while nudges are on.
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
/// each counted-tools entry in the settings' order, the flags it has now, the flags it was
/// already reminded of and whether its launch fires no hooks, so no counts or flags exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAdoption {
    pub agent: AgentId,
    pub role: String,
    pub harness: String,
    pub active: bool,
    pub counts: Vec<u32>,
    pub flags: Vec<Flag>,
    pub reminded: Vec<Flag>,
    #[serde(default)]
    pub hookless: bool,
}

/// A project's adoption view: whether nudges are on, the counted-tools entries that name
/// the count columns, and its newest agents, at most `AGENTS_SHOWN_MAX`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adoption {
    pub nudges: bool,
    pub counted: Vec<String>,
    pub agents: Vec<AgentAdoption>,
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

fn runs_graphify(command: &str) -> bool {
    command
        .split(|c: char| c.is_whitespace() || ";&|()'\"`".contains(c))
        .any(|word| word.rsplit('/').next() == Some(GRAPH_TOOL))
}

/// The flags one agent's habits raise now (Q18). Edits by an agent that never started or
/// owned work raise the first. A running agent with tools that worked `UNGROUNDED_MS`
/// since its start, busy or calling tools, with no graphify and no `ctx_*` call, raises
/// the second. An agent whose launch fires no hooks raises none, since none of its calls are
/// counted (DEC-104 bare-hooks). Takes the database, the agent, its row, its counts by stored
/// tool name and the time in ms. Returns the flags in `Flag::ALL` order.
pub(super) fn flags(
    db: &Connection,
    id: AgentId,
    row: &AgentRow,
    used: &[(String, i64)],
    now: i64,
) -> Result<Vec<Flag>, EngineError> {
    let (tools, declared, hookless): (bool, bool, bool) = db
        .query_row(
            "SELECT a.tools,
             EXISTS(SELECT 1 FROM coord_audit WHERE by_kind='agent' AND by_agent=a.id
                    AND action='start')
             OR EXISTS(SELECT 1 FROM coord_work WHERE owner_agent=a.id),
             a.hookless
             FROM team_agents a WHERE a.id=?1",
            [id],
            |found| Ok((found.get(0)?, found.get(1)?, found.get(2)?)),
        )
        .map_err(state)?;
    if hookless {
        return Ok(Vec::new());
    }
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
/// its settings and its newest agents with their counts, flags and reminders. Reads
/// only. Takes the database, the project and the time in ms. Returns the view, at most
/// `VIEW_BYTES_MAX` once encoded.
pub fn view(db: &Connection, project: &ProjectId, now: i64) -> Result<Adoption, EngineError> {
    let settings = super::settings::read(db, project)?;
    assert!(settings.counted_tools.len() <= COUNTED_TOOLS_MAX);
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
        agents.push(AgentAdoption {
            agent: id,
            role: label(&row.role),
            harness: label(&harness),
            active: row.live(),
            counts: counted(&used, &settings.counted_tools),
            flags: flags(db, id, &row, &used, now)?,
            reminded: reminded(db, project, id)?,
            hookless,
        });
    }
    assert!(agents.len() <= AGENTS_SHOWN_MAX);
    Ok(Adoption {
        nudges: settings.nudges,
        counted: settings.counted_tools,
        agents,
    })
}

/// Sends each running agent of a project one Engine note per flag it raised and was never
/// reminded of, at most `NUDGES_PER_TICK_MAX` a call, only while the project's nudges are
/// on. The note's idempotency key is the flag and the agent, so a reminder is sent once
/// whatever the retries. Takes the database, the project and the time in ms. Returns Ok
/// or the first refusal.
pub fn tick(db: &Connection, project: &ProjectId, now: i64) -> Result<(), EngineError> {
    if !super::settings::read(db, project)?.nudges {
        return Ok(());
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
