use super::*;
use crate::cleanup_settings::{CleanupMode, CleanupSettings};
use crate::github::GitHubMethod;
use crate::master_frame_limit::{FrameSize, MasterFrameSettings};
use crate::master_step_limit::{MasterStepSettings, StepLimit};
use crate::master_time_limit::{MasterTimeSettings, TimeLimit};
use crate::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crate::sandbox::LinuxSockets;
use crate::sandbox_settings::SandboxSettings;
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
        hooks_refused: None,
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
        notes: Vec::new(),
        version: 4,
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
    settings_with(width, height, LinuxSockets::TcpOnly)
}

fn settings_with(width: u16, height: u16, linux_sockets: LinuxSockets) -> Shell {
    let mut shell = Shell::with(ctx(width, height), Box::new(AppSetup::new(false)));
    assert!(matches!(
        jobs(&mut shell).as_slice(),
        [
            AppSetupJob::Status,
            AppSetupJob::Home,
            AppSetupJob::GitHub,
            AppSetupJob::Sandbox,
            AppSetupJob::Cleanup,
            AppSetupJob::Quit,
            AppSetupJob::MasterSteps,
            AppSetupJob::MasterTime,
            AppSetupJob::MasterFrames
        ]
    ));
    shell.arrive(event(AppSetupEvent::Status(Ok(installed_status()))));
    shell.arrive(event(AppSetupEvent::Home(Ok(home()))));
    shell.arrive(event(AppSetupEvent::GitHub(Ok(signed_in()))));
    shell.arrive(event(AppSetupEvent::Sandbox(Ok(SandboxSettings {
        linux_sockets,
    }))));
    shell.arrive(event(AppSetupEvent::Cleanup(
        Ok(CleanupSettings::default()),
    )));
    shell.arrive(event(AppSetupEvent::Quit(Ok(QuitSettings::default()))));
    let steps = MasterStepSettings::default();
    shell.arrive(event(AppSetupEvent::MasterSteps(Ok(steps))));
    let time = MasterTimeSettings::default();
    shell.arrive(event(AppSetupEvent::MasterTime(Ok(time))));
    let frames = MasterFrameSettings::default();
    shell.arrive(event(AppSetupEvent::MasterFrames(Ok(frames))));
    shell
}

fn chosen(shell: &mut Shell) -> LinuxSockets {
    let sent = jobs(shell);
    let [AppSetupJob::SetSandbox { settings, .. }] = sent.as_slice() else {
        panic!("one sandbox change required, got {} jobs", sent.len())
    };
    settings.linux_sockets
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
fn a_setup_note_shows_its_plain_reason_under_tools() {
    let mut shell = Shell::with(ctx(120, 36), Box::new(AppSetup::new(true)));
    assert!(matches!(jobs(&mut shell).as_slice(), [AppSetupJob::Status]));
    let mut status = first_run_status();
    status.notes = vec!["agent_git_needs_tools".into()];
    shell.arrive(event(AppSetupEvent::Status(Ok(status))));
    let rows = render(&mut shell, 120, 36);
    find(
        &rows,
        "Agent git needs the Command Line Tools: run xcode-select --install",
    );
    assert!(
        rows.iter()
            .all(|row| !row.contains("agent_git_needs_tools"))
    );
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
fn a_failed_install_shows_the_sentence_the_engine_sent() {
    let mut shell = first_run(120, 36);
    shell.key(Key::Enter);
    jobs(&mut shell);
    shell.arrive(event(AppSetupEvent::Ran(Ok(()))));
    jobs(&mut shell);
    let mut finished = installed_status();
    finished.dependencies[0].state = DependencyState::Failed {
        reason: crate::setup::plain_reason("setup_homebrew_missing").to_owned(),
    };
    finished.pack[2].state = PackState::Failed {
        reason: crate::setup::plain_reason("pack_fetch_failed").to_owned(),
    };
    shell.arrive(event(AppSetupEvent::Status(Ok(finished))));
    let rows = render(&mut shell, 120, 36);
    find(&rows, "Homebrew is not installed. Install tmux yourself");
    assert!(
        !rows.iter().any(|row| row.contains("Something went wrong")),
        "{}",
        rows.join("\n")
    );
    shell.key(Key::Down);
    shell.key(Key::Down);
    find(
        &render(&mut shell, 120, 36),
        "caveman: The source could not be downloaded.",
    );
}

#[test]
fn an_installed_entry_whose_hooks_were_refused_says_why_without_being_picked() {
    let mut status = installed_status();
    status.pack[1].hooks_refused = Some(
        crate::plugins::plain_problem("hooks_plugin_manifest_missing")
            .unwrap()
            .to_owned(),
    );
    let mut shell = settings(120, 36);
    shell.arrive(event(AppSetupEvent::Status(Ok(status))));
    let rows = render(&mut shell, 120, 36);
    assert!(find(&rows, "[x] superpowers").contains("Installed"));
    find(
        &rows,
        "superpowers hooks were not added: This folder has hooks but no .claude-plugin/plugin.json naming the plugin.",
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("hooks were not added"))
            .count(),
        1,
        "{}",
        rows.join("\n")
    );
}

#[test]
fn a_setup_problem_without_a_sentence_names_its_code() {
    assert_eq!(
        setup_plain("setup_code_nobody_uses"),
        "Something went wrong during setup. (code: setup_code_nobody_uses)"
    );
    assert_eq!(
        setup_plain("Permission denied (os error 13)"),
        "Something went wrong during setup. (code: engine_error_unplain)"
    );
    assert_eq!(
        setup_plain("github_config_unsafe"),
        "The GitHub settings file is unreadable or not private to you."
    );
    assert_eq!(
        setup_plain("home_move_nested"),
        crate::projects_home::plain_problem("home_move_nested").unwrap()
    );
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
            change: PackChange::Switch { id, on, version },
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one switch required")
    };
    assert_eq!((id.as_str(), *on, *version), ("superpowers", false, 4));
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
            change: PackChange::Remove { id, version },
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one remove required")
    };
    assert_eq!(*version, 4);
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

