use super::managed_agent::{AGENT, ManagedAgent, NativeTeam, SignedIn, claude, shown};
use super::{Order, Report, TurnEnd, TurnRules};
use crate::sandbox::policy::AgentAccess;
use crate::terminal::tmux::installed;
use crate::terminal::{AgentId, harness_socket, teammates_socket};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TURN: Duration = Duration::from_secs(180);
const STOP: Duration = Duration::from_secs(30);
const ONE: &str = "Reply with exactly the word ONE and nothing else.";
const TWO: &str = "Reply with exactly the word TWO and nothing else.";
const COUNT: &str = "Write every whole number from 1 to 600 in order, one per line, then a last line with only the word COUNTED. Use no tools.";
const BASH_ONE: &str = "Use your Bash tool to run the command `echo snapshot-check`, then reply with exactly the word ONE and nothing else.";
const BASH_TWO: &str = "Use your Bash tool to run the command `echo snapshot-check`, then reply with exactly the word TWO and nothing else.";
const TEAM_TURN: Duration = Duration::from_secs(420);
const POLL: Duration = Duration::from_millis(250);
const GRACE: Duration = Duration::from_secs(30);

/// A real Claude Code lead the team runtime plans and starts as a native-mode lead on the
/// owner's signed-in account, on the engine's own terminal host, started as a real worker of
/// its team runtime at `access` with its first turn granted, as the runtime's first dispatch
/// to it does. Inputs: the held account, the launch's name and the saved level. Output: the
/// team and the first turn's reports, the finished turn last.
fn native_worker(
    signed_in: &mut SignedIn,
    launch: &str,
    access: AgentAccess,
) -> (NativeTeam, Vec<Report>) {
    let lead = ManagedAgent::engine_native_lead(&claude());
    let team = lead.into_native_worker(signed_in.acquire(launch), ONE, TURN, access);
    let first = team.until_finished(TURN);
    (team, first)
}

fn ready(reports: &[Report]) -> Vec<bool> {
    reports
        .iter()
        .filter_map(|report| match report {
            Report::Ready { pane, .. } => Some(pane.is_some()),
            _ => None,
        })
        .collect()
}

fn own_started(report: &Report) -> bool {
    matches!(report, Report::Own { end: None, .. })
}

fn own_ended(report: &Report) -> bool {
    matches!(report, Report::Own { end: Some(_), .. })
}

fn answered(turns: &[(String, TurnEnd, Option<String>)], word: &str) -> bool {
    turns.len() == 1 && turns[0].1 == TurnEnd::Success && turns[0].0.contains(word)
}

fn finished(reports: &[Report]) -> Vec<(String, TurnEnd, Option<String>)> {
    reports
        .iter()
        .filter_map(|report| match report {
            Report::Finished { text, end, cause } => Some((text.clone(), *end, cause.clone())),
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_native_lead_managed_keeps_one_session() {
    let mut signed_in = SignedIn::claude();
    let mut lead = ManagedAgent::engine_native_lead(&claude());
    let profile = signed_in.acquire("claude-live-native-lead");
    let (reports, problem) = lead.run_native_lead(
        profile,
        &[
            "Reply with exactly the word ONE and nothing else.",
            "Reply with exactly the word TWO and nothing else.",
        ],
        TURN,
        false,
    );
    let turns = finished(&reports);
    println!("real claude native lead: problem={problem:?} turns={turns:?}");
    assert_eq!(problem, None);
    let started: Vec<bool> = reports
        .iter()
        .filter_map(|report| match report {
            Report::Ready { pane, .. } => Some(pane.is_some()),
            _ => None,
        })
        .collect();
    assert_eq!(
        started,
        vec![true],
        "the app starts once, and its pane record is kept"
    );
    assert_eq!(turns.len(), 2, "both turns run on the one session");
    assert!(
        turns.iter().all(|(_, end, _)| *end == TurnEnd::Success),
        "{turns:?}"
    );
    assert!(turns[0].0.contains("ONE") && turns[1].0.contains("TWO"));
    assert!(
        !lead.home.join(".credentials.json").exists(),
        "the credential copy ends with the session"
    );
}

#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_native_lead_managed_adopted_gets_credential() {
    let mut signed_in = SignedIn::claude();
    let mut lead = ManagedAgent::engine_native_lead(&claude());
    let (reports, problem) = lead.run_native_lead(
        signed_in.acquire("claude-live-native-left"),
        &["Reply with exactly the word ONE and nothing else."],
        TURN,
        true,
    );
    assert_eq!(problem, None, "{:?}", finished(&reports));
    let record = reports
        .into_iter()
        .find_map(|report| match report {
            Report::Ready {
                pane: Some(record), ..
            } => Some(record),
            _ => None,
        })
        .expect("the lead's pane record");
    assert!(
        !lead.home.join(".credentials.json").exists(),
        "at engine quit the pane is left and the credential copy removed"
    );
    let (reports, problem) = lead.adopt_native_lead(
        record,
        signed_in.acquire("claude-live-native-adopted"),
        "Reply with exactly the word TWO and nothing else.",
        TURN,
    );
    let turns = finished(&reports);
    println!("real claude adopted native lead: problem={problem:?} turns={turns:?}");
    assert_eq!(problem, None);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].1, TurnEnd::Success, "{turns:?}");
    assert!(
        turns[0].0.contains("TWO"),
        "the credential handed again signs the adopted app in"
    );
}

