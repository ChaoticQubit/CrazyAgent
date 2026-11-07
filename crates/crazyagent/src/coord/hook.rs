use super::relay::{
    self, ANSWER, ANSWER_BYTES_MAX, ASK, ASKED, Answer, Ask, Held, IDLE, INBOX, INBOX_MAX,
    LEAD_TAG, Letter, Part, REQUEST_WAIT, SHOWN, TOOLS, TOOLS_LOG_BYTES_MAX, TOUCH, WAITING,
};
use super::settings::{Nudges, Overlaps};
use super::{TOOL_BYTES_MAX, Waiting};
use crate::team::tool::{Mailbox, call_id};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// The most hook input read: a Write call carries the whole file it writes.
const INPUT_BYTES_MAX: u64 = 16 * 1024 * 1024;
/// The most characters one delivery hands a session, below Claude Code's 10,000
/// character cap on `additionalContext` and plain hook output.
const CONTEXT_CHARS_MAX: usize = 9000;
/// The most characters of letters one delivery hands a session, leaving room for the header.
pub(crate) const LETTERS_CHARS_MAX: usize = CONTEXT_CHARS_MAX - 400;
const ASK_WAIT: Duration = Duration::from_secs(8);
const ASK_POLL: Duration = Duration::from_millis(50);
const TEAM_FOLDERS_MAX: usize = 16;
const CONFIG_BYTES_MAX: u64 = 256 * 1024;
const MEMBERS_MAX: usize = 64;
const GRAPHIFY: &str = "graphify";
const GRAPHED: &str = ".graphed";
const NUDGED: &str = ".nudged";

const _: () = assert!(CONTEXT_CHARS_MAX < 10_000 && CONTEXT_CHARS_MAX > 2 * super::BODY_BYTES_MAX);
const _: () = assert!(ASK_WAIT.as_secs() < Event::Edit.timeout_seconds() as u64);
/// How long a hook or monitor waits for the engine to name the pane it runs in.
const PANE_WAIT: Duration = Duration::from_secs(3);
const _: () = assert!(
    PANE_WAIT.as_secs() < Event::Asked.timeout_seconds() as u64
        && PANE_WAIT.as_secs() < Event::Notification.timeout_seconds() as u64
        && PANE_WAIT.as_secs() < Event::Prompt.timeout_seconds() as u64
        && PANE_WAIT.as_secs() + ASK_WAIT.as_secs() < Event::Edit.timeout_seconds() as u64
);
const _: () = assert!(
    PANE_WAIT.as_millis() + REQUEST_WAIT.as_millis()
        < 1000 * Event::Asked.timeout_seconds() as u128
        && PANE_WAIT.as_millis() + ASK_WAIT.as_millis() + REQUEST_WAIT.as_millis()
            < 1000 * Event::Edit.timeout_seconds() as u128
);
/// Why an edit of a session the engine placed in no pane is held (DEC-111 Q18).
const UNNAMED_EDIT: &str = "CrazyAgent: the engine could not tell which agent this session is, so this edit is held. Try again in a moment.";

/// The Claude Code hook events the relay runs on, each named by its `coord-hook`
/// argument: after every tool call, at a stop, before AskUserQuestion, on a permission
/// prompt or elicitation notification, on a submitted prompt, and before a file edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    PostTool,
    Stop,
    Asked,
    Notification,
    Prompt,
    Edit,
}

impl Event {
    /// The argument naming this event. Takes the event. Returns its fixed text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PostTool => "post-tool",
            Self::Stop => "stop",
            Self::Asked => "asked",
            Self::Notification => "notification",
            Self::Prompt => "prompt",
            Self::Edit => "edit",
        }
    }

    /// Reads an event argument. Takes the text. Returns the event, or None.
    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::PostTool,
            Self::Stop,
            Self::Asked,
            Self::Notification,
            Self::Prompt,
            Self::Edit,
        ]
        .into_iter()
        .find(|event| event.as_str() == text)
    }

    /// The `timeout` the plugin gives this hook, in seconds: long enough for the edit
    /// guard's question to the engine, short for the rest.
    pub const fn timeout_seconds(self) -> u32 {
        match self {
            Self::Edit => 15,
            Self::PostTool | Self::Stop => 10,
            Self::Asked | Self::Notification | Self::Prompt => 5,
        }
    }
}

