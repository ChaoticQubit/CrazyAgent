use super::*;
use crate::harnesses::update::{HarnessNeeds, LastUpdate, Latest, Method};
use crate::harnesses::{Search, catalog};
use crate::setup::{Dependency, DependencyStatus, SetupStatus, Version};
use crate::tui::client::OwnerEvent;
use crate::tui::client::app_setup::{AppSetupEvent, AppSetupJob};
use crate::tui::client::settings::SettingsJob;
use crate::tui::screens::settings_accounts::tests::{bar_rows, ctx, render};
use crate::tui::screens::settings_harnesses::update_copy;
use crate::tui::seam::Request;
use crate::tui::shell::Shell;
use std::os::unix::fs::PermissionsExt;

const LINE: &str = "/opt/homebrew/bin/npm install -g @openai/codex";
const NEEDS_NODE: &str = "harness_install_needs_node";
const NEEDS_LINE: &str = "codex needs node, which is missing. Install Node.js from nodejs.org, or run brew install node in Terminal. Then try again.";
const NODE_FAILED: &str = "Node.js didn't download. Check the connection, then try again.";

#[derive(Debug, PartialEq, Eq)]
enum Sent {
    Catalog,
    Queue,
    Setup,
    Versions(String),
    InstallMany(Vec<String>),
    InstallNode,
    Other,
}

