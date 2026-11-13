use super::*;
use crate::harnesses::update::{CheckWhen, HarnessNeeds, LastUpdate};
use crate::harnesses::{Search, catalog};
use crate::setup::{Dependency, DependencyState, DependencyStatus, SetupStatus, Version};
use crate::tui::client::OwnerEvent;
use crate::tui::client::app_setup::{AppSetupEvent, AppSetupJob};
use crate::tui::screens::settings_accounts::tests::{dataset, fixture, list_ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::Shell;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Found {
    _root: tempfile::TempDir,
    entries: Vec<HarnessEntry>,
    codex: PathBuf,
}

fn found() -> Found {
    let root = tempfile::Builder::new()
        .prefix("ca-th-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    for name in ["codex", "claude"] {
        let path = root.path().join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let entries = catalog(&Search::new(
        vec![root.path().to_path_buf()],
        None,
        Vec::new(),
    ));
    let codex = root.path().join("codex");
    Found {
        _root: root,
        entries,
        codex,
    }
}

fn harness_ctx(width: u16, height: u16) -> Ctx {
    let mut ctx = list_ctx(width, height, dataset(None).1);
    ctx.store.harnesses = Load::Ready((2, 9));
    ctx
}

fn put(row: &str, column: usize, text: &str, room: usize) -> String {
    let mut cells: Vec<char> = row.chars().collect();
    let mut value: Vec<char> = text.chars().collect();
    value.resize(room, ' ');
    cells.splice(column..column + room, value);
    cells.into_iter().collect()
}

/// Writes the engine's Works with words and Limited control marks over a design frame's
/// harness rows. Takes the rows, the first harness row, the two columns, the Works with
/// width and how many harness rows show. Returns nothing.
fn engine_table(
    rows: &mut [String],
    first: usize,
    control: usize,
    works: usize,
    room: usize,
    shown: usize,
) {
    let words = [
        "Any account",
        "Subscription, API key",
        "Any account",
        "Subscription, API key",
        "Subscription, API key",
        "Any API key or URL",
        "Subscription",
        "Subscription, API key, Local model",
        "Any account",
    ];
    assert!(shown <= words.len());
    for (index, word) in words.iter().enumerate().take(shown) {
        let row = &mut rows[first + index];
        *row = put(row, works, &cut(word, room, "…"), room);
        if index == 5 || index == 7 {
            *row = put(row, control, "◧ Limited control", 17);
        }
    }
}

#[test]
fn harness_list_frames_match_the_design() {
    let found = found();
    let nine: Vec<HarnessEntry> = found.entries.iter().take(9).cloned().collect();
    let path = found.codex.display().to_string();

    let mut shell = Shell::with(harness_ctx(80, 24), Box::new(Harnesses::with(nine.clone())));
    let mut expected = fixture("harnesses-80", &[]);
    engine_table(&mut expected, 6, 35, 54, 23, 8);
    expected[16] = put(&expected[16], 15, &path, 63);
    expected[19] = " ".repeat(80);
    expected[20] = " ".repeat(80);
    assert_eq!(render(&mut shell, 80, 24), expected);

    let mut shell = Shell::with(harness_ctx(120, 36), Box::new(Harnesses::with(nine)));
    let mut expected = fixture("harnesses-120", &[]);
    engine_table(&mut expected, 9, 37, 56, 62, 9);
    expected[20] = put(&expected[20], 15, &path, 103);
    expected[21] = expected[22].clone();
    for row in expected.iter_mut().skip(22).take(3) {
        *row = " ".repeat(120);
    }
    assert_eq!(render(&mut shell, 120, 36), expected);
}

#[test]
fn a_harness_that_is_not_installed_says_how_to_add_it() {
    let found = found();
    let nine: Vec<HarnessEntry> = found.entries.iter().take(9).cloned().collect();
    let mut shell = Shell::with(harness_ctx(80, 24), Box::new(Harnesses::with(nine)));
    for _ in 0..4 {
        shell.key(Key::Down);
    }
    let mut expected = fixture("harness-limited-80", &[]);
    engine_table(&mut expected, 6, 35, 54, 24, 9);
    expected[17] = format!(
        "{:<80}",
        "  Limited control: tools are allowed or denied only through Cursor's"
    );
    expected[18] = format!("{:<80}", "  permission file.");
    assert_eq!(render(&mut shell, 80, 24), expected);
}

#[test]
fn every_engine_harness_scrolls_in_a_short_window() {
    let found = found();
    assert_eq!(found.entries.len(), crate::harnesses::specs().len());
    let mut shell = Shell::with(
        harness_ctx(80, 24),
        Box::new(Harnesses::with(found.entries.clone())),
    );
    shell.key(Key::End);
    let rows = render(&mut shell, 80, 24);
    let last = &found.entries[found.entries.len() - 1].name;
    assert!(
        rows[14].contains(&format!("> {}", cut(last, 12, "…"))),
        "{}",
        rows[14]
    );
    assert!(rows[4].contains(&format!("2 of {} installed", found.entries.len())));
    assert!(
        rows[6..15]
            .iter()
            .all(|row| row.chars().nth(77) != Some(' ')),
        "scroll bar shows"
    );
}

#[test]
fn check_again_asks_the_engine_and_the_reply_updates_the_counts() {
    let found = found();
    let mut ctx = harness_ctx(120, 36);
    ctx.store.harnesses = Load::NotReady;
    let mut screen = Harnesses::new();
    screen.shown(&mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Settings(_))));
    assert!(matches!(
        ctx.next_request(),
        Some(Request::Settings(job)) if matches!(*job, SettingsJob::HarnessUpdateSettings)
    ));
    assert!(screen.busy(&ctx));
    screen.key(&Key::Char('r'), &mut ctx);
    assert_eq!(ctx.queued(), 0, "one check at a time");
    let reply = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Harnesses(Ok(found.entries.clone())),
    ))));
    screen.arrive(&reply, &mut ctx);
    assert!(!screen.busy(&ctx));
    assert_eq!(ctx.store.harnesses, Load::Ready((2, found.entries.len())));
    screen.key(&Key::Char('r'), &mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Settings(_))));
    let failed = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Harnesses(Err("controller_pending_intent".into())),
    ))));
    screen.arrive(&failed, &mut ctx);
    assert_eq!(ctx.message().unwrap().lead, "Could not check harnesses.");
    assert_eq!(
        screen.entries.len(),
        found.entries.len(),
        "a failed check keeps the list"
    );
}