/// Who a hook or monitor runs for, as the engine places it: the tag whose messages the
/// session's main thread takes (none inside a subagent or in-process teammate), the tag its
/// activity counts for, and, for a native teammate, its name, whose `.worktrees/<name>`
/// its edits must stay in (Q17); `unnamed` when the engine placed the process nowhere, so
/// it is nobody (DEC-111 Q18); and the part of the mailbox its entries sit in (relay
/// mailbox forgery C1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub inbox: Option<String>,
    pub activity: Option<String>,
    pub teammate: Option<String>,
    pub cwd: Option<PathBuf>,
    pub unnamed: bool,
    pub part: Part,
}

/// One in-process member of a Claude Code agent team config: its name, `agentId` and
/// folder.
struct Member {
    name: String,
    agent: String,
    cwd: Option<String>,
}

/// Runs one relay hook (K2a) for Claude Code: reads the hook input JSON, writes the JSON
/// answer when there is one, and never makes the harness fail: an unreadable mailbox or
/// input means no answer. PostToolUse hands waiting messages over as `additionalContext`
/// and logs the tool; Stop blocks with waiting messages as the `reason`, and a native
/// teammate that stops with none left rests until its next tool call or prompt;
/// AskUserQuestion and permission or elicitation notifications set Waiting; a user prompt
/// clears it; an edit is denied outside a teammate's own worktree or, under Overlaps Block,
/// on a file another active agent claimed, and under the FirstEdit nudges a session's first
/// edit before any code-graph query gets one reminder. A native teammate's session does
/// all of it over its lead's caller socket instead of the mailbox (relay over the caller
/// socket, OQ-7 B). Inputs: the event argument, the mailbox, the input and the output.
/// Output: Ok, or the usage text for an unknown event.
pub fn run(
    event: &str,
    mailbox: &Path,
    input: impl Read,
    mut output: impl Write,
) -> Result<(), String> {
    let Some(event) = Event::parse(event) else {
        return Err(format!(
            "usage: crazyagent {} post-tool|stop|asked|notification|prompt|edit MAILBOX_DIR",
            relay::HOOK_COMMAND
        ));
    };
    let mut bytes = Vec::new();
    if input
        .take(INPUT_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > INPUT_BYTES_MAX
    {
        return Ok(());
    }
    let Ok(input) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    let Ok(root) = Mailbox::open(mailbox) else {
        return Ok(());
    };
    let who = identify(mailbox, input["agent_id"].as_str());
    let Ok(folder) = relay::part_folder(root, who.part) else {
        return Ok(());
    };
    let answer = if who.part == Part::Teammates && who.activity.is_some() {
        teammate(event, mailbox, &folder, &who, &input)
    } else {
        in_mailbox(event, &folder, &who, &input)
    };
    if let Some(answer) = answer {
        let _ = writeln!(output, "{answer}");
        let _ = output.flush();
    }
    Ok(())
}

/// Runs one relay hook through the session's part of the mailbox: the lead's sessions of
/// a native lead and every session of an agent that leads no native team. Takes the event,
/// the folder, the identity and the hook input. Returns the answer, or None for none.
fn in_mailbox(event: Event, folder: &Mailbox, who: &Identity, input: &Value) -> Option<Value> {
    let mate = who.teammate.as_ref().and(who.activity.as_deref());
    match event {
        Event::PostTool => {
            if let Some(tag) = &who.activity {
                settle(folder, tag);
                log_tools(folder, tag, input);
            }
            if let Some(tag) = mate {
                wake(folder, tag);
            }
            who.inbox.as_deref().and_then(|tag| {
                deliver(folder, tag).map(|text| {
                    json!({"hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "additionalContext": text,
                    }})
                })
            })
        }
        Event::Stop => {
            if let Some(tag) = &who.activity {
                settle(folder, tag);
                touch(folder, tag);
            }
            let held = who.inbox.as_deref().and_then(|tag| deliver(folder, tag));
            if held.is_none()
                && let Some(tag) = mate
            {
                rest(folder, tag);
            }
            held.map(|text| json!({"decision": "block", "reason": text}))
        }
        Event::Asked => {
            if let Some(tag) = &who.activity {
                wait_on(folder, tag, Waiting::Question);
            }
            None
        }
        Event::Notification => {
            if let (Some(tag), Some(waiting)) = (&who.activity, notified(input)) {
                wait_on(folder, tag, waiting);
            }
            None
        }
        Event::Prompt => {
            if let Some(tag) = mate {
                wake(folder, tag);
            }
            if let Some(tag) = who.activity.as_deref().filter(|_| typed(input)) {
                settle(folder, tag);
                touch(folder, tag);
            }
            None
        }
        Event::Edit => edit_answer(folder, None, who, input),
    }
}

