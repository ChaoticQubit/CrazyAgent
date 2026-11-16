use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const MINUTE_MS: i64 = 60 * 1000;
const HOUR_MS: i64 = 60 * MINUTE_MS;

/// When Keep awake holds: while work runs (Auto), only for a timer the owner starts
/// (Manual), or never (Off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Auto,
    Manual,
    Off,
}

impl Mode {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 3] = [Self::Auto, Self::Manual, Self::Off];
}

/// How long Auto keeps holding after the last work ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grace {
    Min0,
    Min5,
    Min10,
    Min30,
}

impl Grace {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 4] = [Self::Min0, Self::Min5, Self::Min10, Self::Min30];

    /// The grace period in milliseconds. Input: the choice. Output: zero to 30 minutes.
    pub fn ms(self) -> i64 {
        let minutes = match self {
            Self::Min0 => 0,
            Self::Min5 => 5,
            Self::Min10 => 10,
            Self::Min30 => 30,
        };
        assert!((0..=30).contains(&minutes));
        minutes * MINUTE_MS
    }
}

/// The longest one hold may last before it ends by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    H4,
    H8,
    H24,
    None,
}

impl Cap {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 4] = [Self::H4, Self::H8, Self::H24, Self::None];

    /// The cap in whole hours. Input: the choice. Output: 4, 8 or 24, or none for no limit.
    pub fn hours(self) -> Option<u8> {
        let hours = match self {
            Self::H4 => Some(4),
            Self::H8 => Some(8),
            Self::H24 => Some(24),
            Self::None => None,
        };
        assert!(hours.is_none_or(|hours| (1..=24).contains(&hours)));
        assert_eq!(hours.is_none(), self == Self::None);
        hours
    }

    /// The cap in milliseconds. Input: the choice. Output: the time, or none for no limit.
    pub fn ms(self) -> Option<i64> {
        let ms = self.hours().map(|hours| i64::from(hours) * HOUR_MS);
        assert!(ms.is_none_or(|ms| ms >= HOUR_MS));
        ms
    }
}

/// When a hold also covers a closed lid: on AC power only, always, or never.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LidRule {
    AcOnly,
    Always,
    Off,
}

impl LidRule {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 3] = [Self::AcOnly, Self::Always, Self::Off];
}

/// The battery level at or below which a hold on battery ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryFloor {
    Off,
    Pct5,
    Pct10,
    Pct20,
}

impl BatteryFloor {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 4] = [Self::Off, Self::Pct5, Self::Pct10, Self::Pct20];

    /// The floor in percent. Input: the choice. Output: the percent, or none for Off.
    pub fn percent(self) -> Option<u8> {
        let percent = match self {
            Self::Off => None,
            Self::Pct5 => Some(5),
            Self::Pct10 => Some(10),
            Self::Pct20 => Some(20),
        };
        assert!(percent.is_none_or(|percent| (1..=100).contains(&percent)));
        assert_eq!(percent.is_none(), self == Self::Off);
        percent
    }
}

/// The heat level that ends a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeatEnd {
    Critical,
    Serious,
    Never,
}

impl HeatEnd {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 3] = [Self::Critical, Self::Serious, Self::Never];
}

/// Whether serious heat during a hold raises a notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeatNotice {
    Serious,
    Off,
}

impl HeatNotice {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 2] = [Self::Serious, Self::Off];
}

/// What the display does while the lid is closed during a hold (macOS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LidDisplay {
    Sleep,
    LeaveOn,
}

impl LidDisplay {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 2] = [Self::Sleep, Self::LeaveOn];
}

/// Whether a hold also keeps the display on while the lid is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayAwake {
    Off,
    WhileHolding,
}

impl DisplayAwake {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 2] = [Self::Off, Self::WhileHolding];
}

/// When a hold turns on the platform's low power mode (DEC-167 addendum 2 item 4, QUESTION
/// 39e): on battery below `BELOW_PCT`, whenever on battery, or never.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LowPower {
    #[default]
    BatteryBelow40,
    OnBattery,
    Off,
}

