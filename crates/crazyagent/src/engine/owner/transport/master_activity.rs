use crate::adapters::events::EventKind;
use crate::engine::Engine;
use crate::engine::owner::{OwnerData, OwnerReadReply};
use crate::master_session::{MasterActivityLine, PAGE_BYTES_MAX};
use crate::onboarding::draft::names::secret_like;
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::protocol::{FRAME_BYTES_MAX, RequestId, SessionId};
use crate::team::tool;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The most activity lines one running master turn keeps; the oldest drops first.
pub(crate) const LINES_MAX: usize = 20;
/// The most running master turns whose activity the engine keeps at once.
pub(crate) const TURNS_MAX: usize = 64;
/// The longest project-relative path an activity line names.
pub(crate) const PATH_BYTES_MAX: usize = 160;
/// The longest activity line: the longest phrase with the longest path.
pub(crate) const LINE_BYTES_MAX: usize = 192;

const _: () = assert!(LINE_BYTES_MAX >= "Editing ".len() + PATH_BYTES_MAX);
const _: () =
    assert!(PAGE_BYTES_MAX + LINES_MAX * (2 * LINE_BYTES_MAX + 32) <= FRAME_BYTES_MAX * 3 / 4);

const SUB_AGENT_WORKING: &str = "A sub-agent is working";
const SUB_AGENT_STARTING: &str = "Starting a sub-agent";
const WRITING: &str = "Writing a reply";
const THINKING: &str = "Thinking";
const RUNNING: &str = "Running a command";
const PLANNING: &str = "Updating its plan";
const WORKING: &str = "Working";
const READ_FIELDS: [&str; 3] = ["file_path", "notebook_path", "path"];

/// What a master turn's activity lines may name: paths below the master's worktree, never
/// one in the project's hidden folders (kept relative to the project). `blind` names no path
/// at all, for a hidden folder that does not sit strictly inside the project as given.
pub(crate) struct Screen {
    cwd: PathBuf,
    hidden: Vec<PathBuf>,
    blind: bool,
}

impl Screen {
    /// Builds the screen of one master turn. Inputs: the master's worktree, the project
    /// folder and the project's hidden folders. Output: the screen.
    pub(crate) fn new(cwd: &Path, repository: &Path, hidden: &[PathBuf]) -> Self {
        let relative: Vec<PathBuf> = hidden
            .iter()
            .filter_map(|folder| folder.strip_prefix(repository).ok())
            .map(Path::to_path_buf)
            .collect();
        let blind = relative.len() != hidden.len()
            || relative.iter().any(|folder| folder.as_os_str().is_empty());
        Self {
            cwd: cwd.to_path_buf(),
            hidden: relative,
            blind,
        }
    }

    /// Words one stream event as an activity line: a fixed phrase per event kind, tool or
    /// Codex method, with at most one screened path for a file read or edit. Never the
    /// tool's input, a command, a pattern, a URL, text, output or an unknown tool's name.
    /// Input: the event. Output: the line, or none for bookkeeping frames.
    pub(crate) fn label(&self, kind: &EventKind) -> Option<String> {
        let label = match kind {
            EventKind::ToolUse {
                parent_tool_use_id: Some(_),
                ..
            }
            | EventKind::AssistantText {
                parent_tool_use_id: Some(_),
                ..
            } => SUB_AGENT_WORKING.to_owned(),
            EventKind::ToolUse { name, input, .. } => self.tool(name, input),
            EventKind::AssistantText { .. } | EventKind::AssistantDelta { .. } => {
                WRITING.to_owned()
            }
            EventKind::ApprovalRequired { .. } => "Waiting for approval".to_owned(),
            EventKind::TurnError {
                will_retry: true, ..
            } => "Retrying after an error".to_owned(),
            EventKind::Unsupported {
                message_type,
                subtype: Some(subtype),
            } => frame(message_type, subtype)?.to_owned(),
            _ => return None,
        };
        assert!(!label.is_empty() && label.len() <= LINE_BYTES_MAX);
        Some(label)
    }

