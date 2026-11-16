use super::sensing::{step_until, steps};
use super::{IDLE, IDLE_AND_LID, NOW_MS, SECOND_MS, agent, engine};
use crate::keep_awake::{Heat, Keeper, LidSupport, LowPowerSupport, PowerSource};
use crate::keep_awake::hold_macos::{
    ENDED_TO_PROTECT, HELPER_UNANSWERED, HelperLine, LEASE_S, LID_UNREAD, LID_WITHHELD_MACOS,
    LidLine, LidPlatform, PIN_FILE_KEPT, PIN_NONE, PIN_OTHER, READ_EVERY_MS, REMOVE_NOT_PUT_BACK,
    REMOVE_NOT_YOURS, REMOVE_REFUSED, REMOVE_UNANSWERED, REMOVE_UNFINISHED, STATUS_UNREAD,
    TURN_ON_FAILED, UNINSTALL_SLEEP_KEPT, VERSION_UNREAD, gate, macos_major,
};
use crate::keep_awake::power::{
    LID_REFUSED, LidAsk, LidCommand, LidReader, PowerReading, READ_MS, Reader,
};
use crazyagent_mac_power::client::CallError;
use crazyagent_mac_power::protocol::{
    FloorView, LidDisplay, PinView, Refusal, Reply, Request, View,
};
use crazyagent_mac_power::service::{Service, Status};
use std::io;
use std::sync::{Arc, Mutex};

const OFF: LidAsk = LidAsk {
    rule_on: false,
    wanted: false,
    sleep_display: false,
    low_power: false,
};
const RESTING: LidAsk = LidAsk {
    rule_on: true,
    wanted: false,
    sleep_display: false,
    low_power: false,
};
const WANTED: LidAsk = LidAsk {
    rule_on: true,
    wanted: true,
    sleep_display: true,
    low_power: false,
};
const READING_MS: i64 = 30 * SECOND_MS;

/// One call the stand-ins saw, in the order they saw them: a helper request or a call
/// on the daemon's registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Helper(Request),
    Register,
    Unregister,
    LoginItems,
}

/// A daemon registration the row steers: it answers with the status set last and counts
/// the status reads. Register and unregister move the status as macOS does, unless the
/// row set a refusal, which they return instead.
#[derive(Debug, Clone)]
struct StandInService {
    status: Arc<Mutex<Option<Status>>>,
    reads: Arc<Mutex<u32>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    refusal: Arc<Mutex<Option<String>>>,
}

impl StandInService {
    fn change(&self, seen: Seen, then: Status) -> Result<(), String> {
        self.seen.lock().unwrap().push(seen);
        if let Some(refusal) = self.refusal.lock().unwrap().clone() {
            return Err(refusal);
        }
        *self.status.lock().unwrap() = Some(then);
        Ok(())
    }
}

impl Service for StandInService {
    fn status(&self) -> Option<Status> {
        *self.reads.lock().unwrap() += 1;
        *self.status.lock().unwrap()
    }

    fn register(&self) -> Result<(), String> {
        self.change(Seen::Register, Status::RequiresApproval)
    }

    fn unregister(&self) -> Result<(), String> {
        self.change(Seen::Unregister, Status::NotRegistered)
    }

    fn open_login_items(&self) -> Result<(), String> {
        self.seen.lock().unwrap().push(Seen::LoginItems);
        Ok(())
    }
}

/// What the stand-in helper answers to a hold, a read and a forget.
#[derive(Debug, Clone, Copy)]
struct Answers {
    hold: Result<Reply, CallError>,
    read: Result<Reply, CallError>,
    forget: Result<Reply, CallError>,
}

/// A helper the row steers: it records every request and answers from `Answers`; a
/// release is always accepted.
#[derive(Debug, Clone)]
struct StandInHelper {
    calls: Arc<Mutex<Vec<Request>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    answers: Arc<Mutex<Answers>>,
}

impl HelperLine for StandInHelper {
    fn call(&mut self, request: &Request) -> Result<Reply, CallError> {
        self.calls.lock().unwrap().push(*request);
        self.seen.lock().unwrap().push(Seen::Helper(*request));
        let answers = *self.answers.lock().unwrap();
        match request {
            Request::Hold { .. } | Request::LowPower { .. } => answers.hold,
            Request::Read => answers.read,
            Request::Release => Ok(Reply::Released),
            Request::Forget => answers.forget,
        }
    }
}

/// A helper view with the pin, floor and journal the row names, and nothing held.
fn view(pin: PinView, floor: FloorView, journal_ok: bool) -> View {
    View {
        held: false,
        holders: 0,
        lease_left_s: 0,
        sleep_disabled: Some(false),
        foreign: false,
        journal_ok,
        floor,
        pin,
        lid_closed: Some(false),
        external_display: Some(false),
        low_power: Some(false),
        low_power_held: false,
    }
}

