use super::*;
use crate::ide::bridge::reveal::RevealTarget;
use crate::owner_profile::OwnerProfile;
use crate::protocol::ProjectId;
use crate::team::{AgentStatus, GraphNode, NodeKind, TeamGraph};
use crate::tui::client::ide::RevealQuery;
use crate::tui::glyphs::UNICODE;
use crate::tui::keys::{ShiftArrows, ShiftEnter};
use crate::tui::seam::{Activity, ResearchCounts};
use crate::tui::theme::{Appearance, Colors, Depth};

fn ctx() -> Ctx {
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        String::new(),
    );
    ctx.link = Link::Ready;
    ctx
}

fn card(name: &str, running: usize) -> ProjectCard {
    ProjectCard {
        id: ProjectId::new(format!("project-{}", name.to_lowercase())).unwrap(),
        name: name.into(),
        about: String::new(),
        platforms: Vec::new(),
        pool: None,
        pool_id: None,
        research: ResearchCounts {
            running,
            ..Default::default()
        },
        created_ms: None,
        activity: Default::default(),
    }
}

fn at(place: Place, ctx: Ctx) -> Shell {
    let mut ctx = ctx;
    let screen = open(&place, &mut ctx);
    Shell::with(ctx, screen)
}

fn drain(shell: &mut Shell) -> Vec<String> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .map(|request| format!("{request:?}"))
        .collect()
}

#[test]
fn go_to_ranks_prefix_then_contained_then_letters_in_order() {
    assert_eq!(rank("tide", "Tidepool"), Some(0));
    assert_eq!(rank("", "Tidepool"), Some(0));
    assert_eq!(rank("POOL", "Tidepool"), Some(1));
    assert_eq!(rank("tdp", "Tidepool"), Some(2));
    assert_eq!(rank("t p", "Tidepool"), Some(2));
    assert_eq!(rank("pt", "Tidepool"), None);
    assert_eq!(rank("xyz", "Tidepool"), None);
}

#[test]
fn go_to_lists_projects_tabs_settings_and_actions_and_opens_over_home() {
    let mut ctx = ctx();
    ctx.store.projects = Load::Ready(vec![card("Tidepool", 0)]);
    let labels: Vec<String> = targets(&ctx)
        .into_iter()
        .map(|target| target.label)
        .collect();
    assert_eq!(labels[0], "Tidepool");
    assert!(labels.contains(&"Tidepool / Agents".to_owned()));
    assert!(labels.contains(&"Settings / Plugins and skills".to_owned()));
    assert!(labels.contains(&"New pool".to_owned()));

    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx);
    shell.key(Key::Ctrl('k'));
    assert!(matches!(shell.overlay, Some(Overlay::GoTo(_))));
    for character in "set pools".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    assert!(shell.overlay.is_none());
    assert_eq!(shell.depth(), 2);
    assert_eq!(shell.place(), Some(Place::Settings(SettingsSection::Pools)));
    assert_eq!(shell.stack[0].place(), Some(Place::Home));
}

#[test]
fn go_to_and_help_stay_closed_where_they_do_not_apply() {
    let mut shell = Shell::new(ctx());
    shell.key(Key::Ctrl('k'));
    assert!(
        shell.overlay.is_none(),
        "no Go to before the owner is set up"
    );

    let mut shell = at(Place::Welcome, ctx());
    shell.key(Key::Ctrl('k'));
    assert!(shell.overlay.is_none(), "no Go to on the first-run page");
    shell.key(Key::Char('?'));
    assert!(
        shell.overlay.is_none(),
        "? types into the focused name field"
    );

    let mut shell = at(Place::Home, ctx());
    shell.key(Key::Char('?'));
    assert!(matches!(shell.overlay, Some(Overlay::Help)));
    shell.key(Key::Char('x'));
    assert!(
        matches!(shell.overlay, Some(Overlay::Help)),
        "other keys keep it open"
    );
    shell.key(Key::Esc);
    assert!(shell.overlay.is_none());
}

#[test]
fn esc_goes_back_and_home_is_the_floor() {
    let mut shell = at(Place::Home, ctx());
    shell.key(Key::Char('s'));
    assert_eq!(shell.depth(), 2);
    assert_eq!(
        shell.place(),
        Some(Place::Settings(SettingsSection::Accounts))
    );
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home));
    shell.key(Key::Esc);
    assert_eq!((shell.depth(), shell.place()), (1, Some(Place::Home)));
    assert!(!shell.quitting());
}

#[test]
fn settings_sections_step_both_ways_and_wrap() {
    let mut shell = at(Place::Home, ctx());
    shell.key(Key::Char('s'));
    shell.key(Key::ShiftLeft);
    assert_eq!(
        shell.place(),
        Some(Place::Settings(SettingsSection::Profile))
    );
    shell.key(Key::Char(']'));
    assert_eq!(
        shell.place(),
        Some(Place::Settings(SettingsSection::Accounts))
    );
    shell.key(Key::ShiftRight);
    assert_eq!(shell.place(), Some(Place::Settings(SettingsSection::Pools)));
    assert_eq!(shell.depth(), 2, "sections replace each other");
}