    fn tool(&self, name: &str, input: &Value) -> String {
        let named = |verb: &str, plain: &str| match self.path(input) {
            Some(path) => format!("{verb} {path}"),
            None => plain.to_owned(),
        };
        match name {
            "Read" | "NotebookRead" => named("Reading", "Reading a file"),
            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => named("Editing", "Editing a file"),
            "Grep" | "Glob" | "LS" => "Searching the code".to_owned(),
            "Bash" | "BashOutput" | "KillShell" | "KillBash" => RUNNING.to_owned(),
            "WebFetch" | "WebSearch" => "Reading the web".to_owned(),
            "Task" | "Agent" => SUB_AGENT_STARTING.to_owned(),
            "TodoWrite" => PLANNING.to_owned(),
            _ => engine_tool(name).unwrap_or("Using a tool").to_owned(),
        }
    }

    fn path(&self, input: &Value) -> Option<String> {
        if self.blind {
            return None;
        }
        let raw = READ_FIELDS
            .into_iter()
            .find_map(|field| input.get(field)?.as_str())?;
        let path = Path::new(raw);
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.cwd).ok()?
        } else {
            path
        };
        let text = relative.to_str()?;
        if text.is_empty() || text.len() > PATH_BYTES_MAX || text.chars().any(char::is_control) {
            return None;
        }
        let parts: Vec<&str> = text.split('/').collect();
        let refused = |part: &&str| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || secret_like(part)
                || part.eq_ignore_ascii_case(".git")
                || part.eq_ignore_ascii_case(CRAZYAGENT_DIRECTORY)
        };
        if parts.iter().any(refused) || self.hidden.iter().any(|folder| below(&parts, folder)) {
            return None;
        }
        assert!(text.len() <= PATH_BYTES_MAX);
        Some(text.to_owned())
    }
}

fn below(parts: &[&str], folder: &Path) -> bool {
    let folder: Vec<Component<'_>> = folder.components().collect();
    folder.len() <= parts.len()
        && folder
            .iter()
            .zip(parts)
            .all(|(component, part)| match component {
                Component::Normal(name) => name
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(part)),
                _ => false,
            })
}

fn engine_tool(name: &str) -> Option<&'static str> {
    let own = name
        .strip_prefix("mcp__")?
        .strip_prefix(tool::SERVER_NAME)?
        .strip_prefix("__")?;
    Some(match own {
        tool::TOOL_NAME => "Handing a task to the team",
        tool::START_WORK_TOOL => "Starting team work",
        tool::INSPECT_WORK_TOOL => "Checking the team's work",
        tool::UPDATE_WORK_TOOL => "Updating team work",
        tool::TRANSFER_WORK_TOOL => "Moving team work",
        tool::FINISH_WORK_TOOL => "Finishing team work",
        tool::SPAWN_TOOL => SUB_AGENT_STARTING,
        tool::STOP_TOOL => "Stopping a sub-agent",
        tool::MERGE_TOOL => "Merging a sub-agent's work",
        tool::REMEMBER_TOOL => "Saving to project memory",
        tool::RECALL_TOOL => "Recalling project memory",
        tool::SESSIONS_TOOL | tool::SESSION_TOOL => "Reading past sessions",
        tool::ACTIVITY_TOOL => "Logging its progress",
        tool::ASK_TOOL => "Asking you a question",
        tool::RECORD_TOOL => "Recording your answer",
        _ => return None,
    })
}