#[test]
fn settings_show_the_linux_sockets_setting_and_its_risk() {
    let mut shell = settings(120, 60);
    let rows = render(&mut shell, 120, 60);
    assert!(find(&rows, "Linux sockets  ").contains("TCP only, names looked up over TCP"));
    find(&rows, "RES_OPTIONS");
    assert!(
        sockets_words(LinuxSockets::TcpOnly)
            .1
            .contains("Wrappers are judged by their first file, a script by its interpreter")
    );
    assert!(rows.last().unwrap().contains("l Linux sockets"));
    shell.key(Key::Char('l'));
    let rows = render(&mut shell, 120, 60);
    find(
        &rows,
        "how an agent limited to its model reaches the network",
    );
    for title in [
        "> TCP only, names looked up over TCP (recommended)",
        "  Do not start limited-network agents",
        "  TCP ports only",
    ] {
        find(&rows, title);
    }
    find(&rows, "Hosts and UDP are not limited");
    find(&rows, "the master is unavailable there");
    shell.key(Key::Enter);
    assert!(
        jobs(&mut shell).is_empty(),
        "the current option saves nothing"
    );
    assert_eq!(lead(&shell), "That is already the setting.");
}

#[test]
fn choosing_tcp_only_saves_it_in_the_engine() {
    let mut shell = settings_with(120, 60, LinuxSockets::TcpPorts);
    assert!(find(&render(&mut shell, 120, 60), "Linux sockets  ").contains("TCP ports only"));
    shell.key(Key::Char('l'));
    find(&render(&mut shell, 120, 60), "> TCP ports only");
    shell.key(Key::Up);
    shell.key(Key::Up);
    shell.key(Key::Up);
    shell.key(Key::Enter);
    assert_eq!(chosen(&mut shell), LinuxSockets::TcpOnly);
    shell.arrive(event(AppSetupEvent::SandboxSet(Ok(()))));
    assert_eq!(
        lead(&shell),
        "Linux sockets: TCP only, names looked up over TCP (recommended)."
    );
    assert!(
        jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, AppSetupJob::Sandbox)),
        "a saved change reads the setting back"
    );
}

#[test]
fn choosing_do_not_start_saves_refuse_in_the_engine() {
    let mut shell = settings(120, 60);
    shell.key(Key::Char('l'));
    shell.key(Key::Down);
    find(
        &render(&mut shell, 120, 60),
        "> Do not start limited-network agents",
    );
    shell.key(Key::Enter);
    assert_eq!(chosen(&mut shell), LinuxSockets::Refuse);
    shell.arrive(event(AppSetupEvent::SandboxSet(Ok(()))));
    assert_eq!(
        lead(&shell),
        "Linux sockets: Do not start limited-network agents."
    );
}

#[test]
fn choosing_tcp_ports_saves_it_and_a_refused_save_says_why() {
    let mut shell = settings(120, 60);
    shell.key(Key::Char('l'));
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Down);
    find(&render(&mut shell, 120, 60), "> TCP ports only");
    shell.key(Key::Enter);
    assert_eq!(chosen(&mut shell), LinuxSockets::TcpPorts);
    shell.arrive(event(AppSetupEvent::SandboxSet(Err("busy".into()))));
    assert_ne!(lead(&shell), "Linux sockets: TCP ports only.");
    assert!(!lead(&shell).is_empty());
    shell.key(Key::Char('l'));
    shell.key(Key::Esc);
    assert!(jobs(&mut shell).is_empty(), "Esc changes nothing");
}

fn cleanup_chosen(shell: &mut Shell) -> CleanupSettings {
    let sent = jobs(shell);
    let [AppSetupJob::SetCleanup { settings, .. }] = sent.as_slice() else {
        panic!(
            "one finished agents change required, got {} jobs",
            sent.len()
        )
    };
    *settings
}

fn stored(shell: &mut Shell, mode: CleanupMode, days: u16) {
    shell.arrive(event(AppSetupEvent::Cleanup(Ok(CleanupSettings {
        mode,
        days,
    }))));
}

fn clear(shell: &mut Shell) {
    for _ in 0..3 {
        shell.key(Key::Backspace);
    }
}

