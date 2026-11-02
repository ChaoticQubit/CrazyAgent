use super::managed_agent::{AGENT, ManagedAgent, NativeTeam, SignedIn, claude};
use super::{Order, Report, TurnEnd};
use crate::sandbox::policy::AgentAccess;
use crate::team::AgentRole;
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{FOLDER, Host};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TURN: Duration = Duration::from_secs(180);
const STOP: Duration = Duration::from_secs(30);
const ONE: &str = "Reply with exactly the word ONE and nothing else.";
const TWO: &str = "Reply with exactly the word TWO and nothing else.";
const COUNT: &str = "Write every whole number from 1 to 600 in order, one per line, then a last line with only the word COUNTED. Use no tools.";

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
    let team = lead.into_native_worker(signed_in.acquire(launch), ONE, TURN, access, root);
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
    assert_eq!(team.lead().grants, 0, "the runtime took no profile for it");
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
