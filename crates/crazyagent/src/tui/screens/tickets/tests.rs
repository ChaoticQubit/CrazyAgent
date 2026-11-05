use super::*;
use crate::tui::client::tracker::run;
use crate::tui::client::tracker::tests::{Engine, Recorder, answer, fieldnotes};
use crate::tui::screens::new_project_frames::{check_frames, compose};
use crate::tui::screens::project::{ProjectScreen, Tab};
use crate::tui::screens::project_view::project_screen;
use serde_json::json;
use trackwright_core::model::OpKind;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn text(rows: &[Row]) -> String {
    rows.iter().map(Row::text).collect::<Vec<_>>().join("\n")
}

fn settle(tickets: &mut ProjectTickets, engine: &mut Engine) {
    for _ in 0..16 {
        let Some(job) = tickets.next_job() else {
            return;
        };
        tickets.apply(&answer(job, engine));
    }
    panic!("the Tickets tab kept asking");
}

fn tab(engine: &mut Engine) -> ProjectTickets {
    let mut tickets = ProjectTickets::new(fieldnotes(), "Fieldnotes Mobile".into());
    settle(&mut tickets, engine);
    tickets
}

#[test]
fn the_tickets_page_renders_the_engine_backed_80_column_frame() {
    let mut engine = Engine::seeded();
    let mut screen = ProjectScreen::new(fieldnotes(), "Fieldnotes Mobile".into(), None);
    screen.open(Tab::Tickets);
    settle(&mut screen.tickets, &mut engine);
    let look = Look::plain();
    let mut drawn = Vec::new();
    for (width, height) in [(80, 24), (120, 36)] {
        screen.width = width;
        let frame = project_screen(&mut screen, width, height, &look);
        drawn.push((
            format!("proj-tickets-{width}"),
            compose(&frame, width, height, "", &look),
        ));
    }
    check_frames(&drawn);
}

#[test]
fn a_board_move_sends_the_version_the_board_read() {
    let mut engine = Engine::seeded();
    let version = engine.version("FM-2");
    let mut tickets = tab(&mut engine);
    assert_eq!(tickets.key(key(KeyCode::Tab)), TicketsReply::Handled);
    assert_eq!(tickets.key(key(KeyCode::Tab)), TicketsReply::Handled);
    settle(&mut tickets, &mut engine);
    let (rows, _, help) = tickets.rows(76, 18, &Look::plain());
    assert!(text(&rows).starts_with(" Tickets   Ready  [Board]  Roadmap "));
    assert!(help.contains(&("< >", "Move")), "{help:?}");

    let more = KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT);
    assert_eq!(tickets.key(more), TicketsReply::Handled);
    let Some(TrackerJob::Views { views, steps, .. }) = tickets.next_job() else {
        panic!("a move goes to the worker");
    };
    assert_eq!(steps, vec![Step::Key(ViewKey::Char('>'))]);
    let mut recorder = Recorder {
        engine: &mut engine,
        sent: Vec::new(),
    };
    run(&views, &steps, &mut recorder);
    let moves: Vec<(OpKind, serde_json::Value)> = recorder
        .sent
        .iter()
        .filter(|(op, _)| *op == OpKind::Move)
        .cloned()
        .collect();
    assert_eq!(
        moves,
        vec![(
            OpKind::Move,
            json!({ "id": "FM-2", "status": "in_progress", "version": version }),
        )]
    );
    let shown = engine.ok(OpKind::Show, json!({ "id": "FM-2" }));
    assert_eq!(shown["ticket"]["status"], json!("in_progress"));
    assert!(engine.version("FM-2") > version);
}