#[test]
fn words_follow_the_engine_catalog() {
    let found = found();
    let codex = &found.entries[0];
    assert_eq!(
        (status_word(codex), control_word(codex)),
        ("Ready", "Full control")
    );
    let mut cursor = found
        .entries
        .iter()
        .find(|entry| entry.id == "cursor-agent")
        .unwrap()
        .clone();
    assert_eq!(
        (status_word(&cursor), control_word(&cursor)),
        ("Not installed", "Limited control")
    );
    cursor.executable = Some(found.codex.clone());
    cursor.status = HarnessStatus::Limited {
        reason: "installed, but CrazyAgent cannot start it yet".into(),
    };
    assert_eq!(status_word(&cursor), "Can't start");
    assert_eq!(
        control_sentence(&cursor),
        "Limited control: installed, but CrazyAgent cannot start it yet; tools are allowed or denied only through Cursor's permission file."
    );
    let opencode = found
        .entries
        .iter()
        .find(|entry| entry.id == "opencode")
        .unwrap();
    assert_eq!(control_word(opencode), "Unknown");
    assert_eq!(works_with(&[]), "Not known yet");
    assert_eq!(works_with(&[AccountKind::Local]), "Local model");
}

const BREW_LINE: &str = "/opt/homebrew/bin/brew upgrade --cask claude-code@latest";

#[derive(Debug, PartialEq)]
enum Sent {
    Versions(String, bool),
    Update(String),
    Install(String),
    Settings,
    Find(String),
    Choose(String, Option<String>),
    Catalog,
    Setup,
    InstallNode,
    Other,
}

fn sent(ctx: &mut Ctx) -> Vec<Sent> {
    let mut all = Vec::new();
    while let Some(request) = ctx.next_request() {
        if let Request::Settings(job) = request {
            all.push(match *job {
                SettingsJob::HarnessVersions { id, look } => Sent::Versions(id, look),
                SettingsJob::HarnessUpdate { id } => Sent::Update(id),
                SettingsJob::HarnessInstall { id } => Sent::Install(id),
                SettingsJob::HarnessUpdateSettings => Sent::Settings,
                SettingsJob::HarnessFind { id } => Sent::Find(id),
                SettingsJob::HarnessChoosePath { id, path } => Sent::Choose(id, path),
                SettingsJob::Harnesses => Sent::Catalog,
                SettingsJob::AppSetup(AppSetupJob::Status) => Sent::Setup,
                SettingsJob::AppSetup(AppSetupJob::Install {
                    name: Dependency::Node,
                    ..
                }) => Sent::InstallNode,
                _ => Sent::Other,
            });
        }
    }
    all
}

fn versions_of(installed: &str, latest: Latest) -> HarnessVersions {
    HarnessVersions {
        id: "claude-code".into(),
        method: Method::Homebrew {
            cask: "claude-code@latest".into(),
        },
        installed: Some(installed.into()),
        latest,
        command: Some(BREW_LINE.into()),
        install: None,
        refusal: None,
        guide: None,
        running: false,
        last: None,
        needs: None,
    }
}

fn newer() -> HarnessVersions {
    versions_of(
        "2.1.309",
        Latest::Newer {
            version: "2.1.322".into(),
        },
    )
}

fn settings_with(mode: UpdateMode, when: CheckWhen) -> UpdateSettings {
    UpdateSettings {
        mode,
        when,
        ..UpdateSettings::default()
    }
}

fn arrival(event: SettingsEvent) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(event))))
}

fn deliver(shell: &mut Shell, versions: HarnessVersions) {
    let id = versions.id.clone();
    shell.arrive(arrival(SettingsEvent::HarnessVersions(id, Ok(versions))));
}

fn press(shell: &mut Shell, key: char, versions: HarnessVersions) {
    shell.key(Key::Char(key));
    deliver(shell, versions);
}

fn select(shell: &mut Shell, found: &Found, id: &str) {
    let at = found
        .entries
        .iter()
        .position(|entry| entry.id == id)
        .unwrap();
    shell.key(Key::Home);
    for _ in 0..at {
        shell.key(Key::Down);
    }
}

fn ended(from: Option<&str>, to: Option<&str>) -> HarnessVersions {
    HarnessVersions {
        installed: to.map(str::to_owned),
        latest: Latest::UpToDate,
        last: Some(LastUpdate {
            ok: true,
            code: None,
            output: String::new(),
            from: from.map(str::to_owned),
            to: to.map(str::to_owned),
            ended_ms: 1,
        }),
        ..newer()
    }
}

fn jobs(calls: &[Sent]) -> usize {
    calls
        .iter()
        .filter(|call| matches!(call, Sent::Update(_) | Sent::Install(_)))
        .count()
}

const NPM_LINE: &str = "/opt/homebrew/bin/npm install -g @anthropic-ai/claude-code";

fn with_npm() -> HarnessVersions {
    HarnessVersions {
        method: Method::Npm {
            package: "@anthropic-ai/claude-code".into(),
        },
        command: Some(NPM_LINE.into()),
        ..newer()
    }
}

fn on_claude(found: &Found) -> Shell {
    on(found, "claude-code")
}

fn on(found: &Found, id: &str) -> Shell {
    let mut shell = Shell::with(
        harness_ctx(120, 36),
        Box::new(Harnesses::with(found.entries.clone())),
    );
    let at = found
        .entries
        .iter()
        .position(|entry| entry.id == id)
        .unwrap();
    for _ in 0..at {
        shell.key(Key::Down);
    }
    sent(&mut shell.ctx);
    shell
}

fn frame(shell: &mut Shell) -> String {
    render(shell, 120, 36).join("\n")
}

fn footer(shell: &mut Shell) -> String {
    crate::tui::screens::settings_accounts::tests::bar_rows(&render(shell, 120, 36)).join("\n")
}

#[test]
fn c_checks_the_selected_harness_and_the_row_shows_its_version_line() {
    let found = found();
    let mut shell = on_claude(&found);
    shell.key(Key::Char('c'));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("claude-code".into(), true)]
    );
    assert!(shell.busy(), "a check in flight shows a spinner");
    deliver(&mut shell, newer());
    assert!(!shell.busy());
    assert!(frame(&mut shell).contains("Version 2.1.309. Version 2.1.322 is available."));
}

