use super::{MessageKind, NewMessage, Party};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TOOL_BYTES_MAX: usize = 64;
pub const COUNTED_MAX: usize = 32;
pub const AGENTS_SHOWN_MAX: usize = 48;
pub const AGENTS_CHECKED_MAX: usize = 64;
pub const NUDGES_PER_TICK_MAX: usize = 16;
pub const TOOLS_PER_AGENT_MAX: usize = 256;
pub const WORK_WITHOUT_GRAPH_MS: i64 = 10 * 60 * 1000;
pub const COMMAND_BYTES_MAX: usize = 16 * 1024;
pub const VIEW_BYTES_MAX: usize =
    AGENTS_SHOWN_MAX * (COUNTED_MAX * 11 + 256) + COUNTED_MAX * (TOOL_BYTES_MAX + 4) + 64;
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit", "apply_patch"];
const SHELL_TOOLS: &[&str] = &[
    "Bash",
    "shell",
    "shell_command",
    "exec_command",
    "local_shell_call",
    "container.exec",
];
const GRAPH_TOOL: &str = "graphify";
const CONTEXT_PREFIX: &str = "ctx_";
const START_WORK: &str = "start_work";
const ACTIVE: &str = "status IN ('starting','idle','working')";
const LABEL_BYTES_MAX: usize = 64;

const _: () = assert!(VIEW_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX / 2);

pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS coord_nudges (
    project TEXT NOT NULL,
    agent INTEGER NOT NULL,
    flag TEXT NOT NULL,
    message INTEGER,
    at INTEGER NOT NULL,
    PRIMARY KEY (agent, flag)
);
CREATE INDEX IF NOT EXISTS coord_nudges_project ON coord_nudges(project);";

/// A workflow habit an agent missed, shown red on the Process page: editing files with no
/// declared work, or working ten minutes with no code-graph query and no context-mode call.
/// Each one earns that agent a single reminder while the project's nudges are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Flag {
    #[serde(rename = "edits_without_start_work")]
    EditsWithoutWork,
    #[serde(rename = "no_graphify_or_ctx")]
    NoGraphOrContext,
}

impl Flag {
    pub const ALL: [Flag; 2] = [Flag::EditsWithoutWork, Flag::NoGraphOrContext];

    /// The flag's stored name, the same as its wire name. Input: the flag. Output: the name.
    pub fn name(self) -> &'static str {
        match self {
            Flag::EditsWithoutWork => "edits_without_start_work",
            Flag::NoGraphOrContext => "no_graphify_or_ctx",
        }
    }

    /// The one reminder the engine sends an agent for this flag, as an Engine note.
    /// Input: the flag. Output: the note's text.
    pub fn reminder(self) -> &'static str {
        match self {
            Flag::EditsWithoutWork => {
                "CrazyAgent reminder: you are editing files without declared work. Call start_work with a title and the files or folders you will change, so other agents see your claim and overlapping edits are caught."
            }
            Flag::NoGraphOrContext => {
                "CrazyAgent reminder: you have worked 10 minutes without a code-graph query or a context-mode call. Query the graph first (graphify query \"<question>\") and run large reads and command output through ctx_execute or ctx_batch_execute."
            }
        }
    }

    /// The flag in the owner's words, as the Process page shows it. Input: the flag.
    /// Output: the words.
    pub fn words(self) -> &'static str {
        match self {
            Flag::EditsWithoutWork => "Edits without start_work",
            Flag::NoGraphOrContext => "10+ min without graphify or ctx",
        }
    }

    fn parse(name: &str) -> Option<Flag> {
        Flag::ALL.into_iter().find(|flag| flag.name() == name)
    }
}

/// One agent's row on the Process page: who it is, whether it still runs, its count for
/// each counted-tool entry in the settings' order, the flags it has now and the flags it
/// was already reminded of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAdoption {
    pub agent: i64,
    pub role: String,
    pub harness: String,
    pub active: bool,
    pub counts: Vec<u32>,
    pub flags: Vec<Flag>,
    pub reminded: Vec<Flag>,
}

/// A project's adoption view: whether nudges are on, the counted-tool entries that name the
/// count columns, and the newest agents, at most `AGENTS_SHOWN_MAX`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adoption {
    pub nudges: bool,
    pub counted: Vec<String>,
    pub agents: Vec<AgentAdoption>,
}

/// Names the tool call an agent made the way adoption counts it: an MCP tool by its own
/// name without the server, a shell command that runs graphify as `graphify`, one that
/// reads a `SKILL.md` as `Skill`, any other call by its name. Inputs: the harness's tool
/// name and its input, the parsed arguments. Output: the name, none when it is not short
/// plain text.
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
    let shown = !key.is_empty()
        && key.len() <= TOOL_BYTES_MAX
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte));
    shown.then(|| key.to_owned())
}

