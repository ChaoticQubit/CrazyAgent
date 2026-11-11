use super::{metrics::money_totals, prices, storage};
use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_money (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    main_number TEXT NOT NULL CHECK(main_number IN ('dollars','tokens')),
    prices TEXT NOT NULL CHECK(prices IN ('bundled','off'))
);
CREATE TABLE IF NOT EXISTS usage_project_limits (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    choice TEXT NOT NULL CHECK(choice IN ('hidden','shown'))
);";

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

/// Whether the app Metrics page with one project picked still shows every account's limits,
/// usage notices and next accounts, which belong to accounts, not projects. Hidden by
/// default (DEC-129 item 1, DEC-132).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectLimits {
    #[default]
    Hidden,
    Shown,
}

/// The owner's choices for dollars and for the Metrics page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoneySettings {
    pub main_number: MainNumber,
    pub prices: PriceSource,
    #[serde(default)]
    pub project_limits: ProjectLimits,
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

/// Reads the owner's choices, the defaults until one was made; a store made before the
/// account limits choice reads it as Hidden. Input: the store. Output: the settings, or
/// `usage_unavailable`.
pub fn read(db: &Connection) -> Result<MoneySettings, EngineError> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT main_number,prices FROM usage_money WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    let limits: Option<String> = db
        .query_row(
            "SELECT choice FROM usage_project_limits WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    let (main, source) = row.unwrap_or_default();
    Ok(MoneySettings {
        main_number: if main == "tokens" {
            MainNumber::Tokens
        } else {
            MainNumber::Dollars
        },
        prices: if source == "off" {
            PriceSource::Off
        } else {
            PriceSource::Bundled
        },
        project_limits: if limits.as_deref() == Some("shown") {
            ProjectLimits::Shown
        } else {
            ProjectLimits::Hidden
        },
    })
}

/// Stores the owner's choices, all of them or none: a choice that cannot be stored leaves
/// the others as they were. Inputs: the store and the settings. Output: done, or
/// `usage_unavailable`.
pub fn write(db: &Connection, settings: MoneySettings) -> Result<(), EngineError> {
    db.execute_batch("SAVEPOINT usage_money_write")
        .map_err(storage)?;
    let stored = store(db, settings);
    let closed = match stored {
        Ok(()) => db.execute_batch("RELEASE usage_money_write"),
        Err(_) => db.execute_batch("ROLLBACK TO usage_money_write; RELEASE usage_money_write"),
    };
    closed.map_err(storage)?;
    stored
}

/// Writes the two rows the owner's choices live in, inside the caller's savepoint. Inputs:
/// the store and the settings. Output: done, or `usage_unavailable`.
fn store(db: &Connection, settings: MoneySettings) -> Result<(), EngineError> {
    let main = match settings.main_number {
        MainNumber::Dollars => "dollars",
        MainNumber::Tokens => "tokens",
    };
    let source = match settings.prices {
        PriceSource::Bundled => "bundled",
        PriceSource::Off => "off",
    };
    db.execute(
        "INSERT INTO usage_money (singleton,main_number,prices) VALUES (1,?1,?2)
         ON CONFLICT(singleton) DO UPDATE SET main_number=excluded.main_number,prices=excluded.prices",
        params![main, source],
    )
    .map_err(storage)?;
    let limits = match settings.project_limits {
        ProjectLimits::Hidden => "hidden",
        ProjectLimits::Shown => "shown",
    };
    db.execute(
        "INSERT INTO usage_project_limits (singleton,choice) VALUES (1,?1)
         ON CONFLICT(singleton) DO UPDATE SET choice=excluded.choice",
        params![limits],
    )
    .map_err(storage)?;
    Ok(())
}

/// Reads what the charts and tables show. With prices Off no dollar leaves the engine.
/// Inputs: the store, the project and the range start. Output: the view.
pub(super) fn view(
    db: &Connection,
    project: Option<&str>,
    since_ms: i64,
) -> Result<MoneyView, EngineError> {
    let settings = read(db)?;
    let (cost, unpriced) = money_totals(db, project, since_ms)?;
    Ok(match settings.prices {
        PriceSource::Off => MoneyView {
            settings,
            shown: MainNumber::Tokens,
            as_of: None,
            cost_nanos: 0,
            unpriced_tokens: 0,
        },
        PriceSource::Bundled => MoneyView {
            settings,
            shown: settings.main_number,
            as_of: Some(prices::table().as_of().to_owned()),
            cost_nanos: cost,
            unpriced_tokens: unpriced,
        },
    })
}