/// Runs one relay hook of a native teammate's session over its lead's caller socket, never
/// through the mailbox, which every teammate writes (relay over the caller socket, OQ-7 B):
/// each event is one request the engine carries out for the pane it finds this process in,
/// and its letters, overlap answer, first-edit reminder and Task-start reminder come back on
/// the same connection. Only the roster the edit guard reads stays a file, the engine's own in the
/// lead's part. Takes the event, the mailbox, the teammates' folder, the identity and the
/// hook input. Returns the answer, or None for none.
fn teammate(
    event: Event,
    mailbox: &Path,
    folder: &Mailbox,
    who: &Identity,
    input: &Value,
) -> Option<Value> {
    let own = if who.inbox.is_some() { "own" } else { "sub" };
    let text = |bytes: Vec<u8>| {
        String::from_utf8(bytes)
            .ok()
            .filter(|text| !text.is_empty() && text.chars().count() <= CONTEXT_CHARS_MAX)
    };
    match event {
        Event::PostTool => {
            let mut line = format!("post {own}");
            for tool in tool_lines(input).0 {
                line.push(' ');
                line.push_str(&tool);
            }
            request(mailbox, &line, REQUEST_WAIT)
                .and_then(text)
                .map(|text| {
                    json!({"hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "additionalContext": text,
                    }})
                })
        }
        Event::Stop => request(mailbox, &format!("stop {own}"), REQUEST_WAIT)
            .and_then(text)
            .map(|text| json!({"decision": "block", "reason": text})),
        Event::Asked => {
            request(mailbox, "wait question", REQUEST_WAIT);
            None
        }
        Event::Notification => {
            if let Some(waiting) = notified(input) {
                request(mailbox, &format!("wait {}", waiting.as_str()), REQUEST_WAIT);
            }
            None
        }
        Event::Prompt => {
            let kind = if typed(input) { "typed" } else { "other" };
            request(mailbox, &format!("prompt {kind}"), REQUEST_WAIT)
                .and_then(text)
                .map(|text| {
                    json!({"hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
                        "additionalContext": text,
                    }})
                })
        }
        Event::Edit => edit_answer(folder, Some(mailbox), who, input),
    }
}

/// Sends one relay request over the lead's caller socket and waits up to `wait` for the
/// engine's answer (relay over the caller socket, OQ-7 B). Takes the mailbox, the request
/// and the wait. Returns the answer's payload, or None for a refusal or no answer, always
/// where the platform has no caller socket.
fn request(mailbox: &Path, line: &str, wait: Duration) -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        crate::team::caller::relay(mailbox, line, wait)
    }
    #[cfg(not(unix))]
    {
        let _ = (mailbox, line, wait);
        None
    }
}

/// The wait a notification shows: a permission prompt, or an elicitation as a question.
/// Takes the hook input. Returns the wait, or None for any other notification.
fn notified(input: &Value) -> Option<Waiting> {
    match input["notification_type"].as_str() {
        Some("permission_prompt") => Some(Waiting::Permission),
        Some("elicitation_dialog" | "elicitation_url_dialog") => Some(Waiting::Question),
        _ => None,
    }
}

/// Whether a submitted prompt is the user's, not one the harness typed itself. Takes the
/// hook input. Returns the answer.
fn typed(input: &Value) -> bool {
    matches!(input["source"].as_str(), None | Some("user" | "sdk"))
}