/// DS-99 r2 (rv-sec2 F1): below 80x24 the shell draws no quit question, so one open there
/// takes no arrow or Enter, and Ctrl+Q or Ctrl+C still quits: at once when nothing runs,
/// past the hidden question when agents do.
#[test]
fn below_80x24_only_ctrl_q_acts_and_a_hidden_quit_question_takes_no_enter() {
    let mut shell = at(Place::Home, ctx());
    shell.resize(79, 24);
    shell.key(Key::Ctrl('q'));
    assert!(shell.quitting(), "nothing running, so no question");

    let mut ctx = ctx();
    ctx.store.projects = Load::Ready(vec![card("Tidepool", 1)]);
    let mut shell = at(Place::Home, ctx);
    shell.key(Key::Ctrl('q'));
    assert!(matches!(shell.overlay, Some(Overlay::Quit(_))));
    shell.resize(80, 23);
    for key in [Key::Right, Key::Enter, Key::Esc, Key::Tab] {
        shell.key(key);
    }
    assert!(!shell.quitting());
    assert!(matches!(&shell.overlay, Some(Overlay::Quit(ask)) if !ask.quit_focused));
    shell.key(Key::Ctrl('c'));
    assert!(
        shell.quitting(),
        "a second Ctrl-C quits past the hidden question"
    );
}

#[test]
fn quit_asks_only_when_agents_run_and_stay_is_the_default() {
    let mut shell = at(Place::Home, ctx());
    shell.key(Key::Ctrl('q'));
    assert!(shell.quitting(), "nothing running, so no question");

    let mut ctx = ctx();
    ctx.store.projects = Load::Ready(vec![card("Tidepool", 2), card("Ledgerline", 1)]);
    let mut shell = at(Place::Home, ctx);
    shell.key(Key::Ctrl('c'));
    match &shell.overlay {
        Some(Overlay::Quit(ask)) => assert_eq!((ask.running, ask.quit_focused), (3, false)),
        _ => panic!("quit asks first"),
    }
    shell.key(Key::Enter);
    assert!(
        !shell.quitting() && shell.overlay.is_none(),
        "Enter on Stay stays"
    );
    shell.key(Key::Ctrl('q'));
    shell.key(Key::Esc);
    assert!(!shell.quitting() && shell.overlay.is_none());
    shell.key(Key::Ctrl('q'));
    shell.key(Key::Right);
    shell.key(Key::Enter);
    assert!(shell.quitting());

    let mut ctx = self::ctx();
    ctx.store.projects = Load::Ready(vec![card("Tidepool", 1)]);
    let mut shell = at(Place::Home, ctx);
    shell.key(Key::Ctrl('q'));
    shell.key(Key::Ctrl('q'));
    assert!(shell.quitting(), "a second Ctrl-Q quits");

    let mut ctx = self::ctx();
    let mut team = card("Ledgerline", 0);
    team.activity = Activity {
        working: Some(2),
        ..Activity::default()
    };
    ctx.store.projects = Load::Ready(vec![team, card("Tidepool", 1)]);
    let mut shell = at(Place::Home, ctx);
    shell.key(Key::Ctrl('q'));
    match &shell.overlay {
        Some(Overlay::Quit(ask)) => assert_eq!(ask.running, 3),
        _ => panic!("team agents at work count as running"),
    }
}

#[test]
fn loading_lines_stay_empty_for_the_first_150_ms_of_a_busy_spell() {
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx());
    assert!(shell.busy(), "the profile is still loading");
    assert!(!shell.ctx.quiet);
    let start = Instant::now();
    assert!(shell.pace(start), "becoming busy starts the quiet spell");
    assert!(shell.ctx.quiet);
    assert_eq!(shell.ctx.loading("Loading your profile"), Line::default());
    assert!(!shell.pace(start + Duration::from_millis(149)));
    assert!(shell.ctx.quiet);
    assert!(shell.pace(start + Duration::from_millis(150)));
    assert!(!shell.ctx.quiet);
    assert_eq!(
        crate::tui::text::plain(&shell.ctx.loading("Loading your profile")),
        format!("{} Loading your profile", shell.ctx.spinner())
    );
    assert!(!shell.pace(start + Duration::from_millis(400)));
    assert!(!shell.ctx.quiet, "a long load keeps its spinner");

    shell.arrive(Arrival::Profile(Ok(Some(OwnerProfile {
        name: "Mira".into(),
        role: String::new(),
        bio: String::new(),
        preferred_stack: Vec::new(),
        working_style: Default::default(),
    }))));
    assert!(!shell.busy());
    assert!(!shell.pace(start + Duration::from_millis(500)));
    assert!(!shell.ctx.quiet);
    assert_eq!(
        shell.busy_since, None,
        "the next busy spell starts quiet again"
    );
}

#[test]
fn a_profile_read_failure_says_so_and_r_reads_it_again() {
    let mut shell = Shell::new(ctx());
    shell.arrive(Arrival::Connected(None));
    assert!(drain(&mut shell).contains(&"Profile".to_owned()));
    shell.arrive(Arrival::Profile(Err("owner_profile_read".into())));
    assert_eq!(
        shell.ctx.link,
        Link::Ready,
        "the engine answered, so it is not down"
    );
    assert!(matches!(shell.ctx.store.profile, Load::Failed(_)));
    assert!(!shell.busy(), "no spinner while waiting for the owner");
    assert_eq!(shell.place(), None);
    shell.key(Key::Char('r'));
    assert_eq!(drain(&mut shell), vec!["Profile".to_owned()]);
    assert!(shell.busy());
    shell.arrive(Arrival::Profile(Ok(None)));
    assert_eq!(shell.place(), Some(Place::Welcome));
}

#[test]
fn quit_on_welcome_asks_once_something_is_typed() {
    let mut shell = at(Place::Welcome, ctx());
    shell.key(Key::Ctrl('q'));
    assert!(shell.quitting(), "an empty welcome form quits at once");

    let mut shell = at(Place::Welcome, ctx());
    shell.key(Key::Char('M'));
    shell.key(Key::Ctrl('q'));
    match &shell.overlay {
        Some(Overlay::Quit(ask)) => {
            assert_eq!(ask.loss, Some("Your profile will not be saved."))
        }
        _ => panic!("typed profile asks first"),
    }
    shell.key(Key::Right);
    shell.key(Key::Enter);
    assert!(shell.quitting(), "Quit still quits");
}

