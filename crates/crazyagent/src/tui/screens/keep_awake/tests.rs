use super::words::{self, Form, LidOffers, LidPlace, Tone};
use crate::keep_awake::{
    BatteryFloor, Cap, Decision, EndReason, HeatEnd, KeepAwakeAction, KeepAwakeSettings,
    KeepAwakeView, LidRule, LidSupport, Mode, Reason, Want,
};
use crate::paths::Platform;
use crate::tui::client::OwnerEvent;
use crate::tui::client::keep_awake::{KeepAwakeEvent, KeepAwakeQuery, KeepAwakeReply, Picks};
use crate::tui::keys::Key;
use crate::tui::render_tests::{check, ready, render, shell_at};
use crate::tui::screens::Place;
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::Shell;

const NOW_MS: i64 = 1_800_000_000_000;
const MINUTE_MS: i64 = 60_000;

/// Keep awake in mode Auto with nothing running and nothing held, on a Mac whose lid rule
/// is off and whose low power mode is ready. Input: none. Output: the view.
pub(in crate::tui) fn resting() -> KeepAwakeView {
    KeepAwakeView {
        settings: KeepAwakeSettings::DEFAULT,
        version: 0,
        state: Decision {
            want: Want::Nothing,
            reason: Reason::Idle,
            ends_at_ms: None,
            ended: None,
        },
        lid_support: LidSupport::Off,
        lid_platform: LidSupport::HelperMissing,
        platform: Platform::MacOs,
        work: 0,
        freeze_apps: Vec::new(),
        low_power: crate::keep_awake::LowPowerSupport::Ready,
    }
}

/// Keep awake holding for running agents, without the lid. Input: how many agents run.
/// Output: the view.
pub(in crate::tui) fn working(work: u32) -> KeepAwakeView {
    assert!(work > 0);
    let mut view = resting();
    view.work = work;
    view.state.want = Want::Idle { display: false };
    view.state.reason = Reason::Work;
    view
}

/// Keep awake holding on a manual timer. Input: the end in milliseconds. Output: the view.
pub(in crate::tui) fn timer(ends_at_ms: i64) -> KeepAwakeView {
    let mut view = resting();
    view.state.want = Want::Idle { display: false };
    view.state.reason = Reason::Timer;
    view.state.ends_at_ms = Some(ends_at_ms);
    view
}

/// Keep awake holding for three agents with the lid covered, the state the screen frames
/// show. Input: none. Output: the view.
pub(in crate::tui) fn lid_held() -> KeepAwakeView {
    let mut view = working(3);
    view.state.want = Want::IdleAndLid { display: false };
    view.lid_support = LidSupport::Held;
    view
}

fn ended(reason: EndReason) -> KeepAwakeView {
    let mut view = resting();
    view.state.ended = Some(reason);
    view
}

fn limited(lid: LidSupport) -> KeepAwakeView {
    let mut view = working(3);
    view.lid_support = lid;
    view
}

fn with_mode(mode: Mode) -> KeepAwakeView {
    let mut view = resting();
    view.settings.mode = mode;
    view
}

fn labels(view: &KeepAwakeView) -> [String; 3] {
    [Form::Words, Form::Header, Form::Short].map(|form| words::label(view, NOW_MS, form))
}

