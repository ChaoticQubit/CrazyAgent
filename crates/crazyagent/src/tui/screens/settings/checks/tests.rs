use super::*;
use crate::tui::client::OwnerEvent;
use crate::tui::client::checks::tests::{Engine, answer, check, fieldnotes};
use crate::tui::screens::open;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::{Shell, targets};
use serde_json::json;

fn jobs(shell: &mut Shell) -> Vec<ChecksJob> {
    ctx_jobs(&mut shell.ctx)
}

/// The check jobs a page sent through its context, in order. Takes the context. Returns
/// the jobs.
fn ctx_jobs(ctx: &mut Ctx) -> Vec<ChecksJob> {
    std::iter::from_fn(|| ctx.next_request())
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
    assert!(says(&mut shell, "for this project only."));

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

/// A profile with every field the review shows: two ordinary checks, a check that reaches
/// the sandbox remotes at turn end and before Accept, a protected holdout with one stored
/// file, and two protected paths. Input: the engine. Output: the stored profile's id.
fn wide_profile(engine: &Engine) -> ProfileId {
    let digest = engine.holdout(b"#[test]\nfn hidden_case() {}\n");
    let mut fetch = check("fetch");
    fetch["kind"] = json!("integration");
    fetch["toolchain"] = Value::Null;
    fetch["argv"] = json!(["curl", "--fail", "https://api.example.test/v1/health"]);
    fetch["network"] = json!("remotes");
    fetch["when"] = json!(["turn_end", "before_accept"]);
    fetch["required"] = json!(false);
    let mut hidden = check("hidden");
    hidden["kind"] = json!("e2e");
    hidden["argv"] = json!(["cargo", "test", "--test", "hidden"]);
    hidden["protected"] = json!(true);
    engine.propose_body(&json!({
        "version": 1,
        "checks": [check("unit"), check("lint"), fetch],
        "holdouts": [{"check": hidden, "files": {"tests/hidden.rs": digest}}],
        "protected_paths": ["tests/", "Cargo.lock"],
    }))
}

/// Approves the selected profile the way the owner does: p reads it, PgDn shows the rest
/// of it, Enter approves it. Inputs: the shell and the engine. Output: the profile the page
/// approved.
fn approve(shell: &mut Shell, engine: &mut Engine) -> ProfileId {
    shell.key(Key::Char('p'));
    let job = one(shell);
    reply(shell, engine, job);
    for _ in 0..4 {
        render(shell, 120, 40);
        shell.key(Key::PageDown);
    }
    render(shell, 120, 40);
    shell.key(Key::Enter);
    let job = one(shell);
    let Some((true, id)) = job_profile(&job).map(|(approve, id)| (approve, id.clone())) else {
        panic!("Enter after the end approves");
    };
    reply(shell, engine, job);
    serve(shell, engine);
    id
}

#[test]
fn approving_a_profile_shows_its_whole_body_first_and_keep_it_changes_nothing() {
    let mut engine = Engine::new();
    let id = wide_profile(&engine);
    let short: String = id.as_str().chars().take(PROFILE_SHOWN).collect();
    let mut shell = page(&mut engine, project());
    assert!(shows(&mut shell, &short, "Proposed"));
    assert!(shows(
        &mut shell,
        &short,
        "3 checks and 1 holdout, proposed by owner"
    ));
    for _ in 0..choices().len() {
        shell.key(Key::Down);
    }
    assert!(says(&mut shell, "p Approve"));

    shell.key(Key::Char('p'));
    let job = one(&mut shell);
    assert!(
        matches!(&job, ChecksJob::Show { profile, .. } if *profile == id),
        "p reads the whole profile before anything is approved"
    );
    reply(&mut shell, &mut engine, job);
    assert!(says(&mut shell, &format!("Approve check profile {short}?")));
    assert!(says(&mut shell, "Proposed by owner."));
    assert!(shows(&mut shell, "unit", "unit, required"));
    assert!(shows(
        &mut shell,
        "Command",
        r#"["cargo","test","--locked","--offline"]"#
    ));
    assert!(shows(&mut shell, "fetch", "integration, optional"));
    assert!(shows(
        &mut shell,
        "Command",
        r#"["curl","--fail","https://api.example.test/v1/health"]"#
    ));
    assert!(shows(
        &mut shell,
        "Network",
        "The project's sandbox remotes"
    ));
    assert!(shows(&mut shell, "Runs at", "turn end, before Accept"));
    assert!(shows(&mut shell, "Toolchain", "None"));
    assert!(shows(&mut shell, "Timeout", "600 s"));
    assert!(shows(&mut shell, "Reports", "libtest output"));
    assert!(
        !says(&mut shell, "Protected paths"),
        "the rest of the profile is below the first screen"
    );
    assert!(says(&mut shell, "PgDn Read on"));
    assert!(jobs(&mut shell).is_empty(), "nothing is sent before Enter");

    shell.key(Key::Enter);
    assert!(says(&mut shell, "Read the whole profile first."));
    assert!(
        jobs(&mut shell).is_empty(),
        "Enter before the end is shown approves nothing"
    );

    shell.key(Key::PageDown);
    assert!(shows(&mut shell, "hidden", "e2e, required, protected"));
    assert!(shows(
        &mut shell,
        "Command",
        r#"["cargo","test","--test","hidden"]"#
    ));
    assert!(shows(&mut shell, "Files", "tests/hidden.rs"));
    assert!(says(&mut shell, "Protected paths"));
    assert!(
        render(&mut shell, 120, 40)
            .iter()
            .any(|row| row.trim() == "tests/")
    );
    assert!(
        render(&mut shell, 120, 40)
            .iter()
            .any(|row| row.trim() == "Cargo.lock")
    );
    assert!(says(&mut shell, "Enter approves this profile."));
    assert!(says(&mut shell, "Enter Approve"));
    assert!(
        !says(&mut shell, "hidden_case"),
        "a holdout's bytes stay with the engine"
    );

    shell.key(Key::Esc);
    assert!(!says(&mut shell, "Approve check profile"));
    assert!(jobs(&mut shell).is_empty(), "Keep it sends nothing");
    assert_eq!(engine.profiles().unwrap()[0].state, ProfileState::Proposed);
    assert!(says(&mut shell, "p Approve"));

    assert_eq!(
        approve(&mut shell, &mut engine),
        id,
        "Enter approves the profile shown"
    );
    assert!(says(&mut shell, "Approved."));
    assert_eq!(engine.profiles().unwrap()[0].state, ProfileState::Current);
    assert!(shows(&mut shell, &short, "Current"));
    assert!(!says(&mut shell, "p Approve"));

    shell.key(Key::Char('p'));
    assert!(says(&mut shell, "This profile is already the current one."));
    assert!(jobs(&mut shell).is_empty());
}

#[test]
fn a_profile_the_engine_no_longer_holds_is_never_shown_for_approval() {
    let mut engine = Engine::new();
    let id = engine.propose(&["unit"]);
    let mut shell = page(&mut engine, project());
    for _ in 0..choices().len() {
        shell.key(Key::Down);
    }
    shell.key(Key::Char('p'));
    let job = one(&mut shell);
    let ChecksJob::Show { project, .. } = job else {
        panic!("p reads the profile first");
    };
    let gone = ProfileId::new("0".repeat(64)).unwrap();
    assert_ne!(gone, id);
    reply(
        &mut shell,
        &mut engine,
        ChecksJob::Show {
            project,
            profile: gone,
        },
    );
    assert!(!says(&mut shell, "Approve check profile"));
    assert!(says(&mut shell, "Could not read the check profile."));
    assert!(says(&mut shell, "That check profile is no longer stored."));
    shell.key(Key::Enter);
    assert!(jobs(&mut shell).is_empty(), "nothing is approved unseen");
    assert_eq!(engine.profiles().unwrap()[0].state, ProfileState::Proposed);
}

#[test]
fn revoking_a_profile_asks_first_and_leaves_the_project_without_one() {
    let mut engine = Engine::new();
    let id = engine.propose(&["unit"]);
    let mut shell = page(&mut engine, project());
    for _ in 0..choices().len() {
        shell.key(Key::Down);
    }
    assert_eq!(approve(&mut shell, &mut engine), id);

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
    assert!(says(&mut shell, "No check profile yet."));
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

#[test]
fn enter_approves_only_after_every_line_of_the_profile_was_shown() {
    let mut engine = Engine::new();
    let ids: Vec<String> = (0..12).map(|at| format!("check-{at:02}")).collect();
    let id = engine.propose(&ids.iter().map(String::as_str).collect::<Vec<_>>());
    let mut shell = page(&mut engine, project());
    for _ in 0..choices().len() {
        shell.key(Key::Down);
    }
    shell.key(Key::Char('p'));
    let job = one(&mut shell);
    reply(&mut shell, &mut engine, job);
    assert!(says(&mut shell, "check-00"));
    assert!(!says(&mut shell, "check-11"));

    shell.key(Key::End);
    assert!(says(&mut shell, "check-11"));
    assert!(says(&mut shell, "Enter approves this profile."));
    assert!(!says(&mut shell, "check-00"));
    assert!(
        says(&mut shell, "PgDn Read on"),
        "jumping to the end skipped lines no draw has shown"
    );
    shell.key(Key::Enter);
    assert!(says(&mut shell, "Read the whole profile first."));
    assert!(jobs(&mut shell).is_empty(), "nothing is approved unseen");

    shell.key(Key::Home);
    assert!(says(&mut shell, "check-00"));
    let mut pages = 0;
    while !says(&mut shell, "Enter Approve") {
        assert!(pages < 8, "paging down reaches the end");
        shell.key(Key::PageDown);
        pages += 1;
    }
    assert!(pages >= 2, "the profile is longer than two screens");
    shell.key(Key::Enter);
    let job = one(&mut shell);
    assert_eq!(job_profile(&job), Some((true, &id)));
}

#[test]
fn no_revoke_opens_while_a_profile_is_being_read_for_approval() {
    let mut engine = Engine::new();
    engine.propose(&["unit"]);
    engine.propose(&["unit", "lint"]);
    let listed = engine.profiles().unwrap();
    let first: String = listed[0].id.as_str().chars().take(PROFILE_SHOWN).collect();
    let second: String = listed[1].id.as_str().chars().take(PROFILE_SHOWN).collect();
    let mut shell = page(&mut engine, project());
    for _ in 0..choices().len() {
        shell.key(Key::Down);
    }
    shell.key(Key::Char('p'));
    let job = one(&mut shell);
    assert!(
        matches!(&job, ChecksJob::Show { profile, .. } if *profile == listed[0].id),
        "p reads the first profile"
    );

    shell.key(Key::Down);
    shell.key(Key::Char('v'));
    assert!(
        !says(&mut shell, "Revoke check profile"),
        "no confirm opens while the read waits"
    );
    shell.key(Key::Char('p'));
    assert!(jobs(&mut shell).is_empty(), "one request at a time");

    reply(&mut shell, &mut engine, job);
    assert!(says(&mut shell, &format!("Approve check profile {first}?")));
    assert!(!says(&mut shell, "Revoke check profile"));
    shell.key(Key::Esc);
    assert!(jobs(&mut shell).is_empty(), "Keep it sends nothing");
    assert!(
        listed_states(&engine)
            .iter()
            .all(|state| *state == ProfileState::Proposed)
    );

    shell.key(Key::Char('v'));
    assert!(says(&mut shell, &format!("Revoke check profile {second}?")));
    shell.key(Key::Esc);
    assert!(jobs(&mut shell).is_empty());
}

fn listed_states(engine: &Engine) -> Vec<ProfileState> {
    engine
        .profiles()
        .unwrap()
        .iter()
        .map(|profile| profile.state)
        .collect()
}

/// The help bar as an 80x24 window draws it, every row of it. Takes the shell. Returns the
/// bar rows joined by newlines.
fn bar_at_80(shell: &mut Shell) -> String {
    crate::tui::screens::settings_accounts::tests::bar_rows(&render(shell, 80, 24)).join("\n")
}

#[test]
fn every_check_settings_key_stays_in_the_hint_bar_at_80_columns() {
    let mut engine = Engine::new();
    engine
        .save_settings(
            Some(&fieldnotes()),
            &ChecksChoice {
                gate: Some(crate::checks::settings::GateMode::Warn),
                ..ChecksChoice::default()
            },
            0,
        )
        .unwrap();
    let mut own = page(&mut engine, project());
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
    let mut app = page(&mut engine, Scope::App);
    let bar = bar_at_80(&mut app);
    for key in ["Enter Change", "Move", "r Read again", "Esc Back", "? Help"] {
        assert!(bar.contains(key), "{key}:\n{bar}");
    }
}

#[test]
fn check_settings_use_the_desktop_words() {
    let mut engine = Engine::new();
    let mut app = page(&mut engine, Scope::App);
    let rows = render(&mut app, 120, 40);
    let at = |text: &str| {
        rows.iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text}:\n{}", rows.join("\n")))
    };
    let title = rows
        .iter()
        .position(|row| row.trim() == "Checks")
        .unwrap_or_else(|| panic!("a Checks title row:\n{}", rows.join("\n")));
    let purpose = at("The check settings every project starts from");
    let rules = at("Check rules");
    let effect = at("Changes apply to check runs queued after you save and to the next Accept.");
    let table = at("Accept gate");
    assert!(title < purpose && purpose < rules && rules < effect && effect < table);
    assert!(!says(&mut app, "for this project only."));
    assert!(!says(&mut app, "Check settings"));

    let mut own = page(&mut engine, project());
    assert!(says(&mut own, "Check rules"));
    assert!(says(
        &mut own,
        "Changes apply to check runs queued after you save and to the next Accept."
    ));
    assert!(says(&mut own, "for this project only."));
    assert!(says(&mut own, "Check profiles"));
    assert!(says(
        &mut own,
        "A check runs only under a profile you approved. Agents can propose one; nothing runs until you approve it here."
    ));
    assert!(says(&mut own, "No check profile yet."));
    assert!(!says(
        &mut own,
        "The check settings every project starts from"
    ));
}

#[test]
fn the_last_check_setting_s_options_stay_in_view_at_80x24() {
    let mut engine = Engine::new();
    let mut shell = Shell::with(ctx(80, 24), Box::new(CheckSettingsPage::new(project())));
    serve(&mut shell, &mut engine);
    for _ in 0..7 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 24).join("\n");
    for words in [
        "Removed tests",
        "A test a run no longer reports is not looked",
        "A test a run no longer reports is recorded",
        "A test a run no longer reports blocks Accept",
    ] {
        assert!(rows.contains(words), "{words}:\n{rows}");
    }
}

