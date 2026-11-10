use super::live_claude_tests::processes_with;
use super::managed_agent::{ManagedAgent, SignedIn, codex, shown};
use super::{Report, TurnEnd};
use crate::team::{AgentRole, BackgroundAtEnd};
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{FOLDER, Host};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CODEX_TURN: Duration = Duration::from_secs(180);
const REAPED: Duration = Duration::from_secs(10);
const MARKER_GRACE: Duration = Duration::from_secs(5);
const FIRST_PROMPT: &str = "Reply with exactly the word OK and nothing else.";

/// Reads every rollout Codex wrote below a sessions folder. Input: the folder.
/// Output: the rollouts' text, joined.
fn rollouts(folder: &Path) -> String {
    let mut text = String::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for item in std::fs::read_dir(&folder).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                text.push_str(&std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    text
}

/// A real Codex team agent runs a turn with the launch files of its own home read-only and
/// its skills folder pinned (LL-2): the turn succeeds, so Codex starts without writing them,
/// and the rollout it wrote holds the engine's developer instructions, so it read its
/// `config.toml`.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_agent_reads_its_developer_instructions_with_its_launch_files_read_only() {
    let mut signed_in = SignedIn::codex();
    let agent = ManagedAgent::new("codex", &codex());
    let profile = signed_in.acquire("codex-live-launch-files");
    let instructions = agent.instructions();
    let first = instructions
        .lines()
        .find(|line| line.trim().len() > 20)
        .expect("the engine gives the agent instructions");
    let (text, end, cause) = agent.run_turn(
        profile,
        "Reply with exactly the word OK and nothing else.",
        Duration::from_secs(120),
    );
    println!(
        "real codex agent with read-only launch files: end={end:?} cause={cause:?} reply={text:?}"
    );
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("OK"), "{text}");
    let written = rollouts(&agent.home.join("sessions"));
    let quoted = serde_json::to_string(first).unwrap();
    assert!(
        written.contains(quoted.trim_matches('"')),
        "no rollout holds the developer instructions line {first:?}"
    );
}

#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_team_agent_turn_is_signed_in_inside_the_agent_sandbox() {
    let mut signed_in = SignedIn::codex();
    let agent = ManagedAgent::new("codex", &codex());
    let profile = signed_in.acquire("codex-live-team-turn");
    let managed = profile.profile_path().unwrap();
    let rules = std::fs::read_to_string(&agent.sandbox_profile).unwrap();
    let (text, end, cause) = agent.run_turn(
        profile,
        "Reply with exactly the word OK and nothing else.",
        Duration::from_secs(120),
    );
    println!("real codex team agent turn: end={end:?} cause={cause:?} reply={text:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("OK"), "{text}");
    assert!(!agent.home.join("auth.json").exists());
    assert!(!agent.home.starts_with(&managed));
    assert!(!rules.contains(managed.to_str().unwrap()));
}

