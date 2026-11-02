use super::managed_agent::{ManagedAgent, SignedIn, claude};
use super::{Report, TurnEnd};
use crate::harnesses::terminal_session::Resume;
use crate::team::AgentRole;
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{FOLDER, Host};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const RESUME_TURN: Duration = Duration::from_secs(180);
const FIRST_CODEWORD: &str = "HERON-4417";
const SECOND_CODEWORD: &str = "LARCH-8093";
const ASK_FIRST: &str =
    "What is the codeword your instructions give? Reply with only that codeword.";
const ASK_SECOND: &str = "Read your instructions as they stand now. What is the second codeword they give? Reply with only that codeword, or with the word NONE if they give no second codeword.";

#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_claude_team_agent_turn_is_signed_in_inside_the_agent_sandbox() {
    let mut signed_in = SignedIn::claude();
    let agent = ManagedAgent::new("claude-code", &claude());
    let profile = signed_in.acquire("claude-live-team-turn");
    let managed = profile.profile_path().unwrap();
    let rules = std::fs::read_to_string(&agent.sandbox_profile).unwrap();
    let (text, end, cause) = agent.run_turn(
        profile,
        "Reply with exactly the word OK and nothing else.",
        Duration::from_secs(120),
    );
    println!("real claude team agent turn: end={end:?} cause={cause:?} reply={text:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("OK"), "{text}");
    assert!(!agent.home.join(".credentials.json").exists());
    assert!(!agent.home.starts_with(&managed));
    assert!(!rules.contains(managed.to_str().unwrap()));
}

/// The texts of one kind of entry in the transcript Claude Code keeps: each `user` or
/// `assistant` line's message content, a plain string or its text blocks joined.
/// Inputs: the transcript path and the entry type. Output: the texts in transcript order.
fn said(record: &Path, kind: &str) -> Vec<String> {
    std::fs::read_to_string(record)
        .unwrap_or_else(|problem| panic!("no transcript at {}: {problem}", record.display()))
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["type"] == kind)
        .map(|entry| match &entry["message"]["content"] {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        })
        .collect()
}

/// How one run of turns ended. Input: the worker's reports. Output: the last finished
/// turn's reply, end and cause, and the newest conversation the worker recorded.
fn outcome(reports: &[Report]) -> (String, TurnEnd, Option<String>, Option<Resume>) {
    let (text, end, cause) = reports
        .iter()
        .rev()
        .find_map(|report| match report {
            Report::Finished { text, end, cause } => Some((text.clone(), *end, cause.clone())),
            _ => None,
        })
        .expect("the turn finished");
    let conversation = reports.iter().rev().find_map(|report| match report {
        Report::Conversation(resume) => Some(resume.clone()),
        _ => None,
    });
    (text, end, cause, conversation)
}

/// QR2 (DEC-111): a Claude Code sub-agent the team runtime runs in a pane, relaunched with
/// `--resume` after the owner edited the instructions, keeps the conversation it started and
/// sees the edit only through what the engine gives it at the relaunch, the instructions file
/// the launch installs and records as given, never as text typed into the conversation.
/// Asserts on the real transcript and real replies on the owner's signed-in account. A
/// failure at the second codeword means `--resume` kept the prompt the session started with.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_resumed_claude_agent_keeps_its_conversation_and_sees_an_edit_only_as_the_engine_gives_it() {
    let root = tempfile::Builder::new()
        .prefix("ca-live-resume-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &base.join(FOLDER)));
    let mut signed_in = SignedIn::claude();
    let mut agent = ManagedAgent::in_pane("claude-code", &claude(), host, AgentRole::Sub);
    let started = format!("## Codeword\n\nThe codeword is {FIRST_CODEWORD}.\n");
    let edited =
        format!("{started}\n## Second codeword\n\nThe second codeword is {SECOND_CODEWORD}.\n");
    crate::agent_home::save_app(&agent.base, &started).unwrap();

    let first = signed_in.acquire("claude-live-resume-first");
    let (reports, problem) = agent.run_turns(None, vec![(ASK_FIRST, first)], RESUME_TURN);
    let (reply, end, cause, conversation) = outcome(&reports);
    println!("first turn: end={end:?} cause={cause:?} reply={reply:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?} {problem:?}");
    assert!(
        reply.contains(FIRST_CODEWORD),
        "the session starts with the first text: {reply:?}"
    );
    let conversation = conversation.expect("the first turn records its conversation");
    assert!(
        said(&conversation.record, "assistant")
            .iter()
            .any(|text| text.contains(FIRST_CODEWORD)),
        "the transcript holds the first reply"
    );

    crate::agent_home::save_app(&agent.base, &edited).unwrap();
    let second = signed_in.acquire("claude-live-resume-second");
    let (reports, problem) = agent.run_turns(
        Some(conversation.clone()),
        vec![(ASK_SECOND, second)],
        RESUME_TURN,
    );
    assert!(
        !reports
            .iter()
            .any(|report| matches!(report, Report::ResumeRefused)),
        "the relaunch resumes the conversation: {problem:?}"
    );
    let (reply, end, cause, resumed) = outcome(&reports);
    println!("resumed turn: end={end:?} cause={cause:?} reply={reply:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?} {problem:?}");
    let record = resumed.map_or(conversation.record, |resumed| resumed.record);
    let typed = said(&record, "user");
    let replies = said(&record, "assistant");
    assert!(
        typed.iter().any(|text| text.contains(ASK_FIRST))
            && replies.iter().any(|text| text.contains(FIRST_CODEWORD)),
        "the resumed session keeps the conversation it started: {typed:?} {replies:?}"
    );
    assert!(
        typed.iter().any(|text| text.contains(ASK_SECOND)),
        "the resumed turn is in the same conversation: {typed:?}"
    );
    assert!(
        reply.contains(SECOND_CODEWORD),
        "the resumed session sees the edit the relaunch installed (QR2): {reply:?}"
    );
    assert!(
        !typed.iter().any(|text| text.contains(SECOND_CODEWORD)),
        "the edit is never typed into the conversation: {typed:?}"
    );
    let given = crate::agent_home::given_text(&agent.base, &agent.home)
        .unwrap()
        .expect("the relaunch records what it gave");
    assert!(
        given.contains(SECOND_CODEWORD),
        "the engine records the edited text as given at the relaunch"
    );
}