#[test]
fn home_asks_for_harness_readiness_until_a_reply_fills_the_store() {
    use crate::harnesses::{HarnessEntry, Search, catalog};
    use crate::tui::client::OwnerEvent;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::Builder::new()
        .prefix("ca-hh-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let codex = root.path().join("codex");
    std::fs::write(&codex, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let entries = catalog(&Search::new(
        vec![root.path().to_path_buf()],
        None,
        Vec::new(),
    ));
    let reply = |result: Result<Vec<HarnessEntry>, String>| {
        Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
            SettingsEvent::Harnesses(result),
        ))))
    };

    let mut known = ctx();
    known.store.setup = Load::Ready(crate::tui::screens::home::tests::tools(&[]));
    known.store.accounts = Load::Ready(vec![crate::tui::seam::AccountRow {
        id: crate::protocol::CredentialRef::new("account-1").unwrap(),
        name: "Team key".into(),
        provider: "Anthropic API".into(),
        status: crate::accounts::AccountStatus::KeySaved.label().into(),
        no_window: None,
    }]);
    let mut shell = at(Place::Home, known);
    assert!(drain(&mut shell).contains(&"Settings".to_owned()));
    shell.arrive(reply(Err("controller_pending_intent".into())));
    assert_eq!(shell.ctx.store.harnesses, Load::NotReady);
    shell.arrive(reply(Ok(entries.clone())));
    assert_eq!(shell.ctx.store.harnesses, Load::Ready((1, entries.len())));

    shell.key(Key::Char('s'));
    drain(&mut shell);
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home));
    let requests = drain(&mut shell);
    assert!(requests.contains(&"Projects".to_owned()), "{requests:?}");
    assert!(
        !requests.contains(&"Settings".to_owned()),
        "known readiness is not asked for again"
    );
}

#[test]
fn quit_asks_while_a_new_project_draft_is_kept_after_leaving_the_wizard() {
    let mut shell = at(Place::Home, ctx());
    shell.key(Key::Char('n'));
    assert_eq!(shell.place(), Some(Place::NewProject));
    shell.key(Key::Esc);
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home));
    shell.key(Key::Ctrl('q'));
    assert!(shell.quitting(), "an empty wizard keeps no draft");

    let mut shell = at(Place::Home, ctx());
    shell.key(Key::Char('n'));
    for character in "Fieldnotes".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Esc);
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Home));
    assert_eq!(kept_draft().as_deref(), Some("Fieldnotes"));
    shell.key(Key::Ctrl('q'));
    match &shell.overlay {
        Some(Overlay::Quit(ask)) => assert_eq!(ask.loss, Some(DRAFT_LOSS)),
        _ => panic!("a kept draft asks first"),
    }
    assert!(!shell.quitting());
}

#[test]
fn skip_for_now_in_a_first_run_started_again_from_profile_leaves_one_home() {
    let mut shell = at(Place::Home, ctx());
    shell.apply(Flow::Open(Place::FirstRunAccounts));
    assert_eq!(shell.depth(), 2);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::Home));
    assert_eq!(shell.depth(), 1, "Skip for now goes back to the root Home");
}

#[test]
fn quit_asks_before_unsaved_profile_changes_are_lost() {
    let loaded = || {
        let mut ctx = ctx();
        ctx.store.profile = Load::Ready(Some(OwnerProfile {
            name: "Mira".into(),
            role: String::new(),
            bio: String::new(),
            preferred_stack: Vec::new(),
            working_style: Default::default(),
        }));
        at(Place::Settings(SettingsSection::Profile), ctx)
    };
    let mut shell = loaded();
    shell.key(Key::Ctrl('q'));
    assert!(shell.quitting(), "an untouched profile quits at once");

    let mut shell = loaded();
    shell.key(Key::Tab);
    shell.key(Key::Char('!'));
    shell.key(Key::Ctrl('q'));
    match &shell.overlay {
        Some(Overlay::Quit(ask)) => assert_eq!(
            (ask.running, ask.loss),
            (0, Some("Your profile changes will be lost."))
        ),
        _ => panic!("edited profile asks first"),
    }
    shell.key(Key::Esc);
    assert!(!shell.quitting());
    shell.key(Key::Backspace);
    shell.key(Key::Ctrl('q'));
    assert!(shell.quitting(), "undoing the edit leaves nothing to lose");

    let mut shell = loaded();
    shell.key(Key::Tab);
    shell.key(Key::Char('!'));
    for _ in 0..9 {
        shell.key(Key::Tab);
    }
    shell.key(Key::Enter);
    assert!(drain(&mut shell).contains(&"SaveProfile".to_owned()));
    shell.key(Key::Ctrl('q'));
    assert!(
        matches!(&shell.overlay, Some(Overlay::Quit(ask)) if ask.loss.is_some()),
        "a save still in flight can be lost"
    );
    shell.key(Key::Esc);
    shell.arrive(Arrival::ProfileSaved(Ok(())));
    shell.key(Key::Ctrl('q'));
    assert!(shell.quitting(), "a saved profile has nothing to lose");
}

#[test]
fn saving_the_profile_keeps_the_master_shift_arrows_choice_on_this_computer() {
    let folder = tempfile::tempdir().unwrap();
    let colors = folder.path().join("tui-colors");
    let mut ctx = ctx();
    ctx.prefs = Some(colors.clone());
    ctx.store.profile = Load::Ready(Some(OwnerProfile {
        name: "Mira".into(),
        role: String::new(),
        bio: String::new(),
        preferred_stack: Vec::new(),
        working_style: Default::default(),
    }));
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx);
    assert_eq!(shell.ctx.shift_arrows, ShiftArrows::SwitchTabs);
    for _ in 0..8 {
        shell.key(Key::Tab);
    }
    shell.key(Key::Right);
    shell.key(Key::Tab);
    shell.key(Key::Tab);
    shell.key(Key::Enter);
    assert!(drain(&mut shell).contains(&"SaveProfile".to_owned()));
    assert_eq!(
        shell.ctx.shift_arrows,
        ShiftArrows::SwitchTabs,
        "the choice applies once saved"
    );
    shell.arrive(Arrival::ProfileSaved(Ok(())));
    assert_eq!(shell.ctx.shift_arrows, ShiftArrows::SendToAgent);
    assert_eq!(
        crate::tui::prefs::load_shift_arrows(&crate::tui::prefs::shift_arrows_path(&colors)),
        ShiftArrows::SendToAgent
    );
}

