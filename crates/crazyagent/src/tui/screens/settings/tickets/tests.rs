use super::*;
use crate::tui::client::OwnerEvent;
use crate::tui::client::tracker::tests::{Engine, answer, fieldnotes};
use crate::tui::screens::open;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::{Shell, targets};
use trackwright_core::model::{ClosePolicy, CycleDays};

fn jobs(shell: &mut Shell) -> Vec<TrackerJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => match *job {
                SettingsJob::Tracker(job) => Some(*job),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn reply(shell: &mut Shell, engine: &mut Engine, job: TrackerJob) {
    let event = answer(job, engine);
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        event,
    )))));
}

fn serve(shell: &mut Shell, engine: &mut Engine) {
    for _ in 0..8 {
        let sent = jobs(shell);
        if sent.is_empty() {
            return;
        }
        for job in sent {
            reply(shell, engine, job);
        }
    }
    panic!("the page kept asking");
}

fn one(shell: &mut Shell) -> TrackerJob {
    let mut sent = jobs(shell);
    assert_eq!(sent.len(), 1, "one request at a time");
    sent.remove(0)
}

fn shows(shell: &mut Shell, label: &str, value: &str) -> bool {
    render(shell, 80, 24)
        .iter()
        .any(|row| row.contains(label) && row.contains(value))
}

fn project() -> Scope {
    Scope::Project {
        id: fieldnotes(),
        name: "Fieldnotes Mobile".into(),
    }
}

#[test]
fn a_project_override_is_set_and_reset_at_the_version_read() {
    let mut engine = Engine::new();
    let mut shell = Shell::with(ctx(80, 24), Box::new(TicketSettingsPage::new(project())));
    serve(&mut shell, &mut engine);
    assert!(shows(
        &mut shell,
        "Who closes tickets",
        "App value (Evidence)"
    ));
    assert!(shows(&mut shell, "Cycle length", "App value (14 days)"));
    assert!(shows(&mut shell, "CI checks", "App value (Engine)"));

    shell.key(Key::Enter);
    assert!(shows(
        &mut shell,
        "Who closes tickets",
        "Evidence \u{2190}\u{2192}"
    ));
    assert!(shows(&mut shell, "Owner", "Only you close tickets."));
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    let TrackerJob::SaveSettings {
        project: Some(saved),
        values,
        version: 0,
    } = &job
    else {
        panic!("the change saves the project's values at version 0");
    };
    assert_eq!(*saved, fieldnotes());
    assert_eq!(
        *values,
        TicketOverride {
            close: Some(ClosePolicy::Owner),
            ..TicketOverride::default()
        }
    );
    reply(&mut shell, &mut engine, job);
    serve(&mut shell, &mut engine);
    assert!(shows(&mut shell, "Who closes tickets", "Owner"));
    assert!(!shows(&mut shell, "Who closes tickets", "App value"));
    assert_eq!(
        engine
            .settings_view(Some(&fieldnotes()))
            .effective
            .policy
            .close,
        ClosePolicy::Owner
    );

    shell.key(Key::Char('u'));
    let job = one(&mut shell);
    let TrackerJob::SaveSettings {
        values, version, ..
    } = &job
    else {
        panic!("a reset saves the project's values");
    };
    assert_eq!(*values, TicketOverride::default());
    assert_eq!(*version, 1);
    reply(&mut shell, &mut engine, job);
    serve(&mut shell, &mut engine);
    assert!(shows(
        &mut shell,
        "Who closes tickets",
        "App value (Evidence)"
    ));
    assert_eq!(
        engine
            .settings_view(Some(&fieldnotes()))
            .effective
            .policy
            .close,
        ClosePolicy::Evidence
    );
    shell.key(Key::Char('u'));
    assert!(jobs(&mut shell).is_empty(), "nothing set, nothing to reset");
}

