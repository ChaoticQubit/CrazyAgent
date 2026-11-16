#![cfg(target_os = "macos")]

mod support;

use crazyagent_mac_power::floor::{Battery, Pressure};
use crazyagent_mac_power::helper::{Config, Helper, Readings};
use crazyagent_mac_power::lease::Holder;
use crazyagent_mac_power::lid::Lid;
use crazyagent_mac_power::protocol::{
    FloorView, LidDisplay, PinView, Refusal, Reply, Request, View,
};
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

/// A hold that leaves the display on, as every row before Task 12 holds.
const HOLD_LEAVE_ON: Request = Request::Hold {
    lease_s: 120,
    lid_display: LidDisplay::LeaveOn,
};

fn hold(helper: &mut Helper, who: Holder, now_ms: u64) -> Reply {
    helper.handle(who, HOLD_LEAVE_ON, now_ms, OPEN)
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

/// A configuration whose pmset fails `-a disablesleep 0` while the stand-in's `refuse-put-back`
/// file exists, and fails `-g` while its `refuse-read` file exists. A refused call is not
/// recorded in the stand-in's calls.
fn refusing(stand_in: &StandIn) -> Config {
    let put_back = stand_in.path("refuse-put-back");
    let read = stand_in.path("refuse-read");
    let wrapper = stand_in.path("wrapper");
    let text = format!(
        "#!/bin/sh\nif [ \"$*\" = \"-a disablesleep 0\" ] && [ -e '{}' ]; then exit 1; fi\n\
         if [ \"$*\" = \"-g\" ] && [ -e '{}' ]; then exit 1; fi\n\
         exec '{}' \"$@\"\n",
        put_back.display(),
        read.display(),
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
    let mut helper = Helper::start(refusing(&stand_in));
    assert_eq!(
        hold(&mut helper, holder(&stand_in, 10), NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    fs::write(stand_in.path("refuse-put-back"), "").unwrap();
    helper.expire(NOW_MS + 120_000, CLOSED_ALONE);
    assert_eq!(stand_in.sleep_disabled(), 1);
    assert!(stand_in.journal_text().unwrap().contains("sleep_disabled"));
    assert!(!stand_in.calls().contains(&"sleepnow".to_string()));
    assert!(
        !helper.idle(),
        "a value still to put back keeps the helper up"
    );
    assert!(helper.wants_readings());
    fs::remove_file(stand_in.path("refuse-put-back")).unwrap();
    helper.sense(NOW_MS + 150_000, COOL);
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(stand_in.journal_text().as_deref(), Some("[]"));
    assert!(helper.idle());
    assert!(!helper.wants_readings());
}

#[test]
fn forget_after_a_failed_put_back_is_refused_and_keeps_the_pin_and_the_entry() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(refusing(&stand_in));
    let me = holder(&stand_in, 10);
    let mine = format!("{}\n", stand_in.my_uid());
    assert_eq!(
        hold(&mut helper, me, NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    fs::write(stand_in.path("refuse-put-back"), "").unwrap();
    let refused = helper.handle(me, Request::Forget, NOW_MS + 1_000, OPEN);
    assert_eq!(
        refused,
        Reply::Refused {
            reason: Refusal::Pmset
        }
    );
    assert_eq!(stand_in.pin_text().as_deref(), Some(mine.as_str()));
    assert!(stand_in.journal_text().unwrap().contains("sleep_disabled"));
    assert_eq!(stand_in.sleep_disabled(), 1);
    let state = view(&mut helper, me);
    assert_eq!(state.pin, PinView::You, "the pin stays in memory too");
    assert!(!state.held, "the leases still end");
    assert!(!helper.idle(), "the value still to put back keeps the helper up");
    fs::remove_file(stand_in.path("refuse-put-back")).unwrap();
    assert_eq!(
        helper.handle(me, Request::Forget, NOW_MS + 2_000, OPEN),
        Reply::Forgotten
    );
    assert_eq!(stand_in.pin_text(), None);
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(stand_in.journal_text().as_deref(), Some("[]"));
}

#[test]
fn forget_with_an_unreadable_journal_is_refused_until_the_journal_is_gone() {
    let stand_in = StandIn::new(0);
    drop(Helper::start(stand_in.config()));
    let mine = format!("{}\n", stand_in.my_uid());
    let file = stand_in.folder().join("journal.json");
    fs::write(&file, "not json").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let mut helper = Helper::start(stand_in.config());
    let me = holder(&stand_in, 10);
    assert_eq!(
        helper.handle(me, Request::Forget, NOW_MS, OPEN),
        Reply::Refused {
            reason: Refusal::JournalUnreadable
        }
    );
    assert_eq!(stand_in.pin_text().as_deref(), Some(mine.as_str()));
    assert_eq!(view(&mut helper, me).pin, PinView::You);
    assert_eq!(fs::read_to_string(&file).unwrap(), "not json");
    assert!(stand_in.sleep_writes().is_empty());
    fs::remove_file(&file).unwrap();
    assert_eq!(
        helper.handle(me, Request::Forget, NOW_MS + 1_000, OPEN),
        Reply::Forgotten
    );
    assert_eq!(stand_in.pin_text(), None);
    assert!(
        view(&mut helper, me).journal_ok,
        "Forget opens the journal again"
    );
}

#[test]
fn forget_that_cannot_remove_the_pin_file_is_refused_and_keeps_the_pin_in_memory() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let me = holder(&stand_in, 10);
    let mine = format!("{}\n", stand_in.my_uid());
    fs::set_permissions(stand_in.folder(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        helper.handle(me, Request::Forget, NOW_MS, OPEN),
        Reply::Refused {
            reason: Refusal::JournalUnreadable
        }
    );
    let state = view(&mut helper, me);
    assert_eq!(state.pin, PinView::You, "a pin file not removed keeps the pin");
    assert!(state.journal_ok, "the journal itself is fine, so the engine tells them apart");
    fs::set_permissions(stand_in.folder(), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(stand_in.pin_text().as_deref(), Some(mine.as_str()));
    assert_eq!(
        helper.handle(me, Request::Forget, NOW_MS + 1_000, OPEN),
        Reply::Forgotten
    );
    assert_eq!(stand_in.pin_text(), None);
    assert_eq!(view(&mut helper, me).pin, PinView::None);
}

#[test]
fn a_renewal_that_cannot_read_pmset_keeps_its_lease_until_it_runs_out_and_then_sleeps_the_mac() {
    let first = StandIn::new(0);
    let mut helper = Helper::start(refusing(&first));
    fs::write(first.path("refuse-read"), "").unwrap();
    let me = holder(&first, 10);
    let refused = helper.handle(me, HOLD_LEAVE_ON, NOW_MS, CLOSED_ALONE);
    assert_eq!(
        refused,
        Reply::Refused {
            reason: Refusal::Pmset
        }
    );
    assert!(first.sleep_writes().is_empty(), "a first hold that fails changes nothing");
    assert!(!first.calls().contains(&"sleepnow".to_string()));
    assert!(helper.idle());

    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(refusing(&stand_in));
    let me = holder(&stand_in, 10);
    assert_eq!(
        hold(&mut helper, me, NOW_MS),
        Reply::Held { lease_left_s: 120 }
    );
    fs::write(stand_in.path("refuse-read"), "").unwrap();
    let renewal = helper.handle(me, HOLD_LEAVE_ON, NOW_MS + 30_000, CLOSED_ALONE);
    assert_eq!(
        renewal,
        Reply::Refused {
            reason: Refusal::Pmset
        }
    );
    assert_eq!(
        stand_in.sleep_disabled(),
        1,
        "one unread pmset -g does not end the hold"
    );
    assert!(!helper.idle());
    helper.expire(NOW_MS + 119_999, CLOSED_ALONE);
    assert_eq!(stand_in.sleep_disabled(), 1);
    helper.expire(NOW_MS + 120_000, CLOSED_ALONE);
    assert_eq!(stand_in.sleep_disabled(), 0, "the lease was not renewed");
    assert_eq!(
        stand_in.calls().last().map(String::as_str),
        Some("sleepnow")
    );
}

#[test]
fn a_put_back_tried_again_after_a_hold_ended_sleeps_the_mac_when_the_lid_is_closed_alone() {
    for (lid, sleeps) in [(CLOSED_ALONE, true), (OPEN, false)] {
        let stand_in = StandIn::new(0);
        let mut helper = Helper::start(refusing(&stand_in));
        assert_eq!(
            hold(&mut helper, holder(&stand_in, 10), NOW_MS),
            Reply::Held { lease_left_s: 120 }
        );
        fs::write(stand_in.path("refuse-put-back"), "").unwrap();
        helper.expire(NOW_MS + 120_000, CLOSED_ALONE);
        assert_eq!(stand_in.sleep_disabled(), 1);
        assert!(!stand_in.calls().contains(&"sleepnow".to_string()));
        fs::remove_file(stand_in.path("refuse-put-back")).unwrap();
        helper.sense(NOW_MS + 150_000, Readings { lid, ..COOL });
        assert_eq!(stand_in.sleep_disabled(), 0);
        let slept = stand_in.calls().last().map(String::as_str) == Some("sleepnow");
        assert_eq!(slept, sleeps, "{lid:?}");
        helper.sense(NOW_MS + 180_000, Readings { lid, ..COOL });
        let sleepnows = stand_in.calls().iter().filter(|call| *call == "sleepnow").count();
        assert_eq!(sleepnows, usize::from(sleeps), "sleep is owed once");
    }
}

const HELD: Reply = Reply::Held { lease_left_s: 120 };
const CLOSED: Readings = Readings {
    lid: CLOSED_ALONE,
    ..COOL
};

/// A hold from `who` at `now_ms` with its display ask, the lid read as `lid`.
fn ask(
    helper: &mut Helper,
    who: Holder,
    lid_display: LidDisplay,
    now_ms: u64,
    lid: Option<Lid>,
) -> Reply {
    let request = Request::Hold {
        lease_s: 120,
        lid_display,
    };
    helper.handle(who, request, now_ms, lid)
}

/// How many times the helper ran `pmset` with `word` alone.
fn ran(stand_in: &StandIn, word: &str) -> usize {
    stand_in.calls().iter().filter(|call| *call == word).count()
}

#[test]
fn the_display_sleeps_once_per_close_while_a_lease_asks_for_it() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let me = holder(&stand_in, 10);
    assert_eq!(ask(&mut helper, me, LidDisplay::Sleep, NOW_MS, OPEN), HELD);
    assert_eq!(ran(&stand_in, "displaysleepnow"), 0, "the lid is open");
    helper.sense(NOW_MS + 30_000, CLOSED);
    assert_eq!(ran(&stand_in, "displaysleepnow"), 1, "the first reading after the close");
    helper.sense(NOW_MS + 60_000, CLOSED);
    let _ = helper.handle(me, Request::Read, NOW_MS + 61_000, CLOSED_ALONE);
    let renewed = ask(&mut helper, me, LidDisplay::Sleep, NOW_MS + 62_000, CLOSED_ALONE);
    assert_eq!(renewed, HELD);
    assert_eq!(ran(&stand_in, "displaysleepnow"), 1, "once per close (QUESTION 38d)");
    helper.sense(NOW_MS + 90_000, COOL);
    helper.sense(NOW_MS + 120_000, CLOSED);
    assert_eq!(ran(&stand_in, "displaysleepnow"), 2, "a new close");
    assert_eq!(ran(&stand_in, "sleepnow"), 0, "the hold lives");
    assert_eq!(stand_in.sleep_disabled(), 1);

    let closed = StandIn::new(0);
    let mut helper = Helper::start(closed.config());
    let me = holder(&closed, 10);
    assert_eq!(ask(&mut helper, me, LidDisplay::Sleep, NOW_MS, CLOSED_ALONE), HELD);
    assert_eq!(ran(&closed, "displaysleepnow"), 1, "a hold that starts closed, at once");
    let released = helper.handle(me, Request::Release, NOW_MS + 1_000, CLOSED_ALONE);
    assert_eq!(released, Reply::Released);
    let again = ask(&mut helper, me, LidDisplay::Sleep, NOW_MS + 2_000, CLOSED_ALONE);
    assert_eq!(again, HELD);
    assert_eq!(ran(&closed, "displaysleepnow"), 2, "a new hold counts its own close");
}

#[test]
fn the_display_is_left_on_at_a_desk_or_while_no_lease_asks_for_it() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let first = holder(&stand_in, 10);
    let second = holder(&stand_in, 11);
    let leave_on = ask(&mut helper, first, LidDisplay::LeaveOn, NOW_MS, CLOSED_ALONE);
    assert_eq!(leave_on, HELD);
    helper.sense(NOW_MS + 30_000, CLOSED);
    assert_eq!(ran(&stand_in, "displaysleepnow"), 0, "no lease asks for it");
    let sleep = ask(&mut helper, second, LidDisplay::Sleep, NOW_MS + 31_000, CLOSED_ALONE);
    assert_eq!(sleep, HELD);
    assert_eq!(ran(&stand_in, "displaysleepnow"), 1, "any live lease that asks is enough");

    let desk = StandIn::new(0);
    let mut helper = Helper::start(desk.config());
    let me = holder(&desk, 10);
    assert_eq!(ask(&mut helper, me, LidDisplay::Sleep, NOW_MS, CLOSED_AT_DESK), HELD);
    let at_desk = Readings {
        lid: CLOSED_AT_DESK,
        ..COOL
    };
    helper.sense(NOW_MS + 30_000, at_desk);
    helper.sense(NOW_MS + 60_000, Readings { lid: None, ..COOL });
    assert_eq!(
        ran(&desk, "displaysleepnow"),
        0,
        "an external display in use, or a lid not read, leaves the display on"
    );
}

#[test]
fn a_start_that_put_back_our_entry_sleeps_a_closed_mac_at_its_first_reading_with_no_lease() {
    let stand_in = StandIn::new(0);
    let mut first = Helper::start(stand_in.config());
    assert_eq!(hold(&mut first, holder(&stand_in, 10), NOW_MS), HELD);
    drop(first);
    let mut second = Helper::start(stand_in.config());
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(ran(&stand_in, "sleepnow"), 0, "the start sleeps nothing before it serves");
    assert!(second.wants_readings(), "the owed sleep keeps a reading due (QUESTION 38e)");
    assert!(!second.idle(), "and keeps the helper from idling out");
    second.sense(NOW_MS, CLOSED);
    assert_eq!(ran(&stand_in, "sleepnow"), 1);
    assert!(second.idle());
    second.sense(NOW_MS + 30_000, CLOSED);
    assert_eq!(ran(&stand_in, "sleepnow"), 1, "once");

    let open = StandIn::new(0);
    let mut first = Helper::start(open.config());
    assert_eq!(hold(&mut first, holder(&open, 10), NOW_MS), HELD);
    drop(first);
    let mut second = Helper::start(open.config());
    second.sense(NOW_MS, COOL);
    assert_eq!(ran(&open, "sleepnow"), 0, "an open lid sleeps nothing");
    assert!(second.idle(), "the mark is cleared either way");

    let clean = StandIn::new(0);
    let helper = Helper::start(clean.config());
    assert!(helper.idle(), "a start with nothing to put back owes nothing");
}

#[test]
fn a_hold_accepted_after_the_start_owes_no_sleep() {
    let stand_in = StandIn::new(0);
    let me = holder(&stand_in, 10);
    let mut first = Helper::start(stand_in.config());
    assert_eq!(hold(&mut first, me, NOW_MS), HELD);
    drop(first);
    let mut second = Helper::start(stand_in.config());
    assert_eq!(hold(&mut second, me, NOW_MS + 1_000), HELD);
    second.sense(NOW_MS + 30_000, CLOSED);
    assert_eq!(ran(&stand_in, "sleepnow"), 0, "the engine still wants the lid");
    assert_eq!(stand_in.sleep_disabled(), 1);
    assert!(view(&mut second, me).held);
}