#[test]
fn every_state_has_the_boards_words_in_each_form_within_the_header_bound() {
    let mut paused = resting();
    paused.work = 2;
    let mut grace = timer(NOW_MS + 8 * MINUTE_MS);
    grace.state.reason = Reason::Grace;
    let cases: Vec<(KeepAwakeView, [&str; 3])> = vec![
        (
            resting(),
            [
                "Auto, nothing running",
                "Keep awake: auto, nothing running",
                "Auto",
            ],
        ),
        (
            paused,
            [
                "Paused until work ends",
                "Keep awake: paused until work ends",
                "Paused",
            ],
        ),
        (
            with_mode(Mode::Manual),
            ["Manual, no timer", "Keep awake: manual, no timer", "Manual"],
        ),
        (
            with_mode(Mode::Off),
            ["Off", "Keep awake: off", "Awake off"],
        ),
        (
            working(3),
            [
                "Awake while 3 agents run",
                "Awake while 3 agents run",
                "Awake: 3 agents",
            ],
        ),
        (
            working(1),
            [
                "Awake while 1 agent runs",
                "Awake while 1 agent runs",
                "Awake: 1 agent",
            ],
        ),
        (
            working(1200),
            [
                "Awake while 999+ agents run",
                "Awake while 999+ agents run",
                "Awake: 999+ agents",
            ],
        ),
        (
            timer(NOW_MS + 134 * MINUTE_MS),
            ["Awake, 2h 14m left", "Awake, 2h 14m left", "Awake 2h14m"],
        ),
        (
            grace,
            [
                "Awake 8m more, work done",
                "Awake 8m more, work done",
                "Awake 8m more",
            ],
        ),
        (lid_held(), ["Awake, lid OK"; 3]),
        (
            limited(LidSupport::IdleOnlyOnBattery),
            [
                "Idle only: on battery",
                "Idle only: on battery",
                "Idle only",
            ],
        ),
        (
            limited(LidSupport::NeedsMacos13),
            [
                "Lid-closed hold needs macOS 13",
                "Lid-closed hold needs macOS 13",
                "Idle only (macOS 13)",
            ],
        ),
        (limited(LidSupport::NeedsApproval), ["Needs approval"; 3]),
        (
            ended(EndReason::Battery { pct: 9 }),
            ["Ended: battery 9%", "Ended: battery 9%", "Ended: batt 9%"],
        ),
        (
            ended(EndReason::BatteryUnread),
            [
                "Ended: battery unreadable",
                "Ended: battery unreadable",
                "Ended: batt ?",
            ],
        ),
        (ended(EndReason::Heat), ["Ended: heat"; 3]),
        (
            ended(EndReason::Cap { hours: 24 }),
            ["Ended: 24 h limit", "Ended: 24 h limit", "Ended: limit"],
        ),
        (
            ended(EndReason::HoldLost),
            ["Ended: hold lost", "Ended: hold lost", "Ended: lost"],
        ),
        (
            ended(EndReason::Manual),
            [
                "Auto, nothing running",
                "Keep awake: auto, nothing running",
                "Auto",
            ],
        ),
    ];
    for (view, want) in cases {
        let got = labels(&view);
        assert_eq!(got, want.map(str::to_owned), "{view:?}");
        for label in got {
            let length = label.chars().count();
            assert!(length <= words::LABEL_CHARS_MAX, "{label}");
        }
    }
}

#[test]
fn the_time_left_rounds_up_to_the_minute_and_never_says_zero() {
    let left = |ms: i64, form| words::time_left(Some(NOW_MS + ms), NOW_MS, form);
    assert_eq!(left(1, Form::Words), "1m");
    assert_eq!(left(-5 * MINUTE_MS, Form::Words), "1m", "a passed end");
    assert_eq!(left(59 * MINUTE_MS, Form::Words), "59m");
    assert_eq!(left(59 * MINUTE_MS + 1, Form::Words), "1h");
    assert_eq!(left(180 * MINUTE_MS, Form::Short), "3h");
    assert_eq!(left(134 * MINUTE_MS, Form::Words), "2h 14m");
    assert_eq!(left(134 * MINUTE_MS, Form::Header), "2h 14m");
    assert_eq!(left(134 * MINUTE_MS, Form::Short), "2h14m");
    assert_eq!(left(i64::MAX / 2, Form::Short), "999h59m", "a far end");
    assert_eq!(words::time_left(None, NOW_MS, Form::Words), "1m");
}

