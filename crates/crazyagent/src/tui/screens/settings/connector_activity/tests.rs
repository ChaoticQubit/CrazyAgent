use super::*;
use crate::connectors::audit::{ActivityView, CallKind, CallView, ProjectChoice};
use crate::connectors::{ConnectorSettings, KeepLog};
use crate::engine::owner::ConnectorsSaved;
use crate::tui::client::OwnerEvent;
use crate::tui::client::connectors::{ConnectorsEvent, ConnectorsJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::clock::stamp;
use crate::tui::render_tests::{check, ready, render, shell_at};
use crate::tui::screens::open;
use crate::tui::screens::project_time::{local_zone, zone_offset};
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::{Shell, targets};

const NOV_15_1204_UTC: i64 = 1_794_744_240_000;
const MINUTE: i64 = 60_000;
const WRITES: &str =
    "This tool can change things at the vendor; writes arrive with the approval step.";

fn call(connector: &str, tool: &str, kind: CallKind, at: i64) -> CallView {
    CallView {
        connector: connector.into(),
        tool: tool.into(),
        kind,
        project: "alpha".into(),
        project_name: "Plant shop".into(),
        agent: "Agent 1".into(),
        started_ms: at,
        ms: 340,
        bytes_in: 120,
        bytes_out: 2048,
        outcome: "ok".into(),
        outcome_text: "OK".into(),
    }
}

fn view() -> ActivityView {
    let mut write = call(
        "board",
        "create_issue",
        CallKind::Write,
        NOV_15_1204_UTC - MINUTE,
    );
    write.agent = "Master".into();
    write.ms = 0;
    write.bytes_in = 64;
    write.bytes_out = 0;
    write.outcome = "connector_writes_not_yet".into();
    write.outcome_text = WRITES.into();
    let mut list = call("desk", "", CallKind::List, NOV_15_1204_UTC - 6 * MINUTE);
    list.project = "beta".into();
    list.project_name = "beta".into();
    list.agent = "Agent 2".into();
    list.ms = 1210;
    list.bytes_in = 0;
    list.bytes_out = 5_400;
    ActivityView {
        calls: vec![
            call("board", "list_issues", CallKind::Read, NOV_15_1204_UTC),
            write,
            list,
        ],
        more: false,
        connector: None,
        project: None,
        connectors: vec!["board".into(), "desk".into()],
        projects: vec![
            ProjectChoice {
                id: "beta".into(),
                name: "beta".into(),
            },
            ProjectChoice {
                id: "alpha".into(),
                name: "Plant shop".into(),
            },
        ],
        settings: ConnectorSettings::DEFAULT,
        version: 3,
    }
}

fn requests(shell: &mut Shell) -> Vec<ConnectorsJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => match *job {
                SettingsJob::Connectors(job) => Some(*job),
                _ => panic!("the page sent another Settings request"),
            },
            _ => None,
        })
        .collect()
}

fn one(shell: &mut Shell) -> ConnectorsJob {
    let mut sent = requests(shell);
    assert_eq!(sent.len(), 1, "one request at a time");
    sent.remove(0)
}

fn arrive(shell: &mut Shell, event: ConnectorsEvent) {
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Connectors(Box::new(event)),
    )))));
}

fn all(job: &ConnectorsJob) -> bool {
    matches!(
        job,
        ConnectorsJob::Activity {
            connector: None,
            project: None,
        }
    )
}

fn page(read: ActivityView) -> Shell {
    let mut shell = shell_at(ready(80, 24), Place::ConnectorActivity);
    assert!(all(&one(&mut shell)));
    arrive(&mut shell, ConnectorsEvent::Activity(Ok(read)));
    shell
}

fn shows(shell: &mut Shell, text: &str) -> bool {
    render(shell, 80, 24).iter().any(|row| row.contains(text))
}

fn said(shell: &Shell) -> String {
    shell
        .ctx
        .message()
        .map(|message| format!("{} {}", message.lead, message.rest))
        .unwrap_or_default()
}

fn key(shell: &mut Shell, key: Key) {
    shell.key(key);
}

#[test]
fn the_page_shows_each_call_with_every_field_and_the_keep_setting_beside_the_list() {
    let mut shell = page(view());
    let zone = local_zone();
    let swaps: Vec<(String, String)> = [0, 1, 6]
        .into_iter()
        .map(|back| {
            let at = NOV_15_1204_UTC - back * MINUTE;
            (stamp(at, 0), stamp(at, zone_offset(zone.as_deref(), at)))
        })
        .collect();
    let swaps: Vec<(&str, &str)> = swaps
        .iter()
        .map(|(from, to)| (from.as_str(), to.as_str()))
        .collect();
    check("connector-activity-80", &render(&mut shell, 80, 24), &swaps);
    for words in ["arguments", "result", "SECRET"] {
        assert!(
            !render(&mut shell, 80, 24)
                .iter()
                .skip(5)
                .any(|row| row.to_lowercase().contains(&words.to_lowercase())),
            "{words}"
        );
    }
}