#[test]
fn turning_tickets_on_attaches_the_project_under_the_key_typed() {
    let mut engine = Engine::bare();
    let mut tickets = tab(&mut engine);
    let look = Look::plain();
    let (rows, _, help) = tickets.rows(76, 18, &look);
    assert!(text(&rows).contains("Fieldnotes Mobile has no tickets yet."));
    assert_eq!(help[0], ("t", "Turn on tickets"));

    assert_eq!(tickets.key(key(KeyCode::Char('t'))), TicketsReply::Handled);
    assert!(tickets.editing());
    let (rows, cursor, _) = tickets.rows(76, 18, &look);
    let field = rows
        .iter()
        .position(|row| row.text().starts_with("Key [ FM "))
        .expect("the key field shows the suggested key");
    assert_eq!(cursor, Some((FIELD_LEFT + 2, field)));

    tickets.key(key(KeyCode::Backspace));
    tickets.key(key(KeyCode::Backspace));
    assert_eq!(tickets.key(key(KeyCode::Char('1'))), TicketsReply::Handled);
    assert_eq!(tickets.key(key(KeyCode::Enter)), TicketsReply::Handled);
    let (rows, _, _) = tickets.rows(76, 18, &look);
    assert!(text(&rows).contains("Use 2 to 8 letters A to Z."));
    assert!(tickets.next_job().is_none());

    for letter in ['f', 'n'] {
        tickets.key(key(KeyCode::Char(letter)));
    }
    assert_eq!(tickets.key(key(KeyCode::Enter)), TicketsReply::Handled);
    settle(&mut tickets, &mut engine);
    assert!(!tickets.editing());
    assert_eq!(engine.tracked()[0].key.as_str(), "FN");
    let (rows, _, _) = tickets.rows(76, 18, &look);
    let shown = text(&rows);
    assert!(shown.contains(" FN  Tickets"), "{shown}");
    assert!(shown.contains("No tickets yet."), "{shown}");
    assert!(
        rows.last()
            .is_some_and(|row| row.text() == "Tickets are on. Ticket ids start with FN."),
        "{shown}"
    );
}

#[test]
fn esc_closes_a_ticket_before_it_leaves_the_tab() {
    let mut engine = Engine::seeded();
    let mut tickets = tab(&mut engine);
    assert_eq!(tickets.key(key(KeyCode::Enter)), TicketsReply::Handled);
    settle(&mut tickets, &mut engine);
    let (rows, _, help) = tickets.rows(76, 18, &Look::plain());
    assert!(text(&rows).contains("Sync notes when the signal returns"));
    assert!(help.contains(&("Esc", "Back")), "{help:?}");
    assert!(!help.contains(&("Esc", "Home")), "{help:?}");

    assert_eq!(tickets.key(key(KeyCode::Esc)), TicketsReply::Handled);
    assert!(
        tickets.next_job().is_none(),
        "closing a ticket needs no call"
    );
    let (_, _, help) = tickets.rows(76, 18, &Look::plain());
    assert!(help.contains(&("Enter", "Open")), "{help:?}");
    assert!(help.contains(&("Esc", "Home")), "{help:?}");
    assert_eq!(tickets.key(key(KeyCode::Esc)), TicketsReply::Ignored);
}

#[test]
fn esc_while_the_views_are_away_closes_an_open_ticket_or_goes_home() {
    let mut engine = Engine::seeded();
    let mut tickets = tab(&mut engine);
    tickets.key(key(KeyCode::Enter));
    settle(&mut tickets, &mut engine);
    tickets.rows(76, 18, &Look::plain());
    assert_eq!(tickets.key(key(KeyCode::Down)), TicketsReply::Handled);
    let Some(TrackerJob::Views { views, steps, .. }) = tickets.next_job() else {
        panic!("Down goes to the worker");
    };
    assert_eq!(tickets.key(key(KeyCode::Esc)), TicketsReply::Handled);
    run(&views, &steps, &mut engine);
    tickets.apply(&SettingsEvent::Tracker(Box::new(TrackerEvent::Views {
        project: fieldnotes(),
        problem: None,
    })));
    let Some(TrackerJob::Views { views, steps, .. }) = tickets.next_job() else {
        panic!("the queued Back follows");
    };
    assert_eq!(steps, vec![Step::Key(ViewKey::Back)]);
    run(&views, &steps, &mut engine);
    tickets.apply(&SettingsEvent::Tracker(Box::new(TrackerEvent::Views {
        project: fieldnotes(),
        problem: None,
    })));
    let (_, _, help) = tickets.rows(76, 18, &Look::plain());
    assert!(
        help.contains(&("Enter", "Open")),
        "the ticket closed: {help:?}"
    );

    assert_eq!(tickets.key(key(KeyCode::Down)), TicketsReply::Handled);
    assert!(tickets.next_job().is_some());
    assert_eq!(
        tickets.key(key(KeyCode::Esc)),
        TicketsReply::Ignored,
        "with no ticket open, Esc goes Home while the views are away"
    );
}

