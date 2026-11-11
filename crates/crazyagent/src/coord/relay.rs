use super::hook::Event;
use super::settings::{self, Delivery, HarnessKind, Nudges, Overlaps, TaskStart};
use super::store::{failure, message, state};
use super::{
    AgentId, BODY_BYTES_MAX, Message, MessageId, MessageKind, PENDING_MAX, Party, QuestionFor,
    Receipt, Severity, TOOL_BYTES_MAX, Waiting, WorkId,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::team::AgentStatus;
use crate::team::tool::{Mailbox, call_id, call_id_valid};
use crate::usage::capture::Format;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The engine subcommand the relay's hooks run: `coord-hook <event> <mailbox>`.
pub const HOOK_COMMAND: &str = "coord-hook";
/// The engine subcommand the relay's monitor runs: `coord-watch <mailbox>`.
pub const WATCH_COMMAND: &str = "coord-watch";
/// The folder in an agent's private home that holds its relay plugin.
pub const PLUGIN_FOLDER: &str = "crazyagent-relay";
/// The delivery tag of a lead's own session: Claude Code's `tmuxPaneId` for the lead in
/// its team config. A teammate's tag is its tmux pane id, such as `%5`.
pub(crate) const LEAD_TAG: &str = "leader";
/// The folder inside a native lead's mailbox that holds what the lead's own sessions and
/// the engine exchange, kept from its teammates (relay mailbox forgery C1).
pub const LEAD_PART: &str = "lead";
/// The text that opens every message a session reads, followed by the id and `]`, which
/// the read receipt scan finds in the session's transcript.
pub(crate) const MARK: &str = "[crazyagent msg ";
const NOTIFICATION: &str = "<task-notification>";
pub(crate) const ROSTER_FILE: &str = "relay.json";
pub(crate) const INBOX: &str = ".inbox";
pub(crate) const SHOWN: &str = ".shown";
pub(crate) const TOOLS: &str = ".tools";
pub(crate) const TOUCH: &str = ".touch";
pub(crate) const IDLE: &str = ".idle";
pub(crate) const WAITING: &str = ".waiting";
pub(crate) const ASKED: &str = ".asked";
pub(crate) const ASK: &str = ".ask";
pub(crate) const ANSWER: &str = ".answer";
/// The most messages waiting in one mailbox for its sessions to take.
pub(crate) const INBOX_MAX: usize = 32;
pub(crate) const LETTER_BYTES_MAX: u64 = 16 * 1024;
pub(crate) const ROSTER_BYTES_MAX: u64 = 4 * 1024;
pub(crate) const ASK_BYTES_MAX: u64 = 4 * 1024;
/// The most files one overlap question names: the files of one Codex patch, asked together
/// so one answer and one wait cover the whole patch (PA1213-F2).
pub(crate) const ASK_PATHS_MAX: usize = 32;
pub(crate) const ANSWER_BYTES_MAX: u64 = 64 * 1024;
/// The longest tool log one session keeps before the engine takes it; past it, tool calls
/// are not counted until the next tick.
pub(crate) const TOOLS_LOG_BYTES_MAX: u64 = 32 * 1024;
pub(crate) const WAITING_BYTES_MAX: u64 = 16;
pub(crate) const PREVIEW_BYTES: usize = 200;
/// The bytes of a letter's quoted body a session reads. Quoting grows a body of line breaks
/// threefold, and a letter past one delivery's room would hold up every letter behind it.
const QUOTED_BODY_BYTES_MAX: usize = 2 * BODY_BYTES_MAX;
const _: () = assert!(QUOTED_BODY_BYTES_MAX + 2 * FROM_BYTES_MAX < super::hook::LETTERS_CHARS_MAX);
const PLUGIN_NAME: &str = "crazyagent-relay";
const MONITOR_NAME: &str = "crazyagent-inbox";
const ROSTER_PART: &str = "relay.json.part";
const TOOLS_TAKEN: &str = ".ttaken";
const WAITING_TAKEN: &str = ".wtaken";
const ASK_TAKEN: &str = ".asking";
const PART: &str = ".rpart";
const ASKS_PER_TICK_MAX: usize = 8;
/// The most names one relay `post` request counts: the tool, `graphify` and a skill.
const TOOLS_PER_POST_MAX: usize = 3;
/// The most bytes of the engine's answer to one relay request after its `ok` line.
pub const REPLY_BYTES_MAX: usize = 64 * 1024;
/// How long a native teammate's hook or monitor waits for the engine's answer to one relay
/// request: several engine ticks, within every relay hook's own timeout.
pub const REQUEST_WAIT: Duration = Duration::from_millis(1500);
/// The engine's yes that opens its answer to a relay request, and its no.
pub(crate) const ANSWERED: &[u8; 3] = b"ok\n";
pub(crate) const REFUSED: &[u8; 3] = b"no\n";
const HELD_MAX: usize = 16;
const TOOLS_PER_TICK_MAX: usize = 256;
const FROM_BYTES_MAX: usize = 128;
const TEAMMATES_MAX: usize = 64;
const TRANSCRIPTS_READ_MAX: usize = 8;
const TRANSCRIPT_TAIL_BYTES: u64 = 1024 * 1024;
const RECEIPTS_CHECKED_MAX: usize = 64;
const LEFT_STALE: Duration = Duration::from_secs(10 * 60);

const _: () = assert!(INBOX_MAX <= PENDING_MAX && TEAMMATES_MAX >= 1);
const _: () = assert!(LETTER_BYTES_MAX as usize > 2 * BODY_BYTES_MAX + 1024);
const _: () = assert!(PREVIEW_BYTES < BODY_BYTES_MAX && FROM_BYTES_MAX < 1024);
const _: () = assert!(ANSWER_BYTES_MAX as usize > HELD_MAX * (2 * 512 + FROM_BYTES_MAX + 64));
const _: () = assert!(
    ANSWER_BYTES_MAX as usize <= REPLY_BYTES_MAX
        && 4 * super::hook::LETTERS_CHARS_MAX + 1024 <= REPLY_BYTES_MAX
        && INBOX_MAX * (PREVIEW_BYTES + FROM_BYTES_MAX + 128) <= REPLY_BYTES_MAX
);

/// One message as the relay hands it to a session: written by the engine as
/// `<id as 32 hex digits>.inbox` in the agent's mailbox, taken by the session whose tag
/// it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Letter {
    pub id: MessageId,
    pub to: String,
    pub from: String,
    pub kind: MessageKind,
    pub question: Option<(QuestionFor, Severity)>,
    pub work: Option<WorkId>,
    pub reply_to: Option<MessageId>,
    pub body: String,
}

impl Letter {
    /// The one line the monitor prints to wake a session: the mark, the sender and the
    /// body's first `PREVIEW_BYTES` bytes on one line. Takes the letter. Returns the line.
    pub(crate) fn notice(&self) -> String {
        let preview = one_line(clip(&self.body, PREVIEW_BYTES));
        format!("{MARK}{}] from {}: {preview}", self.id, self.from)
    }

