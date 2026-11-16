#![cfg(target_os = "macos")]

mod support;

use crazyagent_mac_power::floor::{Battery, Floor, Pressure, judge};
use crazyagent_mac_power::journal::Key;
use crazyagent_mac_power::pmset::{
    Pmset, PmsetError, Values, parse_battery, parse_custom_low_power, parse_values,
};
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

/// `pmset -g custom` text with the battery section first, each source's `lowpowermode` given.
fn custom(battery: &str, ac: &str) -> String {
    format!(
        "Battery Power:\n Sleep On Power Button 1\n lowpowermode         {battery}\n \
         displaysleep         2\nAC Power:\n Sleep On Power Button 1\n \
         lowpowermode         {ac}\n displaysleep         10\n"
    )
}

#[test]
fn the_custom_text_gives_low_power_from_the_battery_section_alone() {
    assert_eq!(
        parse_custom_low_power(&custom("0", "1")),
        Some(false),
        "an AC line never counts (rv-secreview r2 note 2)"
    );
    assert_eq!(parse_custom_low_power(&custom("1", "0")), Some(true));
    let ac_first = "AC Power:\n lowpowermode         1\nBattery Power:\n lowpowermode         0\n";
    assert_eq!(parse_custom_low_power(ac_first), Some(false));
    let no_battery = "AC Power:\n lowpowermode         1\n";
    assert_eq!(parse_custom_low_power(no_battery), None);
    let without_key =
        "Battery Power:\n displaysleep         2\nAC Power:\n lowpowermode         1\n";
    assert_eq!(parse_custom_low_power(without_key), None);
    let ups = "Battery Power:\n displaysleep         2\nUPS Power:\n lowpowermode         1\n";
    assert_eq!(parse_custom_low_power(ups), None);
    assert_eq!(parse_custom_low_power(&custom("2", "1")), None);
    assert_eq!(parse_custom_low_power(""), None);
}

#[test]
fn low_power_writes_the_battery_source_and_sleep_every_source() {
    let stand_in = StandIn::new(0);
    let pmset = Pmset::new(stand_in.program());
    pmset.set(Key::LowPower, true).unwrap();
    pmset.set(Key::SleepDisabled, true).unwrap();
    pmset.set(Key::LowPower, false).unwrap();
    assert_eq!(
        stand_in.calls(),
        [
            "-b lowpowermode 1",
            "-a disablesleep 1",
            "-b lowpowermode 0"
        ],
        "r9.1 F2: Low Power Mode on the battery source only"
    );
    stand_in.set_low_power_by_owner(1, 0);
    assert_eq!(pmset.battery_low_power().unwrap(), Some(true));
    stand_in.set_low_power_by_owner(0, 1);
    assert_eq!(pmset.battery_low_power().unwrap(), Some(false));
    stand_in.drop_low_power_key();
    assert_eq!(pmset.battery_low_power().unwrap(), None);
}
