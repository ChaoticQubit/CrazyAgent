#![cfg(target_os = "macos")]

mod support;

use crazyagent_mac_power::floor::{Battery, Pressure};
use crazyagent_mac_power::helper::{Config, Helper, Readings};
use crazyagent_mac_power::lease::Holder;
use crazyagent_mac_power::lid::Lid;
use crazyagent_mac_power::protocol::{FloorView, PinView, Refusal, Reply, Request, View};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use support::StandIn;

const NOW_MS: u64 = 5_000_000;
const OPEN: Option<Lid> = Some(Lid {
    closed: false,
    external_display: false,
});
const CLOSED_ALONE: Option<Lid> = Some(Lid {
    closed: true,
    external_display: false,
});
const CLOSED_AT_DESK: Option<Lid> = Some(Lid {
    closed: true,
    external_display: true,
});
const COOL: Readings = Readings {
    battery: Battery::Ac { percent: 100 },
    pressure: Pressure::Level(0),
    lid: OPEN,
};
const LOGIN_WINDOW: &str = "/usr";

fn holder(stand_in: &StandIn, pid: i32) -> Holder {
    Holder {
        euid: stand_in.my_uid(),
        pid,
        pid_version: 1,
    }
}

fn hold(helper: &mut Helper, who: Holder, now_ms: u64) -> Reply {
    helper.handle(who, Request::Hold { lease_s: 120 }, now_ms, OPEN)
}

fn view(helper: &mut Helper, who: Holder) -> View {
    match helper.handle(who, Request::Read, NOW_MS, OPEN) {
        Reply::State(view) => view,
        other => panic!("Read answered {other:?}"),
    }
}