    /// The whole message as a session reads it: the mark, the sender, what kind of
    /// message it is and the full body with every line quoted, so none starts as a speaker
    /// or another message. Takes the letter. Returns the text.
    pub(crate) fn text(&self) -> String {
        let about = match (self.kind, self.question) {
            (MessageKind::Question, Some((target, severity))) => {
                let target = match target {
                    QuestionFor::Me => "the owner",
                    QuestionFor::Master => "the master",
                    QuestionFor::Lead => "you as lead",
                };
                format!(" (question for {target}, {} severity)", severity.as_str())
            }
            (MessageKind::Reply, _) => self
                .reply_to
                .map(|id| format!(" (reply to msg {id})"))
                .unwrap_or_default(),
            (MessageKind::Prompt, _) => " (work for you)".to_owned(),
            (MessageKind::Note, _) => " (note, it takes no reply)".to_owned(),
            _ => String::new(),
        };
        let work = self
            .work
            .map(|work| format!(" on work {work}"))
            .unwrap_or_default();
        let body = crate::team::events::quoted_lines(&self.body);
        format!(
            "{MARK}{}] from {}{about}{work}:\n{}",
            self.id,
            self.from,
            clip(&body, QUOTED_BODY_BYTES_MAX)
        )
    }
}

/// What the engine tells a mailbox's sessions each tick, as `relay.json`: the project's
/// "Overlapping edits" and workflow nudges settings and the lead's own worktree, under
/// which its teammates work in `.worktrees/<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Roster {
    pub overlaps: Overlaps,
    #[serde(default)]
    pub nudges: Nudges,
    pub root: Option<String>,
}

/// A session's question to the engine before an edit under Overlaps Block: whether a file,
/// relative to the repository root, is claimed by another active agent; `more` names the
/// other files of the same patch, so one answer covers them all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ask {
    pub to: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more: Vec<String>,
}

/// The engine's answer to an `Ask`: the active claims of other agents that cover the
/// file, at most `HELD_MAX`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Answer {
    pub held: Vec<Held>,
}

/// One claim of another agent that covers a file: who holds it and its pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Held {
    pub by: String,
    pub pattern: String,
}

/// Writes the relay plugin into an agent's private home (K2a): `.claude-plugin/plugin.json`
/// with one `experimental.monitors` entry running `<engine> coord-watch <mailbox>`, and
/// `hooks/hooks.json` with exec-form hooks running `<engine> coord-hook <event> <mailbox>`.
/// A plugin left by an earlier launch is replaced; the mailbox folder is made 0700 when
/// missing. Inputs: the home, the mailbox and the engine executable, all absolute.
/// Output: the plugin folder to pass with `--plugin-dir`, or a plain refusal.
pub fn install(home: &Path, mailbox: &Path, engine: &Path) -> Result<PathBuf, EngineError> {
    let (Some(engine_text), Some(mailbox_text)) = (plain(engine), plain(mailbox)) else {
        return Err(failure("coord_relay_path_invalid"));
    };
    if !home.is_absolute() || !real_folder(home) {
        return Err(failure("coord_relay_home_invalid"));
    }
    if !real_folder(mailbox) {
        private_folder(mailbox)?;
    }
    let unwritable = |_| failure("coord_relay_unwritable");
    let plugin = home.join(PLUGIN_FOLDER);
    crate::agent_home::remove_entry(&plugin).map_err(unwritable)?;
    let manifest = plugin.join(".claude-plugin");
    let hooks = plugin.join("hooks");
    for folder in [&plugin, &manifest, &hooks] {
        private_folder(folder)?;
    }
    let monitor = format!("exec '{engine_text}' {WATCH_COMMAND} '{mailbox_text}'");
    let plugin_json = serde_json::json!({
        "name": PLUGIN_NAME,
        "version": "1.0.0",
        "description": "Delivers CrazyAgent team messages to this session and reports its activity to the engine",
        "experimental": {
            "monitors": [{
                "name": MONITOR_NAME,
                "command": monitor,
                "description": "New CrazyAgent team messages",
                "when": "always",
            }],
        },
    });
    let hook = |event: Event| {
        serde_json::json!({
            "type": "command",
            "command": engine_text,
            "args": [HOOK_COMMAND, event.as_str(), mailbox_text],
            "timeout": event.timeout_seconds(),
        })
    };
    let hooks_json = serde_json::json!({
        "hooks": {
            "PostToolUse": [{"matcher": "*", "hooks": [hook(Event::PostTool)]}],
            "Stop": [{"hooks": [hook(Event::Stop)]}],
            "PreToolUse": [
                {"matcher": "AskUserQuestion", "hooks": [hook(Event::Asked)]},
                {"matcher": "Edit|Write|MultiEdit|NotebookEdit", "hooks": [hook(Event::Edit)]},
            ],
            "Notification": [{
                "matcher": "permission_prompt|elicitation_dialog|elicitation_url_dialog",
                "hooks": [hook(Event::Notification)],
            }],
            "UserPromptSubmit": [{"hooks": [hook(Event::Prompt)]}],
        },
    });
    let bytes = |value: &serde_json::Value| {
        let mut text = value.to_string();
        text.push('\n');
        text.into_bytes()
    };
    crate::agent_home::write_whole(&manifest, "plugin.json", &bytes(&plugin_json))
        .map_err(unwritable)?;
    crate::agent_home::write_whole(&hooks, "hooks.json", &bytes(&hooks_json))
        .map_err(unwritable)?;
    assert!(plugin.starts_with(home));
    Ok(plugin)
}

/// The Codex tools a team agent's edit guard runs before (PA-12): the patch tool and the
/// shell, which carries `apply_patch` when the model has no patch tool of its own (proof
/// on Codex 0.167.1, `.local-state/flow-sprint/w-v231-pa1213/PA12-codex-hooks-proof.md`).
pub const CODEX_EDIT_MATCHER: &str = "apply_patch|Bash";

/// The command of a Codex team agent's edit guard hook, `'<engine>' coord-hook codex-edit
/// '<mailbox>'`, which Codex runs through the shell before each `CODEX_EDIT_MATCHER` call;
/// the mailbox folder is made 0700 when missing, so the engine serves it. Inputs: the
/// mailbox and the engine executable, both absolute. Output: the command, or a plain
/// refusal.
pub fn codex_edit_hook(mailbox: &Path, engine: &Path) -> Result<String, EngineError> {
    let (Some(engine_text), Some(mailbox_text)) = (plain(engine), plain(mailbox)) else {
        return Err(failure("coord_relay_path_invalid"));
    };
    if !real_folder(mailbox) {
        private_folder(mailbox)?;
    }
    let command = format!(
        "'{engine_text}' {HOOK_COMMAND} {} '{mailbox_text}'",
        Event::CodexEdit.as_str()
    );
    assert!(command.starts_with('\'') && !command.chars().any(char::is_control));
    Ok(command)
}

/// An absolute path as hook text: valid UTF-8 with no control character, quote or `$`, so
/// it sits inside single quotes in a shell command. Takes the path. Returns the text, or
/// None.
fn plain(path: &Path) -> Option<String> {
    path.is_absolute()
        .then(|| path.to_str())
        .flatten()
        .filter(|text| {
            !text
                .chars()
                .any(|c| c.is_control() || matches!(c, '\'' | '$'))
        })
        .map(str::to_owned)
}

/// Tells whether an agent's home holds a relay plugin, so the engine serves its mailbox.
/// Takes the home. Returns true when the plugin manifest is a plain file there.
pub fn installed(home: &Path) -> bool {
    std::fs::symlink_metadata(
        home.join(PLUGIN_FOLDER)
            .join(".claude-plugin")
            .join("plugin.json"),
    )
    .is_ok_and(|metadata| metadata.is_file())
}

