use super::project::{ACTIVITY_MAX, ACTIVITY_SHOWN, MasterStatus, Message, Speaker, Transcript};
use super::project_markdown::sanitize;
use super::project_time::{clock, duration};
use crate::conversation::ConversationEntry;
use crate::master_session::{
    MasterActivityLine, MasterReason, MasterSessionPhase, MasterTurnPhase, MasterTurnView,
};
use crate::master_sessions::{MasterMode, MessageSource, MessageSpeaker, SessionMessage};
use crate::master_step_limit::steps_of_cause;
use crate::master_time_limit::minutes_of_cause;

pub(in crate::tui) const TURNS_SHOWN_MAX: usize = 200;
pub(in crate::tui) const ACTIVITY_QUIET_MS: i64 = 60_000;

/// The engine's master conversation for one project, as read through the owner connection.
/// Holds the session phase (none before the master is opened), its turns in order, the owner's
/// messages and every stored message of the session, empty when the client does not hold them all.
/// The Conversation tab turns it into a transcript with `transcript_from`.
pub(in crate::tui) struct MasterRead<'a> {
    pub session: Option<MasterSessionPhase>,
    pub turns: &'a [MasterTurnView],
    pub inputs: &'a [ConversationEntry],
    pub messages: &'a [SessionMessage],
}

/// What an owner row of a terminal turn shows when its typed words were not read.
pub(in crate::tui) const TYPED_IN_TERMINAL: &str =
    "Typed in the master's terminal. The words were not read.";

/// What marks a stored message whose words the engine did not see typed.
pub(in crate::tui) const FROM_RECORD: &str = "read from the harness record";
/// What leads the owner's words in the transcript when they were read from the harness record.
pub(in crate::tui) const RECORD_TYPED: &str =
    "Typed in the master's terminal, as the harness record has it:";

/// Whether a stored message holds words the engine did not see typed: text read from the
/// harness's own record, or a row that stands for a terminal turn an older engine only
/// counted. Input: the row. Output: whether a view marks it.
pub(in crate::tui) fn from_record(row: &SessionMessage) -> bool {
    matches!(row.source, MessageSource::Record | MessageSource::Counter)
}

/// One stored message as the transcript shows it: the owner's words, the master's reply,
/// or a notice for a request the engine typed, for the divider where another master took
/// the conversation over and for a terminal turn whose typed words were not read. Owner
/// words read from the harness record are a notice that says so, never the owner's own
/// message. Input: the row. Output: the message, or none for any other row with no text.
/// Stored text is untrusted: its terminal escapes and control characters are removed here,
/// before the transcript wraps or measures it.
fn stored_message(row: &SessionMessage) -> Option<Message> {
    let (speaker, text) = match (row.speaker, &row.text) {
        (MessageSpeaker::Owner, Some(text)) if from_record(row) => (
            Speaker::Notice,
            format!("{RECORD_TYPED} {}", sanitize(text)),
        ),
        (MessageSpeaker::Owner, Some(text)) => (Speaker::You, sanitize(text)),
        (MessageSpeaker::Owner, None) => (Speaker::Notice, TYPED_IN_TERMINAL.to_owned()),
        (MessageSpeaker::Master, Some(text)) => (Speaker::Master, sanitize(text)),
        (MessageSpeaker::Engine | MessageSpeaker::Switch, Some(text)) => {
            (Speaker::Notice, sanitize(text))
        }
        (_, None) => return None,
    };
    Some(Message {
        speaker,
        time: None,
        text,
        error: false,
    })
}

/// Adds the messages of one master turn: the owner's message, or the engine's plain line
/// for a turn it asked for, the master's reply, a notice per task the reply handed to the
/// team, the engine's note when the turn's edits wait in Changes and a plain notice for a
/// stopped turn. Inputs: the read, the turn's index in it, the master harness name, the
/// UTC offset and the messages to add to.
fn turn_messages(
    read: &MasterRead<'_>,
    index: usize,
    harness: &str,
    offset: i64,
    messages: &mut Vec<Message>,
) {
    let turn = &read.turns[index];
    let latest = index + 1 == read.turns.len();
    if let Some(entry) = read
        .inputs
        .iter()
        .find(|entry| entry.event_cursor == turn.input_cursor)
    {
        messages.push(Message {
            speaker: Speaker::You,
            time: None,
            text: entry.text.clone(),
            error: false,
        });
    } else if let Some(request) = &turn.engine_request {
        messages.push(Message {
            speaker: Speaker::Notice,
            time: None,
            text: request.line.clone(),
            error: false,
        });
    }
    if let Some(reply) = &turn.reply {
        let time = turn
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.completion.as_ref())
            .and_then(|completion| i64::try_from(completion.finished_at_ms).ok())
            .map(|at| clock(at, offset));
        messages.push(Message {
            speaker: Speaker::Master,
            time,
            text: reply.text.clone(),
            error: false,
        });
    }
    for task in &turn.tasks {
        messages.push(Message {
            speaker: Speaker::Task,
            time: None,
            text: task.line(),
            error: false,
        });
    }
    if let Some(changes) = &turn.changes {
        messages.push(Message {
            speaker: Speaker::Notice,
            time: None,
            text: changes.clone(),
            error: false,
        });
    }
    if let Some(text) = notice(turn, harness, latest) {
        messages.push(Message {
            speaker: Speaker::Notice,
            time: None,
            text,
            error: !asks_approval(turn),
        });
    }
}

/// Builds the Conversation transcript from the engine's master turns and the session's
/// stored messages. Takes the read, the master harness name, the account label, whether
/// tools are on and the UTC offset. Returns the transcript: each turn's messages as
/// `turn_messages` gives them, and, where the store holds them, what a live terminal master
/// said and the divider of each master switch between the turns. The stored rows that open
/// a one-turn turn are matched to the turns in order from the session's first message; a
/// turn with no row yet comes last and a row with no turn shows as stored. With no stored
/// message the transcript is the turns alone.
pub(in crate::tui) fn transcript_from(
    read: &MasterRead<'_>,
    harness: &str,
    account: Option<String>,
    tools: bool,
    offset: i64,
) -> Transcript {
    let start = read.turns.len().saturating_sub(TURNS_SHOWN_MAX);
    let turns = &read.turns[start..];
    let mut messages = Vec::new();
    let mut placed = 0;
    let mut plain = true;
    for row in read.messages {
        let one_turn = row.mode == MasterMode::OneTurn && row.speaker != MessageSpeaker::Switch;
        let opens = one_turn && row.speaker != MessageSpeaker::Master;
        if opens && placed < read.turns.len() {
            if placed >= start {
                turn_messages(read, placed, harness, offset, &mut messages);
            }
            placed += 1;
            plain = false;
            continue;
        }
        plain = plain || opens;
        if (one_turn && !plain) || placed < start {
            continue;
        }
        messages.extend(stored_message(row));
    }
    assert!(placed <= read.turns.len());
    for index in placed.max(start)..read.turns.len() {
        turn_messages(read, index, harness, offset, &mut messages);
    }
    assert!(
        messages.len()
            <= turns.len() * 4
                + turns.iter().map(|t| t.tasks.len()).sum::<usize>()
                + read.messages.len()
    );
    let status = match (read.session, turns.last()) {
        (None, _) => MasterStatus::NotReady,
        (
            Some(
                MasterSessionPhase::Parked
                | MasterSessionPhase::Closing
                | MasterSessionPhase::Closed,
            ),
            _,
        ) => MasterStatus::Stopped,
        (Some(MasterSessionPhase::Configured), None) => MasterStatus::Ready,
        (Some(MasterSessionPhase::Configured), Some(turn)) => status(turn),
    };
    if read
        .session
        .is_some_and(|phase| phase != MasterSessionPhase::Configured)
    {
        let text = if read.session == Some(MasterSessionPhase::Parked) {
            "This session is parked. Resume it from Sessions to continue it."
        } else {
            "The master conversation is closed."
        };
        messages.push(Message {
            speaker: Speaker::Notice,
            time: None,
            text: text.into(),
            error: false,
        });
    }
    Transcript {
        harness: harness.to_owned(),
        account,
        tools,
        open: read.session == Some(MasterSessionPhase::Configured),
        status,
        messages,
    }
}