#[test]
fn keys_wait_their_turn_while_the_views_are_away() {
    let mut engine = Engine::seeded();
    let mut tickets = tab(&mut engine);
    assert_eq!(tickets.key(key(KeyCode::Down)), TicketsReply::Handled);
    let Some(TrackerJob::Views { views, steps, .. }) = tickets.next_job() else {
        panic!("Down goes to the worker");
    };
    assert_eq!(steps, vec![Step::Key(ViewKey::Down)]);
    assert!(tickets.busy());
    let (_, _, help) = tickets.rows(76, 18, &Look::plain());
    assert_eq!(help[0], ("", "Working..."));

    assert_eq!(tickets.key(key(KeyCode::Down)), TicketsReply::Handled);
    assert_eq!(tickets.key(key(KeyCode::Up)), TicketsReply::Handled);
    assert!(tickets.next_job().is_none(), "one job at a time");
    run(&views, &steps, &mut engine);
    tickets.apply(&SettingsEvent::Tracker(Box::new(TrackerEvent::Views {
        project: fieldnotes(),
        problem: None,
    })));
    let Some(TrackerJob::Views { steps, .. }) = tickets.next_job() else {
        panic!("the queued keys follow");
    };
    assert_eq!(
        steps,
        vec![Step::Key(ViewKey::Down), Step::Key(ViewKey::Up)]
    );
}

#[test]
fn a_pasted_key_keeps_its_letters_only() {
    let mut engine = Engine::bare();
    let mut tickets = tab(&mut engine);
    assert!(!tickets.paste("FM"), "no field, no paste");
    tickets.key(key(KeyCode::Char('t')));
    assert!(tickets.paste("n-1 b\u{1b}["));
    let (rows, _, _) = tickets.rows(76, 18, &Look::plain());
    assert!(
        rows.iter().any(|row| row.text().starts_with("Key [ FMNB ")),
        "{}",
        text(&rows)
    );
}

#[test]
fn shift_arrows_and_control_keys_stay_with_the_project_screen() {
    let mut engine = Engine::seeded();
    let mut tickets = tab(&mut engine);
    for code in [KeyCode::Left, KeyCode::Right] {
        assert_eq!(
            tickets.key(KeyEvent::new(code, KeyModifiers::SHIFT)),
            TicketsReply::Ignored
        );
    }
    assert_eq!(
        tickets.key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        TicketsReply::Ignored
    );
    assert_eq!(tickets.key(key(KeyCode::Char('s'))), TicketsReply::Settings);
    assert!(tickets.next_job().is_none());
}

#[test]
fn a_suggested_key_takes_each_word_s_first_letter() {
    assert_eq!(suggested_key("Fieldnotes Mobile"), "FM");
    assert_eq!(suggested_key("tidepool"), "TID");
    assert_eq!(suggested_key("A b c d e f g h i j"), "ABCDEFGH");
    assert_eq!(suggested_key("42"), "TK");
}

fn turn_on_job(tickets: &mut ProjectTickets) -> (ProjectId, ProjectKey, String) {
    assert_eq!(tickets.key(key(KeyCode::Char('t'))), TicketsReply::Handled);
    assert_eq!(tickets.key(key(KeyCode::Enter)), TicketsReply::Handled);
    let Some(TrackerJob::Attach { project, key, name }) = tickets.next_job() else {
        panic!("Enter turns tickets on");
    };
    (project, key, name)
}

fn ends_with(tickets: &mut ProjectTickets, line: &str) {
    let (rows, _, _) = tickets.rows(76, 18, &Look::plain());
    assert!(
        rows.last().is_some_and(|row| row.text() == line),
        "{}",
        text(&rows)
    );
}

#[test]
fn a_turn_on_whose_outcome_is_unknown_reads_the_projects_again() {
    let mut engine = Engine::bare();
    let mut tickets = tab(&mut engine);
    let (project, chosen, name) = turn_on_job(&mut tickets);
    engine.attach(&project, &chosen, &name).unwrap();
    tickets.apply(&SettingsEvent::Tracker(Box::new(TrackerEvent::Attached {
        project,
        result: Err("tracker_outcome_unknown".into()),
    })));
    assert!(!tickets.editing(), "the key field closes");
    let (rows, _, _) = tickets.rows(76, 18, &Look::plain());
    assert!(
        text(&rows).contains("Loading tickets..."),
        "{}",
        text(&rows)
    );
    let Some(job @ TrackerJob::Projects) = tickets.next_job() else {
        panic!("the tab reads the tracked projects again");
    };
    tickets.apply(&answer(job, &mut engine));
    settle(&mut tickets, &mut engine);
    let (rows, _, _) = tickets.rows(76, 18, &Look::plain());
    assert!(text(&rows).contains(" FM  Tickets"), "{}", text(&rows));
    ends_with(&mut tickets, "Tickets are on. Ticket ids start with FM.");

    let mut engine = Engine::bare();
    let mut tickets = tab(&mut engine);
    let (project, _, _) = turn_on_job(&mut tickets);
    tickets.apply(&SettingsEvent::Tracker(Box::new(TrackerEvent::Attached {
        project,
        result: Err("tracker_outcome_unknown".into()),
    })));
    settle(&mut tickets, &mut engine);
    assert!(engine.tracked().is_empty());
    assert!(!tickets.editing());
    ends_with(
        &mut tickets,
        "Tickets did not turn on. Press t to try again.",
    );
    let (_, _, help) = tickets.rows(76, 18, &Look::plain());
    assert_eq!(help[0], ("t", "Turn on tickets"));
}