#[test]
fn a_filter_change_reads_again_at_once_and_a_keep_pick_waits_for_enter() {
    let mut shell = page(view());
    key(&mut shell, Key::Down);
    key(&mut shell, Key::Right);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Activity {
            connector: Some(ref connector),
            project: None,
        } if connector == "board"
    ));
    let mut board = view();
    board.calls.truncate(2);
    board.connector = Some("board".into());
    arrive(&mut shell, ConnectorsEvent::Activity(Ok(board)));
    assert!(shows(&mut shell, "Connector           board"));
    assert!(shows(&mut shell, "Calls  2"));
    key(&mut shell, Key::Down);
    key(&mut shell, Key::Left);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::Activity {
            connector: Some(ref connector),
            project: Some(ref project),
        } if connector == "board" && project == "alpha"
    ));

    let mut both = view();
    both.connector = Some("board".into());
    both.project = Some("alpha".into());
    arrive(&mut shell, ConnectorsEvent::Activity(Ok(both)));
    assert!(shows(&mut shell, "Project             Plant shop"));
    key(&mut shell, Key::Home);
    key(&mut shell, Key::Right);
    assert!(
        requests(&mut shell).is_empty(),
        "a keep pick waits for Enter"
    );
    assert!(shows(&mut shell, "[90 days]"));
    assert!(shows(&mut shell, "Enter Save changes"));
    key(&mut shell, Key::Enter);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SaveSettings {
            settings,
            version: 3,
        } if settings == ConnectorSettings {
            keep_log: KeepLog::Days90,
            ..ConnectorSettings::DEFAULT
        }
    ));
    arrive(
        &mut shell,
        ConnectorsEvent::Saved(Ok(ConnectorsSaved::Stale(4))),
    );
    assert!(said(&shell).contains("The connector settings changed elsewhere."));
    assert!(
        matches!(
            one(&mut shell),
            ConnectorsJob::Activity {
                connector: Some(ref connector),
                project: Some(ref project),
            } if connector == "board" && project == "alpha"
        ),
        "a stale save reads the page again"
    );
}

#[test]
fn esc_drops_an_unsaved_keep_pick_and_a_good_save_says_so() {
    let mut shell = page(view());
    key(&mut shell, Key::Left);
    assert!(shows(&mut shell, "[7 days]"));
    assert!(shows(&mut shell, "Esc Discard changes"));
    key(&mut shell, Key::Esc);
    assert!(shows(&mut shell, "[30 days]"));
    assert!(requests(&mut shell).is_empty());
    key(&mut shell, Key::Left);
    key(&mut shell, Key::Enter);
    assert!(matches!(
        one(&mut shell),
        ConnectorsJob::SaveSettings {
            settings: ConnectorSettings {
                keep_log: KeepLog::Days7,
                ..
            },
            version: 3,
        }
    ));
    let mut stored = view();
    stored.settings.keep_log = KeepLog::Days7;
    let page_after = crate::connectors::ConnectorsView {
        connectors: Vec::new(),
        more: false,
        settings: stored.settings,
        version: 4,
        project: None,
        grants: Vec::new(),
    };
    arrive(
        &mut shell,
        ConnectorsEvent::Saved(Ok(ConnectorsSaved::Saved(Box::new(page_after)))),
    );
    assert!(said(&shell).contains("Connector settings saved."));
    assert!(all(&one(&mut shell)), "a save reads the page again");
}

#[test]
fn a_failed_filter_read_keeps_the_list_and_puts_the_filter_back() {
    let mut shell = page(view());
    key(&mut shell, Key::Down);
    key(&mut shell, Key::Right);
    assert!(matches!(one(&mut shell), ConnectorsJob::Activity { .. }));
    arrive(
        &mut shell,
        ConnectorsEvent::Activity(Err("connectors_unavailable".into())),
    );
    assert!(said(&shell).contains("Couldn't load the connector activity."));
    assert!(said(&shell).contains("The engine could not be reached."));
    assert!(shows(&mut shell, "Connector           All"));
    assert!(shows(&mut shell, "Calls  3"));
}

#[test]
fn an_empty_log_and_a_filter_with_no_match_say_so_and_a_failed_first_read_offers_a_retry() {
    let mut empty = view();
    empty.calls.clear();
    empty.connectors.clear();
    empty.projects.clear();
    let mut shell = page(empty.clone());
    assert!(shows(&mut shell, "No connector calls yet."));

    let mut more = view();
    more.more = true;
    let mut shell = page(more);
    assert!(shows(&mut shell, "Showing the newest calls."));

    let mut shell = shell_at(ready(80, 24), Place::ConnectorActivity);
    assert!(all(&one(&mut shell)));
    arrive(
        &mut shell,
        ConnectorsEvent::Activity(Err("connectors_unavailable".into())),
    );
    assert!(shows(&mut shell, "Couldn't load the connector activity."));
    assert!(shows(&mut shell, "r Try again"));
    key(&mut shell, Key::Char('r'));
    assert!(all(&one(&mut shell)));
    let mut none = view();
    none.calls.clear();
    none.connector = Some("board".into());
    arrive(&mut shell, ConnectorsEvent::Activity(Ok(none)));
    assert!(
        !shows(&mut shell, "No calls match these filters."),
        "the page asked for all"
    );
}

#[test]
fn go_to_lists_connector_activity_under_settings_and_opens_it() {
    let mut context = ready(80, 24);
    assert!(targets(&context).iter().any(|target| {
        target.label == "Settings / Connector activity"
            && target.kind == "Settings"
            && target.place == Place::ConnectorActivity
    }));
    let screen = open(&Place::ConnectorActivity, &mut context);
    assert_eq!(screen.place(), Some(Place::ConnectorActivity));
    assert_eq!(screen.crumbs(), ["Settings", "Connector activity"]);
}

#[test]
fn sizes_read_the_same_as_the_desktops() {
    assert_eq!(size_words(120, 2048), "120 B in, 2.0 KiB out");
    assert_eq!(size_words(0, 1023), "0 B in, 1023 B out");
    assert_eq!(size_words(1536, 5_400), "1.5 KiB in, 5.3 KiB out");
    assert_eq!(size_words(1_048_576, 1_101_005), "1.0 MiB in, 1.1 MiB out");
    assert_eq!(size_words(-4, 1_048_575), "0 B in, 1024.0 KiB out");
}
