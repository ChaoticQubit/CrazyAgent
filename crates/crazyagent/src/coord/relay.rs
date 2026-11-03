use super::hook::Event;
use super::settings::{self, Delivery, HarnessKind, Nudges, Overlaps};
use super::store::{failure, message, state};
use super::{
    AgentId, BODY_BYTES_MAX, MessageId, MessageKind, PENDING_MAX, Party, QuestionFor, Receipt,
    Severity, TOOL_BYTES_MAX, Waiting, WorkId,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::team::AgentStatus;
use crate::team::tool::{Mailbox, call_id, call_id_valid};
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
pub(crate) const ANSWER_BYTES_MAX: u64 = 64 * 1024;
/// The longest tool log one session keeps before the engine takes it; past it, tool calls
/// are not counted until the next tick.
pub(crate) const TOOLS_LOG_BYTES_MAX: u64 = 32 * 1024;
pub(crate) const WAITING_BYTES_MAX: u64 = 16;
pub(crate) const PREVIEW_BYTES: usize = 200;
const PLUGIN_NAME: &str = "crazyagent-relay";
const MONITOR_NAME: &str = "crazyagent-inbox";
const ROSTER_PART: &str = "relay.json.part";
const TOOLS_TAKEN: &str = ".ttaken";
const WAITING_TAKEN: &str = ".wtaken";
const ASK_TAKEN: &str = ".asking";
const PART: &str = ".rpart";
const ASKS_PER_TICK_MAX: usize = 8;
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
    /// message it is and the full body. Takes the letter. Returns the text.
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
        format!(
            "{MARK}{}] from {}{about}{work}:\n{}",
            self.id, self.from, self.body
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
/// relative to the repository root, is claimed by another active agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ask {
    pub to: String,
    pub path: String,
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
    let plain = |path: &Path| {
        path.is_absolute()
            .then(|| path.to_str())
            .flatten()
            .filter(|text| {
                !text
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\'' | '$'))
            })
            .map(str::to_owned)
    };
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
/// no native team, or, for a native lead, the lead's sessions in [`LEAD_PART`] apart from
/// its teammates' in the mailbox itself (relay mailbox forgery C1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Single,
    Split,
}

/// Runs one engine tick of an agent's relay: refreshes `relay.json`, marks Delivered each
/// message a session took, drops letters no longer waiting, writes the next waiting
/// messages of the agent and its native teammates (none under the Typing only delivery
/// setting, nor for a master, which K2b feeds through its engine input), applies the
/// sessions' tool counts, activity and waits and each native teammate's Idle or Working,
/// answers their overlap
/// questions, and, when `receipts` is set, marks Read each delivered message its session
/// answered after (from the home's newest transcripts). Everything a session wrote is
/// validated; nothing from the mailbox is trusted. Inputs: the database, the project, the
/// agent, its home, its mailbox, whether to scan transcripts and the time in ms. Output:
/// Ok, or a SQLite failure; an unreadable mailbox is skipped.
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
    let _ = layout;
    let Ok(folder) = Mailbox::open_for_engine(mailbox) else {
        return Ok(());
    };
    let team = recipients(db, project, lead)?;
    if team.is_empty() {
        return Ok(());
    }
    let chosen = settings::read(db, project)?;
    let delivery = settings::delivery(db, project, HarnessKind::ClaudeCode)?;
    let roster = Roster {
        overlaps: chosen.overlaps,
        nudges: chosen.nudges,
        root: worktree(db, project, lead)?,
    };
    write_roster(&folder, &roster);
    let master: bool = db
        .query_row(
            "SELECT role='master' FROM team_agents WHERE id=?1",
            [lead],
            |row| row.get(0),
        )
        .map_err(state)?;
    let posts = delivery != Delivery::TypingOnly && !master;
    collect_shown(db, &folder, &team, now)?;
    sweep_letters(db, &folder, &team, posts)?;
    if posts {
        post_letters(db, &folder, &team)?;
    }
    take_activity(db, &folder, &team, now)?;
    answer_asks(db, project, &folder, &team)?;
    if receipts {
        read_receipts(db, home, &team, now)?;
    }
    Ok(())
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