fn ready() -> Reply {
    Reply::State(view(PinView::You, FloorView::None, true))
}

fn held() -> Reply {
    Reply::Held {
        lease_left_s: LEASE_S,
    }
}

/// A Mac on the major version given, with the helper in its bundle and nothing withheld.
fn mac(major: u32) -> LidPlatform {
    LidPlatform {
        macos: Some(major),
        bundled: true,
        withheld: &[],
    }
}

fn not_available(reason: &str) -> LidSupport {
    LidSupport::NotAvailable {
        reason: reason.into(),
    }
}

/// A lid line wired to the stand-ins, and the handles the row steers them by.
struct Rig {
    line: LidLine,
    status: Arc<Mutex<Option<Status>>>,
    status_reads: Arc<Mutex<u32>>,
    calls: Arc<Mutex<Vec<Request>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    refusal: Arc<Mutex<Option<String>>>,
    answers: Arc<Mutex<Answers>>,
}

impl Rig {
    fn new(platform: LidPlatform, status: Option<Status>) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = StandInService {
            status: Arc::new(Mutex::new(status)),
            reads: Arc::new(Mutex::new(0)),
            seen: seen.clone(),
            refusal: Arc::new(Mutex::new(None)),
        };
        let helper = StandInHelper {
            calls: Arc::new(Mutex::new(Vec::new())),
            seen: seen.clone(),
            answers: Arc::new(Mutex::new(Answers {
                hold: Ok(held()),
                read: Ok(ready()),
                forget: Ok(Reply::Forgotten),
            })),
        };
        Self {
            status: service.status.clone(),
            status_reads: service.reads.clone(),
            calls: helper.calls.clone(),
            seen,
            refusal: service.refusal.clone(),
            answers: helper.answers.clone(),
            line: LidLine::new(Box::new(service), Box::new(helper), platform),
        }
    }

    fn enabled() -> Self {
        Self::new(mac(15), Some(Status::Enabled))
    }

    fn calls(&self) -> Vec<Request> {
        self.calls.lock().unwrap().clone()
    }

    fn answer(&self, hold: Result<Reply, CallError>, read: Result<Reply, CallError>) {
        let mut answers = self.answers.lock().unwrap();
        answers.hold = hold;
        answers.read = read;
    }

    fn forget_answer(&self, forget: Result<Reply, CallError>) {
        self.answers.lock().unwrap().forget = forget;
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn refuse(&self, refusal: &str) {
        *self.refusal.lock().unwrap() = Some(refusal.into());
    }

    fn set_status(&self, status: Option<Status>) {
        *self.status.lock().unwrap() = status;
    }
}

const HOLD: Request = Request::Hold {
    lease_s: LEASE_S,
    lid_display: LidDisplay::Sleep,
    low_power: false,
};

#[test]
fn below_macos_13_the_lid_support_says_needs_macos_13() {
    let mut twelve = Rig::new(mac(12), Some(Status::Enabled));
    assert_eq!(twelve.line.step(WANTED, NOW_MS), LidSupport::NeedsMacos13);
    assert!(twelve.calls().is_empty(), "below macOS 13 the helper is never called");
    assert_eq!(*twelve.status_reads.lock().unwrap(), 0);

    let mut thirteen = Rig::new(mac(13), Some(Status::Enabled));
    assert_eq!(thirteen.line.step(WANTED, NOW_MS), LidSupport::Held);
    assert_eq!(thirteen.calls(), [HOLD]);
}

#[test]
fn no_helper_in_the_bundle_says_helper_missing() {
    let bare = LidPlatform {
        bundled: false,
        ..mac(15)
    };
    let mut unbundled = Rig::new(bare, Some(Status::Enabled));
    assert_eq!(unbundled.line.step(WANTED, NOW_MS), LidSupport::HelperMissing);
    assert!(unbundled.calls().is_empty());
    assert_eq!(*unbundled.status_reads.lock().unwrap(), 0);

    let mut not_found = Rig::new(mac(15), Some(Status::NotFound));
    assert_eq!(not_found.line.step(WANTED, NOW_MS), LidSupport::HelperMissing);
    assert!(not_found.calls().is_empty(), "a daemon macOS cannot find is not called");
}

