use super::*;
use crate::keep_awake::power::{
    LID_REFUSED, LidAsk, LidCommand, LidReader, PowerReading, READ_MS, Reader, Sensing,
};
use crate::notifications::keep_awake::LID_SUBJECT;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);
const STEP: Duration = Duration::from_millis(3);
const STEPS_MAX: u32 = 120;

/// A reader the row steers: it answers with the reading set last and counts its calls.
#[derive(Clone)]
struct Fake {
    reading: Arc<Mutex<PowerReading>>,
    reads: Arc<AtomicU32>,
}

impl Fake {
    fn new(reading: PowerReading) -> Self {
        Self {
            reading: Arc::new(Mutex::new(reading)),
            reads: Arc::new(AtomicU32::new(0)),
        }
    }

    fn set(&self, reading: PowerReading) {
        *self.reading.lock().unwrap() = reading;
    }

    fn reads(&self) -> u32 {
        self.reads.load(Ordering::SeqCst)
    }

    fn reader(&self) -> Reader {
        let fake = self.clone();
        Box::new(move || {
            let reading = fake.reading.lock().unwrap().clone();
            fake.reads.fetch_add(1, Ordering::SeqCst);
            reading
        })
    }
}

/// A reading on AC power of a platform that can hold the closed lid.
fn on_ac() -> PowerReading {
    PowerReading {
        source: PowerSource::Ac,
        battery_pct: Some(80),
        heat: Heat::Nominal,
        lid_closed: Some(false),
        lid_support: LidSupport::Held,
    }
}

fn on_battery(battery_pct: Option<u8>) -> PowerReading {
    PowerReading {
        source: PowerSource::Battery,
        battery_pct,
        ..on_ac()
    }
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

/// Steps the keeper one second of engine time at a time, with a short real pause so the
/// sensor thread can answer. Inputs: the keeper, the engine database, the engine time,
/// moved on in place, and how many steps. Output: the last view a step produced.
pub(super) fn steps(
    keeper: &mut Keeper,
    connection: &Connection,
    now_ms: &mut i64,
    count: u32,
) -> Option<KeepAwakeView> {
    let mut last = None;
    for _ in 0..count {
        *now_ms += SECOND_MS;
        std::thread::sleep(STEP);
        if let Some(view) = keeper.tick(connection, *now_ms).unwrap() {
            last = Some(view);
        }
    }
    last
}

/// Steps the keeper until a view meets the check. Inputs: as `steps`, and the check.
/// Output: the first view that meets it; the row fails after `STEPS_MAX` steps.
pub(super) fn step_until(
    keeper: &mut Keeper,
    connection: &Connection,
    now_ms: &mut i64,
    check: impl Fn(&KeepAwakeView) -> bool,
) -> KeepAwakeView {
    for _ in 0..STEPS_MAX {
        if let Some(view) = steps(keeper, connection, now_ms, 1)
            && check(&view)
        {
            return view;
        }
    }
    panic!("no view met the check in {STEPS_MAX} steps");
}

#[test]
fn the_first_step_asks_for_a_reading_and_the_view_shows_what_came_back() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let mut keeper = Keeper::default().with_reader(fake.reader());
    let mut now_ms = NOW_MS;

    let first = keeper.tick(&connection, now_ms).unwrap().unwrap();
    assert_ne!(
        first.lid_support,
        LidSupport::Held,
        "before a reading the lid support is the platform's starting reason"
    );
    let read = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.lid_support == LidSupport::Held
    });
    assert_eq!(
        read.state.want,
        Want::Nothing,
        "a reading alone holds nothing"
    );
    assert!(now_ms - NOW_MS < READ_MS, "the first reading does not wait");
    assert_eq!(fake.reads(), 1);
}

#[test]
fn the_keeper_reads_every_thirty_seconds_with_work_and_without() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let mut keeper = Keeper::default().with_reader(fake.reader());
    let mut now_ms = NOW_MS;
    assert_eq!(READ_MS, 30 * SECOND_MS);

    keeper.tick(&connection, now_ms).unwrap();
    steps(&mut keeper, &connection, &mut now_ms, 95);
    assert!(until(|| fake.reads() == 4), "read at 0, 30, 60 and 90 s");

    agent(&connection, "working");
    steps(&mut keeper, &connection, &mut now_ms, 60);
    assert!(until(|| fake.reads() == 6), "read at 120 and 150 s");
    std::thread::sleep(10 * STEP);
    assert_eq!(fake.reads(), 6, "work does not add readings");
}