#[test]
fn the_tone_tells_a_hold_a_lid_limit_an_end_and_rest_apart() {
    assert_eq!(words::tone(&resting()), Tone::Plain);
    assert_eq!(words::tone(&with_mode(Mode::Off)), Tone::Plain);
    assert_eq!(words::tone(&working(3)), Tone::Hold);
    assert_eq!(words::tone(&lid_held()), Tone::Hold);
    assert_eq!(words::tone(&timer(NOW_MS + MINUTE_MS)), Tone::Hold);
    assert_eq!(
        words::tone(&limited(LidSupport::IdleOnlyOnBattery)),
        Tone::Limit
    );
    assert_eq!(
        words::tone(&limited(LidSupport::NeedsApproval)),
        Tone::Limit
    );
    assert_eq!(words::tone(&ended(EndReason::Heat)), Tone::Ended);
    assert_eq!(words::tone(&ended(EndReason::Manual)), Tone::Plain);
    let mut resting_limit = resting();
    resting_limit.lid_support = LidSupport::IdleOnlyOnBattery;
    assert_eq!(
        words::tone(&resting_limit),
        Tone::Plain,
        "a limit only shows while holding"
    );
}

#[test]
fn ends_by_itself_lists_the_limits_that_are_on() {
    let all = KeepAwakeSettings::DEFAULT;
    assert_eq!(
        words::ends_by_itself(&all),
        "At 10% battery, critical heat, or 24 h"
    );
    let two = KeepAwakeSettings {
        heat_end: HeatEnd::Never,
        ..all
    };
    assert_eq!(words::ends_by_itself(&two), "At 10% battery or 24 h");
    let one = KeepAwakeSettings {
        battery_floor: BatteryFloor::Off,
        cap: Cap::None,
        heat_end: HeatEnd::Serious,
        ..all
    };
    assert_eq!(words::ends_by_itself(&one), "At serious heat");
    let none = KeepAwakeSettings {
        heat_end: HeatEnd::Never,
        ..one
    };
    assert_eq!(words::ends_by_itself(&none), "Never");
}

#[test]
fn the_lid_line_says_what_a_closed_lid_does_now() {
    assert_eq!(words::lid_now(&resting()), "Sleep as usual");
    assert_eq!(words::lid_now(&lid_held()), "Hold on AC power only");
    let mut always = lid_held();
    always.settings.lid = LidRule::Always;
    assert_eq!(words::lid_now(&always), "Hold always");
    assert_eq!(
        words::lid_now(&limited(LidSupport::IdleOnlyOnBattery)),
        "Idle only: on battery"
    );
    assert_eq!(
        words::lid_now(&limited(LidSupport::HelperMissing)),
        "Not set up on this computer yet"
    );
    let reason = "r".repeat(300);
    let cut = words::lid_now(&limited(LidSupport::NotAvailable { reason }));
    assert_eq!(cut.chars().count(), 200, "a long reason is cut");
    assert_eq!(
        words::lid_now(&limited(LidSupport::NotTurnedOn)),
        "Not turned on yet"
    );
    let reason = "The lid helper belongs to another account on this Mac".to_string();
    assert_eq!(
        words::lid_now(&limited(LidSupport::RemoveHelper {
            reason: reason.clone()
        })),
        reason,
        "a helper to remove says why"
    );
    let reason = "r".repeat(300);
    let cut = words::lid_now(&limited(LidSupport::RemoveHelper { reason }));
    assert_eq!(cut.chars().count(), 200, "a long remove reason is cut");
    assert_eq!(
        words::lid_now(&limited(LidSupport::SleepNotRestored)),
        "Sleep not restored: see Notifications"
    );
}

#[test]
fn custom_minutes_are_whole_and_from_1_to_1440() {
    assert_eq!(words::custom_minutes("1"), Some(1));
    assert_eq!(words::custom_minutes("90"), Some(90));
    assert_eq!(words::custom_minutes(" 1440 "), Some(1440));
    for wrong in ["", "0", "1441", "9999", "12345", "1.5", "-5", "ten", "1 0"] {
        assert_eq!(words::custom_minutes(wrong), None, "{wrong:?}");
    }
}

