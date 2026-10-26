use super::*;
use crate::github::GitHubMethod;
use crate::setup::{Dependency, DependencyStatus, PackGroup, Version};
use crate::tui::client::OwnerEvent;
use crate::tui::glyphs::Status;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::{Shell, targets};

const GIB: u64 = 1024 * 1024 * 1024;

fn event(event: AppSetupEvent) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::AppSetup(event),
    ))))
}

fn jobs(shell: &mut Shell) -> Vec<AppSetupJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => match *job {
                SettingsJob::AppSetup(job) => Some(job),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn tool(name: Dependency, state: DependencyState) -> DependencyStatus {
    DependencyStatus {
        name,
        label: name.label().into(),
        state,
    }
}

fn entry(id: &str, kind: PackKind, group: PackGroup, on: bool, state: PackState) -> PackEntry {
    PackEntry {
        id: id.into(),
        kind,
        group,
        source: format!("https://github.com/example/{id}"),
        license: "MIT".into(),
        on_by_default: on,
        state,
    }
}

fn first_run_status() -> SetupStatus {
    SetupStatus {
        dependencies: vec![
            tool(
                Dependency::Tmux,
                DependencyState::Found {
                    version: version("3.5a"),
                },
            ),
            tool(Dependency::Uv, DependencyState::Missing),
            tool(
                Dependency::Graphify,
                DependencyState::Failed {
                    reason: "setup_uv_missing".into(),
                },
            ),
        ],
        pack: vec![
            entry(
                "graphify",
                PackKind::Skill,
                PackGroup::Graph,
                true,
                PackState::Available,
            ),
            entry(
                "superpowers",
                PackKind::Skill,
                PackGroup::Workflow,
                true,
                PackState::Available,
            ),
            entry(
                "caveman",
                PackKind::Skill,
                PackGroup::ReplyStyle,
                true,
                PackState::Available,
            ),
            entry(
                "context-mode",
                PackKind::McpServer,
                PackGroup::Context,
                true,
                PackState::Available,
            ),
            entry(
                "headroom",
                PackKind::McpServer,
                PackGroup::Context,
                true,
                PackState::Available,
            ),
            entry(
                "serena",
                PackKind::McpServer,
                PackGroup::Optional,
                false,
                PackState::Available,
            ),
        ],
        running: false,
    }
}

fn installed_status() -> SetupStatus {
    let mut status = first_run_status();
    for tool in &mut status.dependencies {
        tool.state = DependencyState::Installed {
            version: version("0.9.9"),
        };
    }
    for entry in &mut status.pack {
        entry.state = match entry.id.as_str() {
            "graphify" | "superpowers" | "context-mode" | "headroom" => PackState::On,
            "caveman" => PackState::Off,
            _ => PackState::Available,
        };
    }
    status
}

fn signed_in() -> GitHubStatus {
    GitHubStatus {
        signed_in: true,
        login: Some("octocat".into()),
        method: GitHubMethod::Device,
        waiting: false,
        problem: None,
    }
}

fn home() -> ProjectsHome {
    ProjectsHome {
        path: Some("/Users/owner/Projects".into()),
        watching: true,
    }
}

fn first_run(width: u16, height: u16) -> Shell {
    let mut shell = Shell::with(ctx(width, height), Box::new(AppSetup::new(true)));
    assert!(matches!(jobs(&mut shell).as_slice(), [AppSetupJob::Status]));
    shell.arrive(event(AppSetupEvent::Status(Ok(first_run_status()))));
    shell
}

fn settings(width: u16, height: u16) -> Shell {
    let mut shell = Shell::with(ctx(width, height), Box::new(AppSetup::new(false)));
    assert!(matches!(
        jobs(&mut shell).as_slice(),
        [AppSetupJob::Status, AppSetupJob::Home, AppSetupJob::GitHub]
    ));
    shell.arrive(event(AppSetupEvent::Status(Ok(installed_status()))));
    shell.arrive(event(AppSetupEvent::Home(Ok(home()))));
    shell.arrive(event(AppSetupEvent::GitHub(Ok(signed_in()))));
    shell
}

fn find<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter()
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row has {needle:?}:\n{}", rows.join("\n")))
}

fn lead(shell: &Shell) -> String {
    shell
        .ctx
        .message()
        .map(|message| message.lead.clone())
        .unwrap_or_default()
}

