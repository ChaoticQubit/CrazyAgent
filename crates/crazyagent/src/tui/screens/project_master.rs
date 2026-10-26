use super::project::{MasterStatus, Message, Speaker, Transcript};
use super::project_time::clock;
use crate::conversation::ConversationEntry;
use crate::master_session::{MasterReason, MasterSessionPhase, MasterTurnPhase, MasterTurnView};

const TURNS_SHOWN_MAX: usize = 200;

/// The engine's master conversation for one project, as read through the owner connection.
/// Holds the session phase (none before the master is opened), its turns in order and the owner's messages.
/// The Conversation tab turns it into a transcript with `transcript_from`.
pub(in crate::tui) struct MasterRead<'a> {
    pub session: Option<MasterSessionPhase>,
    pub turns: &'a [MasterTurnView],
    pub inputs: &'a [ConversationEntry],
}

/// Builds the Conversation transcript from the engine's master turns.
/// Takes the read, the master harness name, the account label, whether tools are on and the UTC offset.
/// Returns the transcript: the owner's messages, the master's real replies and a plain notice for a stopped turn.
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
    for (index, turn) in turns.iter().enumerate() {
        let latest = index + 1 == turns.len();
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
        if let Some(text) = notice(turn, harness, latest) {
            messages.push(Message {
                speaker: Speaker::Notice,
                time: None,
                text,
                error: !asks_approval(turn),
            });
        }
    }
    assert!(messages.len() <= turns.len() * 3);
    let status = match (read.session, turns.last()) {
        (None, _) => MasterStatus::NotReady,
        (Some(MasterSessionPhase::Closing | MasterSessionPhase::Closed), _) => {
            MasterStatus::Stopped
        }
        (Some(MasterSessionPhase::Configured), None) => MasterStatus::Ready,
        (Some(MasterSessionPhase::Configured), Some(turn)) => status(turn),
    };
    if read
        .session
        .is_some_and(|phase| phase != MasterSessionPhase::Configured)
    {
        messages.push(Message {
            speaker: Speaker::Notice,
            time: None,
            text: "The master conversation is closed.".into(),
            error: false,
        });
    }
    Transcript {
        harness: harness.to_owned(),
        account,
        tools,
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
    if asks_approval(turn) {
        return latest.then(|| format!("{harness} is waiting for your approval."));
    }
    if turn.phase == MasterTurnPhase::Uncertain {
        return Some(format!(
            "It is not known whether {harness} finished this turn. Press r to try again."
        ));
    }
    let retry = if latest { " Press r to try again." } else { "" };
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
        Some(MasterReason::CapacityUnavailable) => "another turn is still running.",
        Some(MasterReason::ProfileBusy) => "its account is busy signing in or out.",
        Some(MasterReason::ConnectionChanged) => "its settings changed while it was open.",
        Some(MasterReason::ScopeRevoked) => "it no longer has access to this project.",
        Some(MasterReason::RuntimeUnavailable) => {
            "it could not start. Check it in Settings, Harnesses."
        }
        Some(MasterReason::CaptureIncomplete) => "its reply was cut off.",
        Some(MasterReason::OutputLimit) => "its reply was longer than the limit.",
        Some(MasterReason::UnsupportedCapability) => "it cannot lead a project yet.",
        Some(MasterReason::RepositoryMissing) => "this project has no repository yet.",
        Some(MasterReason::Cancelled) => "you stopped this turn.",
        Some(MasterReason::ApprovalRequired) | Some(MasterReason::Unknown) | None => {
            if turn.phase == MasterTurnPhase::Cancelled {
                "you stopped this turn."
            } else {
                "something went wrong."
            }
        }
    };
    Some(format!("{harness} stopped: {why}{retry}"))
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
        let read = MasterRead {
            session,
            turns,
            inputs,
        };
        transcript_from(&read, "Codex", Some("Main".into()), true, 0)
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
                    "Codex stopped: this project has no repository yet. Press r to try again.",
                    true
                ),
            ]
        );
        assert_ne!(transcript.status, MasterStatus::Thinking { since_ms: None });
    }

    #[test]
    fn a_shared_account_never_tells_the_owner_to_wait_for_a_free_account() {
        let inputs = [entry(3, "Plan the dashboard.")];
        let cases = [
            (
                MasterReason::ProfileBusy,
                "Codex stopped: its account is busy signing in or out. Press r to try again.",
            ),
            (
                MasterReason::CapacityUnavailable,
                "Codex stopped: another turn is still running. Press r to try again.",
            ),
            (
                MasterReason::QuotaExhausted,
                "Codex stopped: the account hit its usage limit. Add an account to the pool, or wait for its limit to reset. Press r to try again.",
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
        assert_eq!(transcript.status, MasterStatus::Thinking { since_ms: None });
        assert_eq!(
            said(&transcript),
            [
                (Speaker::You, "Plan the dashboard.", false),
                (
                    Speaker::Notice,
                    "Codex stopped: the account hit its usage limit. Add an account to the pool, or wait for its limit to reset.",
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
                MasterStatus::Thinking { since_ms: None },
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
                    assert_eq!(last.text, "Codex is waiting for your approval.");
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
            "Codex stopped: you stopped this turn. Press r to try again."
        );
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
}
