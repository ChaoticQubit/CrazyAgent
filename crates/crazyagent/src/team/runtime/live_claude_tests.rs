use super::managed_agent::{ManagedAgent, SignedIn, claude};
use super::{BACKGROUND_STOPPED, Report, TurnEnd};
use crate::team::{AgentRole, BackgroundAtEnd};
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{FOLDER, Host};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const TURN: Duration = Duration::from_secs(180);
const REAPED: Duration = Duration::from_secs(10);

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

/// The running processes whose command line holds `needle`, as `ps` lists them.
/// Input: the text. Output: each process's pid and command line.
fn processes_with(needle: &str) -> Vec<String> {
    let listed = std::process::Command::new("/bin/ps")
        .args(["-axww", "-o", "pid=,command="])
        .output()
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