#[test]
fn settings_show_finished_agents_one_day_after_by_default_and_what_stays() {
    let mut shell = settings(160, 60);
    let rows = render(&mut shell, 160, 60);
    assert!(find(&rows, "Finished agents  ").contains("1 day after the agent finishes"));
    find(
        &rows,
        "Only the worktree goes: the code checkout and its build files.",
    );
    assert!(rows.last().unwrap().contains("f Finished agents"));
    shell.key(Key::Char('f'));
    let rows = render(&mut shell, 160, 60);
    find(
        &rows,
        "when CrazyAgent removes a finished agent's worktree on its own",
    );
    for title in [
        "> 1 day after the agent finishes",
        "  Once its work lands",
        "  Only when I click Clean up",
    ] {
        find(&rows, title);
    }
    find(
        &rows,
        "The agent's conversations, history, logs and messages stay.",
    );
    shell.key(Key::Enter);
    find(
        &render(&mut shell, 160, 60),
        "Days after the agent finishes",
    );
    shell.key(Key::Enter);
    assert!(jobs(&mut shell).is_empty(), "the stored days save nothing");
    assert_eq!(lead(&shell), "That is already the setting.");
}

#[test]
fn after_n_days_saves_the_typed_days_and_refuses_days_outside_one_to_365() {
    let mut shell = settings(120, 60);
    shell.key(Key::Char('f'));
    shell.key(Key::Enter);
    for refused in ["0", "366", "1.5", "-2", "ten", ""] {
        clear(&mut shell);
        typed(&mut shell, refused);
        shell.key(Key::Enter);
        assert!(jobs(&mut shell).is_empty(), "{refused:?} is not sent");
        assert_eq!(lead(&shell), "Enter a whole number of days from 1 to 365.");
        find(
            &render(&mut shell, 120, 60),
            "Days after the agent finishes",
        );
    }
    clear(&mut shell);
    typed(&mut shell, "30");
    shell.key(Key::Enter);
    assert_eq!(
        cleanup_chosen(&mut shell),
        CleanupSettings {
            mode: CleanupMode::AfterDays,
            days: 30
        }
    );
    shell.arrive(event(AppSetupEvent::CleanupSet(Ok(()))));
    assert_eq!(
        lead(&shell),
        "Finished agents: 30 days after the agent finishes."
    );
    assert!(
        jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, AppSetupJob::Cleanup)),
        "a saved change reads the setting back"
    );
}

#[test]
fn once_its_work_lands_saves_it_and_keeps_the_stored_days() {
    let mut shell = settings(120, 60);
    stored(&mut shell, CleanupMode::AfterDays, 7);
    assert!(
        find(&render(&mut shell, 120, 60), "Finished agents  ")
            .contains("7 days after the agent finishes")
    );
    shell.key(Key::Char('f'));
    shell.key(Key::Down);
    find(&render(&mut shell, 120, 60), "> Once its work lands");
    shell.key(Key::Enter);
    assert_eq!(
        cleanup_chosen(&mut shell),
        CleanupSettings {
            mode: CleanupMode::OnceLanded,
            days: 7
        }
    );
    shell.arrive(event(AppSetupEvent::CleanupSet(Ok(()))));
    assert_eq!(lead(&shell), "Finished agents: Once its work lands.");
}

#[test]
fn only_when_i_click_clean_up_saves_manual_and_a_refused_save_says_why() {
    let mut shell = settings(120, 60);
    shell.key(Key::Char('f'));
    shell.key(Key::Down);
    shell.key(Key::Down);
    shell.key(Key::Down);
    find(&render(&mut shell, 120, 60), "> Only when I click Clean up");
    shell.key(Key::Enter);
    assert_eq!(
        cleanup_chosen(&mut shell),
        CleanupSettings {
            mode: CleanupMode::Manual,
            days: 1
        }
    );
    shell.arrive(event(AppSetupEvent::CleanupSet(Err("busy".into()))));
    assert_ne!(lead(&shell), "Finished agents: Only when I click Clean up.");
    assert!(!lead(&shell).is_empty());
    stored(&mut shell, CleanupMode::Manual, 1);
    assert!(
        find(&render(&mut shell, 120, 60), "Finished agents  ")
            .contains("Only when I click Clean up")
    );
    shell.key(Key::Char('f'));
    find(&render(&mut shell, 120, 60), "> Only when I click Clean up");
    shell.key(Key::Esc);
    assert!(jobs(&mut shell).is_empty(), "Esc changes nothing");
}

#[test]
fn a_stored_finished_agents_setting_this_version_does_not_know_is_not_offered() {
    let mut shell = settings(120, 60);
    shell.arrive(event(AppSetupEvent::Cleanup(Err(
        "cleanup_settings_invalid".into(),
    ))));
    let rows = render(&mut shell, 120, 60);
    let row = find(&rows, "Finished agents  ");
    assert!(row.contains("Failed to read it."), "{row}");
    assert!(row.contains("not one this version knows"), "{row}");
    shell.key(Key::Char('f'));
    assert!(jobs(&mut shell).is_empty());
    assert_eq!(
        lead(&shell),
        "The finished agents setting is not known yet."
    );
}