/// Finds who a hook or monitor runs for, from the engine's answer alone (DEC-111 Q18,
/// RH-2). The engine names the pane the asking process runs in from the process the kernel
/// says connected, never from `TMUX_PANE`, and who runs there from its own rows, never from
/// the team config, which every agent of the team can write. In the lead's pane, or for an
/// agent that leads no native team, the session is the lead, an in-process teammate whose
/// `agentId` the hook input carries as the config lists it, or a subagent of the lead. In
/// a teammate's pane it is that teammate under the engine's name for it, or a subagent of
/// it; a process the engine places nowhere is nobody, its edits held. A native lead's
/// sessions use the lead's part of the mailbox and its teammates' the mailbox itself; an
/// agent leading no native team keeps the one folder. Inputs: the mailbox and the hook's
/// `agent_id`, none for the monitor. Output: the identity.
pub(crate) fn identify(mailbox: &Path, agent: Option<&str>) -> Identity {
    #[cfg(unix)]
    let seat = {
        use crate::team::caller::{Place, Whom};
        match crate::team::caller::own_place(mailbox, PANE_WAIT) {
            Place::Alone => Some((Part::Single, None)),
            Place::Placed(_, Whom::Lead) => Some((Part::Lead, None)),
            Place::Placed(pane, Whom::Teammate(name)) => {
                Some((Part::Teammates, Some((pane, name))))
            }
            Place::Unplaced => None,
        }
    };
    #[cfg(not(unix))]
    let seat: Option<(Part, Option<(String, String)>)> = {
        let _ = mailbox;
        Some((Part::Single, None))
    };
    let Some((part, seat)) = seat else {
        return Identity {
            inbox: None,
            activity: None,
            teammate: None,
            cwd: None,
            unnamed: true,
            part: Part::Teammates,
        };
    };
    let in_process = |agent: &str| {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
            .and_then(|config| {
                in_process_members(&config)
                    .into_iter()
                    .find(|member| member.agent == agent)
            })
    };
    let identity = match (seat, agent) {
        (None, None) => Identity {
            inbox: Some(LEAD_TAG.to_owned()),
            activity: Some(LEAD_TAG.to_owned()),
            teammate: None,
            cwd: None,
            unnamed: false,
            part,
        },
        (None, Some(agent)) => match in_process(agent) {
            Some(member) => Identity {
                inbox: None,
                activity: None,
                teammate: Some(member.name),
                cwd: member.cwd.map(PathBuf::from),
                unnamed: false,
                part,
            },
            None => Identity {
                inbox: None,
                activity: Some(LEAD_TAG.to_owned()),
                teammate: None,
                cwd: None,
                unnamed: false,
                part,
            },
        },
        (Some((pane, name)), agent) => {
            let own = agent
                .is_none_or(|agent| in_process(agent).is_some_and(|member| member.name == name));
            Identity {
                inbox: own.then(|| pane.clone()),
                activity: Some(pane),
                teammate: Some(name),
                cwd: None,
                unnamed: false,
                part,
            }
        }
    };
    assert!(identity.inbox.is_none() || identity.inbox == identity.activity);
    identity
}