#[test]
fn saving_the_profile_keeps_the_shift_enter_choice_on_this_computer() {
    let folder = tempfile::tempdir().unwrap();
    let colors = folder.path().join("tui-colors");
    let mut ctx = ctx();
    ctx.prefs = Some(colors.clone());
    ctx.store.profile = Load::Ready(Some(OwnerProfile {
        name: "Mira".into(),
        role: String::new(),
        bio: String::new(),
        preferred_stack: Vec::new(),
        working_style: Default::default(),
    }));
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx);
    assert_eq!(shell.ctx.shift_enter, ShiftEnter::Auto);
    for _ in 0..9 {
        shell.key(Key::Tab);
    }
    shell.key(Key::Right);
    shell.key(Key::Tab);
    shell.key(Key::Enter);
    assert!(drain(&mut shell).contains(&"SaveProfile".to_owned()));
    assert_eq!(
        shell.ctx.shift_enter,
        ShiftEnter::Auto,
        "the choice applies once saved"
    );
    shell.arrive(Arrival::ProfileSaved(Ok(())));
    assert_eq!(shell.ctx.shift_enter, ShiftEnter::Off);
    assert_eq!(
        crate::tui::prefs::load_shift_enter(&crate::tui::prefs::shift_enter_path(&colors)),
        ShiftEnter::Off
    );
    assert_eq!(
        crate::tui::prefs::load_shift_arrows(&crate::tui::prefs::shift_arrows_path(&colors)),
        ShiftArrows::SwitchTabs,
        "the other choice is kept as it was"
    );
}

#[test]
fn losing_the_engine_after_start_says_so_and_refreshes_on_return() {
    let mut shell = at(Place::Home, ctx());
    drain(&mut shell);
    shell.arrive(Arrival::Disconnected("trusted_connection_lost".into()));
    assert_eq!(shell.ctx.link, Link::Reconnecting);
    assert_eq!(
        shell.ctx.message().unwrap().lead,
        "Lost the connection to the engine."
    );
    shell.arrive(Arrival::Connected(None));
    assert_eq!(shell.ctx.link, Link::Ready);
    assert_eq!(
        shell.ctx.message().unwrap().lead,
        "Connected to the engine again."
    );
    let requests = drain(&mut shell);
    assert!(requests.contains(&"Projects".to_owned()), "{requests:?}");
    assert!(requests.contains(&"Pools".to_owned()), "{requests:?}");
}

/// RV-DS241-134 r2: a stored profile the engine read back with its name left blank by
/// the hidden-character cleaning opens Welcome, not Home, so the owner names it again.
#[test]
fn a_stored_profile_read_back_with_a_blank_name_opens_welcome() {
    let mut shell = Shell::new(ctx());
    shell.arrive(Arrival::Connected(None));
    let kept = OwnerProfile {
        name: String::new(),
        role: "Founder".into(),
        bio: "Builds tools.".into(),
        preferred_stack: vec!["Rust".into()],
        working_style: Default::default(),
    };
    shell.arrive(Arrival::Profile(Ok(Some(kept.clone()))));
    assert_eq!(shell.place(), Some(Place::Welcome));
    assert_eq!(shell.ctx.store.profile, Load::Ready(Some(kept)));
}

#[test]
fn startup_opens_welcome_on_first_run_and_home_afterwards() {
    let mut shell = Shell::new(Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        String::new(),
    ));
    shell.arrive(Arrival::Disconnected("engine_unavailable".into()));
    assert_eq!(shell.ctx.link, Link::Down("engine_unavailable".into()));
    assert_eq!(shell.place(), None);
    shell.key(Key::Char('r'));
    assert!(shell.ctx.take_reconnect());
    assert_eq!(shell.ctx.link, Link::Connecting);

    shell.arrive(Arrival::Connected(None));
    assert!(drain(&mut shell).contains(&"Profile".to_owned()));
    shell.arrive(Arrival::Profile(Ok(None)));
    assert_eq!(shell.place(), Some(Place::Welcome));
    assert_eq!(shell.depth(), 1);

    let mut shell = Shell::new(ctx());
    shell.arrive(Arrival::Connected(None));
    let profile = OwnerProfile {
        name: "Mira".into(),
        role: String::new(),
        bio: String::new(),
        preferred_stack: Vec::new(),
        working_style: Default::default(),
    };
    shell.arrive(Arrival::Profile(Ok(Some(profile))));
    assert_eq!(shell.place(), Some(Place::Home));
    assert!(matches!(shell.ctx.store.profile, Load::Ready(Some(_))));
    assert!(drain(&mut shell).contains(&"Projects".to_owned()));
}

fn extras_arrival(account: &str, count: u32, gained_ms: i64) -> Arrival {
    use crate::usage::extras::{Adapter, LimitExtras, Resets, Spend};
    let extras = LimitExtras {
        harness: "Codex".into(),
        adapter: Adapter::Codex,
        may_break: true,
        resets: Some(Resets {
            count,
            kinds: vec![],
            next_expiry_ms: None,
        }),
        credits: None,
        spend: Spend::InApp,
        not_reported: "not reported by Codex".into(),
        resets_checked_ms: Some(gained_ms),
        credits_checked_ms: None,
        resets_gained_ms: Some(gained_ms),
        resets_unread: None,
    };
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Extras(Ok(vec![crate::engine::owner::AccountExtras {
            account: crate::protocol::CredentialRef::new(account).unwrap(),
            extras,
        }])),
    ))))
}