/// The command text of a shell tool call, at most `COMMAND_BYTES_MAX` bytes: Claude's
/// `Bash` and Codex's `shell_command` give it as `command`, `exec_command` as `cmd`, and
/// `shell` and `local_shell_call` as an argument list. Inputs: the tool name and input.
/// Output: the text, none for another tool.
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

/// Sums an agent's calls for each counted-tool entry: an entry ending in `*` counts every
/// tool whose name starts with the rest, any other entry the tool of that exact name.
/// Inputs: the agent's counts by tool and the entries. Output: one count per entry.
pub fn counted(counts: &[(String, u32)], entries: &[String]) -> Vec<u32> {
    let sums: Vec<u32> = entries
        .iter()
        .map(|entry| {
            counts
                .iter()
                .filter(|(tool, _)| match entry.strip_suffix('*') {
                    Some(prefix) => tool.starts_with(prefix),
                    None => tool == entry,
                })
                .fold(0u32, |sum, (_, count)| sum.saturating_add(*count))
        })
        .collect();
    assert_eq!(sums.len(), entries.len());
    sums
}

/// The flags an agent's habits raise: edits with no work it declared or owns, and, once it
/// has worked `WORK_WITHOUT_GRAPH_MS`, no graphify and no `ctx_*` call. Inputs: its counts
/// by tool, whether it owns any work and how long it has worked, none when it does not run
/// or has no tools. Output: the flags in `Flag::ALL` order.
pub fn flags(counts: &[(String, u32)], has_work: bool, worked_ms: Option<i64>) -> Vec<Flag> {
    let calls = |tool: &dyn Fn(&str) -> bool| {
        counts
            .iter()
            .filter(|(name, _)| tool(name))
            .fold(0u32, |sum, (_, count)| sum.saturating_add(*count))
    };
    let edits = calls(&|name| EDIT_TOOLS.contains(&name));
    let started = calls(&|name| name == START_WORK);
    let graph = calls(&|name| name == GRAPH_TOOL || name.starts_with(CONTEXT_PREFIX));
    let mut raised = Vec::new();
    if edits > 0 && started == 0 && !has_work {
        raised.push(Flag::EditsWithoutWork);
    }
    if worked_ms.is_some_and(|worked| worked >= WORK_WITHOUT_GRAPH_MS) && graph == 0 {
        raised.push(Flag::NoGraphOrContext);
    }
    assert!(raised.len() <= Flag::ALL.len());
    raised
}

struct Checked {
    agent: i64,
    role: String,
    harness: String,
    active: bool,
    counts: Vec<(String, u32)>,
    flags: Vec<Flag>,
}