fn sent(shell: &mut Shell) -> Vec<Sent> {
    let mut all = Vec::new();
    while let Some(request) = shell.ctx.next_request() {
        if let Request::Settings(job) = request {
            all.push(match *job {
                SettingsJob::HarnessList => Sent::Catalog,
                SettingsJob::HarnessQueued => Sent::Queue,
                SettingsJob::HarnessVersions { id, look: false } => Sent::Versions(id),
                SettingsJob::HarnessInstallMany { ids } => Sent::InstallMany(ids),
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

fn reads() -> Vec<Sent> {
    vec![
        Sent::Catalog,
        Sent::Queue,
        Sent::Setup,
        Sent::Versions("codex".into()),
        Sent::Versions("claude-code".into()),
    ]
}

struct Entries {
    _root: tempfile::TempDir,
    all: Vec<HarnessEntry>,
}

fn entries(found: &[&str]) -> Entries {
    let root = tempfile::Builder::new()
        .prefix("ca-hs-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    for name in found {
        let path = root.path().join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let all = catalog(&Search::new(
        vec![root.path().to_path_buf()],
        None,
        Vec::new(),
    ));
    Entries { _root: root, all }
}

fn entry<'a>(entries: &'a Entries, id: &str) -> &'a HarnessEntry {
    entries.all.iter().find(|entry| entry.id == id).unwrap()
}

fn versions(id: &str, install: Option<&str>, refusal: Option<&str>) -> HarnessVersions {
    HarnessVersions {
        id: id.into(),
        method: Method::Unknown,
        installed: None,
        latest: Latest::NotChecked,
        command: None,
        install: install.map(str::to_owned),
        refusal: refusal.map(str::to_owned),
        guide: Some(format!("https://example.test/{id}")),
        running: false,
        last: None,
        needs: None,
    }
}

fn needs_node(id: &str) -> HarnessVersions {
    HarnessVersions {
        needs: Some(HarnessNeeds {
            program: Some("node".into()),
            line: NEEDS_LINE.into(),
        }),
        ..versions(id, None, None)
    }
}

fn node(state: DependencyState) -> Arrival {
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

fn ended(ok: bool, code: Option<&str>, to: Option<&str>) -> LastUpdate {
    LastUpdate {
        ok,
        code: code.map(str::to_owned),
        output: String::new(),
        from: None,
        to: to.map(str::to_owned),
        ended_ms: 1,
    }
}

fn arrival(event: SettingsEvent) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(event))))
}

fn step(width: u16, height: u16) -> Shell {
    Shell::with(ctx(width, height), Box::new(HarnessSetup::new()))
}

fn answer(
    shell: &mut Shell,
    catalog: &Entries,
    codex: HarnessVersions,
    claude: HarnessVersions,
    queued: &[&str],
) {
    shell.arrive(arrival(SettingsEvent::Harnesses(Ok(catalog.all.clone()))));
    shell.arrive(arrival(SettingsEvent::HarnessQueue(Ok(queued
        .iter()
        .map(|id| (*id).to_owned())
        .collect()))));
    shell.arrive(arrival(SettingsEvent::HarnessVersions(
        "codex".into(),
        Ok(codex),
    )));
    shell.arrive(arrival(SettingsEvent::HarnessVersions(
        "claude-code".into(),
        Ok(claude),
    )));
    shell.arrive(node(DependencyState::Missing));
}

fn read_step(width: u16, height: u16) -> (Shell, Entries) {
    let mut shell = step(width, height);
    assert_eq!(sent(&mut shell), reads());
    let none = entries(&[]);
    answer(
        &mut shell,
        &none,
        versions("codex", Some(LINE), Some("harness_update_not_installed")),
        versions("claude-code", None, Some(NEEDS_NODE)),
        &[],
    );
    (shell, none)
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
fn the_harness_step_offers_rcli_codex_and_claude_code_with_only_rcli_picked_and_fits() {
    for (width, height) in [(80, 24), (120, 36)] {
        let mut shell = step(width, height);
        assert_eq!(shell.place(), Some(Place::HarnessSetup));
        assert_eq!(sent(&mut shell), reads());
        let rows = render(&mut shell, width, height);
        assert!(shows(&rows, "Harnesses"), "{rows:#?}");
        assert!(shows(&rows, TITLE), "{rows:#?}");
        assert!(words(&rows).contains(INTRO), "{rows:#?}");
        assert!(shows(&rows, "[x] RCLI"), "{rows:#?}");
        assert!(shows(&rows, BUNDLED), "{rows:#?}");
        assert!(shows(&rows, "[ ] Codex"), "{rows:#?}");
        assert!(shows(&rows, "[ ] Claude Code"), "{rows:#?}");
        assert!(shows(&rows, "[> Continue ]"), "{rows:#?}");
        let help = bar(&rows);
        assert!(help.contains("Enter Continue"), "{help}");
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
fn each_row_says_what_installing_runs_or_why_it_cannot_in_the_desktop_words() {
    let none = entries(&[]);
    let found = entries(&["codex", "claude"]);
    let codex = || versions("codex", Some(LINE), Some("harness_update_not_installed"));
    assert_eq!(harness_row("rcli", None, None, &[]), Row::Bundled);
    assert_eq!(
        harness_row("codex", Some(entry(&none, "codex")), None, &[]),
        Row::Reading
    );
    assert_eq!(
        harness_row("codex", Some(entry(&none, "codex")), Some(&codex()), &[]),
        Row::Offer { line: LINE.into() }
    );
    assert_eq!(
        harness_row(
            "codex",
            Some(entry(&none, "codex")),
            Some(&versions("codex", None, Some(NEEDS_NODE))),
            &[]
        ),
        Row::Refused {
            reason: update_copy(NEEDS_NODE).into(),
            guide: Some("https://example.test/codex".into())
        }
    );
    assert_eq!(
        harness_row(
            "codex",
            Some(entry(&none, "codex")),
            Some(&codex()),
            &["codex".into()]
        ),
        Row::Waiting
    );
    let running = HarnessVersions {
        running: true,
        ..codex()
    };
    assert_eq!(
        harness_row("codex", Some(entry(&none, "codex")), Some(&running), &[]),
        Row::Installing
    );
    let failed = HarnessVersions {
        last: Some(ended(false, Some("harness_install_failed"), None)),
        ..codex()
    };
    assert_eq!(
        harness_row("codex", Some(entry(&none, "codex")), Some(&failed), &[]),
        Row::Failed {
            reason: update_copy("harness_install_failed").into()
        }
    );
    let done = HarnessVersions {
        install: None,
        refusal: None,
        last: Some(ended(true, None, Some("0.52.0"))),
        ..codex()
    };
    assert_eq!(
        harness_row("codex", Some(entry(&found, "codex")), Some(&done), &[]),
        Row::Done {
            version: Some("0.52.0".into())
        }
    );
    let installed = HarnessVersions {
        installed: Some("2.1.0".into()),
        install: None,
        refusal: None,
        ..versions("claude-code", None, None)
    };
    assert_eq!(
        harness_row(
            "claude-code",
            Some(entry(&found, "claude-code")),
            Some(&installed),
            &[]
        ),
        Row::Installed {
            version: Some("2.1.0".into())
        }
    );
    assert!(Row::Offer { line: LINE.into() }.tickable());
    for row in [
        Row::Bundled,
        Row::Reading,
        Row::Waiting,
        Row::Installing,
        Row::Done { version: None },
        Row::Failed { reason: "x".into() },
    ] {
        assert!(!row.tickable(), "{row:?}");
    }
}

#[test]
fn needs_node_and_a_bad_list_have_their_own_plain_sentences() {
    let needs = update_copy(NEEDS_NODE);
    assert!(needs.contains("Node.js"), "{needs}");
    assert_ne!(needs, update_copy("harness_install_no_tool"));
    assert_ne!(needs, update_copy("harness_update_outcome_unknown"));
    assert_ne!(
        update_copy("harness_install_list_invalid"),
        update_copy("harness_update_outcome_unknown")
    );
}

#[test]
fn progress_counts_the_sent_picks_in_the_desktop_words() {
    assert_eq!(
        progress_line(Progress {
            done: 1,
            total: 2,
            failed: 0,
            running: true
        }),
        "Installing: 1 of 2 done. You can continue; the installs keep going."
    );
    assert_eq!(
        progress_line(Progress {
            done: 2,
            total: 2,
            failed: 1,
            running: false
        }),
        "Installs finished: 2 of 2 done, 1 failed. Each failure shows its reason above."
    );
    assert_eq!(
        progress_line(Progress {
            done: 2,
            total: 2,
            failed: 0,
            running: false
        }),
        "Installs finished: 2 of 2 done."
    );
}

#[test]
fn space_picks_an_offer_and_enter_installs_the_picks_through_the_engine_queue() {
    let (mut shell, _none) = read_step(120, 36);
    let rows = render(&mut shell, 120, 36);
    assert!(
        shows(&rows, &format!("{INSTALLS_WITH} {LINE}")),
        "{rows:#?}"
    );
    assert!(words(&rows).contains(update_copy(NEEDS_NODE)), "{rows:#?}");
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, "[x] Codex"), "{rows:#?}");
    assert!(
        shows(&rows, "[> Install picked harnesses ]   [ Skip for now ]"),
        "{rows:#?}"
    );
    let help = bar(&rows);
    assert!(help.contains("Enter Install picked harnesses"), "{help}");
    shell.key(Key::Enter);
    assert_eq!(
        sent(&mut shell),
        vec![Sent::InstallMany(vec!["codex".into()])]
    );
    shell.arrive(arrival(SettingsEvent::HarnessesQueued(
        vec!["codex".into()],
        Ok(()),
    )));
    assert_eq!(sent(&mut shell), reads());
    let none = entries(&[]);
    let running = HarnessVersions {
        running: true,
        ..versions("codex", Some(LINE), None)
    };
    answer(
        &mut shell,
        &none,
        running,
        versions("claude-code", None, Some(NEEDS_NODE)),
        &[],
    );
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, INSTALLING), "{rows:#?}");
    assert!(
        shows(
            &rows,
            "Installing: 0 of 1 done. You can continue; the installs keep going."
        ),
        "{rows:#?}"
    );
    assert!(shows(&rows, "[> Continue ]"), "{rows:#?}");
    shell.refresh();
    assert_eq!(sent(&mut shell), reads());
    let found = entries(&["codex"]);
    let done = HarnessVersions {
        install: None,
        refusal: None,
        last: Some(ended(true, None, Some("0.52.0"))),
        ..versions("codex", None, None)
    };
    answer(
        &mut shell,
        &found,
        done,
        versions("claude-code", None, Some(NEEDS_NODE)),
        &[],
    );
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, "Installs finished: 1 of 1 done."), "{rows:#?}");
    shell.refresh();
    assert!(
        sent(&mut shell).is_empty(),
        "no reads once every pick ended"
    );
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::FirstRunAccounts));
}

