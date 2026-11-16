use super::{NOW_MS, SECOND_MS};
use crate::keep_awake::LidSupport;
use crate::keep_awake::hold_macos::{
    ENDED_TO_PROTECT, HELPER_UNANSWERED, HelperLine, LEASE_S, LID_UNREAD, LID_WITHHELD_MACOS,
    LidLine, LidPlatform, PIN_NONE, PIN_OTHER, READ_EVERY_MS, SLEEP_NOT_RESTORED, STATUS_UNREAD,
    VERSION_UNREAD, gate, macos_major,
};
use crate::keep_awake::power::{LID_REFUSED, LidAsk, READ_MS};
use crazyagent_mac_power::client::CallError;
use crazyagent_mac_power::protocol::{FloorView, PinView, Refusal, Reply, Request, View};
use crazyagent_mac_power::service::{Service, Status};
use std::io;
use std::sync::{Arc, Mutex};

const OFF: LidAsk = LidAsk {
    rule_on: false,
    wanted: false,
};
const RESTING: LidAsk = LidAsk {
    rule_on: true,
    wanted: false,
};
const WANTED: LidAsk = LidAsk {
    rule_on: true,
    wanted: true,
};
const READING_MS: i64 = 30 * SECOND_MS;

/// A daemon registration the row steers: it answers with the status set last, counts the
/// status reads, and refuses to register or unregister anything.
#[derive(Debug, Clone)]
struct StandInService {
    status: Arc<Mutex<Option<Status>>>,
    reads: Arc<Mutex<u32>>,
}

impl Service for StandInService {
    fn status(&self) -> Option<Status> {
        *self.reads.lock().unwrap() += 1;
        *self.status.lock().unwrap()
    }

    fn register(&self) -> Result<(), String> {
        panic!("the lid line never registers the daemon")
    }

    fn unregister(&self) -> Result<(), String> {
        panic!("the lid line never unregisters the daemon")
    }

    fn open_login_items(&self) -> Result<(), String> {
        panic!("the lid line never opens System Settings")
    }
}

/// What the stand-in helper answers to a hold and to a read.
#[derive(Debug, Clone, Copy)]
struct Answers {
    hold: Result<Reply, CallError>,
    read: Result<Reply, CallError>,
}

/// A helper the row steers: it records every request and answers from `Answers`; a
/// release is always accepted.
#[derive(Debug, Clone)]
struct StandInHelper {
    calls: Arc<Mutex<Vec<Request>>>,
    answers: Arc<Mutex<Answers>>,
}

impl HelperLine for StandInHelper {
    fn call(&mut self, request: &Request) -> Result<Reply, CallError> {
        self.calls.lock().unwrap().push(*request);
        let answers = *self.answers.lock().unwrap();
        match request {
            Request::Hold { .. } => answers.hold,
            Request::Read => answers.read,
            Request::Release => Ok(Reply::Released),
            Request::Forget => panic!("the lid line never forgets the pin"),
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
    answers: Arc<Mutex<Answers>>,
}

impl Rig {
    fn new(platform: LidPlatform, status: Option<Status>) -> Self {
        let service = StandInService {
            status: Arc::new(Mutex::new(status)),
            reads: Arc::new(Mutex::new(0)),
        };
        let helper = StandInHelper {
            calls: Arc::new(Mutex::new(Vec::new())),
            answers: Arc::new(Mutex::new(Answers {
                hold: Ok(held()),
                read: Ok(ready()),
            })),
        };
        Self {
            status: service.status.clone(),
            status_reads: service.reads.clone(),
            calls: helper.calls.clone(),
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
        *self.answers.lock().unwrap() = Answers { hold, read };
    }

    fn set_status(&self, status: Option<Status>) {
        *self.status.lock().unwrap() = status;
    }
}

const HOLD: Request = Request::Hold { lease_s: LEASE_S };

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
    let mut fifteen = Rig::new(withheld, Some(Status::Enabled));
    assert_eq!(
        fifteen.line.step(WANTED, NOW_MS),
        not_available("Lid-closed hold is not available on macOS 15")
    );
    assert!(fifteen.calls().is_empty());

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
        not_available(VERSION_UNREAD),
        "a version that cannot be read fails closed"
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
            not_available(SLEEP_NOT_RESTORED),
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

    let mut renewal = Rig::enabled();
    assert_eq!(renewal.line.step(WANTED, NOW_MS), LidSupport::Held);
    renewal.answer(
        Ok(Reply::Refused {
            reason: Refusal::Pmset,
        }),
        Ok(ready()),
    );
    assert_eq!(renewal.line.step(WANTED, NOW_MS + READING_MS), refused);
    renewal.line.step(RESTING, NOW_MS + 2 * READING_MS);
    assert_eq!(
        renewal.calls(),
        [HOLD, HOLD, Request::Read, Request::Release],
        "a lease that may still run is released once the lid is not wanted"
    );
}

#[test]
fn a_hold_the_helper_did_not_answer_says_so_and_is_released_later() {
    let mut rig = Rig::enabled();
    assert_eq!(rig.line.step(WANTED, NOW_MS), LidSupport::Held);
    rig.answer(Err(CallError::Connect(io::ErrorKind::NotFound)), Ok(ready()));
    assert_eq!(
        rig.line.step(WANTED, NOW_MS + READING_MS),
        not_available(HELPER_UNANSWERED)
    );
    rig.line.step(RESTING, NOW_MS + 2 * READING_MS);
    assert_eq!(rig.calls(), [HOLD, HOLD, Request::Release]);
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