fn frame(message_type: &str, subtype: &str) -> Option<&'static str> {
    match (message_type, subtype) {
        ("assistant.content", "thinking" | "redacted_thinking") => Some(THINKING),
        ("notification", "item/commandExecution/outputDelta") => Some(RUNNING),
        ("notification", "item/fileChange/outputDelta") => Some("Editing files"),
        ("notification", "turn/plan/updated") => Some(PLANNING),
        ("notification", "native_sub_agent") => Some(SUB_AGENT_STARTING),
        ("notification", method) if method.starts_with("item/reasoning/") => Some(THINKING),
        ("notification", method) if method.starts_with("item/") || method.starts_with("turn/") => {
            Some(WORKING)
        }
        _ => None,
    }
}

/// Milliseconds since the Unix epoch on the engine's clock, for an activity line's time.
/// Input: none. Output: the time, 0 before the epoch.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The running master turns' activity lines by conversation and turn, in the engine's
/// memory only (OB-2): at most `TURNS_MAX` turns of at most `LINES_MAX` lines each, never
/// written to disk, the database or a log.
#[derive(Default)]
pub(crate) struct Board {
    turns: Mutex<HashMap<(SessionId, RequestId), VecDeque<MasterActivityLine>>>,
}

impl Board {
    /// Starts keeping one turn's activity, empty. Inputs: the conversation and the turn.
    /// Output: the watch that records the turn's lines and forgets them when dropped, or
    /// none when `TURNS_MAX` other turns are kept, so the turn runs without lines.
    pub(crate) fn watch(&self, session: &SessionId, turn: &RequestId) -> Option<Watch<'_>> {
        let key = (session.clone(), turn.clone());
        let mut turns = self.turns.lock().ok()?;
        if turns.len() >= TURNS_MAX && !turns.contains_key(&key) {
            return None;
        }
        turns.insert(key.clone(), VecDeque::new());
        assert!(turns.len() <= TURNS_MAX);
        Some(Watch { board: self, key })
    }

    /// The activity lines of one turn, oldest first. Inputs: the conversation and the turn.
    /// Output: the lines, none for a turn not running in this engine.
    pub(crate) fn lines(&self, session: &SessionId, turn: &RequestId) -> Vec<MasterActivityLine> {
        let Ok(turns) = self.turns.lock() else {
            return Vec::new();
        };
        turns
            .get(&(session.clone(), turn.clone()))
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// One running turn's place on the board; dropping it forgets the turn's lines.
pub(crate) struct Watch<'a> {
    board: &'a Board,
    key: (SessionId, RequestId),
}

impl Watch<'_> {
    /// Records one activity line: the same phrase as the last line only moves that line's
    /// time; another is added and the oldest dropped past `LINES_MAX`. Inputs: the line
    /// and when it was seen. Output: none.
    pub(crate) fn record(&self, text: String, at_ms: u64) {
        assert!(!text.is_empty() && text.len() <= LINE_BYTES_MAX);
        let Ok(mut turns) = self.board.turns.lock() else {
            return;
        };
        let Some(lines) = turns.get_mut(&self.key) else {
            return;
        };
        match lines.back_mut() {
            Some(last) if last.text == text => last.at_ms = at_ms,
            _ => {
                lines.push_back(MasterActivityLine { at_ms, text });
                if lines.len() > LINES_MAX {
                    lines.pop_front();
                }
            }
        }
        assert!(lines.len() <= LINES_MAX);
    }
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        if let Ok(mut turns) = self.board.turns.lock() {
            turns.remove(&self.key);
        }
    }
}

impl Engine {
    /// Adds each outstanding master turn's live activity lines to a read of master turns;
    /// any other read passes unchanged. Input: the read. Output: the read.
    pub(crate) fn with_master_activity(&self, read: OwnerReadReply) -> OwnerReadReply {
        with_lines(&self.master_activity, read)
    }
}

fn with_lines(board: &Board, mut read: OwnerReadReply) -> OwnerReadReply {
    if let OwnerData::ProjectMasterTurns { page } = &mut read.data {
        for turn in page.turns.iter_mut() {
            if turn.phase.outstanding() {
                turn.activity = board.lines(&page.session_id, &turn.id);
            }
        }
    }
    read
}

#[cfg(test)]
mod tests;
