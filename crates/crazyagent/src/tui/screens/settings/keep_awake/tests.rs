use crate::keep_awake::{
    BatteryFloor, Cap, DisplayAwake, Grace, HeatEnd, HeatNotice, KeepAwakeSettings, KeepAwakeView,
    LidDisplay, LidRule, LidSupport, Mode,
};
use crate::paths::Platform;
use crate::tui::client::OwnerEvent;
use crate::tui::client::keep_awake::{KeepAwakeEvent, KeepAwakeQuery, KeepAwakeReply, Picks};
use crate::tui::keys::Key;
use crate::tui::render_tests::{ready, render, shell_at};
use crate::tui::screens::Place;
use crate::tui::screens::keep_awake::tests::resting;
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::Shell;

const DEFAULT: KeepAwakeSettings = KeepAwakeSettings::DEFAULT;

fn page(view: KeepAwakeView) -> Shell {
    let mut ctx = ready(120, 60);
    ctx.store.keep_awake = Some(view);
    shell_at(ctx, Place::KeepAwakeSettings)
}

fn saves(shell: &mut Shell) -> Vec<KeepAwakeQuery> {
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
    render(shell, 120, 60).join("\n")
}

/// Presses Enter and returns the picks of the one save it sent. Input: the shell.
/// Output: the picks and the query, to answer it.
fn saved(shell: &mut Shell) -> (Picks, KeepAwakeQuery) {
    shell.key(Key::Enter);
    let mut sent = saves(shell);
    assert_eq!(sent.len(), 1, "Enter sends one save: {sent:?}");
    let query = sent.remove(0);
    let KeepAwakeQuery::Save { picks, generation } = query.clone() else {
        panic!("not a save: {query:?}");
    };
    assert!(generation > 0);
    (picks, query)
}

/// Opens the page on the default settings, moves down `row` settings, presses one arrow
/// and saves. Inputs: the setting's place on the page and the arrow. Output: the settings
/// the save leaves when laid over the defaults.
fn after_arrow(row: usize, arrow: Key) -> KeepAwakeSettings {
    let mut shell = page(resting());
    for _ in 0..row {
        shell.key(Key::Down);
    }
    shell.key(arrow);
    let (picks, _) = saved(&mut shell);
    picks.over(DEFAULT)
}

#[test]
fn the_page_lists_the_nine_settings_in_the_desktops_sections_and_words() {
    let text = shown(&mut page(resting()));
    let in_order = [
        "Keep awake",
        "When the computer stays awake for your agents, and when that ends.",
        "When it holds",
        "What keeps the computer awake and for how long.",
        "Mode",
        "[Auto]  Manual timer   Off",
        "Auto holds while agents work. Manual timer holds only while a timer you start runs.",
        "Grace after work ends",
        " 0   5 min  [10 min]  30 min",
        "How long the computer stays awake after the last agent finishes.",
        "Longest single hold",
        " 4 h   8 h  [24 h]  No limit",
        "One hold ends by itself after this long, even while agents still work.",
        "Whether a hold goes on with the lid closed.",
        "Lid-closed hold",
        "[On AC power only]  Always   Off",
        "Whether a hold also keeps the computer awake with the lid closed.",
        "Display with the lid closed (macOS)",
        "[Sleep the display]  Leave on",
        "What an outside display does while a hold runs with the lid closed.",
        "Ends by itself",
        "The limits that end a hold to protect the battery and the computer.",
        "End at battery",
        " Off   5 %  [10 %]  20 %",
        "On battery, a hold ends when the charge falls to this level.",
        "End at heat",
        "[Critical]  Serious   Never",
        "A hold ends when the computer gets this hot.",
        "Heat notice",
        "[At serious heat]  Off",
        "Tells you when the computer gets hot during a hold.",
        "What a hold does with the display.",
        "Display awake with the lid open",
        "[Off]  While holding",
        "Whether a hold also keeps the display on. While it does, the screen stays unlocked.",
    ];
    let mut from = 0;
    for part in in_order {
        let at = text[from..]
            .find(part)
            .unwrap_or_else(|| panic!("{part:?} is missing or out of order:\n{text}"));
        from += at + part.len();
    }
}

