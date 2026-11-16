use super::*;
use crate::tui::client::OwnerEvent;
use crate::tui::client::connectors::{ConnectorsEvent, ConnectorsJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::clock::{now_ms, stamp};
use crate::tui::render_tests::{ready, render, shell_at};
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::Shell;

const ID: i64 = 42;
const ASKED: i64 = 1_763_250_000_000;
const MINUTE: i64 = 60_000;

fn approval(state: State, expires_ms: i64) -> ApprovalView {
    ApprovalView {
        id: ID,
        project: "alpha".into(),
        project_name: "Plant shop".into(),
        agent: "builder".into(),
        connector: "linear".into(),
        title: "Linear".into(),
        tool: "create_issue".into(),
        preview: "create_issue(title: \"Fix login\")".into(),
        arguments: "{\"title\":\"Fix login\"}".into(),
        hidden: false,
        state,
        created_ms: ASKED,
        expires_ms,
        answered_ms: None,
    }
}

fn waiting() -> ApprovalView {
    approval(State::Waiting, now_ms() + 10 * MINUTE)
}

fn answered(state: State) -> ApprovalView {
    let mut view = approval(state, now_ms() + 10 * MINUTE);
    view.answered_ms = Some(ASKED + MINUTE);
    view
}

fn requests(shell: &mut Shell) -> Vec<ConnectorsJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => match *job {
                SettingsJob::Connectors(job) => Some(*job),
                _ => None,
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

fn approval_event(result: Result<ApprovalView, String>) -> ConnectorsEvent {
    let id = result.as_ref().map_or(ID, |view| view.id);
    ConnectorsEvent::Approval { id, result }
}

fn reads(job: &ConnectorsJob) -> bool {
    matches!(job, ConnectorsJob::Approval { id } if *id == ID)
}

fn answers(job: &ConnectorsJob, allow: bool) -> bool {
    matches!(job, ConnectorsJob::Answer { id, allow: yes } if *id == ID && *yes == allow)
}

fn opened() -> Shell {
    let mut shell = shell_at(ready(80, 24), Place::ConnectorApproval { id: ID });
    assert!(reads(&one(&mut shell)), "the page reads its write");
    shell
}

fn page(read: ApprovalView) -> Shell {
    let mut shell = opened();
    arrive(&mut shell, approval_event(Ok(read)));
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

fn keys(shell: &mut Shell, keys: &[Key]) {
    for key in keys {
        shell.key(key.clone());
    }
}

#[test]
fn the_lines_time_left_and_answer_are_the_desktops_words() {
    let ends = ASKED + 10 * MINUTE;
    let view = approval(State::Waiting, ends);
    assert_eq!(
        approval_lines(&view, 0),
        vec![
            "Connector: Linear".to_owned(),
            "Tool: create_issue".into(),
            "Project: Plant shop".into(),
            "Agent: builder".into(),
            format!("Asked: {}", stamp(ASKED, 0)),
            "What it sends: create_issue(title: \"Fix login\")".into(),
        ]
    );
    let mut odd = view.clone();
    odd.title = "Lin\u{202e}ear".into();
    odd.tool = "make\\u202e".into();
    odd.project_name = "Plant\u{200b}shop".into();
    odd.agent = "two\nlines".into();
    odd.preview = "p".repeat(470);
    let lines = approval_lines(&odd, 0);
    assert_eq!(lines[0], "Connector: Lin\\u202eear");
    assert_eq!(
        lines[1], "Tool: make\\\\u202e",
        "a typed escape never shows like the character it names"
    );
    assert_eq!(lines[2], "Project: Plant\\u200bshop");
    assert_eq!(lines[3], "Agent: two lines");
    assert_eq!(
        lines[5],
        format!("What it sends: {}", "p".repeat(470)),
        "the whole preview shows"
    );
    assert_eq!(time_left(ASKED, ends), "10:00");
    assert_eq!(time_left(ASKED + 1, ends), "10:00", "rounded up");
    assert_eq!(time_left(ASKED + 1_000, ends), "9:59");
    assert_eq!(time_left(ASKED, ASKED + 61_000), "1:01");
    assert_eq!(time_left(ends, ends), "0:00");
    assert_eq!(time_left(ends + MINUTE, ends), "0:00", "never below 0:00");
    assert_eq!(shown_state(&view, ends - 1), State::Waiting);
    assert_eq!(shown_state(&view, ends), State::Expired);
    let allowed = answered(State::Allowed);
    assert_eq!(
        shown_state(&allowed, allowed.expires_ms + MINUTE),
        State::Allowed
    );
    assert_eq!(answered_line(&view, State::Waiting, 0), "");
    assert_eq!(answered_line(&view, State::Expired, 0), "Expired");
    assert_eq!(
        answered_line(&allowed, State::Allowed, 0),
        format!("Allowed {}", stamp(ASKED + MINUTE, 0))
    );
    let denied = answered(State::Denied);
    assert_eq!(
        answered_line(&denied, State::Denied, 0),
        format!("Denied {}", stamp(ASKED + MINUTE, 0))
    );
}

#[test]
fn the_notice_page_reads_its_write_shows_it_and_enter_only_folds_the_arguments() {
    let mut shell = opened();
    assert_eq!(shell.place(), Some(Place::ConnectorApproval { id: ID }));
    let mut read = waiting();
    read.arguments = format!("{{\"body\":\"{}tail-mark\"}}", "word ".repeat(400));
    arrive(&mut shell, approval_event(Ok(read)));
    for text in [
        "Connector write",
        "An agent wants a connector to change something.",
        "Connector: Linear",
        "Tool: create_issue",
        "Project: Plant shop",
        "Agent: builder",
        "Asked: ",
        "What it sends: create_issue",
        "Time left: ",
        "All arguments",
        "a Allow",
        "d Deny",
    ] {
        assert!(shows(&mut shell, text), "{text}");
    }
    assert!(
        !shows(&mut shell, "{\"body\""),
        "the arguments start folded"
    );
    keys(&mut shell, &[Key::Enter]);
    assert!(shows(&mut shell, "{\"body\":\"word"));
    assert!(requests(&mut shell).is_empty(), "Enter never answers");
    assert!(!shows(&mut shell, "tail-mark"));
    keys(&mut shell, &[Key::End]);
    assert!(
        shows(&mut shell, "tail-mark\"}"),
        "End reaches the last argument line"
    );
    keys(&mut shell, &[Key::Home, Key::Enter]);
    assert!(!shows(&mut shell, "{\"body\""), "Enter folds them again");
    assert!(requests(&mut shell).is_empty());
}

#[test]
fn a_allows_once_and_the_answer_stays_with_no_keys_until_the_re_read_lands() {
    let mut shell = page(waiting());
    keys(&mut shell, &[Key::Char('a')]);
    assert!(answers(&one(&mut shell), true));
    keys(&mut shell, &[Key::Char('a'), Key::Char('d')]);
    assert!(requests(&mut shell).is_empty(), "one answer at a time");
    let mut other = answered(State::Denied);
    other.id = 7;
    arrive(&mut shell, approval_event(Ok(other)));
    assert!(
        !shows(&mut shell, "Denied"),
        "another write's answer is not this page's"
    );
    arrive(&mut shell, approval_event(Ok(answered(State::Allowed))));
    assert!(said(&shell).contains("Write allowed."));
    assert!(reads(&one(&mut shell)), "the page reads the write again");
    assert!(shows(&mut shell, "Allowed "));
    assert!(!shows(&mut shell, "Time left"));
    assert!(!shows(&mut shell, "a Allow"));
    arrive(
        &mut shell,
        approval_event(Err("connectors_unavailable".into())),
    );
    assert!(
        shows(&mut shell, "Allowed "),
        "a failed re-read keeps the answer the engine took"
    );
    keys(&mut shell, &[Key::Char('d')]);
    assert!(
        requests(&mut shell).is_empty(),
        "an answered write takes no answer"
    );
    let mut shell = page(waiting());
    keys(&mut shell, &[Key::Char('d')]);
    assert!(answers(&one(&mut shell), false));
    arrive(&mut shell, approval_event(Ok(answered(State::Denied))));
    assert!(said(&shell).contains("Write denied."));
    assert!(reads(&one(&mut shell)));
    arrive(&mut shell, approval_event(Ok(answered(State::Denied))));
    assert!(shows(&mut shell, "Denied "));
}

#[test]
fn a_closed_write_reads_again_and_a_wait_that_ran_out_shows_expired_and_reads_once_more() {
    let mut shell = page(waiting());
    keys(&mut shell, &[Key::Char('d')]);
    assert!(answers(&one(&mut shell), false));
    arrive(
        &mut shell,
        approval_event(Err("connector_approval_closed".into())),
    );
    assert!(said(&shell).contains("This write was already answered or its wait ran out."));
    assert!(reads(&one(&mut shell)), "a closed write is read again");
    let ended = now_ms() - 1;
    arrive(
        &mut shell,
        approval_event(Ok(approval(State::Waiting, ended))),
    );
    assert!(
        shows(&mut shell, "Expired"),
        "a write still waiting past its end shows Expired"
    );
    assert!(!shows(&mut shell, "Time left"));
    assert!(!shows(&mut shell, "a Allow"));
    keys(&mut shell, &[Key::Char('a')]);
    assert!(
        requests(&mut shell).is_empty(),
        "the engine refuses an answer from then on"
    );
    shell.tick();
    assert_eq!(
        requests(&mut shell).iter().filter(|job| reads(job)).count(),
        1,
        "the end of the wait reads the write once more"
    );
    arrive(
        &mut shell,
        approval_event(Ok(approval(State::Waiting, ended))),
    );
    shell.tick();
    assert!(
        requests(&mut shell).is_empty(),
        "and only once for that end"
    );
}

#[test]
fn a_failed_read_says_why_and_r_tries_again() {
    let mut shell = opened();
    arrive(
        &mut shell,
        approval_event(Err("connector_approval_unknown".into())),
    );
    assert!(shows(&mut shell, "That approval is gone."));
    assert!(shows(&mut shell, "r Try again"));
    keys(&mut shell, &[Key::Char('a')]);
    assert!(requests(&mut shell).is_empty());
    keys(&mut shell, &[Key::Char('r')]);
    assert!(reads(&one(&mut shell)));
}

#[test]
fn a_write_with_hidden_characters_says_so_and_shows_the_engines_escapes() {
    let mut read = waiting();
    read.hidden = true;
    read.arguments = "{\"title\":\"Fix\\u200blogin\"}".into();
    let mut shell = page(read);
    assert!(shows(
        &mut shell,
        "This write holds hidden or control characters;"
    ));
    keys(&mut shell, &[Key::Enter]);
    assert!(
        shows(&mut shell, "{\"title\":\"Fix\\u200blogin\"}"),
        "the engine's escapes show as they came"
    );
    let mut shell = page(waiting());
    assert!(!shows(&mut shell, "hidden or control characters"));
}

#[test]
fn a_long_preview_wraps_under_its_label_and_never_starts_a_row_like_another_line() {
    let mut read = waiting();
    read.preview = format!("{} Tool: forged", "w".repeat(58));
    let mut shell = page(read);
    let rows = render(&mut shell, 80, 24);
    assert_eq!(
        rows.iter()
            .filter(|row| row.starts_with("  Tool: "))
            .count(),
        1,
        "only the page's own Tool line starts at the label column"
    );
    assert!(
        rows.iter().any(|row| row.starts_with("    Tool: forged")),
        "the preview's next row hangs under its label"
    );
}

fn owner(event: ConnectorsEvent) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Connectors(Box::new(event)),
    ))))
}

