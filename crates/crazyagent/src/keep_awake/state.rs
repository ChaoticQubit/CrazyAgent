use super::settings::{
    DisplayAwake, HeatEnd, HeatNotice, KeepAwakeSettings, LidRule, LowPower, Mode,
};
use crate::paths::Platform;
use serde::{Deserialize, Serialize};

/// Where the computer's power comes from, as the platform reader last saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    Ac,
    Battery,
    Unknown,
}

/// How hot the computer is, in the platform's own levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Heat {
    Nominal,
    Fair,
    Serious,
    Critical,
    Unknown,
}

/// Whether a closed lid can be held on this computer now, or the plain reason it cannot.
/// From a platform reader `Held` means the platform can hold the lid; from `lid_status`
/// it means the lid rule and the power source allow it too. `NotTurnedOn` is a macOS lid
/// helper never registered (the panel offers Turn on), `RemoveHelper` one the owner must
/// remove and add again, with the reason (the panel offers Remove lid helper), and
/// `SleepNotRestored` a lid helper that cannot read its journal (the restore notice says
/// how to put sleep back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LidSupport {
    Held,
    Off,
    IdleOnlyOnBattery,
    NeedsMacos13,
    NeedsApproval,
    HelperMissing,
    NotTurnedOn,
    RemoveHelper { reason: String },
    SleepNotRestored,
    LockedByPolicy,
    NotAvailable { reason: String },
}

/// The words the low power row shows on Windows, where no public call holds battery saver
/// (QUESTION 39f).
pub const LOW_POWER_WINDOWS: &str = "Not available on Windows";

/// Whether a hold can turn on the platform's low power mode (Task 16): not read yet, ready,
/// on now for the running hold, left as the owner set it during this hold, or not available
/// with the plain reason.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LowPowerSupport {
    #[default]
    Unread,
    Ready,
    Held,
    OwnerChanged,
    NotAvailable { reason: String },
}

impl LowPowerSupport {
    /// What a platform says before its low power line ran: not available on Windows, not
    /// read yet elsewhere. Input: the platform. Output: the support.
    pub fn at_start(platform: Platform) -> Self {
        let red_stub = platform;
        assert!(matches!(
            red_stub,
            Platform::MacOs | Platform::Linux | Platform::Windows
        ));
        Self::Unread
    }
}

/// What the platform readers last saw: power source, battery percent, whether the battery
/// could not be read twice in a row, heat, lid position, lid support and low power support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sensors {
    pub power: PowerSource,
    pub battery_pct: Option<u8>,
    pub battery_unread_twice: bool,
    pub heat: Heat,
    pub lid_closed: Option<bool>,
    pub lid_support: LidSupport,
    #[serde(default)]
    pub low_power: LowPowerSupport,
}

impl Sensors {
    /// The readings before any platform reader ran: nothing known, and the lid support
    /// this platform has with no lid hold set up. Output: the readings.
    pub fn unread() -> Self {
        let lid_support = if cfg!(target_os = "macos") {
            LidSupport::HelperMissing
        } else {
            LidSupport::NotAvailable {
                reason: "Lid-closed hold is not set up on this computer".into(),
            }
        };
        Self {
            power: PowerSource::Unknown,
            battery_pct: None,
            battery_unread_twice: false,
            heat: Heat::Unknown,
            lid_closed: None,
            lid_support,
            low_power: LowPowerSupport::at_start(Platform::CURRENT),
        }
    }
}

/// Everything `decide` looks at: the time, the work count, the manual deadline, when the
/// running hold began, when work was last seen, whether Auto stands down until the running
/// work ends, and the platform readings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    pub now_ms: i64,
    pub work: u32,
    pub manual_until_ms: Option<i64>,
    pub hold_since_ms: Option<i64>,
    pub work_last_ms: Option<i64>,
    pub stood_down: bool,
    pub sensors: Sensors,
}

/// What to hold: nothing, idle sleep, or idle sleep and the closed lid, each with or
/// without the display kept on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Want {
    Nothing,
    Idle { display: bool },
    IdleAndLid { display: bool },
}