#[test]
fn right_picks_the_next_option_of_each_setting() {
    let cases = [
        KeepAwakeSettings {
            mode: Mode::Manual,
            ..DEFAULT
        },
        KeepAwakeSettings {
            grace: Grace::Min30,
            ..DEFAULT
        },
        KeepAwakeSettings {
            cap: Cap::None,
            ..DEFAULT
        },
        KeepAwakeSettings {
            lid: LidRule::Always,
            ..DEFAULT
        },
        KeepAwakeSettings {
            lid_display: LidDisplay::LeaveOn,
            ..DEFAULT
        },
        KeepAwakeSettings {
            battery_floor: BatteryFloor::Pct20,
            ..DEFAULT
        },
        KeepAwakeSettings {
            heat_end: HeatEnd::Serious,
            ..DEFAULT
        },
        KeepAwakeSettings {
            heat_notice: HeatNotice::Off,
            ..DEFAULT
        },
        KeepAwakeSettings {
            display_awake: DisplayAwake::WhileHolding,
            ..DEFAULT
        },
    ];
    for (row, want) in cases.into_iter().enumerate() {
        assert_eq!(after_arrow(row, Key::Right), want, "row {row}");
    }
}

#[test]
fn left_picks_the_option_before_and_wraps_at_the_first() {
    let cases = [
        KeepAwakeSettings {
            mode: Mode::Off,
            ..DEFAULT
        },
        KeepAwakeSettings {
            grace: Grace::Min5,
            ..DEFAULT
        },
        KeepAwakeSettings {
            cap: Cap::H8,
            ..DEFAULT
        },
        KeepAwakeSettings {
            lid: LidRule::Off,
            ..DEFAULT
        },
        KeepAwakeSettings {
            lid_display: LidDisplay::LeaveOn,
            ..DEFAULT
        },
        KeepAwakeSettings {
            battery_floor: BatteryFloor::Pct5,
            ..DEFAULT
        },
        KeepAwakeSettings {
            heat_end: HeatEnd::Never,
            ..DEFAULT
        },
        KeepAwakeSettings {
            heat_notice: HeatNotice::Off,
            ..DEFAULT
        },
        KeepAwakeSettings {
            display_awake: DisplayAwake::WhileHolding,
            ..DEFAULT
        },
    ];
    for (row, want) in cases.into_iter().enumerate() {
        assert_eq!(after_arrow(row, Key::Left), want, "row {row}");
    }
}

#[test]
fn every_option_of_every_setting_is_reachable_and_a_full_turn_saves_nothing() {
    let counts = [3, 4, 4, 3, 2, 4, 3, 2, 2];
    for (row, count) in counts.into_iter().enumerate() {
        let mut seen = vec![DEFAULT];
        for presses in 1..count {
            let mut shell = page(resting());
            for _ in 0..row {
                shell.key(Key::Down);
            }
            for _ in 0..presses {
                shell.key(Key::Right);
            }
            let (picks, _) = saved(&mut shell);
            let settings = picks.over(DEFAULT);
            assert!(!seen.contains(&settings), "row {row}, {presses} presses");
            seen.push(settings);
        }
        assert_eq!(seen.len(), count, "row {row}");
        let mut shell = page(resting());
        for _ in 0..row {
            shell.key(Key::Down);
        }
        for _ in 0..count {
            shell.key(Key::Right);
        }
        shell.key(Key::Enter);
        assert_eq!(
            saves(&mut shell),
            Vec::new(),
            "row {row}: back at the start"
        );
    }
}

#[test]
fn enter_saves_the_picks_only_and_says_saved() {
    let mut shell = page(resting());
    assert!(!shown(&mut shell).contains("Save changes"));
    shell.key(Key::Enter);
    assert_eq!(
        saves(&mut shell),
        Vec::new(),
        "nothing picked, nothing sent"
    );
    shell.key(Key::Down);
    shell.key(Key::Right);
    shell.key(Key::End);
    shell.key(Key::Right);
    let text = shown(&mut shell);
    assert!(text.contains("Save changes"), "{text}");
    assert!(text.contains("Discard changes"), "{text}");
    let (picks, query) = saved(&mut shell);
    let want = Picks {
        grace: Some(Grace::Min30),
        display_awake: Some(DisplayAwake::WhileHolding),
        ..Picks::default()
    };
    assert_eq!(picks, want);
    assert!(shown(&mut shell).contains("Saving"));
    shell.key(Key::Left);
    shell.key(Key::Enter);
    assert_eq!(saves(&mut shell), Vec::new(), "one save at a time");
    answer(&mut shell, query, Ok(KeepAwakeReply::Done));
    let text = shown(&mut shell);
    assert!(text.contains("Keep awake settings saved."), "{text}");
    assert!(!text.contains("Save changes"), "the picks are gone: {text}");
}