#[test]
fn picks_lie_over_the_stored_settings_and_keep_every_other_setting() {
    let stored = KeepAwakeSettings {
        cap: Cap::H4,
        lid: LidRule::Always,
        ..KeepAwakeSettings::DEFAULT
    };
    assert_eq!(Picks::default().over(stored), stored, "no pick, no change");
    let picks = Picks {
        mode: Some(Mode::Manual),
        heat_end: Some(HeatEnd::Never),
        ..Picks::default()
    };
    let want = KeepAwakeSettings {
        mode: Mode::Manual,
        heat_end: HeatEnd::Never,
        ..stored
    };
    assert_eq!(picks.over(stored), want);
    assert_eq!(picks.over(want), want, "a pick equal to the stored option");
}

fn screen(view: KeepAwakeView) -> Shell {
    let mut ctx = ready(80, 24);
    ctx.store.keep_awake = Some(view);
    shell_at(ctx, Place::KeepAwake)
}

fn changes(shell: &mut Shell) -> Vec<KeepAwakeQuery> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::KeepAwake(query) if *query != KeepAwakeQuery::Read => Some(*query),
            _ => None,
        })
        .collect()
}

fn answer(shell: &mut Shell, query: KeepAwakeQuery, result: Result<KeepAwakeReply, String>) {
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::KeepAwake(Box::new(
        KeepAwakeEvent { query, result },
    )))));
}

fn is_read(request: &Request) -> bool {
    matches!(request, Request::KeepAwake(query) if **query == KeepAwakeQuery::Read)
}

fn shown(shell: &mut Shell) -> String {
    render(shell, 80, 24).join("\n")
}

/// The words of the Lid closed line under State, with each line wrapped under its value
/// column joined on by one space. Input: the shown text. Output: the words, empty when
/// the screen shows no such line.
fn lid_now_shown(text: &str) -> String {
    let column = " ".repeat(usize::from(super::GUTTER) + super::LABEL_WIDTH);
    let mut lines = text
        .lines()
        .skip_while(|line| !line.starts_with("  Lid closed "));
    let Some(first) = lines
        .next()
        .and_then(|line| line.strip_prefix("  Lid closed"))
    else {
        return String::new();
    };
    let mut words = vec![first.trim()];
    for line in lines {
        match line.strip_prefix(column.as_str()) {
            Some(more) if !more.starts_with(' ') && !more.trim().is_empty() => {
                words.push(more.trim());
            }
            _ => break,
        }
    }
    words.join(" ")
}

/// Presses one key and returns the single change it sent, answered as applied so the
/// next key is taken. Inputs: the shell and the key. Output: the change.
fn pressed(shell: &mut Shell, key: char) -> KeepAwakeQuery {
    shell.key(Key::Char(key));
    let mut sent = changes(shell);
    assert_eq!(sent.len(), 1, "{key} sends one change: {sent:?}");
    let query = sent.remove(0);
    answer(shell, query.clone(), Ok(KeepAwakeReply::Done));
    query
}

fn action(query: KeepAwakeQuery) -> KeepAwakeAction {
    match query {
        KeepAwakeQuery::Act { action, generation } => {
            assert!(generation > 0);
            action
        }
        other => panic!("not an action: {other:?}"),
    }
}

fn picks(query: KeepAwakeQuery) -> Picks {
    match query {
        KeepAwakeQuery::Save { picks, generation } => {
            assert!(generation > 0);
            picks
        }
        other => panic!("not a save: {other:?}"),
    }
}

#[test]
fn the_timer_keys_send_one_change_each() {
    let mut shell = screen(timer(crate::tui::clock::now_ms() + 30 * MINUTE_MS));
    assert_eq!(
        action(pressed(&mut shell, '1')),
        KeepAwakeAction::Start { minutes: 60 }
    );
    assert_eq!(
        action(pressed(&mut shell, '4')),
        KeepAwakeAction::Start { minutes: 240 }
    );
    assert_eq!(
        action(pressed(&mut shell, 'e')),
        KeepAwakeAction::Extend { minutes: 60 }
    );
    assert_eq!(action(pressed(&mut shell, 'x')), KeepAwakeAction::End);
}