/// Why Keep awake holds or does not: running work, a manual timer, the grace period after
/// work, nothing to hold for, or mode Off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Work,
    Timer,
    Grace,
    Idle,
    Off,
}

/// What ended a hold: the battery floor with the percent read, a battery that could not be
/// read, heat, the cap with the hours the hold ran under, the timer or the owner's End, mode
/// Off, or a hold the platform dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EndReason {
    Battery { pct: u8 },
    BatteryUnread,
    Heat,
    Cap { hours: u8 },
    Manual,
    Off,
    HoldLost,
}

/// One decision: what to hold, why, when the hold ends at the latest as far as known, and
/// what ended it when something did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub want: Want,
    pub reason: Reason,
    pub ends_at_ms: Option<i64>,
    pub ended: Option<EndReason>,
}

impl Decision {
    /// The decision of an engine that has not evaluated yet: hold nothing.
    pub const NONE: Self = Self {
        want: Want::Nothing,
        reason: Reason::Idle,
        ends_at_ms: None,
        ended: None,
    };

    fn released(reason: Reason, ended: Option<EndReason>) -> Self {
        Self {
            want: Want::Nothing,
            reason,
            ends_at_ms: None,
            ended,
        }
    }
}

/// Decides what Keep awake holds. Inputs: the settings and the inputs of this moment. It
/// reads no clock and no system state, so the same inputs always give the same decision.
/// Output: the decision; `ended` is set on the evaluation in which a floor, the cap, the
/// timer or mode Off ends or refuses a hold.
pub fn decide(settings: &KeepAwakeSettings, inputs: &Inputs) -> Decision {
    assert!(inputs.now_ms > 0);
    assert!(inputs.sensors.battery_pct.is_none_or(|pct| pct <= 100));
    let now = inputs.now_ms;
    if settings.mode == Mode::Off {
        let ended = inputs.hold_since_ms.map(|_| EndReason::Off);
        return Decision::released(Reason::Off, ended);
    }
    let timer = inputs.manual_until_ms.filter(|until| now < *until);
    let auto = settings.mode == Mode::Auto && !inputs.stood_down;
    let working = auto && inputs.work > 0;
    let grace = inputs
        .work_last_ms
        .map(|last| last + settings.grace.ms())
        .filter(|until| auto && !working && now < *until);
    let reason = match (working, timer, grace) {
        (true, _, _) => Reason::Work,
        (false, Some(_), _) => Reason::Timer,
        (false, None, Some(_)) => Reason::Grace,
        (false, None, None) => Reason::Idle,
    };
    if reason == Reason::Idle {
        let timed_out = inputs.hold_since_ms.is_some() && inputs.manual_until_ms.is_some();
        return Decision::released(reason, timed_out.then_some(EndReason::Manual));
    }
    let cap_end = settings
        .cap
        .ms()
        .map(|cap| inputs.hold_since_ms.unwrap_or(now) + cap);
    if let Some(ended) = floor(settings, inputs, cap_end) {
        return Decision::released(reason, Some(ended));
    }
    let natural = if working { None } else { timer.max(grace) };
    let ends_at_ms = match (natural, cap_end) {
        (Some(natural), Some(cap_end)) => Some(natural.min(cap_end)),
        (natural, cap_end) => natural.or(cap_end),
    };
    assert!(ends_at_ms.is_none_or(|end| end > now));
    Decision {
        want: want(settings, &inputs.sensors),
        reason,
        ends_at_ms,
        ended: None,
    }
}

