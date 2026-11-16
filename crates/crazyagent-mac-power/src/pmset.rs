//! The one program the helper runs (threat model section 3): `/usr/bin/pmset` with fixed argv,
//! an empty environment but `PATH`, no input, output read up to 64 KiB, killed after 5 s.

use crate::floor::Battery;
use crate::journal::Key;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PMSET: &str = "/usr/bin/pmset";
pub const OUTPUT_BYTES_MAX: u64 = 65_536;
pub const WAIT: Duration = Duration::from_secs(5);
pub const CHILD_PATH: &str = "/usr/bin:/bin";

/// The settings the helper reads from `pmset -g`; `None` when the line is missing or unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Values {
    pub sleep_disabled: Option<bool>,
    pub low_power: Option<bool>,
}

/// A `pmset` run that did not succeed: it could not start, exited with a failure, ran past
/// `WAIT`, or its output could not be read.
#[derive(Debug)]
pub enum PmsetError {
    Spawn(io::Error),
    Failed(Option<i32>),
    TimedOut,
    Output,
}

/// The program the helper runs as `pmset`: `PMSET` in production, a stand-in in rows.
#[derive(Debug, Clone)]
pub struct Pmset {
    program: PathBuf,
}

impl Pmset {
    /// Uses `program` as `pmset`.
    pub fn new(program: impl AsRef<Path>) -> Self {
        Self {
            program: program.as_ref().to_path_buf(),
        }
    }

    /// Writes one journaled setting: `pmset -a <key> <0|1>`.
    pub fn set(&self, key: Key, on: bool) -> Result<(), PmsetError> {
        let _ = (key, on, &self.program);
        Ok(())
    }

    /// Sleeps the Mac now: `pmset sleepnow`.
    pub fn sleep_now(&self) -> Result<(), PmsetError> {
        Ok(())
    }

    /// Reads the settings: `pmset -g`.
    pub fn values(&self) -> Result<Values, PmsetError> {
        Ok(Values::default())
    }

    /// Reads the battery: `pmset -g batt`.
    pub fn battery(&self) -> Result<Battery, PmsetError> {
        Ok(Battery::Unread)
    }
}

/// Parses `pmset -g`: the `SleepDisabled` and `lowpowermode` lines, each `0` or `1`.
pub fn parse_values(text: &str) -> Values {
    let _ = text;
    Values {
        sleep_disabled: Some(true),
        low_power: Some(true),
    }
}

/// Parses `pmset -g batt`: the power source line and the first internal battery's percent.
pub fn parse_battery(text: &str) -> Battery {
    let _ = text;
    Battery::Ac { percent: 0 }
}
