use crate::notifications::{
    Banners, Delivery, KindDelivery, NoticeAction, NoticeClass, NoticeKind, NoticeSettings,
};
use crate::tui::client::OwnerEvent;
use crate::tui::client::notifications::{NoticesEvent, NoticesQuery, NoticesReply};
use crate::tui::frame::Tone;
use crate::tui::keys::Key;
use crate::tui::render_tests::{ready, render, shell_at};
use crate::tui::screens::Place;
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::Shell;

const DAYS_WRONG: &str = "Enter a whole number of days from 1 to 365.";

fn sent(shell: &mut Shell) -> Vec<NoticesQuery> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Notices(query) if *query != NoticesQuery::counts() => Some(*query),
            _ => None,
        })
        .collect()
}

fn saved(shell: &mut Shell) -> Vec<NoticeSettings> {
    sent(shell)
        .into_iter()
        .filter_map(|query| match query {
            NoticesQuery::Act {
                action: NoticeAction::SaveSettings { settings },
                ..
            } => Some(settings),
            _ => None,
        })
        .collect()
}

fn reply(query: NoticesQuery, result: Result<NoticesReply, String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Notices(Box::new(NoticesEvent {
        query,
        result,
    }))))
}

fn with(kind: NoticeKind, delivery: Delivery) -> NoticeSettings {
    let mut settings = NoticeSettings::default();
    settings
        .kinds
        .iter_mut()
        .find(|item| item.kind == kind)
        .unwrap()
        .delivery = delivery;
    settings
}

fn opened(width: u16, height: u16) -> Shell {
    shell_at(ready(width, height), Place::NotificationSettings)
}

fn loaded_at(settings: NoticeSettings, width: u16, height: u16) -> Shell {
    let mut shell = opened(width, height);
    assert_eq!(sent(&mut shell), vec![NoticesQuery::Settings], "one read");
    shell.arrive(reply(
        NoticesQuery::Settings,
        Ok(NoticesReply::Settings(settings)),
    ));
    shell
}

fn loaded(settings: NoticeSettings) -> Shell {
    loaded_at(settings, 120, 40)
}

fn screen(shell: &mut Shell, width: u16, height: u16) -> String {
    render(shell, width, height).join("\n")
}

fn press(shell: &mut Shell, key: Key, times: usize) {
    for _ in 0..times {
        shell.key(key.clone());
    }
}

fn typed(shell: &mut Shell, text: &str) {
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
}

/// The settings the page would save after one change: the stored ones with the change.
/// Inputs: the stored settings and the change. Output: the settings to expect.
fn changed(stored: &NoticeSettings, change: impl FnOnce(&mut NoticeSettings)) -> NoticeSettings {
    let mut settings = stored.clone();
    change(&mut settings);
    settings
}

fn set(settings: &mut NoticeSettings, kind: NoticeKind, delivery: Delivery) {
    settings.kinds = settings
        .kinds
        .iter()
        .map(|item| KindDelivery {
            kind: item.kind,
            delivery: if item.kind == kind {
                delivery
            } else {
                item.delivery
            },
        })
        .collect();
}

#[test]
fn the_page_reads_the_settings_when_shown_and_names_them_in_the_desktop_words() {
    let mut shell = loaded(NoticeSettings::default());
    let top = screen(&mut shell, 120, 40);
    let header = top.lines().next().unwrap();
    assert!(header.contains("Settings  /  Notifications"), "{header}");
    for want in [
        "What reaches you, and how long finished items stay.",
        "Action items",
        "Things waiting on you.",
        "Info items",
        "Things that happened.",
        "Question",
        "Account sign-in",
        "Change set to review",
        "Agent finished",
        "Bell only",
    ] {
        assert!(top.contains(want), "{want} missing in\n{top}");
    }
    shell.key(Key::End);
    let bottom = screen(&mut shell, 120, 40);
    for want in [
        "macOS notifications",
        "Which items also show as a macOS notification.",
        "Show",
        "Action items only",
        "Done items",
        "Finished and deleted items leave the Done filter after this many days.",
        "Keep finished items for",
        "30",
        "days",
        "Team alert",
    ] {
        assert!(bottom.contains(want), "{want} missing in\n{bottom}");
    }
    assert!(sent(&mut shell).is_empty(), "moving reads nothing");
}

#[test]
fn an_action_kind_offers_on_and_bell_only_and_is_never_off() {
    let stored = NoticeSettings::default();
    assert_eq!(NoticeKind::Question.class(), NoticeClass::Action);
    let mut shell = loaded(stored.clone());
    press(&mut shell, Key::Right, 2);
    shell.key(Key::Enter);
    assert!(
        saved(&mut shell).is_empty(),
        "On, Bell only, then On again: nothing changed, nothing is saved"
    );
    shell.key(Key::Left);
    assert!(
        screen(&mut shell, 120, 40).contains("Save changes"),
        "a pick waits for Enter"
    );
    shell.key(Key::Enter);
    assert_eq!(
        saved(&mut shell),
        vec![changed(&stored, |settings| set(
            settings,
            NoticeKind::Question,
            Delivery::BellOnly
        ))],
        "Left wraps from On to Bell only, never to Off"
    );
}