#[test]
fn a_second_key_waits_until_the_first_change_is_answered() {
    let mut shell = screen(resting());
    shell.key(Key::Char('1'));
    shell.key(Key::Char('4'));
    let sent = changes(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(shown(&mut shell).contains("Saving"));
    answer(&mut shell, sent[0].clone(), Ok(KeepAwakeReply::Done));
    assert!(!shown(&mut shell).contains("Saving"));
    let reads: Vec<Request> = std::iter::from_fn(|| shell.ctx.next_request()).collect();
    assert!(
        reads.iter().any(is_read),
        "an answered change reads the view again: {reads:?}"
    );
}

#[test]
fn m_and_l_save_the_next_option_at_once_as_one_pick() {
    let mut shell = screen(resting());
    let want = Picks {
        mode: Some(Mode::Manual),
        ..Picks::default()
    };
    assert_eq!(picks(pressed(&mut shell, 'm')), want);
    let want = Picks {
        lid: Some(LidRule::Always),
        ..Picks::default()
    };
    assert_eq!(picks(pressed(&mut shell, 'l')), want);
    let mut last = with_mode(Mode::Off);
    last.settings.lid = LidRule::Off;
    let mut shell = screen(last);
    assert_eq!(picks(pressed(&mut shell, 'm')).mode, Some(Mode::Auto));
    assert_eq!(picks(pressed(&mut shell, 'l')).lid, Some(LidRule::AcOnly));
}

#[test]
fn extend_without_a_timer_and_end_without_a_hold_send_nothing_and_say_why() {
    let mut shell = screen(working(2));
    shell.key(Key::Char('e'));
    assert_eq!(changes(&mut shell), Vec::new());
    assert!(shown(&mut shell).contains("Extend works while a timer runs. Start one first."));
    let mut shell = screen(resting());
    shell.key(Key::Char('x'));
    assert_eq!(changes(&mut shell), Vec::new());
    assert!(shown(&mut shell).contains("Nothing is held right now."));
}

#[test]
fn mode_off_starts_no_timer_and_says_to_pick_a_mode() {
    let mut shell = screen(with_mode(Mode::Off));
    let text = shown(&mut shell);
    assert!(
        text.contains("Keep awake is Off. Pick Auto or Manual to start a timer."),
        "{text}"
    );
    for key in ['1', '4', 'c'] {
        shell.key(Key::Char(key));
        assert_eq!(changes(&mut shell), Vec::new(), "{key}");
        let text = shown(&mut shell);
        assert!(
            text.contains("Keep awake is Off. Pick Auto or Manual first."),
            "{key}: {text}"
        );
        assert!(!text.contains("Type minutes"), "{key} opens no field");
    }
}

fn typed(shell: &mut Shell, text: &str) {
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
}

#[test]
fn the_custom_field_takes_digits_and_starts_only_within_bounds() {
    let mut shell = screen(resting());
    shell.key(Key::Char('c'));
    assert!(shown(&mut shell).contains("Type minutes"));
    typed(&mut shell, "9x0");
    shell.key(Key::Enter);
    let sent = changes(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        action(sent[0].clone()),
        KeepAwakeAction::Start { minutes: 90 }
    );
    answer(&mut shell, sent[0].clone(), Ok(KeepAwakeReply::Done));
    assert!(
        !shown(&mut shell).contains("Type minutes"),
        "the field closed"
    );

    for wrong in ["", "0", "14411"] {
        shell.key(Key::Char('c'));
        typed(&mut shell, wrong);
        shell.key(Key::Enter);
        assert_eq!(changes(&mut shell), Vec::new(), "{wrong:?}");
        let text = shown(&mut shell);
        assert!(
            text.contains("Enter whole minutes from 1 to 1440."),
            "{wrong:?}: {text}"
        );
        assert!(text.contains("Type minutes"), "{wrong:?} keeps the field");
        shell.key(Key::Esc);
        assert_eq!(
            shell.place(),
            Some(Place::KeepAwake),
            "Esc closes the field"
        );
    }
    shell.key(Key::Char('c'));
    typed(&mut shell, "14");
    shell.key(Key::Backspace);
    typed(&mut shell, "5");
    shell.key(Key::Enter);
    let sent = changes(&mut shell);
    assert_eq!(
        action(sent[0].clone()),
        KeepAwakeAction::Start { minutes: 15 }
    );
}

#[test]
fn a_refused_change_shows_the_engines_words() {
    let mut shell = screen(resting());
    shell.key(Key::Char('1'));
    let sent = changes(&mut shell);
    answer(&mut shell, sent[0].clone(), Err("keep_awake_off".into()));
    let text = shown(&mut shell);
    assert!(text.contains("Keep awake was not changed."), "{text}");
    assert!(
        text.contains("Keep awake is Off. Pick Auto or Manual first."),
        "{text}"
    );
    shell.key(Key::Char('4'));
    assert_eq!(changes(&mut shell).len(), 1, "the next key is taken");
}

#[test]
fn losing_the_engine_closes_the_minutes_field_and_drops_the_change_under_way() {
    let mut shell = screen(resting());
    shell.key(Key::Char('c'));
    typed(&mut shell, "45");
    shell.arrive(Arrival::Disconnected("trusted_connection_lost".into()));
    let text = shown(&mut shell);
    assert!(
        !text.contains("Type minutes"),
        "the field is closed: {text}"
    );
    assert_eq!(
        shell.ctx.store.keep_awake, None,
        "the view went with the engine"
    );
    shell.key(Key::Enter);
    assert_eq!(
        changes(&mut shell),
        Vec::new(),
        "nothing starts without a view"
    );
}

#[test]
fn s_opens_every_setting_and_esc_returns() {
    let mut shell = screen(resting());
    let depth = shell.depth();
    shell.key(Key::Char('s'));
    assert_eq!(shell.place(), Some(Place::KeepAwakeSettings));
    assert_eq!(shell.depth(), depth + 1);
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::KeepAwake));
}

