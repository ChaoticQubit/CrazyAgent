use super::live_claude_tests::processes_with;
use super::managed_agent::{
    Closing, Kept, ManagedAgent, SignedIn, codex, keep_rollouts, open_plain, real_codex,
    rollout_files, shown,
};
use super::{Report, TurnEnd};
use crate::team::{AgentRole, BackgroundAtEnd};
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{AgentId, FOLDER, Host};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CODEX_TURN: Duration = Duration::from_secs(180);
const REAPED: Duration = Duration::from_secs(10);
const MARKER_GRACE: Duration = Duration::from_secs(5);
const FIRST_PROMPT: &str = "Reply with exactly the word OK and nothing else.";
/// The most characters of one rollout tool record a row prints (G1-DIAG).
const CALL_SHOWN: usize = 400;
/// The rollout record kinds that carry a tool call or its output.
const TOOL_RECORDS: [&str; 5] = [
    "function_call",
    "function_call_output",
    "custom_tool_call",
    "custom_tool_call_output",
    "local_shell_call",
];
/// The title row of Codex's folder access screen, as the engine's own screen read finds it.
const FOLDER_SCREEN_TITLE: &str = "Folder access";
/// How often a row reads its agent's pane. The engine leaves the folder access screen up
/// for at least the 700 ms its first screen must stay quiet before it answers.
const SCREEN_LOOK: Duration = Duration::from_millis(100);

/// Reads every rollout Codex wrote below an agent home's sessions folder, through
/// [`rollout_files`], which follows no link. Input: the agent home. Output: the rollouts'
/// text, joined.
fn rollouts(home: &Path) -> String {
    rollout_files(home)
        .unwrap()
        .files
        .iter()
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
        .collect()
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
    let written = rollouts(&agent.home);
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

/// One tool record of a rollout as a row prints it: its kind, the tool's name, and its
/// arguments, input, action or output as text, cut to `CALL_SHOWN` characters. Input: the
/// record's payload. Output: the line.
fn call_line(payload: &serde_json::Value) -> String {
    let body = ["arguments", "input", "action", "output"]
        .iter()
        .map(|key| &payload[key])
        .find(|value| !value.is_null())
        .map_or_else(String::new, |value| {
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned)
        });
    let line = format!(
        "{} {} {body}",
        payload["type"].as_str().unwrap_or("-"),
        payload["name"].as_str().unwrap_or("-")
    );
    line.chars().take(CALL_SHOWN).collect()
}

/// What the real Codex wrote of its sessions under an agent home: each rollout's name, how
/// many entries the no-link read skipped, the `cli_version` each session record names, the
/// text of each assistant message and each tool call and output as [`call_line`] prints it.
struct CodexTrace {
    rollouts: Vec<OsString>,
    skipped: usize,
    versions: Vec<String>,
    said: Vec<String>,
    calls: Vec<String>,
}