#[test]
fn a_turn_on_the_connection_dropped_under_reads_the_projects_again() {
    let mut engine = Engine::bare();
    let mut tickets = tab(&mut engine);
    let (project, chosen, name) = turn_on_job(&mut tickets);
    engine.attach(&project, &chosen, &name).unwrap();
    tickets.forget_reads();
    assert!(!tickets.editing(), "the key field closes");
    settle(&mut tickets, &mut engine);
    ends_with(&mut tickets, "Tickets are on. Ticket ids start with FM.");

    let mut engine = Engine::bare();
    let mut tickets = tab(&mut engine);
    turn_on_job(&mut tickets);
    tickets.forget_reads();
    settle(&mut tickets, &mut engine);
    assert!(!tickets.editing());
    ends_with(
        &mut tickets,
        "Tickets did not turn on. Press t to try again.",
    );
}

#[test]
fn a_retried_turn_on_the_engine_already_took_turns_the_tab_on() {
    let mut engine = Engine::bare();
    let mut tickets = tab(&mut engine);
    engine
        .attach(
            &fieldnotes(),
            &ProjectKey::new("FM").unwrap(),
            "Fieldnotes Mobile",
        )
        .unwrap();
    let (project, chosen, name) = turn_on_job(&mut tickets);
    tickets.apply(&answer(
        TrackerJob::Attach {
            project,
            key: chosen,
            name,
        },
        &mut engine,
    ));
    assert!(
        !tickets.editing(),
        "project_exists for this project is no key to change"
    );
    settle(&mut tickets, &mut engine);
    ends_with(&mut tickets, "Tickets are on. Ticket ids start with FM.");
}

#[test]
fn a_key_another_project_holds_keeps_the_field_open_with_the_key_typed() {
    let mut engine = Engine::bare();
    engine
        .attach(
            &ProjectId::new("field-manual").unwrap(),
            &ProjectKey::new("FM").unwrap(),
            "Field Manual",
        )
        .unwrap();
    let mut tickets = tab(&mut engine);
    let (project, chosen, name) = turn_on_job(&mut tickets);
    tickets.apply(&answer(
        TrackerJob::Attach {
            project,
            key: chosen,
            name,
        },
        &mut engine,
    ));
    settle(&mut tickets, &mut engine);
    assert!(tickets.editing());
    let (rows, _, _) = tickets.rows(76, 18, &Look::plain());
    let shown = text(&rows);
    assert!(shown.contains("Key [ FM "), "{shown}");
    assert!(
        shown.contains("Another project has that key. Pick another one."),
        "{shown}"
    );
    tickets.key(key(KeyCode::Backspace));
    tickets.key(key(KeyCode::Char('n')));
    assert_eq!(tickets.key(key(KeyCode::Enter)), TicketsReply::Handled);
    settle(&mut tickets, &mut engine);
    assert!(!tickets.editing());
    ends_with(&mut tickets, "Tickets are on. Ticket ids start with FN.");
}

#[test]
fn a_recheck_outside_a_turn_on_in_flight_is_refused() {
    let mut reading = ProjectTickets::new(fieldnotes(), "Fieldnotes Mobile".into());
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reading.recheck("project_exists");
    }));
    assert!(refused.is_err(), "a tab still reading has no turn-on");
    let mut engine = Engine::bare();
    let mut tickets = tab(&mut engine);
    tickets.attach = Some(ProjectKey::new("FM").unwrap());
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tickets.recheck("project_exists");
    }));
    assert!(refused.is_err(), "a turn-on not sent yet has no outcome");
}
