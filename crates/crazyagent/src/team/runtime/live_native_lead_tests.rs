use super::managed_agent::{AGENT, ManagedAgent, NativeTeam, SignedIn, claude};
use super::{Order, Report, TurnEnd};
use crate::sandbox::policy::AgentAccess;
use crate::team::AgentRole;
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{AgentId, FOLDER, Host, harness_socket, teammates_socket};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TURN: Duration = Duration::from_secs(180);
const STOP: Duration = Duration::from_secs(30);
const ONE: &str = "Reply with exactly the word ONE and nothing else.";
const TWO: &str = "Reply with exactly the word TWO and nothing else.";
const COUNT: &str = "Write every whole number from 1 to 600 in order, one per line, then a last line with only the word COUNTED. Use no tools.";
const TEAM_TURN: Duration = Duration::from_secs(420);
const POLL: Duration = Duration::from_millis(250);

/// A real Claude Code lead prepared by the team runtime on the owner's signed-in account,
/// in a pane of a tmux host of its own test folder. Output: the lead and the folder.
fn native_lead() -> (ManagedAgent, tempfile::TempDir) {
    let root = tempfile::Builder::new()
        .prefix("ca-live-lead-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
    let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &base.join(FOLDER)));
    let lead = ManagedAgent::in_pane("claude-code", &claude(), host, AgentRole::Sub);
    (lead, root)
}

/// The same lead started as a real worker of its team runtime at `access`, with its first
/// turn granted on the owner's account, as the runtime's first dispatch to it does.
/// Inputs: the held account, the launch's name and the saved level. Output: the team and
/// the first turn's reports, the finished turn last.
fn native_worker(
    signed_in: &mut SignedIn,
    launch: &str,
    access: AgentAccess,
) -> (NativeTeam, Vec<Report>) {
    let (lead, root) = native_lead();
    let team = lead.into_native_worker(signed_in.acquire(launch), ONE, TURN, access, Some(root));
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
    let (mut lead, _root) = native_lead();
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
    let (mut lead, _root) = native_lead();
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
        team.lead().stopping && team.lead().lowered,
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
    team.order(Order::Turn(TWO.to_owned(), None, Some(TURN)));
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

    /// Reads one teammate pane's screen and recent history as the lead's own tmux client
    /// does: inside the lead's installed profile, on the teammates' server its `TMUX`
    /// names. Input: the pane. Output: the text, empty when the read was refused.
    fn read_as_lead(&self, pane: &str) -> String {
        crate::spawn_gate::output(
            Command::new(crate::sandbox::SANDBOX_EXEC)
                .arg("-f")
                .arg(&self.profile)
                .arg(&self.tmux)
                .arg("-S")
                .arg(&self.mates)
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

    /// Polls both servers until `done` is set or `wait` passed. Inputs: the lead's anchor
    /// pane, the run's token, the stop flag and the longest wait. Output: what it saw.
    fn watch(&self, anchor: &str, token: &str, done: &AtomicBool, wait: Duration) -> Seen {
        let deadline = Instant::now() + wait;
        let mut seen = Seen::default();
        while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
            match (self.panes(&self.own), self.panes(&self.mates)) {
                (Some(own), Some(mates)) => {
                    seen.own.extend(own.into_keys());
                    for (pane, window) in mates {
                        if pane != anchor && self.read_as_lead(&pane).contains(token) {
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
/// profile reads that pane, and the lead's turn ends with the teammate's word.
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
        None,
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
        own.len(),
        1,
        "the lead's own server holds only the lead: {own:?}"
    );
    assert_eq!(
        mates.len(),
        1,
        "the teammates' server holds only the lead's anchor before any teammate: {mates:?}"
    );
    let (anchor, window) = mates.into_iter().next().unwrap();
    let done = AtomicBool::new(false);
    let (reports, seen) = std::thread::scope(|scope| {
        let watcher = scope.spawn(|| servers.watch(&anchor, &token, &done, TEAM_TURN));
        let reports = team.until_finished(TEAM_TURN);
        done.store(true, Ordering::SeqCst);
        (reports, watcher.join().unwrap())
    });
    let turns = finished(&reports);
    let teammates: BTreeMap<&String, &String> = seen
        .mates
        .iter()
        .filter(|(pane, _)| **pane != anchor)
        .collect();
    println!(
        "real claude native team: own={:?} mates={:?} read={:?} misses={} turns={turns:?}",
        seen.own, seen.mates, seen.read, seen.misses
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
    assert!(answered(&turns, &token), "{turns:?}");
    team.order(Order::Stop);
    let (_, problem) = team.until_ended(STOP);
    assert_eq!(problem, None);
}
