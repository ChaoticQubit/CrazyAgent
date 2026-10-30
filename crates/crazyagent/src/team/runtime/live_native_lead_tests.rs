use super::managed_agent::{ManagedAgent, SignedIn, claude};
use super::{Report, TurnEnd};
use crate::team::AgentRole;
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{FOLDER, Host};
use std::sync::Arc;
use std::time::Duration;

const TURN: Duration = Duration::from_secs(180);

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