fn quit_chosen(shell: &mut Shell) -> QuitSettings {
    let sent = jobs(shell);
    let [AppSetupJob::SetQuit { settings, .. }] = sent.as_slice() else {
        panic!("one quitting change required, got {} jobs", sent.len())
    };
    *settings
}

#[test]
fn settings_show_team_agents_kept_running_and_ended_a_day_after_quitting_by_default() {
    let mut shell = settings(200, 60);
    let rows = render(&mut shell, 200, 60);
    assert!(find(&rows, "When quitting  ").contains("Keep team agents running"));
    assert!(find(&rows, "Left running  ").contains("24 hours after CrazyAgent quits"));
    find(
        &rows,
        "An agent still running when the time is up ends with its terminal.",
    );
    let note = rows
        .iter()
        .skip_while(|row| !row.contains("An agent still running when the time is up"))
        .take(3)
        .map(|row| row.trim_matches(|c: char| !(c.is_alphanumeric() || c.is_ascii_punctuation())))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        note.contains(
            "The limit also covers agents left behind when CrazyAgent did not quit cleanly."
        ),
        "{note}"
    );
    assert!(rows.last().unwrap().contains("q Quitting"));
    shell.key(Key::Char('q'));
    let rows = render(&mut shell, 200, 60);
    find(&rows, "what happens to team agents that are running");
    find(&rows, "> Keep team agents running");
    find(
        &rows,
        "They go on with their work, and CrazyAgent takes them over when it opens again.",
    );
    find(&rows, "  Stop team agents");
    find(
        &rows,
        "Each agent stops as Stop would, and its task goes back to the queue.",
    );
    assert!(rows.last().unwrap().contains("Enter Next"));
    shell.key(Key::Enter);
    let rows = render(&mut shell, 200, 60);
    find(
        &rows,
        "how long agents left running wait for CrazyAgent to open again",
    );
    find(&rows, "> 24 hours after CrazyAgent quits");
    find(&rows, "  No limit, until CrazyAgent opens again");
    shell.key(Key::Enter);
    find(&render(&mut shell, 200, 60), "Hours after CrazyAgent quits");
    shell.key(Key::Enter);
    assert!(
        jobs(&mut shell).is_empty(),
        "the stored setting saves nothing"
    );
    assert_eq!(lead(&shell), "That is already the setting.");
}

#[test]
fn stop_team_agents_saves_stop_with_the_stored_hours_and_reads_the_setting_back() {
    let mut shell = settings(120, 60);
    shell.key(Key::Char('q'));
    shell.key(Key::Down);
    shell.key(Key::Down);
    find(&render(&mut shell, 120, 60), "> Stop team agents");
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    assert_eq!(
        quit_chosen(&mut shell),
        QuitSettings {
            on_quit: OnQuit::Stop,
            left_running: LeftRunning::AfterHours,
            hours: 24
        }
    );
    shell.arrive(event(AppSetupEvent::QuitSet(Ok(()))));
    assert_eq!(
        lead(&shell),
        "When quitting: Stop team agents. Left running: 24 hours after CrazyAgent quits."
    );
    assert!(
        jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, AppSetupJob::Quit)),
        "a saved change reads the setting back"
    );
    shell.arrive(event(AppSetupEvent::Quit(Ok(QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::AfterHours,
        hours: 24,
    }))));
    assert!(find(&render(&mut shell, 120, 60), "When quitting  ").contains("Stop team agents"));
    shell.key(Key::Char('q'));
    find(&render(&mut shell, 120, 60), "> Stop team agents");
}

#[test]
fn no_limit_saves_never_and_keeps_the_stored_hours_for_later() {
    let mut shell = settings(120, 60);
    shell.arrive(event(AppSetupEvent::Quit(Ok(QuitSettings {
        on_quit: OnQuit::KeepRunning,
        left_running: LeftRunning::AfterHours,
        hours: 48,
    }))));
    assert!(
        find(&render(&mut shell, 120, 60), "Left running  ")
            .contains("48 hours after CrazyAgent quits")
    );
    shell.key(Key::Char('q'));
    shell.key(Key::Enter);
    shell.key(Key::Down);
    find(
        &render(&mut shell, 120, 60),
        "> No limit, until CrazyAgent opens again",
    );
    shell.key(Key::Enter);
    assert_eq!(
        quit_chosen(&mut shell),
        QuitSettings {
            on_quit: OnQuit::KeepRunning,
            left_running: LeftRunning::Never,
            hours: 48
        }
    );
    shell.arrive(event(AppSetupEvent::QuitSet(Ok(()))));
    assert_eq!(
        lead(&shell),
        "When quitting: Keep team agents running. Left running: No limit, until CrazyAgent opens again."
    );
}

