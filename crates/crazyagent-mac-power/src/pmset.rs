//! The one program the helper runs (threat model section 3): `/usr/bin/pmset` with fixed argv,
//! an empty environment but `PATH`, no input, output read up to 64 KiB, killed after 5 s.

use crate::floor::Battery;
use crate::journal::Key;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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

    /// Writes one journaled setting: `SleepDisabled` on every source with `-a`, Low Power
    /// Mode on the battery source only with `-b`, so an owner's AC value is never touched
    /// (threat model 15.2, r9.1 F2).
    pub fn set(&self, key: Key, on: bool) -> Result<(), PmsetError> {
        let value = if on { "1" } else { "0" };
        let source = match key {
            Key::SleepDisabled => "-a",
            Key::LowPower => "-b",
        };
        self.run(&[source, key.pmset_name(), value]).map(drop)
    }

    /// Sleeps the Mac now: `pmset sleepnow`.
    pub fn sleep_now(&self) -> Result<(), PmsetError> {
        self.run(&["sleepnow"]).map(drop)
    }

    /// Sleeps the displays now: `pmset displaysleepnow`.
    pub fn display_sleep_now(&self) -> Result<(), PmsetError> {
        self.run(&["displaysleepnow"]).map(drop)
    }

    /// Reads the settings: `pmset -g`.
    pub fn values(&self) -> Result<Values, PmsetError> {
        self.run(&["-g"]).map(|text| parse_values(&text))
    }

    /// Reads the battery source's Low Power Mode: `pmset -g custom` (threat model 15.2).
    /// Output: the value, none when the battery section has no `lowpowermode` line, or why
    /// the run did not succeed.
    pub fn battery_low_power(&self) -> Result<Option<bool>, PmsetError> {
        self.run(&["-g", "custom"]).map(|text| parse_custom_low_power(&text))
    }

    /// Reads the battery: `pmset -g batt`.
    pub fn battery(&self) -> Result<Battery, PmsetError> {
        self.run(&["-g", "batt"]).map(|text| parse_battery(&text))
    }

    /// Runs the program once with `args`, an empty environment but `PATH` and no input.
    /// Output: its standard output, or why the run did not succeed.
    fn run(&self, args: &[&str]) -> Result<String, PmsetError> {
        assert!(!args.is_empty());
        assert!(args.len() <= ARGS_MAX);
        let mut child = Command::new(&self.program)
            .args(args)
            .env_clear()
            .env("PATH", CHILD_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(PmsetError::Spawn)?;
        let stdout = child.stdout.take().expect("standard output is piped");
        let reader = thread::spawn(move || read_capped(stdout));
        let status = wait_bounded(&mut child)?;
        if !status.success() {
            return Err(PmsetError::Failed(status.code()));
        }
        let bytes = reader.join().map_err(|_| PmsetError::Output)??;
        assert!(bytes.len() as u64 <= OUTPUT_BYTES_MAX);
        String::from_utf8(bytes).map_err(|_| PmsetError::Output)
    }
}

const ARGS_MAX: usize = 3;
const POLL: Duration = Duration::from_millis(10);
const POLLS_MAX: u32 = 501;
const _: () = assert!(POLL.as_millis() * (POLLS_MAX as u128 - 1) >= WAIT.as_millis());

/// Reads at most `OUTPUT_BYTES_MAX` bytes of the child's output. Output: the bytes, or
/// `Output` when the child wrote more or the read failed.
fn read_capped(stdout: ChildStdout) -> Result<Vec<u8>, PmsetError> {
    let mut bytes = Vec::new();
    let read = stdout.take(OUTPUT_BYTES_MAX + 1).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > OUTPUT_BYTES_MAX {
        return Err(PmsetError::Output);
    }
    Ok(bytes)
}

/// Waits for the child up to `WAIT`, then kills and reaps it. Output: its exit status, or
/// `TimedOut`, or `Failed(None)` when it could not be waited on.
fn wait_bounded(child: &mut Child) -> Result<ExitStatus, PmsetError> {
    let started = Instant::now();
    let mut outcome = Err(PmsetError::TimedOut);
    for _ in 0..POLLS_MAX {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() < WAIT => thread::sleep(POLL),
            Ok(None) => break,
            Err(_) => {
                outcome = Err(PmsetError::Failed(None));
                break;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(outcome.is_err());
    outcome
}

/// Parses `pmset -g`: the `SleepDisabled` and `lowpowermode` lines, each `0` or `1`.
pub fn parse_values(text: &str) -> Values {
    let mut values = Values::default();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(name), Some(word), None) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        let value = match word {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        };
        match name {
            "SleepDisabled" => values.sleep_disabled = value,
            "lowpowermode" => values.low_power = value,
            _ => {}
        }
    }
    values
}

/// The header line `pmset -g custom` prints before the battery source's settings.
const BATTERY_SECTION: &str = "Battery Power:";

/// Parses `pmset -g custom` for the battery source's `lowpowermode`, `0` or `1`: a line
/// that does not start with a space opens a section, and only the lines under
/// `BATTERY_SECTION` count. Input: the text, at most `OUTPUT_BYTES_MAX` bytes. Output: the
/// value, or none when the battery section is missing or has no readable line.
pub fn parse_custom_low_power(text: &str) -> Option<bool> {
    assert!(text.len() as u64 <= OUTPUT_BYTES_MAX);
    let mut in_battery = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            in_battery = line.trim_end() == BATTERY_SECTION;
            continue;
        }
        let mut words = line.split_whitespace();
        if !in_battery || words.next() != Some("lowpowermode") {
            continue;
        }
        return match (words.next(), words.next()) {
            (Some("0"), None) => Some(false),
            (Some("1"), None) => Some(true),
            _ => None,
        };
    }
    None
}

/// Parses `pmset -g batt`: the power source line and the first internal battery's percent.
pub fn parse_battery(text: &str) -> Battery {
    let source = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("Now drawing from '"))
        .and_then(|rest| rest.strip_suffix('\''));
    let percent = text
        .lines()
        .find(|line| line.trim_start().starts_with("-InternalBattery-"))
        .map(battery_percent);
    match (source, percent) {
        (Some("AC Power"), None) => Battery::None,
        (Some("AC Power"), Some(Some(percent))) => Battery::Ac { percent },
        (Some("Battery Power"), Some(Some(percent))) => Battery::OnBattery { percent },
        _ => Battery::Unread,
    }
}

/// The percent on one battery line: the number before `%` after the tab, at most 100.
fn battery_percent(line: &str) -> Option<u8> {
    let (_, rest) = line.split_once('\t')?;
    let (digits, _) = rest.split_once('%')?;
    let percent: u8 = digits.trim().parse().ok()?;
    (percent <= 100).then_some(percent)
}