#[test]
fn the_daemon_status_picks_the_lid_words_and_only_enabled_reaches_the_helper() {
    let cases = [
        (None, not_available(STATUS_UNREAD)),
        (Some(Status::NotRegistered), LidSupport::NotTurnedOn),
        (Some(Status::RequiresApproval), LidSupport::NeedsApproval),
        (Some(Status::NotFound), LidSupport::HelperMissing),
        (Some(Status::Enabled), LidSupport::Held),
    ];
    for (status, words) in cases {
        let mut rig = Rig::new(mac(15), status);
        for ask in [OFF, RESTING, WANTED] {
            let support = rig.line.step(ask, NOW_MS);
            if status != Some(Status::Enabled) {
                assert_eq!(support, words, "{status:?} {ask:?}");
            }
        }
        let reached = !rig.calls().is_empty();
        assert_eq!(reached, status == Some(Status::Enabled), "{status:?}");
    }
    assert_eq!(STATUS_UNREAD, "The lid helper's state could not be read");
}

#[test]
fn a_withheld_or_unreadable_macos_version_turns_the_lid_hold_off() {
    assert!(
        LID_WITHHELD_MACOS.is_empty(),
        "live row L16 fills the withheld list"
    );
    let withheld = LidPlatform {
        withheld: &[15],
        ..mac(15)
    };
    let words = "Lid-closed hold is not available on macOS 15";
    let mut fifteen = Rig::new(withheld, Some(Status::Enabled));
    assert_eq!(
        fifteen.line.step(WANTED, NOW_MS),
        remove_helper(words),
        "a helper registered before the withhold is offered for removal (rv-sec2 22:38 F1)"
    );
    assert!(fifteen.calls().is_empty(), "no hold and no read where the lid is withheld");
    let mut never = Rig::new(withheld, Some(Status::NotRegistered));
    assert_eq!(never.line.step(WANTED, NOW_MS), not_available(words));

    let others = LidPlatform {
        withheld: &[15],
        ..mac(26)
    };
    let mut twenty_six = Rig::new(others, Some(Status::Enabled));
    assert_eq!(twenty_six.line.step(WANTED, NOW_MS), LidSupport::Held);

    let unread = LidPlatform {
        macos: None,
        ..mac(15)
    };
    let mut unknown = Rig::new(unread, Some(Status::Enabled));
    assert_eq!(
        unknown.line.step(WANTED, NOW_MS),
        remove_helper(VERSION_UNREAD),
        "a version that cannot be read fails closed for the hold and stays open for Remove"
    );
    assert!(unknown.calls().is_empty());
}

#[test]
fn the_lease_is_renewed_every_30_seconds_while_the_lid_is_wanted() {
    assert_eq!(READ_MS, READING_MS, "the sensor thread steps the lid line each reading");
    assert_eq!(LEASE_S, 120);
    assert_eq!(
        i64::from(LEASE_S) * 1000,
        4 * READ_MS,
        "a lease outlasts three missed renewals"
    );
    let mut rig = Rig::enabled();
    for reading in 0..4 {
        let now_ms = NOW_MS + reading * READING_MS;
        assert_eq!(rig.line.step(WANTED, now_ms), LidSupport::Held);
    }
    assert_eq!(rig.calls(), [HOLD; 4], "one renewal at each reading");

    let released_ms = NOW_MS + 4 * READING_MS;
    assert_eq!(rig.line.step(RESTING, released_ms), LidSupport::Held);
    assert_eq!(rig.calls().last(), Some(&Request::Release));
    assert_eq!(rig.calls().len(), 5);

    for reading in 1..=8 {
        rig.line.step(RESTING, released_ms + reading * READING_MS);
    }
    assert_eq!(
        rig.calls().len(),
        5,
        "a successful hold counts as the last read of the helper"
    );
    rig.line.step(RESTING, released_ms + 9 * READING_MS);
    assert_eq!(rig.calls().last(), Some(&Request::Read));
}

#[test]
fn no_helper_connection_while_the_lid_rule_is_off_or_the_daemon_is_not_enabled() {
    let mut off = Rig::enabled();
    for reading in 0..20 {
        let support = off.line.step(OFF, NOW_MS + reading * READING_MS);
        assert_eq!(support, not_available(LID_UNREAD));
    }
    assert!(off.calls().is_empty(), "the rule Off never reaches the helper");

    let mut waiting = Rig::new(mac(15), Some(Status::RequiresApproval));
    for reading in 0..20 {
        let support = waiting.line.step(WANTED, NOW_MS + reading * READING_MS);
        assert_eq!(support, LidSupport::NeedsApproval);
    }
    assert!(waiting.calls().is_empty());

    let mut holding = Rig::enabled();
    assert_eq!(holding.line.step(WANTED, NOW_MS), LidSupport::Held);
    holding.set_status(Some(Status::RequiresApproval));
    let later_ms = NOW_MS + READING_MS;
    assert_eq!(holding.line.step(RESTING, later_ms), LidSupport::NeedsApproval);
    assert_eq!(
        holding.calls(),
        [HOLD],
        "a daemon turned off in Login Items is not called, launchd stopped it"
    );
    drop(holding.line);
    assert_eq!(holding.calls.lock().unwrap().len(), 1, "nothing is held to release");
}