/// A kept native lead granted once serves the next task dispatch hands it with no grant, on
/// the same session. The lead's reports are read here, not by the runtime's report handling,
/// so the task stays running in the engine's records after the lead answered it.
#[test]
#[ignore = "live qualification: needs a signed-in Claude account, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_dispatch_grants_kept_session_once() {
    let mut signed_in = SignedIn::claude();
    let (mut team, first) = native_worker(
        &mut signed_in,
        "claude-live-dispatch-once",
        AgentAccess::Sandboxed,
    );
    let turns = finished(&first);
    println!("real claude kept lead, granted turn: {turns:?}");
    assert_eq!(
        ready(&first),
        vec![true],
        "the app starts once, on its granted turn"
    );
    assert!(answered(&turns, "ONE"), "{turns:?}");
    assert!(
        team.home.join(".credentials.json").exists(),
        "the granted credential copy stays while the session runs"
    );
    let task = team.queue(TWO);
    assert_eq!(team.task(task).0, "queued");
    team.dispatch();
    assert_eq!(
        team.task(task),
        ("running".to_owned(), Some(AGENT), None),
        "the briefed kept session takes the task with no grant; a grant needs a sign-in this engine never recorded"
    );
    assert!(
        !team.lead().stopping,
        "no grant was tried, so no refused sign-in stopped the lead"
    );
    let second = team.until_finished(TURN);
    let turns = finished(&second);
    println!("real claude kept lead, dispatched task: {turns:?}");
    assert!(
        ready(&second).is_empty(),
        "the task runs on the session already started"
    );
    assert!(answered(&turns, "TWO"), "{turns:?}");
    team.order(Order::Stop);
    let (_, problem) = team.until_ended(STOP);
    assert_eq!(problem, None);
    assert!(
        !team.home.join(".credentials.json").exists(),
        "the credential copy ends with the session"
    );
}

#[test]
#[ignore = "live qualification: needs a signed-in Claude account, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_retire_lowered_stops_idle_kept_session() {
    let mut signed_in = SignedIn::claude();
    let (mut team, first) = native_worker(
        &mut signed_in,
        "claude-live-lowered",
        AgentAccess::OwnBranch,
    );
    let turns = finished(&first);
    println!("real claude kept lead at Own branch: {turns:?}");
    assert!(answered(&turns, "ONE"), "{turns:?}");
    let lead = team.lead();
    assert!(
        lead.kept_session && lead.busy.is_none() && lead.access == AgentAccess::OwnBranch,
        "the lead idles on its kept session, started at Own branch"
    );
    assert!(team.pane_alive());
    team.save_access(AgentAccess::Sandboxed);
    assert!(
        team.lead().stopping && team.lead().narrowed.is_some(),
        "lowering the level stops the idle kept lead at once, not at its idle limit"
    );
    let (reports, problem) = team.until_ended(STOP);
    assert_eq!(problem, None);
    assert!(finished(&reports).is_empty(), "no turn runs after the stop");
    assert!(!team.pane_alive(), "the lead's app ended with its session");
    assert!(
        !team.home.join(".credentials.json").exists(),
        "the credential copy ends with the session"
    );
}

#[test]
#[ignore = "live qualification: needs a signed-in Claude account, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_order_held_during_own_turn() {
    let mut signed_in = SignedIn::claude();
    let (team, first) = native_worker(
        &mut signed_in,
        "claude-live-own-turn",
        AgentAccess::Sandboxed,
    );
    let turns = finished(&first);
    assert!(answered(&turns, "ONE"), "{turns:?}");
    team.type_into_pane(COUNT);
    team.reports_until(TURN, own_started);
    team.order(Order::Turn(
        TWO.to_owned(),
        None,
        TurnRules {
            limit: Some(TURN),
            background: crate::team::BackgroundAtEnd::Wait,
        },
    ));
    let after = team.until_finished(TURN * 2);
    let turns = finished(&after);
    let own = after
        .iter()
        .position(own_ended)
        .expect("the own turn's end is reported before the held turn finishes");
    let Report::Own {
        end: Some(end),
        text,
    } = &after[own]
    else {
        unreachable!("own_ended matched an own turn's end");
    };
    println!(
        "real claude own turn: end={end:?} bytes={} held turn: {turns:?}",
        text.len()
    );
    assert_eq!(*end, TurnEnd::Success);
    assert!(text.contains("COUNTED"), "the own turn ran to its end");
    assert!(
        answered(&turns, "TWO") && !turns[0].0.contains("COUNTED"),
        "the held turn is typed only after the own turn ended: {turns:?}"
    );
    team.type_into_pane(COUNT);
    team.reports_until(TURN, own_started);
    let stopped = Instant::now();
    team.order(Order::Stop);
    let (reports, problem) = team.until_ended(STOP);
    println!(
        "stop during an own turn ended the lead in {:?}",
        stopped.elapsed()
    );
    assert_eq!(problem, None);
    assert!(
        !reports.iter().any(own_ended),
        "a stop is not held: the lead ends before its own turn does"
    );
    assert!(!team.pane_alive());
    assert!(!team.home.join(".credentials.json").exists());
}

