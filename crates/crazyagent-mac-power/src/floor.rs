//! The helper's last-resort floor (threat model section 10, QUESTION 31b): on battery at 3 % or
//! less, or at thermal pressure level 3 or 4, or an unreadable level, every lease ends.

pub const BATTERY_PERCENT_FLOOR: u8 = 3;
pub const PRESSURE_CRITICAL_MIN: u64 = 3;
pub const PRESSURE_MAX: u64 = 4;

/// The battery as `pmset -g batt` reports it: no battery at all, on AC, on battery, or text
/// the helper could not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Battery {
    None,
    Ac { percent: u8 },
    OnBattery { percent: u8 },
    Unread,
}

/// The system thermal pressure level (0 nominal to 4 sleeping, K0-12), or a failed read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    Level(u64),
    Unread,
}

/// Why the floor ended the holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Floor {
    Battery,
    Heat,
}

/// Judges one reading. Inputs: the battery and the pressure. Output: the floor that applies,
/// heat first, or `None`.
pub fn judge(battery: Battery, pressure: Pressure) -> Option<Floor> {
    let hot = match pressure {
        Pressure::Level(level) => level >= PRESSURE_CRITICAL_MIN,
        Pressure::Unread => true,
    };
    if hot {
        return Some(Floor::Heat);
    }
    let empty = match battery {
        Battery::OnBattery { percent } => percent <= BATTERY_PERCENT_FLOOR,
        Battery::None | Battery::Ac { .. } | Battery::Unread => false,
    };
    empty.then_some(Floor::Battery)
}

const _: () = assert!(PRESSURE_CRITICAL_MIN <= PRESSURE_MAX);
const _: () = assert!(BATTERY_PERCENT_FLOOR < 100);