#[test]
fn a_failed_first_read_says_so_and_r_reads_again() {
    let mut shell = shell_at(ready(80, 24), Place::KeepAwake);
    changes(&mut shell);
    answer(&mut shell, KeepAwakeQuery::Read, Err("owner_busy".into()));
    let text = shown(&mut shell);
    assert!(text.contains("Couldn't load Keep awake."), "{text}");
    assert!(text.contains("Try again"), "{text}");
    shell.key(Key::Char('1'));
    assert_eq!(changes(&mut shell), Vec::new(), "no change without a view");
    shell.key(Key::Char('r'));
    let reads: Vec<Request> = std::iter::from_fn(|| shell.ctx.next_request()).collect();
    assert!(reads.iter().any(is_read), "{reads:?}");
    let view = Box::new(resting());
    answer(
        &mut shell,
        KeepAwakeQuery::Read,
        Ok(KeepAwakeReply::View(view)),
    );
    let text = shown(&mut shell);
    assert!(text.contains("Auto, nothing running"), "{text}");
    assert!(!text.contains("Couldn't load"), "{text}");
}

#[test]
fn the_keep_awake_screen_frames_match_the_design() {
    for (width, height) in [(120, 36), (80, 24)] {
        let mut ctx = ready(width, height);
        ctx.store.keep_awake = Some(lid_held());
        let mut shell = shell_at(ctx, Place::KeepAwake);
        check(
            &format!("keep-awake-{width}"),
            &render(&mut shell, width, height),
            &[],
        );
    }
}

const APPROVAL: &str = "Allow the lid helper in System Settings to hold with the lid closed.";
const PIN_NONE: &str = "The lid helper was approved with no account at the screen";
const TURN_ON: &str = "Turn on lid-closed hold";
const LOGIN_ITEMS: &str = "Open Login Items";

/// Keep awake at rest on a Mac with the lid rule on, where the lid line says `lid`.
/// Input: the lid support. Output: the view.
pub(in crate::tui) fn on_mac(lid: LidSupport) -> KeepAwakeView {
    let mut view = resting();
    view.settings.lid = LidRule::AcOnly;
    view.lid_support = lid.clone();
    view.lid_platform = lid;
    view
}

fn offers(turn_on: Option<&'static str>, remove: bool) -> LidOffers {
    LidOffers { turn_on, remove }
}

