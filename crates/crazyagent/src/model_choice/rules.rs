use super::unavailable;
use crate::engine::EngineError;
use crate::storage::versioned::{self, Item};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS model_rules (
    rule TEXT PRIMARY KEY CHECK (rule IN ('pick', 'fallback')),
    value TEXT NOT NULL,
    updated_ms INTEGER NOT NULL
) WITHOUT ROWID;";
const ITEM_PREFIX: &str = "model-choice:rule";

/// How the engine treats the model the master or a lead picks for a new agent: only
/// from the owner's allowed list or the default, any model the harness offers with the
/// list as a hint, or never, so every agent runs the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickRule {
    #[default]
    Strict,
    Hint,
    Default,
}

/// What a start does when no pool account can run the model chosen for it: run on an
/// account's own model and tell the owner and the master once, start nothing and give
/// the reason, or run on an account's own model and tell no one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackRule {
    #[default]
    Notice,
    Stop,
    Silent,
}

/// One rule the owner sets, with its new value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", content = "value", rename_all = "snake_case")]
pub enum Rule {
    Pick(PickRule),
    Fallback(FallbackRule),
}

impl Rule {
    fn name(self) -> &'static str {
        match self {
            Rule::Pick(_) => "pick",
            Rule::Fallback(_) => "fallback",
        }
    }

    fn value(self) -> &'static str {
        match self {
            Rule::Pick(PickRule::Strict) => "strict",
            Rule::Pick(PickRule::Hint) => "hint",
            Rule::Pick(PickRule::Default) => "default",
            Rule::Fallback(FallbackRule::Notice) => "notice",
            Rule::Fallback(FallbackRule::Stop) => "stop",
            Rule::Fallback(FallbackRule::Silent) => "silent",
        }
    }
}

/// The owner's two app-wide model rules with the version a set names for each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub pick: PickRule,
    pub pick_version: u64,
    pub fallback: FallbackRule,
    pub fallback_version: u64,
}

/// What setting a rule did: saved as the next version, or refused as stale with the
/// current rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleWritten {
    Saved { rules: Rules },
    Stale { rules: Rules },
}

/// Reads the owner's model rules, each at the owner's grill pick until set: picks only
/// from the allowed list, and a fallback told once. Input: the engine database, inside
/// or outside a transaction. Output: the rules, or `model_choice_unavailable`.
pub fn rules(db: &Connection) -> Result<Rules, EngineError> {
    let pick = match stored(db, "pick")?.as_deref() {
        None | Some("strict") => PickRule::Strict,
        Some("hint") => PickRule::Hint,
        Some("default") => PickRule::Default,
        Some(_) => return Err(unavailable(())),
    };
    let fallback = match stored(db, "fallback")?.as_deref() {
        None | Some("notice") => FallbackRule::Notice,
        Some("stop") => FallbackRule::Stop,
        Some("silent") => FallbackRule::Silent,
        Some(_) => return Err(unavailable(())),
    };
    Ok(Rules {
        pick,
        pick_version: versioned::current(db, &item("pick")?)?.version,
        fallback,
        fallback_version: versioned::current(db, &item("fallback")?)?.version,
    })
}

/// Sets one model rule when the version named is current. Inputs: the engine database,
/// the rule with its value and the version the owner saw. Output: the saved or stale
/// rules, or `model_choice_unavailable`.
pub fn set_rule(db: &Connection, rule: Rule, version: u64) -> Result<RuleWritten, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    let next = match versioned::owner_write(&tx, &item(rule.name())?, version)? {
        Ok(next) => next,
        Err(_) => return Ok(RuleWritten::Stale { rules: rules(&tx)? }),
    };
    tx.execute_batch(SCHEMA).map_err(unavailable)?;
    let changed = tx
        .execute(
            "INSERT INTO model_rules(rule, value, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(rule) DO UPDATE SET value = excluded.value, updated_ms = excluded.updated_ms",
            params![rule.name(), rule.value(), crate::team::now_ms()],
        )
        .map_err(unavailable)?;
    assert_eq!(changed, 1);
    let saved = rules(&tx)?;
    match rule {
        Rule::Pick(pick) => assert!(saved.pick == pick && saved.pick_version == next),
        Rule::Fallback(fallback) => {
            assert!(saved.fallback == fallback && saved.fallback_version == next)
        }
    }
    tx.commit().map_err(unavailable)?;
    Ok(RuleWritten::Saved { rules: saved })
}

fn item(name: &str) -> Result<Item, EngineError> {
    Item::new(&format!("{ITEM_PREFIX}:{name}"))
}

fn stored(db: &Connection, name: &str) -> Result<Option<String>, EngineError> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'model_rules')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !exists {
        return Ok(None);
    }
    db.query_row(
        "SELECT value FROM model_rules WHERE rule = ?1",
        [name],
        |row| row.get(0),
    )
    .optional()
    .map_err(unavailable)
}