#[test]
fn a_charge_at_the_floor_ends_the_running_hold_and_says_the_percent() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let mut keeper = Keeper::default().with_reader(fake.reader());
    let mut now_ms = NOW_MS;
    agent(&connection, "working");

    let held = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.want == IDLE_AND_LID
    });
    assert_eq!(held.state.ended, None);
    assert_eq!(held.settings.battery_floor, BatteryFloor::Pct10);

    fake.set(on_battery(Some(10)));
    let ended = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.ended.is_some()
    });
    assert_eq!(ended.state.ended, Some(EndReason::Battery { pct: 10 }));
    assert_eq!(ended.state.want, Want::Nothing);
    assert_eq!(ended.lid_support, LidSupport::IdleOnlyOnBattery);
}

#[test]
fn critical_heat_in_a_reading_ends_the_running_hold() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let mut keeper = Keeper::default().with_reader(fake.reader());
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.want == IDLE_AND_LID
    });

    fake.set(PowerReading {
        heat: Heat::Serious,
        ..on_ac()
    });
    steps(&mut keeper, &connection, &mut now_ms, 35);
    let serious = keeper.tick(&connection, now_ms + TICK_MS).unwrap().unwrap();
    assert_eq!(
        serious.state.ended, None,
        "serious heat does not end by default"
    );

    fake.set(PowerReading {
        heat: Heat::Critical,
        ..on_ac()
    });
    now_ms += TICK_MS;
    let ended = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.ended.is_some()
    });
    assert_eq!(ended.state.ended, Some(EndReason::Heat));
}

#[test]
fn one_unread_battery_keeps_the_hold_and_the_next_one_ends_it() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_battery(Some(50)));
    let mut keeper = Keeper::default().with_reader(fake.reader());
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.want == IDLE
    });
    assert!(until(|| fake.reads() == 1));

    fake.set(on_battery(None));
    steps(&mut keeper, &connection, &mut now_ms, 35);
    assert!(until(|| fake.reads() == 2));
    let once = steps(&mut keeper, &connection, &mut now_ms, 6).unwrap();
    assert_eq!(once.state.ended, None, "one unread reading is not enough");
    assert_eq!(once.state.want, IDLE);

    let ended = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.ended.is_some()
    });
    assert_eq!(ended.state.ended, Some(EndReason::BatteryUnread));
    assert_eq!(
        fake.reads(),
        3,
        "the second unread reading in a row ends the hold"
    );

    fake.set(on_battery(Some(50)));
    steps(&mut keeper, &connection, &mut now_ms, 35);
    assert!(until(|| fake.reads() >= 4));
    fake.set(on_battery(None));
    assert_eq!(
        act(&connection, KeepAwakeAction::Start { minutes: 60 }, now_ms),
        Outcome::Applied
    );
    steps(&mut keeper, &connection, &mut now_ms, 35);
    assert!(until(|| fake.reads() >= 5));
    let again = steps(&mut keeper, &connection, &mut now_ms, 6).unwrap();
    assert_eq!(
        again.state.ended, None,
        "a read charge between two unread readings starts the count again"
    );
}

/// A platform hold that takes the idle hold and never the lid, and records what it was
/// asked for.
#[derive(Debug, Default)]
struct NoLid {
    asked: Arc<Mutex<Vec<Want>>>,
}

impl Hold for NoLid {
    fn apply(&mut self, want: &Want) -> Result<Held, HoldError> {
        self.asked.lock().unwrap().push(*want);
        Ok(match want {
            Want::Nothing => Held::NONE,
            Want::Idle { display } | Want::IdleAndLid { display } => Held {
                idle: true,
                lid: false,
                display: *display,
            },
        })
    }

    fn release(&mut self) {}
}