#[test]
fn a_refused_row_cannot_be_picked_and_r_reads_it_again_with_its_guide_shown() {
    let (mut shell, _none) = read_step(120, 36);
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, "[ ] Claude Code"), "{rows:#?}");
    assert!(
        shows(&rows, &format!("{GUIDE}: https://example.test/claude-code")),
        "{rows:#?}"
    );
    let help = bar(&rows);
    assert!(help.contains("r Try again"), "{help}");
    assert!(!help.contains("Space"), "{help}");
    shell.key(Key::Char('r'));
    assert_eq!(sent(&mut shell), reads());
}

#[test]
fn a_failed_install_offers_try_again_which_queues_only_that_harness() {
    let mut shell = step(120, 36);
    sent(&mut shell);
    let none = entries(&[]);
    let failed = HarnessVersions {
        last: Some(ended(false, Some("harness_install_failed"), None)),
        ..versions("codex", Some(LINE), Some("harness_update_not_installed"))
    };
    answer(
        &mut shell,
        &none,
        failed,
        versions("claude-code", None, Some(NEEDS_NODE)),
        &[],
    );
    shell.key(Key::Down);
    let rows = render(&mut shell, 120, 36);
    assert!(
        shows(&rows, update_copy("harness_install_failed")),
        "{rows:#?}"
    );
    assert!(bar(&rows).contains("r Try again"));
    shell.key(Key::Char('r'));
    assert_eq!(
        sent(&mut shell),
        vec![Sent::InstallMany(vec!["codex".into()])]
    );
}