/// Reads the engine's `relay.json`. Takes the mailbox. Returns the roster, or None
/// before the engine wrote one.
pub(crate) fn read_roster(folder: &Mailbox) -> Option<Roster> {
    let bytes = read_entry(folder, ROSTER_FILE, ROSTER_BYTES_MAX)?;
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
            let letter = Letter {
                id: found.id,
                to: tag.clone(),
                from: sender(db, found.from),
                kind: found.kind,
                question: found.question,
                work: found.work,
                reply_to: found.reply_to,
                body: found.body,
            };
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
            let status = if resting.contains(&key) {
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
        let mut held = Vec::new();
        if let Some(ask) = ask
            && let Some(agent) = team.get(&ask.to)
        {
            match super::overlapping(db, project, *agent, &ask.path) {
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
                Err(_) => {}
            }
        }
        let bytes = serde_json::to_vec(&Answer { held }).map_err(state)?;
        assert!(bytes.len() as u64 <= ANSWER_BYTES_MAX);
        let _ = put(folder, &format!("{stem}{ANSWER}"), &bytes);
    }
    Ok(())
}

/// Marks Read each message delivered or typed to the lead or a teammate whose mark shows
/// in one of the home's newest Claude Code transcripts (`projects/**/<session>.jsonl`)
/// with assistant output after it. Reads the last `TRANSCRIPT_TAIL_BYTES` of at most
/// `TRANSCRIPTS_READ_MAX` files, read-only and never following a link.
fn read_receipts(
    db: &Connection,
    home: &Path,
    team: &BTreeMap<String, AgentId>,
    now: i64,
) -> Result<(), EngineError> {
    let mut unread = BTreeSet::new();
    for agent in team.values() {
        let limit = i64::try_from(RECEIPTS_CHECKED_MAX).map_err(state)?;
        let mut statement = db
            .prepare(
                "SELECT id FROM coord_messages WHERE to_kind='agent' AND to_agent=?1
                 AND receipt IN ('delivered','typed') ORDER BY id DESC LIMIT ?2",
            )
            .map_err(state)?;
        let rows = statement
            .query_map(params![agent, limit], |row| row.get::<_, i64>(0))
            .map_err(state)?;
        for row in rows {
            if unread.len() < RECEIPTS_CHECKED_MAX {
                unread.insert(row.map_err(state)?);
            }
        }
    }
    if unread.is_empty() {
        return Ok(());
    }
    let mut read = BTreeSet::new();
    let records = crate::usage::capture::records(home, crate::usage::capture::Format::Claude);
    for path in records.into_iter().take(TRANSCRIPTS_READ_MAX) {
        scan_transcript(&path, &unread, &mut read);
    }
    for id in read {
        advance(db, id, Receipt::Read, now)?;
    }
    Ok(())
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

/// Finds in one transcript's tail the unread ids whose mark is followed by an assistant
/// record. Takes the transcript, the unread ids and the set to add read ids to.
fn scan_transcript(path: &Path, unread: &BTreeSet<MessageId>, read: &mut BTreeSet<MessageId>) {
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
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        if !seen.is_empty()
            && text.contains("\"assistant\"")
            && serde_json::from_str::<serde_json::Value>(text)
                .is_ok_and(|record| record["type"] == "assistant")
        {
            read.append(&mut seen);
        }
        for id in marks(text) {
            if unread.contains(&id) && !read.contains(&id) {
                seen.insert(id);
            }
        }
    }
}

/// The message ids marked in one line of text: each `[crazyagent msg <digits>]`.
fn marks(text: &str) -> Vec<MessageId> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(MARK) {
        rest = &rest[at + MARK.len()..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0
            && digits <= 18
            && rest.as_bytes().get(digits) == Some(&b']')
            && let Ok(id) = rest[..digits].parse::<MessageId>()
        {
            found.push(id);
        }
    }
    found
}

/// Says whether a path is a real folder, not a link to one.
fn real_folder(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// Creates one folder, private to the engine's user where the platform has modes, and
/// checks it is a real folder afterwards. Takes the path. Returns Ok or
/// `coord_relay_unwritable`.
fn private_folder(path: &Path) -> Result<(), EngineError> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
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

/// A text on one line: every run of line breaks and tabs becomes one space.
fn one_line(text: &str) -> String {
    text.split(['\n', '\r', '\t'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(all(test, unix))]
mod tests;