#[test]
fn an_info_kind_steps_through_on_bell_only_and_off() {
    let stored = NoticeSettings::default();
    let actions = NoticeKind::ALL
        .iter()
        .filter(|kind| kind.class() == NoticeClass::Action)
        .count();
    assert_eq!(NoticeKind::ALL[actions], NoticeKind::AgentFinished);
    let mut shell = loaded(stored.clone());
    press(&mut shell, Key::Down, actions);
    press(&mut shell, Key::Right, 2);
    shell.key(Key::Enter);
    assert_eq!(
        saved(&mut shell),
        vec![changed(&stored, |settings| set(
            settings,
            NoticeKind::AgentFinished,
            Delivery::Off
        ))]
    );
}

#[test]
fn each_macos_choice_saves_off_action_items_only_or_all() {
    for (stored, rights, want) in [
        (Banners::All, 1, Banners::Off),
        (Banners::All, 2, Banners::ActionOnly),
        (Banners::Off, 2, Banners::All),
    ] {
        let settings = NoticeSettings {
            banners: stored,
            ..NoticeSettings::default()
        };
        let mut shell = loaded(settings.clone());
        shell.key(Key::End);
        shell.key(Key::Up);
        press(&mut shell, Key::Right, rights);
        shell.key(Key::Enter);
        assert_eq!(
            saved(&mut shell),
            vec![NoticeSettings {
                banners: want,
                ..settings
            }],
            "{stored:?} and {rights} Right"
        );
    }
}

#[test]
fn the_days_step_by_one_within_bounds_and_typed_digits_replace_them() {
    let stored = NoticeSettings::default();
    let mut shell = loaded(stored.clone());
    shell.key(Key::End);
    shell.key(Key::Right);
    assert!(screen(&mut shell, 120, 40).contains("31"));
    press(&mut shell, Key::Left, 2);
    assert!(screen(&mut shell, 120, 40).contains("29"));
    typed(&mut shell, "45");
    shell.key(Key::Backspace);
    typed(&mut shell, "0");
    shell.key(Key::Enter);
    assert_eq!(
        saved(&mut shell),
        vec![NoticeSettings {
            retention_days: 40,
            ..stored
        }],
        "the first digit replaces the shown days"
    );

    for (days, key) in [(365, Key::Right), (1, Key::Left)] {
        let settings = NoticeSettings {
            retention_days: days,
            ..NoticeSettings::default()
        };
        let mut shell = loaded(settings);
        shell.key(Key::End);
        shell.key(key);
        shell.key(Key::Enter);
        assert!(saved(&mut shell).is_empty(), "{days} stays in bounds");
    }
}

#[test]
fn days_outside_the_bounds_are_refused_before_anything_is_sent() {
    for typing in ["0", "400", ""] {
        let mut shell = loaded(NoticeSettings::default());
        shell.key(Key::End);
        typed(&mut shell, "9");
        press(&mut shell, Key::Backspace, 1);
        typed(&mut shell, typing);
        shell.key(Key::Enter);
        assert!(saved(&mut shell).is_empty(), "{typing:?} is refused");
        let shown = screen(&mut shell, 120, 40);
        assert!(shown.contains(DAYS_WRONG), "{typing:?}:\n{shown}");
    }
}

#[test]
fn enter_saves_once_and_a_saved_change_reads_the_settings_again() {
    let stored = NoticeSettings::default();
    let mut shell = loaded(stored.clone());
    shell.key(Key::Right);
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    let queries = sent(&mut shell);
    assert_eq!(queries.len(), 1, "a save on its way takes no second Enter");
    let NoticesQuery::Act { action, .. } = &queries[0] else {
        panic!("Enter saves: {queries:?}");
    };
    let picked = with(NoticeKind::Question, Delivery::BellOnly);
    assert_eq!(
        *action,
        NoticeAction::SaveSettings {
            settings: picked.clone()
        }
    );
    assert!(screen(&mut shell, 120, 40).contains("Saving"));

    shell.arrive(reply(queries[0].clone(), Ok(NoticesReply::Done)));
    let message = shell.ctx.message().cloned().expect("the save is confirmed");
    assert_eq!(message.tone, Tone::Success);
    assert_eq!(message.lead, "Notification settings saved.");
    assert_eq!(sent(&mut shell), vec![NoticesQuery::Settings], "read again");
    shell.arrive(reply(
        NoticesQuery::Settings,
        Ok(NoticesReply::Settings(picked)),
    ));
    let shown = screen(&mut shell, 120, 40);
    assert!(!shown.contains("Save changes"), "{shown}");
}

#[test]
fn a_refused_save_keeps_the_pick_and_says_why() {
    let mut shell = loaded(NoticeSettings::default());
    shell.key(Key::Right);
    shell.key(Key::Enter);
    let queries = sent(&mut shell);
    assert_eq!(queries.len(), 1);
    shell.arrive(reply(
        queries[0].clone(),
        Err("notification_settings_invalid".into()),
    ));
    let shown = screen(&mut shell, 120, 40);
    assert!(
        shown.contains("The notification settings were not saved."),
        "{shown}"
    );
    assert!(shown.contains("Save changes"), "the pick stays:\n{shown}");
    shell.key(Key::Enter);
    assert_eq!(
        saved(&mut shell),
        vec![with(NoticeKind::Question, Delivery::BellOnly)],
        "Enter tries the same save again"
    );
}