fn watch_asks(shell: &mut Shell) -> (usize, usize) {
    let mut asks = (0, 0);
    while let Some(request) = shell.ctx.next_request() {
        if let Request::Settings(job) = request {
            match *job {
                SettingsJob::Extras { detail: false } => asks.0 += 1,
                SettingsJob::AllAccounts => asks.1 += 1,
                _ => {}
            }
        }
    }
    asks
}

/// DEC-154: the shell reads the stored extras at most once a minute from its 5 s tick,
/// never the detail rows, and says a new free reset once, naming the account. Profile
/// reads no accounts of its own, so every account read counted here is the watch's.
#[test]
fn the_shell_reads_the_stored_extras_once_a_minute_and_says_a_new_free_reset_once() {
    let mut ctx = ctx();
    ctx.store.accounts = Load::Ready(vec![crate::tui::seam::AccountRow {
        id: crate::protocol::CredentialRef::new("acct-a").unwrap(),
        name: "Work".into(),
        provider: "OpenAI".into(),
        status: "Ready".into(),
        no_window: None,
    }]);
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx);
    drain(&mut shell);
    shell.tick();
    assert_eq!(
        watch_asks(&mut shell),
        (1, 0),
        "names already loaded are not read again"
    );
    shell.tick();
    assert_eq!(
        watch_asks(&mut shell),
        (0, 0),
        "the 5 s tick never asks again so soon"
    );

    let gained = clock::now_ms() - 60_000;
    shell.arrive(extras_arrival("acct-a", 2, gained));
    let said = shell.ctx.message().expect("a new free reset is said");
    assert_eq!(
        (said.tone, said.lead.as_str()),
        (
            super::super::frame::Tone::Success,
            "Work has a new free reset. 2 free resets now."
        )
    );
    assert_eq!(said.rest, "See it in Settings, Pools.");

    shell.ctx.say(Message::success("Saved.", ""));
    shell.arrive(extras_arrival("acct-a", 2, gained));
    assert_eq!(
        shell.ctx.message().map(|said| said.lead.as_str()),
        Some("Saved."),
        "the same arrival is never said twice"
    );
    shell.arrive(extras_arrival("acct-a", 3, clock::now_ms()));
    assert_eq!(
        shell.ctx.message().map(|said| said.lead.as_str()),
        Some("Work has a new free reset. 3 free resets now.")
    );
}

/// The first watch read also asks for the account names when none are loaded, and an
/// account with no name yet is said as "An account", never by its id.
#[test]
fn the_reset_watch_asks_for_the_account_names_once_and_never_shows_an_id() {
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx());
    drain(&mut shell);
    shell.tick();
    assert_eq!(watch_asks(&mut shell), (1, 1));
    shell.arrive(extras_arrival("acct-a", 1, clock::now_ms()));
    assert_eq!(
        shell.ctx.message().map(|said| said.lead.as_str()),
        Some("An account has a new free reset. 1 free reset now.")
    );
}

/// A shell not yet connected asks for nothing.
#[test]
fn the_reset_watch_waits_for_the_engine() {
    let mut ctx = ctx();
    ctx.link = Link::Connecting;
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx);
    drain(&mut shell);
    shell.tick();
    assert_eq!(watch_asks(&mut shell), (0, 0));
}

fn shop(state: JobState) -> JobView {
    JobView {
        id: crate::protocol::RequestId::new("add-1").unwrap(),
        kind: crate::projects_home::job::JobKind::Add,
        project_id: None,
        source: "/Users/mira/Library/Mobile Documents/com~apple~CloudDocs/shop".into(),
        mode: crate::projects_home::AddMode::Move,
        overrides: Vec::new(),
        stopping: false,
        state,
    }
}

fn intake_arrival(query: IntakeQuery, result: Result<IntakeView, String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Intake(Box::new(IntakeEvent {
        query,
        result,
    }))))
}

fn status_arrival(job: Option<JobView>) -> Arrival {
    intake_arrival(IntakeQuery::JobStatus, Ok(IntakeView::Job(job)))
}

fn intake_asks(shell: &mut Shell) -> Vec<String> {
    use crate::tui::client::intake::IntakeJob;
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Intake(job) => match *job {
                IntakeJob::Query(query) => Some(format!("{query:?}")),
                IntakeJob::GitHubToken(_) => Some("GitHubToken".to_owned()),
            },
            _ => None,
        })
        .collect()
}

/// DEC-177 step 3b: between ticks the run loop reads the engine's intake job again only
/// while the stored one runs, so a New Project page that follows its job sees its end
/// soon; with no job or an ended one it asks nothing.
#[test]
fn a_running_intake_job_is_read_again_between_ticks_and_no_other_is() {
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx());
    drain(&mut shell);
    assert!(!shell.read_running_job(), "no job is stored");
    assert!(intake_asks(&mut shell).is_empty());

    shell.arrive(status_arrival(Some(shop(JobState::Copying {
        copied: 3,
        checked: 0,
    }))));
    drain(&mut shell);
    assert!(shell.read_running_job(), "the stored job runs");
    let asks = intake_asks(&mut shell);
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert!(asks[0].contains("JobStatus"), "{asks:?}");

    shell.arrive(status_arrival(Some(shop(JobState::Done))));
    drain(&mut shell);
    assert!(!shell.read_running_job(), "the stored job ended");
    assert!(intake_asks(&mut shell).is_empty());
}