/// Finds what ends a wanted hold now: the cap, then heat, then on battery the floor or a
/// battery unread twice. Inputs: the settings, the inputs and when the cap ends the hold.
/// Output: the end reason, or none while the hold may run.
fn floor(settings: &KeepAwakeSettings, inputs: &Inputs, cap_end: Option<i64>) -> Option<EndReason> {
    let sensors = &inputs.sensors;
    if cap_end.is_some_and(|end| inputs.now_ms >= end) {
        let hours = settings.cap.hours().expect("a cap end comes from a cap");
        assert!((1..=24).contains(&hours));
        return Some(EndReason::Cap { hours });
    }
    let too_hot = match settings.heat_end {
        HeatEnd::Critical => sensors.heat == Heat::Critical,
        HeatEnd::Serious => matches!(sensors.heat, Heat::Serious | Heat::Critical),
        HeatEnd::Never => false,
    };
    if too_hot {
        return Some(EndReason::Heat);
    }
    if sensors.power != PowerSource::Battery {
        return None;
    }
    let percent = settings.battery_floor.percent()?;
    assert!((1..=100).contains(&percent));
    match sensors.battery_pct {
        Some(pct) if pct <= percent => Some(EndReason::Battery { pct }),
        None if sensors.battery_unread_twice => Some(EndReason::BatteryUnread),
        _ => None,
    }
}

/// Picks the hold a running Keep awake takes: the lid too when `lid_status` says Held, and
/// the display while that setting is on and the lid is not closed. Inputs: the settings
/// and the readings. Output: an idle hold, with or without the lid.
fn want(settings: &KeepAwakeSettings, sensors: &Sensors) -> Want {
    let display =
        settings.display_awake == DisplayAwake::WhileHolding && sensors.lid_closed != Some(true);
    let want = if lid_status(settings, sensors) == LidSupport::Held {
        Want::IdleAndLid { display }
    } else {
        Want::Idle { display }
    };
    assert_ne!(want, Want::Nothing);
    want
}

/// Tells whether a hold covers the closed lid, or the plain reason it does not: the rule
/// is Off, the platform cannot, or the rule wants AC power and the computer is not on it
/// (an unknown power source counts as not on AC). Inputs: the settings and the readings.
/// Output: `Held`, or the reason.
pub fn lid_status(settings: &KeepAwakeSettings, sensors: &Sensors) -> LidSupport {
    if settings.lid == LidRule::Off {
        return LidSupport::Off;
    }
    if sensors.lid_support != LidSupport::Held {
        assert_ne!(sensors.lid_support, LidSupport::Off);
        assert_ne!(sensors.lid_support, LidSupport::IdleOnlyOnBattery);
        return sensors.lid_support.clone();
    }
    if settings.lid == LidRule::AcOnly && sensors.power != PowerSource::Ac {
        return LidSupport::IdleOnlyOnBattery;
    }
    LidSupport::Held
}

/// Tells whether a hold asks for the platform's low power mode (QUESTION 39e): only while a
/// hold runs on battery. Below 40 % needs a read charge under 40, and once asked under that
/// option it stays asked while on battery, so a charge read at 39, then 40, does not turn it
/// on and off. Inputs: the settings, the readings, whether a hold runs, and the option the
/// last ask of this hold was made under, none when it did not ask. Output: whether to ask.
pub fn low_power_wanted(
    settings: &KeepAwakeSettings,
    sensors: &Sensors,
    holding: bool,
    asked_under: Option<LowPower>,
) -> bool {
    assert!(sensors.battery_pct.is_none_or(|pct| pct <= 100));
    if !holding || sensors.power != PowerSource::Battery {
        return false;
    }
    let kept = asked_under == Some(settings.low_power);
    let wanted = match settings.low_power {
        LowPower::Off => false,
        LowPower::OnBattery => true,
        LowPower::BatteryBelow40 => {
            kept || sensors.battery_pct.is_some_and(|pct| pct < LowPower::BELOW_PCT)
        }
    };
    assert!(settings.low_power != LowPower::Off || !wanted);
    wanted
}

/// Tells whether the heat notice is due: the setting is on and the heat is serious or
/// critical. Inputs: the settings and the readings. Output: true when a notice is due.
pub fn heat_notice(settings: &KeepAwakeSettings, sensors: &Sensors) -> bool {
    settings.heat_notice == HeatNotice::Serious
        && matches!(sensors.heat, Heat::Serious | Heat::Critical)
}