fn typed(shell: &mut Shell, text: &str) {
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
}

#[test]
fn setup_status_words_carry_a_glyph() {
    assert_eq!(Status::of("Found 3.5a"), Some(Status::Ok));
    assert_eq!(Status::of("Installed 0.9.9"), Some(Status::Ok));
    assert_eq!(Status::of("Installing"), Some(Status::Active));
    assert_eq!(Status::of("Checking"), Some(Status::Active));
    assert_eq!(Status::of("Founder"), None);
}

#[test]
fn sizes_read_in_plain_units() {
    assert_eq!(size(512), "512 B");
    assert_eq!(size(5 * 1024 * 1024), "5 MB");
    assert_eq!(size(3 * GIB), "3.0 GB");
}

#[test]
fn first_run_checks_the_computer_first() {
    let mut shell = Shell::with(ctx(80, 24), Box::new(AppSetup::new(true)));
    assert!(matches!(jobs(&mut shell).as_slice(), [AppSetupJob::Status]));
    let rows = render(&mut shell, 80, 24);
    assert!(find(&rows, "Setup").contains("Setup"));
    find(&rows, "Checking this computer");
    assert_eq!(shell.place(), Some(Place::Setup { first_run: true }));
}

#[test]
fn tools_show_found_missing_and_failed_in_plain_words() {
    for (width, height) in [(80, 24), (120, 36)] {
        let mut shell = first_run(width, height);
        let rows = render(&mut shell, width, height);
        assert!(find(&rows, "tmux").contains("Found 3.5a"));
        assert!(find(&rows, &format!("{:<14}", "uv")).contains("Not installed"));
        assert!(find(&rows, "Graphify command").contains("Failed"));
        find(&rows, "uv is needed to install Graphify");
        assert!(rows.iter().all(|row| !row.contains("setup_")));
        assert!(
            rows.iter()
                .all(|row| row.chars().count() <= usize::from(width))
        );
    }
}

#[test]
fn ticks_follow_the_pack_and_ticked_mcp_servers_are_listed_for_approval() {
    let mut shell = first_run(120, 36);
    let rows = render(&mut shell, 120, 36);
    assert!(find(&rows, "[x] Graphify skill for agents").contains("Skill"));
    assert!(find(&rows, "[x] context-mode").contains("MCP server"));
    assert!(find(&rows, "[ ] serena").contains("Not installed"));
    assert!(find(&rows, "Skills and plugins").contains("5 of 6 ticked"));
    find(&rows, "approves them for your agents");
    find(&rows, "context-mode, headroom.");
    assert!(
        rows.iter()
            .all(|row| !(row.contains("headroom") && row.contains("serena")))
    );
    assert!(
        render(&mut shell, 120, 36)
            .last()
            .unwrap()
            .contains("Enter Install and continue")
    );
}

#[test]
fn space_unticks_any_entry_including_graphify_and_unticked_entries_are_not_sent() {
    let mut shell = first_run(120, 36);
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    let rows = render(&mut shell, 120, 36);
    find(&rows, "[ ] superpowers");
    assert!(find(&rows, "Skills and plugins").contains("4 of 6 ticked"));
    shell.key(Key::Up);
    shell.key(Key::Char(' '));
    let rows = render(&mut shell, 120, 36);
    find(&rows, "[ ] Graphify skill for agents");
    assert!(find(&rows, "Skills and plugins").contains("3 of 6 ticked"));
    shell.key(Key::Enter);
    let sent = jobs(&mut shell);
    let [AppSetupJob::Run { pack, .. }] = sent.as_slice() else {
        panic!("one setup run required")
    };
    assert_eq!(pack, &["caveman", "context-mode", "headroom"]);
}

#[test]
fn an_entry_already_installed_reads_installed_and_first_run_does_not_install_it_again() {
    let mut status = first_run_status();
    status.pack[0].state = PackState::On;
    status.pack[1].state = PackState::Off;
    let mut shell = Shell::with(ctx(120, 36), Box::new(AppSetup::new(true)));
    jobs(&mut shell);
    shell.arrive(event(AppSetupEvent::Status(Ok(status))));
    jobs(&mut shell);
    let rows = render(&mut shell, 120, 36);
    assert!(find(&rows, "Graphify skill for agents").contains("Installed"));
    assert!(find(&rows, "superpowers").contains("Turned off"));
    shell.key(Key::Char(' '));
    assert_eq!(
        lead(&shell),
        "Graphify skill for agents is already installed."
    );
    assert!(jobs(&mut shell).is_empty());
    shell.key(Key::Enter);
    let sent = jobs(&mut shell);
    let [AppSetupJob::Run { pack, .. }] = sent.as_slice() else {
        panic!("one setup run required")
    };
    assert_eq!(pack, &["caveman", "context-mode", "headroom"]);
}