/// How a lead's mailbox is laid out: one folder for every session of an agent that leads
/// no native team, or, for a native lead, the lead's sessions in [`LEAD_PART`], which the
/// teammates' sandbox keeps them out of (relay mailbox forgery C1), while its teammates'
/// sessions use no mailbox entry but the roster there and talk to the engine over the
/// lead's caller socket (relay over the caller socket, OQ-7 B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Single,
    Split,
}

/// Where one session's entries sit in its mailbox, from where the engine placed it: the
/// mailbox itself for an agent that leads no native team, [`LEAD_PART`] for a native lead's
/// own sessions (relay mailbox forgery C1), and for a native lead's teammates the mailbox
/// itself, whose only entry they read is the roster in the lead's part, as everything else
/// goes over the caller socket (relay over the caller socket, OQ-7 B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    Single,
    #[cfg(unix)]
    Lead,
    Teammates,
}

/// Opens the folder one part's entries sit in, never following a link. Takes the mailbox
/// and the part. Returns the folder, the mailbox itself but for a native lead's own part,
/// or the I/O error.
pub(crate) fn part_folder(mailbox: Mailbox, part: Part) -> std::io::Result<Mailbox> {
    match part {
        #[cfg(unix)]
        Part::Lead => mailbox.folder(LEAD_PART),
        Part::Single | Part::Teammates => Ok(mailbox),
    }
}

/// Runs one engine tick of an agent's relay: refreshes `relay.json`, marks Delivered each
/// message a session took, drops letters no longer waiting, writes the next waiting
/// messages of the agent and its native teammates (none under the Typing only delivery
/// setting, nor for a master, which K2b feeds through its engine input), applies the
/// sessions' tool counts, activity and waits and each native teammate's Idle or Working,
/// answers their overlap questions, and, when `receipts` is set, marks Read each delivered
/// message its session answered after (from the home's newest transcripts). Under the
/// split layout the lead's part, which the engine makes when it is missing, holds the
/// roster and serves the lead's tag alone, and nothing in the mailbox itself is read: the
/// teammates report and take their letters through [`answer`], so nothing one teammate
/// writes counts for the lead or another teammate. Everything a session wrote is
/// validated; nothing from the mailbox is trusted. Inputs: the database,
/// the project, the agent, its home, its mailbox, its layout, whether to scan transcripts
/// and the time in ms. Output: Ok, or a SQLite failure; an unreadable mailbox or lead's
/// part is skipped.
#[allow(clippy::too_many_arguments)]
pub fn serve(
    db: &Connection,
    project: &ProjectId,
    lead: AgentId,
    home: &Path,
    mailbox: &Path,
    layout: Layout,
    receipts: bool,
    now: i64,
) -> Result<(), EngineError> {
    let Ok(folder) = Mailbox::open_for_engine(mailbox) else {
        return Ok(());
    };
    let team = recipients(db, project, lead)?;
    if team.is_empty() {
        return Ok(());
    }
    let chosen = settings::read(db, project)?;
    let roster = Roster {
        overlaps: chosen.overlaps,
        nudges: chosen.nudges,
        root: worktree(db, project, lead)?,
    };
    let posts = posts(db, project, lead)?;
    match layout {
        Layout::Single => {
            write_roster(&folder, &roster);
            serve_part(db, project, &folder, &team, posts, now)?;
        }
        Layout::Split => {
            let Ok(own) = folder
                .make_folder(LEAD_PART)
                .and_then(|()| folder.folder(LEAD_PART))
            else {
                return Ok(());
            };
            let alone: BTreeMap<String, AgentId> = team
                .iter()
                .filter(|(tag, _)| *tag == LEAD_TAG)
                .map(|(tag, agent)| (tag.clone(), *agent))
                .collect();
            write_roster(&own, &roster);
            serve_part(db, project, &own, &alone, posts, now)?;
        }
    }
    if receipts {
        read_receipts(db, home, Format::Claude, team.values().copied(), now)?;
    }
    Ok(())
}

/// Whether the relay hands a lead's team its messages: not under the Typing only delivery
/// setting, nor for a master, which K2b feeds through its engine input, nor for a Codex
/// agent, which its app-server bridge or typing feeds; its mailbox serves the edit guard
/// alone (PA-12). Takes the database, the project and the lead. Returns the answer or a
/// SQLite failure.
fn posts(db: &Connection, project: &ProjectId, lead: AgentId) -> Result<bool, EngineError> {
    let delivery = settings::delivery(db, project, HarnessKind::ClaudeCode)?;
    let (master, harness): (bool, String) = db
        .query_row(
            "SELECT role='master', harness FROM team_agents WHERE id=?1",
            [lead],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(state)?;
    let claude = HarnessKind::from_harness(&harness) == Some(HarnessKind::ClaudeCode);
    Ok(delivery != Delivery::TypingOnly && !master && claude)
}

/// One relay request of a native teammate's session on its lead's caller socket (relay over
/// the caller socket, OQ-7 B), each the engine side of one file report or claim the
/// mailbox carried before: `post own|sub <name>...` after a tool call, `stop own|sub`,
/// `wait question|permission`, `prompt typed|other`, `ask <path>` before an edit under
/// Overlaps Block, `nudge` before an edit, and the monitor's `peek`. `own` marks the
/// teammate's own session, which takes its letters, apart from a subagent of it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    Post { own: bool, tools: Vec<String> },
    Stop { own: bool },
    Wait(Waiting),
    Prompt { typed: bool },
    Ask(String),
    Nudge,
    Peek,
}

impl Request {
    /// Reads one request line without its `relay` word or newline: plain words, a tool name
    /// as `record_tool` takes it, at most `TOOLS_PER_POST_MAX` of them, and a path with no
    /// control character. Takes the bytes. Returns the request, or None for anything else.
    fn parse(line: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(line).ok()?;
        if text.chars().any(char::is_control) {
            return None;
        }
        let (verb, rest) = text.split_once(' ').unwrap_or((text, ""));
        let own = |word: &str| match word {
            "own" => Some(true),
            "sub" => Some(false),
            _ => None,
        };
        let request = match verb {
            "post" => {
                let mut words = rest.split(' ');
                let own = own(words.next()?)?;
                let tools: Vec<String> = words.map(str::to_owned).collect();
                let plain = tools.len() <= TOOLS_PER_POST_MAX
                    && tools.iter().all(|tool| {
                        !tool.is_empty()
                            && tool.len() <= TOOL_BYTES_MAX
                            && !tool.chars().any(char::is_whitespace)
                    });
                plain.then_some(Self::Post { own, tools })?
            }
            "stop" => Self::Stop { own: own(rest)? },
            "wait" => Self::Wait(Waiting::parse(rest)?),
            "prompt" => match rest {
                "typed" => Self::Prompt { typed: true },
                "other" => Self::Prompt { typed: false },
                _ => return None,
            },
            "ask" if !rest.is_empty() => Self::Ask(rest.to_owned()),
            "nudge" if rest.is_empty() => Self::Nudge,
            "peek" if rest.is_empty() => Self::Peek,
            _ => return None,
        };
        Some(request)
    }
}

/// The Task-start reminder of one native lead's teammates (T3-15): the lead's text, none
/// when its launch names nothing, and the teammates given it since they last rested.
#[derive(Debug, Default)]
pub struct TaskStarts {
    pub text: Option<String>,
    pub reminded: BTreeSet<AgentId>,
}