#[test]
fn skip_and_continue_go_to_the_accounts_step_without_installing() {
    let (mut shell, _none) = read_step(80, 24);
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    shell.key(Key::Char('c'));
    assert_eq!(shell.place(), Some(Place::FirstRunAccounts));
    assert!(
        !sent(&mut shell)
            .iter()
            .any(|job| matches!(job, Sent::InstallMany(_)))
    );
    let (mut shell, _none) = read_step(80, 24);
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::FirstRunAccounts));
    assert!(
        !sent(&mut shell)
            .iter()
            .any(|job| matches!(job, Sent::InstallMany(_)))
    );
}

#[test]
fn install_from_the_app_off_leaves_nothing_to_pick_and_enter_continues() {
    let mut shell = step(80, 24);
    sent(&mut shell);
    let none = entries(&[]);
    answer(
        &mut shell,
        &none,
        versions("codex", None, Some("harness_install_off")),
        versions("claude-code", None, Some("harness_install_off")),
        &[],
    );
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "[ ] Codex"), "{rows:#?}");
    assert!(
        shows(&rows, update_copy("harness_install_off")),
        "{rows:#?}"
    );
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::FirstRunAccounts));
}

#[test]
fn the_step_reads_again_only_while_an_install_waits_or_runs_and_never_twice_at_once() {
    let mut shell = step(80, 24);
    assert_eq!(sent(&mut shell), reads());
    shell.refresh();
    assert!(sent(&mut shell).is_empty(), "the first reads are still out");
    let none = entries(&[]);
    answer(
        &mut shell,
        &none,
        versions("codex", Some(LINE), Some("harness_update_not_installed")),
        versions("claude-code", None, Some(NEEDS_NODE)),
        &["codex"],
    );
    assert!(shows(&render(&mut shell, 80, 24), WAITING));
    shell.refresh();
    assert_eq!(sent(&mut shell), reads());
    answer(
        &mut shell,
        &none,
        versions("codex", Some(LINE), Some("harness_update_not_installed")),
        versions("claude-code", None, Some(NEEDS_NODE)),
        &[],
    );
    shell.refresh();
    assert!(sent(&mut shell).is_empty(), "nothing waits or runs");
}