#[test]
fn the_lid_helper_keys_follow_what_this_mac_says_of_the_lid_on_macos_only() {
    let failed = "Lid-closed hold could not be turned on: Invalid argument".to_string();
    let none = offers(None, false);
    let cases = [
        (
            LidSupport::NotTurnedOn,
            offers(Some(TURN_ON), false),
            offers(Some(TURN_ON), false),
        ),
        (
            LidSupport::NeedsApproval,
            offers(Some(LOGIN_ITEMS), true),
            offers(Some(LOGIN_ITEMS), false),
        ),
        (
            LidSupport::NotAvailable { reason: failed },
            offers(Some(TURN_ON), false),
            offers(Some(TURN_ON), false),
        ),
        (LidSupport::Held, offers(None, true), none),
        (
            LidSupport::RemoveHelper {
                reason: PIN_NONE.into(),
            },
            offers(None, true),
            offers(None, true),
        ),
        (LidSupport::SleepNotRestored, offers(None, true), none),
        (
            LidSupport::NotAvailable {
                reason: "The lid helper did not answer".into(),
            },
            offers(None, true),
            none,
        ),
        (LidSupport::HelperMissing, none, none),
        (LidSupport::NeedsMacos13, none, none),
        (LidSupport::LockedByPolicy, none, none),
    ];
    for (lid, settings, screen) in cases {
        let mac = on_mac(lid.clone());
        assert_eq!(
            words::lid_offers(&mac, LidPlace::Settings),
            settings,
            "{lid:?}"
        );
        assert_eq!(words::lid_offers(&mac, LidPlace::Screen), screen, "{lid:?}");
        let mut ruled_off = mac.clone();
        ruled_off.settings.lid = LidRule::Off;
        ruled_off.lid_support = LidSupport::Off;
        let anyway = words::lid_offers(&ruled_off, LidPlace::Settings);
        assert_eq!(anyway, settings, "Settings whatever the rule: {lid:?}");
        let hidden = words::lid_offers(&ruled_off, LidPlace::Screen);
        assert_eq!(hidden, none, "the screen while the rule is Off: {lid:?}");
        for platform in [Platform::Linux, Platform::Windows] {
            let mut other = mac.clone();
            other.platform = platform;
            for place in [LidPlace::Screen, LidPlace::Settings] {
                assert_eq!(
                    words::lid_offers(&other, place),
                    none,
                    "{platform:?} {lid:?}"
                );
            }
        }
    }
}

#[test]
fn the_lid_helper_line_has_the_approval_line_while_it_waits() {
    let line = |lid: LidSupport| words::lid_helper_line(&on_mac(lid));
    assert_eq!(line(LidSupport::NeedsApproval), APPROVAL);
    assert_eq!(line(LidSupport::Held), "Turned on");
    assert_eq!(line(LidSupport::NotTurnedOn), "Not turned on yet");
    assert_eq!(
        line(LidSupport::HelperMissing),
        "Not set up on this computer yet"
    );
    assert_eq!(
        line(LidSupport::SleepNotRestored),
        "Sleep not restored: see Notifications"
    );
    let reason = "r".repeat(300);
    let cut = line(LidSupport::RemoveHelper { reason });
    assert_eq!(cut.chars().count(), 200, "a long reason is cut");
}

#[test]
fn a_none_pin_makes_the_screen_offer_remove_lid_helper_on_d() {
    let pin = LidSupport::RemoveHelper {
        reason: PIN_NONE.into(),
    };
    let mut shell = screen(on_mac(pin));
    let text = shown(&mut shell);
    assert_eq!(
        lid_now_shown(&text),
        PIN_NONE,
        "the screen says why: {text}"
    );
    assert!(text.contains("d  Remove lid helper"), "{text}");
    assert!(!text.contains(TURN_ON), "{text}");
    assert_eq!(
        action(pressed(&mut shell, 'd')),
        KeepAwakeAction::RemoveLidHelper
    );
    shell.key(Key::Char('t'));
    assert_eq!(changes(&mut shell), Vec::new(), "no Turn on to offer");
}