#[test]
fn a_refused_lid_lock_keeps_the_idle_hold_and_the_status_says_why() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let hold = NoLid::default();
    let asked = hold.asked.clone();
    let mut keeper = Keeper::with_hold(Box::new(hold)).with_reader(fake.reader());
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    let refused = LidSupport::NotAvailable {
        reason: LID_REFUSED.into(),
    };
    assert_eq!(LID_REFUSED, "The system did not allow the lid-closed hold");

    let view = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.lid_support == refused
    });
    assert_eq!(view.state.want, IDLE, "the idle hold goes on");
    assert_eq!(
        view.state.ended, None,
        "a refused lid lock is not a lost hold"
    );
    let never_claimed = asked.lock().unwrap().clone();
    let lid_asks = never_claimed
        .iter()
        .filter(|want| **want == IDLE_AND_LID)
        .count();
    assert_eq!(
        lid_asks, 1,
        "the lid lock is asked for once: {never_claimed:?}"
    );
    assert_eq!(never_claimed.last(), Some(&IDLE));

    let later = steps(&mut keeper, &connection, &mut now_ms, 65).unwrap();
    assert_eq!(later.lid_support, refused, "later readings keep the reason");
    assert_eq!(later.state.want, IDLE);
    let lid_asks = asked
        .lock()
        .unwrap()
        .iter()
        .filter(|want| **want == IDLE_AND_LID)
        .count();
    assert_eq!(lid_asks, 1, "the lid lock is not tried again");
}

#[test]
fn a_reader_that_dies_leaves_unknown_readings_and_the_hold_goes_on() {
    let (_folder, connection) = engine();
    let reads = Arc::new(AtomicU32::new(0));
    let counted = reads.clone();
    let reader: Reader = Box::new(move || {
        let read = counted.fetch_add(1, Ordering::SeqCst);
        assert_eq!(read, 0, "the stand-in reader breaks on its second call");
        on_ac()
    });
    let mut keeper = Keeper::default().with_reader(reader);
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.want == IDLE_AND_LID
    });

    let unknown = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.lid_support != LidSupport::Held
    });
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert_eq!(unknown.lid_support, PowerReading::unknown().lid_support);
    assert_eq!(unknown.state.want, IDLE, "the hold goes on without the lid");
    assert_eq!(unknown.state.ended, None);

    let later = steps(&mut keeper, &connection, &mut now_ms, 65).unwrap();
    assert_eq!(later.state.want, IDLE);
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "a dead reader is not asked again"
    );
}

#[test]
fn the_sensor_thread_ends_with_the_keeper() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let mut keeper = Keeper::default().with_reader(fake.reader());
    keeper.tick(&connection, NOW_MS).unwrap();
    assert!(until(|| fake.reads() == 1));
    assert_eq!(
        Arc::strong_count(&fake.reads),
        2,
        "the thread owns the reader"
    );
    drop(keeper);
    assert!(
        until(|| Arc::strong_count(&fake.reads) == 1),
        "the sensor thread dropped its reader"
    );
}

/// Steps the sensor line at one engine time until a reading comes back. Inputs: the
/// line, the time and the lid ask. Output: the reading; the row fails after `WAIT`.
fn answered(sensing: &mut Sensing, now_ms: i64, ask: LidAsk) -> PowerReading {
    let mut reading = None;
    assert!(until(|| {
        reading = sensing.step(now_ms, ask);
        reading.is_some()
    }));
    reading.expect("a reading came back")
}

/// What a recording lid line heard: each ask, the command that came with it, and the time.
type Asks = Arc<Mutex<Vec<(LidAsk, Option<LidCommand>, i64)>>>;

/// A lid line that records every ask and answers with the support the row set last.
/// Input: the first support. Output: the line, what it heard, and its support to set.
fn heard_lid(support: LidSupport) -> (LidReader, Asks, Arc<Mutex<LidSupport>>) {
    let asks: Asks = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(Mutex::new(support));
    let (heard, answers) = (asks.clone(), answer.clone());
    let lid: LidReader = Box::new(move |ask, command, now_ms| {
        assert!(now_ms > 0);
        heard.lock().unwrap().push((ask, command, now_ms));
        answers.lock().unwrap().clone()
    });
    (lid, asks, answer)
}

