use super::*;
use crate::tui::client::OwnerEvent;
use crate::tui::client::checks::tests::{Engine, answer, fieldnotes};
use crate::tui::screens::open;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::{Shell, targets};

fn jobs(shell: &mut Shell) -> Vec<ChecksJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => match *job {
                SettingsJob::Checks(job) => Some(*job),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn reply(shell: &mut Shell, engine: &mut Engine, job: ChecksJob) {
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

fn one(shell: &mut Shell) -> ChecksJob {
    let mut sent = jobs(shell);
    assert_eq!(sent.len(), 1, "one request at a time");
    sent.remove(0)
}

fn shows(shell: &mut Shell, first: &str, then: &str) -> bool {
    render(shell, 120, 40)
        .iter()
        .any(|row| row.contains(first) && row.contains(then))
}

fn says(shell: &mut Shell, text: &str) -> bool {
    render(shell, 120, 40).iter().any(|row| row.contains(text))
}

fn project() -> Scope {
    Scope::Project {
        id: fieldnotes(),
        name: "Fieldnotes Mobile".into(),
    }
}

fn page(engine: &mut Engine, scope: Scope) -> Shell {
    let mut shell = Shell::with(ctx(120, 40), Box::new(CheckSettingsPage::new(scope)));
    serve(&mut shell, engine);
    shell
}

/// Saves every option of one key in the app scope through the page, one after another,
/// and checks each reached the engine at the version the page read and is in force.
/// Input: the key.
fn saves_every_option(key: &str) {
    let mut engine = Engine::new();
    let mut shell = page(&mut engine, Scope::App);
    let keys = choices();
    let index = keys
        .iter()
        .position(|choice| choice.key == key)
        .expect("the key is listed");
    for _ in 0..index {
        shell.key(Key::Down);
    }
    let options = keys[index].options.clone();
    assert!(options.len() >= 2, "{key} has a choice");
    let name = label(key);
    let (proposed, _) = option_text(key, &keys[index].default);
    assert!(shows(&mut shell, &name, &format!("Default ({proposed})")));
    for (at, option) in options.iter().enumerate() {
        let before = engine.settings_view(None);
        let start = options
            .iter()
            .position(|candidate| Some(candidate) == setting(&before.effective, key).as_ref())
            .expect("the value in force is an option");
        shell.key(Key::Enter);
        for _ in 0..(at + options.len() - start) % options.len() {
            shell.key(Key::Right);
        }
        let (shown, meaning) = option_text(key, option);
        assert!(!meaning.is_empty(), "{key} {option}");
        assert!(
            shows(&mut shell, &name, &format!("{shown} \u{2190}\u{2192}")),
            "{key} {option}"
        );
        shell.key(Key::Enter);
        let job = one(&mut shell);
        let ChecksJob::SaveSettings {
            project: None,
            values,
            version,
        } = &job
        else {
            panic!("{key} {option}: the choice saves the app values");
        };
        assert_eq!(*version, before.version);
        assert_eq!(here(values, key).as_ref(), Some(option));
        reply(&mut shell, &mut engine, job);
        serve(&mut shell, &mut engine);
        let after = engine.settings_view(None);
        assert_eq!(after.version, before.version + 1);
        assert_eq!(here(&after.values, key).as_ref(), Some(option));
        assert_eq!(setting(&after.effective, key).as_ref(), Some(option));
        assert!(shows(&mut shell, &name, &shown), "{key} {option}");
        assert!(!shows(&mut shell, &name, "Default ("), "{key} {option}");
    }
}

#[test]
fn the_accept_gate_saves_each_option() {
    saves_every_option("gate");
}

#[test]
fn runs_at_once_saves_each_option() {
    saves_every_option("parallel");
}

#[test]
fn failed_snapshots_kept_saves_each_option() {
    saves_every_option("keep_failed_snapshots");
}

#[test]
fn agent_run_requests_saves_each_option() {
    saves_every_option("agent_requests");
}

#[test]
fn the_test_inventory_saves_each_option() {
    saves_every_option("inventory");
}

#[test]
fn added_skip_markers_saves_each_option() {
    saves_every_option("skip_markers");
}

#[test]
fn protected_paths_saves_each_option() {
    saves_every_option("protected_change");
}

#[test]
fn removed_tests_saves_each_option() {
    saves_every_option("removed_tests");
}

#[test]
fn every_key_and_option_has_a_name_and_a_line() {
    let keys = choices();
    assert_eq!(
        keys.iter().map(|choice| choice.key).collect::<Vec<_>>(),
        [
            "gate",
            "parallel",
            "keep_failed_snapshots",
            "agent_requests",
            "inventory",
            "skip_markers",
            "protected_change",
            "removed_tests",
        ]
    );
    for choice in &keys {
        assert_ne!(label(choice.key), choice.key);
        assert!(label(choice.key).chars().count() < LABEL_CELLS);
        for option in &choice.options {
            let (name, meaning) = option_text(choice.key, option);
            assert!(name.chars().count() < NAME_CELLS, "{name}");
            assert!(!meaning.is_empty(), "{} {option}", choice.key);
        }
    }
    assert_eq!(
        option_text("gate", &serde_json::json!("someday")),
        ("\"someday\"".to_owned(), String::new())
    );
}

#[test]
fn a_project_override_shows_over_the_app_value_and_u_resets_it() {
    let mut engine = Engine::new();
    engine
        .save_settings(
            None,
            &ChecksChoice {
                gate: Some(crate::checks::settings::GateMode::Warn),
                ..ChecksChoice::default()
            },
            0,
        )
        .unwrap();
    let mut shell = page(&mut engine, project());
    assert!(shows(&mut shell, "Accept gate", "App value (Warn)"));
    assert!(says(
        &mut shell,
        "Only Fieldnotes Mobile follows the values set here"
    ));

    shell.key(Key::Enter);
    shell.key(Key::Right);
    assert!(shows(&mut shell, "Accept gate", "Require \u{2190}\u{2192}"));
    assert!(shows(
        &mut shell,
        "Require",
        "Accept waits until every required check passed"
    ));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    let ChecksJob::SaveSettings {
        project: Some(saved),
        values,
        version: 0,
    } = &job
    else {
        panic!("the change saves the project's values at version 0");
    };
    assert_eq!(*saved, fieldnotes());
    assert_eq!(
        values.gate,
        Some(crate::checks::settings::GateMode::Require)
    );
    reply(&mut shell, &mut engine, job);
    serve(&mut shell, &mut engine);
    assert!(shows(&mut shell, "Accept gate", "Require"));
    assert!(!shows(&mut shell, "Accept gate", "App value"));
    assert!(says(&mut shell, "u Use the app value"));

    shell.key(Key::Char('u'));
    let job = one(&mut shell);
    let ChecksJob::SaveSettings {
        values, version, ..
    } = &job
    else {
        panic!("a reset saves the project's values");
    };
    assert_eq!(*values, ChecksChoice::default());
    assert_eq!(*version, 1);
    reply(&mut shell, &mut engine, job);
    serve(&mut shell, &mut engine);
    assert!(shows(&mut shell, "Accept gate", "App value (Warn)"));
    assert_eq!(
        engine.settings_view(Some(&fieldnotes())).effective.gate,
        crate::checks::settings::GateMode::Warn
    );
    shell.key(Key::Char('u'));
    assert!(jobs(&mut shell).is_empty(), "nothing set, nothing to reset");
    assert!(says(&mut shell, "This setting already uses the app value."));
}

#[test]
fn a_save_over_a_newer_version_reads_the_current_values_again() {
    let mut engine = Engine::new();
    let mut shell = page(&mut engine, project());
    engine
        .save_settings(
            Some(&fieldnotes()),
            &ChecksChoice {
                parallel: Some(3),
                ..ChecksChoice::default()
            },
            0,
        )
        .unwrap();
    shell.key(Key::Enter);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let job = one(&mut shell);
    reply(&mut shell, &mut engine, job);
    assert!(says(&mut shell, "Changed somewhere else."));
    serve(&mut shell, &mut engine);
    assert!(shows(&mut shell, "Runs at once", "3 at once"));
    assert_eq!(
        engine.settings_view(Some(&fieldnotes())).effective.parallel,
        3
    );
}

#[test]
fn approving_a_profile_asks_first_and_keep_it_changes_nothing() {
    let mut engine = Engine::new();
    let id = engine.propose(&["unit", "lint"]);
    let short: String = id.as_str().chars().take(PROFILE_SHOWN).collect();
    let mut shell = page(&mut engine, project());
    assert!(shows(&mut shell, &short, "Proposed"));
    assert!(shows(&mut shell, &short, "2 checks, proposed by owner"));
    for _ in 0..choices().len() {
        shell.key(Key::Down);
    }
    assert!(says(&mut shell, "p Approve"));

    shell.key(Key::Char('p'));
    assert!(says(&mut shell, &format!("Approve check profile {short}?")));
    assert!(says(&mut shell, "Checks then run as it says: 2 checks."));
    assert!(jobs(&mut shell).is_empty(), "nothing is sent before Enter");
    shell.key(Key::Esc);
    assert!(!says(&mut shell, "Approve check profile"));
    assert!(jobs(&mut shell).is_empty(), "Keep it sends nothing");
    assert_eq!(engine.profiles().unwrap()[0].state, ProfileState::Proposed);

    shell.key(Key::Char('p'));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert_eq!(
        job_profile(&job),
        Some((true, &id)),
        "Enter approves the profile shown"
    );
    reply(&mut shell, &mut engine, job);
    serve(&mut shell, &mut engine);
    assert!(says(&mut shell, "Approved."));
    assert_eq!(engine.profiles().unwrap()[0].state, ProfileState::Current);
    assert!(shows(&mut shell, &short, "Current"));
    assert!(!says(&mut shell, "p Approve"));

    shell.key(Key::Char('p'));
    assert!(says(&mut shell, "This profile is already the current one."));
    assert!(jobs(&mut shell).is_empty());
}

#[test]
fn revoking_a_profile_asks_first_and_leaves_the_project_without_one() {
    let mut engine = Engine::new();
    let id = engine.propose(&["unit"]);
    let mut shell = page(&mut engine, project());
    for _ in 0..choices().len() {
        shell.key(Key::Down);
    }
    shell.key(Key::Char('p'));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    reply(&mut shell, &mut engine, job);
    serve(&mut shell, &mut engine);

    shell.key(Key::Char('v'));
    let short: String = id.as_str().chars().take(PROFILE_SHOWN).collect();
    assert!(says(&mut shell, &format!("Revoke check profile {short}?")));
    assert!(says(
        &mut shell,
        "the project has none until you approve one."
    ));
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert_eq!(job_profile(&job), Some((false, &id)));
    reply(&mut shell, &mut engine, job);
    serve(&mut shell, &mut engine);
    assert!(says(&mut shell, "Revoked."));
    assert_eq!(engine.profiles().unwrap()[0].state, ProfileState::Revoked);
    assert!(shows(&mut shell, &short, "Revoked"));
    assert!(says(&mut shell, "p Approve"));
    assert!(!says(&mut shell, "v Revoke"));
}

#[test]
fn a_project_without_profiles_says_how_one_arrives() {
    let mut engine = Engine::new();
    let mut shell = page(&mut engine, project());
    assert!(says(&mut shell, "Check profiles"));
    assert!(says(&mut shell, "No profile yet."));
    let mut app = page(&mut engine, Scope::App);
    assert!(!says(&mut app, "Check profiles"));
}

#[test]
fn check_settings_open_from_go_to_and_from_a_project() {
    let mut ctx = ctx(80, 24);
    assert!(
        targets(&ctx)
            .iter()
            .any(|target| target.label == "Settings / Checks"
                && target.kind == "Settings"
                && target.place == Place::CheckSettings { project: None })
    );
    let mut app = open(&Place::CheckSettings { project: None }, &mut ctx);
    assert_eq!(app.crumbs(), ["Settings", "Checks"]);
    assert!(matches!(app.key(&Key::Esc, &mut ctx), Flow::Ignored));
    assert!(matches!(app.key(&Key::Char('a'), &mut ctx), Flow::Ignored));

    let place = Place::CheckSettings {
        project: Some((fieldnotes(), "Fieldnotes Mobile".into())),
    };
    let mut own = open(&place, &mut ctx);
    assert_eq!(own.crumbs(), ["Fieldnotes Mobile", "Tests", "Settings"]);
    assert_eq!(own.place(), Some(place));
    assert!(matches!(
        own.key(&Key::Char('a'), &mut ctx),
        Flow::Open(Place::CheckSettings { project: None })
    ));
}

fn job_profile(job: &ChecksJob) -> Option<(bool, &ProfileId)> {
    match job {
        ChecksJob::Approve { profile, .. } => Some((true, profile)),
        ChecksJob::Revoke { profile, .. } => Some((false, profile)),
        _ => None,
    }
}
