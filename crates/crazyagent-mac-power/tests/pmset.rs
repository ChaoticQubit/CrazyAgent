#![cfg(target_os = "macos")]

mod support;

use crazyagent_mac_power::floor::{Battery, Floor, Pressure, judge};
use crazyagent_mac_power::journal::Key;
use crazyagent_mac_power::pmset::{Pmset, PmsetError, Values, parse_battery, parse_values};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use support::StandIn;

#[test]
fn the_settings_text_gives_sleep_disabled_and_low_power() {
    let text = "System-wide power settings:\n SleepDisabled\t\t1\nCurrently in use:\n \
                standby              1\n lowpowermode         0\n sleep                1\n";
    let values = parse_values(text);
    assert_eq!(
        values,
        Values {
            sleep_disabled: Some(true),
            low_power: Some(false)
        }
    );
    assert_eq!(
        parse_values(" SleepDisabled\t\t0\n"),
        Values {
            sleep_disabled: Some(false),
            low_power: None
        }
    );
    assert_eq!(parse_values(" SleepDisabled\t\tyes\n"), Values::default());
    assert_eq!(parse_values(""), Values::default());
}

#[test]
fn the_battery_text_gives_the_source_and_the_percent() {
    assert_eq!(
        parse_battery(support::BATTERY_AC),
        Battery::Ac { percent: 100 }
    );
    let draining = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t3%; \
                    discharging; 0:09 remaining present: true\n";
    assert_eq!(parse_battery(draining), Battery::OnBattery { percent: 3 });
    assert_eq!(
        parse_battery("Now drawing from 'AC Power'\n"),
        Battery::None
    );
    assert_eq!(
        parse_battery("Now drawing from 'Battery Power'\n"),
        Battery::Unread
    );
    assert_eq!(parse_battery("garbage"), Battery::Unread);
    let over = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t140%; x\n";
    assert_eq!(parse_battery(over), Battery::Unread);
}

#[test]
fn the_floor_is_three_percent_on_battery_and_pressure_three_or_four_or_unread() {
    let cool = Pressure::Level(0);
    assert_eq!(
        judge(Battery::OnBattery { percent: 3 }, cool),
        Some(Floor::Battery)
    );
    assert_eq!(judge(Battery::OnBattery { percent: 4 }, cool), None);
    assert_eq!(judge(Battery::Ac { percent: 1 }, cool), None);
    assert_eq!(judge(Battery::None, cool), None);
    assert_eq!(judge(Battery::Unread, cool), None);
    for level in 0..=2 {
        assert_eq!(judge(Battery::None, Pressure::Level(level)), None);
    }
    for level in [3, 4, 5, u64::MAX] {
        assert_eq!(
            judge(Battery::None, Pressure::Level(level)),
            Some(Floor::Heat)
        );
    }
    assert_eq!(judge(Battery::None, Pressure::Unread), Some(Floor::Heat));
}

#[test]
fn pmset_runs_with_fixed_arguments_an_empty_environment_and_no_input() {
    let stand_in = StandIn::new(0);
    let probe = stand_in.path("probe");
    let text = format!(
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$*\" \"$PATH\" \"$HOME\" > '{}'\nread line\n\
         printf 'stdin:%s\\n' \"$line\" >> '{}'\nprintf ' SleepDisabled\\t\\t0\\n'\n",
        probe.display(),
        probe.display()
    );
    let program = stand_in.path("env-pmset");
    fs::write(&program, text).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let pmset = Pmset::new(&program);
    pmset.set(Key::SleepDisabled, true).unwrap();
    let seen = fs::read_to_string(&probe).unwrap();
    assert_eq!(seen, "-a disablesleep 1|/usr/bin:/bin|\nstdin:\n");
    pmset.set(Key::LowPower, false).unwrap();
    assert!(
        fs::read_to_string(&probe)
            .unwrap()
            .starts_with("-a lowpowermode 0|")
    );
}

#[test]
fn a_pmset_that_fails_or_hangs_is_an_error() {
    let stand_in = StandIn::new(0);
    let failing = stand_in.path("failing");
    fs::write(&failing, "#!/bin/sh\nexit 3\n").unwrap();
    fs::set_permissions(&failing, fs::Permissions::from_mode(0o755)).unwrap();
    let failed = Pmset::new(&failing).set(Key::SleepDisabled, true);
    assert!(
        matches!(failed, Err(PmsetError::Failed(Some(3)))),
        "{failed:?}"
    );
    let hanging = stand_in.path("hanging");
    fs::write(&hanging, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
    fs::set_permissions(&hanging, fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    let hung = Pmset::new(&hanging).values();
    assert!(matches!(hung, Err(PmsetError::TimedOut)), "{hung:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}