#[test]
fn with_no_hold_the_helper_is_read_at_most_every_five_minutes() {
    assert_eq!(READ_EVERY_MS, 300 * SECOND_MS);
    let mut rig = Rig::enabled();
    for reading in 0..20 {
        let support = rig.line.step(RESTING, NOW_MS + reading * READING_MS);
        assert_eq!(support, LidSupport::Held);
    }
    assert_eq!(rig.calls(), [Request::Read; 2], "read at 0 and 300 s");

    rig.set_status(Some(Status::RequiresApproval));
    let waiting_ms = NOW_MS + 20 * READING_MS;
    assert_eq!(rig.line.step(RESTING, waiting_ms), LidSupport::NeedsApproval);
    rig.set_status(Some(Status::Enabled));
    assert_eq!(
        rig.line.step(RESTING, waiting_ms + READING_MS),
        LidSupport::Held
    );
    assert_eq!(
        rig.calls(),
        [Request::Read; 3],
        "an approval is read at once, not at the next five minutes"
    );
}

#[test]
fn the_helper_view_picks_the_lid_words() {
    let remove = |reason: &str| LidSupport::RemoveHelper {
        reason: reason.into(),
    };
    let cases = [
        (ready(), LidSupport::Held),
        (
            Reply::State(view(PinView::You, FloorView::None, false)),
            LidSupport::SleepNotRestored,
        ),
        (
            Reply::State(view(PinView::You, FloorView::Battery, true)),
            not_available(ENDED_TO_PROTECT),
        ),
        (
            Reply::State(view(PinView::You, FloorView::Heat, true)),
            not_available(ENDED_TO_PROTECT),
        ),
        (
            Reply::State(view(PinView::Other, FloorView::None, true)),
            remove(PIN_OTHER),
        ),
        (
            Reply::State(view(PinView::None, FloorView::None, true)),
            remove(PIN_NONE),
        ),
        (Reply::Released, not_available(HELPER_UNANSWERED)),
    ];
    for (reply, words) in cases {
        let mut rig = Rig::enabled();
        rig.answer(Ok(held()), Ok(reply));
        assert_eq!(rig.line.step(RESTING, NOW_MS), words, "{reply:?}");
        assert_eq!(rig.calls(), [Request::Read]);
    }
    let mut silent = Rig::enabled();
    silent.answer(
        Ok(held()),
        Err(CallError::Exchange(io::ErrorKind::TimedOut)),
    );
    assert_eq!(
        silent.line.step(RESTING, NOW_MS),
        not_available(HELPER_UNANSWERED)
    );
    assert_eq!(ENDED_TO_PROTECT, "Ended to protect this Mac");
}

#[test]
fn a_refused_hold_reads_the_helper_once_and_says_why() {
    let mut floor = Rig::enabled();
    floor.answer(
        Ok(Reply::Refused {
            reason: Refusal::Floor,
        }),
        Ok(Reply::State(view(PinView::You, FloorView::Battery, true))),
    );
    assert_eq!(
        floor.line.step(WANTED, NOW_MS),
        not_available(ENDED_TO_PROTECT)
    );
    assert_eq!(floor.calls(), [HOLD, Request::Read]);

    let mut full = Rig::enabled();
    full.answer(
        Ok(Reply::Refused {
            reason: Refusal::HoldersFull,
        }),
        Ok(ready()),
    );
    let refused = not_available(LID_REFUSED);
    assert_eq!(full.line.step(WANTED, NOW_MS), refused);
    assert_eq!(
        full.line.step(RESTING, NOW_MS + READING_MS),
        refused,
        "the refusal stands until the next read"
    );
    assert_eq!(full.calls(), [HOLD, Request::Read]);
}

#[test]
fn a_failed_renewal_keeps_the_lid_held_until_the_lease_runs_out() {
    let mut renewal = Rig::enabled();
    assert_eq!(renewal.line.step(WANTED, NOW_MS), LidSupport::Held);
    renewal.answer(
        Ok(Reply::Refused {
            reason: Refusal::Pmset,
        }),
        Ok(ready()),
    );
    for reading in 1..4 {
        let support = renewal.line.step(WANTED, NOW_MS + reading * READING_MS);
        assert_eq!(support, LidSupport::Held, "a lease that still runs holds the lid");
    }
    assert_eq!(renewal.calls(), [HOLD; 4], "no read while the lease runs");
    let out_ms = NOW_MS + i64::from(LEASE_S) * 1000;
    assert_eq!(
        renewal.line.step(WANTED, out_ms),
        not_available(LID_REFUSED),
        "four failed renewals end the hold within the lease (threat model section 9)"
    );
    assert_eq!(renewal.calls(), [HOLD, HOLD, HOLD, HOLD, HOLD, Request::Read]);
    renewal.line.step(RESTING, out_ms + READING_MS);
    assert_eq!(renewal.calls().len(), 6, "a lease that ran out is not released");

    let mut floor = Rig::enabled();
    assert_eq!(floor.line.step(WANTED, NOW_MS), LidSupport::Held);
    floor.answer(
        Ok(Reply::Refused {
            reason: Refusal::Floor,
        }),
        Ok(Reply::State(view(PinView::You, FloorView::Battery, true))),
    );
    assert_eq!(
        floor.line.step(WANTED, NOW_MS + READING_MS),
        not_available(ENDED_TO_PROTECT),
        "a refusal other than pmset says why at once"
    );
}

