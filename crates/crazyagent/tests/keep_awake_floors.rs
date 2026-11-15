use crazyagent::keep_awake::power::{PowerReading, heat_unavailable, read_power};
use crazyagent::keep_awake::{Heat, LidSupport, PowerSource, power_linux, power_macos};
use crazyagent::paths::Platform;
use std::path::Path;

const THIS_MAC_ON_AC: &str = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=23593059)\t100%; charged; 0:00 remaining present: true\n";
const ON_BATTERY: &str = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23593059)\t9%; discharging; 0:41 remaining present: true\n";
const NO_BATTERY: &str = "Now drawing from 'AC Power'\n";
const ON_UPS: &str = "Now drawing from 'UPS Power'\n -Back-UPS ES 700 (id=-1234567)\t87%; discharging; 0:32 remaining present: true\n";
const ON_UPS_UNREAD: &str = "Now drawing from 'UPS Power'\n";

#[test]
fn pmset_battery_text_parses_into_source_and_percent() {
    assert_eq!(
        power_macos::battery(THIS_MAC_ON_AC),
        (PowerSource::Ac, Some(100))
    );
    assert_eq!(
        power_macos::battery(ON_BATTERY),
        (PowerSource::Battery, Some(9))
    );
    assert_eq!(power_macos::battery(NO_BATTERY), (PowerSource::Ac, None));
    assert_eq!(
        power_macos::battery(ON_UPS),
        (PowerSource::Battery, Some(87)),
        "a Mac running from a UPS is on battery, with the UPS charge"
    );
    assert_eq!(
        power_macos::battery(ON_UPS_UNREAD),
        (PowerSource::Battery, None)
    );
    assert_eq!(power_macos::battery(""), (PowerSource::Unknown, None));
    assert_eq!(
        power_macos::battery("Now drawing from 'Solar Power'\n -InternalBattery-0\t50%; x\n"),
        (PowerSource::Unknown, Some(50)),
        "a source this reader does not know is unknown"
    );
    assert_eq!(
        power_macos::battery("Now drawing from 'Battery Power'\n -InternalBattery-0\t101%; x\n"),
        (PowerSource::Battery, None),
        "a charge above 100 is not a charge"
    );
    assert_eq!(
        power_macos::battery("Now drawing from 'Battery Power'\n -InternalBattery-0 50%; x\n"),
        (PowerSource::Battery, None),
        "the charge follows a tab"
    );
    assert_eq!(
        power_macos::battery(&"x".repeat(9000)),
        (PowerSource::Unknown, None),
        "text longer than the tool prints is not read"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn this_macs_own_pmset_and_thermal_text_is_understood() {
    let batt = crazyagent::spawn_gate::output(
        std::process::Command::new("/usr/bin/pmset").args(["-g", "batt"]),
    )
    .unwrap();
    assert!(batt.status.success());
    let (source, _) = power_macos::battery(&String::from_utf8(batt.stdout).unwrap());
    assert_ne!(source, PowerSource::Unknown);

    let therm = crazyagent::spawn_gate::output(
        std::process::Command::new("/usr/bin/notifyutil").args(["-g", power_macos::THERMAL_KEY]),
    )
    .unwrap();
    assert!(therm.status.success());
    let heat = power_macos::thermal(&String::from_utf8(therm.stdout).unwrap());
    assert_ne!(heat, Heat::Unknown);

    let reading = read_power();
    assert_eq!(reading.source, source);
    assert_ne!(reading.heat, Heat::Unknown);
    assert_eq!(
        reading.lid_closed, None,
        "the Mac's lid is read in a later slice"
    );
}

#[test]
fn the_thermal_pressure_level_maps_to_heat() {
    let text = |level: &str| format!("{} {level}\n", power_macos::THERMAL_KEY);
    assert_eq!(power_macos::thermal(&text("0")), Heat::Nominal);
    assert_eq!(power_macos::thermal(&text("1")), Heat::Fair);
    assert_eq!(power_macos::thermal(&text("2")), Heat::Serious);
    assert_eq!(power_macos::thermal(&text("3")), Heat::Critical);
    assert_eq!(power_macos::thermal(&text("4")), Heat::Critical);
    assert_eq!(power_macos::thermal(&text("5")), Heat::Unknown);
    assert_eq!(power_macos::thermal(&text("")), Heat::Unknown);
    assert_eq!(power_macos::thermal(&text("-1")), Heat::Unknown);
    assert_eq!(power_macos::thermal("another.key 2\n"), Heat::Unknown);
    assert_eq!(power_macos::thermal(&text("2 3")), Heat::Unknown);
    assert_eq!(power_macos::thermal(&"2 ".repeat(5000)), Heat::Unknown);
}

fn put(folder: &Path, name: &str, value: &str) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join(name), format!("{value}\n")).unwrap();
}

