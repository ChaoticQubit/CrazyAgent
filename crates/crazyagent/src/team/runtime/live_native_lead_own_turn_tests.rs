use super::super::managed_agent::{AGENT, NativeTeam, SignedIn};
use super::super::{BACKGROUND_CUT, BACKGROUND_STOPPED, Order, Report, TurnEnd, TurnRules};
use super::{STOP, TURN, answered, finished, native_worker, own_ended, own_started};
use crate::sandbox::policy::AgentAccess;
use crate::team::BackgroundAtEnd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SEEN: Duration = Duration::from_secs(90);
const REAPED: Duration = Duration::from_secs(10);
const WAIT_SECS: u64 = 40;
const LONG_SECS: u64 = 300;
const LIMIT_SECS: u32 = 90;

/// The running processes whose command line holds `needle`, read from `ps` through a pipe.
/// Input: the text. Output: each process's pid and command line.
fn processes_with(needle: &str) -> Vec<String> {
    let listed = crate::spawn_gate::output(
        Command::new("/bin/ps")
            .args(["-axww", "-o", "pid=,command="])
            .stdout(Stdio::piped()),
    )
    .unwrap();
    assert!(listed.status.success());
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|line| line.contains(needle))
        .map(str::to_owned)
        .collect()
}

/// Polls `done` every 200 ms until it holds or `wait` passes. Inputs: the longest wait and
/// the check. Output: whether it held.
fn within(wait: Duration, done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A `sleep` of about `secs` seconds whose command line no other process holds: the
/// fraction is this test process's id. Input: the seconds. Output: the command.
fn marker(secs: u64) -> String {
    format!("sleep {secs}.{:05}", std::process::id() % 100_000)
}

/// Saves the project's choice for commands still running in the background at a task's
/// end through the engine's own team action, and its task limit in the table the engine
/// reads it from. The owner's listed limit choices start at 30 min, so the limit row is
/// written as the runtime's unit test writes it, to keep the live run short. No session
/// limit is saved, so the bound a held own turn gets is the task limit, not the session
/// time left. Inputs: the team, the choice and the limit in seconds. Output: the rules the
/// engine now reads.
fn save_rules(team: &NativeTeam, choice: BackgroundAtEnd, limit: Option<u32>) -> TurnRules {
    let project = team.lead().project.clone();
    let access = (team.runtime.access)().expect("the team's pool");
    let tx = team.runtime.connection.unchecked_transaction().unwrap();
    assert_eq!(
        crate::team::execute(
            &tx,
            &access,
            &crate::team::TeamAction::BackgroundAtEnd {
                project_id: project.clone(),
                choice,
            },
        )
        .unwrap(),
        crate::protocol::Outcome::Applied
    );
    if let Some(secs) = limit {
        tx.execute(
            "INSERT INTO team_time_limits(project,turn_secs,session_secs) VALUES (?1,?2,NULL)
             ON CONFLICT(project) DO UPDATE SET turn_secs=excluded.turn_secs",
            rusqlite::params![project.as_str(), secs],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let limits = crate::team::time_limits(&team.runtime.connection, &project).unwrap();
    assert_eq!(
        limits.session_time(),
        None,
        "no session limit is saved, so a held own turn is bound by the task limit alone"
    );
    TurnRules {
        limit: limits.turn_time(),
        background: crate::team::background_at_end(&team.runtime.connection, &project).unwrap(),
    }
}

/// Has the lead's app take a turn by itself, as when someone types at its terminal, in
/// which it starts `marker` as a background command and ends its reply. The runtime records
/// the turn through its own report handling and its next tick's step sends the lead the
/// project's saved rules for it. Inputs: the team, the command and the saved rules.
/// Output: when the turn's start was reported.
fn own_turn_with_background(team: &mut NativeTeam, marker: &str, saved: TurnRules) -> Instant {
    team.type_into_pane(&format!(
        "Use your Bash tool with run_in_background set to true to start the command `{marker}`. Do not wait for it or check on it. Then reply with exactly the word STARTED and end your turn."
    ));
    team.reports_until(TURN, own_started);
    let started = Instant::now();
    team.runtime
        .apply(
            AGENT,
            Report::Own {
                end: None,
                text: String::new(),
            },
        )
        .unwrap();
    team.runtime.send_own_rules().unwrap();
    assert_eq!(
        team.lead().own_rules.map(|(_, rules)| rules),
        Some(saved),
        "the engine sends the lead the project's saved rules for its own turn"
    );
    assert!(
        within(SEEN, || !processes_with(marker).is_empty()),
        "the lead's app never started its background command `{marker}`"
    );
    started
}

/// The end and result text of the own turn whose end closed `reports`. Input: the
/// reports, the own turn's end last. Output: them.
fn own_end(reports: &[Report]) -> (TurnEnd, String) {
    match reports.last() {
        Some(Report::Own {
            end: Some(end),
            text,
        }) => (*end, text.clone()),
        _ => unreachable!("the wait ends on the own turn's end"),
    }
}

/// N-E9 under Wait: a kept native lead's own turn whose background command outlives the
/// app's reply stays open until the command has ended and the app's turn on its end
/// notification is done, and the lead keeps serving after it. On the owner's signed-in
/// account, in the lead's pane on its own server. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs a signed-in Claude account, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_under_wait_a_kept_native_lead_s_own_turn_stays_open_until_its_background_command_ends() {
    let mut signed_in = SignedIn::claude();
    let (mut team, first) = native_worker(
        &mut signed_in,
        "claude-live-own-bg-wait",
        AgentAccess::Sandboxed,
    );
    assert!(answered(&finished(&first), "ONE"), "{:?}", finished(&first));
    let saved = save_rules(&team, BackgroundAtEnd::Wait, None);
    assert_eq!(
        saved,
        TurnRules {
            limit: None,
            background: BackgroundAtEnd::Wait,
        }
    );
    let marker = marker(WAIT_SECS);
    let started = own_turn_with_background(&mut team, &marker, saved);
    let reports = team.reports_until(TURN + Duration::from_secs(WAIT_SECS), own_ended);
    let running = processes_with(&marker);
    let (end, text) = own_end(&reports);
    println!(
        "real claude own turn under Wait: end={end:?} after {:?} text={text:?}",
        started.elapsed()
    );
    assert!(
        running.is_empty(),
        "the own turn ended while its background command still ran: {running:?}"
    );
    assert_eq!(end, TurnEnd::Success, "{text}");
    assert!(
        !text.starts_with(BACKGROUND_STOPPED) && !text.starts_with(BACKGROUND_CUT),
        "under Wait nothing stopped the command: {text}"
    );
    assert!(
        team.pane_alive(),
        "the lead keeps serving after its own turn"
    );
    team.order(Order::Stop);
    let (_, problem) = team.until_ended(STOP);
    assert_eq!(problem, None);
    assert!(!team.pane_alive());
    assert!(!team.home.join(".credentials.json").exists());
}

/// N-E9 under the project's saved Stop: a kept native lead's own turn whose background
/// command outlives the app's reply ends as soon as the app holds it only for that command,
/// says first that the command was stopped, and the lead stops with the command. On the
/// owner's signed-in account. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs a signed-in Claude account, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_under_a_saved_stop_a_kept_native_lead_s_held_own_turn_ends_at_once_and_its_command_stops() {
    let mut signed_in = SignedIn::claude();
    let (mut team, first) = native_worker(
        &mut signed_in,
        "claude-live-own-bg-stop",
        AgentAccess::Sandboxed,
    );
    assert!(answered(&finished(&first), "ONE"), "{:?}", finished(&first));
    let saved = save_rules(&team, BackgroundAtEnd::Stop, None);
    assert_eq!(
        saved,
        TurnRules {
            limit: None,
            background: BackgroundAtEnd::Stop,
        }
    );
    let marker = marker(LONG_SECS);
    let started = own_turn_with_background(&mut team, &marker, saved);
    let reports = team.reports_until(TURN, own_ended);
    let took = started.elapsed();
    let (end, text) = own_end(&reports);
    println!("real claude own turn under Stop: end={end:?} after {took:?} text={text:?}");
    assert_eq!(end, TurnEnd::Success, "{text}");
    assert!(
        text.starts_with(BACKGROUND_STOPPED),
        "the result says first that the command was stopped: {text}"
    );
    let (_, problem) = team.until_ended(STOP);
    assert_eq!(problem, None);
    assert!(
        within(REAPED, || processes_with(&marker).is_empty()),
        "the lead's stop left its background command running: {:?}",
        processes_with(&marker)
    );
    assert!(!team.pane_alive(), "the lead stopped with its command");
    assert!(!team.home.join(".credentials.json").exists());
}

/// N-E9 with the project's saved task limit under Wait: a kept native lead's own turn held
/// for its background command ends at the limit, counted from the turn's start, says first
/// that the limit ended it, and the lead stops with the command. On the owner's signed-in
/// account. Runs in a live window only.
#[test]
#[ignore = "live qualification: needs a signed-in Claude account, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn live_the_saved_task_limit_cuts_a_kept_native_lead_s_own_turn_held_for_its_background_command() {
    let mut signed_in = SignedIn::claude();
    let (mut team, first) = native_worker(
        &mut signed_in,
        "claude-live-own-bg-limit",
        AgentAccess::Sandboxed,
    );
    assert!(answered(&finished(&first), "ONE"), "{:?}", finished(&first));
    let saved = save_rules(&team, BackgroundAtEnd::Wait, Some(LIMIT_SECS));
    let limit = Duration::from_secs(LIMIT_SECS.into());
    assert_eq!(
        saved,
        TurnRules {
            limit: Some(limit),
            background: BackgroundAtEnd::Wait,
        }
    );
    let marker = marker(LONG_SECS);
    let started = own_turn_with_background(&mut team, &marker, saved);
    let reports = team.reports_until(TURN, own_ended);
    let took = started.elapsed();
    let (end, text) = own_end(&reports);
    println!("real claude own turn at its limit: end={end:?} after {took:?} text={text:?}");
    assert_eq!(end, TurnEnd::TimedOut, "{text}");
    assert!(
        text.starts_with(BACKGROUND_CUT),
        "the result says first that the limit ended it: {text}"
    );
    assert!(
        took + Duration::from_secs(5) >= limit,
        "the limit ended the turn after {took:?}, before its {limit:?}"
    );
    let (_, problem) = team.until_ended(STOP);
    assert_eq!(problem, None);
    assert!(
        within(REAPED, || processes_with(&marker).is_empty()),
        "the limit left the background command running: {:?}",
        processes_with(&marker)
    );
    assert!(!team.pane_alive(), "the lead stopped with its command");
    assert!(!team.home.join(".credentials.json").exists());
}