/// DEC-159 S7: the tick reads the engine's intake job, the store keeps the last view
/// through a failed read, and a running job's end is said once, in its tone, with the
/// found folders read again so Home lists the folder that landed.
#[test]
fn the_shell_keeps_the_intake_job_and_says_its_end_once_then_reads_found_again() {
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx());
    drain(&mut shell);
    shell.tick();
    let asks = intake_asks(&mut shell);
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert!(asks[0].contains("JobStatus"), "{asks:?}");

    let running = shop(JobState::Copying {
        copied: 3,
        checked: 0,
    });
    shell.ctx.say(Message::success("Saved.", ""));
    shell.arrive(status_arrival(Some(running.clone())));
    assert_eq!(shell.ctx.store.intake.as_ref(), Some(&running));
    assert_eq!(
        shell.ctx.message().map(|said| said.lead.as_str()),
        Some("Saved.")
    );
    assert!(intake_asks(&mut shell).is_empty());

    let done = shop(JobState::Done);
    shell.arrive(status_arrival(Some(done.clone())));
    let said = shell.ctx.message().expect("the end is said");
    assert_eq!(
        (said.tone, said.lead.as_str()),
        (
            super::super::frame::Tone::Success,
            "shop is in your projects folder. Set it up from Home."
        )
    );
    let asks = intake_asks(&mut shell);
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert!(asks[0].contains("Found"), "{asks:?}");

    shell.ctx.say(Message::success("Saved.", ""));
    shell.arrive(status_arrival(Some(done.clone())));
    shell.arrive(intake_arrival(
        IntakeQuery::JobStatus,
        Err("owner_timeout".into()),
    ));
    assert_eq!(
        shell.ctx.message().map(|said| said.lead.as_str()),
        Some("Saved.")
    );
    assert_eq!(
        shell.ctx.store.intake.as_ref(),
        Some(&done),
        "a failed read keeps it"
    );
    assert!(intake_asks(&mut shell).is_empty());

    shell.arrive(intake_arrival(
        IntakeQuery::JobDismiss {
            id: done.id.clone(),
        },
        Ok(IntakeView::JobDismissed(true)),
    ));
    let asks = intake_asks(&mut shell);
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert!(asks[0].contains("JobStatus"), "{asks:?}");
    shell.arrive(status_arrival(None));
    assert_eq!(shell.ctx.store.intake, None);
}

/// A job that ends failed, stopped or asking for an answer is said as a warning, and a
/// stop asked from any screen reads the job again at once.
#[test]
fn an_intake_job_that_did_not_land_is_said_as_a_warning_and_a_stop_reads_it_again() {
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx());
    let running = shop(JobState::Downloading {
        progress: crate::projects_home::icloud::Progress { total: 3, done: 1 },
    });
    shell.arrive(status_arrival(Some(running.clone())));
    shell.arrive(intake_arrival(
        IntakeQuery::JobStop {
            id: running.id.clone(),
        },
        Ok(IntakeView::JobStopped(true)),
    ));
    drain(&mut shell);
    let ended = [
        JobState::Cancelled {
            phase: crate::projects_home::job::Phase::Downloading,
        },
        JobState::NeedsAnswer {
            code: "home_move_worktrees".into(),
        },
        JobState::Failed {
            code: "home_move_source_left".into(),
            phase: crate::projects_home::job::Phase::Clearing,
        },
    ];
    for state in ended {
        shell.arrive(status_arrival(Some(running.clone())));
        shell.arrive(status_arrival(Some(shop(state.clone()))));
        let said = shell.ctx.message().expect("the end is said");
        assert_eq!(said.tone, super::super::frame::Tone::Warning, "{state:?}");
    }
}

/// The header shows the running intake job's line in front of the screen's status on
/// every screen, and is left unchanged once the job ended or with none.
#[test]
fn the_header_shows_the_running_intake_job_and_nothing_once_it_ended() {
    let theme = ctx().theme.clone();
    let running = shop(JobState::Downloading {
        progress: crate::projects_home::icloud::Progress {
            total: 3000,
            done: 12,
        },
    });
    let shown = |job: Option<&JobView>, status: &str| {
        let status = Line::raw(status.to_owned());
        crate::tui::text::plain(&with_intake(job, status, &theme, &UNICODE))
    };
    assert_eq!(
        shown(Some(&running), "Ready"),
        "Downloading shop: 12 of 3000  Ready"
    );
    assert_eq!(shown(Some(&running), ""), "Downloading shop: 12 of 3000");
    assert_eq!(shown(Some(&shop(JobState::Done)), "Ready"), "Ready");
    assert_eq!(shown(None, "Ready"), "Ready");
    assert_eq!(shown(None, ""), "");
    let long = JobView {
        source: format!("/Users/mira/{}", "a".repeat(200)),
        ..running.clone()
    };
    let note = shown(Some(&long), "");
    assert!(
        crate::tui::text::width(&note) <= JOB_NOTE_CELLS_MAX,
        "{note}"
    );
    assert!(note.starts_with("Downloading aaa"), "{note}");
    assert!(note.ends_with(": 12 of 3000"), "{note}");
    assert!(note.contains(UNICODE.ellipsis), "{note}");
    assert!(shown(Some(&long), "Ready").ends_with("  Ready"));
}

fn tidepool() -> ProjectId {
    ProjectId::new("project-tidepool").unwrap()
}

fn sub(id: &str, label: &str, status: AgentStatus) -> GraphNode {
    GraphNode {
        id: id.into(),
        kind: NodeKind::Sub,
        parent: Some("master".into()),
        label: label.into(),
        harness: "codex".into(),
        status: Some(status),
        status_label: None,
        account: None,
        branch: None,
        terminal: None,
        questions: None,
        agent: None,
        pane: None,
        presence: None,
        waiting: None,
        waits_for: None,
        owner_questions: 0,
        no_worktree: false,
        hookless: false,
        message_note: None,
        started_ms: None,
        sees_hidden: false,
    }
}