#[test]
fn u_shows_the_exact_command_and_updates_only_after_y() {
    let found = found();
    let mut shell = on_claude(&found);
    deliver(&mut shell, newer());
    shell.key(Key::Char('u'));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("claude-code".into(), false)],
        "the line is read again before it is shown"
    );
    assert!(!frame(&mut shell).contains(BREW_LINE));
    deliver(&mut shell, newer());
    let shown = frame(&mut shell);
    assert!(shown.contains(BREW_LINE), "{shown}");
    assert!(shown.contains("Installed with Homebrew (claude-code@latest)."));
    assert!(shown.contains("Update Claude Code? y to update, any other key to cancel."));
    shell.key(Key::Char('n'));
    assert!(!frame(&mut shell).contains(BREW_LINE));
    assert_eq!(jobs(&sent(&mut shell.ctx)), 0);
    press(&mut shell, 'u', newer());
    shell.key(Key::Char('y'));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![
            Sent::Versions("claude-code".into(), false),
            Sent::Update("claude-code".into())
        ]
    );
}

#[test]
fn u_shows_the_line_the_engine_has_now_not_the_one_read_when_the_page_opened() {
    let found = found();
    let mut shell = on_claude(&found);
    deliver(&mut shell, newer());
    press(&mut shell, 'u', with_npm());
    let shown = frame(&mut shell);
    assert!(shown.contains(NPM_LINE), "{shown}");
    assert!(shown.contains("Installed with npm (@anthropic-ai/claude-code)."));
    assert!(!shown.contains(BREW_LINE), "{shown}");
}

#[test]
fn a_confirm_closes_when_its_line_is_gone_or_changed_and_y_then_starts_nothing() {
    let found = found();
    let mut shell = on_claude(&found);
    press(&mut shell, 'u', newer());
    assert!(frame(&mut shell).contains(BREW_LINE));
    deliver(
        &mut shell,
        HarnessVersions {
            command: None,
            refusal: Some("harness_update_tool_unsafe".into()),
            ..newer()
        },
    );
    assert!(!frame(&mut shell).contains(BREW_LINE));
    assert_eq!(
        shell.ctx.message().unwrap().lead,
        "Claude Code changed while you were asked."
    );
    sent(&mut shell.ctx);
    shell.key(Key::Char('y'));
    assert_eq!(
        jobs(&sent(&mut shell.ctx)),
        0,
        "no dialog, so y starts nothing"
    );
    shell.key(Key::Char('c'));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("claude-code".into(), true)],
        "keys reach the page again"
    );
    deliver(&mut shell, newer());

    press(&mut shell, 'u', newer());
    assert!(frame(&mut shell).contains(BREW_LINE));
    deliver(&mut shell, with_npm());
    let shown = frame(&mut shell);
    assert!(!shown.contains(BREW_LINE), "{shown}");
    assert!(!shown.contains(NPM_LINE), "{shown}");
    sent(&mut shell.ctx);
    shell.key(Key::Char('y'));
    assert_eq!(jobs(&sent(&mut shell.ctx)), 0);
    press(&mut shell, 'u', with_npm());
    assert!(frame(&mut shell).contains(NPM_LINE));
    shell.key(Key::Char('y'));
    assert_eq!(jobs(&sent(&mut shell.ctx)), 1);
}

#[test]
fn a_confirm_asked_on_one_row_stays_that_rows_when_the_selection_moves() {
    let found = found();
    let mut shell = on_claude(&found);
    shell.key(Key::Char('u'));
    select(&mut shell, &found, "aider");
    deliver(&mut shell, newer());
    let shown = frame(&mut shell);
    assert!(
        shown.contains("Update Claude Code? y to update, any other key to cancel."),
        "{shown}"
    );
    assert!(shown.contains(BREW_LINE), "{shown}");
    shell.key(Key::Char('y'));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![
            Sent::Versions("claude-code".into(), false),
            Sent::Update("claude-code".into())
        ]
    );
}

#[test]
fn version_replies_for_a_page_with_no_catalog_yet_are_dropped() {
    let found = found();
    let mut shell = Shell::with(harness_ctx(120, 36), Box::new(Harnesses::new()));
    sent(&mut shell.ctx);
    deliver(&mut shell, newer());
    deliver(&mut shell, missing("aider", Some(AIDER_LINE), AIDER_GUIDE));
    assert!(shell.ctx.message().is_none());
    assert!(!frame(&mut shell).contains("Version 2.1.309"));
    shell.arrive(arrival(SettingsEvent::Harnesses(Ok(found.entries.clone()))));
    shell.arrive(arrival(SettingsEvent::HarnessUpdateSettings(Ok(
        settings_with(UpdateMode::CheckAndUpdate, CheckWhen::OnButton),
    ))));
    assert_eq!(
        sent(&mut shell.ctx).len(),
        found.entries.len(),
        "every row is read once the catalog is here"
    );
    select(&mut shell, &found, "claude-code");
    assert!(
        !frame(&mut shell).contains("Version 2.1.309"),
        "a dropped reply left nothing behind"
    );
    deliver(&mut shell, newer());
    assert!(frame(&mut shell).contains("Version 2.1.309. Version 2.1.322 is available."));
}

#[test]
fn a_second_job_waits_for_the_one_that_runs_and_the_first_row_still_ends() {
    let found = found();
    let mut shell = on_claude(&found);
    press(&mut shell, 'u', newer());
    shell.key(Key::Char('y'));
    shell.arrive(arrival(SettingsEvent::HarnessUpdated(
        "claude-code".into(),
        Ok(()),
    )));
    deliver(
        &mut shell,
        HarnessVersions {
            running: true,
            ..newer()
        },
    );
    sent(&mut shell.ctx);

    select(&mut shell, &found, "aider");
    shell.key(Key::Char('i'));
    let message = shell.ctx.message().unwrap();
    assert_eq!(
        (message.lead.as_str(), message.rest.as_str()),
        (
            "Can't install Aider.",
            "Claude Code is being updated or installed. Wait until that ends."
        )
    );
    shell.key(Key::Char('y'));
    select(&mut shell, &found, "codex");
    shell.key(Key::Char('u'));
    shell.key(Key::Char('y'));
    assert!(
        sent(&mut shell.ctx).is_empty(),
        "no read and no second job while one runs"
    );

    shell.refresh();
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("claude-code".into(), false)],
        "the running update is still read"
    );
    deliver(&mut shell, ended(Some("2.1.309"), Some("2.1.322")));
    select(&mut shell, &found, "claude-code");
    let shown = frame(&mut shell);
    assert!(!shown.contains("Updating..."), "{shown}");
    assert!(
        shown.contains("Updated from 2.1.309 to 2.1.322."),
        "{shown}"
    );
}