#[test]
fn after_hours_saves_the_typed_hours_and_refuses_hours_outside_one_to_168() {
    let mut shell = settings(120, 60);
    shell.key(Key::Char('q'));
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    for refused in ["0", "169", "1.5", "-2", "ten", ""] {
        clear(&mut shell);
        typed(&mut shell, refused);
        shell.key(Key::Enter);
        assert!(jobs(&mut shell).is_empty(), "{refused:?} is not sent");
        assert_eq!(lead(&shell), "Enter a whole number of hours from 1 to 168.");
        find(&render(&mut shell, 120, 60), "Hours after CrazyAgent quits");
    }
    clear(&mut shell);
    typed(&mut shell, "1");
    shell.key(Key::Enter);
    assert_eq!(
        quit_chosen(&mut shell),
        QuitSettings {
            on_quit: OnQuit::KeepRunning,
            left_running: LeftRunning::AfterHours,
            hours: 1
        }
    );
    shell.arrive(event(AppSetupEvent::QuitSet(Err("busy".into()))));
    assert_ne!(
        lead(&shell),
        "When quitting: Keep team agents running. Left running: 1 hour after CrazyAgent quits."
    );
    assert!(!lead(&shell).is_empty());
}

#[test]
fn esc_at_each_quitting_step_changes_nothing() {
    let mut shell = settings(120, 60);
    shell.key(Key::Char('q'));
    shell.key(Key::Esc);
    shell.key(Key::Char('q'));
    shell.key(Key::Enter);
    shell.key(Key::Esc);
    shell.key(Key::Char('q'));
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    shell.key(Key::Esc);
    assert!(jobs(&mut shell).is_empty(), "Esc changes nothing");
}

#[test]
fn a_stored_quitting_setting_this_version_does_not_know_is_not_offered() {
    let mut shell = settings(120, 60);
    shell.arrive(event(AppSetupEvent::Quit(Err(
        "quit_settings_invalid".into()
    ))));
    let rows = render(&mut shell, 120, 60);
    let row = find(&rows, "When quitting  ");
    assert!(row.contains("Failed to read it."), "{row}");
    assert!(row.contains("not one this version knows"), "{row}");
    assert!(!rows.iter().any(|row| row.contains("Left running  ")));
    shell.key(Key::Char('q'));
    assert!(jobs(&mut shell).is_empty());
    assert_eq!(lead(&shell), "The quitting setting is not known yet.");
}

fn flat(rows: &[String]) -> String {
    rows.iter()
        .map(|row| row.trim_matches(|c: char| !(c.is_alphanumeric() || c.is_ascii_punctuation())))
        .collect::<Vec<_>>()
        .join(" ")
}

fn steps_chosen(shell: &mut Shell) -> StepLimit {
    let sent = jobs(shell);
    let [AppSetupJob::SetMasterSteps { settings, .. }] = sent.as_slice() else {
        panic!(
            "one master step limit change required, got {} jobs",
            sent.len()
        )
    };
    settings.limit
}

#[test]
fn settings_show_no_master_step_limit_by_default_with_the_four_options() {
    let mut shell = settings(200, 80);
    let rows = render(&mut shell, 200, 80);
    assert!(find(&rows, "Master steps  ").contains("No limit"));
    let shown = flat(&rows);
    assert!(
        shown.contains("Stopped after N steps; reply with 'continue' to go on."),
        "{shown}"
    );
    assert!(rows.last().unwrap().contains("m Master steps"));
    shell.key(Key::Char('m'));
    let rows = render(&mut shell, 200, 80);
    find(&rows, "the most steps a project's master takes in one turn");
    find(&rows, "> No limit");
    find(
        &rows,
        "A turn runs until the master replies or its time is up.",
    );
    for steps in [50, 100, 200] {
        find(&rows, &format!("  {steps} steps"));
        find(&rows, &format!("A turn stops after {steps} steps."));
    }
    let shown = flat(&rows);
    assert!(
        shown.contains("The limit covers Claude and Codex masters with tools on."),
        "{shown}"
    );
    assert!(
        shown.contains("an rcli master, which has no tools, answers each turn in one step"),
        "{shown}"
    );
    assert!(!shown.contains("counts no steps"), "{shown}");
    assert!(rows.last().unwrap().contains("Enter Save"));
    shell.key(Key::Enter);
    assert!(
        jobs(&mut shell).is_empty(),
        "the stored setting saves nothing"
    );
    assert_eq!(lead(&shell), "That is already the setting.");
}

#[test]
fn each_master_step_limit_is_saved_when_picked_and_shown_once_stored() {
    let mut shell = settings(200, 80);
    let picks = [
        (StepLimit::Steps50, Key::Down, 1, "50 steps"),
        (StepLimit::Steps100, Key::Down, 1, "100 steps"),
        (StepLimit::Steps200, Key::Down, 1, "200 steps"),
        (StepLimit::NoLimit, Key::Up, 3, "No limit"),
    ];
    for (limit, key, presses, title) in picks {
        shell.key(Key::Char('m'));
        for _ in 0..presses {
            shell.key(key.clone());
        }
        find(&render(&mut shell, 200, 80), &format!("> {title}"));
        shell.key(Key::Enter);
        assert_eq!(steps_chosen(&mut shell), limit);
        shell.arrive(event(AppSetupEvent::MasterStepsSet(Ok(()))));
        assert_eq!(lead(&shell), format!("Master step limit: {title}."));
        assert!(
            jobs(&mut shell)
                .iter()
                .any(|job| matches!(job, AppSetupJob::MasterSteps)),
            "the saved limit is read back"
        );
        let stored = MasterStepSettings { limit };
        shell.arrive(event(AppSetupEvent::MasterSteps(Ok(stored))));
        assert!(find(&render(&mut shell, 200, 80), "Master steps  ").contains(title));
    }
}