#[test]
fn continuing_installs_the_ticked_entries_then_opens_home() {
    let mut shell = first_run(120, 36);
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    let sent = jobs(&mut shell);
    let [AppSetupJob::Run { pack, generation }] = sent.as_slice() else {
        panic!("one setup run required")
    };
    assert_eq!(pack, &["graphify", "caveman", "context-mode", "headroom"]);
    assert!(*generation > 0);

    shell.arrive(event(AppSetupEvent::Ran(Ok(()))));
    assert!(matches!(jobs(&mut shell).as_slice(), [AppSetupJob::Status]));
    let mut running = first_run_status();
    running.running = true;
    running.dependencies[1].state = DependencyState::Installing;
    shell.arrive(event(AppSetupEvent::Status(Ok(running))));
    assert!(shell.busy());
    assert!(find(&render(&mut shell, 120, 36), &format!("{:<14}", "uv")).contains("Installing"));
    assert_eq!(shell.place(), Some(Place::Setup { first_run: true }));

    let mut done = installed_status();
    done.pack[1].state = PackState::Available;
    done.pack[2].state = PackState::On;
    shell.arrive(event(AppSetupEvent::Status(Ok(done))));
    assert_eq!(shell.place(), Some(Place::Home));
}

#[test]
fn first_run_offers_to_continue_without_installing_and_runs_nothing() {
    for (width, height) in [(80, 24), (120, 36)] {
        let mut shell = first_run(width, height);
        let rows = render(&mut shell, width, height);
        let help = rows.last().unwrap();
        assert!(help.contains("Enter Install and continue"), "{help}");
        assert!(help.contains("c Continue without installing"), "{help}");
        shell.key(Key::Char('c'));
        assert_eq!(shell.place(), Some(Place::Home));
        let runs = jobs(&mut shell)
            .iter()
            .filter(|job| matches!(job, AppSetupJob::Run { .. }))
            .count();
        assert_eq!(runs, 0, "continuing without installing runs nothing");
    }
}

#[test]
fn a_failed_install_keeps_first_run_here_until_the_owner_continues() {
    let mut shell = first_run(120, 36);
    shell.key(Key::Enter);
    jobs(&mut shell);
    shell.arrive(event(AppSetupEvent::Ran(Ok(()))));
    jobs(&mut shell);
    let mut finished = installed_status();
    finished.dependencies[0].state = DependencyState::Failed {
        reason: "setup_homebrew_missing".into(),
    };
    finished.pack[2].state = PackState::Failed {
        reason: "pack_fetch_failed".into(),
    };
    shell.arrive(event(AppSetupEvent::Status(Ok(finished))));
    assert_eq!(lead(&shell), "Setup finished, but 2 items did not install.");
    assert_eq!(shell.place(), Some(Place::Setup { first_run: true }));
    let rows = render(&mut shell, 120, 36);
    find(&rows, "Homebrew is not installed. Install tmux yourself");
    assert!(rows.last().unwrap().contains("c Continue without it"));
    shell.key(Key::Down);
    shell.key(Key::Down);
    find(
        &render(&mut shell, 120, 36),
        "caveman: The source could not be downloaded.",
    );
    shell.key(Key::Char('c'));
    assert_eq!(shell.place(), Some(Place::Home));
}

#[test]
fn a_tool_still_being_checked_and_a_late_engine_answer_read_plainly() {
    let mut shell = first_run(80, 24);
    let mut status = first_run_status();
    status.dependencies[0].state = DependencyState::Checking;
    shell.arrive(event(AppSetupEvent::Status(Ok(status))));
    let rows = render(&mut shell, 80, 24);
    assert!(find(&rows, "tmux").contains("Checking"));
    shell.arrive(event(AppSetupEvent::Status(Err(
        "owner_outcome_unknown_reconcile_same_identity".into(),
    ))));
    assert_eq!(
        lead(&shell),
        "Failed to check setup. CrazyAgent did not answer in time. Try again."
    );
    assert_ne!(
        setup_plain("setup_outcome_unknown"),
        setup_plain("setup_code_nobody_uses")
    );
}