impl CodexTrace {
    /// Reads the rollouts under an agent home's `sessions` folder through [`rollout_files`],
    /// which follows no link. Input: the agent home. Output: the trace, empty when Codex
    /// wrote none or the sessions folder is a link.
    fn of(home: &Path) -> Self {
        let read = rollout_files(home).unwrap();
        let (mut versions, mut said, mut calls) = (Vec::new(), Vec::new(), Vec::new());
        for (_, bytes) in &read.files {
            for line in String::from_utf8_lossy(bytes).lines() {
                let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let payload = &entry["payload"];
                if entry["type"] == "session_meta" {
                    versions.extend(payload["cli_version"].as_str().map(str::to_owned));
                } else if entry["type"] == "response_item"
                    && payload["type"] == "message"
                    && payload["role"] == "assistant"
                {
                    said.extend(
                        payload["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|part| part["type"] == "output_text")
                            .filter_map(|part| part["text"].as_str().map(str::to_owned)),
                    );
                } else if entry["type"] == "response_item"
                    && payload["type"]
                        .as_str()
                        .is_some_and(|kind| TOOL_RECORDS.contains(&kind))
                {
                    calls.push(call_line(payload));
                }
            }
        }
        Self {
            rollouts: read.files.into_iter().map(|(name, _)| name).collect(),
            skipped: read.skipped,
            versions,
            said,
            calls,
        }
    }

    /// Prints the rollouts, the skipped count, the Codex versions and each tool record, one
    /// line each, so a failed row's log shows what Codex ran. Output: none.
    fn print(&self) {
        println!(
            "codex rollouts={:?} skipped={} cli_version={:?}",
            self.rollouts, self.skipped, self.versions
        );
        for call in &self.calls {
            assert!(call.chars().count() <= CALL_SHOWN);
            println!("codex tool {call:?}");
        }
    }

    /// Checks that the real Codex itself gave a row's reply: a rollout whose session record
    /// names a Codex version, and an assistant message there holding `word`. Input: the
    /// word. Output: none.
    fn assert_said(&self, word: &str) {
        assert!(!self.rollouts.is_empty(), "Codex wrote no rollout");
        assert!(
            self.versions.iter().any(|version| !version.is_empty()),
            "no rollout names its Codex version"
        );
        assert!(
            self.said.iter().any(|text| text.contains(word)),
            "no assistant message in the rollouts holds {word}: {:?}",
            self.said
        );
    }
}

/// Watches a pane for Codex's folder access screen until `done` is set, reading its visible
/// screen every `SCREEN_LOOK` as the engine reads it. Inputs: the pane's host and name and
/// the stop flag. Output: the watch, whose result is how many separate times the screen
/// appeared.
fn folder_screens(
    host: Arc<dyn Host>,
    pane: AgentId,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<usize> {
    std::thread::spawn(move || {
        let (mut screens, mut showing) = (0, false);
        while !done.load(Ordering::SeqCst) {
            if let Ok(screen) = host.screen(&pane) {
                let menu = crate::team::events::screen_rows(&screen)
                    .iter()
                    .any(|row| row.trim() == FOLDER_SCREEN_TITLE);
                if menu && !showing {
                    screens += 1;
                }
                showing = menu;
            }
            std::thread::sleep(SCREEN_LOOK);
        }
        screens
    })
}

/// A real Codex sub-agent in a tmux pane on a host of its own, as the owner's agents run,
/// refused while `CRAZYAGENT_CODEX` names another program. Input: the prefix of its
/// temporary folder. Output: that folder, the host, which closes when dropped, and the
/// prepared agent, to be dropped in reverse order.
fn in_a_pane(prefix: &str) -> (tempfile::TempDir, Closing, ManagedAgent) {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &base.join(FOLDER)));
    let agent = ManagedAgent::in_pane("codex", &real_codex(), host.clone(), AgentRole::Sub);
    (root, Closing::new(host), agent)
}