fn status(turn: &MasterTurnView) -> MasterStatus {
    match turn.phase {
        MasterTurnPhase::Queued
        | MasterTurnPhase::Prepared
        | MasterTurnPhase::Delivering
        | MasterTurnPhase::Running => MasterStatus::Thinking {
            since_ms: turn
                .attempt
                .as_ref()
                .and_then(|attempt| i64::try_from(attempt.selected_at_ms).ok()),
            activity: newest(&turn.activity),
        },
        MasterTurnPhase::WaitingApproval => MasterStatus::NeedsYou,
        MasterTurnPhase::Blocked => match turn.reason {
            Some(MasterReason::ApprovalRequired) => MasterStatus::NeedsYou,
            Some(
                MasterReason::ReadinessUnknown
                | MasterReason::ReadinessStale
                | MasterReason::InvalidAuthentication,
            ) => MasterStatus::NotSignedIn,
            _ => MasterStatus::Stopped,
        },
        MasterTurnPhase::Succeeded => MasterStatus::Replied,
        MasterTurnPhase::Failed | MasterTurnPhase::Cancelled | MasterTurnPhase::Uncertain => {
            MasterStatus::Stopped
        }
    }
}

/// Keeps the newest activity lines of a running turn.
/// Takes the lines as the engine sent them, oldest first.
/// Returns at most `ACTIVITY_MAX` of them, oldest first.
fn newest(activity: &[MasterActivityLine]) -> Vec<MasterActivityLine> {
    let kept = activity[activity.len().saturating_sub(ACTIVITY_MAX)..].to_vec();
    assert!(kept.len() <= ACTIVITY_MAX);
    assert_eq!(kept.last(), activity.last());
    kept
}

fn age_ms(line: &MasterActivityLine, now_ms: i64) -> i64 {
    let at_ms = i64::try_from(line.at_ms).unwrap_or(i64::MAX);
    now_ms.saturating_sub(at_ms).max(0)
}

/// Words what a running turn was last seen doing, for the Thinking row.
/// Takes the turn's activity lines, oldest first, and now in milliseconds.
/// Returns the newest line with its age, or how long nothing new came once that line is
/// `ACTIVITY_QUIET_MS` old, or none without lines.
pub(in crate::tui) fn last_activity(
    activity: &[MasterActivityLine],
    now_ms: i64,
) -> Option<String> {
    let last = activity.last()?;
    let age = age_ms(last, now_ms);
    assert!(age >= 0);
    Some(if age >= ACTIVITY_QUIET_MS {
        format!("no new activity for {}", duration(age))
    } else {
        format!("{}, {} ago", last.text, duration(age))
    })
}

/// Lists a running turn's activity for the rows under the Thinking row.
/// Takes the lines, oldest first, whether the whole list is open and now in milliseconds.
/// Returns each listed line's age and text, oldest first: the newest `ACTIVITY_SHOWN`, or
/// the newest `ACTIVITY_MAX` when open.
pub(in crate::tui) fn activity_rows(
    activity: &[MasterActivityLine],
    open: bool,
    now_ms: i64,
) -> Vec<(String, String)> {
    let listed = if open { ACTIVITY_MAX } else { ACTIVITY_SHOWN };
    let rows: Vec<(String, String)> = activity[activity.len().saturating_sub(listed)..]
        .iter()
        .map(|line| {
            (
                format!("{} ago", duration(age_ms(line, now_ms))),
                line.text.clone(),
            )
        })
        .collect();
    assert!(rows.len() <= listed);
    assert!(rows.len() <= activity.len());
    rows
}

fn asks_approval(turn: &MasterTurnView) -> bool {
    turn.phase == MasterTurnPhase::WaitingApproval
        || turn.reason == Some(MasterReason::ApprovalRequired)
}

fn notice(turn: &MasterTurnView, harness: &str, latest: bool) -> Option<String> {
    let waiting = matches!(
        turn.phase,
        MasterTurnPhase::Queued
            | MasterTurnPhase::Prepared
            | MasterTurnPhase::Delivering
            | MasterTurnPhase::Running
            | MasterTurnPhase::Succeeded
    );
    if waiting {
        return None;
    }
    let code = stop_code(turn);
    if asks_approval(turn) {
        let code = turn
            .reason
            .map_or_else(String::new, |_| format!(" ({code})"));
        return latest.then(|| format!("{harness} is waiting for your approval.{code}"));
    }
    if turn.phase == MasterTurnPhase::Uncertain {
        return Some(format!(
            "It is not known whether {harness} finished this turn. ({code}) Press r to try again."
        ));
    }
    if turn.reason == Some(MasterReason::StepLimit)
        && let Some(steps) = turn.cause.as_deref().and_then(steps_of_cause)
    {
        return Some(format!(
            "Stopped after {steps} steps; reply with 'continue' to go on. ({code})"
        ));
    }
    if turn.reason == Some(MasterReason::TimeLimit)
        && let Some(minutes) = turn.cause.as_deref().and_then(minutes_of_cause)
    {
        return Some(format!(
            "Stopped after {minutes} minutes; reply with 'continue' to go on. ({code})"
        ));
    }
    let retry = if latest { " Press r to try again." } else { "" };
    if turn.reason == Some(MasterReason::RepositoryConfigRefused)
        && let Some(detail) = &turn.detail
    {
        return Some(format!("{harness} stopped: {detail} ({code}){retry}"));
    }
    let why = match turn.reason {
        Some(MasterReason::ReadinessUnknown | MasterReason::ReadinessStale) => {
            "the account is not signed in. Sign in again in Settings, Accounts."
        }
        Some(MasterReason::InvalidAuthentication) => {
            "the sign-in was rejected. Sign in again in Settings, Accounts."
        }
        Some(
            MasterReason::QuotaExhausted
            | MasterReason::QuotaUnknown
            | MasterReason::BudgetExhausted,
        ) => {
            "the account hit its usage limit. Add an account to the pool, or wait for its limit to reset."
        }
        Some(MasterReason::RateLimited | MasterReason::CooldownActive) => {
            "the account is resting after too many requests."
        }
        Some(MasterReason::TransientFailure) => "the account is unavailable for a moment.",
        Some(MasterReason::CapacityUnavailable) => "another turn is still running.",
        Some(MasterReason::ProfileBusy) => "its account is busy signing in or out.",
        Some(MasterReason::ConnectionChanged) => "its settings changed while it was open.",
        Some(MasterReason::ScopeRevoked) => "it no longer has access to this project.",
        Some(MasterReason::RuntimeUnavailable) => {
            "it could not start. Check it in Settings, Harnesses."
        }
        Some(MasterReason::CaptureIncomplete) => "its reply was cut off.",
        Some(MasterReason::NoReply) => "it ended its turn without writing a reply.",
        Some(MasterReason::StepLimit) => "it used every step its limit allows.",
        Some(MasterReason::TimeLimit) => "it reached its time limit.",
        Some(MasterReason::OutputLimit) => "this turn's output was too large to keep.",
        Some(MasterReason::UnsupportedCapability) => "it cannot lead a project yet.",
        Some(MasterReason::RepositoryMissing) => "this project has no repository yet.",
        Some(MasterReason::RepositoryConfigRefused) => {
            "a Git setting in this project's repository runs commands or redirects Git."
        }
        Some(MasterReason::InstructionsTooLarge) => {
            "the app and project instructions are too long for a Claude master. Shorten them on the Process page."
        }
        Some(MasterReason::ProtocolFault) => {
            "it sent a message out of order. Send again to start a fresh turn."
        }
        Some(MasterReason::FrameLimit) => {
            "one message was larger than the master frame size limit. Raise it in Settings, or at 1 MiB shorten the message or the instructions, then send again."
        }
        Some(MasterReason::WorktreeUncertain) => {
            "its copy of the project could not be checked. Close the master and open it again, then send again."
        }
        Some(MasterReason::Cancelled) => "you stopped this turn.",
        Some(MasterReason::ApprovalRequired) | Some(MasterReason::Unknown) | None => {
            if turn.phase == MasterTurnPhase::Cancelled {
                "you stopped this turn."
            } else {
                "something went wrong."
            }
        }
    };
    Some(format!("{harness} stopped: {why} ({code}){retry}"))
}

