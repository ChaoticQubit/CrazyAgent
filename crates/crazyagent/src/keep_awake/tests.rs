use super::*;
use crate::engine::owner::{OwnerData, OwnerMutation, OwnerRead};
use crate::protocol::{Command, Outcome};
use rusqlite::{Connection, params};

const NOW_MS: i64 = 1_800_000_000_000;
const SECOND_MS: i64 = 1000;
const MINUTE_MS: i64 = 60 * SECOND_MS;
const HOUR_MS: i64 = 60 * MINUTE_MS;

fn engine() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let connection = Connection::open(folder.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    crate::landing::number_change_sets(&connection).unwrap();
    initialize(&connection).unwrap();
    (folder, connection)
}

fn agent(connection: &Connection, status: &str) -> i64 {
    connection
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES ('p1','sub','codex',1,?1,?2,?2)",
            params![status, NOW_MS],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn mark(connection: &Connection, agent: i64, status: &str) {
    let changed = connection
        .execute(
            "UPDATE team_agents SET status=?2 WHERE id=?1",
            params![agent, status],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

fn research(connection: &Connection, status: &str) {
    connection
        .execute(
            "INSERT INTO research_jobs(project,kind,harness,tools,status,created_ms)
             VALUES ('p1','market','codex',1,?1,?2)",
            params![status, NOW_MS],
        )
        .unwrap();
}

fn change_set(connection: &Connection, id: &str, state: &str) {
    connection
        .execute(
            "INSERT INTO landing_change_sets(id,project,agent_kind,agent_id,label,repository,
                 worktree,git_dir,base,head,state,last_outcome,turn,prompt_line,files,added,
                 removed,conflicts,created_ms,updated_ms)
             VALUES (?1,'p1','sub','7','Agent','/r',?2,'/g','b','h',?3,'succeeded','t','line',
                 1,1,0,'[]',?4,?4)",
            params![id, format!("/w/{id}"), state, NOW_MS],
        )
        .unwrap();
}

fn master_turn(connection: &Connection, ordinal: i64, phase: &str) {
    connection
        .execute(
            "INSERT INTO master_turns(session,ordinal,id,input,phase,body)
             VALUES ('s1',?1,?2,?3,?4,'{}')",
            params![ordinal, format!("turn-{ordinal}"), ordinal + 100, phase],
        )
        .unwrap();
}

fn act(connection: &Connection, action: KeepAwakeAction, now_ms: i64) -> Outcome {
    apply(connection, &action, now_ms).unwrap()
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

fn set(connection: &Connection, settings: KeepAwakeSettings) {
    let outcome = act(connection, KeepAwakeAction::Set { settings }, NOW_MS);
    assert_eq!(outcome, Outcome::Applied);
}

fn sensors(power: PowerSource, battery_pct: Option<u8>) -> Sensors {
    Sensors {
        power,
        battery_pct,
        battery_unread_twice: false,
        heat: Heat::Nominal,
        lid_closed: Some(false),
        lid_support: LidSupport::Held,
    }
}

fn working(sensors: Sensors) -> Inputs {
    Inputs {
        now_ms: NOW_MS,
        work: 1,
        manual_until_ms: None,
        hold_since_ms: Some(NOW_MS - MINUTE_MS),
        work_last_ms: Some(NOW_MS),
        stood_down: false,
        sensors,
    }
}

const IDLE: Want = Want::Idle { display: false };
const IDLE_AND_LID: Want = Want::IdleAndLid { display: false };

#[test]
fn auto_holds_while_a_team_agent_works_and_releases_after_the_grace_period() {
    let (_folder, connection) = engine();
    assert_eq!(view(&connection).unwrap().settings, KeepAwakeSettings::DEFAULT);
    let nobody = evaluate(&connection, NOW_MS, None).unwrap();
    assert_eq!(nobody.state.want, Want::Nothing);
    assert_eq!(nobody.work, 0);
    let worker = agent(&connection, "working");
    let last_ms = NOW_MS + SECOND_MS;
    let held = evaluate(&connection, last_ms, None).unwrap();
    assert_eq!(held.state.want, IDLE);
    assert_eq!(held.state.reason, Reason::Work);
    assert_eq!(held.state.ended, None);
    assert_eq!(held.work, 1);
    assert_eq!(view(&connection).unwrap(), held);
    mark(&connection, worker, "idle");
    let grace = evaluate(&connection, last_ms + SECOND_MS, None).unwrap();
    assert_eq!(grace.state.want, IDLE);
    assert_eq!(grace.state.reason, Reason::Grace);
    assert_eq!(grace.state.ends_at_ms, Some(last_ms + 10 * MINUTE_MS));
    assert_eq!(grace.work, 0);
    let late = evaluate(&connection, last_ms + 10 * MINUTE_MS - 1, None).unwrap();
    assert_eq!(late.state.want, IDLE);
    let released = evaluate(&connection, last_ms + 10 * MINUTE_MS, None).unwrap();
    assert_eq!(released.state.want, Want::Nothing);
    assert_eq!(released.state.reason, Reason::Idle);
    assert_eq!(released.state.ended, None);
    assert_eq!(view(&connection).unwrap(), released);
}

#[test]
fn manual_hold_ends_at_its_deadline_and_extend_moves_it() {
    let (_folder, connection) = engine();
    for minutes in [0, 1441] {
        assert!(KeepAwakeAction::Start { minutes }.validate().is_err());
        assert!(KeepAwakeAction::Extend { minutes }.validate().is_err());
    }
    assert_eq!(
        act(&connection, KeepAwakeAction::Extend { minutes: 30 }, NOW_MS),
        rejected("keep_awake_no_timer")
    );
    assert_eq!(
        act(&connection, KeepAwakeAction::End, NOW_MS),
        rejected("keep_awake_not_holding")
    );
    let start = KeepAwakeAction::Start { minutes: 60 };
    assert_eq!(act(&connection, start, NOW_MS), Outcome::Applied);
    let held = view(&connection).unwrap();
    assert_eq!(held.state.want, IDLE);
    assert_eq!(held.state.reason, Reason::Timer);
    assert_eq!(held.state.ends_at_ms, Some(NOW_MS + HOUR_MS));
    let extend = KeepAwakeAction::Extend { minutes: 30 };
    assert_eq!(act(&connection, extend, NOW_MS + MINUTE_MS), Outcome::Applied);
    let moved = view(&connection).unwrap();
    assert_eq!(moved.state.ends_at_ms, Some(NOW_MS + 90 * MINUTE_MS));
    let far = KeepAwakeAction::Extend { minutes: 1440 };
    assert_eq!(
        act(&connection, far, NOW_MS + MINUTE_MS),
        rejected("keep_awake_minutes_invalid")
    );
    let before = evaluate(&connection, NOW_MS + HOUR_MS, None).unwrap();
    assert_eq!(before.state.want, IDLE);
    let ended = evaluate(&connection, NOW_MS + 90 * MINUTE_MS, None).unwrap();
    assert_eq!(ended.state.want, Want::Nothing);
    assert_eq!(ended.state.ended, Some(EndReason::Manual));
    let later = evaluate(&connection, NOW_MS + 91 * MINUTE_MS, None).unwrap();
    assert_eq!(later.state.want, Want::Nothing);
    assert_eq!(later.state.ended, Some(EndReason::Manual));
    assert_eq!(later.state.ends_at_ms, None);
}

#[test]
fn a_timer_in_auto_holds_until_the_later_of_the_timer_and_the_work() {
    let (_folder, connection) = engine();
    let worker = agent(&connection, "working");
    let start = KeepAwakeAction::Start { minutes: 1 };
    assert_eq!(act(&connection, start, NOW_MS), Outcome::Applied);
    let both = view(&connection).unwrap();
    assert_eq!(both.state.want, IDLE);
    assert_eq!(both.state.reason, Reason::Work);
    let after_timer = evaluate(&connection, NOW_MS + 2 * MINUTE_MS, None).unwrap();
    assert_eq!(after_timer.state.want, IDLE);
    assert_eq!(after_timer.state.ended, None);
    mark(&connection, worker, "idle");
    let start = KeepAwakeAction::Start { minutes: 60 };
    assert_eq!(act(&connection, start, NOW_MS + 3 * MINUTE_MS), Outcome::Applied);
    let after_grace = evaluate(&connection, NOW_MS + 30 * MINUTE_MS, None).unwrap();
    assert_eq!(after_grace.state.want, IDLE);
    assert_eq!(after_grace.state.reason, Reason::Timer);
    assert_eq!(after_grace.state.ends_at_ms, Some(NOW_MS + 63 * MINUTE_MS));
    assert_eq!(act(&connection, KeepAwakeAction::End, NOW_MS + 31 * MINUTE_MS), Outcome::Applied);
    let ended = view(&connection).unwrap();
    assert_eq!(ended.state.want, Want::Nothing);
    assert_eq!(ended.state.ended, Some(EndReason::Manual));
}

#[test]
fn end_in_auto_stands_down_until_the_running_work_finishes() {
    let (_folder, connection) = engine();
    let worker = agent(&connection, "working");
    assert_eq!(evaluate(&connection, NOW_MS, None).unwrap().state.want, IDLE);
    assert_eq!(act(&connection, KeepAwakeAction::End, NOW_MS + SECOND_MS), Outcome::Applied);
    let ended = view(&connection).unwrap();
    assert_eq!(ended.state.want, Want::Nothing);
    assert_eq!(ended.state.ended, Some(EndReason::Manual));
    let still = evaluate(&connection, NOW_MS + MINUTE_MS, None).unwrap();
    assert_eq!(still.state.want, Want::Nothing);
    assert_eq!(still.state.ended, Some(EndReason::Manual));
    mark(&connection, worker, "idle");
    let no_grace = evaluate(&connection, NOW_MS + 2 * MINUTE_MS, None).unwrap();
    assert_eq!(no_grace.state.want, Want::Nothing);
    mark(&connection, worker, "working");
    let again = evaluate(&connection, NOW_MS + 3 * MINUTE_MS, None).unwrap();
    assert_eq!(again.state.want, IDLE);
    assert_eq!(again.state.ended, None);
}

#[test]
fn off_never_holds_even_with_work() {
    let (_folder, connection) = engine();
    agent(&connection, "working");
    assert_eq!(evaluate(&connection, NOW_MS, None).unwrap().state.want, IDLE);
    set(
        &connection,
        KeepAwakeSettings {
            mode: Mode::Off,
            ..KeepAwakeSettings::DEFAULT
        },
    );
    let off = view(&connection).unwrap();
    assert_eq!(off.state.want, Want::Nothing);
    assert_eq!(off.state.reason, Reason::Off);
    assert_eq!(off.state.ended, Some(EndReason::Off));
    assert_eq!(off.work, 1);
    assert_eq!(
        act(&connection, KeepAwakeAction::Start { minutes: 60 }, NOW_MS),
        rejected("keep_awake_off")
    );
    let later = evaluate(&connection, NOW_MS + HOUR_MS, None).unwrap();
    assert_eq!(later.state.want, Want::Nothing);
    assert_eq!(later.state.reason, Reason::Off);
    let manual = KeepAwakeSettings {
        mode: Mode::Manual,
        ..KeepAwakeSettings::DEFAULT
    };
    set(&connection, manual);
    let waiting = view(&connection).unwrap();
    assert_eq!(waiting.state.want, Want::Nothing, "Manual ignores running work");
    assert_eq!(waiting.state.reason, Reason::Idle);
}

#[test]
fn the_cap_ends_a_hold_that_runs_longer_than_the_cap() {
    let (_folder, connection) = engine();
    set(
        &connection,
        KeepAwakeSettings {
            cap: Cap::H4,
            ..KeepAwakeSettings::DEFAULT
        },
    );
    let worker = agent(&connection, "working");
    let held = evaluate(&connection, NOW_MS, None).unwrap();
    assert_eq!(held.state.want, IDLE);
    assert_eq!(held.state.ends_at_ms, Some(NOW_MS + 4 * HOUR_MS));
    let nearly = evaluate(&connection, NOW_MS + 4 * HOUR_MS - 1, None).unwrap();
    assert_eq!(nearly.state.want, IDLE);
    let capped = evaluate(&connection, NOW_MS + 4 * HOUR_MS, None).unwrap();
    assert_eq!(capped.state.want, Want::Nothing);
    assert_eq!(capped.state.ended, Some(EndReason::Cap));
    let still = evaluate(&connection, NOW_MS + 4 * HOUR_MS + 5 * SECOND_MS, None).unwrap();
    assert_eq!(still.state.want, Want::Nothing, "Auto stands down while that work runs");
    assert_eq!(still.state.ended, Some(EndReason::Cap));
    mark(&connection, worker, "idle");
    let rested = evaluate(&connection, NOW_MS + 5 * HOUR_MS, None).unwrap();
    assert_eq!(rested.state.want, Want::Nothing, "no grace follows a cap");
    mark(&connection, worker, "working");
    let again = evaluate(&connection, NOW_MS + 6 * HOUR_MS, None).unwrap();
    assert_eq!(again.state.want, IDLE);
    assert_eq!(again.state.ends_at_ms, Some(NOW_MS + 10 * HOUR_MS));
    let none = KeepAwakeSettings {
        cap: Cap::None,
        ..KeepAwakeSettings::DEFAULT
    };
    let mut long = working(sensors(PowerSource::Ac, None));
    long.hold_since_ms = Some(NOW_MS - 1000 * HOUR_MS);
    assert_eq!(decide(&none, &long).want, IDLE_AND_LID);
    assert_eq!(decide(&none, &long).ends_at_ms, None);
    let timer = KeepAwakeAction::Start { minutes: 480 };
    assert_eq!(act(&connection, timer, NOW_MS + 6 * HOUR_MS), Outcome::Applied);
    let capped_timer = evaluate(&connection, NOW_MS + 10 * HOUR_MS, None).unwrap();
    assert_eq!(capped_timer.state.want, Want::Nothing);
    assert_eq!(capped_timer.state.ended, Some(EndReason::Cap));
    let after = evaluate(&connection, NOW_MS + 11 * HOUR_MS, None).unwrap();
    assert_eq!(after.state.want, Want::Nothing, "the cap also drops the timer");
}

#[test]
fn on_battery_the_ac_only_lid_rule_drops_to_idle_only() {
    let settings = KeepAwakeSettings::DEFAULT;
    assert_eq!(settings.lid, LidRule::AcOnly);
    let on_ac = working(sensors(PowerSource::Ac, Some(80)));
    assert_eq!(decide(&settings, &on_ac).want, IDLE_AND_LID);
    assert_eq!(lid_status(&settings, &on_ac.sensors), LidSupport::Held);
    for power in [PowerSource::Battery, PowerSource::Unknown] {
        let inputs = working(sensors(power, Some(80)));
        assert_eq!(decide(&settings, &inputs).want, IDLE);
        assert_eq!(
            lid_status(&settings, &inputs.sensors),
            LidSupport::IdleOnlyOnBattery
        );
    }
    let on_battery = working(sensors(PowerSource::Battery, Some(80)));
    let always = KeepAwakeSettings {
        lid: LidRule::Always,
        ..settings
    };
    assert_eq!(decide(&always, &on_battery).want, IDLE_AND_LID);
    let off = KeepAwakeSettings {
        lid: LidRule::Off,
        ..settings
    };
    assert_eq!(decide(&off, &on_ac).want, IDLE);
    assert_eq!(lid_status(&off, &on_ac.sensors), LidSupport::Off);
    let mut helpless = on_ac.clone();
    helpless.sensors.lid_support = LidSupport::NeedsApproval;
    assert_eq!(decide(&always, &helpless).want, IDLE);
    assert_eq!(
        lid_status(&always, &helpless.sensors),
        LidSupport::NeedsApproval
    );
    let shown = KeepAwakeSettings {
        display_awake: DisplayAwake::WhileHolding,
        ..settings
    };
    assert_eq!(decide(&shown, &on_ac).want, Want::IdleAndLid { display: true });
    let mut closed = on_ac.clone();
    closed.sensors.lid_closed = Some(true);
    assert_eq!(decide(&shown, &closed).want, IDLE_AND_LID);
}

#[test]
fn battery_below_the_floor_ends_the_hold_and_says_why() {
    let settings = KeepAwakeSettings::DEFAULT;
    assert_eq!(settings.battery_floor, BatteryFloor::Pct10);
    for pct in [9, 10] {
        let low = decide(&settings, &working(sensors(PowerSource::Battery, Some(pct))));
        assert_eq!(low.want, Want::Nothing);
        assert_eq!(low.ended, Some(EndReason::Battery { pct }));
        assert_eq!(low.ends_at_ms, None);
    }
    let above = decide(&settings, &working(sensors(PowerSource::Battery, Some(11))));
    assert_eq!(above.want, IDLE);
    assert_eq!(above.ended, None);
    let plugged = decide(&settings, &working(sensors(PowerSource::Ac, Some(3))));
    assert_eq!(plugged.want, IDLE_AND_LID);
    let floors = [
        (BatteryFloor::Off, None),
        (BatteryFloor::Pct5, Some(5)),
        (BatteryFloor::Pct10, Some(10)),
        (BatteryFloor::Pct20, Some(20)),
    ];
    assert_eq!(floors.len(), BatteryFloor::ALL.len());
    for (battery_floor, percent) in floors {
        assert_eq!(battery_floor.percent(), percent);
        let chosen = KeepAwakeSettings {
            battery_floor,
            ..settings
        };
        for pct in [1, 5, 6, 10, 11, 20, 21] {
            let decision = decide(&chosen, &working(sensors(PowerSource::Battery, Some(pct))));
            let ends = percent.is_some_and(|floor| pct <= floor);
            assert_eq!(decision.want == Want::Nothing, ends, "{battery_floor:?} at {pct}");
        }
    }
}

#[test]
fn an_unreadable_battery_twice_on_battery_ends_the_hold() {
    let settings = KeepAwakeSettings::DEFAULT;
    let mut once = working(sensors(PowerSource::Battery, None));
    assert_eq!(decide(&settings, &once).want, IDLE);
    once.sensors.battery_unread_twice = true;
    let twice = decide(&settings, &once);
    assert_eq!(twice.want, Want::Nothing);
    assert_eq!(twice.ended, Some(EndReason::BatteryUnread));
    let mut desk = working(sensors(PowerSource::Ac, None));
    desk.sensors.battery_unread_twice = true;
    let no_battery = decide(&settings, &desk);
    assert_eq!(no_battery.want, IDLE_AND_LID, "a Mac with no battery keeps its hold");
    assert_eq!(no_battery.ended, None);
    let mut unknown = working(sensors(PowerSource::Unknown, None));
    unknown.sensors.battery_unread_twice = true;
    assert_eq!(decide(&settings, &unknown).want, IDLE);
    let no_floor = KeepAwakeSettings {
        battery_floor: BatteryFloor::Off,
        ..settings
    };
    assert_eq!(decide(&no_floor, &once).want, IDLE);
}

#[test]
fn critical_heat_ends_and_serious_heat_only_notices_by_default() {
    let settings = KeepAwakeSettings::DEFAULT;
    assert_eq!(settings.heat_end, HeatEnd::Critical);
    assert_eq!(settings.heat_notice, HeatNotice::Serious);
    let at = |heat: Heat| {
        let mut inputs = working(sensors(PowerSource::Ac, Some(90)));
        inputs.sensors.heat = heat;
        inputs
    };
    let critical = decide(&settings, &at(Heat::Critical));
    assert_eq!(critical.want, Want::Nothing);
    assert_eq!(critical.ended, Some(EndReason::Heat));
    let serious = decide(&settings, &at(Heat::Serious));
    assert_eq!(serious.want, IDLE_AND_LID);
    assert_eq!(serious.ended, None);
    assert!(heat_notice(&settings, &at(Heat::Serious).sensors));
    assert!(heat_notice(&settings, &at(Heat::Critical).sensors));
    for cool in [Heat::Nominal, Heat::Fair, Heat::Unknown] {
        assert_eq!(decide(&settings, &at(cool)).want, IDLE_AND_LID);
        assert!(!heat_notice(&settings, &at(cool).sensors));
    }
    let quiet = KeepAwakeSettings {
        heat_notice: HeatNotice::Off,
        ..settings
    };
    assert!(!heat_notice(&quiet, &at(Heat::Critical).sensors));
    let early = KeepAwakeSettings {
        heat_end: HeatEnd::Serious,
        ..settings
    };
    assert_eq!(decide(&early, &at(Heat::Serious)).ended, Some(EndReason::Heat));
    assert_eq!(decide(&early, &at(Heat::Fair)).want, IDLE_AND_LID);
    let never = KeepAwakeSettings {
        heat_end: HeatEnd::Never,
        ..settings
    };
    assert_eq!(decide(&never, &at(Heat::Critical)).want, IDLE_AND_LID);
}

fn every_settings_value() -> Vec<KeepAwakeSettings> {
    let base = KeepAwakeSettings::DEFAULT;
    let mut all = vec![base];
    all.extend(Mode::ALL.map(|mode| KeepAwakeSettings { mode, ..base }));
    all.extend(Grace::ALL.map(|grace| KeepAwakeSettings { grace, ..base }));
    all.extend(Cap::ALL.map(|cap| KeepAwakeSettings { cap, ..base }));
    all.extend(LidRule::ALL.map(|lid| KeepAwakeSettings { lid, ..base }));
    all.extend(BatteryFloor::ALL.map(|battery_floor| KeepAwakeSettings {
        battery_floor,
        ..base
    }));
    all.extend(HeatEnd::ALL.map(|heat_end| KeepAwakeSettings { heat_end, ..base }));
    all.extend(HeatNotice::ALL.map(|heat_notice| KeepAwakeSettings {
        heat_notice,
        ..base
    }));
    all.extend(LidDisplay::ALL.map(|lid_display| KeepAwakeSettings {
        lid_display,
        ..base
    }));
    all.extend(DisplayAwake::ALL.map(|display_awake| KeepAwakeSettings {
        display_awake,
        ..base
    }));
    all
}

#[test]
fn every_setting_option_round_trips_through_the_owner_protocol() {
    let (folder, connection) = engine();
    assert_eq!(
        KeepAwakeSettings::DEFAULT,
        KeepAwakeSettings {
            mode: Mode::Auto,
            grace: Grace::Min10,
            cap: Cap::H24,
            lid: LidRule::AcOnly,
            battery_floor: BatteryFloor::Pct10,
            heat_end: HeatEnd::Critical,
            heat_notice: HeatNotice::Serious,
            lid_display: LidDisplay::Sleep,
            display_awake: DisplayAwake::Off,
        }
    );
    let all = every_settings_value();
    assert_eq!(all.len(), 1 + 3 + 4 + 4 + 3 + 4 + 3 + 2 + 2 + 2);
    for settings in all {
        let mutation = OwnerMutation::KeepAwake {
            action: KeepAwakeAction::Set { settings },
        };
        let wire = serde_json::to_string(&mutation).unwrap();
        let sent: OwnerMutation = serde_json::from_str(&wire).unwrap();
        assert_eq!(sent, mutation);
        let Command::OwnerKeepAwake { action } = sent.command().unwrap() else {
            panic!("a Keep awake change is its own command");
        };
        assert_eq!(apply(&connection, &action, NOW_MS).unwrap(), Outcome::Applied);
        assert_eq!(OwnerRead::KeepAwake.command().unwrap(), None);
        let reader = Connection::open_with_flags(
            folder.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let data = read(&reader).unwrap();
        let wire = serde_json::to_string(&data).unwrap();
        let OwnerData::KeepAwake { view } = serde_json::from_str(&wire).unwrap() else {
            panic!("the Keep awake read answers with its view");
        };
        assert_eq!(view.settings, settings);
        assert_eq!(view.platform, crate::paths::Platform::CURRENT);
    }
    let unknown = r#"{"kind":"KeepAwake","action":{"set":{"settings":{"mode":"sometimes"}}}}"#;
    assert!(serde_json::from_str::<OwnerMutation>(unknown).is_err());
    let short = OwnerMutation::KeepAwake {
        action: KeepAwakeAction::Start { minutes: 0 },
    };
    assert!(short.command().is_err());
    connection
        .execute(
            "UPDATE configured_keep_awake SET settings='{\"mode\":\"sometimes\"}'",
            [],
        )
        .unwrap();
    assert_eq!(
        view(&connection).unwrap_err().0,
        "keep_awake_settings_invalid"
    );
}

#[test]
fn the_work_count_adds_agents_research_landings_and_master_turns() {
    let bare = Connection::open_in_memory().unwrap();
    assert_eq!(work::count(&bare).unwrap(), 0, "no tables yet means no work");
    let (_folder, connection) = engine();
    assert_eq!(work::count(&connection).unwrap(), 0);
    agent(&connection, "working");
    for status in ["starting", "idle", "stopped", "failed"] {
        agent(&connection, status);
    }
    assert_eq!(work::count(&connection).unwrap(), 1);
    research(&connection, "running");
    research(&connection, "done");
    assert_eq!(work::count(&connection).unwrap(), 2);
    change_set(&connection, "c1", "updating");
    change_set(&connection, "c2", "pending");
    change_set(&connection, "c3", "accepted");
    assert_eq!(work::count(&connection).unwrap(), 3);
    let phases = [
        ("queued", true),
        ("blocked", false),
        ("prepared", true),
        ("delivering", true),
        ("running", true),
        ("waiting_approval", false),
        ("succeeded", false),
        ("failed", false),
        ("cancelled", false),
        ("uncertain", false),
    ];
    let mut expected = 3;
    for (ordinal, (phase, counts)) in phases.into_iter().enumerate() {
        master_turn(&connection, ordinal as i64 + 1, phase);
        expected += u32::from(counts);
        assert_eq!(work::count(&connection).unwrap(), expected, "{phase}");
    }
    assert_eq!(expected, 7);
    assert_eq!(evaluate(&connection, NOW_MS, None).unwrap().work, 7);
}

#[test]
fn a_restarted_engine_starts_with_no_hold() {
    let (_folder, connection) = engine();
    let start = KeepAwakeAction::Start { minutes: 60 };
    assert_eq!(act(&connection, start, NOW_MS), Outcome::Applied);
    let chosen = KeepAwakeSettings {
        grace: Grace::Min30,
        ..KeepAwakeSettings::DEFAULT
    };
    set(&connection, chosen);
    assert_eq!(view(&connection).unwrap().state.want, IDLE);
    engine_started(&connection).unwrap();
    let fresh = view(&connection).unwrap();
    assert_eq!(fresh.state.want, Want::Nothing);
    assert_eq!(fresh.state.ended, None);
    assert_eq!(fresh.state.ends_at_ms, None);
    assert_eq!(fresh.settings, chosen, "settings outlive a restart");
    let later = evaluate(&connection, NOW_MS + MINUTE_MS, None).unwrap();
    assert_eq!(later.state.want, Want::Nothing, "the old timer is gone");
}

#[test]
fn the_keeper_evaluates_at_once_on_a_change_and_every_five_seconds() {
    let (_folder, connection) = engine();
    let mut keeper = Keeper::default();
    assert!(keeper.tick(&connection, NOW_MS).unwrap().is_some());
    assert!(keeper.tick(&connection, NOW_MS + 250).unwrap().is_none());
    agent(&connection, "working");
    let seen = keeper.tick(&connection, NOW_MS + 500).unwrap();
    assert_eq!(seen.unwrap().state.want, IDLE, "a work change is seen at once");
    assert!(keeper.tick(&connection, NOW_MS + 750).unwrap().is_none());
    assert_eq!(act(&connection, KeepAwakeAction::End, NOW_MS + 800), Outcome::Applied);
    let ended = keeper.tick(&connection, NOW_MS + 1000).unwrap();
    assert_eq!(ended.unwrap().state.want, Want::Nothing, "an owner change is seen at once");
    assert!(keeper.tick(&connection, NOW_MS + 1250).unwrap().is_none());
    assert!(keeper.tick(&connection, NOW_MS + 1000 + TICK_MS - 1).unwrap().is_none());
    assert!(keeper.tick(&connection, NOW_MS + 1000 + TICK_MS).unwrap().is_some());
}