#[test]
fn a_job_started_somewhere_else_is_read_until_it_ends_and_holds_a_second_one() {
    let found = found();
    let mut shell = on_claude(&found);
    deliver(
        &mut shell,
        HarnessVersions {
            running: true,
            ..newer()
        },
    );
    assert!(frame(&mut shell).contains("Updating..."));
    shell.refresh();
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("claude-code".into(), false)]
    );
    shell.key(Key::Char('u'));
    assert_eq!(
        shell.ctx.message().unwrap().rest,
        "Claude Code is being updated or installed. Wait until that ends."
    );
    assert!(sent(&mut shell.ctx).is_empty());
    deliver(&mut shell, ended(Some("2.1.309"), Some("2.1.322")));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Catalog],
        "the catalog is read again"
    );
    assert!(!frame(&mut shell).contains("Updating..."));
    shell.refresh();
    assert!(sent(&mut shell.ctx).is_empty(), "polling stopped");
}

#[test]
fn a_finished_job_with_no_old_version_is_not_called_an_install() {
    assert_eq!(
        last_line(&ended(None, Some("1.2.3")), "Claude Code").as_deref(),
        Some("Version 1.2.3 is installed now. New agents use it; no restart needed.")
    );
    assert_eq!(
        last_line(&ended(None, None), "Claude Code").as_deref(),
        Some("It is installed now. New agents use it; no restart needed.")
    );
    let found = found();
    let mut shell = on_claude(&found);
    deliver(&mut shell, ended(None, Some("1.2.3")));
    let shown = frame(&mut shell);
    assert!(shown.contains("Version 1.2.3 is installed now."), "{shown}");
    assert!(!shown.contains("Installed version"), "{shown}");
}

#[test]
fn u_on_an_install_the_app_cannot_update_says_why_and_sends_nothing() {
    let found = found();
    let mut shell = on_claude(&found);
    let refused = HarnessVersions {
        method: Method::Unknown,
        command: None,
        refusal: Some("harness_update_unknown_install".into()),
        guide: Some("https://code.claude.com/docs/en/setup".into()),
        ..newer()
    };
    deliver(&mut shell, refused.clone());
    press(&mut shell, 'u', refused);
    let shown = frame(&mut shell);
    assert!(
        shown.contains("CrazyAgent can't tell how this was installed, so it won't update it."),
        "{shown}"
    );
    assert!(shown.contains("https://code.claude.com/docs/en/setup"));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("claude-code".into(), false)],
        "the read, and no update"
    );
}

#[test]
fn a_failed_check_says_could_not_check_and_keeps_u() {
    let found = found();
    let mut shell = on_claude(&found);
    let failed = versions_of(
        "2.1.309",
        Latest::Failed {
            code: "harness_update_lookup_failed".into(),
        },
    );
    deliver(&mut shell, failed.clone());
    assert!(frame(&mut shell).contains("Version 2.1.309. Couldn't check for a newer version."));
    assert!(footer(&mut shell).contains("u Update"));
    press(&mut shell, 'u', failed);
    assert!(frame(&mut shell).contains(BREW_LINE));
}

#[test]
fn the_version_line_has_the_desktop_sentences() {
    let line = |installed: Option<&str>, latest: Latest| {
        version_line(&HarnessVersions {
            installed: installed.map(str::to_owned),
            ..versions_of("x", latest)
        })
    };
    assert_eq!(
        line(Some("2.1.309"), Latest::UpToDate),
        "Version 2.1.309. Up to date."
    );
    assert_eq!(
        line(Some("2.1.309"), Latest::NoCheck),
        "Version 2.1.309. This install updates itself. Update asks it to look now."
    );
    assert_eq!(
        line(
            Some("2.1.309"),
            Latest::Failed {
                code: "harness_update_check_unreadable".into()
            }
        ),
        "Version 2.1.309. Couldn't check for a newer version."
    );
    assert_eq!(
        line(Some("2.1.309"), Latest::NotChecked),
        "Version 2.1.309."
    );
    assert_eq!(line(None, Latest::NotChecked), "Version unknown.");
}

#[test]
fn every_update_sentence_is_the_desktop_sentence_and_no_two_are_alike() {
    let desktop = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/desktop/src/api-harness-updates.ts"
    ))
    .unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for (code, sentence) in COPY {
        assert!(
            desktop.contains(&format!("{code}: \"{sentence}\",")),
            "{code}"
        );
        assert!(seen.insert(sentence), "{code}");
        assert_eq!(update_copy(code), sentence);
    }
    assert_eq!(
        update_copy("harness_update_from_a_newer_engine"),
        update_copy("harness_update_outcome_unknown")
    );
}

#[test]
fn every_code_the_engine_answers_has_its_own_sentence() {
    use crate::harnesses::update as engine;
    let codes = [
        engine::UNKNOWN_INSTALL,
        engine::WITH_APP,
        engine::TOOL_MISSING,
        engine::TOOL_UNSAFE,
        engine::NEEDS_ADMIN,
        engine::UNSUPPORTED_HERE,
        engine::CHECK_UNREADABLE,
        engine::NO_INSTALLER,
        engine::LOOKUP_FAILED,
        engine::TIMEOUT,
        engine::AS_ROOT,
        engine::START_FAILED,
        engine::BUSY,
        engine::CHECK_FAILED,
        engine::CHECK_ONLY,
        engine::INSTALL_ALREADY,
        engine::INSTALL_FAILED,
        engine::INSTALL_NOT_FOUND,
        engine::INSTALL_OFF,
        engine::NOT_INSTALLED,
        engine::SETTINGS_UNREADABLE,
        engine::UNKNOWN_HARNESS,
        engine::UPDATE_FAILED,
        engine::UPDATE_OFF,
        "harness_update_unavailable",
    ];
    for code in codes {
        assert!(COPY.iter().any(|(known, _)| *known == code), "{code}");
    }
    assert!(
        UPDATE_POLLS_MAX as u64 * 5 >= engine::UPDATE_TIME.as_secs(),
        "polling outlasts the longest update"
    );
}