/// The native lead a relay request came in for on its caller socket (OQ-7 B): its project,
/// its id and its teammates' Task-start reminder (T3-15).
pub struct ForLead<'a> {
    pub project: &'a ProjectId,
    pub id: AgentId,
    pub starts: &'a mut TaskStarts,
}

/// Answers one relay request a native teammate's hook or monitor sent on its lead's caller
/// socket (relay over the caller socket, OQ-7 B). It acts for the live teammate the
/// engine's rows put in `pane`, the pane the socket found the connecting process in, never
/// for one the request names, so no teammate reports, takes letters or asks as another.
/// The answer is `ok` and its payload, written at once; letters and the first-edit reminder
/// are marked Delivered only once written whole. A pane no live teammate of the lead holds,
/// a malformed request and a failure get `no`. Inputs: the database, the lead with its
/// project and Task-start reminder, the pane, the request, the connection and the time in
/// ms. Output: Ok, or the SQLite failure after answering no.
pub fn answer(
    db: &Connection,
    lead: ForLead<'_>,
    pane: &str,
    request: &[u8],
    out: &mut dyn Write,
    now: i64,
) -> Result<(), EngineError> {
    let ForLead {
        project,
        id: lead,
        starts,
    } = lead;
    let reply = match reply(db, project, lead, pane, request, now, starts) {
        Ok(reply) => reply,
        Err(problem) => {
            let _ = out.write_all(REFUSED);
            return Err(problem);
        }
    };
    let Some(((payload, delivered), started)) = reply else {
        let _ = out.write_all(REFUSED);
        return Ok(());
    };
    assert!(payload.len() <= REPLY_BYTES_MAX);
    let mut whole = Vec::with_capacity(ANSWERED.len() + payload.len());
    whole.extend_from_slice(ANSWERED);
    whole.extend_from_slice(&payload);
    if out.write_all(&whole).and_then(|()| out.flush()).is_err() {
        return Ok(());
    }
    if let Some(mate) = started {
        starts.reminded.insert(mate);
    }
    for id in delivered {
        advance(db, id, Receipt::Delivered, now)?;
    }
    Ok(())
}

/// A relay answer's payload and the messages it hands over.
type Reply = (Vec<u8>, Vec<MessageId>);

/// Carries out one relay request for the teammate in `pane` and makes its answer: a prompt
/// that starts a task, the teammate's first or its first since it rested, gets the lead's
/// Task-start reminder while the setting is On, and a rest makes its next prompt start one.
/// Takes the database, the project, the lead, the pane, the request, the time in ms and the
/// lead's reminder. Returns the payload, the messages it hands over and the teammate to mark
/// reminded once it is written, None for a pane no live teammate of the lead holds or a
/// malformed request, or a SQLite failure.
fn reply(
    db: &Connection,
    project: &ProjectId,
    lead: AgentId,
    pane: &str,
    request: &[u8],
    now: i64,
    starts: &mut TaskStarts,
) -> Result<Option<(Reply, Option<AgentId>)>, EngineError> {
    let team = recipients(db, project, lead)?;
    let Some(mate) = team.get(pane).copied().filter(|_| pane != LEAD_TAG) else {
        return Ok(None);
    };
    assert_ne!(mate, lead);
    let Some(request) = Request::parse(request) else {
        return Ok(None);
    };
    let letters = |own: bool| -> Result<Vec<Letter>, EngineError> {
        if !own || !posts(db, project, lead)? {
            return Ok(Vec::new());
        }
        let mut taken = Vec::new();
        let mut size = 0;
        for found in super::pending(db, mate, INBOX_MAX)? {
            let letter = letter(db, pane, found);
            let length = super::hook::letter_chars(&letter);
            if size + length > super::hook::LETTERS_CHARS_MAX {
                break;
            }
            size += length;
            taken.push(letter);
        }
        Ok(taken)
    };
    let handed = |mut taken: Vec<Letter>| {
        let ids: Vec<MessageId> = taken.iter().map(|letter| letter.id).collect();
        let text = if taken.is_empty() {
            String::new()
        } else {
            super::hook::bundle(&mut taken)
        };
        (text.into_bytes(), ids)
    };
    let mut started = None;
    let answered = match request {
        Request::Post { own, tools } => {
            settle(db, mate, now)?;
            for tool in &tools {
                super::record_tool(db, mate, tool, now)?;
            }
            rests(db, mate, false)?;
            handed(letters(own)?)
        }
        Request::Stop { own } => {
            settle(db, mate, now)?;
            super::touch(db, mate, now)?;
            let taken = letters(own)?;
            if taken.is_empty() {
                rests(db, mate, true)?;
                starts.reminded.remove(&mate);
            }
            handed(taken)
        }
        Request::Wait(waiting) => {
            super::set_waiting(db, mate, Some(waiting), now)?;
            (Vec::new(), Vec::new())
        }
        Request::Prompt { typed } => {
            rests(db, mate, false)?;
            if typed {
                settle(db, mate, now)?;
                super::touch(db, mate, now)?;
            }
            match starts.text.as_deref() {
                Some(text)
                    if !starts.reminded.contains(&mate)
                        && settings::read(db, project)?.task_start == TaskStart::On =>
                {
                    assert!(text.len() <= crate::launch_context::TASK_START_BYTES_MAX);
                    started = Some(mate);
                    (text.as_bytes().to_vec(), Vec::new())
                }
                _ => (Vec::new(), Vec::new()),
            }
        }
        Request::Ask(path) => {
            let bytes = serde_json::to_vec(&Answer {
                held: held(db, project, mate, &path)?,
            })
            .map_err(state)?;
            (bytes, Vec::new())
        }
        Request::Nudge => match super::adoption::first_edit_note(db, project, mate, now)? {
            Some(note) => (
                super::adoption::FIRST_EDIT_REMINDER.as_bytes().to_vec(),
                vec![note],
            ),
            None => (Vec::new(), Vec::new()),
        },
        Request::Peek => {
            let mut lines = String::new();
            if posts(db, project, lead)? {
                for found in super::pending(db, mate, INBOX_MAX)? {
                    let letter = letter(db, pane, found);
                    lines.push_str(&format!("{} {}\n", letter.id, letter.notice()));
                }
            }
            (lines.into_bytes(), Vec::new())
        }
    };
    Ok(Some((answered, started)))
}

