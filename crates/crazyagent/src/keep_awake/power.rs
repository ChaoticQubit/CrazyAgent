use super::state::{Heat, LidSupport, PowerSource, Sensors};
use crate::paths::Platform;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};

/// How often the keeper asks the platform for a reading.
pub const READ_MS: i64 = 30_000;

/// The reason the lid status shows when the system gave the idle hold and refused the
/// lid-closed part of it.
pub const LID_REFUSED: &str = "The system did not allow the lid-closed hold";

const PERCENT_MAX: u8 = 100;
const LATE_READS: i64 = 2;
const THREAD_NAME: &str = "keep-awake-sensors";

/// One reading of the platform: where the power comes from, the battery charge, the heat,
/// whether the lid is closed, and whether this platform can hold with the lid closed, each
/// unknown or none where the platform does not say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerReading {
    pub source: PowerSource,
    pub battery_pct: Option<u8>,
    pub heat: Heat,
    pub lid_closed: Option<bool>,
    pub lid_support: LidSupport,
}

impl PowerReading {
    /// A reading that knows nothing, with the lid reason this platform starts with.
    pub fn unknown() -> Self {
        let lid_support = Sensors::unread().lid_support;
        assert_ne!(lid_support, LidSupport::Held);
        Self {
            source: PowerSource::Unknown,
            battery_pct: None,
            heat: Heat::Unknown,
            lid_closed: None,
            lid_support,
        }
    }
}

/// Something that reads the platform once per call. It never fails: what cannot be read
/// is unknown in the reading it returns.
pub type Reader = Box<dyn FnMut() -> PowerReading + Send>;

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
        lid_support,
    }
}

/// The keeper's line to the sensor thread. The keeper asks for a reading and takes the
/// answer on a later step, so a slow system tool or bus never holds up the engine's team
/// step. The thread ends when this value is dropped.
#[derive(Debug)]
pub(super) struct Sensing {
    ask: SyncSender<()>,
    answers: Receiver<PowerReading>,
    asked_ms: Option<i64>,
    waiting: bool,
    late: bool,
    stopped: bool,
}

impl Sensing {
    /// Starts the sensor thread around a reader. Input: the reader, which the thread
    /// owns from here on. Output: the line to the thread, or the error of starting it.
    pub(super) fn start(mut reader: Reader) -> std::io::Result<Self> {
        let (ask, asked) = sync_channel::<()>(1);
        let (answer, answers) = sync_channel::<PowerReading>(1);
        std::thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn(move || {
                while asked.recv().is_ok() {
                    if answer.send(reader()).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            ask,
            answers,
            asked_ms: None,
            waiting: false,
            late: false,
            stopped: false,
        })
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
    /// when `READ_MS` passed since the last ask and none is under way. A reading that
    /// takes longer than two periods is logged once. Input: the engine time. Output: the
    /// reading that came back since the last step, if any.
    pub(super) fn step(&mut self, now_ms: i64) -> Option<PowerReading> {
        assert!(now_ms > 0);
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
        let due = since.is_none_or(|since| !(0..READ_MS).contains(&since));
        if due && !self.waiting {
            match self.ask.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) => {
                    self.waiting = true;
                    self.asked_ms = Some(now_ms);
                }
                Err(TrySendError::Disconnected(())) => return answer.or_else(|| self.stop()),
            }
        } else if self.waiting && !self.late && since.is_some_and(|s| s >= LATE_READS * READ_MS) {
            eprintln!("keep awake: a platform reading is taking long, the last one stays");
            self.late = true;
        }
        assert!(self.waiting || self.asked_ms.is_some());
        answer
    }
}