#[test]
fn s_on_the_list_opens_the_page_and_esc_discards_picks_before_going_back() {
    let mut shell = shell_at(ready(120, 40), Place::Notifications);
    sent(&mut shell);
    shell.key(Key::Char('s'));
    assert_eq!(shell.place(), Some(Place::NotificationSettings));
    assert_eq!(sent(&mut shell), vec![NoticesQuery::Settings]);
    shell.arrive(reply(
        NoticesQuery::Settings,
        Ok(NoticesReply::Settings(NoticeSettings::default())),
    ));
    shell.key(Key::Right);
    shell.key(Key::Esc);
    assert_eq!(
        shell.place(),
        Some(Place::NotificationSettings),
        "the first Esc discards the pick"
    );
    shell.key(Key::Enter);
    assert!(saved(&mut shell).is_empty(), "nothing is left to save");
    shell.key(Key::Esc);
    assert_eq!(shell.place(), Some(Place::Notifications));
}

#[test]
fn esc_during_a_save_waits_for_its_reply_so_the_result_is_kept() {
    for (result, lead, tone) in [
        (
            Err("notification_settings_invalid".to_owned()),
            "The notification settings were not saved.",
            Tone::Error,
        ),
        (
            Ok(NoticesReply::Done),
            "Notification settings saved.",
            Tone::Success,
        ),
    ] {
        let mut shell = shell_at(ready(120, 40), Place::Notifications);
        sent(&mut shell);
        shell.key(Key::Char('s'));
        assert_eq!(sent(&mut shell), vec![NoticesQuery::Settings]);
        shell.arrive(reply(
            NoticesQuery::Settings,
            Ok(NoticesReply::Settings(NoticeSettings::default())),
        ));
        shell.key(Key::Right);
        shell.key(Key::Enter);
        let queries = sent(&mut shell);
        assert_eq!(queries.len(), 1, "one save is on its way");
        shell.key(Key::Esc);
        assert_eq!(
            shell.place(),
            Some(Place::NotificationSettings),
            "Esc waits for the save's reply"
        );
        let shown = screen(&mut shell, 120, 40);
        assert!(
            !shown.contains("Discard changes"),
            "no key offers to drop a save on its way:\n{shown}"
        );
        shell.arrive(reply(queries[0].clone(), result));
        let message = shell.ctx.message().cloned().expect("the result is shown");
        assert_eq!((message.lead.as_str(), message.tone), (lead, tone));
        if tone == Tone::Error {
            shell.key(Key::Esc);
            assert_eq!(
                shell.place(),
                Some(Place::NotificationSettings),
                "after a refusal Esc first drops the kept pick"
            );
        }
        shell.key(Key::Esc);
        assert_eq!(shell.place(), Some(Place::Notifications));
    }
}

#[test]
fn go_to_opens_the_page_by_its_settings_name() {
    let mut shell = shell_at(ready(120, 40), Place::Home);
    shell.key(Key::Ctrl('k'));
    typed(&mut shell, "Settings / Notif");
    shell.key(Key::Enter);
    assert_eq!(shell.place(), Some(Place::NotificationSettings));
}

#[test]
fn a_failed_read_says_so_and_r_tries_again() {
    let mut shell = opened(120, 40);
    assert_eq!(sent(&mut shell), vec![NoticesQuery::Settings]);
    shell.arrive(reply(
        NoticesQuery::Settings,
        Err("notification_settings_invalid".into()),
    ));
    let shown = screen(&mut shell, 120, 40);
    assert!(
        shown.contains("Couldn't load the notification settings."),
        "{shown}"
    );
    assert!(shown.contains("r Try again"), "{shown}");
    shell.key(Key::Char('r'));
    assert_eq!(sent(&mut shell), vec![NoticesQuery::Settings]);
}

#[test]
fn the_chosen_row_and_every_key_stay_in_view_at_80x24() {
    let mut shell = loaded_at(NoticeSettings::default(), 80, 24);
    let top = screen(&mut shell, 80, 24);
    assert!(top.contains("Question"), "{top}");
    for want in ["Move", "Change", "Esc Back"] {
        assert!(top.contains(want), "{want} missing in\n{top}");
    }
    let rows = NoticeKind::ALL.len() + 2;
    press(&mut shell, Key::Down, rows);
    let bottom = screen(&mut shell, 80, 24);
    assert!(bottom.contains("Keep finished items for"), "{bottom}");
    shell.key(Key::Right);
    let picked = screen(&mut shell, 80, 24);
    for want in ["Enter Save changes", "Esc Discard changes"] {
        assert!(picked.contains(want), "{want} missing in\n{picked}");
    }
    shell.key(Key::Home);
    assert!(screen(&mut shell, 80, 24).contains("Question"));
}
