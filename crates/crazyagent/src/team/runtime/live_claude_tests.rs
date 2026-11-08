use super::managed_agent::{ManagedAgent, SignedIn, claude};
use super::{BACKGROUND_STOPPED, Report, TurnEnd};
use crate::harnesses::terminal_session::Resume;
use crate::team::{AgentRole, BackgroundAtEnd};
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{FOLDER, Host};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const TURN: Duration = Duration::from_secs(180);
const REAPED: Duration = Duration::from_secs(10);
const MID_TURN: Duration = Duration::from_secs(150);

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

/// LL-2: a real Claude Code team agent runs a turn with the launch files of its own home read
/// only and no write allowed to the skills, agents, commands, output-styles, rules or
/// settings.json Claude Code would load from there: the turn succeeds and it answers from the
/// instructions the engine gave it. It prints its home, so the run's Seatbelt denials can
/// be read for any write Claude Code itself tried there.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_claude_agent_reads_its_instructions_with_its_home_config_read_only() {
    let mut signed_in = SignedIn::claude();
    let agent = ManagedAgent::new("claude-code", &claude());
    let instructions = format!("## Codeword\n\nThe codeword is {FIRST_CODEWORD}.\n");
    crate::agent_home::save_app(&agent.base, &instructions).unwrap();
    let profile = signed_in.acquire("claude-live-home-config");
    let (text, end, cause) = agent.run_turn(profile, ASK_FIRST, TURN);
    println!(
        "real claude agent with read-only home config: home={} end={end:?} cause={cause:?} reply={text:?}",
        agent.home.display()
    );
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains(FIRST_CODEWORD), "{text}");
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

/// The running processes whose command line holds `needle`, as `ps` lists them.
/// Input: the text. Output: each process's pid and command line.
fn processes_with(needle: &str) -> Vec<String> {
    let listed = crate::spawn_gate::output(
        std::process::Command::new("/bin/ps")
            .args(["-axww", "-o", "pid=,command="])
            .stdout(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(listed.status.success());
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|line| line.contains(needle))
        .map(str::to_owned)
        .collect()
}

/// Under Stop, a real Claude Code sub-agent's command started in the background ends with
/// its task (rv-audit S3 F3). On the owner's signed-in account, in a pane as the owner's
/// agents run, the agent starts a long sleep with its Bash tool in the background and ends
/// its turn: the task ends with the turn and says first that the command was stopped, and
/// no process of that command or of the agent's home is left. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn under_stop_a_real_claude_agent_s_background_command_ends_with_its_task() {
    let root = tempfile::Builder::new()
        .prefix("ca-live-bg-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &base.join(FOLDER)));
    let mut agent = ManagedAgent::in_pane("claude-code", &claude(), host.clone(), AgentRole::Sub);
    agent.background = BackgroundAtEnd::Stop;
    let mut signed_in = SignedIn::claude();
    let marker = format!("sleep 300.{:05}", std::process::id() % 100_000);
    let home = agent.home.to_str().unwrap().to_owned();
    let seen = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (seen, done, marker) = (seen.clone(), done.clone(), marker.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                if !processes_with(&marker).is_empty() {
                    seen.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };
    let prompt = format!(
        "Use your Bash tool with run_in_background set to true to start the command `{marker}`. Do not wait for it or check on it. Then reply with exactly the word STARTED and end your turn."
    );
    let turns = vec![(
        prompt.as_str(),
        signed_in.acquire("claude-live-background-stop"),
    )];
    let (reports, problem) = agent.run_turns(None, turns, TURN);
    done.store(true, Ordering::SeqCst);
    watcher.join().unwrap();
    let (text, end, cause) = reports
        .iter()
        .find_map(|report| match report {
            Report::Finished { text, end, cause } => Some((text.clone(), *end, cause.clone())),
            _ => None,
        })
        .expect("the turn finished");
    println!("real claude background stop: end={end:?} cause={cause:?} reply={text:?}");
    let deadline = Instant::now() + REAPED;
    let left = loop {
        let mut left = processes_with(&marker);
        left.extend(processes_with(&home));
        if left.is_empty() || Instant::now() >= deadline {
            break left;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    host.close(&[]).unwrap();
    assert_eq!(problem, None);
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(
        seen.load(Ordering::SeqCst),
        "the agent's background command never ran: {text}"
    );
    assert!(
        text.starts_with(BACKGROUND_STOPPED) && text.contains("STARTED"),
        "{text}"
    );
    assert!(
        left.is_empty(),
        "processes of the agent outlived its task: {left:?}"
    );
}

/// The transcript lines that tell how a background command ended: each `attachment` line
/// and each line whose tool result names a background task. Input: the transcript path.
/// Output: the lines in transcript order.
fn background_lines(record: &Path) -> Vec<String> {
    std::fs::read_to_string(record)
        .unwrap_or_else(|problem| panic!("no transcript at {}: {problem}", record.display()))
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line).is_ok_and(|entry| {
                entry["type"] == "attachment"
                    || entry["toolUseResult"]["backgroundTaskId"].is_string()
                    || entry["toolUseResult"]["taskId"].is_string()
                    || entry["toolUseResult"]["task_id"].is_string()
            })
        })
        .map(str::to_owned)
        .collect()
}

/// S1 (DEC-119, HackerBlog task 177): under Wait, a real Claude Code sub-agent whose
/// background command ends while the agent still works in its turn ends its task with that
/// turn. The agent starts a short sleep in the background, then runs a longer one in the
/// foreground, so the background end lands mid-turn, then replies DONE. The task ends with
/// success before its time limit, never held. Prints the background lines it recorded.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_claude_agent_whose_background_command_ends_mid_turn_ends_its_task_with_its_turn() {
    let root = tempfile::Builder::new()
        .prefix("ca-live-midturn-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &base.join(FOLDER)));
    let mut agent = ManagedAgent::in_pane("claude-code", &claude(), host.clone(), AgentRole::Sub);
    agent.background = BackgroundAtEnd::Wait;
    let mut signed_in = SignedIn::claude();
    let prompt = "Use your Bash tool with run_in_background set to true to run `/bin/sleep 4 && echo early`. Do not check on it. Next, use your Bash tool in the foreground, not in the background, to run `/bin/sleep 25 && echo late`. When that returns, reply with exactly the word DONE and end your turn.";
    let turns = vec![(prompt, signed_in.acquire("claude-live-background-mid-turn"))];
    let started = Instant::now();
    let (reports, problem) = agent.run_turns(None, turns, MID_TURN);
    let took = started.elapsed();
    let (text, end, cause, conversation) = outcome(&reports);
    println!("mid-turn background: end={end:?} cause={cause:?} took={took:?} reply={text:?}");
    if let Some(conversation) = &conversation {
        for line in background_lines(&conversation.record) {
            println!("background line: {line}");
        }
    }
    host.close(&[]).unwrap();
    assert_eq!(problem, None);
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("DONE"), "{text}");
    assert!(
        took < MID_TURN,
        "the task stayed held until its time limit: {took:?}"
    );
}

#[path = "live_kept_screen_tests.rs"]
mod kept_screen;

#[path = "live_display_tests.rs"]
mod display;