#[test]
fn linux_power_supply_files_give_ac_and_percent() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    put(&root.join("AC"), "type", "Mains");
    put(&root.join("AC"), "online", "1");
    put(&root.join("BAT0"), "type", "Battery");
    put(&root.join("BAT0"), "status", "Charging");
    put(&root.join("BAT0"), "capacity", "83");
    assert_eq!(power_linux::supplies(root), (PowerSource::Ac, Some(83)));

    put(&root.join("AC"), "online", "0");
    put(&root.join("BAT0"), "status", "Discharging");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Battery, Some(83))
    );

    put(&root.join("AC"), "online", "1");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Battery, Some(83)),
        "a battery that drains with a charger attached is still draining"
    );
    put(&root.join("AC"), "online", "0");

    put(&root.join("hidpp_battery_0"), "type", "Battery");
    put(&root.join("hidpp_battery_0"), "scope", "Device");
    put(&root.join("hidpp_battery_0"), "status", "Discharging");
    put(&root.join("hidpp_battery_0"), "capacity", "5");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Battery, Some(83)),
        "a mouse battery is not the computer's"
    );

    put(&root.join("BAT1"), "type", "Battery");
    put(&root.join("BAT1"), "status", "Discharging");
    put(&root.join("BAT1"), "capacity", "17");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Battery, Some(50)),
        "two batteries give the mean"
    );

    put(&root.join("BAT1"), "capacity", "250");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Battery, Some(83)),
        "a charge above 100 is not counted"
    );
    put(&root.join("BAT1"), "capacity", &"9".repeat(200));
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Battery, Some(83)),
        "a value longer than a sysfs value is not read"
    );

    put(&root.join("BAT0"), "status", "Full");
    put(&root.join("BAT1"), "status", "Not charging");
    assert_eq!(power_linux::supplies(root).0, PowerSource::Ac);
}

#[test]
fn only_a_mouse_battery_discharging_does_not_put_a_desktop_on_battery() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    put(&root.join("hidpp_battery_0"), "type", "Battery");
    put(&root.join("hidpp_battery_0"), "scope", "Device");
    put(&root.join("hidpp_battery_0"), "status", "Discharging");
    put(&root.join("hidpp_battery_0"), "capacity", "5");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Ac, None),
        "a desktop with a wireless mouse runs on AC"
    );
}

#[test]
fn a_computer_without_a_system_battery_reads_ac() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Ac, None),
        "no supply entries at all: a desktop or a virtual machine"
    );
    assert_eq!(
        power_linux::supplies(&root.join("missing")),
        (PowerSource::Ac, None)
    );
    put(&root.join("AC"), "type", "Mains");
    put(&root.join("AC"), "online", "0");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Ac, None),
        "nothing drains where there is no battery"
    );
}

#[test]
fn a_silent_system_battery_with_nothing_online_stays_unknown() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    put(&root.join("BAT0"), "type", "Battery");
    put(&root.join("BAT0"), "capacity", "40");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Unknown, Some(40)),
        "a battery with no status file"
    );
    put(&root.join("BAT0"), "status", "Unknown");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Unknown, Some(40))
    );
    put(&root.join("AC"), "type", "Mains");
    put(&root.join("AC"), "online", "0");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Unknown, Some(40)),
        "a charger that is not online decides nothing"
    );
    put(&root.join("AC"), "online", "1");
    assert_eq!(power_linux::supplies(root), (PowerSource::Ac, Some(40)));
    put(&root.join("AC"), "online", "2");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Ac, Some(40)),
        "2 is online too: a supply that sets its own voltage"
    );
    put(&root.join("AC"), "online", "on");
    assert_eq!(
        power_linux::supplies(root),
        (PowerSource::Unknown, Some(40)),
        "a value that is no number is not online"
    );
}