#[test]
fn esc_and_a_refused_save_leave_the_master_step_limit_as_it_was() {
    let mut shell = settings(200, 80);
    shell.key(Key::Char('m'));
    shell.key(Key::Down);
    shell.key(Key::Esc);
    assert!(jobs(&mut shell).is_empty(), "Esc changes nothing");
    shell.key(Key::Char('m'));
    shell.key(Key::Down);
    shell.key(Key::Enter);
    assert_eq!(steps_chosen(&mut shell), StepLimit::Steps50);
    shell.arrive(event(AppSetupEvent::MasterStepsSet(Err("busy".into()))));
    assert_ne!(lead(&shell), "Master step limit: 50 steps.");
    assert!(!lead(&shell).is_empty());
    assert!(find(&render(&mut shell, 200, 80), "Master steps  ").contains("No limit"));
}

#[test]
fn a_stored_master_step_limit_this_version_does_not_know_is_not_offered() {
    let mut shell = settings(200, 80);
    shell.arrive(event(AppSetupEvent::MasterSteps(Err(
        "master_step_settings_invalid".into(),
    ))));
    let rows = render(&mut shell, 200, 80);
    let row = find(&rows, "Master steps  ");
    assert!(row.contains("Failed to read it."), "{row}");
    assert!(row.contains("not one this version knows"), "{row}");
    shell.key(Key::Char('m'));
    assert!(jobs(&mut shell).is_empty());
    assert_eq!(lead(&shell), "The master step limit is not known yet.");
}

fn time_chosen(shell: &mut Shell) -> TimeLimit {
    let sent = jobs(shell);
    let [AppSetupJob::SetMasterTime { settings, .. }] = sent.as_slice() else {
        panic!(
            "one master time limit change required, got {} jobs",
            sent.len()
        )
    };
    settings.limit
}

#[test]
fn settings_show_no_master_time_limit_by_default_with_the_four_options() {
    let mut shell = settings(200, 80);
    let rows = render(&mut shell, 200, 80);
    assert!(find(&rows, "Master time  ").contains("No limit"));
    let shown = flat(&rows);
    assert!(
        shown.contains("Stopped after N minutes; reply with 'continue' to go on."),
        "{shown}"
    );
    assert!(!shown.contains("ran out of time"), "{shown}");
    assert!(rows.last().unwrap().contains("t Master time"));
    shell.key(Key::Char('t'));
    let rows = render(&mut shell, 200, 80);
    find(&rows, "the longest a project's master works on one turn");
    find(&rows, "> No limit");
    find(
        &rows,
        "A turn runs until the master replies or you stop it.",
    );
    for minutes in [10, 30, 60] {
        find(&rows, &format!("  {minutes} min"));
        find(&rows, &format!("A turn stops after {minutes} minutes."));
    }
    let shown = flat(&rows);
    assert!(shown.contains("The limit covers every master:"), "{shown}");
    assert!(shown.contains("Claude, Codex and rcli."), "{shown}");
    assert!(!shown.contains("still stops after"), "{shown}");
    assert!(rows.last().unwrap().contains("Enter Save"));
    shell.key(Key::Enter);
    assert!(
        jobs(&mut shell).is_empty(),
        "the stored setting saves nothing"
    );
    assert_eq!(lead(&shell), "That is already the setting.");
}

#[test]
fn each_master_time_limit_is_saved_when_picked_and_shown_once_stored() {
    let mut shell = settings(200, 80);
    let picks = [
        (TimeLimit::Minutes10, Key::Down, 1, "10 min"),
        (TimeLimit::Minutes30, Key::Down, 1, "30 min"),
        (TimeLimit::Minutes60, Key::Down, 1, "60 min"),
        (TimeLimit::NoLimit, Key::Up, 3, "No limit"),
    ];
    for (limit, key, presses, title) in picks {
        shell.key(Key::Char('t'));
        for _ in 0..presses {
            shell.key(key.clone());
        }
        find(&render(&mut shell, 200, 80), &format!("> {title}"));
        shell.key(Key::Enter);
        assert_eq!(time_chosen(&mut shell), limit);
        shell.arrive(event(AppSetupEvent::MasterTimeSet(Ok(()))));
        assert_eq!(lead(&shell), format!("Master time limit: {title}."));
        assert!(
            jobs(&mut shell)
                .iter()
                .any(|job| matches!(job, AppSetupJob::MasterTime)),
            "the saved limit is read back"
        );
        let stored = MasterTimeSettings { limit };
        shell.arrive(event(AppSetupEvent::MasterTime(Ok(stored))));
        assert!(find(&render(&mut shell, 200, 80), "Master time  ").contains(title));
        assert!(
            find(&render(&mut shell, 200, 80), "Master steps  ").contains("No limit"),
            "the step limit is its own setting"
        );
    }
}

