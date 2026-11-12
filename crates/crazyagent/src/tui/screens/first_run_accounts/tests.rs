use super::*;
use crate::learning::MarkStep;
use crate::tui::client::OwnerEvent;
use crate::tui::client::learning::{LearningEvent, LearningQuery, LearningReply};
use crate::tui::screens::settings_accounts::tests::{bar_rows, ctx, render};
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::Shell;

const INTRO: &str = "Agents need a model account to work. Sign in with ChatGPT or Claude, or add an API key or a local model.";

fn step(width: u16, height: u16) -> Shell {
    Shell::with(ctx(width, height), Box::new(FirstRunAccounts::new()))
}

fn shows(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

fn words(rows: &[String]) -> String {
    rows.iter()
        .map(|row| row.trim())
        .filter(|row| !row.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn bar(rows: &[String]) -> String {
    bar_rows(rows).join("\n")
}

#[test]
fn the_accounts_step_says_what_the_desktop_step_says_and_fits() {
    for (width, height) in [(80, 24), (120, 36)] {
        let mut shell = step(width, height);
        assert_eq!(shell.place(), Some(Place::FirstRunAccounts));
        let rows = render(&mut shell, width, height);
        assert!(shows(&rows, "Accounts"), "{rows:#?}");
        assert!(shows(&rows, "Add an account"), "{rows:#?}");
        assert!(words(&rows).contains(INTRO), "{rows:#?}");
        assert!(
            shows(&rows, "[> Add account ]   [ Skip for now ]"),
            "{rows:#?}"
        );
        let help = bar(&rows);
        assert!(help.contains("Enter Add account"), "{help}");
        assert!(help.contains("Ctrl-Q Quit"), "{help}");
        assert!(!help.contains("Esc"), "first run has no way back: {help}");
        assert!(
            rows.iter()
                .all(|row| row.chars().count() <= usize::from(width)),
            "{rows:#?}"
        );
    }
}

#[test]
fn add_account_ends_first_run_with_home_under_add_account() {
    let mut shell = step(80, 24);
    shell.key(Key::Enter);
    assert_eq!(shell.depth(), 2, "Home at the root, Add account on top");
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Add account"), "{rows:#?}");
    assert!(!shows(&rows, "Add an account"), "{rows:#?}");
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home));
    assert_eq!(shell.depth(), 1);
}

#[test]
fn skip_for_now_goes_home_and_the_arrows_and_tab_pick_the_button() {
    let mut shell = step(120, 36);
    shell.key(Key::Right);
    let rows = render(&mut shell, 120, 36);
    assert!(
        shows(&rows, "[ Add account ]   [> Skip for now ]"),
        "{rows:#?}"
    );
    assert!(bar(&rows).contains("Enter Skip for now"), "{}", bar(&rows));
    shell.key(Key::Left);
    assert!(shows(
        &render(&mut shell, 120, 36),
        "[> Add account ]   [ Skip for now ]"
    ));
    shell.key(Key::Tab);
    assert!(shows(
        &render(&mut shell, 120, 36),
        "[ Add account ]   [> Skip for now ]"
    ));
    shell.key(Key::BackTab);
    shell.key(Key::Tab);
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::Home));
    assert_eq!(shell.depth(), 1, "first run is gone from the stack");
}

#[test]
fn esc_keeps_the_accounts_step() {
    let mut shell = step(80, 24);
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::FirstRunAccounts));
    assert_eq!(shell.depth(), 1);
}

const LEARN: &str = "A short guide on Home walks you through accounts, projects, the master and its agents, questions and change sets. Its first step is an optional demo project.";

#[test]
fn the_learn_offer_shows_ticked_under_the_buttons_and_space_unticks_it() {
    for (width, height) in [(80, 24), (120, 36)] {
        let mut shell = step(width, height);
        let rows = render(&mut shell, width, height);
        let buttons = rows
            .iter()
            .position(|row| row.contains("[> Add account ]"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        let heading = rows
            .iter()
            .position(|row| row.trim() == "Learn CrazyAgent")
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(
            buttons < heading,
            "the offer sits under the buttons: {rows:#?}"
        );
        assert!(words(&rows).contains(LEARN), "{rows:#?}");
        assert!(shows(&rows, "[x] Show the guide on Home"), "{rows:#?}");
        assert!(bar(&rows).contains("Space Untick"), "{}", bar(&rows));
        assert!(
            rows.iter()
                .all(|row| row.chars().count() <= usize::from(width)),
            "{rows:#?}"
        );

        shell.key(Key::Char(' '));
        let rows = render(&mut shell, width, height);
        assert!(shows(&rows, "[ ] Show the guide on Home"), "{rows:#?}");
        assert!(bar(&rows).contains("Space Tick"), "{}", bar(&rows));
        assert!(
            shows(&rows, "[> Add account ]   [ Skip for now ]"),
            "Space keeps the focused button: {rows:#?}"
        );
        shell.key(Key::Char(' '));
        assert!(shows(
            &render(&mut shell, width, height),
            "[x] Show the guide on Home"
        ));
    }
}

fn learning(shell: &mut Shell) -> Vec<LearningQuery> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Learning(query) => Some(*query),
            _ => None,
        })
        .collect()
}