#[test]
fn the_settings_decide_the_keys_and_whether_opening_the_page_checks() {
    let found = found();
    let installed = found
        .entries
        .iter()
        .filter(|entry| entry.executable.is_some())
        .count();
    let mut shell = Shell::with(
        harness_ctx(120, 36),
        Box::new(Harnesses::with(found.entries.clone())),
    );
    assert_eq!(sent(&mut shell.ctx), vec![Sent::Settings]);
    shell.arrive(arrival(SettingsEvent::HarnessUpdateSettings(Ok(
        settings_with(UpdateMode::CheckAndUpdate, CheckWhen::OnOpen),
    ))));
    let looks = sent(&mut shell.ctx);
    assert_eq!(looks.len(), found.entries.len(), "every row is read once");
    assert_eq!(
        looks
            .iter()
            .filter(|call| matches!(call, Sent::Versions(_, true)))
            .count(),
        installed,
        "only installed harnesses look for a newer version"
    );
    shell.refresh();
    assert!(sent(&mut shell.ctx).is_empty(), "one read per page opening");

    let mut shell = Shell::with(
        harness_ctx(120, 36),
        Box::new(Harnesses::with(found.entries.clone())),
    );
    sent(&mut shell.ctx);
    shell.arrive(arrival(SettingsEvent::HarnessUpdateSettings(Ok(
        settings_with(UpdateMode::CheckOnly, CheckWhen::OnButton),
    ))));
    assert!(
        sent(&mut shell.ctx)
            .iter()
            .all(|call| matches!(call, Sent::Versions(_, false)))
    );
    let hints = footer(&mut shell);
    assert!(hints.contains("c Check for updates"), "{hints}");
    assert!(!hints.contains("u Update"), "{hints}");
    shell.key(Key::Char('u'));
    assert!(frame(&mut shell).contains("Harness updates are set to Check only."));
    assert!(sent(&mut shell.ctx).is_empty());

    let mut shell = Shell::with(
        harness_ctx(120, 36),
        Box::new(Harnesses::with(found.entries.clone())),
    );
    sent(&mut shell.ctx);
    shell.arrive(arrival(SettingsEvent::HarnessUpdateSettings(Ok(
        settings_with(UpdateMode::Off, CheckWhen::OnOpen),
    ))));
    let reads = sent(&mut shell.ctx);
    assert_eq!(
        reads.len(),
        found.entries.len(),
        "Off still reads every harness, for Install and for what a found one needs to run"
    );
    assert!(
        reads
            .iter()
            .all(|call| matches!(call, Sent::Versions(_, false))),
        "Off never looks for a newer version: {reads:?}"
    );
    let hints = footer(&mut shell);
    assert!(!hints.contains("Check for updates"), "{hints}");
    shell.key(Key::Char('c'));
    assert!(frame(&mut shell).contains("Harness updates are off in settings."));
    assert!(sent(&mut shell.ctx).is_empty());
}

#[test]
fn a_running_update_is_read_again_on_each_refresh_until_it_ends() {
    let found = found();
    let mut shell = on_claude(&found);
    deliver(&mut shell, newer());
    press(&mut shell, 'u', newer());
    shell.key(Key::Char('y'));
    sent(&mut shell.ctx);
    shell.arrive(arrival(SettingsEvent::HarnessUpdated(
        "claude-code".into(),
        Ok(()),
    )));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("claude-code".into(), false)]
    );
    deliver(
        &mut shell,
        HarnessVersions {
            running: true,
            ..newer()
        },
    );
    assert!(frame(&mut shell).contains("Updating..."));
    shell.refresh();
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("claude-code".into(), false)]
    );
    shell.refresh();
    assert!(sent(&mut shell.ctx).is_empty(), "one read in flight");
    let done = HarnessVersions {
        installed: Some("2.1.322".into()),
        latest: Latest::UpToDate,
        last: Some(LastUpdate {
            ok: true,
            code: None,
            output: String::new(),
            from: Some("2.1.309".into()),
            to: Some("2.1.322".into()),
            ended_ms: 1,
        }),
        ..newer()
    };
    deliver(&mut shell, done);
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Catalog],
        "the catalog is read again"
    );
    assert!(
        frame(&mut shell)
            .contains("Updated from 2.1.309 to 2.1.322. New agents use it now; no restart needed.")
    );
    shell.refresh();
    assert!(sent(&mut shell.ctx).is_empty(), "polling stopped");
}

#[test]
fn a_refused_update_says_the_engine_reason() {
    let found = found();
    let mut shell = on_claude(&found);
    deliver(&mut shell, newer());
    press(&mut shell, 'u', newer());
    shell.key(Key::Char('y'));
    sent(&mut shell.ctx);
    shell.arrive(arrival(SettingsEvent::HarnessUpdated(
        "claude-code".into(),
        Err("harness_update_busy".into()),
    )));
    assert_eq!(
        shell.ctx.message().unwrap().rest,
        "Another harness update or install is running."
    );
    shell.refresh();
    assert!(sent(&mut shell.ctx).is_empty());
}

fn missing(id: &str, install: Option<&str>, guide: &str) -> HarnessVersions {
    HarnessVersions {
        id: id.into(),
        method: Method::Unknown,
        installed: None,
        latest: Latest::NotChecked,
        command: None,
        install: install.map(str::to_owned),
        refusal: Some(
            if install.is_some() {
                "harness_update_not_installed"
            } else {
                "harness_install_no_tool"
            }
            .into(),
        ),
        guide: Some(guide.into()),
        running: false,
        last: None,
        needs: None,
    }
}

const AIDER_LINE: &str = "/opt/homebrew/bin/brew install --formula aider";
const AIDER_GUIDE: &str = "https://aider.chat/docs/install.html";

#[test]
fn i_on_a_missing_harness_shows_the_exact_install_line_and_installs_only_after_y() {
    let found = found();
    let mut shell = on(&found, "aider");
    deliver(&mut shell, missing("aider", Some(AIDER_LINE), AIDER_GUIDE));
    assert!(footer(&mut shell).contains("i Install"));
    press(
        &mut shell,
        'i',
        missing("aider", Some(AIDER_LINE), AIDER_GUIDE),
    );
    let shown = frame(&mut shell);
    assert!(shown.contains(AIDER_LINE), "{shown}");
    assert!(shown.contains("It never uses sudo and never runs a script from a website."));
    assert!(shown.contains("When it finishes, new agents can use Aider with no engine restart."));
    assert!(shown.contains("Install Aider? y to install, any other key to cancel."));
    shell.key(Key::Esc);
    assert!(!frame(&mut shell).contains(AIDER_LINE));
    assert_eq!(jobs(&sent(&mut shell.ctx)), 0);
    press(
        &mut shell,
        'i',
        missing("aider", Some(AIDER_LINE), AIDER_GUIDE),
    );
    shell.key(Key::Char('y'));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![
            Sent::Versions("aider".into(), false),
            Sent::Install("aider".into())
        ]
    );
}