#[test]
fn a_renewal_the_helper_did_not_answer_holds_and_is_released_later() {
    let mut rig = Rig::enabled();
    assert_eq!(rig.line.step(WANTED, NOW_MS), LidSupport::Held);
    rig.answer(Err(CallError::Connect(io::ErrorKind::NotFound)), Ok(ready()));
    assert_eq!(
        rig.line.step(WANTED, NOW_MS + READING_MS),
        LidSupport::Held,
        "the lease granted at the last reading still runs"
    );
    rig.line.step(RESTING, NOW_MS + 2 * READING_MS);
    assert_eq!(rig.calls(), [HOLD, HOLD, Request::Release]);

    let mut first = Rig::enabled();
    first.answer(
        Err(CallError::Exchange(io::ErrorKind::TimedOut)),
        Ok(ready()),
    );
    assert_eq!(
        first.line.step(WANTED, NOW_MS),
        not_available(HELPER_UNANSWERED)
    );
    first.line.step(RESTING, NOW_MS + READING_MS);
    assert_eq!(
        first.calls(),
        [HOLD],
        "a first hold that got no answer holds nothing and counts as the last read"
    );
}

#[test]
fn the_keeper_keeps_the_lid_through_failed_renewals_while_the_lease_runs() {
    let (_folder, connection) = engine();
    let rig = Rig::enabled();
    let calls = rig.calls.clone();
    let answers = rig.answers.clone();
    let mut line = rig.line;
    let lid: LidReader = Box::new(move |ask, _, now_ms| line.step(ask, now_ms));
    let reader: Reader = Box::new(|| PowerReading {
        source: PowerSource::Ac,
        battery_pct: Some(80),
        heat: Heat::Nominal,
        lid_closed: Some(false),
        lid_support: LidSupport::HelperMissing,
        low_power: LowPowerSupport::Unread,
    });
    let mut keeper = Keeper::default().with_sensors(reader, Some(lid));
    let mut now_ms = NOW_MS;
    agent(&connection, "working");
    let called = |request: Request| calls.lock().unwrap().contains(&request);

    step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.want == IDLE_AND_LID && called(HOLD)
    });
    answers.lock().unwrap().hold = Ok(Reply::Refused {
        reason: Refusal::Pmset,
    });
    let held = steps(&mut keeper, &connection, &mut now_ms, 65).unwrap();
    assert_eq!(
        held.state.want, IDLE_AND_LID,
        "two failed renewals inside the lease keep the lid"
    );
    assert!(!called(Request::Release), "a lease that still runs is kept");

    let ended = step_until(&mut keeper, &connection, &mut now_ms, |view| {
        view.state.want == IDLE
    });
    assert_eq!(ended.lid_support, not_available(LID_REFUSED));
    assert!(called(Request::Read));
    assert!(!called(Request::Release), "a lease that ran out is not released");
}

#[test]
fn a_dropped_lid_line_releases_its_hold() {
    let rig = Rig::enabled();
    let calls = rig.calls.clone();
    let mut line = rig.line;
    assert_eq!(line.step(WANTED, NOW_MS), LidSupport::Held);
    drop(line);
    assert_eq!(*calls.lock().unwrap(), [HOLD, Request::Release]);

    let idle = Rig::enabled();
    let idle_calls = idle.calls.clone();
    drop(idle.line);
    assert!(
        idle_calls.lock().unwrap().is_empty(),
        "a line that holds nothing calls nothing"
    );
}

#[test]
fn the_helper_connection_is_made_inside_the_spawn_gate() {
    assert!(!crate::spawn_gate::held());
    let mut inside = None;
    let mut connect = || {
        inside = Some(crate::spawn_gate::held());
        Err(io::Error::from(io::ErrorKind::NotFound))
    };
    let made = gate(&mut connect);
    assert_eq!(made.unwrap_err().kind(), io::ErrorKind::NotFound);
    assert_eq!(
        inside,
        Some(true),
        "the socket is made while the spawn gate is held"
    );
    assert!(!crate::spawn_gate::held(), "the gate is let go after");
}