#[test]
fn a_refused_run_says_why_in_plain_words() {
    let mut shell = first_run(80, 24);
    shell.key(Key::Enter);
    jobs(&mut shell);
    shell.arrive(event(AppSetupEvent::Ran(Err("setup_busy".into()))));
    assert_eq!(lead(&shell), "Setup is already running.");
    assert_eq!(shell.place(), Some(Place::Setup { first_run: true }));
}

#[test]
fn without_engine_support_first_run_says_not_ready_and_continues() {
    let mut shell = Shell::with(ctx(80, 24), Box::new(AppSetup::new(true)));
    jobs(&mut shell);
    shell.arrive(event(AppSetupEvent::Status(Err(NOT_READY.into()))));
    let rows = render(&mut shell, 80, 24);
    find(&rows, "Not ready yet.");
    assert!(rows.last().unwrap().contains("Enter Continue"));
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::Home));
}

#[test]
fn settings_setup_opens_from_go_to_and_esc_goes_back() {
    let mut ctx = ctx(80, 24);
    assert!(
        targets(&ctx)
            .iter()
            .any(|target| target.label == "Settings / Setup"
                && target.kind == "Settings"
                && target.place == Place::Setup { first_run: false })
    );
    let mut setup = open(&Place::Setup { first_run: false }, &mut ctx);
    assert_eq!(setup.crumbs(), ["Settings", "Setup"]);
    assert!(matches!(setup.key(&Key::Esc, &mut ctx), Flow::Ignored));
}

#[test]
fn settings_switch_installed_entries_and_remove_after_yes() {
    let mut shell = settings(120, 36);
    let rows = render(&mut shell, 120, 36);
    assert!(find(&rows, "[x] superpowers").contains("Installed"));
    assert!(find(&rows, "[ ] caveman").contains("Turned off"));
    assert!(find(&rows, "Skills and plugins").contains("4 of 6 on"));

    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    let sent = jobs(&mut shell);
    let [
        AppSetupJob::Pack {
            change: PackChange::Switch { id, on },
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one switch required")
    };
    assert_eq!((id.as_str(), *on), ("superpowers", false));
    shell.arrive(event(AppSetupEvent::PackChanged(Ok(()))));
    assert_eq!(lead(&shell), "superpowers is off.");
    assert!(
        jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, AppSetupJob::Status))
    );

    shell.key(Key::Up);
    shell.key(Key::Char('d'));
    find(
        &render(&mut shell, 120, 36),
        "Remove Graphify skill for agents?",
    );
    shell.key(Key::Char('n'));
    assert!(jobs(&mut shell).is_empty());

    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Char('d'));
    find(&render(&mut shell, 120, 36), "Remove caveman?");
    shell.key(Key::Char('n'));
    assert!(jobs(&mut shell).is_empty());
    shell.key(Key::Char('d'));
    shell.key(Key::Char('y'));
    let sent = jobs(&mut shell);
    let [
        AppSetupJob::Pack {
            change: PackChange::Remove { id },
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one remove required")
    };
    assert_eq!(id, "caveman");
}

#[test]
fn settings_install_missing_sends_the_ticked_entries() {
    let mut shell = settings(120, 36);
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    let rows = render(&mut shell, 120, 36);
    find(&rows, "[x] serena");
    find(
        &rows,
        "Installing approves these MCP servers for your agents: serena.",
    );
    shell.key(Key::Enter);
    let sent = jobs(&mut shell);
    let [AppSetupJob::Run { pack, .. }] = sent.as_slice() else {
        panic!("one setup run required")
    };
    assert_eq!(pack, &["serena"]);
}