/// rv-code T13d F1: every stacked page sees every arrival, so a failed read names its write
/// and only the page that asked takes it; the page on top keeps waiting for its own answer.
#[test]
fn a_stacked_page_takes_only_the_answer_for_its_own_write() {
    let mut context = ready(80, 24);
    let mut under = ConnectorApprovalPage::new(ID);
    let mut over = ConnectorApprovalPage::new(8);
    under.shown(&mut context);
    over.shown(&mut context);
    assert_eq!(std::iter::from_fn(|| context.next_request()).count(), 2);
    let failed = owner(ConnectorsEvent::Approval {
        id: ID,
        result: Err("trusted_connection_unavailable".into()),
    });
    for page in [&mut over, &mut under] {
        page.arrive(&failed, &mut context);
    }
    assert!(matches!(under.stored, Load::Failed(_)));
    assert!(
        over.pending.is_some() && over.stored.ready().is_none(),
        "the page on top still waits for its own read"
    );
    let mut eight = waiting();
    eight.id = 8;
    let read = owner(ConnectorsEvent::Approval {
        id: 8,
        result: Ok(eight),
    });
    for page in [&mut over, &mut under] {
        page.arrive(&read, &mut context);
    }
    assert!(over.stored.ready().is_some_and(|view| view.id == 8));
    assert!(matches!(under.stored, Load::Failed(_)));
}