/// The two tmux servers of one native lead and how the test reads them: tmux, the lead's
/// own server's socket, the teammates' server's socket and the lead's installed profile.
/// Dropping it ends both servers, after the team stopped the lead, so a failed assertion
/// leaves no signed-in teammate running.
struct LeadServers {
    tmux: PathBuf,
    own: PathBuf,
    mates: PathBuf,
    profile: PathBuf,
}

/// What the test saw on a native lead's two servers while its turn ran: every pane of the
/// lead's own server, every pane of the teammates' server with its window, the teammate
/// panes whose text the lead's client read with the run's token, and the polls where a
/// server did not answer.
#[derive(Default)]
struct Seen {
    own: BTreeSet<String>,
    mates: BTreeMap<String, String>,
    read: BTreeSet<String>,
    misses: usize,
}

impl LeadServers {
    /// Lists every pane one server holds, read by the test from outside every sandbox.
    /// Input: the server's socket. Output: each pane id with its window id, or none when
    /// the server did not answer.
    fn panes(&self, socket: &Path) -> Option<BTreeMap<String, String>> {
        let output = crate::spawn_gate::output(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(socket)
                .args(["list-panes", "-a", "-F", "#{pane_id} #{window_id}"])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .ok()
        .filter(|output| output.status.success())?;
        Some(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.split_once(' '))
                .map(|(pane, window)| (pane.to_owned(), window.to_owned()))
                .collect(),
        )
    }