#[test]
fn t_turns_on_the_lid_hold_then_opens_login_items_with_the_approval_line() {
    let mut shell = screen(on_mac(LidSupport::NotTurnedOn));
    let text = shown(&mut shell);
    assert!(text.contains("t  Turn on lid-closed hold"), "{text}");
    assert_eq!(
        action(pressed(&mut shell, 't')),
        KeepAwakeAction::TurnOnLidHold
    );
    let mut shell = screen(on_mac(LidSupport::NeedsApproval));
    let text = shown(&mut shell);
    assert!(text.contains("t  Open Login Items"), "{text}");
    assert!(
        text.contains(APPROVAL),
        "the approval screen's line: {text}"
    );
    assert_eq!(
        action(pressed(&mut shell, 't')),
        KeepAwakeAction::TurnOnLidHold,
        "pressed while approval waits it opens Login Items again (QUESTION 35j)"
    );
    shell.key(Key::Char('d'));
    assert_eq!(
        changes(&mut shell),
        Vec::new(),
        "the screen offers Remove only for a pin"
    );
}

#[test]
fn a_failed_unregister_keeps_offering_remove_lid_helper_on_the_screen() {
    let reason = "Remove lid helper did not finish: Operation not permitted";
    let mut shell = screen(on_mac(LidSupport::RemoveHelper {
        reason: reason.into(),
    }));
    let text = shown(&mut shell);
    assert_eq!(lid_now_shown(&text), reason, "{text}");
    assert!(text.contains("d  Remove lid helper"), "{text}");
    assert_eq!(
        action(pressed(&mut shell, 'd')),
        KeepAwakeAction::RemoveLidHelper
    );
}

/// A reason four lines long at 80 columns is read whole under the Lid closed label, and
/// every row below it is still on the screen.
#[test]
fn a_long_lid_reason_is_read_whole_under_its_label_at_80_columns() {
    let reason = format!(
        "The lid helper stopped:{}",
        " it cannot reach the power service".repeat(5)
    );
    assert!((169..=200).contains(&reason.chars().count()));
    let text = shown(&mut screen(on_mac(LidSupport::NotAvailable {
        reason: reason.clone(),
    })));
    assert_eq!(lid_now_shown(&text), reason, "{text}");
    assert!(text.contains("m  Mode"), "{text}");
    assert!(text.contains("s  All Keep awake settings"), "{text}");
    assert!(
        text.lines().all(|line| line.chars().count() <= 80),
        "{text}"
    );
}

#[test]
fn without_a_lid_helper_offer_t_and_d_send_nothing() {
    let mut linux = on_mac(LidSupport::NotTurnedOn);
    linux.platform = Platform::Linux;
    let mut ruled_off = on_mac(LidSupport::NotTurnedOn);
    ruled_off.settings.lid = LidRule::Off;
    ruled_off.lid_support = LidSupport::Off;
    for view in [resting(), linux, ruled_off] {
        let mut shell = screen(view.clone());
        for key in ['t', 'd'] {
            shell.key(Key::Char(key));
            assert_eq!(changes(&mut shell), Vec::new(), "{key} {view:?}");
        }
        let text = shown(&mut shell);
        assert!(!text.contains(TURN_ON), "{text}");
        assert!(!text.contains("Remove lid helper"), "{text}");
    }
}

#[test]
fn a_refused_lid_command_says_why_and_the_next_key_is_taken() {
    let mut shell = screen(on_mac(LidSupport::NotTurnedOn));
    shell.key(Key::Char('t'));
    let sent = changes(&mut shell);
    assert_eq!(sent.len(), 1, "{sent:?}");
    answer(
        &mut shell,
        sent[0].clone(),
        Err("keep_awake_no_lid_helper".into()),
    );
    let text = shown(&mut shell);
    assert!(text.contains("Keep awake was not changed."), "{text}");
    assert!(
        text.contains("Lid-closed hold is set up only on macOS."),
        "{text}"
    );
    shell.key(Key::Char('t'));
    assert_eq!(changes(&mut shell).len(), 1, "the next key is taken");
}