#[test]
fn a_changed_lid_ask_reaches_the_lid_line_at_once_and_an_unchanged_one_waits() {
    let fake = Fake::new(on_ac());
    let (lid, asks, _) = heard_lid(LidSupport::NeedsApproval);
    let mut sensing = Sensing::start(fake.reader(), Some(lid)).unwrap();
    let resting = LidAsk {
        rule_on: true,
        wanted: false,
        sleep_display: false,
    };
    let wanted = LidAsk {
        rule_on: true,
        wanted: true,
        sleep_display: true,
    };
    let count = || asks.lock().unwrap().len();

    assert_eq!(sensing.step(NOW_MS, resting), None);
    let first = answered(&mut sensing, NOW_MS + SECOND_MS, resting);
    assert_eq!(
        first.lid_support,
        LidSupport::NeedsApproval,
        "the lid line's support stands in for the reader's"
    );
    assert_eq!(*asks.lock().unwrap(), [(resting, None, NOW_MS)]);

    let changed_ms = NOW_MS + 2 * SECOND_MS;
    assert_eq!(sensing.step(changed_ms, wanted), None);
    assert!(until(|| count() == 2), "a changed ask goes out at once");
    assert_eq!(asks.lock().unwrap()[1], (wanted, None, changed_ms));
    answered(&mut sensing, changed_ms + SECOND_MS, wanted);

    for second in 4..30 {
        assert_eq!(sensing.step(NOW_MS + second * SECOND_MS, wanted), None);
    }
    let due_ms = changed_ms + READ_MS;
    assert_eq!(sensing.step(due_ms, wanted), None);
    assert!(until(|| count() == 3));
    assert_eq!(
        asks.lock().unwrap()[2],
        (wanted, None, due_ms),
        "an unchanged ask waits for the next reading"
    );
    assert!(until(|| fake.reads() == 3));
}

#[test]
fn the_keeper_hands_the_lid_line_what_the_lid_wants_and_leaves_the_lid_to_it() {
    let (_folder, connection) = engine();
    let fake = Fake::new(PowerReading {
        lid_support: LidSupport::HelperMissing,
        ..on_ac()
    });
    let (lid, asks, _) = heard_lid(LidSupport::Held);
    let hold = NoLid::default();
    let asked = hold.asked.clone();
    let mut keeper = Keeper::with_hold(Box::new(hold)).with_sensors(fake.reader(), Some(lid));
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    let last = || asks.lock().unwrap().last().map(|(ask, _, _)| *ask);
    let off = LidAsk {
        rule_on: false,
        wanted: false,
        sleep_display: false,
    };

    keeper.tick(&connection, now_ms).unwrap();
    assert!(until(|| last().is_some()), "the first reading reaches the lid line");
    assert_eq!(last(), Some(off), "nothing is asked of the helper before an evaluation");

    let wanted = LidAsk {
        rule_on: true,
        wanted: true,
        sleep_display: true,
    };
    let view = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.want == IDLE_AND_LID && last() == Some(wanted)
    });
    assert_eq!(view.lid_support, LidSupport::Held);
    let later = steps(&mut keeper, &connection, &mut now_ms, 65).unwrap();
    assert_eq!(
        later.state.want, IDLE_AND_LID,
        "an idle hold that never takes the lid is no refusal while the lid line holds it"
    );
    assert_eq!(later.lid_support, LidSupport::Held);
    assert!(asked.lock().unwrap().contains(&IDLE_AND_LID));

    let settings = KeepAwakeSettings {
        lid: LidRule::Off,
        ..KeepAwakeSettings::default()
    };
    let outcome = act(&connection, KeepAwakeAction::Set { settings }, now_ms);
    assert_eq!(outcome, Outcome::Applied);
    let ruled_off = step_until(&mut keeper, &connection, &mut now_ms, |_| last() == Some(off));
    assert_eq!(ruled_off.lid_support, LidSupport::Off);
    assert_eq!(ruled_off.state.want, IDLE);
}

#[test]
fn the_keeper_asks_the_lid_line_to_sleep_the_display_only_as_the_setting_says() {
    let (_folder, connection) = engine();
    let fake = Fake::new(PowerReading {
        lid_support: LidSupport::HelperMissing,
        ..on_ac()
    });
    let (lid, asks, _) = heard_lid(LidSupport::Held);
    let hold = Box::new(NoLid::default());
    let mut keeper = Keeper::with_hold(hold).with_sensors(fake.reader(), Some(lid));
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    let last = || asks.lock().unwrap().last().map(|(ask, _, _)| *ask);
    let sleeping = LidAsk {
        rule_on: true,
        wanted: true,
        sleep_display: true,
    };
    step_until(&mut keeper, &connection, &mut now_ms, |_| last() == Some(sleeping));

    let settings = KeepAwakeSettings {
        lid_display: LidDisplay::LeaveOn,
        ..KeepAwakeSettings::default()
    };
    let outcome = act(&connection, KeepAwakeAction::Set { settings }, now_ms);
    assert_eq!(outcome, Outcome::Applied);
    let left_on = LidAsk {
        sleep_display: false,
        ..sleeping
    };
    step_until(&mut keeper, &connection, &mut now_ms, |_| last() == Some(left_on));
    let heard = asks.lock().unwrap().clone();
    assert!(
        heard.iter().all(|(ask, _, _)| ask.wanted || !ask.sleep_display),
        "only a wanted ask carries the display ask (QUESTION 38)"
    );
}