#[test]
fn esc_drops_the_picks_first_and_then_goes_back() {
    let mut ctx = ready(120, 60);
    ctx.store.keep_awake = Some(resting());
    let mut shell = shell_at(ctx, Place::KeepAwake);
    shell.key(Key::Char('s'));
    assert_eq!(shell.place(), Some(Place::KeepAwakeSettings));
    shell.key(Key::Right);
    assert!(shown(&mut shell).contains("[Manual timer]"));
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::KeepAwakeSettings));
    let text = shown(&mut shell);
    assert!(text.contains("[Auto]"), "{text}");
    shell.key(Key::Enter);
    assert_eq!(saves(&mut shell), Vec::new());
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::KeepAwake));
}

#[test]
fn a_pick_survives_a_fresh_read_and_the_save_carries_only_the_pick() {
    let mut shell = page(resting());
    shell.key(Key::Down);
    shell.key(Key::Right);
    let mut theirs = resting();
    theirs.settings.cap = Cap::H4;
    answer(
        &mut shell,
        KeepAwakeQuery::Read,
        Ok(KeepAwakeReply::View(Box::new(theirs))),
    );
    let text = shown(&mut shell);
    assert!(
        text.contains("[4 h]"),
        "the other client's change shows: {text}"
    );
    assert!(text.contains("[30 min]"), "the pick stays: {text}");
    let (picks, _) = saved(&mut shell);
    let want = Picks {
        grace: Some(Grace::Min30),
        ..Picks::default()
    };
    assert_eq!(picks, want, "the save leaves the cap to the engine");
}

#[test]
fn a_refused_save_keeps_the_picks_and_says_why() {
    let mut shell = page(resting());
    shell.key(Key::Right);
    let (_, query) = saved(&mut shell);
    answer(&mut shell, query, Err("owner_busy".into()));
    let text = shown(&mut shell);
    assert!(
        text.contains("The Keep awake settings were not saved."),
        "{text}"
    );
    assert!(text.contains("[Manual timer]"), "{text}");
    assert!(text.contains("Save changes"), "{text}");
    let (picks, _) = saved(&mut shell);
    assert_eq!(picks.mode, Some(Mode::Manual), "Enter saves again");
}

#[test]
fn a_setting_this_computer_cannot_honour_says_why() {
    let text = shown(&mut page(resting()));
    for reason in ["Not available on Windows", "Only on macOS", "Not set up"] {
        assert!(!text.contains(reason), "{reason}: {text}");
    }
    let mut windows = resting();
    windows.platform = Platform::Windows;
    windows.lid_support = LidSupport::HelperMissing;
    let text = shown(&mut page(windows));
    assert_eq!(
        text.matches("Not available on Windows").count(),
        2,
        "{text}"
    );
    assert_eq!(text.matches("Only on macOS").count(), 1, "{text}");
    assert!(text.contains("Not set up on this computer yet"), "{text}");
    let mut linux = resting();
    linux.platform = Platform::Linux;
    let text = shown(&mut page(linux));
    assert!(!text.contains("Not available on Windows"), "{text}");
    assert_eq!(text.matches("Only on macOS").count(), 1, "{text}");
}

#[test]
fn the_selected_setting_stays_in_view_in_a_short_window() {
    let mut ctx = ready(80, 24);
    ctx.store.keep_awake = Some(resting());
    let mut shell = shell_at(ctx, Place::KeepAwakeSettings);
    let first = render(&mut shell, 80, 24).join("\n");
    assert!(first.contains("When it holds"), "{first}");
    assert!(
        !first.contains("Display awake with the lid open"),
        "{first}"
    );
    shell.key(Key::End);
    let last = render(&mut shell, 80, 24).join("\n");
    assert!(last.contains("> Display awake with the lid open"), "{last}");
    assert!(
        last.contains("Whether a hold also keeps the display on."),
        "{last}"
    );
    shell.key(Key::Home);
    let first = render(&mut shell, 80, 24).join("\n");
    assert!(first.contains("> Mode"), "{first}");
    assert!(first.contains("When it holds"), "{first}");
}

#[test]
fn before_the_first_read_no_key_changes_anything_and_a_failed_read_offers_r() {
    let mut shell = shell_at(ready(120, 60), Place::KeepAwakeSettings);
    saves(&mut shell);
    shell.key(Key::Right);
    shell.key(Key::Enter);
    assert_eq!(saves(&mut shell), Vec::new());
    answer(&mut shell, KeepAwakeQuery::Read, Err("owner_busy".into()));
    let text = shown(&mut shell);
    assert!(
        text.contains("Couldn't load the Keep awake settings."),
        "{text}"
    );
    assert!(text.contains("Try again"), "{text}");
    shell.key(Key::Char('r'));
    let reads: Vec<Request> = std::iter::from_fn(|| shell.ctx.next_request()).collect();
    assert!(reads.iter().any(is_read), "{reads:?}");
}