/// The reply, end and cause of the first turn that finished among a worker's reports.
/// Input: the reports. Output: that turn's reply, end and cause; panics with every report
/// when none finished.
fn finished(reports: &[Report]) -> (String, TurnEnd, Option<String>) {
    reports
        .iter()
        .find_map(|report| match report {
            Report::Finished { text, end, cause } => Some((text.clone(), *end, cause.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the turn never finished: {:?}", shown(reports)))
}

/// Tells whether a process runs exactly `command` now. Input: the command line. Output:
/// whether `ps` lists one whose whole command line is that.
fn running(command: &str) -> bool {
    processes_with(command).iter().any(|line| {
        line.trim_start()
            .split_once(' ')
            .is_some_and(|(_, listed)| listed.trim() == command)
    })
}

/// A real Codex sub-agent in a tmux pane on a host of its own, as the owner's agents run.
/// Input: the prefix of its temporary folder. Output: that folder, which must outlive the
/// agent, the host and the prepared agent.
fn in_a_pane(prefix: &str) -> (tempfile::TempDir, Arc<dyn Host>, ManagedAgent) {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &base.join(FOLDER)));
    let agent = ManagedAgent::in_pane("codex", &codex(), host.clone(), AgentRole::Sub);
    (root, host, agent)
}

/// SECAUDIT-5-LIVE G1: under Stop, a real Codex sub-agent in a pane, on the owner's
/// signed-in account, runs a script with its shell tool inside the agent's Seatbelt profile
/// and its process-info deny. The script leaves a `nohup` sleep and a sleep in a session of
/// its own. The turn ends in success with no problem, both sleeps were seen running, and
/// within `REAPED` after the turn no process names either sleep or the agent's home. Runs in
/// a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn under_stop_a_real_codex_agent_s_shell_grandchild_ends_with_its_turn() {
    let (_root, host, mut agent) = in_a_pane("ca-live-codex-bg-");
    agent.background = BackgroundAtEnd::Stop;
    let mut signed_in = SignedIn::codex();
    let tag = std::process::id() % 100_000;
    let kept = format!("/bin/sleep 300.{tag:05}");
    let own_session = format!("/bin/sleep 301.{tag:05}");
    let script = agent.working_dir.join("background.sh");
    std::fs::write(
        &script,
        format!(
            "/usr/bin/nohup {kept} > /dev/null 2>&1 &\n/usr/bin/perl -mPOSIX -e 'POSIX::setsid(); exec \"/bin/sleep\", \"301.{tag:05}\"' > /dev/null 2>&1 &\necho started\n"
        ),
    )
    .unwrap();
    let home = agent.home.to_str().unwrap().to_owned();
    let seen = Arc::new([AtomicBool::new(false), AtomicBool::new(false)]);
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (seen, done) = (seen.clone(), done.clone());
        let sleeps = [kept.clone(), own_session.clone()];
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                for (sleep, flag) in sleeps.iter().zip(seen.iter()) {
                    if running(sleep) {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };
    let prompt = format!(
        "Use your shell tool to run `/bin/sh {}` exactly once. Do not wait for or check on what it starts. Then reply with exactly the word STARTED and end your turn.",
        script.display()
    );
    let turns = vec![(
        prompt.as_str(),
        signed_in.acquire("codex-live-background-stop"),
    )];
    let (reports, problem) = agent.run_turns(None, turns, CODEX_TURN);
    done.store(true, Ordering::SeqCst);
    watcher.join().unwrap();
    let (text, end, cause) = finished(&reports);
    println!("real codex background stop: end={end:?} cause={cause:?} reply={text:?}");
    let deadline = Instant::now() + REAPED;
    let left = loop {
        let mut left = processes_with(&kept);
        left.extend(processes_with(&own_session));
        left.extend(processes_with(&home));
        if left.is_empty() || Instant::now() >= deadline {
            break left;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    host.close(&[]).unwrap();
    assert_eq!(problem, None);
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("STARTED"), "{text}");
    assert!(
        seen[0].load(Ordering::SeqCst),
        "the agent's nohup sleep never ran: {text}"
    );
    assert!(
        seen[1].load(Ordering::SeqCst),
        "the agent's sleep in a session of its own never ran: {text}"
    );
    assert!(
        left.is_empty(),
        "processes of the agent outlived its turn: {left:?}"
    );
}

/// Runs the first turn of a real Codex sub-agent in a pane on the owner's signed-in
/// account. With `planted`, its worktree first gets a repository `.codex` whose MCP server
/// would create a marker file in the worktree when started. Inputs: whether to plant it and
/// the launch's name. Output: the turn's reply, end and cause, the problem that ended the
/// worker, and whether the marker appeared by `MARKER_GRACE` after the turn.
fn first_turn_in_a_pane(
    planted: bool,
    launch: &str,
) -> (String, TurnEnd, Option<String>, Option<String>, bool) {
    let (_root, host, mut agent) = in_a_pane("ca-live-codex-pane-");
    let mut signed_in = SignedIn::codex();
    let marker = agent.working_dir.join("plant-started");
    if planted {
        let folder = agent.working_dir.join(".codex");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(
            folder.join("config.toml"),
            format!(
                "[mcp_servers.plant]\ncommand = \"/bin/sh\"\nargs = [\"-c\", \"/usr/bin/touch {}; /bin/sleep 20\"]\n",
                marker.display()
            ),
        )
        .unwrap();
    }
    let turns = vec![(FIRST_PROMPT, signed_in.acquire(launch))];
    let (reports, problem) = agent.run_turns(None, turns, CODEX_TURN);
    let deadline = Instant::now() + MARKER_GRACE;
    let mut ran = marker.exists();
    while !ran && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        ran = marker.exists();
    }
    host.close(&[]).unwrap();
    let (text, end, cause) = finished(&reports);
    (text, end, cause, problem, ran)
}

/// CODEX-UNTRUSTED-LIVE G3: a real Codex sub-agent on the owner's signed-in account starts
/// in a pane in a worktree with no `.codex`, which its launch records trusted, so no folder
/// access screen holds its first prompt: the turn ends in success with the asked word. Runs
/// in a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_agent_in_a_pane_takes_its_first_prompt_with_no_folder_screen() {
    let (text, end, cause, problem, _) = first_turn_in_a_pane(false, "codex-live-pane-trusted");
    println!("real codex pane, no .codex: end={end:?} cause={cause:?} reply={text:?}");
    assert_eq!(problem, None);
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("OK"), "{text}");
}

/// CODEX-UNTRUSTED-LIVE G4: the same agent with a repository `.codex` in its worktree,
/// whose MCP server would create a marker, starts recorded untrusted behind Codex's folder
/// access screen. The engine gets it past the screen, the first prompt still ends in success
/// with the asked word, and the repository's server never starts. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_agent_in_a_pane_with_a_codex_folder_gets_past_the_screen_and_starts_nothing_from_it()
 {
    let (text, end, cause, problem, ran) = first_turn_in_a_pane(true, "codex-live-pane-untrusted");
    println!(
        "real codex pane, .codex planted: end={end:?} cause={cause:?} reply={text:?} plant_started={ran}"
    );
    assert_eq!(problem, None);
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("OK"), "{text}");
    assert!(!ran, "the repository's .codex MCP server started");
}