#[test]
fn a_keeper_without_a_reader_keeps_the_readings_it_started_with() {
    let (_folder, connection) = engine();
    let mut keeper = Keeper::default();
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    let first = keeper.tick(&connection, now_ms).unwrap().unwrap();
    let later = steps(&mut keeper, &connection, &mut now_ms, 65).unwrap();
    assert_eq!(later.lid_support, first.lid_support);
    assert_ne!(later.lid_support, LidSupport::Held);
    assert_eq!(later.state.want, IDLE);
}

/// Steps the keeper one engine second at a time until the check holds. Inputs: as
/// `steps`, and the check. The row fails after `STEPS_MAX` steps.
fn until_stepped(
    keeper: &mut Keeper,
    connection: &Connection,
    now_ms: &mut i64,
    check: impl Fn() -> bool,
) {
    for _ in 0..STEPS_MAX {
        steps(keeper, connection, now_ms, 1);
        if check() {
            return;
        }
    }
    panic!("the check never held in {STEPS_MAX} steps");
}

/// The commands a recording lid line heard, in order.
fn commands(asks: &Asks) -> Vec<LidCommand> {
    let heard = asks.lock().unwrap();
    heard.iter().filter_map(|(_, command, _)| *command).collect()
}

#[test]
fn a_lid_command_reaches_the_lid_line_once_and_at_once() {
    let fake = Fake::new(on_ac());
    let (lid, asks, _) = heard_lid(LidSupport::NotTurnedOn);
    let mut sensing = Sensing::start(fake.reader(), Some(lid)).unwrap();
    let resting = LidAsk {
        rule_on: true,
        wanted: false,
        sleep_display: false,
    };
    assert_eq!(sensing.step(NOW_MS, resting), None);
    sensing.order(LidCommand::Remove);
    sensing.order(LidCommand::TurnOn);

    let answered_ms = NOW_MS + SECOND_MS;
    answered(&mut sensing, answered_ms, resting);
    assert!(
        until(|| asks.lock().unwrap().len() == 2),
        "a command waits for the reading under way, then goes at once"
    );
    assert_eq!(
        asks.lock().unwrap()[1],
        (resting, Some(LidCommand::TurnOn), answered_ms),
        "a later command replaces one not sent yet"
    );
    answered(&mut sensing, answered_ms + SECOND_MS, resting);
    for second in 3..31 {
        assert_eq!(sensing.step(NOW_MS + second * SECOND_MS, resting), None);
    }
    assert_eq!(sensing.step(NOW_MS + 31 * SECOND_MS, resting), None);
    assert!(until(|| asks.lock().unwrap().len() == 3));
    assert_eq!(commands(&asks), [LidCommand::TurnOn], "a command goes once");
}

#[cfg(target_os = "macos")]
#[test]
fn an_owner_lid_command_reaches_the_lid_line_once() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let (lid, asks, _) = heard_lid(LidSupport::NotTurnedOn);
    let mut keeper = Keeper::default().with_sensors(fake.reader(), Some(lid));
    let mut now_ms = NOW_MS;
    keeper.tick(&connection, now_ms).unwrap();

    let turn_on = act(&connection, KeepAwakeAction::TurnOnLidHold, now_ms);
    assert_eq!(turn_on, Outcome::Applied);
    until_stepped(&mut keeper, &connection, &mut now_ms, || {
        !commands(&asks).is_empty()
    });
    steps(&mut keeper, &connection, &mut now_ms, 65);
    assert_eq!(commands(&asks), [LidCommand::TurnOn], "each command goes once");

    let remove = act(&connection, KeepAwakeAction::RemoveLidHelper, now_ms);
    assert_eq!(remove, Outcome::Applied);
    until_stepped(&mut keeper, &connection, &mut now_ms, || {
        commands(&asks).len() == 2
    });
    assert_eq!(commands(&asks), [LidCommand::TurnOn, LidCommand::Remove]);
}

