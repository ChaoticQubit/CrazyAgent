use super::state::{Heat, LidSupport, LowPowerSupport, PowerSource, Sensors};
use crate::paths::Platform;
use serde::{Deserialize, Serialize};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::time::{Duration, Instant};

/// How often the keeper asks the platform for a reading.
pub const READ_MS: i64 = 30_000;

/// The reason the lid status shows when the system gave the idle hold and refused the
/// lid-closed part of it.
pub const LID_REFUSED: &str = "The system did not allow the lid-closed hold";

/// The start of the lid words for a Turn on that SMAppService refused, which the window and
/// the TUI read to offer Turn on again.
pub const TURN_ON_FAILED: &str = "Lid-closed hold could not be turned on: ";

const PERCENT_MAX: u8 = 100;
const LATE_READS: i64 = 2;
const THREAD_NAME: &str = "keep-awake-sensors";

/// One ask to the sensor thread: the keeper's lid ask, an owner command if one waits, and
/// the engine time.
type Ask = (LidAsk, Option<LidCommand>, i64);

/// One reading of the platform: where the power comes from, the battery charge, the heat,
/// whether the lid is closed, whether an external display is in use (fix5 QUESTION-01:45
/// A, leader 01:45), whether this platform can hold with the lid closed, and whether it can
/// turn on low power mode, each unknown or none where the platform does not say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerReading {
    pub source: PowerSource,
    pub battery_pct: Option<u8>,
    pub heat: Heat,
    pub lid_closed: Option<bool>,
    pub external_display: Option<bool>,
    pub lid_support: LidSupport,
    pub low_power: LowPowerSupport,
}

impl PowerReading {
    /// A reading that knows nothing, with the lid and low power reasons this platform starts
    /// with.
    pub fn unknown() -> Self {
        let Sensors {
            lid_support,
            low_power,
            ..
        } = Sensors::unread();
        assert_ne!(lid_support, LidSupport::Held);
        assert_ne!(low_power, LowPowerSupport::Held);
        Self {
            source: PowerSource::Unknown,
            battery_pct: None,
            heat: Heat::Unknown,
            lid_closed: None,
            external_display: None,
            lid_support,
            low_power,
        }
    }
}

/// Something that reads the platform once per call. It never fails: what cannot be read
/// is unknown in the reading it returns.
pub type Reader = Box<dyn FnMut() -> PowerReading + Send>;

/// What the keeper tells the lid line with each ask: whether the lid rule is on, whether
/// the decision holds the closed lid, which only a rule that is on allows, whether the
/// display sleeps once per close while it does, which only a wanted ask carries (Task 12),
/// and whether the running hold asks for low power mode, with or without the lid (Task 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LidAsk {
    pub rule_on: bool,
    pub wanted: bool,
    pub sleep_display: bool,
    pub low_power: bool,
}

/// An owner command for the lid helper (QUESTION 36a): Turn on lid-closed hold, or Remove
/// lid helper. The lid line runs it on the sensor thread, never in the owner's change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LidCommand {
    TurnOn,
    Remove,
}

/// A lid command waiting in the run state, with its number in this engine run, which only
/// goes up, so the keeper hands each one on once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LidOrder {
    pub command: LidCommand,
    pub number: u32,
}

/// The lid line on the sensor thread (DEC-167 addendum 18): handed the keeper's lid ask,
/// an owner command when one came, and the engine time with each reading, it runs the
/// command, holds or lets go of the closed lid, and returns the lid support, which
/// stands in for the reader's.
pub type LidReader = Box<dyn FnMut(LidAsk, Option<LidCommand>, i64) -> LidSupport + Send>;


/// The power line on the sensor thread (Task 16, QUESTION 39g): handed whether the running
/// hold asks for low power mode and the engine time with each reading, it holds or lets go
/// of the platform's power saver and returns the low power support, which stands in for
/// the reader's.
pub type PowerLine = Box<dyn FnMut(bool, i64) -> LowPowerSupport + Send>;

/// Builds this platform's lid line and power line (QUESTION 40b). On macOS both ride the
/// lid helper: the power line returns what the lid line's last step left and never calls
/// the helper. On Linux only the power line runs, on power-profiles-daemon, and the
/// reader's own lid support stands; Windows has neither. Output: each line, if this
/// platform has it.
pub fn platform_lines() -> (Option<LidReader>, Option<PowerLine>) {
    #[cfg(target_os = "macos")]
    let lines = {
        let (lid, power) = super::hold_macos::lines();
        (Some(lid), Some(power))
    };
    #[cfg(target_os = "linux")]
    let lines = (None, Some(super::low_power_linux::power_line()));
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let lines = (None, None);
    lines
}