/// SECAUDIT-5-LIVE G1: under Stop, the real Codex as a sub-agent in a pane, on the owner's
/// signed-in account, runs a script with its shell tool inside the agent's Seatbelt profile
/// and its process-info deny. The script leaves a `nohup` sleep and a sleep in a session of
/// its own, each writing a marker first. The turn ends in success with no problem, the real
/// Codex's rollout holds the reply, both markers exist, the session sleep was seen running
/// (Codex 0.164 itself ends the `nohup` one with the command's group, G1-DIAG), and within
/// `REAPED` no process names either sleep or the agent's home. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn under_stop_a_real_codex_agent_s_shell_grandchild_ends_with_its_turn() {
    let (_root, host, mut agent) = in_a_pane("ca-live-codex-bg-");
    agent.background = BackgroundAtEnd::Stop;
    let mut signed_in = SignedIn::codex();
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let kept = format!("/bin/sleep 300.{tag:09}");
    let own_session = format!("/bin/sleep 301.{tag:09}");
    let script = agent.working_dir.join("background.sh");
    let ran = [
        agent.working_dir.join("kept-ran"),
        agent.working_dir.join("own-ran"),
    ];
    let [kept_ran, own_ran] = [&ran[0], &ran[1]].map(|path| path.to_str().unwrap().to_owned());
    assert!(
        [&kept_ran, &own_ran].iter().all(|path| path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))),
        "a marker path needs quoting: {kept_ran} {own_ran}"
    );
    std::fs::write(
        &script,
        format!(
            "/usr/bin/nohup /bin/sh -c 'echo ran > {kept_ran}; exec {kept}' > /dev/null 2>&1 &\n/usr/bin/perl -mPOSIX -e 'POSIX::setsid(); exec \"/bin/sh\", \"-c\", \"echo ran > {own_ran}; exec {own_session}\"' > /dev/null 2>&1 &\ntries=0\nwhile [ ! -e {kept_ran} ] || [ ! -e {own_ran} ]; do\n  tries=$((tries + 1))\n  [ \"$tries\" -gt 50 ] && break\n  /bin/sleep 0.1\ndone\necho started\n"
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
    host.close();
    let seen = [
        seen[0].load(Ordering::SeqCst),
        seen[1].load(Ordering::SeqCst),
    ];
    let marked = [ran[0].exists(), ran[1].exists()];
    println!(
        "real codex background stop: seen nohup={} session={} markers nohup={} session={} left={}",
        seen[0],
        seen[1],
        marked[0],
        marked[1],
        left.len()
    );
    let trace = CodexTrace::of(&agent.home);
    trace.print();
    let (text, end, cause) = finished(&reports);
    println!("real codex background stop: end={end:?} cause={cause:?} reply={text:?}");
    assert_eq!(problem, None);
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("STARTED"), "{text}");
    trace.assert_said("STARTED");
    assert!(marked[0], "the agent's nohup sleep never started: {text}");
    assert!(
        marked[1],
        "the agent's sleep in a session of its own never started: {text}"
    );
    assert!(
        seen[1],
        "the agent's sleep in a session of its own never ran: {text}"
    );
    assert!(
        left.is_empty(),
        "processes of the agent outlived its turn: {left:?}"
    );
}