    /// Reads one pane's screen and recent history as the lead's own tmux client does:
    /// inside the lead's installed profile, on the lead's own server or the teammates' one
    /// its `TMUX` names. Inputs: the server's socket and the pane. Output: the text, empty
    /// when the read was refused.
    fn read_as_lead(&self, socket: &Path, pane: &str) -> String {
        crate::spawn_gate::output(
            Command::new(crate::sandbox::SANDBOX_EXEC)
                .arg("-f")
                .arg(&self.profile)
                .arg(&self.tmux)
                .arg("-S")
                .arg(socket)
                .args(["capture-pane", "-p", "-J", "-S", "-200", "-t", pane])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
    }

    /// Every pane of both servers with its text as the lead's client reads it, so a failed
    /// wait shows what the lead and its teammates had on screen. Output: each server's
    /// socket, pane and text.
    fn screens(&self) -> Vec<(PathBuf, String, String)> {
        [&self.own, &self.mates]
            .into_iter()
            .flat_map(|socket| {
                self.panes(socket)
                    .unwrap_or_default()
                    .into_keys()
                    .map(|pane| {
                        let text = self.read_as_lead(socket, &pane);
                        (socket.clone(), pane, text)
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Polls both servers until `wait` passed, or `done` is set and either the lead's client
    /// read a teammate pane holding the token or `GRACE` passed since. Inputs: the lead's
    /// anchor pane, the run's token, the stop flag and the longest wait. Output: what it saw.
    fn watch(&self, anchor: &str, token: &str, done: &AtomicBool, wait: Duration) -> Seen {
        let mut ends = Instant::now() + wait;
        let mut seen = Seen::default();
        while Instant::now() < ends {
            if done.load(Ordering::SeqCst) {
                if !seen.read.is_empty() {
                    break;
                }
                ends = ends.min(Instant::now() + GRACE);
            }
            match (self.panes(&self.own), self.panes(&self.mates)) {
                (Some(own), Some(mates)) => {
                    seen.own.extend(own.into_keys());
                    for (pane, window) in mates {
                        if pane != anchor && self.read_as_lead(&self.mates, &pane).contains(token) {
                            seen.read.insert(pane.clone());
                        }
                        seen.mates.insert(pane, window);
                    }
                }
                _ => seen.misses += 1,
            }
            std::thread::sleep(POLL);
        }
        seen
    }
}

impl Drop for LeadServers {
    fn drop(&mut self) {
        for socket in [&self.own, &self.mates] {
            let _ = crate::spawn_gate::status(
                Command::new(&self.tmux)
                    .arg("-S")
                    .arg(socket)
                    .arg("kill-server")
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
        }
    }
}

/// How a turn of the lead ended and its text, when the text holds `token`, whether the
/// engine typed the turn or a teammate's message started it. Inputs: one report and the
/// token. Output: the end and the text, or none.
fn token_turn(report: &Report, token: &str) -> Option<(TurnEnd, String)> {
    match report {
        Report::Finished { text, end, .. }
        | Report::Own {
            end: Some(end),
            text,
        } if text.contains(token) => Some((*end, text.clone())),
        _ => None,
    }
}

/// The lead's one turn: create one teammate and pass its message on. Input: the run's
/// token. Output: the prompt.
fn team_prompt(token: &str) -> String {
    format!(
        "Use your agent team for this. Spawn exactly one teammate. Tell it to send you a message with exactly the word {token} and nothing else. Wait until that message reaches you, then reply with exactly the word {token} and nothing else."
    )
}

/// TTDL residual 1, live: a native Claude Code lead the runtime plans and starts on the
/// engine's own terminal host gets the teammates' server's anchor as `TMUX` and
/// `TMUX_PANE`. The teammate it creates is split off that anchor's window in the teammates'
/// server and never appears in the lead's own server, the lead's own tmux client inside its
/// profile reads that pane, and a turn of the lead ends in success with the run's word. A
/// wait that fails prints what decides why before it panics: the watcher's view, both
/// servers' screens, the team folders' files with their sizes and both transcripts' team
/// calls (row 5, live22).
#[test]
#[ignore = "live qualification: needs a signed-in Claude account, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_native_lead_splits_teammates_into_mates_server() {
    let mut signed_in = SignedIn::claude();
    let lead = ManagedAgent::engine_native_lead(&claude());
    let name = AgentId::team(AGENT).unwrap();
    let servers = LeadServers {
        tmux: installed(&lead.base).expect("BLOCKED: tmux must be installed"),
        own: harness_socket(&lead.base, &name).unwrap(),
        mates: teammates_socket(&lead.base, &name).unwrap(),
        profile: lead.sandbox_profile.clone(),
    };
    let token = format!(
        "MATES{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let team = lead.into_native_worker(
        signed_in.acquire("claude-live-mates"),
        &team_prompt(&token),
        TEAM_TURN,
        AgentAccess::Sandboxed,
    );
    let started = team.reports_until(TEAM_TURN, |report| matches!(report, Report::Ready { .. }));
    assert_eq!(
        ready(&started),
        vec![true],
        "the lead's app starts once, with its pane record"
    );
    let own = servers
        .panes(&servers.own)
        .expect("the lead's own server answers");
    let mates = servers
        .panes(&servers.mates)
        .expect("the teammates' server answers");
    assert_eq!(
        (own.len(), own.values().collect::<BTreeSet<_>>().len()),
        (2, 2),
        "the lead's own server holds only its holder window and the lead's, one pane each: {own:?}"
    );
    assert_eq!(
        mates.len(),
        1,
        "the teammates' server holds only the lead's anchor before any teammate: {mates:?}"
    );
    let (anchor, window) = mates.into_iter().next().unwrap();
    let done = AtomicBool::new(false);
    let (waited, seen) = std::thread::scope(|scope| {
        let watcher = scope.spawn(|| servers.watch(&anchor, &token, &done, TEAM_TURN));
        let waited = team.reports_within(TEAM_TURN, |report| token_turn(report, &token).is_some());
        done.store(true, Ordering::SeqCst);
        (waited, watcher.join().unwrap())
    });
    let reports = waited.unwrap_or_else(|(why, earlier)| {
        println!(
            "watcher own={:?} mates={:?} read={:?} misses={}",
            seen.own, seen.mates, seen.read, seen.misses
        );
        for (socket, pane, text) in servers.screens() {
            println!("screen {} {pane}:\n{text}", socket.display());
        }
        let sized = |files: Vec<PathBuf>| -> Vec<(PathBuf, u64)> {
            files
                .into_iter()
                .map(|file| {
                    let bytes = std::fs::metadata(&file).map_or(0, |found| found.len());
                    (file, bytes)
                })
                .collect()
        };
        let mates = crate::launch_context::mates_home(&team.home);
        println!(
            "mates transcripts={:?} teams={:?} tasks={:?}",
            sized(files_under(&mates.join("projects"), 3)),
            sized(files_under(&team.home.join("teams"), 3)),
            sized(files_under(&team.home.join("tasks"), 2))
        );
        println!(
            "lead team calls={:?} teammate team calls={:?}",
            team_calls(&files_under(&team.home.join("projects"), 3)),
            team_calls(&files_under(&mates.join("projects"), 3))
        );
        panic!(
            "{why}, after {} others: {:?}",
            earlier.len(),
            shown(&earlier)
        )
    });
    let (end, text) = reports
        .last()
        .and_then(|report| token_turn(report, &token))
        .expect("the wait ends on the lead's turn holding the token");
    let teammates: BTreeMap<&String, &String> = seen
        .mates
        .iter()
        .filter(|(pane, _)| **pane != anchor)
        .collect();
    println!(
        "real claude native team: own={:?} mates={:?} read={:?} misses={} end={end:?} text={text:?} turns={:?}",
        seen.own,
        seen.mates,
        seen.read,
        seen.misses,
        finished(&reports)
    );
    assert_eq!(
        seen.misses, 0,
        "both servers answered through the whole turn"
    );
    assert!(
        !teammates.is_empty(),
        "Claude Code split no teammate pane into the teammates' server"
    );
    assert!(
        teammates.values().all(|split| **split == window),
        "each teammate pane is split off the anchor's window, the TMUX_PANE the lead got"
    );
    assert_eq!(
        seen.own,
        own.into_keys().collect::<BTreeSet<_>>(),
        "no teammate pane ever appeared in the lead's own server"
    );
    assert!(
        !seen.read.is_empty(),
        "the lead's tmux client read no teammate pane holding {token}"
    );
    assert_eq!(end, TurnEnd::Success, "{text:?}");
    team.order(Order::Stop);
    let (_, problem) = team.until_ended(STOP);
    assert_eq!(problem, None);
}

/// A native lead's teammate cannot plant a command in the shell snapshot its lead's Claude
/// Code runs before every Bash command (rv-audit TPH r1 F2). The real lead runs one Bash
/// call, so its snapshot exists under its config home. A process in the installed teammates'
/// profile appends a command to each snapshot there and must be refused, so a snapshot the
/// lead writes again cannot pass for the refusal, and the lead's next Bash call must not run
/// it.
#[test]
#[ignore = "live qualification: needs a signed-in Claude account, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_a_teammate_cannot_plant_a_command_in_its_lead_s_shell_snapshot() {
    let mut signed_in = SignedIn::claude();
    let lead = ManagedAgent::engine_native_lead(&claude());
    let team = lead.into_native_worker(
        signed_in.acquire("claude-live-snapshot"),
        BASH_ONE,
        TURN,
        AgentAccess::Sandboxed,
    );
    let first = finished(&team.until_finished(TURN));
    assert!(answered(&first, "ONE"), "{first:?}");
    let folder = team.home.join("shell-snapshots");
    let snapshots: Vec<PathBuf> = std::fs::read_dir(&folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("snapshot-") && name.ends_with(".sh"))
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !snapshots.is_empty(),
        "the lead's Claude Code keeps its shell snapshot in {}",
        folder.display()
    );
    let teammates = super::agent_teammates_profile(&team.runtime.state, AGENT);
    assert!(
        teammates.is_file(),
        "the native lead's launch installed its teammates' profile"
    );
    let marker = team.home.join("snapshot-planted.txt");
    let planted = crate::spawn_gate::output(
        Command::new(crate::sandbox::SANDBOX_EXEC)
            .arg("-f")
            .arg(&teammates)
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!(
                "for f in '{folder}'/snapshot-*.sh; do printf 'echo planted > {marker}\\n' >> \"$f\" || exit 1; done",
                folder = folder.display(),
                marker = marker.display(),
            ))
            .current_dir(&team.home)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &team.home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    team.order(Order::Turn(
        BASH_TWO.to_owned(),
        None,
        TurnRules {
            limit: Some(TURN),
            background: crate::team::BackgroundAtEnd::Wait,
        },
    ));
    let second = finished(&team.until_finished(TURN));
    println!(
        "real claude lead after a teammate's snapshot write: append={:?} turns={second:?}",
        planted.status.code()
    );
    assert!(answered(&second, "TWO"), "{second:?}");
    assert!(
        !planted.status.success(),
        "a teammate appended to its lead's shell snapshot: {}",
        String::from_utf8_lossy(&planted.stderr)
    );
    assert!(
        !marker.exists(),
        "a teammate's line in its lead's shell snapshot ran in the lead's next Bash call: {}",
        String::from_utf8_lossy(&planted.stderr)
    );
}

/// Every file under `root`, at most `depth` folders down and `FILES_MAX` in all, read by
/// the test from outside every sandbox. Inputs: the folder and the depth. Output: the files.
fn files_under(root: &Path, depth: usize) -> Vec<PathBuf> {
    const FILES_MAX: usize = 4096;
    let mut found = Vec::new();
    let mut folders = vec![(root.to_path_buf(), 0)];
    while let Some((folder, level)) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && level < depth {
                folders.push((entry.path(), level + 1));
            } else if kind.is_file() && found.len() < FILES_MAX {
                found.push(entry.path());
            }
        }
    }
    found
}

/// row5-diag r1 L1: the failure printout's walk never follows a link, neither a linked root
/// nor a linked folder or file inside a plain root, so it lists only the root's own files.
#[test]
fn files_under_follows_no_link() {
    let base = tempfile::tempdir().unwrap();
    let outside = base.path().join("outside");
    std::fs::create_dir_all(outside.join("deep")).unwrap();
    std::fs::write(outside.join("secret.jsonl"), "{}\n").unwrap();
    std::fs::write(outside.join("deep").join("more.jsonl"), "{}\n").unwrap();
    let root = base.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("own.jsonl"), "{}\n").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.jsonl"), root.join("file.jsonl")).unwrap();
    let linked_root = base.path().join("linked-root");
    std::os::unix::fs::symlink(&root, &linked_root).unwrap();

    assert_eq!(files_under(&root, 3), vec![root.join("own.jsonl")]);
    assert_eq!(files_under(&linked_root, 3), Vec::<PathBuf>::new());
    assert_eq!(files_under(&root.join("linked"), 3), Vec::<PathBuf>::new());
}

/// The team tool uses Claude Code transcripts record and the result each got: every
/// `TeamCreate`, `Agent` and `SendMessage` call with its input, then its tool result, each
/// cut to `SHOWN_MAX` characters, so a failed wait shows whether a lead tried to start a
/// teammate and what Claude Code answered. Input: the files. Output: one line per use or
/// result, in file order.
fn team_calls(files: &[PathBuf]) -> Vec<String> {
    const SHOWN_MAX: usize = 400;
    const TEAM_TOOLS: [&str; 3] = ["TeamCreate", "Agent", "SendMessage"];
    let cut = |text: String| text.chars().take(SHOWN_MAX).collect::<String>();
    let mut uses = BTreeSet::new();
    let mut lines = Vec::new();
    for text in files
        .iter()
        .filter(|file| {
            file.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .filter_map(|file| std::fs::read_to_string(file).ok())
    {
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let blocks = entry
                .pointer("/message/content")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            for block in blocks {
                let field = |name: &str| block.get(name).cloned().unwrap_or_default();
                match (field("type").as_str(), field("name").as_str()) {
                    (Some("tool_use"), Some(name)) if TEAM_TOOLS.contains(&name) => {
                        uses.insert(field("id").to_string());
                        lines.push(format!("use {name} {}", cut(field("input").to_string())));
                    }
                    (Some("tool_result"), _)
                        if uses.contains(&field("tool_use_id").to_string()) =>
                    {
                        lines.push(format!(
                            "result error={} {}",
                            field("is_error"),
                            cut(field("content").to_string())
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    lines
}

/// The files of `files` whose text holds `word`. Inputs: the files and the word. Output:
/// the matching files.
fn holding(files: &[PathBuf], word: &str) -> Vec<PathBuf> {
    files
        .iter()
        .filter(|file| std::fs::read_to_string(file).is_ok_and(|text| text.contains(word)))
        .cloned()
        .collect()
}

/// Tells whether one line of the files holds every one of `words`, such as a teammate
/// message Claude Code recorded in a transcript together with its text. Inputs: the files
/// and the words. Output: the answer.
fn line_holding(files: &[PathBuf], words: &[&str]) -> bool {
    files
        .iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .any(|text| {
            text.lines()
                .any(|line| words.iter().all(|word| line.contains(word)))
        })
}

/// The texts of the tool results that are not errors, for every `tool` call in a Claude Code
/// transcript among the files whose input holds every one of `words`. Inputs: the files, the
/// tool name and the words. Output: the result texts, in file order.
fn succeeded(files: &[PathBuf], tool: &str, words: &[&str]) -> Vec<String> {
    let mut calls = BTreeSet::new();
    let mut results = Vec::new();
    for text in files
        .iter()
        .filter(|file| {
            file.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .filter_map(|file| std::fs::read_to_string(file).ok())
    {
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let blocks = entry
                .pointer("/message/content")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            for block in blocks {
                let field = |name: &str| block.get(name).cloned().unwrap_or_default();
                match field("type").as_str() {
                    Some("tool_use") if field("name").as_str() == Some(tool) => {
                        let input = field("input").to_string();
                        if words.iter().all(|word| input.contains(word)) {
                            calls.insert(field("id").to_string());
                        }
                    }
                    Some("tool_result")
                        if calls.contains(&field("tool_use_id").to_string())
                            && field("is_error") != serde_json::Value::Bool(true) =>
                    {
                        results.push(match field("content") {
                            serde_json::Value::String(text) => text,
                            serde_json::Value::Array(parts) => parts
                                .iter()
                                .filter_map(|part| part.get("text")?.as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                            other => other.to_string(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    results
}

/// The id Claude Code 2.1.309 gave the task one of `results` reports, from its TaskCreate
/// result `Task #<id> created successfully: <subject>`. Input: the TaskCreate result texts.
/// Output: the first id, or none.
fn created_task(results: &[String]) -> Option<String> {
    results.iter().find_map(|text| {
        let (id, rest) = text.strip_prefix("Task #")?.split_once(' ')?;
        (!id.is_empty() && rest.starts_with("created successfully")).then(|| id.to_owned())
    })
}

/// The lead's name in its Claude Code team, which names its inbox `inboxes/team-lead.json`.
const LEAD_NAME: &str = "team-lead";

/// The names and agent ids of the lead's teammates, read from every `config.json` among
/// `shared`, the lead itself left out. Input: the files under the lead's `teams/`. Output:
/// the names and ids.
fn teammate_names(shared: &[PathBuf]) -> BTreeSet<String> {
    shared
        .iter()
        .filter(|file| file.file_name().is_some_and(|name| name == "config.json"))
        .filter_map(|file| std::fs::read(file).ok())
        .filter_map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .flat_map(|config| {
            config
                .get("members")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter(|member| member.get("name").and_then(serde_json::Value::as_str) != Some(LEAD_NAME))
        .flat_map(|member| {
            ["name", "agentId"]
                .into_iter()
                .filter_map(|key| member.get(key)?.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The lead's one turn for the TPH-2 C live check: make a task, create one teammate that
/// claims and completes it, send that teammate a word and pass its answer on. Input: the
/// run's token. Output: the prompt.
fn own_home_prompt(token: &str) -> String {
    format!(
        "Use your agent team for this. First create a task in your team's task list with exactly the title {token}TASK. Then spawn exactly one teammate and tell it: claim the task titled {token}TASK and mark it completed, then wait for a message from the team lead and send the team lead back the exact word that message holds followed by OK, and nothing else. Then send the teammate a message with exactly the word {token}MSG. Wait until the teammate's answer reaches you and the task shows completed, then reply with exactly the teammate's answer and nothing else."
    )
}

/// TPH-2 C, live: a native lead's teammates run from their own config home and reach the
/// lead only through the team folders the lead shares. A real Claude Code lead, started by
/// the runtime on the owner's account, makes a task, creates one teammate, messages it and
/// waits for its answer. Checked: the teammate starts signed in and answers, past every
/// first-run, trust and sign-in screen (1); its answer to the lead's message reaches the lead
/// through the lead's own inbox in the linked `teams/`: that inbox file exists and the lead's
/// transcript records the answer as a teammate message, which tmux teammates deliver only
/// through it (2); it, not the lead, completes the task the lead made in its own `tasks/`, so
/// the teammate's own update completing the id the lead's TaskCreate reported succeeds
/// through the linked folder (3). Claude Code
/// 2.1.309 deletes delivered inbox entries and, once every task is completed, the task files,
/// so (2) and (3) read the transcripts, not those files. The teammate's transcript sits in
/// the mates home, where the engine's record walk, which usage and the relay's read receipts
/// use, finds it (4); and Seatbelt denied nothing at or under the lead's `teams` or `tasks`
/// folders during the run (5).
#[test]
#[ignore = "live qualification: needs a signed-in Claude account, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_native_teammates_work_from_their_own_home_through_the_lead_s_team_folders() {
    let mut signed_in = SignedIn::claude();
    let started_at = crate::spawn_gate::output(
        Command::new("/bin/date")
            .arg("+%Y-%m-%d %H:%M:%S")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    .unwrap();
    let lead = ManagedAgent::engine_native_lead(&claude());
    let name = AgentId::team(AGENT).unwrap();
    let servers = LeadServers {
        tmux: installed(&lead.base).expect("BLOCKED: tmux must be installed"),
        own: harness_socket(&lead.base, &name).unwrap(),
        mates: teammates_socket(&lead.base, &name).unwrap(),
        profile: lead.sandbox_profile.clone(),
    };
    let token = format!(
        "OWNHOME{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let answer = format!("{token}MSGOK");
    let team = lead.into_native_worker(
        signed_in.acquire("claude-live-own-home"),
        &own_home_prompt(&token),
        TEAM_TURN,
        AgentAccess::Sandboxed,
    );
    let mates = crate::launch_context::mates_home(&team.home);
    let reports = team
        .reports_within(TEAM_TURN, |report| token_turn(report, &answer).is_some())
        .unwrap_or_else(|(why, seen)| {
            for (socket, pane, text) in servers.screens() {
                println!("screen {} {pane}:\n{text}", socket.display());
            }
            println!(
                "mates transcripts={:?} shared={:?} tasks={:?}",
                files_under(&mates.join("projects"), 3),
                files_under(&team.home.join("teams"), 3),
                files_under(&team.home.join("tasks"), 2)
            );
            println!(
                "lead team calls={:?} teammate team calls={:?}",
                team_calls(&files_under(&team.home.join("projects"), 3)),
                team_calls(&files_under(&mates.join("projects"), 3))
            );
            panic!("{why}, after {} others: {:?}", seen.len(), shown(&seen))
        });
    let (end, text) = reports
        .last()
        .and_then(|report| token_turn(report, &answer))
        .expect("the wait ends on the lead's turn holding the teammate's answer");
    let transcripts = files_under(&mates.join("projects"), 3);
    let shared = files_under(&team.home.join("teams"), 3);
    let tasks = files_under(&team.home.join("tasks"), 2);
    let (walked, _, _) =
        crate::usage::capture::records(&team.home, crate::usage::capture::Format::Claude);
    let denials = crate::spawn_gate::output(
        Command::new("/usr/bin/log")
            .args(["show", "--style", "compact", "--start", &started_at, "--predicate"])
            .arg("eventMessage CONTAINS \"deny\" AND (eventMessage CONTAINS \"/teams\" OR eventMessage CONTAINS \"/tasks\")")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let lead_folders = [
        format!("/agent-{AGENT}/home/teams"),
        format!("/agent-{AGENT}/home/tasks"),
    ];
    let denied: Vec<String> = String::from_utf8_lossy(&denials.stdout)
        .lines()
        .filter(|line| {
            lead_folders
                .iter()
                .any(|folder| line.contains(folder.as_str()))
        })
        .map(str::to_owned)
        .collect();
    let inbox = format!("{LEAD_NAME}.json");
    let lead_inboxes: Vec<PathBuf> = shared
        .iter()
        .filter(|file| {
            file.file_name().is_some_and(|name| name == inbox.as_str())
                && file
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|folder| folder == "inboxes")
        })
        .cloned()
        .collect();
    let mates_names = teammate_names(&shared);
    let lead_transcripts = files_under(&team.home.join("projects"), 3);
    let task = format!("{token}TASK");
    let made = created_task(&succeeded(
        &lead_transcripts,
        "TaskCreate",
        &[task.as_str()],
    ));
    let bound = made.as_deref().map(|id| format!(r#""taskId":"{id}""#));
    let completing = |files: &[PathBuf]| {
        bound.as_deref().is_some_and(|id| {
            !succeeded(files, "TaskUpdate", &[id, r#""status":"completed""#]).is_empty()
        })
    };
    let completed_by_teammate = completing(&transcripts);
    let completed_by_lead = completing(&lead_transcripts);
    let delivered = line_holding(&lead_transcripts, &["teammate-message", answer.as_str()]);
    println!(
        "real claude native team in its own home: end={end:?} text={text:?} transcripts={transcripts:?} lead_transcripts={lead_transcripts:?} shared={shared:?} lead_inboxes={lead_inboxes:?} delivered={delivered} teammates={mates_names:?} tasks={tasks:?} made={made:?} completed_by_teammate={completed_by_teammate} completed_by_lead={completed_by_lead} walked={walked:?} denied={denied:?} log_exit={:?} turns={:?}",
        denials.status.code(),
        finished(&reports)
    );
    assert_eq!(end, TurnEnd::Success, "{text:?}");
    let teammate = holding(&transcripts, &format!("{token}MSG"));
    assert!(
        !teammate.is_empty(),
        "no teammate transcript in its own home {} holds the lead's message",
        mates.display()
    );
    assert!(
        !holding(&teammate, &answer).is_empty(),
        "the teammate's own transcript holds no answer to the lead's message"
    );
    assert!(
        !lead_inboxes.is_empty() && delivered,
        "the teammate's answer did not reach the lead through the lead's own inbox in the lead's teams folder: inboxes {lead_inboxes:?} delivered {delivered} {shared:?}"
    );
    assert!(
        !mates_names.is_empty(),
        "the lead's team config names no teammate: {shared:?}"
    );
    assert!(
        made.is_some() && completed_by_teammate && !completed_by_lead,
        "the task the lead made was not completed by its teammate through the lead's own tasks folder: made {made:?} by teammate {completed_by_teammate} by lead {completed_by_lead} tasks={tasks:?}"
    );
    assert!(
        walked
            .iter()
            .any(|record| record.starts_with(&mates) && teammate.contains(record)),
        "the engine's record walk misses the teammate's transcript: {walked:?}"
    );
    assert!(
        denials.status.success(),
        "the Seatbelt log could not be read: {}",
        String::from_utf8_lossy(&denials.stderr)
    );
    assert!(
        denied.is_empty(),
        "Seatbelt denied a write in the lead's team folders: {denied:?}"
    );
    team.order(Order::Stop);
    let (_, problem) = team.until_ended(STOP);
    assert_eq!(problem, None);
}

#[path = "live_native_lead_own_turn_tests.rs"]
mod own_turn;