/// Builds this platform's reader: system tools on macOS, files below `/sys`, `/proc` and
/// `/run` on Linux (no bus connection), the system power status on Windows. Output: the
/// reader.
pub fn platform_reader() -> Reader {
    #[cfg(target_os = "macos")]
    let reader: Reader = Box::new(super::power_macos::read);
    #[cfg(target_os = "linux")]
    let reader: Reader = Box::new(super::power_linux::read);
    #[cfg(windows)]
    let reader: Reader = Box::new(super::power_windows::read);
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let reader: Reader = Box::new(PowerReading::unknown);
    reader
}

/// Reads this computer's power source, battery charge, heat and lid once. It never fails;
/// what cannot be read is unknown. Output: the reading.
pub fn read_power() -> PowerReading {
    let mut reader = platform_reader();
    let reading = reader();
    assert!(
        reading
            .battery_pct
            .is_none_or(|percent| percent <= PERCENT_MAX)
    );
    assert_ne!(reading.lid_support, LidSupport::Off);
    reading
}

/// Tells why the heat settings do nothing on a platform, in the words the settings show.
/// Input: the platform. Output: none where heat is read, or the plain reason.
pub fn heat_unavailable(platform: Platform) -> Option<&'static str> {
    match platform {
        Platform::Windows => Some("Not available on Windows"),
        Platform::MacOs | Platform::Linux => None,
    }
}

/// Turns one reading into the readings `decide` looks at. Inputs: the reading, whether
/// this is the second reading in a row on battery with no charge, and whether the system
/// refused the lid part of a hold in this engine run. Output: the sensors; a refused lid
/// shows as not available with its reason.
pub fn sensors(reading: &PowerReading, battery_unread_twice: bool, lid_refused: bool) -> Sensors {
    let unread = reading.source == PowerSource::Battery && reading.battery_pct.is_none();
    assert!(
        reading
            .battery_pct
            .is_none_or(|percent| percent <= PERCENT_MAX)
    );
    assert!(!battery_unread_twice || unread);
    assert_ne!(reading.lid_support, LidSupport::Off);
    assert_ne!(reading.lid_support, LidSupport::IdleOnlyOnBattery);
    let lid_support = if lid_refused && reading.lid_support == LidSupport::Held {
        LidSupport::NotAvailable {
            reason: LID_REFUSED.into(),
        }
    } else {
        reading.lid_support.clone()
    };
    Sensors {
        power: reading.source,
        battery_pct: reading.battery_pct,
        battery_unread_twice,
        heat: reading.heat,
        lid_closed: reading.lid_closed,
        external_display: reading.external_display,
        lid_support,
        low_power: reading.low_power.clone(),
    }
}

/// The keeper's line to the sensor thread. The keeper asks for a reading and takes the
/// answer on a later step, so a slow system tool, bus or lid helper never holds up the
/// engine's team step. Each ask carries the keeper's lid ask, and an owner command when
/// one waits, for the lid line, and the low power ask for the power line, if the thread
/// runs them. The thread ends when this value is dropped, and a power line's hold with it.
#[derive(Debug)]
pub(super) struct Sensing {
    ask: SyncSender<Ask>,
    answers: Receiver<PowerReading>,
    lid: bool,
    power: bool,
    pending: Option<LidCommand>,
    asked_ms: Option<i64>,
    asked_with: Option<LidAsk>,
    waiting: bool,
    late: bool,
    stopped: bool,
}

impl Sensing {
    /// Starts the sensor thread around a reader and, where there is one, the lid line,
    /// whose lid support stands in for the reader's in every reading; the rows' short form
    /// of `start_lines`. Inputs: the reader and the lid line, which the thread owns from
    /// here on. Output: the line to the thread, or the error of starting it.
    #[cfg(test)]
    pub(super) fn start(reader: Reader, lid: Option<LidReader>) -> std::io::Result<Self> {
        Self::start_lines(reader, lid, None)
    }