/// G1-DIAG: the live keep and the trace read each plain `.jsonl` file below an agent home's
/// sessions folder, nested ones too, and nothing through a link the agent could plant: not a
/// linked file or folder inside, not a hard-linked file, not a file swapped for a link after
/// it was listed, and not a sessions folder that is itself a link. A second keep never writes
/// over a kept file, and a home with no sessions folder keeps nothing.
#[test]
fn the_live_keep_copies_only_the_rollouts_under_the_agent_home() {
    use rustix::fs::{CWD, Mode, OFlags};
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let (home, outside, keep) = (base.join("home"), base.join("outside"), base.join("keep"));
    let sessions = home.join("sessions");
    std::fs::create_dir_all(sessions.join("2026").join("11")).unwrap();
    std::fs::create_dir_all(outside.join("deeper")).unwrap();
    std::fs::write(
        sessions.join("2026").join("11").join("rollout-a.jsonl"),
        "{}\n",
    )
    .unwrap();
    std::fs::write(sessions.join("rollout-b.jsonl"), "{}\n").unwrap();
    std::fs::write(sessions.join("notes.txt"), "notes\n").unwrap();
    std::fs::write(outside.join("owner.jsonl"), "{}\n").unwrap();
    std::fs::write(outside.join("deeper").join("owner.jsonl"), "{}\n").unwrap();
    std::os::unix::fs::symlink(outside.join("owner.jsonl"), sessions.join("linked.jsonl")).unwrap();
    std::os::unix::fs::symlink(&outside, sessions.join("linked")).unwrap();
    std::fs::hard_link(outside.join("owner.jsonl"), sessions.join("hard.jsonl")).unwrap();
    assert_eq!(
        keep_rollouts(&home, &keep).unwrap(),
        Kept {
            copied: 2,
            skipped: 4
        },
        "notes.txt, the linked file, the linked folder and the hard link are skipped"
    );
    let mut kept: Vec<String> = std::fs::read_dir(&keep)
        .unwrap()
        .map(|item| item.unwrap().file_name().into_string().unwrap())
        .collect();
    kept.sort();
    assert_eq!(kept, ["rollout-a.jsonl", "rollout-b.jsonl"]);
    assert_eq!(
        keep_rollouts(&home, &keep).unwrap(),
        Kept {
            copied: 0,
            skipped: 6
        },
        "a second keep never writes over a kept file"
    );
    let folders = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let listed = rustix::fs::openat(CWD, &sessions, folders, Mode::empty()).unwrap();
    assert!(open_plain(&listed, c"rollout-b.jsonl").is_some());
    assert!(
        open_plain(&listed, c"linked.jsonl").is_none(),
        "a name listed as a file and swapped for a link by the time it opens"
    );
    assert!(open_plain(&listed, c"hard.jsonl").is_none());
    let linked_home = base.join("linked-home");
    std::fs::create_dir_all(&linked_home).unwrap();
    std::os::unix::fs::symlink(&sessions, linked_home.join("sessions")).unwrap();
    let none = base.join("keep-none");
    assert_eq!(
        keep_rollouts(&linked_home, &none).unwrap(),
        Kept {
            copied: 0,
            skipped: 1
        }
    );
    assert!(!none.exists());
    let traced = CodexTrace::of(&linked_home);
    assert!(traced.rollouts.is_empty() && traced.calls.is_empty());
    assert_eq!(traced.skipped, 1);
    let bare = base.join("bare-home");
    std::fs::create_dir_all(&bare).unwrap();
    assert_eq!(
        keep_rollouts(&bare, &none).unwrap(),
        Kept {
            copied: 0,
            skipped: 0
        }
    );
    assert!(!none.exists());
}

/// What one first turn of a real Codex sub-agent in a pane showed: its reply, end and
/// cause, the problem that ended the worker, how many separate times the folder access
/// screen appeared, whether the planted server's marker appeared, and what Codex wrote.
struct FirstTurn {
    text: String,
    end: TurnEnd,
    cause: Option<String>,
    problem: Option<String>,
    screens: usize,
    ran: bool,
    trace: CodexTrace,
}