#[test]
fn a_custom_app_cycle_length_is_checked_and_reaches_every_project() {
    let mut engine = Engine::new();
    let mut shell = Shell::with(ctx(80, 24), Box::new(TicketSettingsPage::new(Scope::App)));
    serve(&mut shell, &mut engine);
    assert!(shows(&mut shell, "Cycle length", "Default (14 days)"));
    for _ in 0..3 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    for _ in 0..3 {
        shell.key(Key::Right);
    }
    assert!(shows(
        &mut shell,
        "Custom",
        "Type a length from 1 to 90 days."
    ));
    shell.key(Key::Enter);
    for character in ['9', '1'] {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    assert!(jobs(&mut shell).is_empty());
    assert!(
        render(&mut shell, 80, 24)
            .iter()
            .any(|row| row.contains("Use a whole number from 1 to 90."))
    );
    shell.key(Key::Backspace);
    shell.key(Key::Backspace);
    for character in ['1', '0'] {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    let job = one(&mut shell);
    let TrackerJob::SaveSettings {
        project: None,
        values,
        version: 0,
    } = &job
    else {
        panic!("the change saves the app values at version 0");
    };
    assert_eq!(values.cycle_days, Some(CycleDays::new(10).unwrap()));
    reply(&mut shell, &mut engine, job);
    serve(&mut shell, &mut engine);
    assert!(shows(&mut shell, "Cycle length", "10 days"));
    assert!(!shows(&mut shell, "Cycle length", "Default"));
    assert_eq!(
        engine
            .settings_view(Some(&fieldnotes()))
            .effective
            .policy
            .cycle_days,
        CycleDays::new(10).unwrap()
    );
}

#[test]
fn a_save_over_a_newer_version_reads_the_current_values_again() {
    let mut engine = Engine::new();
    let mut shell = Shell::with(ctx(80, 24), Box::new(TicketSettingsPage::new(project())));
    serve(&mut shell, &mut engine);
    engine.save_settings(
        Some(&fieldnotes()),
        &TicketOverride {
            close: Some(ClosePolicy::Agents),
            ..TicketOverride::default()
        },
        0,
    );
    shell.key(Key::Enter);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    reply(&mut shell, &mut engine, job);
    assert!(
        render(&mut shell, 80, 24)
            .iter()
            .any(|row| row.contains("Changed somewhere else."))
    );
    serve(&mut shell, &mut engine);
    assert!(shows(&mut shell, "Who closes tickets", "Agents"));
    assert_eq!(
        engine
            .settings_view(Some(&fieldnotes()))
            .effective
            .policy
            .close,
        ClosePolicy::Agents
    );
}

#[test]
fn ticket_settings_open_from_go_to_and_from_a_project() {
    let mut ctx = ctx(80, 24);
    assert!(
        targets(&ctx)
            .iter()
            .any(|target| target.label == "Settings / Tickets"
                && target.kind == "Settings"
                && target.place == Place::TicketSettings { project: None })
    );
    let mut app = open(&Place::TicketSettings { project: None }, &mut ctx);
    assert_eq!(app.crumbs(), ["Settings", "Tickets"]);
    assert!(matches!(app.key(&Key::Esc, &mut ctx), Flow::Ignored));

    let place = Place::TicketSettings {
        project: Some((fieldnotes(), "Fieldnotes Mobile".into())),
    };
    let mut own = open(&place, &mut ctx);
    assert_eq!(own.crumbs(), ["Fieldnotes Mobile", "Tickets", "Settings"]);
    assert_eq!(own.place(), Some(place));
    assert!(matches!(
        own.key(&Key::Char('a'), &mut ctx),
        Flow::Open(Place::TicketSettings { project: None })
    ));
}

#[test]
fn every_key_and_option_has_a_name_and_a_line() {
    let engine = Engine::new();
    let view = engine.settings_view(None);
    assert_eq!(view.choices.len(), 8);
    for choice in &view.choices {
        assert_ne!(label(&choice.key), choice.key);
        for option in &choice.options {
            let (name, meaning) = option_text(&choice.key, option);
            assert!(name.chars().count() < NAME_CELLS, "{name}");
            assert!(!meaning.is_empty(), "{} {option}", choice.key);
        }
    }
    assert_eq!(
        option_text("close", &serde_json::json!("someday")),
        ("\"someday\"".to_owned(), String::new())
    );
}

#[test]
fn the_create_lines_and_the_saved_line_say_what_the_engine_does() {
    assert_eq!(
        option_text("create", &serde_json::json!("triage")).1,
        "Tickets agents create wait in triage until you accept them."
    );
    assert_eq!(
        option_text("create", &serde_json::json!("direct")),
        (
            "Direct".to_owned(),
            "New tickets start where their creator puts them.".to_owned()
        )
    );
    let mut engine = Engine::new();
    let mut shell = Shell::with(ctx(80, 24), Box::new(TicketSettingsPage::new(project())));
    serve(&mut shell, &mut engine);
    shell.key(Key::Enter);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    reply(&mut shell, &mut engine, job);
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Saved.");
    assert_eq!(message.rest, "It applies from the next change on.");
}

/// The help bar as an 80x24 window draws it, every row of it. Takes the shell. Returns the
/// bar rows joined by newlines.
fn bar_at_80(shell: &mut Shell) -> String {
    crate::tui::screens::settings_accounts::tests::bar_rows(&render(shell, 80, 24)).join("\n")
}

fn says(shell: &mut Shell, text: &str) -> bool {
    render(shell, 120, 40).iter().any(|row| row.contains(text))
}

#[test]
fn every_ticket_settings_key_stays_in_the_hint_bar_at_80_columns() {
    let mut engine = Engine::new();
    let mut own = Shell::with(ctx(80, 24), Box::new(TicketSettingsPage::new(project())));
    serve(&mut own, &mut engine);
    own.key(Key::Enter);
    own.key(Key::Right);
    own.key(Key::Enter);
    serve(&mut own, &mut engine);
    let bar = bar_at_80(&mut own);
    for key in [
        "Enter Change",
        "u Use the app value",
        "a App values",
        "Move",
        "r Read again",
        "Esc Back",
        "? Help",
    ] {
        assert!(bar.contains(key), "{key}:\n{bar}");
    }
    let mut app = Shell::with(ctx(80, 24), Box::new(TicketSettingsPage::new(Scope::App)));
    serve(&mut app, &mut engine);
    let bar = bar_at_80(&mut app);
    for key in ["Enter Change", "Move", "r Read again", "Esc Back", "? Help"] {
        assert!(bar.contains(key), "{key}:\n{bar}");
    }
}

#[test]
fn ticket_settings_use_the_desktop_words() {
    let mut engine = Engine::new();
    let mut app = Shell::with(ctx(120, 40), Box::new(TicketSettingsPage::new(Scope::App)));
    serve(&mut app, &mut engine);
    let rows = render(&mut app, 120, 40);
    let at = |text: &str| {
        rows.iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text}:\n{}", rows.join("\n")))
    };
    let title = rows
        .iter()
        .position(|row| row.trim() == "Tickets")
        .unwrap_or_else(|| panic!("a Tickets title row:\n{}", rows.join("\n")));
    let purpose = at("The ticket settings every project with tickets starts from");
    let rules = at("Ticket rules");
    let effect = at("Changes apply to the next ticket action. Tickets already closed stay closed.");
    let table = at("Who closes tickets");
    assert!(title < purpose && purpose < rules && rules < effect && effect < table);
    assert!(!says(&mut app, "for this project only."));
    assert!(!says(&mut app, "Ticket settings"));

    let mut own = Shell::with(ctx(120, 40), Box::new(TicketSettingsPage::new(project())));
    serve(&mut own, &mut engine);
    assert!(says(&mut own, "Ticket rules"));
    assert!(says(
        &mut own,
        "Changes apply to the next ticket action. Tickets already closed stay closed."
    ));
    assert!(says(&mut own, "for this project only."));
    assert!(!says(
        &mut own,
        "The ticket settings every project with tickets starts from"
    ));
}

#[test]
fn a_project_without_tickets_says_so_in_the_desktop_words() {
    let mut shell = Shell::with(ctx(80, 24), Box::new(TicketSettingsPage::new(project())));
    let TrackerJob::Settings { project } = one(&mut shell) else {
        panic!("the page reads its settings first");
    };
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Tracker(Box::new(TrackerEvent::Settings {
            project,
            result: Err("project_not_attached".into()),
        })),
    )))));
    let rows = render(&mut shell, 80, 24).join("\n");
    assert!(
        rows.contains("Tickets aren't set up for this project yet."),
        "{rows}"
    );
    assert!(rows.contains("Set them up on the Tickets tab"), "{rows}");
    assert!(!rows.contains("Could not read"), "{rows}");
    assert!(bar_at_80(&mut shell).contains("Esc Back"));
}

#[test]
fn the_last_ticket_setting_s_choices_stay_in_view_at_80x24() {
    let mut engine = Engine::new();
    let mut shell = Shell::with(ctx(80, 24), Box::new(TicketSettingsPage::new(project())));
    serve(&mut shell, &mut engine);
    for _ in 0..7 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 24).join("\n");
    for words in [
        "CI checks",
        "CI evidence comes from CrazyAgent's own runs",
        "Only you record CI evidence.",
        "Agents may record CI evidence.",
    ] {
        assert!(rows.contains(words), "{words}:\n{rows}");
    }
}

#[test]
fn a_settings_page_keeps_its_focused_part_in_view() {
    use crate::tui::screens::settings::top_for;
    assert_eq!(top_for(3, 5, 10), 0);
    assert_eq!(top_for(12, 18, 10), 9);
    assert_eq!(
        top_for(12, 30, 10),
        12,
        "a part taller than the window starts at its first line"
    );
}

#[test]
#[should_panic(expected = "a focused part starts before it ends")]
fn a_focused_part_that_ends_before_it_starts_is_refused() {
    crate::tui::screens::settings::top_for(4, 3, 10);
}
