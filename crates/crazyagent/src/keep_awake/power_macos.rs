use super::state::{Heat, PowerSource};

const TEXT_BYTES_MAX: usize = 8192;
const LINES_MAX: usize = 32;
const PERCENT_MAX: u8 = 100;

/// The public notify key behind the system thermal state.
pub const THERMAL_KEY: &str = "com.apple.system.thermalpressurelevel";

/// Reads the text of `pmset -g batt`: the first line names the source the Mac draws from,
/// and the first power device line below it (the internal battery, or a UPS) carries its
/// charge after a tab. Input: the text. Output: the source (`Unknown` for text this reader
/// does not know; a UPS counts as battery) and the percent, none when there is no device
/// line or it cannot be read.
pub fn battery(text: &str) -> (PowerSource, Option<u8>) {
    if text.len() > TEXT_BYTES_MAX {
        return (PowerSource::Unknown, None);
    }
    let mut lines = text.lines().take(LINES_MAX);
    let source = match lines.next().map(str::trim) {
        Some("Now drawing from 'AC Power'") => PowerSource::Ac,
        Some("Now drawing from 'Battery Power'" | "Now drawing from 'UPS Power'") => {
            PowerSource::Battery
        }
        _ => PowerSource::Unknown,
    };
    let percent = lines
        .find(|line| line.trim_start().starts_with('-'))
        .and_then(|line| line.split_once('\t'))
        .and_then(|(_, charge)| charge.split_once('%'))
        .and_then(|(digits, _)| digits.trim().parse::<u8>().ok())
        .filter(|percent| *percent <= PERCENT_MAX);
    assert!(percent.is_none_or(|percent| percent <= PERCENT_MAX));
    (source, percent)
}

/// Reads the text of `notifyutil -g com.apple.system.thermalpressurelevel`: the key and
/// the system's thermal pressure level. Input: the text. Output: 0 nominal, 1 fair, 2
/// serious, 3 and 4 critical, anything else unknown.
pub fn thermal(text: &str) -> Heat {
    if text.len() > TEXT_BYTES_MAX {
        return Heat::Unknown;
    }
    let mut words = text.split_whitespace();
    if words.next() != Some(THERMAL_KEY) {
        return Heat::Unknown;
    }
    let level = words.next().and_then(|word| word.parse::<u8>().ok());
    if words.next().is_some() {
        return Heat::Unknown;
    }
    match level {
        Some(0) => Heat::Nominal,
        Some(1) => Heat::Fair,
        Some(2) => Heat::Serious,
        Some(3 | 4) => Heat::Critical,
        _ => Heat::Unknown,
    }
}

#[cfg(target_os = "macos")]
mod live {
    use super::super::power::PowerReading;
    use super::super::state::{Heat, PowerSource};
    use super::{PERCENT_MAX, TEXT_BYTES_MAX, THERMAL_KEY, battery, thermal};
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const RUN_WAIT: Duration = Duration::from_secs(2);
    const RUN_STEP: Duration = Duration::from_millis(10);

    /// Runs one system tool with an empty environment and returns what it printed, waiting
    /// at most `RUN_WAIT` and reading at most `TEXT_BYTES_MAX`. Inputs: the program and its
    /// arguments. Output: the text, or none when the tool did not start, did not finish in
    /// time, failed, or printed something that is not text.
    pub(crate) fn run(program: &str, arguments: &[&str]) -> Option<String> {
        assert!(program.starts_with('/'));
        let mut command = Command::new(program);
        command
            .args(arguments)
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = crate::spawn_gate::spawn(&mut command).ok()?;
        let end = Instant::now() + RUN_WAIT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < end => std::thread::sleep(RUN_STEP),
                _ => break None,
            }
        };
        let Some(status) = status else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        let mut text = String::new();
        let read = child
            .stdout
            .take()?
            .take(TEXT_BYTES_MAX as u64)
            .read_to_string(&mut text);
        assert!(text.len() <= TEXT_BYTES_MAX);
        (status.success() && read.is_ok()).then_some(text)
    }

    /// Reads this Mac's power source, charge and thermal pressure from two system tools, and
    /// whether the lid is closed from `AppleClamshellState` on the power management root
    /// domain, read in process with no helper and no root (fix5 ASK-01:41, leader 01:42),
    /// closed with or without an external display, as Linux reads it, and whether an
    /// external display keeps it awake, from `AppleClamshellCausesSleep` (QUESTION-01:45 A).
    /// The lid support stays the reason the Mac starts with. Output: the reading, unknown
    /// where nothing was read.
    pub(crate) fn read() -> PowerReading {
        let (source, battery_pct) = run("/usr/bin/pmset", &["-g", "batt"])
            .map_or((PowerSource::Unknown, None), |text| battery(&text));
        let heat = run("/usr/bin/notifyutil", &["-g", THERMAL_KEY])
            .map_or(Heat::Unknown, |text| thermal(&text));
        let lid = crazyagent_mac_power::lid::read();
        let lid_closed = lid.map(|lid| lid.closed);
        let reading = PowerReading {
            source,
            battery_pct,
            heat,
            lid_closed,
            external_display: lid.map(|lid| lid.external_display),
            ..PowerReading::unknown()
        };
        assert_eq!(reading.lid_closed, lid_closed);
        assert!(
            reading
                .battery_pct
                .is_none_or(|percent| percent <= PERCENT_MAX)
        );
        reading
    }
}

#[cfg(target_os = "macos")]
pub(crate) use live::{read, run};