/// Control for a planted `.codex`: runs the MCP server command the planted config names,
/// read back from that config, inside the agent's own Seatbelt profile and worktree, and
/// checks it makes its marker, then removes the marker. So a marker missing after the
/// turn means Codex never started the server, not that the command cannot run there.
/// Inputs: the agent, the planted config and the marker. Output: none.
fn planted_server_makes_its_marker(agent: &ManagedAgent, config: &str, marker: &Path) {
    let table: toml::Table = toml::from_str(config).unwrap();
    let server = &table["mcp_servers"]["plant"];
    let program = server["command"].as_str().unwrap();
    let arguments = server["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|argument| argument.as_str().unwrap())
        .collect::<Vec<_>>();
    let ran =
        crate::spawn_gate::output(&mut agent.command(Path::new(program), &arguments)).unwrap();
    assert!(
        ran.status.success() && marker.exists(),
        "control: the planted server's own command made no marker inside the agent's sandbox"
    );
    std::fs::remove_file(marker).unwrap();
}

/// Runs the first turn of the real Codex as a sub-agent in a pane on the owner's signed-in
/// account, watching the pane for the folder access screen. With `planted`, its worktree
/// first gets a repository `.codex` naming an MCP server that would create a marker file
/// in the worktree when started, and the control shows that command makes the marker.
/// Inputs: whether to plant it and the launch's name. Output: what the turn showed, the
/// marker read by `MARKER_GRACE` after the turn.
fn first_turn_in_a_pane(planted: bool, launch: &str) -> FirstTurn {
    let (_root, host, mut agent) = in_a_pane("ca-live-codex-pane-");
    let mut signed_in = SignedIn::codex();
    let marker = agent.working_dir.join("plant-started");
    if planted {
        let folder = agent.working_dir.join(".codex");
        std::fs::create_dir(&folder).unwrap();
        let config = format!(
            "[mcp_servers.plant]\ncommand = \"/usr/bin/touch\"\nargs = [{}]\n",
            serde_json::to_string(marker.to_str().unwrap()).unwrap()
        );
        std::fs::write(folder.join("config.toml"), &config).unwrap();
        planted_server_makes_its_marker(&agent, &config, &marker);
    }
    let (pane_host, pane) = agent.plan.pane.clone().expect("a pane agent has its pane");
    let done = Arc::new(AtomicBool::new(false));
    let watch = folder_screens(pane_host, pane, done.clone());
    let turns = vec![(FIRST_PROMPT, signed_in.acquire(launch))];
    let (reports, problem) = agent.run_turns(None, turns, CODEX_TURN);
    done.store(true, Ordering::SeqCst);
    let screens = watch.join().unwrap();
    let deadline = Instant::now() + MARKER_GRACE;
    let mut ran = marker.exists();
    while !ran && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        ran = marker.exists();
    }
    host.close();
    let (text, end, cause) = finished(&reports);
    FirstTurn {
        text,
        end,
        cause,
        problem,
        screens,
        ran,
        trace: CodexTrace::of(&agent.home),
    }
}

/// CODEX-UNTRUSTED-LIVE G3: the real Codex as a sub-agent on the owner's signed-in account
/// starts in a pane in a worktree with no `.codex`, which its launch records trusted. The
/// folder access screen never shows in the pane, the first prompt ends in success with the
/// asked word, and the real Codex's rollout holds that reply. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_agent_in_a_pane_takes_its_first_prompt_with_no_folder_screen() {
    let turn = first_turn_in_a_pane(false, "codex-live-pane-trusted");
    println!(
        "real codex pane, no .codex: end={:?} cause={:?} reply={:?} folder_screens={}",
        turn.end, turn.cause, turn.text, turn.screens
    );
    turn.trace.print();
    assert_eq!(turn.problem, None);
    assert_eq!(turn.end, TurnEnd::Success, "{:?}", turn.cause);
    assert!(turn.text.contains("OK"), "{}", turn.text);
    turn.trace.assert_said("OK");
    assert_eq!(turn.screens, 0, "the folder access screen showed");
}

/// CODEX-UNTRUSTED-LIVE G4: the same agent with a repository `.codex` in its worktree,
/// whose MCP server's command makes a marker when run inside the agent's sandbox (the
/// control), starts recorded untrusted. The folder access screen shows in the pane exactly
/// once and the engine gets the agent past it, the first prompt still ends in success with
/// the asked word in the real Codex's rollout, and the repository's server never starts.
/// Runs in a live window only.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_agent_in_a_pane_with_a_codex_folder_gets_past_the_screen_and_starts_nothing_from_it()
 {
    let turn = first_turn_in_a_pane(true, "codex-live-pane-untrusted");
    println!(
        "real codex pane, .codex planted: end={:?} cause={:?} reply={:?} folder_screens={} plant_started={}",
        turn.end, turn.cause, turn.text, turn.screens, turn.ran
    );
    turn.trace.print();
    assert_eq!(turn.problem, None);
    assert_eq!(turn.end, TurnEnd::Success, "{:?}", turn.cause);
    assert!(turn.text.contains("OK"), "{}", turn.text);
    turn.trace.assert_said("OK");
    assert_eq!(
        turn.screens, 1,
        "the folder access screen did not show exactly once"
    );
    assert!(!turn.ran, "the repository's .codex MCP server started");
}