#[test]
fn the_macos_version_is_the_first_number_of_sw_vers() {
    assert_eq!(macos_major("15.4.1\n"), Some(15));
    assert_eq!(macos_major("26.0\n"), Some(26));
    assert_eq!(macos_major("13\n"), Some(13));
    assert_eq!(macos_major("12.7.6"), Some(12));
    let long = format!("{}\n", "1".repeat(40));
    let wrongs = ["", "\n", "abc\n", "+15.1\n", " 15.1\n", "15.4\n16.0\n", "0.1\n"];
    for wrong in wrongs.into_iter().chain([long.as_str()]) {
        assert_eq!(macos_major(wrong), None, "{wrong:?}");
    }
}

#[test]
fn this_mac_reads_its_version_and_finds_no_helper_next_to_a_test_binary() {
    let mac = LidPlatform::this_mac();
    assert!(
        mac.macos.is_some_and(|major| major >= 13),
        "the row machine runs macOS 13 or later: {mac:?}"
    );
    assert!(!mac.bundled, "a test binary has no helper next to it");
    assert_eq!(mac.withheld, LID_WITHHELD_MACOS);
}

fn remove_helper(reason: &str) -> LidSupport {
    LidSupport::RemoveHelper {
        reason: reason.into(),
    }
}

#[test]
fn remove_lid_helper_sends_forget_before_unregister() {
    let mut rig = Rig::enabled();
    assert_eq!(rig.line.step(WANTED, NOW_MS), LidSupport::Held);
    rig.line.run(LidCommand::Remove, NOW_MS + SECOND_MS);
    assert_eq!(
        rig.seen()[1..],
        [Seen::Helper(Request::Forget), Seen::Unregister],
        "Forget, then unregister only on Forgotten (QUESTION 35i)"
    );
    let removed_ms = NOW_MS + 2 * SECOND_MS;
    assert_eq!(rig.line.step(OFF, removed_ms), LidSupport::NotTurnedOn);
    drop(rig.line);
    assert_eq!(
        rig.calls.lock().unwrap().len(),
        2,
        "the lease Forget ended is not released"
    );
}

#[test]
fn a_failed_unregister_keeps_offering_remove_lid_helper() {
    let mut rig = Rig::enabled();
    rig.refuse("Operation not permitted");
    rig.line.run(LidCommand::Remove, NOW_MS);
    assert_eq!(
        rig.seen(),
        [Seen::Helper(Request::Forget), Seen::Unregister]
    );
    let failed = remove_helper(&format!("{REMOVE_UNFINISHED}Operation not permitted"));
    for reading in 1..4 {
        let support = rig.line.step(RESTING, NOW_MS + reading * READING_MS);
        assert_eq!(support, failed, "the failure stands while the daemon stays registered");
    }
    rig.set_status(Some(Status::NotRegistered));
    let later_ms = NOW_MS + 5 * READING_MS;
    assert_eq!(rig.line.step(RESTING, later_ms), LidSupport::NotTurnedOn);
}

#[test]
fn a_refused_forget_keeps_the_daemon_registered_and_says_why() {
    let journal = Ok(Reply::Refused {
        reason: Refusal::JournalUnreadable,
    });
    let unreadable = Reply::State(view(PinView::You, FloorView::None, false));
    let cases = [
        (journal, Ok(ready()), remove_helper(PIN_FILE_KEPT)),
        (journal, Ok(unreadable), LidSupport::SleepNotRestored),
        (
            Ok(Reply::Refused {
                reason: Refusal::Pmset,
            }),
            Ok(ready()),
            remove_helper(REMOVE_NOT_PUT_BACK),
        ),
        (
            Ok(Reply::Refused {
                reason: Refusal::Unpinned,
            }),
            Ok(ready()),
            remove_helper(REMOVE_NOT_YOURS),
        ),
        (
            Ok(Reply::Refused {
                reason: Refusal::Version,
            }),
            Ok(ready()),
            remove_helper(REMOVE_REFUSED),
        ),
        (
            Err(CallError::Exchange(io::ErrorKind::TimedOut)),
            Ok(ready()),
            remove_helper(REMOVE_UNANSWERED),
        ),
    ];
    for (forget, read, words) in cases {
        let mut rig = Rig::enabled();
        rig.forget_answer(forget);
        rig.answer(Ok(held()), read);
        rig.line.run(LidCommand::Remove, NOW_MS);
        assert!(
            !rig.seen().contains(&Seen::Unregister),
            "a refused Forget never unregisters: {forget:?}"
        );
        assert_eq!(rig.line.step(RESTING, NOW_MS + SECOND_MS), words, "{forget:?}");
    }
}