fn zone(root: &Path, name: &str, temp: &str, trips: &[(&str, &str)]) {
    let folder = root.join(name);
    put(&folder, "temp", temp);
    for (index, (kind, at)) in trips.iter().enumerate() {
        put(&folder, &format!("trip_point_{index}_type"), kind);
        put(&folder, &format!("trip_point_{index}_temp"), at);
    }
}

#[test]
fn linux_trip_points_map_to_serious_and_critical() {
    let trips = [
        ("passive", "80000"),
        ("hot", "90000"),
        ("critical", "100000"),
    ];
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    assert_eq!(power_linux::heat(root), Heat::Unknown);

    zone(root, "thermal_zone0", "50000", &trips);
    assert_eq!(power_linux::heat(root), Heat::Nominal);
    zone(root, "thermal_zone0", "85000", &trips);
    assert_eq!(
        power_linux::heat(root),
        Heat::Nominal,
        "a passive trip is not heat the owner hears about"
    );
    zone(root, "thermal_zone0", "89999", &trips);
    assert_eq!(power_linux::heat(root), Heat::Nominal);
    zone(root, "thermal_zone0", "90000", &trips);
    assert_eq!(power_linux::heat(root), Heat::Serious);
    zone(root, "thermal_zone0", "100000", &trips);
    assert_eq!(power_linux::heat(root), Heat::Critical);

    zone(root, "thermal_zone0", "50000", &trips);
    zone(root, "thermal_zone1", "95000", &trips);
    assert_eq!(
        power_linux::heat(root),
        Heat::Serious,
        "the hottest zone decides"
    );
    zone(
        root,
        "thermal_zone1",
        "20000",
        &[("critical", "0"), ("hot", "-1")],
    );
    assert_eq!(
        power_linux::heat(root),
        Heat::Nominal,
        "a trip at or below zero is not a trip"
    );
    put(&root.join("cooling_device0"), "temp", "999999");
    assert_eq!(power_linux::heat(root), Heat::Nominal);
    zone(root, "thermal_zone2", "not a number", &trips);
    assert_eq!(
        power_linux::heat(root),
        Heat::Nominal,
        "a zone without a temperature is left out"
    );
}

#[test]
fn the_lid_state_files_say_open_closed_or_nothing() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    assert_eq!(power_linux::lid(root), None);
    assert_eq!(power_linux::lid(&root.join("missing")), None);

    put(&root.join("LID0"), "state", "state:      open");
    assert_eq!(power_linux::lid(root), Some(false));
    put(&root.join("LID1"), "state", "state:      closed");
    assert_eq!(
        power_linux::lid(root),
        Some(true),
        "one closed lid is a closed lid"
    );
    put(&root.join("LID1"), "state", "state:      ajar");
    assert_eq!(
        power_linux::lid(root),
        Some(false),
        "a word this reader does not know says nothing"
    );
    put(&root.join("LID0"), "state", "closed");
    assert_eq!(
        power_linux::lid(root),
        None,
        "a file that is not a state line says nothing"
    );
    let long = format!("state: closed{}", " ".repeat(200));
    put(&root.join("LID0"), "state", &long);
    assert_eq!(
        power_linux::lid(root),
        None,
        "a value longer than a sysfs value is not read"
    );
}