#[test]
fn settings_show_the_home_and_github_and_sign_out_after_yes() {
    let mut shell = settings(120, 36);
    let rows = render(&mut shell, 120, 36);
    assert!(find(&rows, "Projects home").contains("/Users/owner/Projects"));
    let github = find(&rows, "GitHub    ");
    assert!(github.contains("Signed in as octocat"));
    assert!(github.contains("with GitHub sign-in"));
    assert!(rows.last().unwrap().contains("g Sign out of GitHub"));
    shell.key(Key::Char('g'));
    find(&render(&mut shell, 120, 36), "Sign out of GitHub?");
    shell.key(Key::Char('y'));
    assert!(matches!(
        jobs(&mut shell).as_slice(),
        [AppSetupJob::GitHubSignOut]
    ));
    let out = GitHubStatus {
        signed_in: false,
        login: None,
        method: GitHubMethod::None,
        waiting: false,
        problem: None,
    };
    shell.arrive(event(AppSetupEvent::GitHubSignedOut(Ok(out))));
    assert_eq!(lead(&shell), "Signed out of GitHub.");
    let rows = render(&mut shell, 120, 36);
    assert!(find(&rows, "GitHub    ").contains("Not signed in"));
    assert!(!rows.last().unwrap().contains("Sign out of GitHub"));
}

#[test]
fn changing_the_home_moves_nothing_unless_move_all_is_chosen() {
    let mut shell = settings(120, 36);
    shell.key(Key::Char('h'));
    find(&render(&mut shell, 120, 36), "/Users/owner/Projects");
    shell.key(Key::Ctrl('u'));
    typed(&mut shell, "Work");
    shell.key(Key::Enter);
    assert_eq!(
        lead(&shell),
        "Use a full folder path, such as /Users/you/Projects."
    );
    assert!(jobs(&mut shell).is_empty());
    shell.key(Key::Ctrl('u'));
    typed(&mut shell, "/Users/owner/Work");
    shell.key(Key::Enter);
    let sent = jobs(&mut shell);
    let [AppSetupJob::FolderSize { path }] = sent.as_slice() else {
        panic!("the current home is measured first")
    };
    assert_eq!(path, "/Users/owner/Projects");
    find(
        &render(&mut shell, 120, 36),
        "Use /Users/owner/Work as the projects home?",
    );
    shell.arrive(event(AppSetupEvent::FolderSize(
        "/Users/owner/Elsewhere".into(),
        Ok(9 * GIB),
    )));
    assert!(
        !render(&mut shell, 120, 36)
            .iter()
            .any(|row| row.contains("9.0 GB")),
        "a size measured for another folder is ignored"
    );
    shell.arrive(event(AppSetupEvent::FolderSize(path.clone(), Ok(3 * GIB))));
    find(
        &render(&mut shell, 120, 36),
        "Move all projects takes 3.0 GB with it.",
    );
    shell.key(Key::Enter);
    let sent = jobs(&mut shell);
    let [
        AppSetupJob::SetHome {
            path,
            move_projects,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one home change required")
    };
    assert_eq!(
        (path.as_str(), *move_projects),
        ("/Users/owner/Work", false)
    );
    shell.arrive(event(AppSetupEvent::HomeSet(Ok(()))));
    assert_eq!(
        lead(&shell),
        "The projects home is now /Users/owner/Work. Nothing was moved."
    );

    shell.key(Key::Char('h'));
    shell.key(Key::Ctrl('u'));
    typed(&mut shell, "/Volumes/Big/Projects");
    shell.key(Key::Enter);
    jobs(&mut shell);
    shell.key(Key::Char('m'));
    let sent = jobs(&mut shell);
    assert!(matches!(
        sent.as_slice(),
        [AppSetupJob::SetHome {
            move_projects: true,
            ..
        }]
    ));
}

#[test]
fn a_long_pack_scrolls_with_the_cursor() {
    let mut shell = Shell::with(ctx(80, 24), Box::new(AppSetup::new(true)));
    jobs(&mut shell);
    let mut status = first_run_status();
    status.pack = (0..17)
        .map(|index| {
            entry(
                &format!("entry-{index}"),
                PackKind::Skill,
                PackGroup::Workflow,
                true,
                PackState::Available,
            )
        })
        .collect();
    shell.arrive(event(AppSetupEvent::Status(Ok(status))));
    for _ in 0..16 {
        shell.key(Key::Down);
    }
    let rows = render(&mut shell, 80, 24);
    assert!(find(&rows, "entry-16").contains("> [x] entry-16"));
    assert!(rows.iter().all(|row| !row.contains("entry-0 ")));
    assert!(rows.iter().all(|row| row.chars().count() <= 80));
}
