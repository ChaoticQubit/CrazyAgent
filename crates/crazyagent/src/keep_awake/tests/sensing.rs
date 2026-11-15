use super::*;
use crate::keep_awake::power::{LID_REFUSED, PowerReading, READ_MS, Reader};
use std::sync::atomic::{AtomicU32, Ordering};
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
            fake.reads.fetch_add(1, Ordering::SeqCst);
            fake.reading.lock().unwrap().clone()
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
fn steps(
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
fn step_until(
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
    fake.set(on_battery(None));
    assert!(until(|| fake.reads() >= 4));
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