#[test]
fn the_linux_reader_reads_one_root_folder_and_no_bus() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let unknown = PowerReading::unknown();
    assert_ne!(unknown.lid_support, LidSupport::Held);
    assert_eq!(
        power_linux::read_under(root),
        PowerReading {
            source: PowerSource::Ac,
            ..unknown.clone()
        },
        "an empty root has no battery, and nothing else is known"
    );

    let supplies = root.join("sys/class/power_supply");
    put(&supplies.join("AC"), "type", "Mains");
    put(&supplies.join("AC"), "online", "0");
    put(&supplies.join("BAT0"), "type", "Battery");
    put(&supplies.join("BAT0"), "status", "Discharging");
    put(&supplies.join("BAT0"), "capacity", "31");
    let trips = [("hot", "90000"), ("critical", "100000")];
    zone(
        &root.join("sys/class/thermal"),
        "thermal_zone0",
        "95000",
        &trips,
    );
    put(
        &root.join("proc/acpi/button/lid/LID0"),
        "state",
        "state:      closed",
    );
    assert_eq!(
        power_linux::read_under(root),
        PowerReading {
            source: PowerSource::Battery,
            battery_pct: Some(31),
            heat: Heat::Serious,
            lid_closed: Some(true),
            lid_support: unknown.lid_support.clone(),
        },
        "without logind's seats folder the closed lid cannot be held"
    );

    let seats = root.join("run/systemd/seats");
    put(&root.join("run/systemd"), "seats", "not a folder");
    assert_eq!(
        power_linux::read_under(root).lid_support,
        unknown.lid_support,
        "a file of that name is not logind's folder"
    );
    std::fs::remove_file(&seats).unwrap();
    std::fs::create_dir(&seats).unwrap();
    assert_eq!(power_linux::read_under(root).lid_support, LidSupport::Held);

    let source = include_str!("../src/keep_awake/power_linux.rs");
    assert!(
        !source.contains("zbus"),
        "the reader opens no bus connection (rv-sec3 ka-task3 F1)"
    );
}

#[test]
fn many_other_entries_never_hide_a_thermal_zone() {
    let trips = [("critical", "100000")];
    let stands = [
        ("thermal_zone0", true),
        ("thermal_zone3", false),
        ("thermal_zone7", true),
        ("thermal_zone12", false),
    ];
    for (name, zone_first) in stands {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        if zone_first {
            zone(root, name, "100000", &trips);
        }
        for index in 0..200 {
            std::fs::create_dir(root.join(format!("cooling_device{index}"))).unwrap();
        }
        if !zone_first {
            zone(root, name, "100000", &trips);
        }
        assert_eq!(
            power_linux::heat(root),
            Heat::Critical,
            "{name} beside 200 cooling devices"
        );
    }
}

#[test]
fn with_more_zones_than_the_reader_keeps_the_first_by_name_are_read() {
    let trips = [("critical", "100000")];
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    for index in (1..70).rev() {
        zone(root, &format!("thermal_zone{index}"), "50000", &trips);
    }
    zone(root, "thermal_zone0", "100000", &trips);
    assert_eq!(
        power_linux::heat(root),
        Heat::Critical,
        "thermal_zone0 is first by name wherever the folder lists it"
    );
}

#[test]
fn windows_without_a_heat_signal_reports_unknown_and_the_setting_says_not_available() {
    assert_eq!(
        heat_unavailable(Platform::Windows),
        Some("Not available on Windows")
    );
    assert_eq!(heat_unavailable(Platform::MacOs), None);
    assert_eq!(heat_unavailable(Platform::Linux), None);
    #[cfg(windows)]
    {
        let reading = read_power();
        assert_eq!(reading.heat, Heat::Unknown);
        assert_eq!(reading.lid_closed, None);
    }
}

#[test]
fn a_reading_never_reports_a_charge_above_one_hundred() {
    let reading = read_power();
    assert!(reading.battery_pct.is_none_or(|percent| percent <= 100));
}