#[test]
fn remove_while_the_daemon_is_not_enabled_goes_straight_to_unregister() {
    let mut waiting = Rig::new(mac(15), Some(Status::RequiresApproval));
    waiting.line.run(LidCommand::Remove, NOW_MS);
    assert_eq!(waiting.seen(), [Seen::Unregister], "no helper runs to forget");
    assert_eq!(
        waiting.line.step(RESTING, NOW_MS + SECOND_MS),
        LidSupport::NotTurnedOn
    );

    let mut never = Rig::new(mac(15), Some(Status::NotRegistered));
    never.line.run(LidCommand::Remove, NOW_MS);
    assert!(never.seen().is_empty(), "nothing to remove");
}

#[test]
fn turn_on_registers_then_opens_login_items_and_again_while_approval_waits() {
    let mut rig = Rig::new(mac(15), Some(Status::NotRegistered));
    assert_eq!(rig.line.step(RESTING, NOW_MS), LidSupport::NotTurnedOn);
    rig.line.run(LidCommand::TurnOn, NOW_MS + SECOND_MS);
    assert_eq!(rig.seen(), [Seen::Register, Seen::LoginItems]);
    let pending_ms = NOW_MS + 2 * SECOND_MS;
    assert_eq!(rig.line.step(RESTING, pending_ms), LidSupport::NeedsApproval);

    rig.line.run(LidCommand::TurnOn, pending_ms + SECOND_MS);
    assert_eq!(
        rig.seen(),
        [Seen::Register, Seen::LoginItems, Seen::LoginItems],
        "pressed while approval waits it opens Login Items again (QUESTION 35j)"
    );

    let mut enabled = Rig::enabled();
    enabled.line.run(LidCommand::TurnOn, NOW_MS);
    assert!(enabled.seen().is_empty(), "an enabled daemon is not registered again");
}

#[test]
fn a_failed_turn_on_says_why_until_the_status_changes() {
    let mut rig = Rig::new(mac(15), Some(Status::NotRegistered));
    rig.refuse("Invalid argument");
    rig.line.run(LidCommand::TurnOn, NOW_MS);
    assert_eq!(rig.seen(), [Seen::Register], "Login Items opens only after a register");
    let failed = not_available(&format!("{TURN_ON_FAILED}Invalid argument"));
    assert_eq!(rig.line.step(RESTING, NOW_MS + SECOND_MS), failed);
    assert_eq!(rig.line.step(RESTING, NOW_MS + READING_MS), failed);
    rig.set_status(Some(Status::RequiresApproval));
    let later_ms = NOW_MS + 2 * READING_MS;
    assert_eq!(rig.line.step(RESTING, later_ms), LidSupport::NeedsApproval);
}