fn tidepool_team() -> TeamGraph {
    TeamGraph {
        project_id: tidepool(),
        nodes: vec![
            GraphNode {
                kind: NodeKind::Master,
                parent: None,
                ..sub("master", "Codex", AgentStatus::Working)
            },
            GraphNode {
                kind: NodeKind::Lead,
                ..sub("team-1", "Ada", AgentStatus::Working)
            },
            sub("team-2", "Grace", AgentStatus::Idle),
            sub("team-3", "Linus", AgentStatus::Stopped),
            sub("team-4", "Hopper", AgentStatus::Starting),
            GraphNode {
                kind: NodeKind::Teammate,
                ..sub("teammate-4-12", "Teammate %12", AgentStatus::Working)
            },
        ],
        edges: Vec::new(),
        messages: Vec::new(),
        overlaps: Vec::new(),
        relations: Vec::new(),
        master_live_terminal: true,
    }
}

fn tidepool_page() -> Shell {
    let mut ctx = ctx();
    ctx.store.projects = Load::Ready(vec![card("Tidepool", 0)]);
    let mut shell = at(
        Place::Project {
            id: tidepool(),
            name: "Tidepool".into(),
            tab: ProjectTab::Overview,
        },
        ctx,
    );
    drain(&mut shell);
    shell
}

fn typed(shell: &mut Shell, text: &str) {
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
}

fn go_to_rows(shell: &Shell) -> Vec<(String, &'static str)> {
    match &shell.overlay {
        Some(Overlay::GoTo(go)) => go
            .matches(&shell.ctx)
            .into_iter()
            .map(|target| (target.label, target.kind))
            .collect(),
        _ => panic!("Go to is not open"),
    }
}

fn files_asked(shell: &mut Shell) -> Vec<String> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::ProjectFiles { project, query } if project == tidepool() => Some(query),
            _ => None,
        })
        .collect()
}

fn files(query: &str, paths: &[&str]) -> Arrival {
    Arrival::ProjectFiles {
        project: tidepool(),
        query: query.into(),
        result: Ok(paths.iter().map(|path| (*path).to_owned()).collect()),
    }
}

#[test]
fn go_to_lists_the_live_role_sub_agents_of_the_team_graph_leads_included() {
    let listed = go_to_agents(&tidepool_team());
    let listed: Vec<(&str, &str)> = listed
        .iter()
        .map(|(node, label)| (node.as_str(), label.as_str()))
        .collect();
    assert_eq!(
        listed,
        [("team-1", "Ada"), ("team-2", "Grace"), ("team-4", "Hopper")]
    );
}

#[test]
fn go_to_on_a_project_page_reads_its_agents_and_files_only_while_open() {
    let mut shell = tidepool_page();
    shell.key(Key::Ctrl('k'));
    let asked: Vec<Request> = std::iter::from_fn(|| shell.ctx.next_request()).collect();
    assert!(asked.iter().any(
        |request| matches!(request, Request::TeamGraph { project } if *project == tidepool())
    ));
    assert!(asked.iter().any(|request| matches!(
        request,
        Request::ProjectFiles { project, query } if *project == tidepool() && query.is_empty()
    )));
    shell.arrive(Arrival::TeamGraph {
        project: tidepool(),
        result: Ok(Box::new(tidepool_team())),
    });
    shell.arrive(files("", &["src/main.rs", "README.md"]));
    let rows = go_to_rows(&shell);
    assert_eq!(rows.len(), GO_TO_ROWS_MAX);
    assert_eq!(rows[ProjectTab::ALL.len()], ("Ada".to_owned(), "Agent"));

    typed(&mut shell, "main");
    assert_eq!(files_asked(&mut shell), ["m"], "one read at a time");
    shell.arrive(files("ma", &["docs/main-notes.md"]));
    assert!(
        !go_to_rows(&shell)
            .iter()
            .any(|(label, _)| label == "docs/main-notes.md"),
        "a read Go to did not ask for is not listed"
    );
    shell.arrive(files("m", &["src/main.rs"]));
    assert_eq!(files_asked(&mut shell), ["main"], "the query typed since");
    shell.arrive(files("main", &["src/main.rs", "src/domain.rs"]));
    let rows = go_to_rows(&shell);
    assert!(rows.contains(&("src/main.rs".to_owned(), "File")));
    assert!(rows.contains(&("src/domain.rs".to_owned(), "File")));
    assert!(files_asked(&mut shell).is_empty());

    shell.key(Key::Backspace);
    assert_eq!(files_asked(&mut shell), ["mai"]);
    shell.key(Key::Esc);
    assert!(shell.overlay.is_none());
    shell.arrive(files("mai", &["src/main.rs"]));
    assert!(
        files_asked(&mut shell).is_empty(),
        "a closed Go to reads nothing more"
    );
}

#[test]
fn go_to_off_a_project_page_reads_no_agents_or_files() {
    let mut shell = at(Place::Settings(SettingsSection::Profile), ctx());
    drain(&mut shell);
    shell.key(Key::Ctrl('k'));
    assert!(matches!(shell.overlay, Some(Overlay::GoTo(_))));
    let asked = drain(&mut shell);
    assert!(
        !asked
            .iter()
            .any(|request| request.starts_with("TeamGraph") || request.starts_with("ProjectFiles")),
        "{asked:?}"
    );
}