#[cfg(target_os = "linux")]
mod linux {
    use crazyagent::keep_awake::power::read_power;
    use crazyagent::keep_awake::{LidSupport, Want, platform_hold};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant, SystemTime};

    type Inhibitor = (String, String, String, String, u32, u32);

    const LID: &str = "handle-lid-switch";
    const SWAPS: u32 = 20;
    const SLEEP_WAIT: Duration = Duration::from_secs(120);
    const SLEPT: Duration = Duration::from_secs(3);
    const SETTLE: Duration = Duration::from_secs(2);

    /// Lists the `what` of every lock logind holds for this process under our name.
    fn ours() -> Vec<String> {
        let bus = zbus::blocking::Connection::system().unwrap();
        let reply = bus
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "ListInhibitors",
                &(),
            )
            .unwrap();
        let all: Vec<Inhibitor> = reply.body().deserialize().unwrap();
        all.into_iter()
            .filter(|lock| lock.5 == std::process::id() && lock.1 == "CrazyAgent")
            .map(|lock| lock.0)
            .collect()
    }

    /// Waits for logind to list exactly `count` of our locks: it drops a lock a moment
    /// after its descriptor closes. Input: the count. Output: whether it got there.
    fn settles_at(count: usize) -> bool {
        let end = Instant::now() + SETTLE;
        loop {
            if ours().len() == count {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn this_computers_logind_folder_makes_the_reading_say_held() {
        assert!(
            std::path::Path::new("/run/systemd/seats").is_dir(),
            "this row needs a computer with logind"
        );
        assert_eq!(read_power().lid_support, LidSupport::Held);
    }

    #[test]
    fn the_linux_lid_hold_swaps_the_lock_without_a_gap() {
        let mut hold = platform_hold(std::process::id());
        let idle = hold.apply(&Want::Idle { display: false }).unwrap();
        assert!(idle.idle);
        assert!(!idle.lid);
        assert_eq!(ours(), ["idle:sleep"]);

        let stop = Arc::new(AtomicBool::new(false));
        let gaps = Arc::new(AtomicU32::new(0));
        let watcher = {
            let (stop, gaps) = (stop.clone(), gaps.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if ours().is_empty() {
                        gaps.fetch_add(1, Ordering::SeqCst);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };
        for _ in 0..SWAPS {
            let lid = hold.apply(&Want::IdleAndLid { display: false }).unwrap();
            assert!(lid.lid, "logind gave the lid lock");
            assert!(ours().iter().any(|what| what.contains(LID)));
            let idle = hold.apply(&Want::Idle { display: false }).unwrap();
            assert!(!idle.lid);
            assert!(ours().iter().any(|what| !what.contains(LID)));
        }
        stop.store(true, Ordering::SeqCst);
        watcher.join().unwrap();
        assert_eq!(
            gaps.load(Ordering::SeqCst),
            0,
            "a lock was there at every look"
        );
        assert!(settles_at(1), "the old lock closes after each swap");
        hold.release();
        assert!(settles_at(0));
    }

    /// Live row for the owner window on the Linux laptop: close the lid within the wait
    /// after the row starts, with the computer's lid rule left at suspend. The row holds
    /// the lid, sees it closed, lets go, and then sees the wall clock run ahead of the
    /// monotonic clock, which only a suspend does. The engine asks for no suspend itself:
    /// logind looks at a closed lid again by itself (DEC-167 addendum 9).
    #[test]
    #[ignore = "live row: suspends this computer; by hand in the owner window, lid closed"]
    fn a_hold_ending_with_the_linux_lid_closed_suspends() {
        let mut hold = platform_hold(std::process::id());
        let held = hold.apply(&Want::IdleAndLid { display: false }).unwrap();
        assert!(held.lid);
        let end = Instant::now() + SLEEP_WAIT;
        while read_power().lid_closed != Some(true) {
            assert!(Instant::now() < end, "the lid was not closed in time");
            std::thread::sleep(Duration::from_millis(500));
        }
        std::thread::sleep(SLEPT);
        assert_eq!(
            read_power().lid_closed,
            Some(true),
            "the lid hold kept it awake"
        );

        let (wall, steady) = (SystemTime::now(), Instant::now());
        hold.release();
        let end = Instant::now() + SLEEP_WAIT;
        loop {
            let ahead = wall
                .elapsed()
                .unwrap_or_default()
                .saturating_sub(steady.elapsed());
            if ahead >= SLEPT {
                break;
            }
            assert!(Instant::now() < end, "the computer did not suspend");
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}