#[test]
fn the_keeper_notes_a_lid_helper_journal_it_cannot_read_and_clears_it() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let (lid, _, support) = heard_lid(LidSupport::SleepNotRestored);
    let mut keeper = Keeper::default().with_sensors(fake.reader(), Some(lid));
    let mut now_ms = NOW_MS;
    let rows = || -> i64 {
        connection
            .query_row(
                "SELECT count(*) FROM keep_awake_restore WHERE subject=?1",
                [LID_SUBJECT],
                |row| row.get(0),
            )
            .unwrap()
    };

    until_stepped(&mut keeper, &connection, &mut now_ms, || rows() == 1);
    crate::notifications::keep_awake::left_over(&connection, &Leftover::Kinds(Vec::new()))
        .unwrap();
    assert_eq!(rows(), 1, "the engine start's undo leaves the lid helper row");
    *support.lock().unwrap() = LidSupport::Held;
    until_stepped(&mut keeper, &connection, &mut now_ms, || rows() == 0);
}

#[test]
fn the_view_shows_the_platform_lid_support_whatever_the_lid_rule() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let (lid, _, _) = heard_lid(LidSupport::NeedsApproval);
    let mut keeper = Keeper::default().with_sensors(fake.reader(), Some(lid));
    let mut now_ms = NOW_MS;
    let settings = KeepAwakeSettings {
        lid: LidRule::Off,
        ..KeepAwakeSettings::default()
    };
    let outcome = act(&connection, KeepAwakeAction::Set { settings }, now_ms);
    assert_eq!(outcome, Outcome::Applied);

    let view = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.lid_platform == LidSupport::NeedsApproval
    });
    assert_eq!(view.lid_support, LidSupport::Off, "the rule Off still decides the lid");
    let read = crate::keep_awake::view(&connection).unwrap();
    assert_eq!(read.lid_platform, LidSupport::NeedsApproval);
}

#[test]
fn stop_hands_the_lid_line_a_last_not_wanted_ask_before_it_returns() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let (lid, asks, _) = heard_lid(LidSupport::Held);
    let hold = NoLid::default();
    let mut keeper = Keeper::with_hold(Box::new(hold)).with_sensors(fake.reader(), Some(lid));
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    let last = || asks.lock().unwrap().last().map(|(ask, _, _)| *ask);
    let wanted = LidAsk {
        rule_on: true,
        wanted: true,
        sleep_display: true,
    };
    until_stepped(&mut keeper, &connection, &mut now_ms, || last() == Some(wanted));

    keeper.stop();
    let let_go = LidAsk {
        rule_on: true,
        wanted: false,
        sleep_display: false,
    };
    assert_eq!(last(), Some(let_go), "the lid line heard the last ask before stop returned");
}

#[test]
fn stop_waits_for_the_lid_line_at_most_stop_wait() {
    let (_folder, connection) = engine();
    let fake = Fake::new(on_ac());
    let slow = Arc::new(AtomicBool::new(false));
    let late = slow.clone();
    let heard = Arc::new(AtomicU32::new(0));
    let count = heard.clone();
    let lid: LidReader = Box::new(move |ask, _, _| {
        count.fetch_add(1, Ordering::SeqCst);
        if late.load(Ordering::SeqCst) {
            std::thread::sleep(3 * STOP_WAIT);
        }
        assert!(ask.rule_on || !ask.wanted);
        LidSupport::Held
    });
    let hold = NoLid::default();
    let mut keeper = Keeper::with_hold(Box::new(hold)).with_sensors(fake.reader(), Some(lid));
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.want == IDLE_AND_LID
    });
    until_stepped(&mut keeper, &connection, &mut now_ms, || {
        heard.load(Ordering::SeqCst) >= 3
    });

    slow.store(true, Ordering::SeqCst);
    let started = Instant::now();
    keeper.stop();
    let waited = started.elapsed();
    assert!(
        waited + Duration::from_millis(200) >= STOP_WAIT,
        "stop waits for the lid line: {waited:?}"
    );
    assert!(
        waited < STOP_WAIT + Duration::from_secs(1),
        "stop waits at most STOP_WAIT: {waited:?}"
    );
}
