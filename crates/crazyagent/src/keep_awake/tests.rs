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

/// Adds one master turn in a phase, in a session of its own, since a session holds at most
/// one outstanding turn (storage/master_session.rs `master_outstanding`). Inputs: the
/// engine connection, the turn's number and its phase.
fn master_turn(connection: &Connection, ordinal: i64, phase: &str) {
    assert!(ordinal > 0);
    connection
        .execute(
            "INSERT INTO master_turns(session,ordinal,id,input,phase,body)
             VALUES (?1,1,?2,?3,?4,'{}')",
            params![
                format!("s{ordinal}"),
                format!("turn-{ordinal}"),
                ordinal + 100,
                phase
            ],
        )
        .unwrap();
}

/// Applies one owner change the way the owner path does (storage.rs): inside an open
/// transaction, which `apply` requires, then commits it. Inputs: the engine connection,
/// the change and the time. Output: the outcome.
fn act(connection: &Connection, action: KeepAwakeAction, now_ms: i64) -> Outcome {
    let tx = connection.unchecked_transaction().unwrap();
    let outcome = apply(&tx, &action, now_ms).unwrap();
    tx.commit().unwrap();
    assert!(connection.is_autocommit());
    outcome
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

/// The save a client sends after reading: the settings over the version it read. Inputs:
/// the engine connection and the settings. Output: the change.
fn saved(connection: &Connection, settings: KeepAwakeSettings) -> KeepAwakeAction {
    KeepAwakeAction::Set {
        settings,
        version: view(connection).unwrap().version,
    }
}

fn set(connection: &Connection, settings: KeepAwakeSettings) {
    let outcome = act(connection, saved(connection, settings), NOW_MS);
    assert_eq!(outcome, Outcome::Applied);
}

fn sensors(power: PowerSource, battery_pct: Option<u8>) -> Sensors {
    Sensors {
        power,
        battery_pct,
        battery_unread_twice: false,
        heat: Heat::Nominal,
        lid_closed: Some(false),
        external_display: Some(false),
        lid_support: LidSupport::Held,
        low_power: LowPowerSupport::Unread,
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
    assert_eq!(
        view(&connection).unwrap().settings,
        KeepAwakeSettings::DEFAULT
    );
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
    let gone_ms = last_ms + SECOND_MS;
    let grace = evaluate(&connection, gone_ms, None).unwrap();
    assert_eq!(grace.state.want, IDLE);
    assert_eq!(grace.state.reason, Reason::Grace);
    assert_eq!(grace.state.ends_at_ms, Some(gone_ms + 10 * MINUTE_MS));
    assert_eq!(grace.work, 0);
    let late = evaluate(&connection, gone_ms + 10 * MINUTE_MS - 1, None).unwrap();
    assert_eq!(late.state.want, IDLE);
    assert_eq!(late.state.ends_at_ms, Some(gone_ms + 10 * MINUTE_MS));
    let released = evaluate(&connection, gone_ms + 10 * MINUTE_MS, None).unwrap();
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
    assert_eq!(
        act(&connection, extend, NOW_MS + MINUTE_MS),
        Outcome::Applied
    );
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
    assert_eq!(act(&connection, start.clone(), NOW_MS), Outcome::Applied);
    let both = view(&connection).unwrap();
    assert_eq!(both.state.want, IDLE);
    assert_eq!(both.state.reason, Reason::Work);
    let after_timer = evaluate(&connection, NOW_MS + 2 * MINUTE_MS, None).unwrap();
    assert_eq!(after_timer.state.want, IDLE);
    assert_eq!(after_timer.state.ended, None);
    mark(&connection, worker, "idle");
    let start = KeepAwakeAction::Start { minutes: 60 };
    assert_eq!(
        act(&connection, start, NOW_MS + 3 * MINUTE_MS),
        Outcome::Applied
    );
    let after_grace = evaluate(&connection, NOW_MS + 30 * MINUTE_MS, None).unwrap();
    assert_eq!(after_grace.state.want, IDLE);
    assert_eq!(after_grace.state.reason, Reason::Timer);
    assert_eq!(after_grace.state.ends_at_ms, Some(NOW_MS + 63 * MINUTE_MS));
    assert_eq!(
        act(&connection, KeepAwakeAction::End, NOW_MS + 31 * MINUTE_MS),
        Outcome::Applied
    );
    let ended = view(&connection).unwrap();
    assert_eq!(ended.state.want, Want::Nothing);
    assert_eq!(ended.state.ended, Some(EndReason::Manual));
}

#[test]
fn end_in_auto_stands_down_until_the_running_work_finishes() {
    let (_folder, connection) = engine();
    let worker = agent(&connection, "working");
    assert_eq!(
        evaluate(&connection, NOW_MS, None).unwrap().state.want,
        IDLE
    );
    assert_eq!(
        act(&connection, KeepAwakeAction::End, NOW_MS + SECOND_MS),
        Outcome::Applied
    );
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
    assert_eq!(
        evaluate(&connection, NOW_MS, None).unwrap().state.want,
        IDLE
    );
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
    assert_eq!(
        waiting.state.want,
        Want::Nothing,
        "Manual ignores running work"
    );
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
    assert_eq!(capped.state.ended, Some(EndReason::Cap { hours: 4 }));
    let still = evaluate(&connection, NOW_MS + 4 * HOUR_MS + 5 * SECOND_MS, None).unwrap();
    assert_eq!(
        still.state.want,
        Want::Nothing,
        "Auto stands down while that work runs"
    );
    assert_eq!(still.state.ended, Some(EndReason::Cap { hours: 4 }));
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
    assert_eq!(
        act(&connection, timer, NOW_MS + 6 * HOUR_MS),
        Outcome::Applied
    );
    let capped_timer = evaluate(&connection, NOW_MS + 10 * HOUR_MS, None).unwrap();
    assert_eq!(capped_timer.state.want, Want::Nothing);
    assert_eq!(capped_timer.state.ended, Some(EndReason::Cap { hours: 4 }));
    let after = evaluate(&connection, NOW_MS + 11 * HOUR_MS, None).unwrap();
    assert_eq!(
        after.state.want,
        Want::Nothing,
        "the cap also drops the timer"
    );
}

#[test]
fn a_cap_end_names_the_cap_the_hold_ran_with_after_the_setting_changes() {
    for (cap, hours) in [(Cap::H4, 4_u8), (Cap::H8, 8), (Cap::H24, 24)] {
        let (_folder, connection) = engine();
        let ran_with = KeepAwakeSettings {
            cap,
            ..KeepAwakeSettings::DEFAULT
        };
        set(&connection, ran_with);
        agent(&connection, "working");
        assert_eq!(
            evaluate(&connection, NOW_MS, None).unwrap().state.want,
            IDLE
        );
        let end_ms = NOW_MS + i64::from(hours) * HOUR_MS;
        let capped = evaluate(&connection, end_ms, None).unwrap();
        assert_eq!(capped.state.ended, Some(EndReason::Cap { hours }));
        for later in [Cap::H4, Cap::H8, Cap::H24, Cap::None] {
            let outcome = act(
                &connection,
                saved(
                    &connection,
                    KeepAwakeSettings {
                        cap: later,
                        ..ran_with
                    },
                ),
                end_ms + SECOND_MS,
            );
            assert_eq!(outcome, Outcome::Applied);
            let shown = evaluate(&connection, end_ms + 2 * SECOND_MS, None).unwrap();
            assert_eq!(shown.settings.cap, later);
            assert_eq!(
                shown.state.ended,
                Some(EndReason::Cap { hours }),
                "{cap:?} then {later:?}"
            );
        }
    }
    let text = serde_json::to_string(&EndReason::Cap { hours: 24 }).unwrap();
    assert_eq!(text, r#"{"kind":"cap","hours":24}"#);
    assert_eq!(Cap::None.hours(), None);
    assert_eq!(Cap::H8.ms(), Some(8 * HOUR_MS));
}

#[test]
fn every_refusal_of_an_owner_change_has_plain_words() {
    let (_folder, connection) = engine();
    let off = KeepAwakeSettings {
        mode: Mode::Off,
        ..KeepAwakeSettings::DEFAULT
    };
    let start = KeepAwakeAction::Start { minutes: 60 };
    let extend = KeepAwakeAction::Extend { minutes: 60 };
    let far = KeepAwakeAction::Extend { minutes: 1440 };
    assert_eq!(
        act(&connection, extend, NOW_MS),
        rejected("keep_awake_no_timer")
    );
    assert_eq!(
        act(&connection, KeepAwakeAction::End, NOW_MS),
        rejected("keep_awake_not_holding")
    );
    assert_eq!(act(&connection, start.clone(), NOW_MS), Outcome::Applied);
    assert_eq!(
        act(&connection, far, NOW_MS + SECOND_MS),
        rejected("keep_awake_minutes_invalid")
    );
    set(&connection, off);
    assert_eq!(
        act(&connection, start, NOW_MS + 2 * SECOND_MS),
        rejected("keep_awake_off")
    );
    let words = [
        (
            "keep_awake_off",
            "Keep awake is Off. Pick Auto or Manual first.",
        ),
        (
            "keep_awake_no_timer",
            "The timer already ended. Start a new one.",
        ),
        ("keep_awake_not_holding", "Nothing is held right now."),
        (
            "keep_awake_minutes_invalid",
            "Enter whole minutes from 1 to 1440.",
        ),
        (
            "keep_awake_no_lid_helper",
            "Lid-closed hold is set up only on macOS.",
        ),
        (
            "keep_awake_no_lid_setting",
            "Put the lid setting back is only on Windows.",
        ),
        (
            "keep_awake_lid_held",
            "The lid setting goes back when this hold ends.",
        ),
        (
            crate::storage::versioned::STALE_CODE,
            "Keep awake changed in another window meanwhile. Try again.",
        ),
    ];
    for (code, sentence) in words {
        assert_eq!(refusal_words(code), Some(sentence), "{code}");
    }
    assert_eq!(refusal_words("owner_busy"), None);
    assert_eq!(refusal_words(""), None);
}

#[test]
fn the_lid_helper_commands_are_owner_changes_with_plain_names() {
    let names = [
        (KeepAwakeAction::TurnOnLidHold, "\"turn_on_lid_hold\""),
        (KeepAwakeAction::RemoveLidHelper, "\"remove_lid_helper\""),
    ];
    for (action, name) in &names {
        assert_eq!(serde_json::to_string(action).unwrap(), *name);
        assert_eq!(
            &serde_json::from_str::<KeepAwakeAction>(name).unwrap(),
            action
        );
        assert!(action.validate().is_ok());
    }
    let (_folder, connection) = engine();
    for (action, _) in names {
        let outcome = act(&connection, action.clone(), NOW_MS);
        if cfg!(target_os = "macos") {
            assert_eq!(outcome, Outcome::Applied, "{action:?}");
        } else {
            assert_eq!(outcome, rejected("keep_awake_no_lid_helper"), "{action:?}");
        }
    }
}

/// DEC-167 addendum 32, QUESTION 22:58 Q3: Put the lid setting back now is an owner change
/// on Windows only, and waits while a hold covers the closed lid, which puts it back as it
/// ends.
#[test]
fn put_the_lid_setting_back_now_is_an_owner_change_on_windows_only() {
    let action = KeepAwakeAction::PutLidBack;
    assert_eq!(serde_json::to_string(&action).unwrap(), "\"put_lid_back\"");
    let parsed = serde_json::from_str::<KeepAwakeAction>("\"put_lid_back\"").unwrap();
    assert_eq!(parsed, action);
    assert!(action.validate().is_ok());
    let (_folder, connection) = engine();
    let ac = sensors(PowerSource::Ac, Some(80));
    let resting = evaluate(&connection, NOW_MS, Some(&ac)).unwrap();
    assert_eq!(resting.state.want, Want::Nothing);
    let outcome = act(&connection, action, NOW_MS);
    if !cfg!(windows) {
        assert_eq!(outcome, rejected("keep_awake_no_lid_setting"));
        return;
    }
    assert_eq!(outcome, Outcome::Applied, "nothing holds the lid");
    agent(&connection, "working");
    let held = evaluate(&connection, NOW_MS + SECOND_MS, Some(&ac)).unwrap();
    assert_eq!(held.state.want, IDLE_AND_LID);
    let waiting = act(&connection, action, NOW_MS + 2 * SECOND_MS);
    assert_eq!(waiting, rejected("keep_awake_lid_held"));
}

/// Plan Task 14: `crazyagent keep-awake restore` runs the engine start's undo now and says
/// what is left in the owner's words, never by a change's kind.
#[test]
fn keep_awake_restore_runs_the_engine_start_undo_and_says_what_is_left() {
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(
        restore(folder.path()),
        Ok(()),
        "nothing recorded, nothing left"
    );
    let mut found = journal::Journal::open(folder.path()).unwrap();
    let unknown = journal::Entry {
        kind: "stand_in".into(),
        restore: "before".into(),
    };
    found.record(unknown.clone()).unwrap();
    drop(found);
    assert_eq!(restore(folder.path()), Err(RESTORE_LEFT));
    let kept = journal::Journal::open(folder.path()).unwrap();
    assert_eq!(
        kept.entries(),
        [unknown],
        "a change not put back stays recorded"
    );
    assert!(!RESTORE_LEFT.contains("stand_in"));
    let file = folder.path().join("keep-awake").join("journal.json");
    std::fs::write(&file, "not a journal").unwrap();
    assert_eq!(restore(folder.path()), Err(RESTORE_UNREADABLE));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "not a journal");
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
    assert_eq!(
        decide(&shown, &on_ac).want,
        Want::IdleAndLid { display: true }
    );
    let mut closed = on_ac.clone();
    closed.sensors.lid_closed = Some(true);
    assert_eq!(decide(&shown, &closed).want, IDLE_AND_LID);
}

#[test]
fn battery_below_the_floor_ends_the_hold_and_says_why() {
    let settings = KeepAwakeSettings::DEFAULT;
    assert_eq!(settings.battery_floor, BatteryFloor::Pct10);
    for pct in [9, 10] {
        let low = decide(
            &settings,
            &working(sensors(PowerSource::Battery, Some(pct))),
        );
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
            assert_eq!(
                decision.want == Want::Nothing,
                ends,
                "{battery_floor:?} at {pct}"
            );
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
    assert_eq!(
        no_battery.want, IDLE_AND_LID,
        "a Mac with no battery keeps its hold"
    );
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
    assert_eq!(
        decide(&early, &at(Heat::Serious)).ended,
        Some(EndReason::Heat)
    );
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
    all.extend(LowPower::ALL.map(|low_power| KeepAwakeSettings { low_power, ..base }));
    all.extend(Freeze::ALL.map(|freeze| KeepAwakeSettings { freeze, ..base }));
    all.extend(Mute::ALL.map(|mute| KeepAwakeSettings { mute, ..base }));
    all
}

#[test]
fn a_settings_row_saved_before_low_power_loads_with_its_default() {
    let (_folder, connection) = engine();
    let mut text = serde_json::to_value(KeepAwakeSettings::DEFAULT).unwrap();
    let removed = text.as_object_mut().unwrap().remove("low_power");
    assert_eq!(removed, Some(serde_json::json!("battery_below40")));
    let saved = connection
        .execute(
            "INSERT INTO configured_keep_awake(singleton,settings) VALUES(1,?1)",
            params![text.to_string()],
        )
        .unwrap();
    assert_eq!(saved, 1);
    assert_eq!(
        settings::load(&connection).ok(),
        Some(KeepAwakeSettings::DEFAULT),
        "a row from an earlier V3 dev build loads, with the owner's default (addendum 26)"
    );
    assert_eq!(LowPower::default(), KeepAwakeSettings::DEFAULT.low_power);
}

#[test]
fn low_power_is_asked_only_while_a_hold_runs_on_battery_as_the_setting_says() {
    let below = KeepAwakeSettings::DEFAULT;
    assert_eq!(
        below.low_power,
        LowPower::BatteryBelow40,
        "owner question 4 A"
    );
    let on_battery = KeepAwakeSettings {
        low_power: LowPower::OnBattery,
        ..below
    };
    let off = KeepAwakeSettings {
        low_power: LowPower::Off,
        ..below
    };
    let battery = |pct| sensors(PowerSource::Battery, pct);
    let ac = sensors(PowerSource::Ac, Some(30));
    let unknown = sensors(PowerSource::Unknown, Some(30));

    assert!(low_power_wanted(
        &on_battery,
        &battery(Some(90)),
        true,
        None
    ));
    assert!(
        !low_power_wanted(&on_battery, &battery(Some(90)), false, None),
        "no hold"
    );
    assert!(
        !low_power_wanted(&on_battery, &ac, true, None),
        "on AC power"
    );
    assert!(
        !low_power_wanted(&on_battery, &unknown, true, None),
        "a source not read"
    );
    assert!(!low_power_wanted(&off, &battery(Some(5)), true, None));
    assert!(low_power_wanted(&below, &battery(Some(39)), true, None));
    assert!(!low_power_wanted(&below, &battery(Some(40)), true, None));
    assert!(
        !low_power_wanted(&below, &battery(None), true, None),
        "a charge not read"
    );

    let asked = Some(LowPower::BatteryBelow40);
    assert!(
        low_power_wanted(&below, &battery(Some(41)), true, asked),
        "once asked in a hold it stays asked on battery (QUESTION 39e)"
    );
    assert!(low_power_wanted(&below, &battery(None), true, asked));
    assert!(
        !low_power_wanted(&below, &ac, true, asked),
        "AC power ends it"
    );
    assert!(
        !low_power_wanted(&below, &battery(Some(41)), false, asked),
        "so does the end"
    );
    assert!(
        !low_power_wanted(&below, &battery(Some(80)), true, Some(LowPower::OnBattery)),
        "an ask made under another option does not keep it"
    );
}

#[test]
fn low_power_starts_not_available_on_windows_and_unread_elsewhere() {
    use crate::paths::Platform;
    assert_eq!(
        LowPowerSupport::at_start(Platform::Windows),
        LowPowerSupport::NotAvailable {
            reason: state::LOW_POWER_WINDOWS.into()
        }
    );
    assert_eq!(
        LowPowerSupport::at_start(Platform::MacOs),
        LowPowerSupport::Unread
    );
    assert_eq!(
        LowPowerSupport::at_start(Platform::Linux),
        LowPowerSupport::Unread
    );
    assert_eq!(
        Sensors::unread().low_power,
        LowPowerSupport::at_start(Platform::CURRENT),
        "the readings this platform starts with"
    );
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
            low_power: LowPower::BatteryBelow40,
            freeze: Freeze::Off,
            mute: Mute::Off,
        }
    );
    let all = every_settings_value();
    assert_eq!(all.len(), 1 + 3 + 4 + 4 + 3 + 4 + 3 + 2 + 2 + 2 + 3 + 2 + 2);
    for settings in all {
        let mutation = OwnerMutation::KeepAwake {
            action: saved(&connection, settings),
        };
        let wire = serde_json::to_string(&mutation).unwrap();
        let sent: OwnerMutation = serde_json::from_str(&wire).unwrap();
        assert_eq!(sent, mutation);
        let Command::OwnerKeepAwake { action } = sent.command().unwrap() else {
            panic!("a Keep awake change is its own command");
        };
        assert_eq!(act(&connection, action, NOW_MS), Outcome::Applied);
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
    let unknown =
        r#"{"kind":"KeepAwake","action":{"set":{"settings":{"mode":"sometimes"},"version":0}}}"#;
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
    assert_eq!(
        work::count(&bare).unwrap(),
        0,
        "no tables yet means no work"
    );
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
fn the_keeper_looks_each_second_and_evaluates_on_a_change_or_every_five_seconds() {
    let (_folder, connection) = engine();
    let mut keeper = Keeper::default();
    assert!(keeper.tick(&connection, NOW_MS).unwrap().is_some());
    agent(&connection, "working");
    let early = keeper.tick(&connection, NOW_MS + 250).unwrap();
    assert!(early.is_none(), "the keeper looks once a second");
    let seen = keeper.tick(&connection, NOW_MS + SECOND_MS).unwrap();
    assert_eq!(
        seen.unwrap().state.want,
        IDLE,
        "a work change is seen in a second"
    );
    assert!(
        keeper
            .tick(&connection, NOW_MS + 2 * SECOND_MS)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        act(&connection, KeepAwakeAction::End, NOW_MS + 2500),
        Outcome::Applied
    );
    let ended = keeper.tick(&connection, NOW_MS + 3 * SECOND_MS).unwrap();
    assert_eq!(
        ended.unwrap().state.want,
        Want::Nothing,
        "an owner change is seen too"
    );
    assert!(
        keeper
            .tick(&connection, NOW_MS + 4 * SECOND_MS)
            .unwrap()
            .is_none()
    );
    let evaluated_ms = NOW_MS + 3 * SECOND_MS;
    assert!(
        keeper
            .tick(&connection, evaluated_ms + TICK_MS - 1)
            .unwrap()
            .is_none()
    );
    let due = keeper
        .tick(&connection, evaluated_ms + TICK_MS + SECOND_MS)
        .unwrap();
    assert!(
        due.is_some(),
        "with no change the keeper evaluates every five seconds"
    );
    let unchanged: String = connection
        .query_row("SELECT state FROM keep_awake_state", [], |row| row.get(0))
        .unwrap();
    assert!(
        keeper
            .tick(&connection, evaluated_ms + 3 * TICK_MS)
            .unwrap()
            .is_some()
    );
    let after: String = connection
        .query_row("SELECT state FROM keep_awake_state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        after, unchanged,
        "an evaluation that changes nothing writes nothing"
    );
}

/// A platform hold that remembers what it holds and counts how often it was let go.
#[derive(Debug, Default)]
struct Counted {
    held: std::sync::Arc<std::sync::Mutex<Option<Want>>>,
    releases: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl Hold for Counted {
    fn apply(&mut self, want: &Want) -> Result<Held, HoldError> {
        *self.held.lock().unwrap() = Some(*want).filter(|want| *want != Want::Nothing);
        Ok(Held {
            idle: *want != Want::Nothing,
            lid: false,
            display: false,
        })
    }

    fn release(&mut self) {
        *self.held.lock().unwrap() = None;
        self.releases
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Reviewer note N4, leader 14:22: a keeper step that fails lets the platform hold go, so
/// a broken engine fails toward sleep, and the next good step takes the hold again at once.
#[test]
fn a_failed_step_drops_the_platform_hold_and_the_next_good_step_takes_it_again() {
    let (_folder, connection) = engine();
    let hold = Counted::default();
    let (held, releases) = (hold.held.clone(), hold.releases.clone());
    let released = || releases.load(std::sync::atomic::Ordering::SeqCst);
    let mut keeper = Keeper::with_hold(Box::new(hold));
    agent(&connection, "working");
    let view = keeper.tick(&connection, NOW_MS).unwrap().unwrap();
    assert_eq!(view.state.want, IDLE);
    assert_eq!(*held.lock().unwrap(), Some(IDLE));
    assert_eq!(released(), 0);

    connection
        .execute_batch("ALTER TABLE keep_awake_state RENAME TO keep_awake_state_away")
        .unwrap();
    let failed = keeper.tick(&connection, NOW_MS + SECOND_MS);
    assert!(failed.is_err(), "the step cannot read its state");
    assert_eq!(*held.lock().unwrap(), None, "the hold is let go");
    assert_eq!(released(), 1);
    assert!(keeper.tick(&connection, NOW_MS + 2 * SECOND_MS).is_err());
    assert_eq!(released(), 2, "every failed step lets go");

    connection
        .execute_batch("ALTER TABLE keep_awake_state_away RENAME TO keep_awake_state")
        .unwrap();
    let back = keeper.tick(&connection, NOW_MS + 3 * SECOND_MS).unwrap();
    assert_eq!(
        back.expect("the first good step evaluates at once")
            .state
            .want,
        IDLE
    );
    assert_eq!(*held.lock().unwrap(), Some(IDLE), "the hold is taken again");
    assert_eq!(released(), 2);
}

#[test]
fn a_settings_save_from_an_older_read_is_refused_and_keeps_the_newer_save() {
    let (_folder, connection) = engine();
    let read = view(&connection).unwrap();
    assert_eq!(read.version, 0, "settings never saved are version 0");
    let first = KeepAwakeSettings {
        grace: Grace::Min30,
        ..read.settings
    };
    let second = KeepAwakeSettings {
        cap: Cap::H4,
        ..read.settings
    };
    let first_save = KeepAwakeAction::Set {
        settings: first,
        version: read.version,
    };
    assert_eq!(act(&connection, first_save, NOW_MS), Outcome::Applied);
    let after_first = view(&connection).unwrap();
    assert_eq!(after_first.settings, first);
    assert_eq!(
        after_first.version,
        read.version + 1,
        "one save, one version"
    );

    let late_save = KeepAwakeAction::Set {
        settings: second,
        version: read.version,
    };
    assert_eq!(
        act(&connection, late_save, NOW_MS + SECOND_MS),
        rejected(crate::storage::versioned::STALE_CODE),
        "a save built on the read before the first save is refused"
    );
    let kept = view(&connection).unwrap();
    assert_eq!(kept.settings, first, "the newer save is kept");
    assert_eq!(
        kept.version, after_first.version,
        "a refused save bumps nothing"
    );

    let laid_again = KeepAwakeAction::Set {
        settings: KeepAwakeSettings {
            cap: Cap::H4,
            ..kept.settings
        },
        version: kept.version,
    };
    assert_eq!(
        act(&connection, laid_again, NOW_MS + 2 * SECOND_MS),
        Outcome::Applied
    );
    let both = view(&connection).unwrap();
    assert_eq!(both.settings.grace, Grace::Min30, "the first save survives");
    assert_eq!(both.settings.cap, Cap::H4, "the pick laid again lands");
    assert_eq!(both.version, kept.version + 1);
}

#[test]
fn a_timer_change_leaves_the_settings_version_and_every_read_shows_it() {
    let (_folder, connection) = engine();
    set(
        &connection,
        KeepAwakeSettings {
            mode: Mode::Manual,
            ..KeepAwakeSettings::DEFAULT
        },
    );
    assert_eq!(view(&connection).unwrap().version, 1);
    let start = KeepAwakeAction::Start { minutes: 60 };
    assert_eq!(act(&connection, start, NOW_MS), Outcome::Applied);
    assert_eq!(
        act(&connection, KeepAwakeAction::End, NOW_MS),
        Outcome::Applied
    );
    assert_eq!(view(&connection).unwrap().version, 1, "a timer is no save");
    let evaluated = evaluate(&connection, NOW_MS + SECOND_MS, None).unwrap();
    assert_eq!(evaluated.version, 1, "an evaluation shows the version too");
    let OwnerData::KeepAwake { view: owned } = read(&connection).unwrap() else {
        panic!("the owner read of Keep awake is its view");
    };
    assert_eq!(owned.version, 1, "the owner read carries the version");
}

#[cfg(target_os = "macos")]
mod lid_macos;
mod lid_windows;
mod notices;
mod power_saver;
mod sensing;

#[cfg(target_os = "linux")]
mod on_linux {
    use crate::keep_awake::hold_linux::close_on_exec;
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};

    /// rv-sec2 KA F1: the descriptor logind hands out arrives without close-on-exec; the
    /// hold marks every descriptor of the reply inside the spawn gate, so no child the
    /// engine starts later inherits the lock.
    #[test]
    fn a_descriptor_the_bus_hands_over_is_marked_close_on_exec() {
        let (received, _peer) = crate::spawn_gate::unix_pair_std().unwrap();
        fcntl_setfd(&received, FdFlags::empty()).unwrap();
        assert!(!fcntl_getfd(&received).unwrap().contains(FdFlags::CLOEXEC));
        close_on_exec(&received).unwrap();
        assert!(fcntl_getfd(&received).unwrap().contains(FdFlags::CLOEXEC));
        close_on_exec(&received).unwrap();
        assert!(fcntl_getfd(&received).unwrap().contains(FdFlags::CLOEXEC));
    }
}

#[cfg(target_os = "macos")]
mod with_a_real_hold {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(5);
    const STEP: Duration = Duration::from_millis(50);

    /// A real process that stands in for the engine the hold is taken for.
    fn stand_in() -> Child {
        let child = crate::spawn_gate::spawn(
            Command::new("/bin/sleep")
                .arg("120")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(child.id() > 1);
        child
    }

    /// Counts the running caffeinate children that hold for one engine pid. Input: the pid.
    /// Output: how many `pgrep` finds by their exact command line, read from its collected
    /// output; an exited child that nobody reaped yet has no command line and is not counted.
    fn holders(engine: u32) -> usize {
        let line = format!("^/usr/bin/caffeinate (-d )?-i -w {engine}$");
        let output =
            crate::spawn_gate::collected(Command::new("/usr/bin/pgrep").args(["-f", &line]))
                .unwrap();
        assert!(
            output.status.code().is_some_and(|code| code <= 1),
            "{output:?}"
        );
        String::from_utf8(output.stdout).unwrap().lines().count()
    }

    fn until(mut condition: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + WAIT;
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(STEP);
        }
    }

    #[test]
    fn the_keeper_takes_the_hold_while_work_runs_and_drops_it_when_the_owner_ends() {
        let (_folder, connection) = engine();
        let mut process = stand_in();
        let pid = process.id();
        let mut keeper = Keeper::with_hold(platform_hold(pid));
        let worker = agent(&connection, "working");

        let view = keeper.tick(&connection, NOW_MS).unwrap().unwrap();
        assert_eq!(view.state.want, IDLE);
        assert!(until(|| holders(pid) == 1));

        assert_eq!(
            act(&connection, KeepAwakeAction::End, NOW_MS + SECOND_MS),
            Outcome::Applied
        );
        let ended = keeper
            .tick(&connection, NOW_MS + 2 * SECOND_MS)
            .unwrap()
            .unwrap();
        assert_eq!(ended.state.want, Want::Nothing);
        assert_eq!(ended.state.ended, Some(EndReason::Manual));
        assert!(until(|| holders(pid) == 0));

        mark(&connection, worker, "idle");
        let idle = keeper
            .tick(&connection, NOW_MS + 3 * SECOND_MS)
            .unwrap()
            .unwrap();
        assert_eq!(idle.state.want, Want::Nothing);
        mark(&connection, worker, "working");
        let again = keeper
            .tick(&connection, NOW_MS + 4 * SECOND_MS)
            .unwrap()
            .unwrap();
        assert_eq!(again.state.want, IDLE);
        assert!(until(|| holders(pid) == 1));
        keeper.release();
        assert!(until(|| holders(pid) == 0));

        let _ = process.kill();
        process.wait().unwrap();
    }

    #[test]
    fn a_hold_lost_twice_in_a_row_ends_with_hold_lost_and_stands_down() {
        let (_folder, connection) = engine();
        let mut process = stand_in();
        let pid = process.id();
        process.kill().unwrap();
        process.wait().unwrap();
        let mut keeper = Keeper::with_hold(platform_hold(pid));
        let worker = agent(&connection, "working");

        let first = keeper.tick(&connection, NOW_MS).unwrap().unwrap();
        assert_eq!(first.state.want, IDLE);
        assert!(until(|| holders(pid) == 0));

        let retaken = keeper.tick(&connection, NOW_MS + TICK_MS).unwrap().unwrap();
        assert_eq!(retaken.state.want, IDLE, "lost once: taken again");
        assert_eq!(retaken.state.ended, None);
        assert!(until(|| holders(pid) == 0));

        let failed = keeper
            .tick(&connection, NOW_MS + 2 * TICK_MS)
            .unwrap()
            .unwrap();
        assert_eq!(failed.state.want, Want::Nothing);
        assert_eq!(failed.state.ended, Some(EndReason::HoldLost));
        assert_eq!(super::view(&connection).unwrap(), failed);

        let stood = keeper
            .tick(&connection, NOW_MS + 3 * TICK_MS)
            .unwrap()
            .unwrap();
        assert_eq!(
            stood.state.want,
            Want::Nothing,
            "stands down while work runs"
        );
        assert_eq!(stood.state.ended, Some(EndReason::HoldLost));

        mark(&connection, worker, "idle");
        let idle = keeper
            .tick(&connection, NOW_MS + 4 * TICK_MS)
            .unwrap()
            .unwrap();
        assert_eq!(idle.state.want, Want::Nothing);
        mark(&connection, worker, "working");
        let tried = keeper
            .tick(&connection, NOW_MS + 5 * TICK_MS)
            .unwrap()
            .unwrap();
        assert_eq!(tried.state.want, IDLE, "new work tries the hold again");
        assert_eq!(tried.state.ended, None);
    }

    #[test]
    fn a_keeper_without_a_platform_hold_decides_and_takes_nothing() {
        let (_folder, connection) = engine();
        let mut keeper = Keeper::default();
        agent(&connection, "working");
        let view = keeper.tick(&connection, NOW_MS).unwrap().unwrap();
        assert_eq!(view.state.want, IDLE);
        assert_eq!(view.state.ended, None);
        keeper.release();
    }
}

/// rv-sec3 KA T14 r1 F2, QUESTION 01:16 Q3: the engine start takes the journal lock and
/// keeps it, so a restore cannot run beside the engine; once it is let go, restore runs.
#[test]
fn the_engine_start_takes_the_journal_lock_and_restore_waits_for_it() {
    let root = tempfile::tempdir().unwrap();
    let lock = journal_lock(root.path());
    assert!(lock.is_some(), "a free lock is taken at once");
    let beside = journal::JournalLock::try_take(root.path()).unwrap();
    assert!(beside.is_none(), "one holder at a time");
    assert_eq!(restore(root.path()), Err(RESTORE_ENGINE_RUNNING));
    drop(lock);
    assert_eq!(restore(root.path()), Ok(()));
}

/// fix5 QUESTION-01:45 A, leader 01:45 (KA-ED decides the Mac's reading): Freeze and Mute
/// cover a closed lid only while no external display is in use. At a desk nothing is
/// covered, unplugging the display covers, plugging it in again uncovers, an unread display
/// covers as before, and an open lid or a hold that does not want the lid never covers.
#[test]
fn freeze_and_mute_cover_a_closed_lid_only_without_an_external_display() {
    let cover = freeze::Cover {
        freeze: true,
        mute: true,
        apps: vec![FreezeApp {
            name: "Notes".into(),
            id: "com.example.notes".into(),
        }],
    };
    let read = |lid_closed: Option<bool>, external_display: Option<bool>| Sensors {
        lid_closed,
        external_display,
        ..sensors(PowerSource::Ac, Some(80))
    };
    let desk = read(Some(true), Some(true));
    let away = read(Some(true), Some(false));
    let unread = read(Some(true), None);
    let open = read(Some(false), Some(false));

    assert_eq!(
        lid_cover(true, Some(&desk), cover.clone()),
        None,
        "at a desk"
    );
    assert_eq!(
        lid_cover(true, Some(&away), cover.clone()),
        Some(cover.clone()),
        "the display unplugged"
    );
    assert_eq!(
        lid_cover(true, Some(&desk), cover.clone()),
        None,
        "plugged in again"
    );
    assert_eq!(
        lid_cover(true, Some(&unread), cover.clone()),
        Some(cover.clone()),
        "an unread display covers as before"
    );
    assert_eq!(lid_cover(true, Some(&open), cover.clone()), None);
    assert_eq!(lid_cover(false, Some(&away), cover.clone()), None);
    assert_eq!(lid_cover(true, None, cover.clone()), None);
    let nothing = freeze::Cover {
        freeze: false,
        mute: false,
        apps: Vec::new(),
    };
    assert_eq!(lid_cover(true, Some(&away), nothing), None);
}