/// Names the engine code a stopped turn shows: the cause the engine stored, else its
/// reason, else its phase when the engine gave no specific reason, so the owner never
/// sees a bare "unknown". Takes the turn. Returns the code as the engine spells it.
fn stop_code(turn: &MasterTurnView) -> String {
    if let Some(cause) = &turn.cause {
        return cause.clone();
    }
    let value = match turn
        .reason
        .filter(|reason| *reason != MasterReason::Unknown)
    {
        Some(reason) => serde_json::to_value(reason),
        None => serde_json::to_value(turn.phase),
    }
    .expect("master reasons and phases serialize");
    let code = value
        .as_str()
        .expect("master reasons and phases serialize as names")
        .to_owned();
    assert!(!code.is_empty() && code != "unknown");
    code
}

/// Turns the engine's refusal of a master request into a plain line that keeps its code.
/// Takes the rejection code.
/// Returns what happened, ending with the code in brackets.
pub(in crate::tui) fn master_problem(code: &str) -> String {
    let plain = match code {
        "master_source_conflict" => "Settings changed since this conversation started.",
        "master_session_active" => "A master conversation is already open.",
        "master_session_missing" | "master_session_not_active" => {
            "This conversation has ended. Send again to start a new one."
        }
        "master_turn_outstanding" => "The master is still working on the last message.",
        "master_turn_capacity" => "This conversation is full. Press e to end it, then send again.",
        "master_turn_not_current" => "That turn already ended.",
        "master_session_live" => "The live session cannot be deleted.",
        "master_session_files_kept" => {
            "Its transcript could not be removed from the master's home, so nothing was deleted."
        }
        "stale_revision" => "Something changed meanwhile. Try again.",
        _ => "The engine refused it.",
    };
    format!("{plain} ({code})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationContent;
    use crate::engine::owner::OwnerProvenance;
    use crate::protocol::{Cursor, RequestId, SessionId};

    fn turn(
        ordinal: u64,
        input: u64,
        phase: MasterTurnPhase,
        reason: Option<MasterReason>,
    ) -> MasterTurnView {
        MasterTurnView {
            id: RequestId::new(format!("turn-{ordinal}")).unwrap(),
            session_id: SessionId::new("session-1").unwrap(),
            ordinal,
            input_cursor: Cursor::new(input),
            phase,
            reason,
            attempt: None,
            runtime: None,
            reply: None,
            accounts: Vec::new(),
            tasks: Vec::new(),
            changes: None,
            engine_request: None,
            activity: Vec::new(),
            detail: None,
            cause: None,
        }
    }

    fn entry(cursor: u64, text: &str) -> ConversationEntry {
        ConversationEntry {
            event_cursor: Cursor::new(cursor),
            content: ConversationContent::Message,
            text: text.into(),
            provenance: OwnerProvenance {
                origin: "tui".into(),
                owner_id: "owner".into(),
                operation_id: RequestId::new(format!("operation-{cursor}")).unwrap(),
                request_hash: "0".repeat(64),
                engine_epoch: "epoch".into(),
                admitted_epoch: 1,
                resulting_epoch: 1,
                revision_before: Cursor::new(cursor - 1),
                revision_after: Cursor::new(cursor),
                event_cursor: Some(Cursor::new(cursor)),
            },
        }
    }

    fn build(
        session: Option<MasterSessionPhase>,
        turns: &[MasterTurnView],
        inputs: &[ConversationEntry],
    ) -> Transcript {
        build_stored(session, turns, inputs, &[])
    }

    fn build_stored(
        session: Option<MasterSessionPhase>,
        turns: &[MasterTurnView],
        inputs: &[ConversationEntry],
        messages: &[SessionMessage],
    ) -> Transcript {
        let read = MasterRead {
            session,
            turns,
            inputs,
            messages,
        };
        transcript_from(&read, "Codex", Some("Main".into()), true, 0)
    }

    fn stored(
        seq: u64,
        speaker: MessageSpeaker,
        mode: MasterMode,
        text: Option<&str>,
    ) -> SessionMessage {
        SessionMessage {
            seq,
            speaker,
            mode,
            harness: Some(
                if mode == MasterMode::Live {
                    "claude-code"
                } else {
                    "rcli"
                }
                .into(),
            ),
            text: text.map(str::to_owned),
            at_ms: 1_000 + seq,
            source: match (speaker, mode) {
                (MessageSpeaker::Switch, _) => crate::master_sessions::MessageSource::Switch,
                (_, MasterMode::Live) => crate::master_sessions::MessageSource::Terminal,
                (_, MasterMode::OneTurn) => crate::master_sessions::MessageSource::Turn,
            },
        }
    }

    fn replied(ordinal: u64, input: u64, text: &str) -> MasterTurnView {
        use crate::master_session::{MasterReply, MasterReplyProvenance};
        let mut turn = turn(ordinal, input, MasterTurnPhase::Succeeded, None);
        turn.reply = Some(MasterReply {
            text: text.into(),
            provenance: MasterReplyProvenance {
                attempt_id: RequestId::new("attempt-1").unwrap(),
                runtime_ref: RequestId::new("runtime-1").unwrap(),
                capture_ref: RequestId::new("capture-1").unwrap(),
                capture_sequence: 1,
            },
        });
        turn
    }

    const SWITCHED: &str = "Switched to Claude Code. The conversation continues: Claude Code got its earlier turns, newest first, and the open tasks.";
    const BACK: &str = "Switched to One-turn master (RCLI). The conversation continues.";

    #[test]
    fn a_move_from_the_one_turn_master_to_claude_code_keeps_the_earlier_turns_above_the_divider() {
        use MasterMode::{Live, OneTurn};
        use MessageSpeaker::{Master, Owner, Switch};
        let inputs = [entry(3, "Plan the launch.")];
        let turns = [replied(1, 3, "Launch planned.")];
        let rows = [
            stored(1, Owner, OneTurn, Some("Plan the launch.")),
            stored(2, Master, OneTurn, Some("Launch planned.")),
            stored(3, Switch, Live, Some(SWITCHED)),
            stored(4, Owner, Live, None),
            stored(5, Master, Live, Some("Launch shipped.")),
        ];
        let transcript = build_stored(Some(MasterSessionPhase::Configured), &turns, &inputs, &rows);
        assert_eq!(
            said(&transcript),
            [
                (Speaker::You, "Plan the launch.", false),
                (Speaker::Master, "Launch planned.", false),
                (Speaker::Notice, SWITCHED, false),
                (Speaker::Notice, TYPED_IN_TERMINAL, false),
                (Speaker::Master, "Launch shipped.", false),
            ]
        );
    }

    #[test]
    fn a_move_from_claude_code_to_the_one_turn_master_shows_the_terminal_turns_then_the_divider() {
        use MasterMode::{Live, OneTurn};
        use MessageSpeaker::{Master, Owner, Switch};
        let inputs = [entry(3, "Now publish it.")];
        let turns = [turn(1, 3, MasterTurnPhase::Running, None)];
        let rows = [
            stored(1, Owner, Live, Some("Draft the page.")),
            stored(2, Master, Live, Some("Page drafted.")),
            stored(3, Switch, OneTurn, Some(BACK)),
        ];
        let transcript = build_stored(Some(MasterSessionPhase::Configured), &turns, &inputs, &rows);
        assert_eq!(
            said(&transcript),
            [
                (Speaker::You, "Draft the page.", false),
                (Speaker::Master, "Page drafted.", false),
                (Speaker::Notice, BACK, false),
                (Speaker::You, "Now publish it.", false),
            ],
            "the running turn the store has no row for yet comes last"
        );
        assert_eq!(
            transcript.status,
            MasterStatus::Thinking {
                since_ms: None,
                activity: Vec::new(),
            }
        );
    }

    #[test]
    fn a_session_only_the_one_turn_master_ran_reads_as_its_turns_and_a_row_with_no_turn_as_stored()
    {
        use MasterMode::OneTurn;
        use MessageSpeaker::{Engine, Master, Owner};
        let inputs = [entry(3, "Plan the page."), entry(7, "Add a theme.")];
        let turns = [
            replied(1, 3, "Page planned."),
            replied(2, 7, "Theme added."),
        ];
        let rows = [
            stored(1, Owner, OneTurn, Some("Plan the page.")),
            stored(2, Master, OneTurn, Some("Page planned.")),
            stored(3, Owner, OneTurn, Some("Add a theme.")),
            stored(4, Master, OneTurn, Some("Theme added.")),
        ];
        let alone = build(Some(MasterSessionPhase::Configured), &turns, &inputs);
        let both = build_stored(Some(MasterSessionPhase::Configured), &turns, &inputs, &rows);
        assert_eq!(said(&both), said(&alone));
        assert_eq!(
            both.messages.len(),
            4,
            "each reply shows once, through its turn"
        );

        let ahead = [
            rows[0].clone(),
            rows[1].clone(),
            stored(3, Engine, OneTurn, Some("A result came in.")),
            stored(4, Master, OneTurn, Some("Noted.")),
        ];
        let behind = build_stored(
            Some(MasterSessionPhase::Configured),
            &turns[..1],
            &inputs,
            &ahead,
        );
        assert_eq!(
            said(&behind),
            [
                (Speaker::You, "Plan the page.", false),
                (Speaker::Master, "Page planned.", false),
                (Speaker::Notice, "A result came in.", false),
                (Speaker::Master, "Noted.", false),
            ]
        );
    }

    #[test]
    fn stored_text_reaches_the_transcript_without_terminal_escapes_or_control_characters() {
        use MasterMode::Live;
        use MessageSpeaker::{Engine, Master, Owner, Switch};
        let hostile = "\u{1b}[2J\u{1b}[31mRed\u{1b}[0m\u{7}\u{0} text\r\n\u{1b}]52;c;cGF5bG9hZA==\u{7}clip\u{9b}31m\u{202e}evil\ttab";
        let clean = "Red text\nclip31m\\u202eevil    tab";
        let rows = [
            stored(1, Owner, Live, Some(hostile)),
            stored(2, Master, Live, Some(hostile)),
            stored(3, Engine, Live, Some(hostile)),
            stored(4, Switch, Live, Some(hostile)),
        ];
        let transcript = build_stored(Some(MasterSessionPhase::Configured), &[], &[], &rows);
        assert_eq!(
            said(&transcript),
            [
                (Speaker::You, clean, false),
                (Speaker::Master, clean, false),
                (Speaker::Notice, clean, false),
                (Speaker::Notice, clean, false),
            ]
        );
        for message in &transcript.messages {
            assert!(
                message
                    .text
                    .chars()
                    .all(|character| character == '\n' || !character.is_control()),
                "{:?}",
                message.text
            );
        }
    }

    #[test]
    fn owner_words_read_from_the_harness_record_are_a_notice_and_never_the_owner_s_message() {
        use MasterMode::Live;
        use MessageSpeaker::{Engine, Master, Owner};
        let from = |source: MessageSource, row: SessionMessage| SessionMessage { source, ..row };
        let rows = [
            from(
                MessageSource::Record,
                stored(1, Owner, Live, Some("Ship it.\u{1b}[2J")),
            ),
            from(
                MessageSource::Record,
                stored(2, Master, Live, Some("Shipped.")),
            ),
            from(MessageSource::Counter, stored(3, Owner, Live, None)),
            from(
                MessageSource::Counter,
                stored(4, Master, Live, Some("Last line.")),
            ),
            stored(5, Engine, Live, Some("A result came in.")),
            stored(6, Owner, Live, Some("Seen typed.")),
            stored(7, Master, Live, Some("Done.")),
        ];
        assert_eq!(
            rows.iter().map(from_record).collect::<Vec<_>>(),
            [true, true, true, true, false, false, false]
        );
        let transcript = build_stored(Some(MasterSessionPhase::Configured), &[], &[], &rows);
        let typed = format!("{RECORD_TYPED} Ship it.");
        assert_eq!(
            said(&transcript),
            [
                (Speaker::Notice, typed.as_str(), false),
                (Speaker::Master, "Shipped.", false),
                (Speaker::Notice, TYPED_IN_TERMINAL, false),
                (Speaker::Master, "Last line.", false),
                (Speaker::Notice, "A result came in.", false),
                (Speaker::You, "Seen typed.", false),
                (Speaker::Master, "Done.", false),
            ],
            "only the words the engine saw typed are the owner's message"
        );
    }

    fn said(transcript: &Transcript) -> Vec<(Speaker, &str, bool)> {
        transcript
            .messages
            .iter()
            .map(|m| (m.speaker, m.text.as_str(), m.error))
            .collect()
    }

    #[test]
    fn a_turn_that_could_not_start_names_the_missing_repository_and_offers_a_retry() {
        let inputs = [entry(3, "Plan the dashboard.")];
        let turns = [turn(
            1,
            3,
            MasterTurnPhase::Blocked,
            Some(MasterReason::RepositoryMissing),
        )];
        let transcript = build(Some(MasterSessionPhase::Configured), &turns, &inputs);
        assert_eq!(
            said(&transcript),
            [
                (Speaker::You, "Plan the dashboard.", false),
                (
                    Speaker::Notice,
                    "Codex stopped: this project has no repository yet. (repository_missing) Press r to try again.",
                    true
                ),
            ]
        );
        assert_eq!(transcript.status, MasterStatus::Stopped);
    }

    #[test]
    fn a_turn_whose_instructions_are_too_long_says_to_shorten_them_on_the_process_page() {
        let inputs = [entry(3, "Plan the dashboard.")];
        let mut stopped = turn(
            1,
            3,
            MasterTurnPhase::Failed,
            Some(MasterReason::InstructionsTooLarge),
        );
        stopped.cause = Some("master_instructions_too_large".into());
        let transcript = build(Some(MasterSessionPhase::Configured), &[stopped], &inputs);
        let last = &transcript.messages.last().unwrap().text;
        assert!(
            last.ends_with(
                " stopped: the app and project instructions are too long for a Claude master. Shorten them on the Process page. (master_instructions_too_large) Press r to try again."
            ),
            "{last}"
        );
    }

    #[test]
    fn a_shared_account_never_tells_the_owner_to_wait_for_a_free_account() {
        let inputs = [entry(3, "Plan the dashboard.")];
        let cases = [
            (
                MasterReason::ProfileBusy,
                "Codex stopped: its account is busy signing in or out. (profile_busy) Press r to try again.",
            ),
            (
                MasterReason::CapacityUnavailable,
                "Codex stopped: another turn is still running. (capacity_unavailable) Press r to try again.",
            ),
            (
                MasterReason::QuotaExhausted,
                "Codex stopped: the account hit its usage limit. Add an account to the pool, or wait for its limit to reset. (quota_exhausted) Press r to try again.",
            ),
        ];
        for (reason, notice) in cases {
            let turns = [turn(1, 3, MasterTurnPhase::Failed, Some(reason))];
            let transcript = build(Some(MasterSessionPhase::Configured), &turns, &inputs);
            assert_eq!(
                said(&transcript),
                [
                    (Speaker::You, "Plan the dashboard.", false),
                    (Speaker::Notice, notice, true),
                ]
            );
            assert!(!notice.contains("free"));
        }
    }

    #[test]
    fn no_session_is_not_ready_and_an_open_master_without_turns_is_ready() {
        let empty = build(None, &[], &[]);
        assert_eq!(empty.status, MasterStatus::NotReady);
        assert!(empty.messages.is_empty());
        let ready = build(Some(MasterSessionPhase::Configured), &[], &[]);
        assert_eq!(ready.status, MasterStatus::Ready);
        assert_eq!(ready.harness, "Codex");
        assert_eq!(ready.account.as_deref(), Some("Main"));
        assert!(ready.tools);
    }

    #[test]
    fn the_owners_messages_pair_with_their_turns_and_a_running_turn_is_thinking() {
        let inputs = [
            entry(3, "Plan the dashboard."),
            entry(7, "Add a dark theme."),
        ];
        let turns = [
            turn(
                1,
                3,
                MasterTurnPhase::Failed,
                Some(MasterReason::QuotaExhausted),
            ),
            turn(2, 7, MasterTurnPhase::Running, None),
        ];
        let transcript = build(Some(MasterSessionPhase::Configured), &turns, &inputs);
        assert_eq!(
            transcript.status,
            MasterStatus::Thinking {
                since_ms: None,
                activity: Vec::new(),
            }
        );
        assert_eq!(
            said(&transcript),
            [
                (Speaker::You, "Plan the dashboard.", false),
                (
                    Speaker::Notice,
                    "Codex stopped: the account hit its usage limit. Add an account to the pool, or wait for its limit to reset. (quota_exhausted)",
                    true
                ),
                (Speaker::You, "Add a dark theme.", false),
            ]
        );
    }

    #[test]
    fn the_latest_turn_sets_the_status_and_offers_a_retry() {
        let configured = Some(MasterSessionPhase::Configured);
        let inputs = [entry(2, "Go.")];
        let cases = [
            (
                MasterTurnPhase::Blocked,
                Some(MasterReason::ReadinessUnknown),
                MasterStatus::NotSignedIn,
            ),
            (
                MasterTurnPhase::Blocked,
                Some(MasterReason::InvalidAuthentication),
                MasterStatus::NotSignedIn,
            ),
            (
                MasterTurnPhase::Blocked,
                Some(MasterReason::ApprovalRequired),
                MasterStatus::NeedsYou,
            ),
            (
                MasterTurnPhase::WaitingApproval,
                None,
                MasterStatus::NeedsYou,
            ),
            (
                MasterTurnPhase::Blocked,
                Some(MasterReason::CapacityUnavailable),
                MasterStatus::Stopped,
            ),
            (
                MasterTurnPhase::Failed,
                Some(MasterReason::RuntimeUnavailable),
                MasterStatus::Stopped,
            ),
            (MasterTurnPhase::Cancelled, None, MasterStatus::Stopped),
            (MasterTurnPhase::Uncertain, None, MasterStatus::Stopped),
            (
                MasterTurnPhase::Queued,
                None,
                MasterStatus::Thinking {
                    since_ms: None,
                    activity: Vec::new(),
                },
            ),
            (MasterTurnPhase::Succeeded, None, MasterStatus::Replied),
        ];
        for (phase, reason, want) in cases {
            let transcript = build(configured, &[turn(1, 2, phase, reason)], &inputs);
            assert_eq!(transcript.status, want, "{phase:?} {reason:?}");
            let last = transcript.messages.last().unwrap();
            match want {
                MasterStatus::Stopped | MasterStatus::NotSignedIn => {
                    assert_eq!(last.speaker, Speaker::Notice);
                    assert!(last.error);
                    assert!(
                        last.text.ends_with("Press r to try again."),
                        "{}",
                        last.text
                    );
                }
                MasterStatus::NeedsYou => {
                    let code = if reason.is_some() {
                        " (approval_required)"
                    } else {
                        ""
                    };
                    assert_eq!(
                        last.text,
                        format!("Codex is waiting for your approval.{code}")
                    );
                    assert!(!last.error);
                }
                _ => assert_eq!(last.speaker, Speaker::You),
            }
        }
        let cancelled = build(
            configured,
            &[turn(1, 2, MasterTurnPhase::Cancelled, None)],
            &inputs,
        );
        assert_eq!(
            cancelled.messages.last().unwrap().text,
            "Codex stopped: you stopped this turn. (cancelled) Press r to try again."
        );
    }

    /// F39: a turn stopped by a harness protocol fault, the master frame size limit or a
    /// master worktree the engine could not check says what to do next in plain words.
    #[test]
    fn a_configured_master_failure_line_names_its_action() {
        let configured = Some(MasterSessionPhase::Configured);
        let inputs = [entry(3, "Go.")];
        for (reason, expected) in [
            (
                MasterReason::ProtocolFault,
                "Codex stopped: it sent a message out of order. Send again to start a fresh turn. (protocol_fault) Press r to try again.",
            ),
            (
                MasterReason::FrameLimit,
                "Codex stopped: one message was larger than the master frame size limit. Raise it in Settings, or at 1 MiB shorten the message or the instructions, then send again. (frame_limit) Press r to try again.",
            ),
            (
                MasterReason::WorktreeUncertain,
                "Codex stopped: its copy of the project could not be checked. Close the master and open it again, then send again. (worktree_uncertain) Press r to try again.",
            ),
        ] {
            let transcript = build(
                configured,
                &[turn(1, 3, MasterTurnPhase::Failed, Some(reason))],
                &inputs,
            );
            assert_eq!(transcript.messages.last().unwrap().text, expected);
        }
    }

    #[test]
    fn every_stopped_turn_names_its_engine_code_and_never_a_bare_unknown() {
        let configured = Some(MasterSessionPhase::Configured);
        let inputs = [entry(3, "Go.")];
        let named = [
            (MasterReason::ReadinessUnknown, "readiness_unknown"),
            (MasterReason::ReadinessStale, "readiness_stale"),
            (
                MasterReason::InvalidAuthentication,
                "invalid_authentication",
            ),
            (MasterReason::QuotaUnknown, "quota_unknown"),
            (MasterReason::QuotaExhausted, "quota_exhausted"),
            (MasterReason::RateLimited, "rate_limited"),
            (MasterReason::CooldownActive, "cooldown_active"),
            (MasterReason::TransientFailure, "transient_failure"),
            (MasterReason::ConnectionChanged, "connection_changed"),
            (MasterReason::ScopeRevoked, "scope_revoked"),
            (MasterReason::CapacityUnavailable, "capacity_unavailable"),
            (MasterReason::BudgetExhausted, "budget_exhausted"),
            (MasterReason::ProfileBusy, "profile_busy"),
            (MasterReason::RuntimeUnavailable, "runtime_unavailable"),
            (MasterReason::CaptureIncomplete, "capture_incomplete"),
            (MasterReason::NoReply, "no_reply"),
            (MasterReason::StepLimit, "step_limit"),
            (MasterReason::TimeLimit, "time_limit"),
            (MasterReason::OutputLimit, "output_limit"),
            (
                MasterReason::UnsupportedCapability,
                "unsupported_capability",
            ),
            (MasterReason::RepositoryMissing, "repository_missing"),
            (MasterReason::InstructionsTooLarge, "instructions_too_large"),
            (MasterReason::ProtocolFault, "protocol_fault"),
            (MasterReason::FrameLimit, "frame_limit"),
            (MasterReason::WorktreeUncertain, "worktree_uncertain"),
            (MasterReason::Cancelled, "cancelled"),
        ];
        for (reason, code) in named {
            for phase in [MasterTurnPhase::Blocked, MasterTurnPhase::Failed] {
                let transcript = build(configured, &[turn(1, 3, phase, Some(reason))], &inputs);
                let last = &transcript.messages.last().unwrap().text;
                assert!(last.contains(&format!(" ({code}) Press r")), "{last}");
            }
        }
        let unnamed = [
            (
                MasterTurnPhase::Blocked,
                Some(MasterReason::Unknown),
                "blocked",
            ),
            (
                MasterTurnPhase::Failed,
                Some(MasterReason::Unknown),
                "failed",
            ),
            (MasterTurnPhase::Failed, None, "failed"),
            (MasterTurnPhase::Cancelled, None, "cancelled"),
            (MasterTurnPhase::Uncertain, None, "uncertain"),
            (
                MasterTurnPhase::Uncertain,
                Some(MasterReason::Unknown),
                "uncertain",
            ),
        ];
        for (phase, reason, code) in unnamed {
            let transcript = build(configured, &[turn(1, 3, phase, reason)], &inputs);
            let last = &transcript.messages.last().unwrap().text;
            assert!(last.contains(&format!(" ({code}) Press r")), "{last}");
            assert!(!last.contains("unknown"), "{last}");
        }
    }

    #[test]
    fn a_turn_that_ended_without_reply_text_is_not_called_cut_off() {
        let turns = [turn(
            1,
            3,
            MasterTurnPhase::Failed,
            Some(MasterReason::NoReply),
        )];
        let transcript = build(
            Some(MasterSessionPhase::Configured),
            &turns,
            &[entry(3, "Go.")],
        );
        let last = &transcript.messages.last().unwrap().text;
        assert!(
            last.contains("stopped: it ended its turn without writing a reply. (no_reply)"),
            "{last}"
        );
        assert!(!last.contains("cut off"), "{last}");
    }

    #[test]
    fn a_turn_stopped_at_each_step_limit_names_its_steps_and_how_to_go_on() {
        for steps in [50_u16, 100, 200] {
            let mut stopped = turn(1, 3, MasterTurnPhase::Failed, Some(MasterReason::StepLimit));
            stopped.cause = Some(format!("master_step_limit_{steps}"));
            let transcript = build(
                Some(MasterSessionPhase::Configured),
                &[stopped],
                &[entry(3, "Go.")],
            );
            let last = &transcript.messages.last().unwrap().text;
            let line = format!("Stopped after {steps} steps; reply with 'continue' to go on.");
            assert!(last.contains(&line), "{last}");
            assert!(
                last.contains(&format!("(master_step_limit_{steps})")),
                "{last}"
            );
            assert!(!last.contains("cut off"), "{last}");
            assert!(!last.contains("Press r"), "{last}");
        }
    }

    #[test]
    fn a_turn_stopped_at_each_time_limit_names_its_minutes_and_how_to_go_on() {
        for minutes in [10_u16, 30, 60] {
            let mut stopped = turn(1, 3, MasterTurnPhase::Failed, Some(MasterReason::TimeLimit));
            stopped.cause = Some(format!("master_time_limit_{minutes}"));
            let transcript = build(
                Some(MasterSessionPhase::Configured),
                &[stopped],
                &[entry(3, "Go.")],
            );
            let last = &transcript.messages.last().unwrap().text;
            let line = format!("Stopped after {minutes} minutes; reply with 'continue' to go on.");
            assert!(last.contains(&line), "{last}");
            assert!(
                last.contains(&format!("(master_time_limit_{minutes})")),
                "{last}"
            );
            assert!(!last.contains("ran out of time"), "{last}");
            assert!(!last.contains("at most"), "{last}");
            assert!(!last.contains("cut off"), "{last}");
            assert!(!last.contains("Press r"), "{last}");
        }
    }

    #[test]
    fn a_turn_a_clock_ended_with_no_minutes_named_claims_no_ten_minutes() {
        let mut late = turn(1, 3, MasterTurnPhase::Failed, Some(MasterReason::TimeLimit));
        late.cause = Some("codex_host_deadline".into());
        let transcript = build(
            Some(MasterSessionPhase::Configured),
            &[late],
            &[entry(3, "Go.")],
        );
        let last = &transcript.messages.last().unwrap().text;
        let line = "stopped: it reached its time limit. (codex_host_deadline)";
        assert!(last.contains(line), "{last}");
        assert!(!last.contains("10 minutes"), "{last}");
        assert!(!last.contains("cut off"), "{last}");
    }

    #[test]
    fn a_turn_with_more_output_than_the_app_keeps_says_so_plainly() {
        let mut large = turn(
            1,
            3,
            MasterTurnPhase::Failed,
            Some(MasterReason::OutputLimit),
        );
        large.cause = Some("claude_host_stdout_limit".into());
        let transcript = build(
            Some(MasterSessionPhase::Configured),
            &[large],
            &[entry(3, "Go.")],
        );
        let last = &transcript.messages.last().unwrap().text;
        let line = "stopped: this turn's output was too large to keep. (claude_host_stdout_limit)";
        assert!(last.contains(line), "{last}");
        assert!(!last.contains("something went wrong"), "{last}");
        assert!(!last.contains("cut off"), "{last}");
    }

    #[test]
    fn a_refused_master_request_reads_plainly_and_keeps_its_code() {
        assert_eq!(
            master_problem("master_source_conflict"),
            "Settings changed since this conversation started. (master_source_conflict)"
        );
        assert_eq!(
            master_problem("master_turn_capacity"),
            "This conversation is full. Press e to end it, then send again. (master_turn_capacity)"
        );
        assert_eq!(
            master_problem("master_dispatch_capacity"),
            "The engine refused it. (master_dispatch_capacity)"
        );
    }

    #[test]
    fn a_stopped_turn_names_the_cause_the_engine_stored() {
        let configured = Some(MasterSessionPhase::Configured);
        let inputs = [entry(3, "Go.")];
        let refused = "/owner/repo has the Git setting core.hooksPath, which runs hooks from a folder the repository chooses. Remove it from the repository's .git/config, then send again.";
        const KEY_SETUP: &str = "The master could not set this account's API key up for agents. Set it up again: in the app, Settings, Pools, Get ready for agents; in the terminal app, Model access, k Private profile setup.";
        let cases = [
            (
                MasterReason::RuntimeUnavailable,
                None,
                "rcli_profile_configuration_required",
                "Codex stopped: it could not start. Check it in Settings, Harnesses. (rcli_profile_configuration_required) Press r to try again.".to_owned(),
            ),
            (
                MasterReason::Unknown,
                None,
                "master_runtime_binding",
                "Codex stopped: something went wrong. (master_runtime_binding) Press r to try again.".to_owned(),
            ),
            (
                MasterReason::RepositoryConfigRefused,
                Some(refused),
                "repository_config_refused:core_hookspath",
                format!("Codex stopped: {refused} (repository_config_refused:core_hookspath) Press r to try again."),
            ),
            (
                MasterReason::ConnectionChanged,
                Some(KEY_SETUP),
                "rcli_master_key_setup_required",
                format!("{KEY_SETUP} (rcli_master_key_setup_required) Press r to try again."),
            ),
        ];
        for (reason, detail, cause, line) in cases {
            let mut stopped = turn(1, 3, MasterTurnPhase::Blocked, Some(reason));
            stopped.detail = detail.map(str::to_owned);
            stopped.cause = Some(cause.to_owned());
            let transcript = build(configured, &[stopped], &inputs);
            assert_eq!(transcript.messages.last().unwrap().text, line);
        }
    }

    #[test]
    fn a_closed_master_is_stopped_and_says_so() {
        let transcript = build(Some(MasterSessionPhase::Closed), &[], &[]);
        assert_eq!(transcript.status, MasterStatus::Stopped);
        assert_eq!(
            said(&transcript),
            [(Speaker::Notice, "The master conversation is closed.", false)]
        );
    }

    #[test]
    fn old_approval_requests_do_not_repeat_and_long_histories_keep_the_latest_turns() {
        let inputs: Vec<ConversationEntry> =
            (1..=260).map(|n| entry(n + 1, &format!("m{n}"))).collect();
        let mut turns: Vec<MasterTurnView> = (1..=260)
            .map(|n| turn(n, n + 1, MasterTurnPhase::Succeeded, None))
            .collect();
        turns[0].phase = MasterTurnPhase::WaitingApproval;
        turns[258].phase = MasterTurnPhase::WaitingApproval;
        let transcript = build(Some(MasterSessionPhase::Configured), &turns, &inputs);
        assert_eq!(transcript.status, MasterStatus::Replied);
        assert_eq!(transcript.messages.len(), TURNS_SHOWN_MAX);
        assert_eq!(transcript.messages[0].text, "m61");
        assert_eq!(transcript.messages.last().unwrap().text, "m260");
        assert!(
            transcript
                .messages
                .iter()
                .all(|m| m.speaker == Speaker::You)
        );
    }

    #[test]
    fn the_tasks_a_reply_handed_off_show_under_it_with_their_outcome_or_as_still_running() {
        use crate::master_session::{MasterReply, MasterReplyProvenance, MasterTask};
        let inputs = [entry(3, "Plan the page.")];
        let mut replied = turn(1, 3, MasterTurnPhase::Succeeded, None);
        replied.reply = Some(MasterReply {
            text: "I handed it to a sub-agent.".into(),
            provenance: MasterReplyProvenance {
                attempt_id: RequestId::new("attempt-1").unwrap(),
                runtime_ref: RequestId::new("runtime-1").unwrap(),
                capture_ref: RequestId::new("capture-1").unwrap(),
                capture_sequence: 1,
            },
        });
        let done = "Task 7 \"Write the README\" (codex, sub-agent 3) is done. Full result in the Team view.";
        replied.tasks = vec![
            MasterTask {
                task: 7,
                note: Some(done.into()),
            },
            MasterTask {
                task: 8,
                note: None,
            },
        ];
        let transcript = build(Some(MasterSessionPhase::Configured), &[replied], &inputs);
        assert_eq!(
            said(&transcript),
            [
                (Speaker::You, "Plan the page.", false),
                (Speaker::Master, "I handed it to a sub-agent.", false),
                (Speaker::Task, done, false),
                (Speaker::Task, "Task 8 is still with a sub-agent.", false),
            ]
        );
    }

    #[test]
    fn a_turn_whose_edits_wait_in_changes_says_so_under_its_reply_and_tasks() {
        use crate::master_session::{MasterReply, MasterReplyProvenance, MasterTask};
        let inputs = [entry(3, "Fix the README.")];
        let mut replied = turn(1, 3, MasterTurnPhase::Succeeded, None);
        replied.reply = Some(MasterReply {
            text: "I fixed the README.".into(),
            provenance: MasterReplyProvenance {
                attempt_id: RequestId::new("attempt-1").unwrap(),
                runtime_ref: RequestId::new("runtime-1").unwrap(),
                capture_ref: RequestId::new("capture-1").unwrap(),
                capture_sequence: 1,
            },
        });
        replied.tasks = vec![MasterTask {
            task: 8,
            note: None,
        }];
        let waiting = "The edits from this turn are waiting for you in Changes.";
        replied.changes = Some(waiting.into());
        let quiet = turn(2, 4, MasterTurnPhase::Succeeded, None);
        let transcript = build(
            Some(MasterSessionPhase::Configured),
            &[replied, quiet],
            &inputs,
        );
        assert_eq!(
            said(&transcript),
            [
                (Speaker::You, "Fix the README.", false),
                (Speaker::Master, "I fixed the README.", false),
                (Speaker::Task, "Task 8 is still with a sub-agent.", false),
                (Speaker::Notice, waiting, false),
            ]
        );
    }

    #[test]
    fn a_turn_the_engine_asked_for_shows_its_plain_line_before_the_master_s_reply() {
        use crate::master_session::{MasterEngineRequest, MasterReply, MasterReplyProvenance};
        let inputs = [entry(3, "Fix the README.")];
        let line = "CrazyAgent asked the master to resolve the conflicts in NOTES.md.";
        let mut asked = turn(2, 5, MasterTurnPhase::Succeeded, None);
        asked.engine_request = Some(MasterEngineRequest {
            change_set: crate::landing::ChangeSetId::new("0".repeat(32)).unwrap(),
            line: line.into(),
            text: "Resolve every conflict in NOTES.md.".into(),
        });
        asked.reply = Some(MasterReply {
            text: "I kept both notes.".into(),
            provenance: MasterReplyProvenance {
                attempt_id: RequestId::new("attempt-2").unwrap(),
                runtime_ref: RequestId::new("runtime-2").unwrap(),
                capture_ref: RequestId::new("capture-2").unwrap(),
                capture_sequence: 1,
            },
        });
        let transcript = build(
            Some(MasterSessionPhase::Configured),
            &[turn(1, 3, MasterTurnPhase::Succeeded, None), asked],
            &inputs,
        );
        assert_eq!(
            said(&transcript),
            [
                (Speaker::You, "Fix the README.", false),
                (Speaker::Notice, line, false),
                (Speaker::Master, "I kept both notes.", false),
            ]
        );
    }

    fn activity(lines: &[(u64, &str)]) -> Vec<MasterActivityLine> {
        lines
            .iter()
            .map(|(at_ms, text)| MasterActivityLine {
                at_ms: *at_ms,
                text: (*text).into(),
            })
            .collect()
    }

    #[test]
    fn a_running_turn_carries_its_newest_activity_and_a_turn_that_is_not_running_carries_none() {
        let inputs = [entry(3, "Plan the dashboard.")];
        let configured = Some(MasterSessionPhase::Configured);
        let sent: Vec<MasterActivityLine> = (0..25u64)
            .map(|step| MasterActivityLine {
                at_ms: 1_000 + step,
                text: format!("Step {step}"),
            })
            .collect();
        let mut running = turn(1, 3, MasterTurnPhase::Running, None);
        running.activity = sent.clone();
        let transcript = build(configured, &[running], &inputs);
        assert_eq!(
            transcript.status,
            MasterStatus::Thinking {
                since_ms: None,
                activity: sent[5..].to_vec(),
            }
        );
        assert_eq!(transcript.status.activity_len(), ACTIVITY_MAX);
        for phase in [
            MasterTurnPhase::WaitingApproval,
            MasterTurnPhase::Uncertain,
            MasterTurnPhase::Succeeded,
            MasterTurnPhase::Failed,
            MasterTurnPhase::Cancelled,
        ] {
            let mut ended = turn(1, 3, phase, None);
            ended.activity = sent.clone();
            let transcript = build(configured, &[ended], &inputs);
            assert_eq!(transcript.status.activity_len(), 0, "{phase:?}");
            assert!(
                !transcript.messages.iter().any(|m| m.text.contains("Step")),
                "{phase:?}"
            );
        }
    }

    #[test]
    fn the_thinking_row_names_the_last_activity_and_its_age_then_how_long_nothing_new_came() {
        assert_eq!(last_activity(&[], 60_000), None);
        let lines = activity(&[
            (10_000, "Searching the code"),
            (56_000, "Reading src/app/page.tsx"),
        ]);
        assert_eq!(
            last_activity(&lines, 60_000).as_deref(),
            Some("Reading src/app/page.tsx, 4 s ago")
        );
        assert_eq!(
            last_activity(&lines, 56_000 + ACTIVITY_QUIET_MS - 1).as_deref(),
            Some("Reading src/app/page.tsx, 59 s ago")
        );
        assert_eq!(
            last_activity(&lines, 56_000 + ACTIVITY_QUIET_MS).as_deref(),
            Some("no new activity for 1 min")
        );
        assert_eq!(
            last_activity(&lines, 56_000 + 6 * 60_000 + 30_000).as_deref(),
            Some("no new activity for 6 min")
        );
        assert_eq!(
            last_activity(&lines, 50_000).as_deref(),
            Some("Reading src/app/page.tsx, 0 s ago"),
            "a line stamped after this screen's clock counts as just seen"
        );
        assert_eq!(
            last_activity(&activity(&[(u64::MAX, "Working")]), 60_000).as_deref(),
            Some("Working, 0 s ago")
        );
    }

    #[test]
    fn the_activity_list_shows_the_newest_five_lines_and_every_kept_line_when_open() {
        let lines: Vec<MasterActivityLine> = (0..7u64)
            .map(|step| MasterActivityLine {
                at_ms: 10_000 * (step + 1),
                text: format!("Step {step}"),
            })
            .collect();
        let now = 74_000;
        let row = |age: &str, step: u64| (age.to_owned(), format!("Step {step}"));
        assert_eq!(
            activity_rows(&lines, false, now),
            [
                row("44 s ago", 2),
                row("34 s ago", 3),
                row("24 s ago", 4),
                row("14 s ago", 5),
                row("4 s ago", 6),
            ]
        );
        let open = activity_rows(&lines, true, now);
        assert_eq!(open.len(), 7);
        assert_eq!(open[0], row("1 min ago", 0));
        assert_eq!(open[6], row("4 s ago", 6));
        assert_eq!(activity_rows(&lines[..2], false, now).len(), 2);
        assert!(activity_rows(&[], true, now).is_empty());
        let many: Vec<MasterActivityLine> = (0..30u64)
            .map(|step| MasterActivityLine {
                at_ms: step,
                text: format!("Step {step}"),
            })
            .collect();
        let kept = activity_rows(&many, true, now);
        assert_eq!(kept.len(), ACTIVITY_MAX);
        assert_eq!(kept[0].1, "Step 10");
        assert_eq!(ACTIVITY_SHOWN, 5);
    }
}