fn skip_sent(shell: &mut Shell) -> LearningQuery {
    let sent = learning(shell);
    let [
        LearningQuery::Mark {
            step: MarkStep::Skipped,
            on: true,
            generation,
        },
    ] = sent.as_slice()
    else {
        panic!("one skip of the guide expected, got {sent:?}")
    };
    assert!(*generation > 0);
    sent[0].clone()
}

fn answer(query: LearningQuery, result: Result<LearningReply, String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Learning(Box::new(LearningEvent {
        query,
        result,
    }))))
}

fn lead(shell: &Shell) -> String {
    shell
        .ctx
        .message()
        .map(|message| message.lead.clone())
        .unwrap_or_default()
}

#[test]
fn a_ticked_guide_writes_nothing_on_either_button() {
    let mut shell = step(80, 24);
    learning(&mut shell);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::Home));
    assert!(
        learning(&mut shell).is_empty(),
        "Skip for now with the guide"
    );

    let mut shell = step(80, 24);
    learning(&mut shell);
    shell.key(Key::Enter);
    assert_eq!(shell.depth(), 2);
    assert!(
        learning(&mut shell).is_empty(),
        "Add account with the guide"
    );
}

#[test]
fn an_unticked_guide_is_skipped_before_skip_for_now_goes_home() {
    let mut shell = step(120, 36);
    learning(&mut shell);
    shell.key(Key::Char(' '));
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let query = skip_sent(&mut shell);
    assert_eq!(
        shell.place(),
        Some(Place::FirstRunAccounts),
        "waits for the skip"
    );
    assert!(shell.busy());
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, "Skipping the guide"), "{rows:#?}");
    assert!(!shows(&rows, "[ Add account ]"), "{rows:#?}");
    shell.key(Key::Enter);
    shell.key(Key::Char(' '));
    assert!(
        learning(&mut shell).is_empty(),
        "keys wait while the skip saves"
    );

    shell.arrive(answer(query, Ok(LearningReply::Marked)));
    assert_eq!(shell.place(), Some(Place::Home));
    assert_eq!(shell.depth(), 1);
    assert_eq!(lead(&shell), "");
}

#[test]
fn an_unticked_guide_is_skipped_before_add_account_opens() {
    let mut shell = step(80, 24);
    learning(&mut shell);
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    let query = skip_sent(&mut shell);
    assert_eq!(shell.depth(), 1);
    shell.arrive(answer(query, Ok(LearningReply::Marked)));
    assert_eq!(shell.depth(), 2, "Home at the root, Add account on top");
    assert!(shows(&render(&mut shell, 80, 24), "Add account"));
}

#[test]
fn a_skip_that_did_not_save_still_leaves_and_says_the_guide_stays_on_home() {
    for code in ["owner_busy", "trusted_connection_unavailable"] {
        let mut shell = step(80, 24);
        learning(&mut shell);
        shell.key(Key::Char(' '));
        shell.key(Key::Right);
        shell.key(Key::Enter);
        let query = skip_sent(&mut shell);
        shell.arrive(answer(
            LearningQuery::Mark {
                step: MarkStep::Terminal,
                on: true,
                generation: 1,
            },
            Ok(LearningReply::Marked),
        ));
        assert_eq!(
            shell.place(),
            Some(Place::FirstRunAccounts),
            "another mark's reply is not this skip"
        );
        shell.arrive(answer(query, Err(code.into())));
        assert_eq!(shell.place(), Some(Place::Home), "{code}");
        assert_eq!(
            lead(&shell),
            "Couldn't skip the guide, so it stays on Home.",
            "{code}"
        );
    }
}