#[test]
fn the_worst_case_rows_keep_the_problem_and_buttons_in_an_80_by_24_frame() {
    let mut shell = step(80, 24);
    sent(&mut shell);
    let none = entries(&[]);
    answer(
        &mut shell,
        &none,
        versions("codex", Some(LINE), Some("harness_update_not_installed")),
        versions("claude-code", None, Some(NEEDS_NODE)),
        &[],
    );
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    sent(&mut shell);
    shell.arrive(arrival(SettingsEvent::HarnessesQueued(
        vec!["codex".into()],
        Err("harness_update_busy".into()),
    )));
    let rows = render(&mut shell, 80, 24);
    assert!(
        shows(&rows, update_copy("harness_update_busy")),
        "{rows:#?}"
    );
    assert!(shows(&rows, "[> Install picked harnesses ]"), "{rows:#?}");
    assert!(
        shows(&rows, &format!("{GUIDE}: https://example.test/claude-code")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().all(|row| row.chars().count() <= 80),
        "{rows:#?}"
    );
}

#[test]
fn a_found_codex_the_resolver_cannot_run_reads_needs_node_and_never_installed() {
    let found = entries(&["codex"]);
    let blocked = Row::Blocked {
        line: NEEDS_LINE.into(),
    };
    assert_eq!(
        harness_row(
            "codex",
            Some(entry(&found, "codex")),
            Some(&needs_node("codex")),
            &[]
        ),
        blocked
    );
    let fresh = HarnessVersions {
        last: Some(ended(true, None, Some("0.52.0"))),
        ..needs_node("codex")
    };
    assert_eq!(
        harness_row("codex", Some(entry(&found, "codex")), Some(&fresh), &[]),
        blocked,
        "a fresh install that cannot run is not Installed"
    );
    assert_eq!(blocked.badge(), "Can't run");
    assert_eq!(blocked.line(), NEEDS_LINE);
    assert!(blocked.retry() && blocked.ended() && !blocked.tickable());
}

#[test]
fn the_node_offer_follows_the_node_setup_row_while_a_harness_needs_node() {
    let version = || Version::parse("26.13.1").unwrap();
    assert_eq!(
        node_offer(false, Some(&DependencyState::Missing)),
        NodeOffer::None
    );
    assert_eq!(
        node_offer(true, Some(&DependencyState::Missing)),
        NodeOffer::Offer
    );
    assert_eq!(node_offer(true, None), NodeOffer::Offer);
    assert_eq!(
        node_offer(true, Some(&DependencyState::Installing)),
        NodeOffer::Installing
    );
    assert_eq!(
        node_offer(
            true,
            Some(&DependencyState::Failed {
                reason: NODE_FAILED.into()
            })
        ),
        NodeOffer::Failed {
            reason: NODE_FAILED.into()
        }
    );
    for settled in [
        DependencyState::Checking,
        DependencyState::Found { version: version() },
        DependencyState::Installed { version: version() },
    ] {
        assert_eq!(
            node_offer(true, Some(&settled)),
            NodeOffer::None,
            "{settled:?}"
        );
    }
}

#[test]
fn n_installs_node_for_a_harness_that_needs_it_and_shows_its_progress_and_failure() {
    let mut shell = step(120, 36);
    assert_eq!(sent(&mut shell), reads());
    let found = entries(&["codex"]);
    let claude = || versions("claude-code", None, Some(NEEDS_NODE));
    answer(&mut shell, &found, needs_node("codex"), claude(), &[]);
    shell.key(Key::Down);
    let rows = render(&mut shell, 120, 36);
    assert!(words(&rows).contains(NEEDS_LINE), "{rows:#?}");
    assert!(shows(&rows, "Can't run"), "{rows:#?}");
    assert!(!shows(&rows, INSTALLED), "{rows:#?}");
    let help = bar(&rows);
    assert!(help.contains(&format!("n {INSTALL_NODE}")), "{help}");
    assert!(help.contains("r Try again"), "{help}");
    shell.key(Key::Char('n'));
    assert_eq!(sent(&mut shell), vec![Sent::InstallNode]);
    shell.arrive(arrival(SettingsEvent::AppSetup(AppSetupEvent::Installed(
        Ok(()),
    ))));
    assert_eq!(sent(&mut shell), reads());
    answer(&mut shell, &found, needs_node("codex"), claude(), &[]);
    shell.arrive(node(DependencyState::Installing));
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, INSTALLING_NODE), "{rows:#?}");
    assert!(!bar(&rows).contains(INSTALL_NODE), "{rows:#?}");
    shell.refresh();
    assert_eq!(sent(&mut shell), reads(), "reads again while Node installs");
    answer(&mut shell, &found, needs_node("codex"), claude(), &[]);
    shell.arrive(node(DependencyState::Failed {
        reason: NODE_FAILED.into(),
    }));
    let rows = render(&mut shell, 120, 36);
    assert!(shows(&rows, NODE_FAILED), "{rows:#?}");
    assert!(
        bar(&rows).contains(&format!("n {INSTALL_NODE}")),
        "{rows:#?}"
    );
    shell.refresh();
    assert!(
        sent(&mut shell).is_empty(),
        "a failed Node install waits for n"
    );
}

#[test]
fn n_does_nothing_when_no_offered_harness_needs_node() {
    let mut shell = step(120, 36);
    sent(&mut shell);
    let none = entries(&[]);
    answer(
        &mut shell,
        &none,
        versions("codex", Some(LINE), Some("harness_update_not_installed")),
        versions(
            "claude-code",
            Some(LINE),
            Some("harness_update_not_installed"),
        ),
        &[],
    );
    assert!(!bar(&render(&mut shell, 120, 36)).contains(INSTALL_NODE));
    shell.key(Key::Char('n'));
    assert!(sent(&mut shell).is_empty());
}

#[test]
fn a_sent_pick_that_ends_unable_to_run_counts_as_ended_and_failed() {
    let found = entries(&["codex"]);
    let mut setup = HarnessSetup::new();
    setup.entries = found.all.clone();
    setup.picks = vec!["codex"];
    setup.versions.insert("codex", Ok(needs_node("codex")));
    assert_eq!(
        setup.progress(),
        Progress {
            done: 1,
            total: 1,
            failed: 1,
            running: false
        }
    );
}