#[test]
fn esc_and_a_refused_save_leave_the_master_time_limit_as_it_was() {
    let mut shell = settings(200, 80);
    shell.key(Key::Char('t'));
    shell.key(Key::Down);
    shell.key(Key::Esc);
    assert!(jobs(&mut shell).is_empty(), "Esc changes nothing");
    shell.key(Key::Char('t'));
    shell.key(Key::Down);
    shell.key(Key::Enter);
    assert_eq!(time_chosen(&mut shell), TimeLimit::Minutes10);
    shell.arrive(event(AppSetupEvent::MasterTimeSet(Err("busy".into()))));
    assert_ne!(lead(&shell), "Master time limit: 10 min.");
    assert!(!lead(&shell).is_empty());
    assert!(find(&render(&mut shell, 200, 80), "Master time  ").contains("No limit"));
}

#[test]
fn a_stored_master_time_limit_this_version_does_not_know_is_not_offered() {
    let mut shell = settings(200, 80);
    shell.arrive(event(AppSetupEvent::MasterTime(Err(
        "master_time_settings_invalid".into(),
    ))));
    let rows = render(&mut shell, 200, 80);
    let row = find(&rows, "Master time  ");
    assert!(row.contains("Failed to read it."), "{row}");
    assert!(row.contains("not one this version knows"), "{row}");
    shell.key(Key::Char('t'));
    assert!(jobs(&mut shell).is_empty());
    assert_eq!(lead(&shell), "The master time limit is not known yet.");
}

const ASK_SIZES: [(u16, u16); 3] = [(80, 24), (80, 27), (120, 31)];

/// Renders Settings and checks the open ask is drawn whole inside the body rows: its first
/// needle (the cursor glyph for a field ask) above or on its last needle's row, and that
/// last row the last filled body row. Takes the shell, the size and both needles.
/// Returns nothing; panics with the frame when the ask is cut.
fn ask_in_body(shell: &mut Shell, width: u16, height: u16, first: &str, last: &str) {
    let rows = render(shell, width, height);
    assert_eq!(rows.len(), usize::from(height));
    let body = &rows[2..rows.len() - 2];
    let shown = |needle: &str| {
        body.iter()
            .rposition(|row| row.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "{width}x{height}: no body row has {needle:?}:\n{}",
                    rows.join("\n")
                )
            })
    };
    let (top, end) = (shown(first), shown(last));
    assert!(top <= end, "{width}x{height}:\n{}", rows.join("\n"));
    let filled = body.iter().rposition(|row| !row.trim().is_empty());
    assert_eq!(
        filled,
        Some(end),
        "{width}x{height}: the ask ends the body:\n{}",
        rows.join("\n")
    );
}

#[test]
fn the_remove_ask_is_drawn_whole_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('d'));
        let first = "Remove Graphify skill for agents?";
        ask_in_body(&mut shell, width, height, first, "stop getting it.");
    }
}

#[test]
fn the_sign_out_ask_is_drawn_whole_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('g'));
        let (first, last) = ("Sign out of GitHub?", "projects stay.");
        ask_in_body(&mut shell, width, height, first, last);
    }
}

#[test]
fn the_home_path_field_and_its_cursor_are_inside_the_body_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('h'));
        ask_in_body(&mut shell, width, height, "█", "you choose next.");
    }
}

#[test]
fn the_home_choice_ask_is_drawn_whole_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('h'));
        shell.key(Key::Ctrl('u'));
        typed(&mut shell, "/Users/owner/Work");
        shell.key(Key::Enter);
        let first = "Use /Users/owner/Work as the projects home?";
        let last = "Measuring the projects in the current home";
        ask_in_body(&mut shell, width, height, first, last);
    }
}

#[test]
fn the_sockets_ask_is_drawn_whole_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('l'));
        let (first, last) = ("Linux limited-network sockets:", "host.");
        ask_in_body(&mut shell, width, height, first, last);
    }
}

#[test]
fn the_cleanup_ask_and_its_days_field_are_drawn_whole_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('f'));
        let first = "Finished agents: when";
        ask_in_body(&mut shell, width, height, first, "shows why.");
        shell.key(Key::Up);
        shell.key(Key::Up);
        shell.key(Key::Enter);
        ask_in_body(&mut shell, width, height, "█", "days after it finishes.");
    }
}

#[test]
fn the_quit_asks_and_the_hours_field_are_drawn_whole_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('q'));
        let (first, last) = ("When CrazyAgent quits:", "back to the queue.");
        ask_in_body(&mut shell, width, height, first, last);
        shell.key(Key::Enter);
        let (first, last) = ("Left running: how long agents", "did not quit cleanly.");
        ask_in_body(&mut shell, width, height, first, last);
        shell.key(Key::Up);
        shell.key(Key::Enter);
        ask_in_body(
            &mut shell,
            width,
            height,
            "█",
            "quits ends with its terminal.",
        );
    }
}

#[test]
fn the_master_step_limit_ask_is_drawn_whole_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('m'));
        let (first, last) = ("Master step limit:", "never stops it.");
        ask_in_body(&mut shell, width, height, first, last);
    }
}

#[test]
fn the_master_time_limit_ask_is_drawn_whole_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('t'));
        let (first, last) = ("Master time limit:", "Codex and rcli.");
        ask_in_body(&mut shell, width, height, first, last);
    }
}