#[test]
fn a_chosen_check_profile_stays_in_view_at_80x24() {
    let mut engine = Engine::new();
    wide_profile(&engine);
    let mut shell = Shell::with(ctx(80, 24), Box::new(CheckSettingsPage::new(project())));
    serve(&mut shell, &mut engine);
    for _ in 0..8 {
        shell.key(Key::Down);
    }
    let rows = render(&mut shell, 80, 24).join("\n");
    assert!(
        rows.contains("3 checks and 1 holdout"),
        "the chosen profile row:\n{rows}"
    );
    assert!(rows.contains("p Approve"), "{rows}");
}

/// A check settings page outside any shell, read from the engine as a shell would serve it,
/// so a test can ask the page itself which lines it keeps in view. Inputs: the engine and the
/// scope. Output: the page and its context.
fn served_page(engine: &mut Engine, scope: Scope) -> (CheckSettingsPage, Ctx) {
    let mut ctx = ctx(80, 24);
    let mut page = CheckSettingsPage::new(scope);
    page.shown(&mut ctx);
    for _ in 0..8 {
        let sent = ctx_jobs(&mut ctx);
        if sent.is_empty() {
            return (page, ctx);
        }
        for job in sent {
            let event = answer(job, engine);
            page.arrive(
                &Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(event)))),
                &mut ctx,
            );
        }
    }
    panic!("the page kept asking");
}

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn the_focused_part_runs_from_the_chosen_setting_through_its_last_option() {
    let mut engine = Engine::new();
    wide_profile(&engine);
    let (mut page, mut ctx) = served_page(&mut engine, project());
    let all = choices();
    let last = all.len() - 1;
    for _ in 0..last {
        page.key(&Key::Down, &mut ctx);
    }
    page.key(&Key::Enter, &mut ctx);
    let (lines, (first, end)) = page.body(76, &ctx);
    assert!(
        line_text(&lines[first]).contains("Removed tests"),
        "{}",
        line_text(&lines[first])
    );
    assert_eq!(
        end,
        first + 1 + all[last].options.len(),
        "the row, a blank, every option"
    );
    assert!(
        line_text(&lines[end]).contains("A test a run no longer reports"),
        "{}",
        line_text(&lines[end])
    );
    page.key(&Key::Esc, &mut ctx);
    page.key(&Key::Down, &mut ctx);
    let (lines, (first, end)) = page.body(76, &ctx);
    assert_eq!(first, end, "a chosen profile is its row alone");
    assert!(
        line_text(&lines[first]).contains("3 checks and 1 holdout"),
        "{}",
        line_text(&lines[first])
    );
}
