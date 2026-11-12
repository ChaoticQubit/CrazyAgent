use crate::config::ModelConnection;
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most free resets an account is taken to hold; a larger count is a malformed reply.
pub const RESETS_MAX: u32 = 1000;
/// The longest credits sentence the engine composes and stores.
pub const CREDITS_BYTES_MAX: usize = 64;
/// The most reset kinds one account's resets name; there are only four kinds.
pub const KINDS_MAX: usize = 4;
/// The query that asks Claude's usage call for the free resets the Claude CLI reads; a
/// private call that may break (DEC-154).
pub const CLAUDE_DETAIL_QUERY: &str = "cedar_ember=1&skip_spend=1";
/// How long one ask from an open Pools page keeps the private detail reads on; each reread
/// of the page renews it.
pub const DETAIL_LEASE_MS: i64 = 3 * 60 * 1000;
/// The least time between two private detail reads of the same engine run, however often
/// the Pools page rereads.
pub const DETAIL_EVERY_MS: i64 = 5 * 60 * 1000;

/// How an account's free resets, credits and spend are read beyond the 5-hour and weekly
/// windows (DEC-154): a managed Codex or Claude sign-in by its harness (Claude through the
/// private query its CLI uses, which may break), a saved API key by its provider's documented
/// balance call (OpenRouter, DeepSeek, Moonshot for Kimi), every other account not reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adapter {
    Codex,
    Claude,
    OpenRouter,
    DeepSeek,
    Moonshot,
    NotReported,
}

/// One limits adapter entry for every harness in `harnesses::specs()`, by catalog id, for its
/// managed sign-ins. Kept explicit, with no catch-all, so a harness added to the catalog fails
/// the row that compares the two lists until it gets its own entry.
pub const ADAPTERS: [(&str, Adapter); 23] = [
    ("codex", Adapter::Codex),
    ("claude-code", Adapter::Claude),
    ("opencode", Adapter::NotReported),
    ("gemini-cli", Adapter::NotReported),
    ("cursor-agent", Adapter::NotReported),
    ("fx", Adapter::NotReported),
    ("antigravity", Adapter::NotReported),
    ("openclaw", Adapter::NotReported),
    ("pi", Adapter::NotReported),
    ("rcli", Adapter::NotReported),
    ("amp", Adapter::NotReported),
    ("goose", Adapter::NotReported),
    ("aider", Adapter::NotReported),
    ("qwen-code", Adapter::NotReported),
    ("crush", Adapter::NotReported),
    ("droid", Adapter::NotReported),
    ("kiro", Adapter::NotReported),
    ("copilot", Adapter::NotReported),
    ("cline", Adapter::NotReported),
    ("continue", Adapter::NotReported),
    ("openhands", Adapter::NotReported),
    ("auggie", Adapter::NotReported),
    ("kimi", Adapter::NotReported),
];

/// The kind of limit one free reset clears, when the provider says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetKind {
    FiveHour,
    Weekly,
    Model,
    AllLimits,
}

/// An account's free resets: how many, the kinds the provider named (empty when it names
/// none) and when the next one expires, if it says. Never an id of a reset or grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resets {
    pub count: u32,
    #[serde(deserialize_with = "bounded_kinds")]
    pub kinds: Vec<ResetKind>,
    pub next_expiry_ms: Option<i64>,
}

/// Reads the reset kinds of the wire, refusing a repeated kind or more than `KINDS_MAX`.
/// Input: the deserializer. Output: the kinds, or the deserializer's error.
fn bounded_kinds<'de, D: serde::Deserializer<'de>>(reader: D) -> Result<Vec<ResetKind>, D::Error> {
    Vec::<ResetKind>::deserialize(reader)
}

/// One field of a read: not asked for (the stored value stays), asked and absent or
/// malformed (cleared, so it reads not reported), or known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field<T> {
    NotAsked,
    Absent,
    Known(T),
}

/// What one read found beyond the windows: free resets and credits, the credits as a
/// sentence the engine composed from parsed fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extras {
    pub resets: Field<Resets>,
    pub credits: Field<String>,
}

impl Extras {
    /// A read that asked for nothing beyond the windows. Output: both fields not asked.
    pub fn not_asked() -> Self {
        Self {
            resets: Field::NotAsked,
            credits: Field::NotAsked,
        }
    }
}

/// Where the owner spends a free reset or credit: in the app, at a named place (the
/// provider's own host for a key's balance), or not reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum Spend {
    InApp,
    Elsewhere { place: String },
    NotReported,
}

/// An account's stored extras as they stand now: none for a field never read, read absent,
/// or a count whose next expiry has passed; with when each was last read, and when a read
/// last found more free resets than the unexpired count before it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredExtras {
    pub resets: Option<Resets>,
    pub credits: Option<String>,
    pub resets_checked_ms: Option<i64>,
    pub credits_checked_ms: Option<i64>,
    pub resets_gained_ms: Option<i64>,
}

/// The limits contract every client shows for one account (DEC-154): the harness's display
/// name, the account's adapter, whether its read or spend is a private call that may break,
/// the free resets and credits (none means not reported, never 0), where to spend, the words
/// for a field that is not reported ("not reported by <harness>"), when each field was last
/// read, and when new free resets last arrived, for a notice shown once per time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitExtras {
    pub harness: String,
    pub adapter: Adapter,
    pub may_break: bool,
    pub resets: Option<Resets>,
    pub credits: Option<String>,
    pub spend: Spend,
    pub not_reported: String,
    pub resets_checked_ms: Option<i64>,
    pub credits_checked_ms: Option<i64>,
    pub resets_gained_ms: Option<i64>,
}

