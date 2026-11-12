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
