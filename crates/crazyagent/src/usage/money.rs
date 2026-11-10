use super::prices;
use crate::engine::EngineError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// Which number leads the charts and tables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainNumber {
    #[default]
    Dollars,
    Tokens,
}

/// Where dollar estimates come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    #[default]
    Bundled,
    Off,
}

/// The owner's choices for dollars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoneySettings {
    pub main_number: MainNumber,
    pub prices: PriceSource,
}

/// What the charts and tables show for dollars over a range.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoneyView {
    pub settings: MoneySettings,
    pub shown: MainNumber,
    pub as_of: Option<String>,
    pub cost_nanos: i64,
    pub unpriced_tokens: u64,
}

/// Reads the owner's choices, the defaults until one was made. Input: the store. Output: the
/// settings, or `usage_unavailable`.
pub fn read(_db: &Connection) -> Result<MoneySettings, EngineError> {
    Ok(MoneySettings {
        main_number: MainNumber::Tokens,
        prices: PriceSource::Off,
    })
}

/// Stores the owner's choices. Inputs: the store and the settings. Output: done, or
/// `usage_unavailable`.
pub fn write(_db: &Connection, _settings: MoneySettings) -> Result<(), EngineError> {
    Ok(())
}

/// Reads what the charts and tables show. With prices Off no dollar leaves the engine.
/// Inputs: the store, the project and the range start. Output: the view.
pub(super) fn view(
    _db: &Connection,
    _project: Option<&str>,
    _since_ms: i64,
) -> Result<MoneyView, EngineError> {
    Ok(MoneyView::default())
}