/// The adapter entry of a harness for its managed sign-ins, by catalog id or the name a
/// model connection stores ("claude" for Claude Code). Input: the harness. Output: the
/// adapter, or none for a harness outside the catalog.
pub fn adapter(harness: &str) -> Option<Adapter> {
    let _ = harness;
    None
}

/// The documented balance call of a saved key's provider, chosen by the account's own base
/// URL: OpenRouter, DeepSeek or Moonshot (Kimi) over https. Input: the base URL. Output: the
/// adapter, or none for any other host or scheme.
pub fn key_adapter(base_url: &str) -> Option<Adapter> {
    let _ = base_url;
    None
}

/// The adapter that reads one account: its harness's entry for a managed sign-in, its
/// provider's balance call for a saved key on a known host, else not reported. Input: the
/// connection. Output: the adapter.
pub fn account_adapter(connection: &ModelConnection) -> Adapter {
    let _ = connection;
    Adapter::NotReported
}

/// The words for a field an account's harness does not report. Input: the harness id or
/// connection name. Output: "not reported by <harness name>", the id itself for a harness
/// outside the catalog.
pub fn not_reported(harness: &str) -> String {
    let _ = harness;
    String::new()
}

/// Reads free resets and credits from Codex's usage reply, the same reply the poll reads the
/// windows from. A missing or malformed field is absent; nothing here fails the poll. Input:
/// the parsed body. Output: the extras.
pub fn codex_from_usage(body: &Value) -> Extras {
    let _ = body;
    Extras::not_asked()
}

/// Reads free resets from the Codex app server's `account/rateLimits/read` result: the count,
/// the kinds, and the earliest expiry of an available reset after `now_ms`. Reset ids are
/// never read. Inputs: the result and the time. Output: the resets, or absent.
pub fn codex_reset_credits(result: &Value, now_ms: i64) -> Field<Resets> {
    let _ = (result, now_ms);
    Field::NotAsked
}

/// Reads free resets from Claude's private detail reply: the resets left summed over grants
/// that are not paused, and the earliest end of those after `now_ms`; an account that is not
/// eligible holds none. Grant ids are never read. Inputs: the parsed body and the time.
/// Output: the resets, or absent.
pub fn claude_resets(body: &Value, now_ms: i64) -> Field<Resets> {
    let _ = (body, now_ms);
    Field::NotAsked
}

/// Reads Claude's extra usage from its usage reply as a sentence: off, or the amount used of
/// the monthly cap in its currency. Input: the parsed body. Output: the words, or absent.
pub fn claude_credits(body: &Value) -> Field<String> {
    let _ = body;
    Field::NotAsked
}

/// The documented balance call of a key adapter on the account's own base URL host. Inputs:
/// the adapter and the base URL. Output: the URL, or none for an adapter without one or a
/// base URL off its provider's host.
pub fn balance_url(adapter: Adapter, base_url: &str) -> Option<url::Url> {
    let _ = (adapter, base_url);
    None
}

/// Reads a key provider's documented balance reply as a sentence in its own currency.
/// Inputs: the adapter and the parsed body. Output: the words, or absent.
pub fn key_balance(adapter: Adapter, body: &Value) -> Field<String> {
    let _ = (adapter, body);
    Field::NotAsked
}

/// Stores what a read found beyond the windows: a field not asked keeps the stored value, an
/// absent or out-of-bounds one is cleared, a known one replaced; only counts, kinds, expiry
/// and the composed words are kept. A count above the unexpired one stored before stamps
/// `resets_gained_ms`. Inputs: the store, the account, the extras and the time. Output: the
/// unexpired count stored before, or `usage_unavailable`.
pub fn record_extras(
    db: &Connection,
    account: &CredentialRef,
    extras: &Extras,
    now_ms: i64,
) -> Result<Option<u32>, EngineError> {
    let _ = (db, account, extras, now_ms);
    Ok(None)
}

/// An account's stored extras at `now_ms`, a count whose next expiry has passed read as not
/// reported. Inputs: the store, the account and the time. Output: the extras, empty for an
/// account never read, or `usage_unavailable`.
pub fn extras_of(
    db: &Connection,
    account: &CredentialRef,
    now_ms: i64,
) -> Result<StoredExtras, EngineError> {
    let _ = (db, account, now_ms);
    Ok(StoredExtras::default())
}

/// The limits contract for one account from its connection and stored extras. Inputs: the
/// connection and the stored extras. Output: the contract.
pub fn limit_extras(connection: &ModelConnection, stored: &StoredExtras) -> LimitExtras {
    let _ = stored;
    LimitExtras {
        harness: connection.harness.as_str().to_owned(),
        adapter: Adapter::NotReported,
        may_break: false,
        resets: None,
        credits: None,
        spend: Spend::NotReported,
        not_reported: String::new(),
        resets_checked_ms: None,
        credits_checked_ms: None,
        resets_gained_ms: None,
    }
}

/// Keeps the private detail reads on for `DETAIL_LEASE_MS` from `now_ms`, as an open Pools
/// page asks on each reread. Inputs: the store and the time. Output: done, or
/// `usage_unavailable`.
pub fn request_detail(db: &Connection, now_ms: i64) -> Result<(), EngineError> {
    let _ = (db, now_ms);
    Ok(())
}

/// Tells whether the next poll makes the private detail reads: a Pools lease runs at `now_ms`
/// and the last detail read of this engine run, if any, is `DETAIL_EVERY_MS` old. Inputs: the
/// store, the last detail read and the time. Output: the answer, or `usage_unavailable`.
pub fn detail_due(
    db: &Connection,
    detail_ms: Option<i64>,
    now_ms: i64,
) -> Result<bool, EngineError> {
    let _ = (db, detail_ms, now_ms);
    Ok(false)
}

#[cfg(test)]
mod tests;