/// Reads the newest `limit` agents of a project with their counts and flags: only running
/// agents with tools when `active_only`, else every agent the project had. Inputs: the
/// store, the project, the limit, whether only running agents count and the time.
/// Output: the agents, newest first.
fn checked(
    db: &Connection,
    project: &ProjectId,
    limit: usize,
    active_only: bool,
    now: i64,
) -> Result<Vec<Checked>, EngineError> {
    let unavailable = |_| EngineError("coord_adoption_unavailable".into());
    let filter = if active_only {
        format!("AND {ACTIVE} AND tools=1")
    } else {
        String::new()
    };
    type Row = (i64, String, String, bool, bool, bool, i64);
    let rows: Vec<Row> = {
        let mut statement = db
            .prepare(&format!(
                "SELECT id,role,harness,{ACTIVE},status='working',tools,created_ms
                 FROM team_agents WHERE project=?1 {filter} ORDER BY id DESC LIMIT ?2"
            ))
            .map_err(unavailable)?;
        statement
            .query_map(params![project.as_str(), limit as i64], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    assert!(rows.len() <= limit);
    let label = |text: String| {
        if text.len() <= LABEL_BYTES_MAX && !text.chars().any(char::is_control) {
            text
        } else {
            String::new()
        }
    };
    let mut agents = Vec::with_capacity(rows.len());
    for (agent, role, harness, active, working, tools, created) in rows {
        let counts: Vec<(String, u32)> = {
            let mut statement = db
                .prepare_cached(
                    "SELECT tool,count FROM coord_tool_use WHERE agent=?1 ORDER BY tool LIMIT ?2",
                )
                .map_err(unavailable)?;
            statement
                .query_map(params![agent, TOOLS_PER_AGENT_MAX as i64], |row| {
                    let count: i64 = row.get(1)?;
                    Ok((row.get(0)?, u32::try_from(count.max(0)).unwrap_or(u32::MAX)))
                })
                .map_err(unavailable)?
                .collect::<Result<_, _>>()
                .map_err(unavailable)?
        };
        let has_work: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM coord_work WHERE owner_agent=?1)",
                [agent],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        let busy = working || counts.iter().any(|(_, count)| *count > 0);
        let worked = (active && tools && busy).then(|| now.saturating_sub(created));
        agents.push(Checked {
            flags: flags(&counts, has_work, worked),
            agent,
            role: label(role),
            harness: label(harness),
            active,
            counts,
        });
    }
    Ok(agents)
}

fn reminded(db: &Connection, agent: i64) -> Result<Vec<Flag>, EngineError> {
    let mut statement = db
        .prepare_cached("SELECT flag FROM coord_nudges WHERE agent=?1 AND message IS NOT NULL")
        .map_err(|_| EngineError("coord_adoption_unavailable".into()))?;
    let names: Vec<String> = statement
        .query_map([agent], |row| row.get(0))
        .map_err(|_| EngineError("coord_adoption_unavailable".into()))?
        .collect::<Result<_, _>>()
        .map_err(|_| EngineError("coord_adoption_unavailable".into()))?;
    Ok(Flag::ALL
        .into_iter()
        .filter(|flag| names.iter().any(|name| Flag::parse(name) == Some(*flag)))
        .collect())
}

/// Reads a project's adoption view for the Process page: the counted-tool entries from its
/// settings and its newest agents with their counts, flags and reminders. Reads only.
/// Inputs: the store, the project and the time. Output: the view, at most
/// `VIEW_BYTES_MAX` once encoded, or a plain code.
pub fn view(db: &Connection, project: &ProjectId, now: i64) -> Result<Adoption, EngineError> {
    let settings = super::settings::read(db, project)?;
    let entries: Vec<String> = settings
        .counted_tools
        .iter()
        .filter(|entry| !entry.is_empty() && entry.len() <= TOOL_BYTES_MAX)
        .take(COUNTED_MAX)
        .cloned()
        .collect();
    let mut agents = Vec::new();
    for agent in checked(db, project, AGENTS_SHOWN_MAX, false, now)? {
        agents.push(AgentAdoption {
            counts: counted(&agent.counts, &entries),
            reminded: reminded(db, agent.agent)?,
            agent: agent.agent,
            role: agent.role,
            harness: agent.harness,
            active: agent.active,
            flags: agent.flags,
        });
    }
    assert!(agents.len() <= AGENTS_SHOWN_MAX && entries.len() <= COUNTED_MAX);
    Ok(Adoption {
        nudges: settings.nudges,
        counted: entries,
        agents,
    })
}

/// Sends each running agent of a project one reminder per flag it raised and was never
/// reminded of, at most `NUDGES_PER_TICK_MAX` a call, and only while the project's nudges
/// are on. A reminder is claimed before it is sent and the claim is dropped when the send
/// fails, so no agent gets the same reminder twice. Inputs: the store, the project and the
/// time. Output: done, or a plain code.
pub fn tick(db: &Connection, project: &ProjectId, now: i64) -> Result<(), EngineError> {
    if !super::settings::read(db, project)?.nudges {
        return Ok(());
    }
    let unavailable = |_| EngineError("coord_adoption_unavailable".into());
    let mut sent = 0;
    for agent in checked(db, project, AGENTS_CHECKED_MAX, true, now)? {
        for flag in agent.flags {
            if sent == NUDGES_PER_TICK_MAX {
                return Ok(());
            }
            let claimed = db
                .execute(
                    "INSERT OR IGNORE INTO coord_nudges(project,agent,flag,message,at)
                     VALUES (?1,?2,?3,NULL,?4)",
                    params![project.as_str(), agent.agent, flag.name(), now],
                )
                .map_err(unavailable)?;
            if claimed == 0 {
                continue;
            }
            let note = NewMessage {
                project: project.clone(),
                from: Party::Engine,
                to: Party::Agent(agent.agent),
                kind: MessageKind::Note,
                question: None,
                work: None,
                reply_to: None,
                body: flag.reminder().to_owned(),
                idempotency_key: Some(format!("adoption-{}-{}", flag.name(), agent.agent)),
            };
            match super::send(db, note, now) {
                Ok(message) => {
                    let stored = db
                        .execute(
                            "UPDATE coord_nudges SET message=?3 WHERE agent=?1 AND flag=?2",
                            params![agent.agent, flag.name(), message],
                        )
                        .map_err(unavailable)?;
                    assert_eq!(stored, 1);
                    sent += 1;
                }
                Err(problem) => {
                    db.execute(
                        "DELETE FROM coord_nudges WHERE agent=?1 AND flag=?2 AND message IS NULL",
                        params![agent.agent, flag.name()],
                    )
                    .map_err(unavailable)?;
                    return Err(problem);
                }
            }
        }
    }
    assert!(sent <= NUDGES_PER_TICK_MAX);
    Ok(())
}
