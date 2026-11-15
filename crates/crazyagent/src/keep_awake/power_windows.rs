use super::power::PowerReading;
use super::state::PowerSource;

/// Reads this computer's power source and battery charge from the Windows system power
/// status. Windows has no public heat signal and the lid is not read here, so both stay
/// unknown, and the lid support stays the reason Windows starts with. Output: the
/// reading, all unknown when the status call fails.
pub(crate) fn read() -> PowerReading {
    let Ok(status) = crazyagent_win_sandbox::power::status() else {
        return PowerReading::unknown();
    };
    let source = match status.ac {
        Some(true) => PowerSource::Ac,
        Some(false) => PowerSource::Battery,
        None => PowerSource::Unknown,
    };
    assert!(status.battery_pct.is_none_or(|percent| percent <= 100));
    PowerReading {
        source,
        battery_pct: status.battery_pct,
        ..PowerReading::unknown()
    }
}