/// Reads the in-process members of the Claude Code agent team configs below a config
/// folder, read-only: at most `TEAM_FOLDERS_MAX` `teams/<team>/config.json` files of at
/// most `CONFIG_BYTES_MAX` bytes, each a plain file. The files are the agents' own, so
/// every field is untrusted and bounded, and they serve only to name an in-process
/// teammate by the `agentId` its hook input carries; members whose name names no worktree
/// are left out. Takes the config folder. Returns the members.
fn in_process_members(config: &Path) -> Vec<Member> {
    let mut found = Vec::new();
    for root in [config.join("teams"), config.join(".claude").join("teams")] {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten().take(TEAM_FOLDERS_MAX) {
            let path = entry.path().join("config.json");
            let Some(text) = crate::platform::read_plain_file(&path, CONFIG_BYTES_MAX) else {
                continue;
            };
            let Ok(config) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let text_of = |value: &Value, key: &str| value[key].as_str().map(str::to_owned);
            found.extend(
                config["members"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(MEMBERS_MAX)
                    .filter_map(|member| {
                        let name = text_of(member, "name")
                            .filter(|name| crate::agent_git::worktree_name_valid(name))?;
                        Some(Member {
                            agent: text_of(member, "agentId").filter(|agent| agent.len() <= 256)?,
                            cwd: text_of(member, "cwd").filter(|cwd| Path::new(cwd).is_absolute()),
                            name,
                        })
                    }),
            );
        }
    }
    found
}

/// Takes the session's waiting messages, oldest first, by renaming each letter addressed
/// to its tag to `.shown`, until the next would pass `CONTEXT_CHARS_MAX`; the rest wait
/// for the next tool call or stop. Takes the mailbox and the tag. Returns the text the
/// session reads, None when nothing waits.
fn deliver(folder: &Mailbox, tag: &str) -> Option<String> {
    let mut taken: Vec<Letter> = Vec::new();
    let mut size = 0;
    for (_, stem) in folder.entries(INBOX).into_iter().take(INBOX_MAX) {
        let name = format!("{stem}{INBOX}");
        let Some(letter) = relay::read_letter(folder, &name) else {
            continue;
        };
        if letter.to != tag || relay::letter_stem(letter.id).as_deref() != Some(stem.as_str()) {
            continue;
        }
        let length = letter_chars(&letter);
        if size + length > LETTERS_CHARS_MAX {
            break;
        }
        if folder.rename(&name, &format!("{stem}{SHOWN}")).is_err() {
            continue;
        }
        size += length;
        taken.push(letter);
    }
    if taken.is_empty() {
        return None;
    }
    Some(bundle(&mut taken))
}

/// The characters one letter takes of a delivery: its text and the blank line before it.
/// Takes the letter. Returns the count.
pub(crate) fn letter_chars(letter: &Letter) -> usize {
    letter.text().chars().count() + 2
}

/// The text a session reads for the letters it takes: a header with their count and how to
/// answer, then each letter, oldest first. Takes the letters, at least one and at most
/// `LETTERS_CHARS_MAX` characters of them by `letter_chars`. Returns the text.
pub(crate) fn bundle(taken: &mut [Letter]) -> String {
    assert!(!taken.is_empty());
    taken.sort_by_key(|letter| letter.id);
    let mut text = format!(
        "CrazyAgent team messages for you ({}). Answer a question or work for you with update_work kind reply and its id in reply_to. A note takes no reply; to answer one, send kind note with to.",
        taken.len()
    );
    for letter in taken.iter() {
        text.push_str("\n\n");
        text.push_str(&letter.text());
    }
    assert!(text.chars().count() <= CONTEXT_CHARS_MAX);
    text
}

/// Marks a session waiting on its user (Q14): the latest wait replaces the last one for
/// the engine, and a marker tells a later tool call or prompt to clear it.
fn wait_on(folder: &Mailbox, tag: &str, waiting: Waiting) {
    let Some(key) = relay::tag_key(tag) else {
        return;
    };
    let _ = relay::put(
        folder,
        &format!("{key}{WAITING}"),
        waiting.as_str().as_bytes(),
    );
    let _ = folder.create(&format!("{key}{ASKED}"));
}

/// Clears a session's wait once it moves on: only when its marker was there, so a
/// session that never waited writes nothing.
fn settle(folder: &Mailbox, tag: &str) {
    let Some(key) = relay::tag_key(tag) else {
        return;
    };
    if folder.remove(&format!("{key}{ASKED}")).is_ok() {
        let _ = relay::put(folder, &format!("{key}{WAITING}"), b"none");
    }
}

/// Records activity of a session for the engine; one waiting touch is enough.
fn touch(folder: &Mailbox, tag: &str) {
    if let Some(key) = relay::tag_key(tag) {
        let _ = folder.create(&format!("{key}{TOUCH}"));
    }
}

/// Marks a native teammate's session resting for the engine, which shows its row Idle
/// while the mark stays: its turn ended with nothing left to hand it.
fn rest(folder: &Mailbox, tag: &str) {
    if let Some(key) = relay::tag_key(tag) {
        let _ = folder.create(&format!("{key}{IDLE}"));
    }
}

/// Clears a native teammate's resting mark once a tool call or prompt shows it works again.
fn wake(folder: &Mailbox, tag: &str) {
    if let Some(key) = relay::tag_key(tag) {
        let _ = folder.remove(&format!("{key}{IDLE}"));
    }
}

/// Appends the tool a session called to its tool log in one write, the names
/// `tool_lines` gives, one a line, and marks a code-graph query. Nothing is logged past
/// `TOOLS_LOG_BYTES_MAX` or for a name that is not plain.
fn log_tools(folder: &Mailbox, tag: &str, input: &Value) {
    let Some(key) = relay::tag_key(tag) else {
        return;
    };
    let (names, graphed) = tool_lines(input);
    if names.is_empty() {
        return;
    }
    if graphed {
        let _ = folder.create(&format!("{key}{GRAPHED}"));
    }
    let mut lines = String::new();
    for name in names {
        lines.push_str(&name);
        lines.push('\n');
    }
    let Ok(mut file) = folder.append(&format!("{key}{TOOLS}")) else {
        return;
    };
    if file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= TOOLS_LOG_BYTES_MAX)
    {
        let _ = file.write_all(lines.as_bytes());
    }
}