#[test]
fn i_without_an_install_line_shows_the_guide_address_and_sends_nothing() {
    let found = found();
    let guide = "https://docs.openhands.dev/openhands/usage/cli/installation";
    let mut shell = on(&found, "openhands");
    deliver(&mut shell, missing("openhands", None, guide));
    assert!(!footer(&mut shell).contains("i Install"));
    press(&mut shell, 'i', missing("openhands", None, guide));
    let shown = frame(&mut shell);
    assert!(shown.contains(guide), "{shown}");
    assert!(shown.contains(
        "CrazyAgent installs with Homebrew or npm and found no way to install this one."
    ));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("openhands".into(), false)],
        "the read, and no install"
    );
}

#[test]
fn a_finished_install_names_its_version_and_reads_the_catalog_again() {
    let found = found();
    let mut shell = on(&found, "aider");
    deliver(&mut shell, missing("aider", Some(AIDER_LINE), AIDER_GUIDE));
    press(
        &mut shell,
        'i',
        missing("aider", Some(AIDER_LINE), AIDER_GUIDE),
    );
    shell.key(Key::Char('y'));
    sent(&mut shell.ctx);
    shell.arrive(arrival(SettingsEvent::HarnessInstalled(
        "aider".into(),
        Ok(()),
    )));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Versions("aider".into(), false)]
    );
    deliver(
        &mut shell,
        HarnessVersions {
            running: true,
            ..missing("aider", Some(AIDER_LINE), AIDER_GUIDE)
        },
    );
    assert!(frame(&mut shell).contains("Installing..."));
    let lost = LastUpdate {
        ok: false,
        code: Some("harness_install_not_found".into()),
        output: String::new(),
        from: None,
        to: None,
        ended_ms: 1,
    };
    deliver(
        &mut shell,
        HarnessVersions {
            last: Some(lost),
            ..missing("aider", Some(AIDER_LINE), AIDER_GUIDE)
        },
    );
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Catalog],
        "the catalog is read again"
    );
    assert!(
        frame(&mut shell)
            .contains("The install finished, but CrazyAgent can't find Aider. Choose Check again.")
    );
    let done = HarnessVersions {
        installed: Some("0.86.2".into()),
        last: Some(LastUpdate {
            ok: true,
            code: None,
            output: String::new(),
            from: None,
            to: Some("0.86.2".into()),
            ended_ms: 2,
        }),
        ..newer()
    };
    assert_eq!(
        last_line(&done, "Aider").as_deref(),
        Some("Version 0.86.2 is installed now. New agents use it; no restart needed.")
    );
}

#[test]
fn a_refused_install_says_the_engine_reason() {
    let found = found();
    let mut shell = on(&found, "aider");
    deliver(&mut shell, missing("aider", Some(AIDER_LINE), AIDER_GUIDE));
    press(
        &mut shell,
        'i',
        missing("aider", Some(AIDER_LINE), AIDER_GUIDE),
    );
    shell.key(Key::Char('y'));
    sent(&mut shell.ctx);
    shell.arrive(arrival(SettingsEvent::HarnessInstalled(
        "aider".into(),
        Err("harness_install_off".into()),
    )));
    let message = shell.ctx.message().unwrap();
    assert_eq!(
        (message.lead.as_str(), message.rest.as_str()),
        (
            "Could not install Aider.",
            "Install from the app is off in settings."
        )
    );
}

/// A catalog with Claude Code found and Codex not installed, the case the file choice is for.
/// Output: the entries with their folder kept.
fn codex_missing() -> Found {
    let root = tempfile::tempdir().unwrap();
    let claude = root.path().join("claude");
    std::fs::write(&claude, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let entries = catalog(&Search::new(
        vec![root.path().to_path_buf()],
        None,
        Vec::new(),
    ));
    let codex = root.path().join("codex");
    assert!(
        entries
            .iter()
            .any(|entry| entry.id == "codex" && entry.executable.is_none())
    );
    Found {
        _root: root,
        entries,
        codex,
    }
}

#[test]
fn w_lists_where_the_engine_looked_for_a_missing_codex_and_f_chooses_its_file_by_path() {
    use crate::harnesses::finding::Found as Look;
    let missing = codex_missing();
    let mut shell = on(&missing, "codex");
    let hints = footer(&mut shell);
    assert!(
        hints.contains("f Choose file") && hints.contains("w Where it looked"),
        "{hints}"
    );
    assert!(!hints.contains("Forget"), "{hints}");
    shell.key(Key::Char('w'));
    shell.key(Key::Char('w'));
    assert_eq!(sent(&mut shell.ctx), vec![Sent::Find("codex".into())]);
    let looked = vec![
        "/opt/homebrew/bin".to_owned(),
        "~/.volta/bin (shim, skipped)".to_owned(),
    ];
    shell.arrive(arrival(SettingsEvent::HarnessFound(
        "codex".into(),
        Ok(Look::Missing { looked }),
    )));
    let shown = frame(&mut shell);
    assert!(
        shown.contains("Where CrazyAgent looked for codex"),
        "{shown}"
    );
    assert!(shown.contains("~/.volta/bin (shim, skipped)"), "{shown}");
    shell.key(Key::Char('q'));
    let shown = frame(&mut shell);
    assert!(
        !shown.contains("Where CrazyAgent looked for codex"),
        "{shown}"
    );
    assert!(
        shown.contains("CrazyAgent looked for codex in 2 folders and didn't find it."),
        "{shown}"
    );
    let chosen = "/Users/someone/tools/codex";
    shell.key(Key::Char('f'));
    assert!(frame(&mut shell).contains("Choose the codex file"));
    shell.key(Key::Enter);
    assert_eq!(sent(&mut shell.ctx), Vec::<Sent>::new());
    shell.key(Key::Paste(chosen.into()));
    shell.key(Key::Enter);
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Choose("codex".into(), Some(chosen.into()))]
    );
    assert!(!frame(&mut shell).contains("Choose the codex file"));
    shell.arrive(arrival(SettingsEvent::HarnessChosen(
        "codex".into(),
        Some(chosen.into()),
        Err("harness_path_shim".into()),
    )));
    let message = shell.ctx.message().unwrap();
    assert_eq!(
        (message.lead.as_str(), message.rest.as_str()),
        (
            "CrazyAgent won't use /Users/someone/tools/codex.",
            path_copy("harness_path_shim")
        )
    );
    shell.arrive(arrival(SettingsEvent::HarnessChosen(
        "codex".into(),
        Some(chosen.into()),
        Ok(Look::Registered {
            path: PathBuf::from(chosen),
        }),
    )));
    assert_eq!(sent(&mut shell.ctx), vec![Sent::Catalog]);
}