    /// Starts the sensor thread around a reader, the lid line and the power line, where
    /// the platform has them; each line's support stands in for the reader's in every
    /// reading. Inputs: the reader and both lines, which the thread owns from here on.
    /// Output: the line to the thread, or the error of starting it.
    pub(super) fn start_lines(
        mut reader: Reader,
        mut lid: Option<LidReader>,
        mut power: Option<PowerLine>,
    ) -> std::io::Result<Self> {
        let (ask, asked) = sync_channel::<Ask>(1);
        let (answer, answers) = sync_channel::<PowerReading>(1);
        let (has_lid, has_power) = (lid.is_some(), power.is_some());
        std::thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn(move || {
                while let Ok((lid_ask, command, now_ms)) = asked.recv() {
                    let mut reading = reader();
                    if let Some(lid) = lid.as_mut() {
                        reading.lid_support = lid(lid_ask, command, now_ms);
                    }
                    if let Some(power) = power.as_mut() {
                        reading.low_power = power(lid_ask.low_power, now_ms);
                    }
                    assert_ne!(reading.lid_support, LidSupport::Off);
                    if answer.send(reading).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            ask,
            answers,
            lid: has_lid,
            power: has_power,
            pending: None,
            asked_ms: None,
            asked_with: None,
            waiting: false,
            late: false,
            stopped: false,
        })
    }

    /// Hands an owner command to the lid line with the next ask, which goes out at once
    /// unless a reading is under way; a later command before then replaces it. Input: the
    /// command.
    pub(super) fn order(&mut self, command: LidCommand) {
        if !self.lid {
            eprintln!("keep awake: no lid line, the lid command {command:?} is dropped");
            return;
        }
        self.pending = Some(command);
        assert!(self.pending.is_some());
    }

    /// Sends a last ask and waits for its answer, at most `wait` in all, so a lid or low
    /// power lease is released before the engine stops. A reading under way is waited for
    /// first; the last ask goes only when the ask sent before it wanted the lid or low power.
    /// It does nothing without a lid or power line, or before any ask. Inputs: the last ask
    /// and the longest wait.
    pub(super) fn finish(&mut self, last: LidAsk, wait: Duration) {
        assert!(!last.wanted);
        assert!(!last.sleep_display);
        assert!(!last.low_power);
        assert!(!wait.is_zero());
        let lines = self.lid || self.power;
        let Some(asked_ms) = self.asked_ms.filter(|_| lines && !self.stopped) else {
            return;
        };
        let until = Instant::now() + wait;
        let left = || until.saturating_duration_since(Instant::now());
        if self.waiting && self.answers.recv_timeout(left()).is_err() {
            return;
        }
        self.waiting = false;
        if !self.asked_with.is_some_and(|asked| asked.wanted || asked.low_power) {
            return;
        }
        if self.ask.try_send((last, None, asked_ms)).is_err() {
            return;
        }
        let answered = self.answers.recv_timeout(left()).is_ok();
        self.waiting = !answered;
        self.asked_with = Some(last);
        if !answered {
            eprintln!("keep awake: the lid line did not answer the last ask in time");
        }
    }

    /// Notes that the sensor thread is gone, once, and answers with a reading that knows
    /// nothing so the status does not go on showing the last one.
    fn stop(&mut self) -> Option<PowerReading> {
        if self.stopped {
            return None;
        }
        eprintln!("keep awake: the sensor thread stopped, the floors read nothing from here on");
        self.stopped = true;
        self.waiting = false;
        Some(PowerReading::unknown())
    }

    /// Takes one step: hands over a reading that came back, and asks for the next one
    /// when `READ_MS` passed since the last ask, or at once when the thread runs a lid
    /// line and the lid ask changed, or a power line and the low power ask changed, while
    /// none is under way. A reading that takes longer
    /// than two periods is logged once. Inputs: the engine time and the lid ask. Output:
    /// the reading that came back since the last step, if any.
    pub(super) fn step(&mut self, now_ms: i64, lid_ask: LidAsk) -> Option<PowerReading> {
        assert!(now_ms > 0);
        assert!(lid_ask.rule_on || !lid_ask.wanted);
        assert!(lid_ask.wanted || !lid_ask.sleep_display);
        let answer = match self.answers.try_recv() {
            Ok(reading) => {
                self.waiting = false;
                self.late = false;
                Some(reading)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => self.stop(),
        };
        if self.stopped {
            return answer;
        }
        let since = self.asked_ms.map(|asked| now_ms - asked);
        let lid_changed = self.lid && self.asked_with != Some(lid_ask);
        let low_power_asked = self.asked_with.map(|asked| asked.low_power);
        let power_changed = self.power && low_power_asked != Some(lid_ask.low_power);
        let changed = lid_changed || power_changed;
        let ordered = self.pending.is_some();
        let due = changed || ordered || since.is_none_or(|since| !(0..READ_MS).contains(&since));
        if due && !self.waiting {
            let sent = self.ask.try_send((lid_ask, self.pending, now_ms));
            if sent.is_ok() {
                self.pending = None;
            }
            match sent {
                Ok(()) | Err(TrySendError::Full(_)) => {
                    self.waiting = true;
                    self.asked_ms = Some(now_ms);
                    self.asked_with = Some(lid_ask);
                }
                Err(TrySendError::Disconnected(_)) => return answer.or_else(|| self.stop()),
            }
        } else if self.waiting && !self.late && since.is_some_and(|s| s >= LATE_READS * READ_MS) {
            eprintln!("keep awake: a platform reading is taking long, the last one stays");
            self.late = true;
        }
        assert!(self.waiting || self.asked_ms.is_some());
        assert_eq!(self.asked_with.is_some(), self.asked_ms.is_some());
        assert!(self.lid || self.pending.is_none());
        answer
    }
}