impl LowPower {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 3] = [Self::BatteryBelow40, Self::OnBattery, Self::Off];

    /// The charge, in percent, that `BatteryBelow40` asks below.
    pub const BELOW_PCT: u8 = 40;
}

/// Whether the owner's chosen apps are frozen while the lid is closed during a hold
/// (DEC-167 Task 17, owner question 4: Off by default, with an empty list).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freeze {
    #[default]
    Off,
    On,
}

impl Freeze {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 2] = [Self::Off, Self::On];
}

/// Whether the sound is muted while the lid is closed during a hold (DEC-167 Task 17,
/// owner question 4: Off by default).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mute {
    #[default]
    Off,
    On,
}

impl Mute {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 2] = [Self::Off, Self::On];
}

/// The owner's Keep awake choices for this computer (DEC-167). Before the first save they
/// are `DEFAULT`: Auto, 10 minutes of grace, a 24 hour cap, lid hold on AC power only, end
/// at 10 % battery and at critical heat, a notice at serious heat, low power mode on battery
/// below 40 %, no app frozen and the sound left as it is. A row saved before freeze and
/// mute existed reads them as Off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepAwakeSettings {
    pub mode: Mode,
    pub grace: Grace,
    pub cap: Cap,
    pub lid: LidRule,
    pub battery_floor: BatteryFloor,
    pub heat_end: HeatEnd,
    pub heat_notice: HeatNotice,
    pub lid_display: LidDisplay,
    pub display_awake: DisplayAwake,
    #[serde(default)]
    pub low_power: LowPower,
    #[serde(default)]
    pub freeze: Freeze,
    #[serde(default)]
    pub mute: Mute,
}

impl KeepAwakeSettings {
    /// The settings of a computer whose owner never saved any.
    pub const DEFAULT: Self = Self {
        mode: Mode::Auto,
        grace: Grace::Min10,
        cap: Cap::H24,
        lid: LidRule::AcOnly,
        battery_floor: BatteryFloor::Pct10,
        heat_end: HeatEnd::Critical,
        heat_notice: HeatNotice::Serious,
        lid_display: LidDisplay::Sleep,
        display_awake: DisplayAwake::Off,
        low_power: LowPower::BatteryBelow40,
        freeze: Freeze::Off,
        mute: Mute::Off,
    };
}

impl Default for KeepAwakeSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

const TABLE: &str = "CREATE TABLE IF NOT EXISTS configured_keep_awake (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    settings TEXT NOT NULL
)";

const SETTINGS_BYTES_MAX: usize = 1024;

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn invalid() -> EngineError {
    error("keep_awake_settings_invalid")
}

/// Creates the settings table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
pub(super) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// Saves the owner's Keep awake settings over the stored ones. Inputs: the open
/// transaction and the settings. Output: Ok, or the database error.
pub(super) fn save(
    connection: &Connection,
    settings: &KeepAwakeSettings,
) -> Result<(), EngineError> {
    let text = serde_json::to_string(settings).map_err(error)?;
    assert!(!text.is_empty());
    assert!(text.len() <= SETTINGS_BYTES_MAX);
    let stored = connection
        .execute(
            "INSERT INTO configured_keep_awake(singleton,settings) VALUES(1,?1) \
             ON CONFLICT(singleton) DO UPDATE SET settings=excluded.settings",
            rusqlite::params![text],
        )
        .map_err(error)?;
    assert_eq!(stored, 1);
    Ok(())
}

/// Loads the owner's Keep awake settings. Input: the engine database, writable or
/// read-only. Output: the stored settings, the defaults before the first save, or
/// `keep_awake_settings_invalid` for a stored value this engine does not know, so the
/// engine holds on no guess.
pub fn load(connection: &Connection) -> Result<KeepAwakeSettings, EngineError> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(settings AS BLOB))<=?1 THEN settings END \
             FROM configured_keep_awake WHERE singleton=1",
            [SETTINGS_BYTES_MAX as i64],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(error)?
        .map(|text| text.ok_or_else(invalid))
        .transpose()?;
    let Some(text) = stored else {
        return Ok(KeepAwakeSettings::DEFAULT);
    };
    serde_json::from_str(&text).map_err(|_| invalid())
}