fn frames_chosen(shell: &mut Shell) -> FrameSize {
    let sent = jobs(shell);
    let [AppSetupJob::SetMasterFrames { settings, .. }] = sent.as_slice() else {
        panic!(
            "one master frame size change required, got {} jobs",
            sent.len()
        )
    };
    settings.limit
}

#[test]
fn settings_show_a_64_kib_master_frame_by_default_with_the_three_options() {
    let mut shell = settings(200, 80);
    let rows = render(&mut shell, 200, 80);
    assert!(find(&rows, "Master frame  ").contains("64 KiB"));
    let shown = flat(&rows);
    assert!(
        shown.contains("One message was larger than the master frame size limit."),
        "{shown}"
    );
    assert!(
        shown.contains("or at 1 MiB shorten the message or the instructions"),
        "{shown}"
    );
    let bar = rows.last().unwrap();
    assert!(bar.contains("t Master time   z Master frame"), "{bar}");
    shell.key(Key::Char('z'));
    let rows = render(&mut shell, 200, 80);
    find(
        &rows,
        "the largest one message the engine sends a project's master",
    );
    find(&rows, "> 64 KiB");
    find(
        &rows,
        "The engine's own message size; enough for most turns.",
    );
    find(&rows, "  256 KiB");
    find(&rows, "Room for long instructions and large pasted text.");
    find(&rows, "  1 MiB");
    find(
        &rows,
        "The largest; for a turn that still stops at 256 KiB.",
    );
    let shown = flat(&rows);
    assert!(
        shown.contains("The limit covers Claude and Codex masters;"),
        "{shown}"
    );
    assert!(shown.contains("rcli keeps 64 KiB."), "{shown}");
    assert!(rows.last().unwrap().contains("Enter Save"));
    shell.key(Key::Enter);
    assert!(
        jobs(&mut shell).is_empty(),
        "the stored setting saves nothing"
    );
    assert_eq!(lead(&shell), "That is already the setting.");
}

#[test]
fn each_master_frame_size_is_saved_when_picked_and_shown_once_stored() {
    let mut shell = settings(200, 80);
    let picks = [
        (FrameSize::Kib256, Key::Down, 1, "256 KiB"),
        (FrameSize::Mib1, Key::Down, 1, "1 MiB"),
        (FrameSize::Kib64, Key::Up, 2, "64 KiB"),
    ];
    assert_eq!(picks.len(), FrameSize::ALL.len());
    for (limit, key, presses, title) in picks {
        shell.key(Key::Char('z'));
        for _ in 0..presses {
            shell.key(key.clone());
        }
        find(&render(&mut shell, 200, 80), &format!("> {title}"));
        shell.key(Key::Enter);
        assert_eq!(frames_chosen(&mut shell), limit);
        shell.arrive(event(AppSetupEvent::MasterFramesSet(Ok(()))));
        assert_eq!(lead(&shell), format!("Master frame size limit: {title}."));
        assert!(
            jobs(&mut shell)
                .iter()
                .any(|job| matches!(job, AppSetupJob::MasterFrames)),
            "the saved size is read back"
        );
        let stored = MasterFrameSettings { limit };
        shell.arrive(event(AppSetupEvent::MasterFrames(Ok(stored))));
        assert!(find(&render(&mut shell, 200, 80), "Master frame  ").contains(title));
    }
}

#[test]
fn esc_and_a_refused_save_leave_the_master_frame_size_as_it_was() {
    let mut shell = settings(200, 80);
    shell.key(Key::Char('z'));
    shell.key(Key::Down);
    shell.key(Key::Esc);
    assert!(jobs(&mut shell).is_empty(), "Esc changes nothing");
    shell.key(Key::Char('z'));
    shell.key(Key::Down);
    shell.key(Key::Enter);
    assert_eq!(frames_chosen(&mut shell), FrameSize::Kib256);
    shell.arrive(event(AppSetupEvent::MasterFramesSet(Err("busy".into()))));
    assert_ne!(lead(&shell), "Master frame size limit: 256 KiB.");
    assert!(!lead(&shell).is_empty());
    assert!(find(&render(&mut shell, 200, 80), "Master frame  ").contains("64 KiB"));
}

#[test]
fn a_stored_master_frame_size_this_version_does_not_know_is_not_offered() {
    let mut shell = settings(200, 80);
    shell.arrive(event(AppSetupEvent::MasterFrames(Err(
        "master_frame_settings_invalid".into(),
    ))));
    let rows = render(&mut shell, 200, 80);
    let row = find(&rows, "Master frame  ");
    assert!(row.contains("Failed to read it."), "{row}");
    assert!(row.contains("not one this version knows"), "{row}");
    shell.key(Key::Char('z'));
    assert!(jobs(&mut shell).is_empty());
    assert_eq!(
        lead(&shell),
        "The master frame size limit is not known yet."
    );
}

#[test]
fn the_master_frame_size_ask_is_drawn_whole_at_the_smallest_and_odd_sizes() {
    for (width, height) in ASK_SIZES {
        let mut shell = settings(width, height);
        shell.key(Key::Char('z'));
        let (first, last) = ("Master frame size limit:", "rcli keeps 64 KiB.");
        ask_in_body(&mut shell, width, height, first, last);
    }
}