#[test]
fn a_refused_codex_file_shows_its_reason_and_x_forgets_the_chosen_file() {
    use crate::harnesses::finding::Found as Look;
    let missing = codex_missing();
    let mut shell = on(&missing, "codex");
    shell.key(Key::Char('x'));
    assert_eq!(sent(&mut shell.ctx), Vec::<Sent>::new());
    shell.key(Key::Char('w'));
    sent(&mut shell.ctx);
    shell.arrive(arrival(SettingsEvent::HarnessFound(
        "codex".into(),
        Ok(Look::Refused {
            path: PathBuf::from("/Users/someone/gone/codex"),
            code: "harness_path_not_found".into(),
        }),
    )));
    let shown = frame(&mut shell);
    assert!(
        !shown.contains("Where CrazyAgent looked for codex"),
        "{shown}"
    );
    assert!(
        shown.contains("CrazyAgent won't use /Users/someone/gone/codex. There's no file there."),
        "{shown}"
    );
    assert!(footer(&mut shell).contains("x Forget the chosen file"));
    shell.key(Key::Char('x'));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Choose("codex".into(), None)]
    );
    shell.arrive(arrival(SettingsEvent::HarnessChosen(
        "codex".into(),
        None,
        Ok(Look::Missing { looked: Vec::new() }),
    )));
    let shown = frame(&mut shell);
    assert!(
        shown.contains("CrazyAgent didn't find codex. Press f to choose the codex file."),
        "{shown}"
    );
    assert!(shell.ctx.message().is_none());
    assert!(!footer(&mut shell).contains("Forget"));
}

#[test]
fn the_file_choice_is_offered_only_for_a_missing_codex_or_claude_code() {
    let found = found();
    let mut shell = on(&found, "codex");
    let hints = footer(&mut shell);
    assert!(!hints.contains("Choose file"), "{hints}");
    shell.key(Key::Char('f'));
    shell.key(Key::Char('w'));
    assert_eq!(sent(&mut shell.ctx), Vec::<Sent>::new());
    assert!(!frame(&mut shell).contains("Choose the codex file"));
    for (code, copy) in PATH_COPY {
        assert_eq!(path_copy(code), copy);
    }
    assert_eq!(
        PATH_COPY
            .iter()
            .map(|(_, copy)| copy)
            .collect::<BTreeSet<_>>()
            .len(),
        PATH_COPY.len()
    );
    assert_eq!(file_name("claude-code"), "claude");
    assert_eq!(file_name("codex"), "codex");
}

const NEEDS_LINE: &str = "codex needs node, which is missing. Install Node.js from nodejs.org, or run brew install node in Terminal. Then try again.";
const RUNTIME_LINE: &str = "codex needs ca-missing-runtime, which is missing. Install it, or run setup again from Settings. Then try again.";
const NODE_FAILED: &str = "Node.js didn't download. Check the connection, then try again.";

/// The engine's read of an npm Codex the tool resolver finds but can't run. Inputs: the
/// program its first line asks for and the resolver's line. Output: the read.
fn codex_needing(program: &str, line: &str) -> HarnessVersions {
    HarnessVersions {
        id: "codex".into(),
        method: Method::Npm {
            package: "@openai/codex".into(),
        },
        installed: None,
        latest: Latest::NotChecked,
        command: Some("/opt/homebrew/bin/npm install -g @openai/codex@latest".into()),
        needs: Some(HarnessNeeds {
            program: Some(program.into()),
            line: line.into(),
        }),
        ..newer()
    }
}

fn codex_runs(installed: Option<&str>) -> HarnessVersions {
    HarnessVersions {
        id: "codex".into(),
        installed: installed.map(str::to_owned),
        latest: Latest::NotChecked,
        ..newer()
    }
}

fn setup_node(state: DependencyState) -> Arrival {
    arrival(SettingsEvent::AppSetup(AppSetupEvent::Status(Ok(
        SetupStatus {
            dependencies: vec![DependencyStatus {
                name: Dependency::Node,
                label: "Node.js".into(),
                state,
            }],
            pack: Vec::new(),
            running: false,
            notes: Vec::new(),
            version: 1,
        },
    ))))
}

fn table_row(text: &str, name: &str) -> String {
    text.lines()
        .find(|line| line.contains(&format!("> {name} ")))
        .unwrap_or_else(|| panic!("no selected {name} row:\n{text}"))
        .to_owned()
}

/// v241 (L7c open item, DEC-141): a found harness the tool resolver can't run reads Can't run
/// with the resolver's line, the first-run step's words, never Ready or a version line.
#[test]
fn a_found_harness_the_resolver_cant_run_reads_cant_run_with_its_line_never_ready() {
    let found = found();
    let mut shell = on(&found, "codex");
    deliver(&mut shell, codex_needing("node", NEEDS_LINE));
    let text = frame(&mut shell);
    let row = table_row(&text, "Codex");
    assert!(row.contains("Can't run"), "{row}");
    assert!(!row.contains("Ready"), "{row}");
    assert!(
        text.contains("codex needs node, which is missing."),
        "{text}"
    );
    assert!(!text.contains("Version unknown."), "{text}");
    let help = footer(&mut shell);
    assert!(help.contains("r Try again"), "{help}");
    assert!(!help.contains("Check for updates"), "{help}");
}

/// v241: r on a harness that can't run reads it again; a clean read brings back Ready and
/// its version line, and a program other than Node offers no Node.js install.
#[test]
fn r_on_a_harness_that_cant_run_reads_it_again_and_a_clean_read_shows_ready() {
    let found = found();
    let mut shell = on(&found, "codex");
    deliver(
        &mut shell,
        codex_needing("ca-missing-runtime", RUNTIME_LINE),
    );
    assert_eq!(
        sent(&mut shell.ctx),
        vec![],
        "a need other than Node reads no setup row"
    );
    assert!(!footer(&mut shell).contains("Install Node.js"));
    shell.key(Key::Char('r'));
    let calls = sent(&mut shell.ctx);
    assert!(calls.contains(&Sent::Catalog), "{calls:?}");
    assert!(
        calls.contains(&Sent::Versions("codex".into(), false)),
        "Try again reads the harness again: {calls:?}"
    );
    deliver(&mut shell, codex_runs(Some("0.167.1")));
    let text = frame(&mut shell);
    let row = table_row(&text, "Codex");
    assert!(row.contains("Ready"), "{row}");
    assert!(text.contains("Version 0.167.1."), "{text}");
    assert!(!text.contains("Can't run"), "{text}");
}