#[test]
fn go_to_asks_the_ide_for_a_picked_file_and_stays_on_the_page() {
    let mut shell = tidepool_page();
    shell.key(Key::Ctrl('k'));
    shell.arrive(files("", &["src/main.rs"]));
    typed(&mut shell, "src/main");
    assert_eq!(go_to_rows(&shell)[0], ("src/main.rs".to_owned(), "File"));
    drain(&mut shell);
    shell.key(Key::Enter);
    assert!(shell.overlay.is_none());
    assert_eq!(shell.depth(), 1);
    assert_eq!(
        shell.place(),
        Some(Place::Project {
            id: tidepool(),
            name: "Tidepool".into(),
            tab: ProjectTab::Overview,
        })
    );
    let wanted = RevealQuery {
        project: tidepool(),
        target: RevealTarget::Project {
            path: "src/main.rs".into(),
        },
    };
    let asked: Vec<Request> = std::iter::from_fn(|| shell.ctx.next_request()).collect();
    assert!(
        asked
            .iter()
            .any(|request| matches!(request, Request::Reveal(query) if **query == wanted)),
        "{asked:?}"
    );
}

fn messages_read(shell: &mut Shell) -> Vec<i64> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Setup(crate::project_setup::SetupQuery::TeamMessages {
                project,
                agent,
                ..
            }) if project == tidepool() => Some(agent),
            _ => None,
        })
        .collect()
}

fn grace_picked() -> Shell {
    let mut shell = tidepool_page();
    shell.key(Key::Ctrl('k'));
    shell.arrive(Arrival::TeamGraph {
        project: tidepool(),
        result: Ok(Box::new(tidepool_team())),
    });
    typed(&mut shell, "grace");
    assert_eq!(go_to_rows(&shell)[0], ("Grace".to_owned(), "Agent"));
    shell.key(Key::Enter);
    assert!(shell.overlay.is_none());
    shell
}

#[test]
fn go_to_opens_the_team_on_a_picked_agent_with_its_messages_open() {
    let mut shell = grace_picked();
    assert_eq!(
        shell.place(),
        Some(Place::Team {
            id: tidepool(),
            name: "Tidepool".into(),
        })
    );
    assert_eq!(
        shell.ctx.messages_for, None,
        "the Team screen took the agent Go to picked"
    );
    assert_eq!(messages_read(&mut shell), Vec::<i64>::new());
    shell.arrive(Arrival::TeamGraph {
        project: tidepool(),
        result: Ok(Box::new(tidepool_team())),
    });
    assert_eq!(
        messages_read(&mut shell),
        [2],
        "Grace's Messages list opens"
    );
    shell.key(Key::Esc);
    drain(&mut shell);
    shell.key(Key::Char('m'));
    assert_eq!(
        messages_read(&mut shell),
        [2],
        "the tree cursor sits on Grace's card"
    );
}

#[test]
fn a_picked_agent_gone_from_the_team_is_said_and_opens_no_messages() {
    let mut shell = grace_picked();
    let mut gone = tidepool_team();
    gone.nodes.retain(|node| node.id != "team-2");
    shell.arrive(Arrival::TeamGraph {
        project: tidepool(),
        result: Ok(Box::new(gone)),
    });
    assert_eq!(messages_read(&mut shell), Vec::<i64>::new());
    let said = shell
        .ctx
        .message()
        .expect("the Team screen says the agent left");
    assert_eq!(said.lead, "That agent is no longer on the team.");
    shell.arrive(Arrival::TeamGraph {
        project: tidepool(),
        result: Ok(Box::new(tidepool_team())),
    });
    assert_eq!(
        messages_read(&mut shell),
        Vec::<i64>::new(),
        "the pick is used once"
    );
}

#[test]
fn a_team_screen_leaves_an_agent_picked_for_another_project() {
    let other = ProjectId::new("project-harbor").unwrap();
    let mut ctx = ctx();
    ctx.messages_for = Some((other.clone(), "team-2".into()));
    let shell = at(
        Place::Team {
            id: tidepool(),
            name: "Tidepool".into(),
        },
        ctx,
    );
    assert_eq!(shell.ctx.messages_for, Some((other, "team-2".into())));
}

#[test]
fn a_go_to_file_row_keeps_the_end_of_a_long_path() {
    let long = "crates/crazyagent/src/tui/shell/tests.rs";
    let ellipsis = UNICODE.ellipsis;
    assert_eq!(path_tail("src/main.rs", 32, ellipsis), "src/main.rs");
    assert_eq!(
        path_tail(long, 20, ellipsis),
        format!(
            "{ellipsis}{}",
            &long[long.len() - (20 - ellipsis.chars().count())..]
        )
    );
    let mut shell = tidepool_page();
    shell.key(Key::Ctrl('k'));
    shell.arrive(files("", &[long]));
    typed(&mut shell, "tests.rs");
    let Some(Overlay::GoTo(go)) = &shell.overlay else {
        panic!("Go to is not open");
    };
    let (modal, _) = shell.go_to_modal(go);
    let text: Vec<String> = modal
        .body
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    assert!(
        text.iter()
            .any(|line| line.contains("shell/tests.rs") && line.contains("File")),
        "{text:?}"
    );
}

#[test]
fn go_to_keeps_its_selection_on_a_row_when_a_read_shrinks_the_list() {
    let mut shell = tidepool_page();
    shell.key(Key::Ctrl('k'));
    shell.arrive(files("", &[]));
    typed(&mut shell, "zzq");
    shell.arrive(files("z", &["zzq/one.rs", "zzq/two.rs"]));
    assert_eq!(go_to_rows(&shell).len(), 2);
    shell.key(Key::Down);
    shell.arrive(files("zzq", &["zzq/one.rs"]));
    assert_eq!(go_to_rows(&shell), [("zzq/one.rs".to_owned(), "File")]);
    let Some(Overlay::GoTo(go)) = &shell.overlay else {
        panic!("Go to is not open");
    };
    assert_eq!(go.selected, 0);
}