/// The platforms where the lid is never held although the helper is in the bundle: a
/// withheld macOS version and one that cannot be read.
fn held_back() -> [(LidPlatform, &'static str); 2] {
    let withheld = LidPlatform {
        withheld: &[15],
        ..mac(15)
    };
    let unread = LidPlatform {
        macos: None,
        ..mac(15)
    };
    [
        (withheld, "Lid-closed hold is not available on macOS 15"),
        (unread, VERSION_UNREAD),
    ]
}

#[test]
fn turn_on_runs_only_behind_the_platform_gate_and_remove_needs_only_the_bundle() {
    let unbundled = LidPlatform {
        bundled: false,
        ..mac(15)
    };
    let [(withheld, _), (unread, _)] = held_back();
    for platform in [mac(12), unread, withheld, unbundled] {
        let mut rig = Rig::new(platform, Some(Status::NotRegistered));
        rig.line.run(LidCommand::TurnOn, NOW_MS);
        assert!(rig.seen().is_empty(), "nothing is registered: {platform:?}");
        assert_eq!(*rig.status_reads.lock().unwrap(), 0, "{platform:?}");
    }
    let mut rig = Rig::new(unbundled, Some(Status::Enabled));
    rig.line.run(LidCommand::Remove, NOW_MS);
    assert!(rig.seen().is_empty(), "no helper in the bundle, nothing to remove");
    assert_eq!(*rig.status_reads.lock().unwrap(), 0);
}

#[test]
fn remove_lid_helper_on_a_withheld_or_unread_version_sends_forget_then_unregister() {
    for (platform, words) in held_back() {
        let mut rig = Rig::new(platform, Some(Status::Enabled));
        assert_eq!(rig.line.step(RESTING, NOW_MS), remove_helper(words), "{platform:?}");
        rig.line.run(LidCommand::Remove, NOW_MS + SECOND_MS);
        assert_eq!(
            rig.seen(),
            [Seen::Helper(Request::Forget), Seen::Unregister],
            "Remove always works (rv-sec2 22:38 F1): {platform:?}"
        );
        let removed_ms = NOW_MS + 2 * SECOND_MS;
        assert_eq!(rig.line.step(RESTING, removed_ms), not_available(words));

        let mut waiting = Rig::new(platform, Some(Status::RequiresApproval));
        assert_eq!(waiting.line.step(RESTING, NOW_MS), remove_helper(words));
        waiting.line.run(LidCommand::Remove, NOW_MS + SECOND_MS);
        assert_eq!(waiting.seen(), [Seen::Unregister], "{platform:?}");
    }
}

#[test]
fn a_command_is_followed_by_one_read_of_the_helper() {
    let mut rig = Rig::new(mac(15), Some(Status::RequiresApproval));
    for reading in 0..3 {
        rig.line.step(RESTING, NOW_MS + reading * READING_MS);
    }
    rig.set_status(Some(Status::Enabled));
    let approved_ms = NOW_MS + 3 * READING_MS;
    assert_eq!(rig.line.step(RESTING, approved_ms), LidSupport::Held);
    rig.line.run(LidCommand::TurnOn, approved_ms + SECOND_MS);
    assert_eq!(
        rig.line.step(RESTING, approved_ms + 2 * SECOND_MS),
        LidSupport::Held
    );
    assert_eq!(
        rig.calls(),
        [Request::Read; 2],
        "one read after the approval and one after the command (QUESTION 35d)"
    );
}

#[test]
fn uninstall_from_a_bundle_with_no_helper_reaches_no_smappservice() {
    let unbundled = LidPlatform {
        bundled: false,
        ..mac(15)
    };
    let mut rig = Rig::new(unbundled, Some(Status::Enabled));
    assert_eq!(rig.line.uninstall(NOW_MS), None);
    assert_eq!(*rig.status_reads.lock().unwrap(), 0);
    assert!(rig.seen().is_empty());
}

#[test]
fn uninstall_on_a_withheld_or_unread_version_still_removes_the_helper() {
    for (platform, _) in held_back() {
        let mut rig = Rig::new(platform, Some(Status::Enabled));
        assert_eq!(rig.line.uninstall(NOW_MS), None, "{platform:?}");
        assert_eq!(
            rig.seen(),
            [Seen::Helper(Request::Forget), Seen::Unregister],
            "{platform:?}"
        );
        let mut unfinished = Rig::new(platform, Some(Status::Enabled));
        unfinished.refuse("Operation not permitted");
        let words = format!("{REMOVE_UNFINISHED}Operation not permitted");
        assert_eq!(unfinished.line.uninstall(NOW_MS), Some(words), "{platform:?}");
    }
}

#[test]
fn uninstall_removes_the_lid_helper_and_says_nothing_when_it_did() {
    let mut rig = Rig::enabled();
    assert_eq!(rig.line.uninstall(NOW_MS), None);
    assert_eq!(
        rig.seen(),
        [Seen::Helper(Request::Forget), Seen::Unregister],
        "Forget, then unregister, as Remove lid helper does (QUESTION 37b)"
    );
    let mut never = Rig::new(mac(15), Some(Status::NotRegistered));
    assert_eq!(never.line.uninstall(NOW_MS), None, "nothing to remove");
    assert!(never.seen().is_empty());
}

#[test]
fn uninstall_says_what_it_could_not_remove() {
    let mut unfinished = Rig::enabled();
    unfinished.refuse("Operation not permitted");
    let words = format!("{REMOVE_UNFINISHED}Operation not permitted");
    assert_eq!(unfinished.line.uninstall(NOW_MS), Some(words));

    let mut pmset = Rig::enabled();
    pmset.forget_answer(Ok(Reply::Refused {
        reason: Refusal::Pmset,
    }));
    assert_eq!(pmset.line.uninstall(NOW_MS), Some(REMOVE_NOT_PUT_BACK.into()));

    let mut sleep = Rig::enabled();
    sleep.forget_answer(Ok(Reply::Refused {
        reason: Refusal::JournalUnreadable,
    }));
    let unreadable = Reply::State(view(PinView::You, FloorView::None, false));
    sleep.answer(Ok(held()), Ok(unreadable));
    assert_eq!(sleep.line.uninstall(NOW_MS), Some(UNINSTALL_SLEEP_KEPT.into()));
    assert!(!sleep.seen().contains(&Seen::Unregister), "the helper stays to put sleep back");
}

#[test]
fn every_hold_names_the_display_the_owner_picked() {
    let mut rig = Rig::enabled();
    assert_eq!(rig.line.step(WANTED, NOW_MS), LidSupport::Held);
    let left_on = LidAsk {
        sleep_display: false,
        ..WANTED
    };
    assert_eq!(rig.line.step(left_on, NOW_MS + READING_MS), LidSupport::Held);
    let leave_on = Request::Hold {
        lease_s: LEASE_S,
        lid_display: LidDisplay::LeaveOn,
        low_power: false,
    };
    assert_eq!(rig.calls(), [HOLD, leave_on], "the owner's pick rides on each Hold (Task 12)");
}