/// v241 (DEC-157): a harness that needs Node reads setup's Node row once; one n installs
/// Node.js through setup's one-tool install, the page says it installs, and once Node settles
/// the harnesses that needed it are read again. A failed install says why and offers n again.
#[test]
fn one_n_installs_node_for_the_harnesses_that_need_it_and_they_read_again_once_it_settles() {
    let found = found();
    let mut shell = on(&found, "codex");
    deliver(&mut shell, codex_needing("node", NEEDS_LINE));
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Setup],
        "a harness that needs Node reads setup's Node row"
    );
    shell.arrive(setup_node(DependencyState::Missing));
    let help = footer(&mut shell);
    assert_eq!(help.matches("n Install Node.js").count(), 1, "{help}");
    shell.key(Key::Char('n'));
    assert_eq!(sent(&mut shell.ctx), vec![Sent::InstallNode]);
    shell.arrive(arrival(SettingsEvent::AppSetup(AppSetupEvent::Installed(
        Ok(()),
    ))));
    assert!(frame(&mut shell).contains("Installing Node.js."));
    assert!(!footer(&mut shell).contains("Install Node.js"));
    assert_eq!(sent(&mut shell.ctx), vec![Sent::Setup]);
    shell.arrive(setup_node(DependencyState::Installing));
    shell.refresh();
    assert_eq!(
        sent(&mut shell.ctx),
        vec![Sent::Setup],
        "the page reads setup's Node row again on each refresh while Node installs"
    );
    shell.arrive(setup_node(DependencyState::Failed {
        reason: NODE_FAILED.into(),
    }));
    assert!(frame(&mut shell).contains(NODE_FAILED));
    assert!(footer(&mut shell).contains("n Install Node.js"));
    shell.key(Key::Char('n'));
    assert_eq!(sent(&mut shell.ctx), vec![Sent::InstallNode]);
    shell.arrive(arrival(SettingsEvent::AppSetup(AppSetupEvent::Installed(
        Ok(()),
    ))));
    shell.refresh();
    sent(&mut shell.ctx);
    shell.arrive(setup_node(DependencyState::Installed {
        version: Version::parse("26.13.1").unwrap(),
    }));
    assert!(
        sent(&mut shell.ctx).contains(&Sent::Versions("codex".into(), false)),
        "the harness that needed Node is read again once Node is installed"
    );
    deliver(&mut shell, codex_runs(Some("0.167.1")));
    let text = frame(&mut shell);
    assert!(!text.contains("Installing Node.js."), "{text}");
    assert!(table_row(&text, "Codex").contains("Ready"), "{text}");
}

/// v241: with harness updates off a harness that can't run still reads Can't run, and a
/// harness that runs shows no version line, since Off starts nothing.
#[test]
fn with_updates_off_a_harness_that_cant_run_still_reads_cant_run_and_no_version_line() {
    let found = found();
    let mut shell = on(&found, "codex");
    shell.arrive(arrival(SettingsEvent::HarnessUpdateSettings(Ok(
        settings_with(UpdateMode::Off, CheckWhen::OnButton),
    ))));
    sent(&mut shell.ctx);
    deliver(&mut shell, codex_needing("node", NEEDS_LINE));
    let text = frame(&mut shell);
    assert!(table_row(&text, "Codex").contains("Can't run"), "{text}");
    assert!(
        text.contains("codex needs node, which is missing."),
        "{text}"
    );
    deliver(&mut shell, codex_runs(None));
    let text = frame(&mut shell);
    assert!(table_row(&text, "Codex").contains("Ready"), "{text}");
    assert!(!text.contains("Version unknown."), "{text}");
}

/// v241 HR-L1: a harness the engine lists as Can't run reads Can't run with the engine's line
/// and Try again before its versions arrive, and once a Node.js install the page waited on
/// settles, the page reads the catalog again too, so its status and the Home count follow.
#[test]
fn a_harness_the_catalog_lists_cant_run_shows_its_line_and_node_settling_reads_the_catalog_again() {
    let mut found = found();
    let codex = found
        .entries
        .iter_mut()
        .find(|entry| entry.id == "codex")
        .unwrap();
    codex.status = HarnessStatus::CantRun {
        reason: NEEDS_LINE.into(),
    };
    codex.status_label = codex.status.label();
    let mut shell = on(&found, "codex");
    let text = frame(&mut shell);
    let row = table_row(&text, "Codex");
    assert!(row.contains("Can't run"), "{row}");
    assert!(!row.contains("Ready"), "{row}");
    assert!(
        text.contains("codex needs node, which is missing."),
        "{text}"
    );
    assert!(footer(&mut shell).contains("r Try again"));

    deliver(&mut shell, codex_needing("node", NEEDS_LINE));
    sent(&mut shell.ctx);
    shell.arrive(setup_node(DependencyState::Missing));
    shell.key(Key::Char('n'));
    assert_eq!(sent(&mut shell.ctx), vec![Sent::InstallNode]);
    shell.arrive(arrival(SettingsEvent::AppSetup(AppSetupEvent::Installed(
        Ok(()),
    ))));
    sent(&mut shell.ctx);
    shell.arrive(setup_node(DependencyState::Installed {
        version: Version::parse("26.13.1").unwrap(),
    }));
    let calls = sent(&mut shell.ctx);
    assert!(
        calls.contains(&Sent::Catalog),
        "once Node settles the catalog is read again: {calls:?}"
    );
    assert!(
        calls.contains(&Sent::Versions("codex".into(), false)),
        "{calls:?}"
    );
    deliver(&mut shell, codex_runs(Some("0.167.1")));
    let row = table_row(&frame(&mut shell), "Codex");
    assert!(
        row.contains("Ready"),
        "a fresh read that runs wins over the catalog's older word: {row}"
    );
    assert!(!row.contains("Can't run"), "{row}");
}