#[test]
fn an_expired_lease_restores_sleep_through_the_pmset_program_and_clears_the_journal() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let me = holder(&stand_in, 10);
    assert_eq!(
        hold(&mut helper, me, NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    assert_eq!(stand_in.sleep_disabled(), 1);
    assert!(stand_in.journal_text().unwrap().contains("sleep_disabled"));
    helper.expire(NOW_MS + 119_999, OPEN);
    assert_eq!(stand_in.sleep_disabled(), 1);
    helper.expire(NOW_MS + 120_000, OPEN);
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(
        stand_in.sleep_writes(),
        ["-a disablesleep 1", "-a disablesleep 0"]
    );
    assert_eq!(stand_in.journal_text().as_deref(), Some("[]"));
    assert!(helper.idle());
    assert!(!stand_in.calls().contains(&"sleepnow".to_string()));
}

#[test]
fn a_lease_that_ends_with_the_lid_closed_and_no_display_sleeps_the_mac_now() {
    let alone = StandIn::new(0);
    let mut helper = Helper::start(alone.config());
    assert_eq!(
        hold(&mut helper, holder(&alone, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    helper.expire(NOW_MS + 120_000, CLOSED_ALONE);
    assert_eq!(alone.calls().last().map(String::as_str), Some("sleepnow"));
    assert_eq!(alone.sleep_disabled(), 0);

    let at_desk = StandIn::new(0);
    let mut helper = Helper::start(at_desk.config());
    assert_eq!(
        hold(&mut helper, holder(&at_desk, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    helper.expire(NOW_MS + 120_000, CLOSED_AT_DESK);
    assert!(!at_desk.calls().contains(&"sleepnow".to_string()));
    let unread = StandIn::new(0);
    let mut helper = Helper::start(unread.config());
    assert_eq!(
        hold(&mut helper, holder(&unread, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    helper.expire(NOW_MS + 120_000, None);
    assert!(!unread.calls().contains(&"sleepnow".to_string()));
    assert_eq!(unread.sleep_disabled(), 0);
}

#[test]
fn the_journal_entry_is_on_disk_before_pmset_runs() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    let at_set = stand_in
        .journal_when_set(1)
        .expect("a journal existed when pmset ran");
    assert!(at_set.contains("\"key\":\"sleep_disabled\""), "{at_set}");
    assert!(at_set.contains("\"before\":0"), "{at_set}");
    assert_eq!(
        helper.handle(holder(&stand_in, 10), Request::Release, NOW_MS, OPEN),
        Reply::Released
    );
    let at_restore = stand_in.journal_when_set(0).expect("journal at restore");
    assert!(
        at_restore.contains("sleep_disabled"),
        "pmset puts back before the entry goes"
    );
    assert_eq!(stand_in.journal_text().as_deref(), Some("[]"));
}

#[test]
fn a_foreign_sleep_disabled_is_reported_and_never_cleared() {
    let stand_in = StandIn::new(1);
    let mut helper = Helper::start(stand_in.config());
    let me = holder(&stand_in, 10);
    let before = view(&mut helper, me);
    assert_eq!(before.sleep_disabled, Some(true));
    assert!(before.foreign);
    assert_eq!(
        hold(&mut helper, me, NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    assert_eq!(
        helper.handle(me, Request::Release, NOW_MS, OPEN),
        Reply::Released
    );
    helper.terminate(OPEN);
    assert!(stand_in.sleep_writes().is_empty(), "{:?}", stand_in.calls());
    assert_eq!(stand_in.sleep_disabled(), 1);
    assert!(view(&mut helper, me).foreign);
}

#[test]
fn reset_by_another_tool_is_set_again_and_journaled() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    stand_in.set_by_another_tool(0);
    helper.sense(NOW_MS + 30_000, COOL);
    assert_eq!(stand_in.sleep_disabled(), 1);
    assert_eq!(
        stand_in.sleep_writes(),
        ["-a disablesleep 1", "-a disablesleep 1"]
    );
    assert!(stand_in.journal_text().unwrap().contains("sleep_disabled"));
    helper.expire(NOW_MS + 120_000, OPEN);
    assert_eq!(stand_in.sleep_disabled(), 0);
    stand_in.set_by_another_tool(0);
    helper.sense(NOW_MS + 150_000, COOL);
    assert_eq!(
        stand_in.sleep_writes().len(),
        3,
        "no lease: nothing is set again"
    );
}

#[test]
fn start_reconciles_a_journal_left_by_a_killed_helper() {
    let stand_in = StandIn::new(0);
    let mut first = Helper::start(stand_in.config());
    assert_eq!(
        hold(&mut first, holder(&stand_in, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    drop(first);
    assert_eq!(stand_in.sleep_disabled(), 1);
    let mut second = Helper::start(stand_in.config());
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(
        stand_in.sleep_writes(),
        ["-a disablesleep 1", "-a disablesleep 0"]
    );
    assert_eq!(stand_in.journal_text().as_deref(), Some("[]"));
    let after = view(&mut second, holder(&stand_in, 11));
    assert!(!after.held);
    assert!(after.journal_ok);
}

#[test]
fn an_unreadable_journal_refuses_hold_and_changes_nothing() {
    let stand_in = StandIn::new(0);
    drop(Helper::start(stand_in.config()));
    let file = stand_in.folder().join("journal.json");
    fs::write(&file, "not json").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let mut helper = Helper::start(stand_in.config());
    let me = holder(&stand_in, 10);
    let refused = hold(&mut helper, me, NOW_MS);
    assert_eq!(
        refused,
        Reply::Refused {
            reason: Refusal::JournalUnreadable
        }
    );
    assert!(!view(&mut helper, me).journal_ok);
    helper.terminate(OPEN);
    assert!(stand_in.sleep_writes().is_empty());
    assert_eq!(fs::read_to_string(&file).unwrap(), "not json");
}

#[test]
fn a_term_puts_every_journaled_value_back() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 11), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    helper.terminate(CLOSED_ALONE);
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(stand_in.journal_text().as_deref(), Some("[]"));
    assert_eq!(
        stand_in.calls().last().map(String::as_str),
        Some("sleepnow")
    );
}

#[test]
fn a_hold_from_another_uid_is_refused() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let other = Holder {
        euid: stand_in.my_uid() + 1,
        pid: 20,
        pid_version: 1,
    };
    assert_eq!(
        hold(&mut helper, other, NOW_MS),
        Reply::Refused {
            reason: Refusal::Unpinned
        }
    );
    assert_eq!(view(&mut helper, other).pin, PinView::Other);
    let forget = helper.handle(other, Request::Forget, NOW_MS, OPEN);
    assert_eq!(
        forget,
        Reply::Refused {
            reason: Refusal::Unpinned
        }
    );
    assert!(stand_in.sleep_writes().is_empty());
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
}

#[test]
fn the_first_start_pins_the_console_user_and_forget_clears_it() {
    let stand_in = StandIn::new(0);
    assert_eq!(stand_in.pin_text(), None);
    let mut helper = Helper::start(stand_in.config());
    let mine = format!("{}\n", stand_in.my_uid());
    assert_eq!(stand_in.pin_text().as_deref(), Some(mine.as_str()));
    let me = holder(&stand_in, 10);
    assert_eq!(
        hold(&mut helper, me, NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    assert_eq!(
        helper.handle(me, Request::Forget, NOW_MS, OPEN),
        Reply::Forgotten
    );
    assert_eq!(stand_in.pin_text(), None);
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(stand_in.journal_text().as_deref(), Some("[]"));
    drop(helper);
    let _again = Helper::start(stand_in.config());
    assert_eq!(stand_in.pin_text().as_deref(), Some(mine.as_str()));
}

#[test]
fn a_start_with_no_non_system_console_user_pins_none_and_refuses_hold() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config_with_console(Path::new(LOGIN_WINDOW)));
    assert_eq!(stand_in.pin_text().as_deref(), Some("none\n"));
    let me = holder(&stand_in, 10);
    assert_eq!(
        hold(&mut helper, me, NOW_MS),
        Reply::Refused {
            reason: Refusal::Unpinned
        }
    );
    assert_eq!(view(&mut helper, me).pin, PinView::None);
    assert!(stand_in.sleep_writes().is_empty());
}

#[test]
fn a_none_pin_stays_none_at_later_starts() {
    let stand_in = StandIn::new(0);
    drop(Helper::start(
        stand_in.config_with_console(Path::new(LOGIN_WINDOW)),
    ));
    let mut later = Helper::start(stand_in.config());
    assert_eq!(stand_in.pin_text().as_deref(), Some("none\n"));
    let me = holder(&stand_in, 10);
    assert_eq!(
        hold(&mut later, me, NOW_MS),
        Reply::Refused {
            reason: Refusal::Unpinned
        }
    );
    assert_eq!(view(&mut later, me).pin, PinView::None);
}

#[test]
fn forget_under_a_none_pin_is_accepted_only_from_the_non_system_console_user() {
    let stand_in = StandIn::new(0);
    drop(Helper::start(
        stand_in.config_with_console(Path::new(LOGIN_WINDOW)),
    ));
    let mut helper = Helper::start(stand_in.config());
    let other = Holder {
        euid: stand_in.my_uid() + 1,
        pid: 20,
        pid_version: 1,
    };
    let refused = helper.handle(other, Request::Forget, NOW_MS, OPEN);
    assert_eq!(
        refused,
        Reply::Refused {
            reason: Refusal::Unpinned
        }
    );
    assert_eq!(stand_in.pin_text().as_deref(), Some("none\n"));
    let me = holder(&stand_in, 10);
    assert_eq!(
        helper.handle(me, Request::Forget, NOW_MS, OPEN),
        Reply::Forgotten
    );
    assert_eq!(stand_in.pin_text(), None);
    assert_eq!(
        hold(&mut helper, me, NOW_MS),
        Reply::Refused {
            reason: Refusal::Unpinned
        }
    );
    drop(helper);
    let mut next = Helper::start(stand_in.config());
    assert_eq!(
        hold(&mut next, me, NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
}

#[test]
fn read_reports_the_pin_as_you_other_or_none() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let me = holder(&stand_in, 10);
    let other = Holder {
        euid: stand_in.my_uid() + 1,
        pid: 20,
        pid_version: 1,
    };
    assert_eq!(view(&mut helper, me).pin, PinView::You);
    assert_eq!(view(&mut helper, other).pin, PinView::Other);
    assert_eq!(
        helper.handle(me, Request::Forget, NOW_MS, OPEN),
        Reply::Forgotten
    );
    assert_eq!(view(&mut helper, me).pin, PinView::None);
}

#[test]
fn a_hold_after_forget_is_refused_until_the_next_start() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let me = holder(&stand_in, 10);
    assert_eq!(
        helper.handle(me, Request::Forget, NOW_MS, OPEN),
        Reply::Forgotten
    );
    for attempt in 0..3 {
        let reply = hold(&mut helper, me, NOW_MS + attempt * 1_000);
        assert_eq!(
            reply,
            Reply::Refused {
                reason: Refusal::Unpinned
            }
        );
    }
    assert_eq!(stand_in.pin_text(), None, "a call never pins");
    drop(helper);
    let mut next = Helper::start(stand_in.config());
    assert_eq!(
        hold(&mut next, me, NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
}

#[test]
fn a_critical_battery_reading_ends_every_lease() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 11), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    let four = Readings {
        battery: Battery::OnBattery { percent: 4 },
        ..COOL
    };
    helper.sense(NOW_MS + 30_000, four);
    assert_eq!(stand_in.sleep_disabled(), 1);
    let three = Readings {
        battery: Battery::OnBattery { percent: 3 },
        lid: CLOSED_ALONE,
        ..COOL
    };
    helper.sense(NOW_MS + 60_000, three);
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(
        stand_in.calls().last().map(String::as_str),
        Some("sleepnow")
    );
    let me = holder(&stand_in, 10);
    let state = view(&mut helper, me);
    assert!(!state.held);
    assert_eq!(state.floor, FloorView::Battery);
    assert_eq!(
        hold(&mut helper, me, NOW_MS + 61_000),
        Reply::Refused {
            reason: Refusal::Floor
        }
    );
    let charging = Readings {
        battery: Battery::Ac { percent: 3 },
        ..COOL
    };
    helper.sense(NOW_MS + 90_000, charging);
    assert_eq!(view(&mut helper, me).floor, FloorView::None);
    assert_eq!(
        hold(&mut helper, me, NOW_MS + 91_000),
        Reply::Held { lease_left_s: 120 }
    );
}

#[test]
fn a_thermal_pressure_level_of_three_or_four_ends_every_lease() {
    for level in [3, 4] {
        let stand_in = StandIn::new(0);
        let mut helper = Helper::start(stand_in.config());
        let me = holder(&stand_in, 10);
        assert_eq!(
            hold(&mut helper, me, NOW_MS),
            Reply::Held { lease_left_s: 120 }
        );
        helper.sense(
            NOW_MS + 30_000,
            Readings {
                pressure: Pressure::Level(2),
                ..COOL
            },
        );
        assert_eq!(stand_in.sleep_disabled(), 1, "level 2 is not critical");
        helper.sense(
            NOW_MS + 60_000,
            Readings {
                pressure: Pressure::Level(level),
                ..COOL
            },
        );
        assert_eq!(stand_in.sleep_disabled(), 0, "level {level}");
        assert_eq!(view(&mut helper, me).floor, FloorView::Heat);
        assert_eq!(
            hold(&mut helper, me, NOW_MS + 61_000),
            Reply::Refused {
                reason: Refusal::Floor
            }
        );
    }
}

#[test]
fn an_unreadable_thermal_pressure_level_ends_every_lease() {
    for pressure in [Pressure::Unread, Pressure::Level(5)] {
        let stand_in = StandIn::new(0);
        let mut helper = Helper::start(stand_in.config());
        let me = holder(&stand_in, 10);
        assert_eq!(
            hold(&mut helper, me, NOW_MS),
            Reply::Held { lease_left_s: 120 }
        );
        helper.sense(NOW_MS + 30_000, Readings { pressure, ..COOL });
        assert_eq!(stand_in.sleep_disabled(), 0, "{pressure:?}");
        assert_eq!(view(&mut helper, me).floor, FloorView::Heat);
    }
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    helper.sense(
        NOW_MS + 30_000,
        Readings {
            battery: Battery::Unread,
            ..COOL
        },
    );
    assert_eq!(
        stand_in.sleep_disabled(),
        1,
        "an unread battery does not end a hold"
    );
}

fn refusing_put_back(stand_in: &StandIn) -> Config {
    let gate = stand_in.path("refuse-put-back");
    let wrapper = stand_in.path("wrapper");
    let text = format!(
        "#!/bin/sh\nif [ \"$*\" = \"-a disablesleep 0\" ] && [ -e '{}' ]; then exit 1; fi\n\
         exec '{}' \"$@\"\n",
        gate.display(),
        stand_in.program().display()
    );
    fs::write(&wrapper, text).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    Config {
        pmset: wrapper,
        ..stand_in.config()
    }
}

#[test]
fn a_put_back_that_fails_keeps_its_entry_and_is_tried_again_at_the_next_reading() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(refusing_put_back(&stand_in));
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    fs::write(stand_in.path("refuse-put-back"), "").unwrap();
    helper.expire(NOW_MS + 120_000, CLOSED_ALONE);
    assert_eq!(stand_in.sleep_disabled(), 1);
    assert!(stand_in.journal_text().unwrap().contains("sleep_disabled"));
    assert!(!stand_in.calls().contains(&"sleepnow".to_string()));
    assert!(!helper.idle(), "a value still to put back keeps the helper up");
    assert!(helper.wants_readings());
    fs::remove_file(stand_in.path("refuse-put-back")).unwrap();
    helper.sense(NOW_MS + 150_000, COOL);
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(stand_in.journal_text().as_deref(), Some("[]"));
    assert!(helper.idle());
    assert!(!helper.wants_readings());
}