/// Clears a teammate's wait once its session moves on, only while one is stored, so a
/// session that never waited writes nothing. Takes the database, the teammate and the time
/// in ms. Returns Ok or a SQLite failure.
fn settle(db: &Connection, agent: AgentId, now: i64) -> Result<(), EngineError> {
    let waiting: Option<String> = db
        .query_row(
            "SELECT waiting FROM coord_activity WHERE agent=?1",
            [agent],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?
        .flatten();
    if waiting.is_some() {
        super::set_waiting(db, agent, None, now)?;
    }
    Ok(())
}

/// Shows a native teammate's row Idle while it rests or Working once it works again, only
/// from Idle or Working. Takes the database, the teammate and whether it rests. Returns Ok
/// or a SQLite failure.
fn rests(db: &Connection, agent: AgentId, resting: bool) -> Result<(), EngineError> {
    let status = if resting {
        AgentStatus::Idle
    } else {
        AgentStatus::Working
    };
    db.execute(
        "UPDATE team_agents SET status=?2 WHERE id=?1 AND role='teammate'
         AND status IN ('idle','working') AND status<>?2",
        params![agent, status.name()],
    )
    .map_err(state)?;
    Ok(())
}

/// Runs the steps of one relay tick that read and write one folder of a mailbox, for the
/// sessions whose entries sit there and no other: their claims, letters, reports and
/// overlap questions. Takes the database, the project, the folder, its sessions by tag,
/// whether letters are posted and the time in ms. Returns Ok, or a SQLite failure.
fn serve_part(
    db: &Connection,
    project: &ProjectId,
    folder: &Mailbox,
    team: &BTreeMap<String, AgentId>,
    posts: bool,
    now: i64,
) -> Result<(), EngineError> {
    collect_shown(db, folder, team, now)?;
    sweep_letters(db, folder, team, posts)?;
    if posts {
        post_letters(db, folder, team)?;
    }
    take_activity(db, folder, team, now)?;
    answer_asks(db, project, folder, team)
}

/// The mailbox name stem of a delivery tag: 32 hex digits, zero for the lead and the pane
/// number plus one for a teammate's pane. Takes the tag. Returns the stem, or None for a
/// tag that is neither.
pub(crate) fn tag_key(tag: &str) -> Option<String> {
    let number: u128 = if tag == LEAD_TAG {
        0
    } else if crate::terminal::pane_id_valid(tag) {
        tag[1..].parse::<u128>().ok()? + 1
    } else {
        return None;
    };
    let key = format!("{number:032x}");
    assert!(call_id_valid(&key));
    Some(key)
}

/// The mailbox name stem of a message, its id as 32 hex digits. Takes the id. Returns
/// the stem, or None for an id that is not positive.
pub(crate) fn letter_stem(id: MessageId) -> Option<String> {
    let id = u64::try_from(id).ok().filter(|id| *id > 0)?;
    let stem = format!("{id:032x}");
    assert!(call_id_valid(&stem));
    Some(stem)
}

/// Reads a message id back from a mailbox name stem. Takes the stem. Returns the id, or
/// None for a stem that is not one `letter_stem` writes.
pub(crate) fn letter_id(stem: &str) -> Option<MessageId> {
    if !call_id_valid(stem) {
        return None;
    }
    u128::from_str_radix(stem, 16)
        .ok()
        .and_then(|number| i64::try_from(number).ok())
        .filter(|id| *id > 0)
}

/// Reads one small mailbox entry without following a link or waiting on a FIFO, only a
/// regular file of at most `bytes_max` bytes. Takes the mailbox, the name and the bound.
/// Returns the bytes, or None.
pub(crate) fn read_entry(folder: &Mailbox, name: &str, bytes_max: u64) -> Option<Vec<u8>> {
    let file = folder.read(name).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > bytes_max {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(bytes_max + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= bytes_max).then_some(bytes)
}

/// Reads one letter. Takes the mailbox and its entry name. Returns the letter, or None
/// for anything that is not a whole letter.
pub(crate) fn read_letter(folder: &Mailbox, name: &str) -> Option<Letter> {
    let bytes = read_entry(folder, name, LETTER_BYTES_MAX)?;
    let letter: Letter = serde_json::from_slice(&bytes).ok()?;
    (letter.body.len() <= BODY_BYTES_MAX && letter.from.len() <= FROM_BYTES_MAX).then_some(letter)
}

/// Reads the engine's `relay.json` from one part's folder: its own, but a native lead's
/// teammates read the one in the lead's part, the only entry there their sandbox reads.
/// Takes the part's folder and the part. Returns the roster, or None before the engine
/// wrote one.
pub(crate) fn read_roster(folder: &Mailbox, part: Part) -> Option<Roster> {
    let name = match part {
        Part::Teammates => format!("{LEAD_PART}/{ROSTER_FILE}"),
        Part::Single => ROSTER_FILE.to_owned(),
        #[cfg(unix)]
        Part::Lead => ROSTER_FILE.to_owned(),
    };
    let bytes = read_entry(folder, &name, ROSTER_BYTES_MAX)?;
    serde_json::from_slice(&bytes).ok()
}

/// Puts one entry in place whole: written under a fresh part name and renamed over the
/// name, so a reader never sees half of it. Takes the mailbox, the name and the bytes.
/// Returns the I/O failure; the part is removed on failure.
pub(crate) fn put(folder: &Mailbox, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let part = format!("{}{PART}", call_id()?);
    let written = folder
        .create(&part)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| folder.rename(&part, name));
    if written.is_err() {
        let _ = folder.remove(&part);
    }
    written
}

/// The lead and its live native teammates, by delivery tag. A teammate row is K5a's: role
/// `teammate`, parent the lead, and its tmux pane id. Takes the database, the project and
/// the lead. Returns the map, empty for a lead that is not a live agent of the project.
fn recipients(
    db: &Connection,
    project: &ProjectId,
    lead: AgentId,
) -> Result<BTreeMap<String, AgentId>, EngineError> {
    let live: Option<i64> = db
        .query_row(
            "SELECT id FROM team_agents WHERE id=?1 AND project=?2
             AND status IN ('starting','idle','working')",
            params![lead, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    let mut team = BTreeMap::new();
    if live.is_none() {
        return Ok(team);
    }
    team.insert(LEAD_TAG.to_owned(), lead);
    let Ok(mut statement) = db.prepare(
        "SELECT id,pane FROM team_agents WHERE project=?1 AND parent=?2 AND role='teammate'
         AND status IN ('starting','idle','working') AND pane IS NOT NULL ORDER BY id LIMIT ?3",
    ) else {
        return Ok(team);
    };
    let limit = i64::try_from(TEAMMATES_MAX).map_err(state)?;
    let rows = statement
        .query_map(params![project.as_str(), lead, limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(state)?;
    for row in rows {
        let (id, pane) = row.map_err(state)?;
        if crate::terminal::pane_id_valid(&pane) {
            team.insert(pane, id);
        }
    }
    assert!(team.len() <= TEAMMATES_MAX + 1);
    Ok(team)
}

/// The lead's own worktree, under which its teammates work. Takes the database, the
/// project and the lead. Returns the path, None when the lead has none recorded.
fn worktree(
    db: &Connection,
    project: &ProjectId,
    lead: AgentId,
) -> Result<Option<String>, EngineError> {
    db.query_row(
        "SELECT worktree FROM team_worktrees WHERE agent=?1 AND project=?2",
        params![lead, project.as_str()],
        |row| row.get(0),
    )
    .optional()
    .map_err(state)
}

/// Replaces `relay.json`; a part or link an agent planted at the part name is removed
/// first, and a failure leaves the last roster in place.
fn write_roster(folder: &Mailbox, roster: &Roster) {
    let Ok(bytes) = serde_json::to_vec(roster) else {
        return;
    };
    if bytes.len() as u64 > ROSTER_BYTES_MAX {
        return;
    }
    let _ = folder.remove(ROSTER_PART);
    let written = folder
        .create(ROSTER_PART)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| folder.rename(ROSTER_PART, ROSTER_FILE));
    if written.is_err() {
        let _ = folder.remove(ROSTER_PART);
    }
}

/// Says whether a message is addressed to one of the mailbox's recipients.
fn addressed(to: Party, team: &BTreeMap<String, AgentId>) -> bool {
    matches!(to, Party::Agent(agent) if team.values().any(|member| *member == agent))
}

/// Marks Delivered each message a session took (renamed to `.shown`) that is addressed to
/// the mailbox's lead or teammates and still Sent, then removes the claim.
fn collect_shown(
    db: &Connection,
    folder: &Mailbox,
    team: &BTreeMap<String, AgentId>,
    now: i64,
) -> Result<(), EngineError> {
    for (_, stem) in folder.entries(SHOWN) {
        let name = format!("{stem}{SHOWN}");
        if let Some(id) = letter_id(&stem) {
            match message(db, id) {
                Ok(found) if addressed(found.to, team) && found.receipt == Receipt::Sent => {
                    advance(db, id, Receipt::Delivered, now)?;
                }
                Ok(_) => {}
                Err(problem) if problem.0 == "coord_message_unknown" => {}
                Err(problem) => return Err(problem),
            }
        }
        let _ = folder.remove(&name);
    }
    Ok(())
}

/// Removes each letter whose message no longer waits for delivery here: unknown, for
/// another agent, typed, failed or expired, or every letter when the relay does not post
/// (Typing only, or a master, which hears its messages through its own engine input).
fn sweep_letters(
    db: &Connection,
    folder: &Mailbox,
    team: &BTreeMap<String, AgentId>,
    posts: bool,
) -> Result<(), EngineError> {
    for (_, stem) in folder.entries(INBOX) {
        let keep = match letter_id(&stem) {
            Some(id) if posts => match message(db, id) {
                Ok(found) => addressed(found.to, team) && found.receipt == Receipt::Sent,
                Err(problem) if problem.0 == "coord_message_unknown" => false,
                Err(problem) => return Err(problem),
            },
            _ => false,
        };
        if !keep {
            let _ = folder.remove(&format!("{stem}{INBOX}"));
        }
    }
    Ok(())
}

/// Writes a letter for each waiting message of the lead and its teammates, oldest first,
/// while fewer than `INBOX_MAX` wait; a message whose letter is waiting or taken is
/// skipped. Letters are listed before claims, so one a session takes between the two
/// listings shows in both.
fn post_letters(
    db: &Connection,
    folder: &Mailbox,
    team: &BTreeMap<String, AgentId>,
) -> Result<(), EngineError> {
    let waiting: BTreeSet<String> = folder
        .entries(INBOX)
        .into_iter()
        .map(|(_, stem)| stem)
        .collect();
    let taken: BTreeSet<String> = folder
        .entries(SHOWN)
        .into_iter()
        .map(|(_, stem)| stem)
        .collect();
    let mut count = waiting.len();
    for (tag, agent) in team {
        if count >= INBOX_MAX {
            break;
        }
        for found in super::pending(db, *agent, INBOX_MAX)? {
            if count >= INBOX_MAX {
                break;
            }
            let Some(stem) = letter_stem(found.id) else {
                continue;
            };
            if waiting.contains(&stem) || taken.contains(&stem) {
                continue;
            }
            let letter = letter(db, tag, found);
            let bytes = serde_json::to_vec(&letter).map_err(state)?;
            assert!(bytes.len() as u64 <= LETTER_BYTES_MAX);
            if put(folder, &format!("{stem}{INBOX}"), &bytes).is_ok() {
                count += 1;
            }
        }
    }
    assert!(count <= INBOX_MAX.max(waiting.len()));
    Ok(())
}

/// The letter of one waiting message for a delivery tag. Takes the database, the tag and
/// the message. Returns the letter.
fn letter(db: &Connection, tag: &str, found: Message) -> Letter {
    Letter {
        id: found.id,
        to: tag.to_owned(),
        from: sender(db, found.from),
        kind: found.kind,
        question: found.question,
        work: found.work,
        reply_to: found.reply_to,
        body: found.body,
    }
}

/// The name a session reads for a sender: "the owner", "CrazyAgent", or the agent's
/// label, without control characters and at most `FROM_BYTES_MAX` bytes.
fn sender(db: &Connection, from: Party) -> String {
    let name = match from {
        Party::Owner => "the owner".to_owned(),
        Party::Engine => "CrazyAgent".to_owned(),
        Party::Agent(agent) => {
            crate::team::events::label(db, agent).unwrap_or_else(|_| format!("agent {agent}"))
        }
    };
    let plain: String = name.chars().filter(|c| !c.is_control()).collect();
    clip(&plain, FROM_BYTES_MAX).to_owned()
}

/// Applies what each session reported since the last tick: its tool log as counted tool
/// calls, its touch as activity, its latest wait, and for a native teammate whether it
/// rests, as its row's Idle or Working. Each report is claimed by rename before it is
/// read, so one written meanwhile waits for the next tick; the resting mark stays until
/// the teammate's own hook clears it.
fn take_activity(
    db: &Connection,
    folder: &Mailbox,
    team: &BTreeMap<String, AgentId>,
    now: i64,
) -> Result<(), EngineError> {
    let resting: BTreeSet<String> = folder
        .entries(IDLE)
        .into_iter()
        .map(|(_, stem)| stem)
        .collect();
    for (tag, agent) in team {
        let Some(key) = tag_key(tag) else {
            continue;
        };
        if tag != LEAD_TAG {
            rests(db, *agent, resting.contains(&key))?;
        }
        if let Some(log) = claim(folder, &key, TOOLS, TOOLS_TAKEN, TOOLS_LOG_BYTES_MAX) {
            let text = String::from_utf8_lossy(&log);
            for tool in text.lines().take(TOOLS_PER_TICK_MAX) {
                let plain = !tool.is_empty()
                    && tool.len() <= TOOL_BYTES_MAX
                    && !tool.chars().any(|c| c.is_control() || c.is_whitespace());
                if plain {
                    super::record_tool(db, *agent, tool, now)?;
                }
            }
        }
        if folder.remove(&format!("{key}{TOUCH}")).is_ok() {
            super::touch(db, *agent, now)?;
        }
        if let Some(text) = claim(folder, &key, WAITING, WAITING_TAKEN, WAITING_BYTES_MAX) {
            let waiting = match text.trim_ascii() {
                b"none" => Some(None),
                other => std::str::from_utf8(other)
                    .ok()
                    .and_then(Waiting::parse)
                    .map(Some),
            };
            if let Some(waiting) = waiting {
                super::set_waiting(db, *agent, waiting, now)?;
            }
        }
    }
    Ok(())
}

/// Claims one fixed report by renaming it, reads it and removes it. Takes the mailbox,
/// the tag key, both suffixes and the bound. Returns the bytes, or None.
fn claim(
    folder: &Mailbox,
    key: &str,
    suffix: &str,
    taken: &str,
    bytes_max: u64,
) -> Option<Vec<u8>> {
    let held = format!("{key}{taken}");
    folder.rename(&format!("{key}{suffix}"), &held).ok()?;
    let bytes = read_entry(folder, &held, bytes_max);
    let _ = folder.remove(&held);
    bytes
}

/// Answers the overlap questions sessions asked before an edit, at most
/// `ASKS_PER_TICK_MAX` a tick, with the active claims of other agents covering the file
/// (K1a `overlapping`); a question from an unknown tag or about a path that is not one
/// plain repository file gets an empty answer. Answers and claims left for `LEFT_STALE`
/// are removed.
fn answer_asks(
    db: &Connection,
    project: &ProjectId,
    folder: &Mailbox,
    team: &BTreeMap<String, AgentId>,
) -> Result<(), EngineError> {
    let now = SystemTime::now();
    for suffix in [ANSWER, ASK_TAKEN] {
        for (changed, stem) in folder.entries(suffix) {
            if now
                .duration_since(changed)
                .is_ok_and(|age| age >= LEFT_STALE)
            {
                let _ = folder.remove(&format!("{stem}{suffix}"));
            }
        }
    }
    for (_, stem) in folder.entries(ASK).into_iter().take(ASKS_PER_TICK_MAX) {
        let held_name = format!("{stem}{ASK_TAKEN}");
        if folder.rename(&format!("{stem}{ASK}"), &held_name).is_err() {
            continue;
        }
        let ask = read_entry(folder, &held_name, ASK_BYTES_MAX)
            .and_then(|bytes| serde_json::from_slice::<Ask>(&bytes).ok());
        let _ = folder.remove(&held_name);
        let held = match ask.as_ref().and_then(|ask| Some((team.get(&ask.to)?, ask))) {
            Some((agent, ask)) if ask.more.len() < ASK_PATHS_MAX => {
                let mut all: Vec<Held> = Vec::new();
                for path in std::iter::once(&ask.path).chain(&ask.more) {
                    for one in held(db, project, *agent, path)? {
                        if all.len() < HELD_MAX && !all.contains(&one) {
                            all.push(one);
                        }
                    }
                }
                all
            }
            Some(_) => vec![Held {
                by: "CrazyAgent".to_owned(),
                pattern: "more files than one check reads; split the patch".to_owned(),
            }],
            None => Vec::new(),
        };
        assert!(held.len() <= HELD_MAX);
        let bytes = serde_json::to_vec(&Answer { held }).map_err(state)?;
        assert!(bytes.len() as u64 <= ANSWER_BYTES_MAX);
        let _ = put(folder, &format!("{stem}{ANSWER}"), &bytes);
    }
    Ok(())
}

/// The active claims of other agents that cover one file, at most `HELD_MAX`, for an
/// agent's overlap question (K1a `overlapping`); a path that is not one plain repository
/// file has none. A project holding more claims than one check reads answers one claim
/// held by CrazyAgent, so the edit check fails closed. Takes the database, the project, the asking agent and the
/// repository-relative path. Returns the claims, or a SQLite failure.
fn held(
    db: &Connection,
    project: &ProjectId,
    agent: AgentId,
    path: &str,
) -> Result<Vec<Held>, EngineError> {
    let mut held = Vec::new();
    match super::overlapping(db, project, agent, path) {
        Ok(claims) => {
            for claim in claims.into_iter().filter(|claim| !claim.stale) {
                if held.len() == HELD_MAX {
                    break;
                }
                held.push(Held {
                    by: sender(db, Party::Agent(claim.agent)),
                    pattern: claim.pattern,
                });
            }
        }
        Err(problem) if problem.0.starts_with("coord_state_unavailable") => {
            return Err(problem);
        }
        Err(problem) if problem.0 == "coord_claims_full" => {
            held.push(Held {
                by: "CrazyAgent".to_owned(),
                pattern:
                    "too many claims in this project to check; they free up as the owner accepts or discards finished change sets, or as work no agent is on is released"
                        .to_owned(),
            });
        }
        Err(_) => {}
    }
    assert!(held.len() <= HELD_MAX);
    Ok(held)
}

/// Marks Read each message delivered or typed to one Codex agent whose mark shows in one
/// of its home's newest rollouts with the agent's own output after it, as [`serve`] does
/// from a Claude Code lead's transcripts, and puts back to Sent each one the turn it went
/// into dropped when it ended aborted (`Tails`). Takes the database, the agent, its
/// `CODEX_HOME` and the time in ms. Returns Ok or a SQLite failure.
pub fn codex_receipts(
    db: &Connection,
    agent: AgentId,
    home: &Path,
    now: i64,
) -> Result<(), EngineError> {
    read_receipts(db, home, Format::Codex, [agent], now)
}

/// Marks Read each message delivered or typed to the agents whose mark shows in one of the
/// home's newest session records of the format, Claude Code transcripts
/// (`projects/**/<session>.jsonl`) or Codex rollouts (`sessions/**/rollout-*.jsonl`), with
/// the session's answer after it ([`answered`]), and puts back to Sent each message the
/// Codex bridge delivered into a turn (its `turn`) whose start and aborted end both show in
/// one rollout tail, when no tail shows its mark. Reads the last `TRANSCRIPT_TAIL_BYTES` of at most
/// `TRANSCRIPTS_READ_MAX` files, read-only and never following a link.
fn read_receipts(
    db: &Connection,
    home: &Path,
    format: Format,
    agents: impl IntoIterator<Item = AgentId>,
    now: i64,
) -> Result<(), EngineError> {
    let mut unread = BTreeSet::new();
    let mut turns: BTreeMap<MessageId, (AgentId, String)> = BTreeMap::new();
    for agent in agents {
        let limit = i64::try_from(RECEIPTS_CHECKED_MAX).map_err(state)?;
        let mut statement = db
            .prepare(
                "SELECT id, turn FROM coord_messages WHERE to_kind='agent' AND to_agent=?1
                 AND receipt IN ('delivered','typed') ORDER BY id DESC LIMIT ?2",
            )
            .map_err(state)?;
        let rows = statement
            .query_map(params![agent, limit], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(state)?;
        for row in rows {
            let (id, turn) = row.map_err(state)?;
            if unread.len() < RECEIPTS_CHECKED_MAX {
                unread.insert(id);
                if let Some(turn) = turn {
                    turns.insert(id, (agent, turn));
                }
            }
        }
    }
    if unread.is_empty() {
        return Ok(());
    }
    let wanted: BTreeSet<&str> = turns.values().map(|(_, turn)| turn.as_str()).collect();
    let mut tails = Tails::default();
    let records = crate::usage::capture::records(home, format).0;
    for path in records.into_iter().take(TRANSCRIPTS_READ_MAX) {
        scan_transcript(&path, format, &unread, &wanted, &mut tails);
    }
    for id in &tails.read {
        advance(db, *id, Receipt::Read, now)?;
    }
    for (id, (agent, turn)) in &turns {
        if !tails.shown.contains(id) && tails.aborted.contains(turn) {
            super::messages::requeue_aborted(db, *id, *agent, turn, now)?;
        }
    }
    Ok(())
}

/// What the tails of a home's session records showed: the unread ids answered after their
/// mark, every unread id whose mark shows at all, and the turns unread messages went into
/// whose start and aborted end both show in one Codex rollout tail, so a mark recorded in
/// such a turn would show too; Codex 0.164 drops a steer still waiting when its turn aborts
/// (b19 P2).
#[derive(Default)]
struct Tails {
    read: BTreeSet<MessageId>,
    shown: BTreeSet<MessageId>,
    aborted: BTreeSet<String>,
}

/// Moves a message's receipt forward, leaving one that already moved past it alone.
/// Takes the database, the message, the receipt and the time in ms. Returns Ok or a
/// SQLite failure.
fn advance(db: &Connection, id: MessageId, receipt: Receipt, now: i64) -> Result<(), EngineError> {
    match super::mark(db, id, receipt, now) {
        Err(problem) if problem.0 == "coord_receipt_stale" => Ok(()),
        done => done,
    }
}

/// Tells whether one session record line is the session answering: a Claude Code
/// `assistant` record, or a Codex rollout item the agent wrote, its message or tool call,
/// as the session search reads it (`record_message` names it the agent's). Takes the format
/// and the line. Returns the answer.
fn answered(format: Format, text: &str) -> bool {
    match format {
        Format::Claude => {
            text.contains("\"assistant\"")
                && serde_json::from_str::<serde_json::Value>(text)
                    .is_ok_and(|record| record["type"] == "assistant")
        }
        Format::Codex => {
            text.contains("\"response_item\"")
                && crate::harnesses::transcript::record_message("codex", text.as_bytes())
                    .is_some_and(|(speaker, _)| speaker == "agent")
        }
    }
}

/// One wanted Codex turn's start or aborted end, as a 0.164 rollout line records it.
enum TurnEvent {
    Started(String),
    Aborted(String),
}

/// Reads the wanted turn one Codex rollout line starts or ends aborted: an `event_msg`
/// `task_started` with its `turn_id` or `turn_attribution.turn_id` (0.164 writes the
/// attribution only for a regular turn), or a `turn_aborted` with its `turn_id`. Takes the
/// line and the wanted turn ids. Returns that event, or none for any other line or turn.
fn turn_event(text: &str, wanted: &BTreeSet<&str>) -> Option<TurnEvent> {
    if wanted.is_empty()
        || !(text.contains("\"task_started\"") || text.contains("\"turn_aborted\""))
    {
        return None;
    }
    let record = serde_json::from_str::<serde_json::Value>(text).ok()?;
    if record["type"] != "event_msg" {
        return None;
    }
    let payload = &record["payload"];
    let kind = payload["type"].as_str()?;
    let ids: &[&str] = match kind {
        "task_started" => &["/turn_id", "/turn_attribution/turn_id"],
        "turn_aborted" => &["/turn_id"],
        _ => return None,
    };
    let turn = ids
        .iter()
        .filter_map(|id| payload.pointer(id)?.as_str())
        .find(|turn| wanted.contains(turn))?
        .to_owned();
    Some(if kind == "task_started" {
        TurnEvent::Started(turn)
    } else {
        TurnEvent::Aborted(turn)
    })
}

/// Finds in one session record's tail the unread ids whose mark is followed by the
/// session's answer ([`answered`]), the unread ids whose mark shows at all and, in a Codex
/// rollout, the wanted turns whose start and later aborted end both lie in this tail
/// ([`turn_event`]). Takes the record, its format, the unread ids, the turns they went into
/// and what the tails showed so far.
fn scan_transcript(
    path: &Path,
    format: Format,
    unread: &BTreeSet<MessageId>,
    wanted: &BTreeSet<&str>,
    tails: &mut Tails,
) {
    let Some((mut file, _, length)) = crate::usage::capture::open_record(path) else {
        return;
    };
    let start = length.saturating_sub(TRANSCRIPT_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return;
    }
    let mut reader = BufReader::new(file.take(TRANSCRIPT_TAIL_BYTES));
    let mut line = Vec::new();
    if start > 0 && reader.read_until(b'\n', &mut line).is_err() {
        return;
    }
    let mut seen: BTreeSet<MessageId> = BTreeSet::new();
    let mut started: BTreeSet<String> = BTreeSet::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        if !seen.is_empty() && answered(format, text) {
            tails.read.append(&mut seen);
        }
        for id in marks(format, text) {
            if unread.contains(&id) {
                tails.shown.insert(id);
                if !tails.read.contains(&id) {
                    seen.insert(id);
                }
            }
        }
        if format == Format::Codex {
            match turn_event(text, wanted) {
                Some(TurnEvent::Started(turn)) => {
                    started.insert(turn);
                }
                Some(TurnEvent::Aborted(turn)) if started.contains(&turn) => {
                    tails.aborted.insert(turn);
                }
                _ => {}
            }
        }
    }
}

/// The message ids one session record line marks as handed to the session: each
/// `[crazyagent msg <digits>]` that starts a line of a text the session was given
/// ([`given`]), outside a task notification. A mark the agent or a tool wrote, or one
/// quoted in a letter's body, starts no such line and counts for nothing (SECAUDIT-2-F3).
/// Takes the format and the line. Returns the ids.
fn marks(format: Format, text: &str) -> Vec<MessageId> {
    let mut found = Vec::new();
    if !text.contains(MARK) {
        return found;
    }
    let Ok(record) = serde_json::from_str::<serde_json::Value>(text) else {
        return found;
    };
    let mut texts = Vec::new();
    if let Some(input) = given(format, &record) {
        strings(input, &mut texts);
    }
    for text in texts {
        if text.starts_with(NOTIFICATION) {
            continue;
        }
        for line in text.split(crate::team::events::line_break) {
            let Some(rest) = line.strip_prefix(MARK) else {
                continue;
            };
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0
                && digits <= 18
                && rest.as_bytes().get(digits) == Some(&b']')
                && let Ok(id) = rest[..digits].parse::<MessageId>()
            {
                found.push(id);
            }
        }
    }
    found
}

/// The part of one session record that is input handed to the session, never its own
/// answer or a tool's output: a Claude Code user message holding no tool result, or a
/// hook's output or queued prompt attachment; a Codex user message or its `user_message`
/// event. Takes the format and the parsed record. Returns that part, or none.
fn given(format: Format, record: &serde_json::Value) -> Option<&serde_json::Value> {
    match format {
        Format::Claude => match record["type"].as_str()? {
            "user" => {
                let content = &record["message"]["content"];
                let tool = content
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|part| part["type"] == "tool_result"));
                (!tool).then_some(content)
            }
            "attachment" => {
                let kind = record["attachment"]["type"].as_str()?;
                (kind == "queued_command" || kind.starts_with("hook_"))
                    .then_some(&record["attachment"])
            }
            _ => None,
        },
        Format::Codex => {
            let payload = &record["payload"];
            match (record["type"].as_str()?, payload["type"].as_str()?) {
                ("response_item", "message") if payload["role"] == "user" => {
                    Some(&payload["content"])
                }
                ("event_msg", "user_message") => Some(&payload["message"]),
                _ => None,
            }
        }
    }
}

/// Collects every string inside a JSON value, in document order. Takes the value and the
/// list to extend.
fn strings<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(text) => out.push(text),
        serde_json::Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
        serde_json::Value::Object(fields) => fields.values().for_each(|field| strings(field, out)),
        _ => {}
    }
}

/// Says whether a path is a real folder, not a link to one.
fn real_folder(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// Creates one folder, private to the engine's user where the platform has modes, and
/// checks it is a real folder afterwards. Takes the path. Returns Ok or
/// `coord_relay_unwritable`.
fn private_folder(path: &Path) -> Result<(), EngineError> {
    #[cfg(unix)]
    let created =
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(path);
    #[cfg(not(unix))]
    let created = std::fs::DirBuilder::new().create(path);
    match created {
        Ok(()) => {}
        Err(problem) if problem.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(failure("coord_relay_unwritable")),
    }
    if !real_folder(path) {
        return Err(failure("coord_relay_unwritable"));
    }
    Ok(())
}

/// The longest start of a text within `bytes` bytes that ends on a character boundary.
pub(crate) fn clip(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A text on one line: every run of `line_break` characters and tabs becomes one space.
fn one_line(text: &str) -> String {
    text.split(|c: char| crate::team::events::line_break(c) || c == '\t')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(all(test, unix))]
mod tests;