/// The names one tool call counts under: the tool name, for a command line that runs
/// graphify anywhere in it (relay B F1) or the skill `graphify` too (Q18), and for a skill
/// `skill:` and its name, so each skill counts by name (F7), and whether the call queried
/// the code graph. Takes the hook input. Returns the names, none for a tool name that is
/// not plain, and the answer.
fn tool_lines(input: &Value) -> (Vec<String>, bool) {
    let Some(tool) = input["tool_name"].as_str().filter(|tool| {
        !tool.is_empty()
            && tool.len() <= TOOL_BYTES_MAX
            && !tool.chars().any(|c| c.is_control() || c.is_whitespace())
    }) else {
        return (Vec::new(), false);
    };
    let mut names = vec![tool.to_owned()];
    let graphify = match tool {
        "Bash" => {
            super::adoption::tool_key(tool, &input["tool_input"]).as_deref() == Some(GRAPHIFY)
        }
        "Skill" => input["tool_input"]["skill"]
            .as_str()
            .is_some_and(|skill| skill == GRAPHIFY || skill.ends_with(":graphify")),
        _ => false,
    };
    if graphify {
        names.push(GRAPHIFY.to_owned());
    }
    let graphed = graphify
        || super::adoption::tool_key(tool, &input["tool_input"]).as_deref() == Some(GRAPHIFY);
    if let Some(skill) = super::adoption::skill_key(tool, &input["tool_input"]) {
        names.push(skill);
    }
    assert!(names.len() <= 3);
    (names, graphed)
}

/// Answers one file edit before it runs: the guard's denial when it refuses the edit, else,
/// under the FirstEdit nudges, the first-edit reminder as `additionalContext` with no
/// permission decision, so the harness's own permission rules still decide the edit. Takes
/// the session's folder, the mailbox whose caller socket a native teammate asks the engine
/// over (None to ask through the folder), the identity and the hook input. Returns the
/// answer, or None for none.
fn edit_answer(
    folder: &Mailbox,
    socket: Option<&Path>,
    who: &Identity,
    input: &Value,
) -> Option<Value> {
    if let Some(reason) = guard(folder, socket, who, input) {
        return Some(json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }}));
    }
    first_edit(folder, socket, who).map(|text| {
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": text,
        }})
    })
}

/// Hands a session the first-edit reminder once (DEC-111 R4): only under the project's
/// FirstEdit nudges, only while the session made no code-graph query (`log_tools` marks
/// one), and only the first time, which an exclusive marker in the mailbox records; a
/// native teammate's comes from the engine, which decides it from its own rows (relay over
/// the caller socket, OQ-7 B). Takes the folder, the mailbox whose caller socket a native
/// teammate asks over, and the identity. Returns the reminder, or None.
fn first_edit(folder: &Mailbox, socket: Option<&Path>, who: &Identity) -> Option<&'static str> {
    if let Some(mailbox) = socket {
        let said = request(mailbox, "nudge", REQUEST_WAIT)?;
        let reminder = super::adoption::FIRST_EDIT_REMINDER;
        return (said == reminder.as_bytes()).then_some(reminder);
    }
    if relay::read_roster(folder, who.part)?.nudges != Nudges::FirstEdit {
        return None;
    }
    let key = relay::tag_key(who.activity.as_deref()?)?;
    if folder.read(&format!("{key}{GRAPHED}")).is_ok() {
        return None;
    }
    folder.create(&format!("{key}{NUDGED}")).ok()?;
    Some(super::adoption::FIRST_EDIT_REMINDER)
}

