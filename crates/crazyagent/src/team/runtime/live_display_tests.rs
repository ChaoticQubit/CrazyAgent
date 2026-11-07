use super::super::TurnEnd;
use super::super::managed_agent::{AGENT, ManagedAgent, SignedIn, claude};
use super::{TURN, processes_with};
use crate::coord::settings::ClaudeDisplay;
use crate::team::AgentRole;
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{AgentId, FOLDER, Host};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ASK_SUM: &str = "What is 2468 plus 1357? Reply with only the number.";
const SUM: &str = "3825";
const ALTERNATE_SCREEN_ON: &[u8] = b"\x1b[?1049h";
const MOUSE_ON: [&[u8]; 4] = [
    b"\x1b[?1000h",
    b"\x1b[?1002h",
    b"\x1b[?1003h",
    b"\x1b[?1006h",
];
const LOOK: Duration = Duration::from_millis(200);

/// What the app's view of a running Claude Code pane starts from, as the engine's snapshot
/// gives it to a new viewer: how many snapshots were read, whether any put the view on the
/// alternate screen or turned mouse reporting on, and the display variables the running
/// Claude Code process had.
#[derive(Debug, Default)]
struct Seen {
    snapshots: usize,
    alternate: bool,
    mouse: bool,
    variables: Vec<String>,
}

/// Whether `bytes` hold `wanted`. Inputs: the bytes and the sequence. Output: whether.
fn holds(bytes: &[u8], wanted: &[u8]) -> bool {
    bytes.windows(wanted.len()).any(|window| window == wanted)
}

/// The N-D9 display variables a running process has, read from its environment by `ps`.
/// Input: the process id. Output: each `name=value` of the three variables it has.
fn display_variables(pid: &str) -> Vec<String> {
    let listed = crate::spawn_gate::output(
        std::process::Command::new("/bin/ps")
            .args(["-E", "-ww", "-o", "command=", "-p", pid])
            .stdout(std::process::Stdio::piped()),
    )
    .unwrap();
    String::from_utf8_lossy(&listed.stdout)
        .split_whitespace()
        .filter(|word| {
            word.starts_with("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=")
                || word.starts_with("CLAUDE_CODE_SCROLL_SPEED=")
                || word.starts_with("CLAUDE_CODE_NO_FLICKER=")
        })
        .map(str::to_owned)
        .collect()
}

/// Runs one real Claude Code sub-agent turn in a pane under `display`, the way the team
/// runtime runs a managed agent, and watches the pane while the turn runs. Input: the
/// display. Output: the reply text, how the turn ended, its cause and what the watcher saw.
fn watched_turn(display: ClaudeDisplay) -> (String, TurnEnd, Option<String>, Seen) {
    let root = tempfile::Builder::new()
        .prefix("ca-live-display-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &base.join(FOLDER)));
    let agent = ManagedAgent::in_pane_with_display(
        "claude-code",
        &claude(),
        host.clone(),
        AgentRole::Sub,
        display,
    );
    let mut signed_in = SignedIn::claude();
    let home = agent.home.to_str().unwrap().to_owned();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (seen, done, host) = (seen.clone(), done.clone(), host.clone());
        std::thread::spawn(move || {
            let pane = AgentId::team(AGENT).unwrap();
            while !done.load(Ordering::SeqCst) {
                if let Ok(stream) = host.snapshot(&pane) {
                    let mut seen = seen.lock().unwrap();
                    seen.snapshots += 1;
                    seen.alternate |= holds(&stream.screen, ALTERNATE_SCREEN_ON);
                    seen.mouse |= MOUSE_ON.iter().any(|on| holds(&stream.screen, on));
                    if seen.variables.is_empty() {
                        for line in processes_with(&home) {
                            let pid = line.split_whitespace().next().unwrap_or_default();
                            let variables = display_variables(pid);
                            if !variables.is_empty() {
                                seen.variables = variables;
                                break;
                            }
                        }
                    }
                }
                std::thread::sleep(LOOK);
            }
        })
    };
    let profile = signed_in.acquire(match display {
        ClaudeDisplay::Classic => "claude-live-display-classic",
        ClaudeDisplay::Fullscreen => "claude-live-display-fullscreen",
    });
    let (text, end, cause) = agent.run_turn(profile, ASK_SUM, TURN);
    done.store(true, Ordering::SeqCst);
    watcher.join().unwrap();
    drop(agent);
    host.close(&[]).unwrap();
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    (text, end, cause, seen)
}

/// N-D9 Classic: a real Claude Code sub-agent the team runtime runs in a pane on the
/// owner's managed account, in a project at the Classic display, answers, and while it runs
/// its process has `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1` and no snapshot of its pane puts
/// the app's view on the alternate screen or turns mouse reporting on, so the view scrolls
/// with the wheel at once and a plain drag selects. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_claude_pane_at_classic_leaves_the_wheel_and_selection_to_the_view() {
    let (text, end, cause, seen) = watched_turn(ClaudeDisplay::Classic);
    println!(
        "real claude classic display: end={end:?} cause={cause:?} reply={text:?} seen={seen:?}"
    );
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains(SUM), "{text}");
    assert!(seen.snapshots > 0, "the pane was never seen running");
    assert_eq!(seen.variables, ["CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1"]);
    assert!(!seen.alternate, "Classic drew on the alternate screen");
    assert!(!seen.mouse, "Classic turned mouse reporting on");
}

/// N-D9 Fullscreen: the same turn in a project at the Fullscreen display answers, and while
/// it runs its process has `CLAUDE_CODE_SCROLL_SPEED=3` and `CLAUDE_CODE_NO_FLICKER=1`
/// (R8-FS) and its pane is on the alternate screen with mouse reporting on, Claude Code's
/// own view, whatever the account's own renderer choice. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_claude_pane_at_fullscreen_keeps_claude_code_s_own_view() {
    let (text, end, cause, seen) = watched_turn(ClaudeDisplay::Fullscreen);
    println!(
        "real claude fullscreen display: end={end:?} cause={cause:?} reply={text:?} seen={seen:?}"
    );
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains(SUM), "{text}");
    assert!(seen.snapshots > 0, "the pane was never seen running");
    assert_eq!(
        seen.variables,
        ["CLAUDE_CODE_SCROLL_SPEED=3", "CLAUDE_CODE_NO_FLICKER=1"]
    );
    assert!(
        seen.alternate,
        "Fullscreen never drew on the alternate screen"
    );
    assert!(seen.mouse, "Fullscreen never turned mouse reporting on");
}
