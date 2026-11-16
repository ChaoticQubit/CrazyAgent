use crate::keep_awake::LowPowerSupport;
use crate::keep_awake::power_saver::{
    FAILED, NOT_RUNNING, PowerSaver, REASON_CHARS_MAX, REFUSED, Saver, SaverError,
};
use std::sync::{Arc, Mutex};

/// One call the power line made to the stand-in daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Present,
    Hold,
    Holds,
    Release(u32),
}

/// A power profiles daemon the row steers: whether it runs, what a hold answers instead of
/// a cookie, the holds it lists, the last cookie it gave, and every call it heard.
#[derive(Debug, Default)]
struct Daemon {
    running: bool,
    refuse: Option<SaverError>,
    holds: Vec<u32>,
    cookie_last: u32,
    calls: Vec<Call>,
}

/// The saver the power line talks to in these rows, over the shared stand-in daemon.
struct StandIn(Arc<Mutex<Daemon>>);

impl Saver for StandIn {
    fn present(&mut self) -> Result<(), SaverError> {
        let mut daemon = self.0.lock().unwrap();
        daemon.calls.push(Call::Present);
        if daemon.running {
            Ok(())
        } else {
            Err(SaverError::NotRunning)
        }
    }

    fn hold(&mut self) -> Result<u32, SaverError> {
        let mut daemon = self.0.lock().unwrap();
        daemon.calls.push(Call::Hold);
        if !daemon.running {
            return Err(SaverError::NotRunning);
        }
        if let Some(problem) = daemon.refuse.clone() {
            return Err(problem);
        }
        daemon.cookie_last += 1;
        let cookie = daemon.cookie_last;
        daemon.holds.push(cookie);
        Ok(cookie)
    }

    fn holds_ours(&mut self) -> Result<bool, SaverError> {
        let mut daemon = self.0.lock().unwrap();
        daemon.calls.push(Call::Holds);
        Ok(!daemon.holds.is_empty())
    }

    fn release(&mut self, cookie: u32) {
        let mut daemon = self.0.lock().unwrap();
        daemon.calls.push(Call::Release(cookie));
        daemon.holds.retain(|held| *held != cookie);
    }
}

/// A power line around a stand-in daemon. Input: whether the daemon runs. Output: the line
/// and the daemon the row steers.
fn line(running: bool) -> (PowerSaver, Arc<Mutex<Daemon>>) {
    let daemon = Arc::new(Mutex::new(Daemon {
        running,
        ..Daemon::default()
    }));
    let saver = StandIn(daemon.clone());
    (PowerSaver::new(Box::new(saver)), daemon)
}

fn not_available(reason: &str) -> LowPowerSupport {
    LowPowerSupport::NotAvailable {
        reason: reason.into(),
    }
}

fn count(daemon: &Arc<Mutex<Daemon>>, call: Call) -> usize {
    let heard = daemon.lock().unwrap();
    heard.calls.iter().filter(|made| **made == call).count()
}

#[test]
fn a_wanted_ask_holds_power_saver_once_and_an_unwanted_one_releases_it() {
    let (mut line, daemon) = line(true);
    assert_eq!(line.step(false), LowPowerSupport::Ready, "a first look finds the daemon");
    assert_eq!(line.step(true), LowPowerSupport::Held);
    assert_eq!(line.step(true), LowPowerSupport::Held);
    assert_eq!(daemon.lock().unwrap().holds, [1], "one hold for the whole ask");

    assert_eq!(line.step(false), LowPowerSupport::Ready);
    assert!(daemon.lock().unwrap().holds.is_empty(), "an unwanted ask lets go");
    assert_eq!(line.step(false), LowPowerSupport::Ready);
    assert_eq!(
        daemon.lock().unwrap().calls,
        [Call::Present, Call::Hold, Call::Holds, Call::Release(1)],
        "a resting line makes no call after its first look"
    );
}

#[test]
fn an_owner_who_picks_a_profile_keeps_it_for_the_rest_of_the_hold() {
    let (mut line, daemon) = line(true);
    assert_eq!(line.step(true), LowPowerSupport::Held);
    daemon.lock().unwrap().holds.clear();

    assert_eq!(line.step(true), LowPowerSupport::OwnerChanged);
    assert_eq!(line.step(true), LowPowerSupport::OwnerChanged);
    assert_eq!(count(&daemon, Call::Hold), 1, "the owner's profile is not taken back");

    assert_eq!(line.step(false), LowPowerSupport::Ready);
    assert_eq!(count(&daemon, Call::Release(1)), 0, "the daemon already dropped it");
    assert_eq!(line.step(true), LowPowerSupport::Held, "a new hold may ask again");
    assert_eq!(daemon.lock().unwrap().holds, [2]);
}

#[test]
fn a_missing_daemon_says_not_available_and_a_later_ask_tries_again() {
    let (mut line, daemon) = line(false);
    assert_eq!(line.step(false), not_available(NOT_RUNNING));
    assert_eq!(line.step(true), not_available(NOT_RUNNING));
    assert!(daemon.lock().unwrap().holds.is_empty());

    daemon.lock().unwrap().running = true;
    assert_eq!(line.step(true), LowPowerSupport::Held, "a daemon that came up is used");
    assert_eq!(line.step(false), LowPowerSupport::Ready);
    assert!(daemon.lock().unwrap().holds.is_empty());
}

#[test]
fn a_refused_or_failed_hold_says_why_in_bounded_words() {
    let (mut line, daemon) = line(true);
    daemon.lock().unwrap().refuse = Some(SaverError::Refused);
    assert_eq!(line.step(true), not_available(REFUSED));

    daemon.lock().unwrap().refuse = Some(SaverError::Failed("bus\nbroke\u{1b}".into()));
    assert_eq!(line.step(true), not_available(&format!("{FAILED}busbroke")));

    let long = format!("{}\u{7}{}", "x".repeat(400), "y".repeat(400));
    daemon.lock().unwrap().refuse = Some(SaverError::Failed(long));
    let LowPowerSupport::NotAvailable { reason } = line.step(true) else {
        panic!("a failed hold is not available");
    };
    let text = reason.strip_prefix(FAILED).expect("the failed words lead");
    assert_eq!(text.chars().count(), REASON_CHARS_MAX);
    assert!(!text.chars().any(char::is_control), "no control character is shown");
    assert!(daemon.lock().unwrap().holds.is_empty());
}