/// Decides one file edit before it runs. A session the engine placed in no pane may not
/// edit (DEC-111 Q18). A native teammate may edit only inside
/// `<lead worktree>/.worktrees/<its name>/` (Q17); under Overlaps Block or Wait, an edit to
/// a repository file another active agent claimed is refused after asking the engine (Q15,
/// OS-8).
/// The guard is cooperative: the session owns its home and mailbox. Takes the folder, the
/// mailbox whose caller socket a native teammate asks over, the identity and the hook
/// input. Returns the reason to deny, or None to let it run.
fn guard(folder: &Mailbox, socket: Option<&Path>, who: &Identity, input: &Value) -> Option<String> {
    if who.unnamed {
        return Some(UNNAMED_EDIT.to_owned());
    }
    let tool_input = &input["tool_input"];
    let path = tool_input["file_path"]
        .as_str()
        .or_else(|| tool_input["notebook_path"].as_str())
        .map(Path::new)
        .filter(|path| path.is_absolute())?;
    let target = resolved(path);
    let roster = relay::read_roster(folder, who.part);
    let root = roster
        .as_ref()
        .and_then(|roster| roster.root.as_ref().map(PathBuf::from))
        .or_else(|| who.cwd.clone())
        .or_else(|| std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from))
        .filter(|root| root.is_absolute())
        .map(|root| resolved(&root));
    let base = match (&who.teammate, &root) {
        (Some(name), Some(root)) => {
            let own = resolved(&root.join(crate::agent_git::TEAMMATES_FOLDER).join(name));
            if !target.starts_with(&own) {
                return Some(format!(
                    "CrazyAgent: you are teammate {name}. Edit files only inside your own worktree {own}, on your own branch <task branch>--{name}, which CrazyAgent makes for you when you start. If it does not exist yet, wait a few seconds and try again; do not make it yourself.",
                    own = own.display(),
                ));
            }
            own
        }
        (None, Some(root)) => root.clone(),
        (_, None) => return None,
    };
    let option = roster
        .as_ref()
        .map(|roster| roster.overlaps)
        .filter(|overlaps| matches!(overlaps, Overlaps::Block | Overlaps::Wait));
    let tag = who.activity.as_deref()?;
    let option = option?;
    let relative = target.strip_prefix(&base).ok()?.to_str()?;
    if relative.is_empty()
        || relative.starts_with(".git/")
        || relative.starts_with(&format!("{}/", crate::agent_git::TEAMMATES_FOLDER))
    {
        return None;
    }
    let held = match socket {
        Some(mailbox) => request(mailbox, &format!("ask {relative}"), ASK_WAIT)
            .and_then(|bytes| serde_json::from_slice::<Answer>(&bytes).ok())
            .map(|answer| answer.held)
            .unwrap_or_default(),
        None => ask(folder, tag, relative),
    };
    if held.is_empty() {
        return None;
    }
    let holders: Vec<String> = held
        .iter()
        .map(|held| format!("{} ({})", held.by, held.pattern))
        .collect();
    let advice = match option {
        Overlaps::Wait => {
            "set to Wait in this project: start_work on it to get a message when it is free, or start_work on other files."
        }
        _ => {
            "set to Block in this project: message the holder with update_work, or start_work on other files."
        }
    };
    Some(format!(
        "CrazyAgent: {relative} is claimed by {}. Overlapping edits are {advice}",
        holders.join(", ")
    ))
}

/// Asks the engine which other active agents claim a file and waits up to `ASK_WAIT`
/// for its answer; with no answer in time the question is withdrawn and the edit runs.
/// Takes the mailbox, the tag and the repository-relative path. Returns the claims.
fn ask(folder: &Mailbox, tag: &str, path: &str) -> Vec<Held> {
    let Ok(id) = call_id() else {
        return Vec::new();
    };
    let Ok(bytes) = serde_json::to_vec(&Ask {
        to: tag.to_owned(),
        path: path.to_owned(),
    }) else {
        return Vec::new();
    };
    if bytes.len() as u64 > relay::ASK_BYTES_MAX
        || relay::put(folder, &format!("{id}{ASK}"), &bytes).is_err()
    {
        return Vec::new();
    }
    let answered = format!("{id}{ANSWER}");
    let deadline = Instant::now() + ASK_WAIT;
    while Instant::now() < deadline {
        if let Some(bytes) = relay::read_entry(folder, &answered, ANSWER_BYTES_MAX) {
            let _ = folder.remove(&answered);
            return serde_json::from_slice::<Answer>(&bytes)
                .map(|answer| answer.held)
                .unwrap_or_default();
        }
        std::thread::sleep(ASK_POLL);
    }
    let _ = folder.remove(&format!("{id}{ASK}"));
    Vec::new()
}

/// A path with `.` and `..` resolved by name, then its longest existing start resolved by
/// the file system, so a link inside it cannot point the check elsewhere. Takes an
/// absolute path. Returns the resolved path.
fn resolved(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other),
        }
    }
    let mut base = clean.clone();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = base.canonicalize() {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (base.file_name().map(ToOwned::to_owned), base.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                base = parent.to_path_buf();
            }
            _ => return clean,
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
