use super::poll::rfc3339_ms;
use super::{RESET_MS_MAX, checked_text, storage};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use rusqlite::{Connection, OptionalExtension, params};
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
/// The largest amount of money or credits a reply is taken to state.
const AMOUNT_MAX: f64 = 1e12;
/// The longest decimal text a reply may state an amount in.
const DECIMAL_BYTES_MAX: usize = 16;
/// The most balance lines one DeepSeek reply is read for.
const BALANCES_MAX: usize = 2;

const _: () = assert!(DETAIL_EVERY_MS > super::service::CONFIRM_EVERY_MS);
const _: () = assert!(DETAIL_LEASE_MS > 60 * 1000 && DETAIL_LEASE_MS <= DETAIL_EVERY_MS);
const _: () = assert!(RESETS_MAX == 1000 && CREDITS_BYTES_MAX == 64 && KINDS_MAX == 4);
const _: () = assert!(RESET_MS_MAX == 32_503_680_000_000);

/// The extras table, one row per account, and the Pools detail lease. Its bounds repeat
/// `RESETS_MAX`, `RESET_MS_MAX` and `CREDITS_BYTES_MAX`, asserted above.
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS account_limit_extras (
    account TEXT PRIMARY KEY NOT NULL,
    resets_count INTEGER CHECK (resets_count IS NULL OR resets_count BETWEEN 0 AND 1000),
    reset_kinds TEXT CHECK (reset_kinds IS NULL OR length(reset_kinds) <= 64),
    resets_expire_ms INTEGER CHECK (resets_expire_ms IS NULL OR resets_expire_ms BETWEEN 1 AND 32503680000000),
    resets_checked_ms INTEGER,
    resets_gained_ms INTEGER,
    credits TEXT CHECK (credits IS NULL OR length(credits) BETWEEN 1 AND 64),
    credits_checked_ms INTEGER
);
CREATE TABLE IF NOT EXISTS usage_detail (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    until_ms INTEGER NOT NULL
);";

/// One saved-key provider whose documented balance call the engine makes: the exact API
/// host a base URL must name, its adapter, the constant balance URL and where the owner
/// tops up. Nothing here comes from the account's own URL text.
struct KeyHost {
    host: &'static str,
    adapter: Adapter,
    balance: &'static str,
    place: &'static str,
}

/// The fixed set of key hosts (SP-M1): OpenRouter, DeepSeek, and Moonshot for Kimi on its
/// international and China hosts.
static KEY_HOSTS: [KeyHost; 4] = [
    KeyHost {
        host: "openrouter.ai",
        adapter: Adapter::OpenRouter,
        balance: "https://openrouter.ai/api/v1/credits",
        place: "openrouter.ai",
    },
    KeyHost {
        host: "api.deepseek.com",
        adapter: Adapter::DeepSeek,
        balance: "https://api.deepseek.com/user/balance",
        place: "platform.deepseek.com",
    },
    KeyHost {
        host: "api.moonshot.ai",
        adapter: Adapter::Moonshot,
        balance: "https://api.moonshot.ai/v1/users/me/balance",
        place: "platform.moonshot.ai",
    },
    KeyHost {
        host: "api.moonshot.cn",
        adapter: Adapter::Moonshot,
        balance: "https://api.moonshot.cn/v1/users/me/balance",
        place: "platform.moonshot.cn",
    },
];

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
    let kinds = Vec::<ResetKind>::deserialize(reader)?;
    if !kinds_bounded(&kinds) {
        return Err(serde::de::Error::custom(
            "reset kinds repeat or pass the bound",
        ));
    }
    Ok(kinds)
}

fn kinds_bounded(kinds: &[ResetKind]) -> bool {
    kinds.len() <= KINDS_MAX
        && kinds
            .iter()
            .enumerate()
            .all(|(at, kind)| !kinds[..at].contains(kind))
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
/// or a count whose next expiry has passed; with when each was last read, when a read last
/// found more free resets than the unexpired count before it, and whether the last free
/// resets read found no count (unread, never true for an expired count).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredExtras {
    pub resets: Option<Resets>,
    pub credits: Option<String>,
    pub resets_checked_ms: Option<i64>,
    pub credits_checked_ms: Option<i64>,
    pub resets_gained_ms: Option<i64>,
    #[serde(default)]
    pub resets_unread: bool,
}

/// The limits contract every client shows for one account (DEC-154): the harness's name, its
/// adapter, whether its read or spend is a private call that may break, the free resets and
/// credits (none means not reported, never 0), where to spend, the not reported words, when
/// each field was last read and new resets arrived, and whether the last resets read found no
/// count, which clients say as "couldn't read", never as none.
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
    #[serde(default)]
    pub resets_unread: bool,
}

/// The adapter entry of a harness for its managed sign-ins, by catalog id or the name a
/// model connection stores ("claude" for Claude Code). Input: the harness. Output: the
/// adapter, or none for a harness outside the catalog.
pub fn adapter(harness: &str) -> Option<Adapter> {
    let id = crate::harnesses::spec(harness)?.id;
    ADAPTERS
        .iter()
        .find(|(entry, _)| *entry == id)
        .map(|(_, adapter)| *adapter)
}

/// The key host a base URL names: https, no user name or password, no port but 443, and a
/// host exactly equal to one in `KEY_HOSTS`. Input: the base URL. Output: the key host.
fn key_host(base_url: &str) -> Option<&'static KeyHost> {
    let url = url::Url::parse(base_url).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    KEY_HOSTS.iter().find(|key| key.host == host)
}

/// The documented balance call of a saved key's provider, chosen by the account's own base
/// URL: OpenRouter, DeepSeek or Moonshot (Kimi) over https. Input: the base URL. Output: the
/// adapter, or none for any other host or scheme.
pub fn key_adapter(base_url: &str) -> Option<Adapter> {
    key_host(base_url).map(|key| key.adapter)
}

fn key_of(connection: &ModelConnection) -> Option<&'static KeyHost> {
    match (&connection.auth, &connection.endpoint) {
        (ModelAuthentication::ApiKey { .. }, ModelEndpoint::Http { url }) => key_host(url),
        _ => None,
    }
}

/// The adapter that reads one account: its harness's entry for a managed sign-in, its
/// provider's balance call for a saved key on a known host, else not reported. Input: the
/// connection. Output: the adapter.
pub fn account_adapter(connection: &ModelConnection) -> Adapter {
    match &connection.auth {
        ModelAuthentication::NativeManaged { .. } => {
            adapter(connection.harness.as_str()).unwrap_or(Adapter::NotReported)
        }
        _ => key_of(connection).map_or(Adapter::NotReported, |key| key.adapter),
    }
}

/// The words for a field an account's harness does not report. Input: the harness id or
/// connection name. Output: "not reported by <harness name>", the id itself for a harness
/// outside the catalog.
pub fn not_reported(harness: &str) -> String {
    let name = crate::harnesses::spec(harness).map_or(harness, |spec| spec.name);
    format!("not reported by {name}")
}

fn words(text: String) -> Field<String> {
    if checked_text(Some(text.as_str()), CREDITS_BYTES_MAX).is_ok() {
        Field::Known(text)
    } else {
        Field::Absent
    }
}

fn reset_count(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|count| u32::try_from(count).ok())
        .filter(|count| *count <= RESETS_MAX)
}

fn amount(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|amount| amount.is_finite() && (0.0..=AMOUNT_MAX).contains(amount))
}

fn decimal(value: &Value) -> Option<&str> {
    let text = value.as_str()?;
    let digits = text.split('.').collect::<Vec<_>>();
    let shaped = text.len() <= DECIMAL_BYTES_MAX
        && (1..=2).contains(&digits.len())
        && digits
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    shaped.then_some(text)
}

fn currency(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|code| code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase()))
}

fn codex_resets(count: u32, next_expiry_ms: Option<i64>) -> Resets {
    assert!(count <= RESETS_MAX);
    Resets {
        count,
        kinds: if count > 0 {
            vec![ResetKind::AllLimits]
        } else {
            Vec::new()
        },
        next_expiry_ms,
    }
}

fn codex_credits(credits: &Value) -> Field<String> {
    let Some(has_credits) = credits["has_credits"].as_bool() else {
        return Field::Absent;
    };
    let unlimited = match &credits["unlimited"] {
        Value::Null => false,
        Value::Bool(unlimited) => *unlimited,
        _ => return Field::Absent,
    };
    if unlimited {
        return words("Unlimited credits".to_owned());
    }
    if !has_credits {
        return words("No credits".to_owned());
    }
    let balance = &credits["balance"];
    match (decimal(balance), amount(balance)) {
        (Some(text), _) => words(format!("{text} credits")),
        (None, Some(number)) => words(format!("{number} credits")),
        (None, None) => Field::Absent,
    }
}

/// Reads free resets and credits from Codex's usage reply, the same reply the poll reads the
/// windows from. A missing or malformed field is absent; nothing here fails the poll. Input:
/// the parsed body. Output: the extras.
pub fn codex_from_usage(body: &Value) -> Extras {
    let resets = match reset_count(&body["rate_limit_reset_credits"]["available_count"]) {
        Some(count) => Field::Known(codex_resets(count, None)),
        None => Field::Absent,
    };
    Extras {
        resets,
        credits: codex_credits(&body["credits"]),
    }
}

fn codex_next_expiry(credits: &Value, now_ms: i64) -> Option<Option<i64>> {
    let credits = match credits {
        Value::Null => return Some(None),
        Value::Array(credits) if credits.len() <= RESETS_MAX as usize => credits,
        _ => return None,
    };
    let mut next: Option<i64> = None;
    for credit in credits {
        let expires = match &credit["expiresAt"] {
            Value::Null => None,
            value => Some(
                value
                    .as_i64()
                    .and_then(|seconds| seconds.checked_mul(1000))
                    .filter(|at| (1..=RESET_MS_MAX).contains(at))?,
            ),
        };
        if credit["status"].as_str() != Some("available") {
            continue;
        }
        if let Some(at) = expires.filter(|at| *at > now_ms) {
            next = Some(next.map_or(at, |soonest| soonest.min(at)));
        }
    }
    Some(next)
}

/// Reads free resets from the Codex app server's `account/rateLimits/read` result: the count,
/// the kinds, and the earliest expiry of an available reset after `now_ms`. Reset ids are
/// never read. Inputs: the result and the time. Output: the resets, or absent.
pub fn codex_reset_credits(result: &Value, now_ms: i64) -> Field<Resets> {
    let block = &result["rateLimitResetCredits"];
    let Some(count) = reset_count(&block["availableCount"]) else {
        return Field::Absent;
    };
    match codex_next_expiry(&block["credits"], now_ms) {
        Some(next) => Field::Known(codex_resets(count, next)),
        None => Field::Absent,
    }
}

/// Reads free resets from Claude's private detail reply: the resets left summed over grants
/// that are not paused and have not ended, and the earliest end of those after `now_ms`; an
/// account that is not eligible holds none. Grant ids are never read. Inputs: the parsed body
/// and the time. Output: the resets, or absent.
pub fn claude_resets(body: &Value, now_ms: i64) -> Field<Resets> {
    let detail = &body["cedar_ember"];
    if !detail.is_object() {
        return Field::Absent;
    }
    let Some(eligible) = detail["eligible"].as_bool() else {
        return Field::Absent;
    };
    if !eligible {
        return Field::Known(Resets {
            count: 0,
            kinds: Vec::new(),
            next_expiry_ms: None,
        });
    }
    let Value::Array(grants) = &detail["grants"] else {
        return Field::Absent;
    };
    if grants.len() > RESETS_MAX as usize {
        return Field::Absent;
    }
    let mut total: u32 = 0;
    let mut next: Option<i64> = None;
    for grant in grants {
        let Some(left) = reset_count(&grant["resets_left"]) else {
            return Field::Absent;
        };
        let paused = match &grant["paused"] {
            Value::Null => false,
            Value::Bool(paused) => *paused,
            _ => return Field::Absent,
        };
        let ends = match &grant["ends_at"] {
            Value::Null => None,
            Value::String(text) => match rfc3339_ms(text) {
                Some(at) => Some(at),
                None => return Field::Absent,
            },
            _ => return Field::Absent,
        };
        if paused || left == 0 || ends.is_some_and(|at| at <= now_ms) {
            continue;
        }
        total += left;
        if total > RESETS_MAX {
            return Field::Absent;
        }
        if let Some(at) = ends {
            next = Some(next.map_or(at, |soonest| soonest.min(at)));
        }
    }
    Field::Known(Resets {
        count: total,
        kinds: Vec::new(),
        next_expiry_ms: next,
    })
}

/// Reads Claude's extra usage from its usage reply as a sentence: off, on, or the amount used
/// of the monthly cap in its currency. Input: the parsed body. Output: the words, or absent.
pub fn claude_credits(body: &Value) -> Field<String> {
    let extra = &body["extra_usage"];
    let Some(enabled) = extra["is_enabled"].as_bool() else {
        return Field::Absent;
    };
    if !enabled {
        return words("Extra usage off".to_owned());
    }
    let (limit, used) = (&extra["monthly_limit"], &extra["used_credits"]);
    if limit.is_null() && used.is_null() {
        return words("Extra usage on".to_owned());
    }
    let (Some(limit), Some(used)) = (amount(limit), amount(used)) else {
        return Field::Absent;
    };
    match &extra["currency"] {
        Value::Null => words(format!("{used:.2} of {limit:.2} extra usage used")),
        code => match currency(code) {
            Some(code) => words(format!("{used:.2} of {limit:.2} {code} extra usage used")),
            None => Field::Absent,
        },
    }
}

/// The documented balance call of a key adapter on the account's own base URL host: the
/// constant URL of that host in `KEY_HOSTS`, never joined onto the base URL. Inputs: the
/// adapter and the base URL. Output: the URL, or none for an adapter without one or a base
/// URL off its provider's hosts.
pub fn balance_url(adapter: Adapter, base_url: &str) -> Option<url::Url> {
    let key = key_host(base_url).filter(|key| key.adapter == adapter)?;
    url::Url::parse(key.balance).ok()
}

fn deepseek_balance(body: &Value) -> Option<String> {
    let Value::Array(lines) = &body["balance_infos"] else {
        return None;
    };
    if lines.is_empty() || lines.len() > BALANCES_MAX {
        return None;
    }
    let mut parts = Vec::with_capacity(lines.len());
    for line in lines {
        let total = decimal(&line["total_balance"])?;
        let code = currency(&line["currency"])?;
        parts.push(format!("{total} {code}"));
    }
    Some(format!("{} left", parts.join(" and ")))
}

/// Reads a key provider's documented balance reply as a sentence in its own currency.
/// Inputs: the adapter and the parsed body. Output: the words, or absent.
pub fn key_balance(adapter: Adapter, body: &Value) -> Field<String> {
    let text = match adapter {
        Adapter::OpenRouter => {
            let data = &body["data"];
            amount(&data["total_credits"])
                .zip(amount(&data["total_usage"]))
                .map(|(total, used)| format!("{:.2} USD left", total - used))
        }
        Adapter::DeepSeek => deepseek_balance(body),
        Adapter::Moonshot if body["code"].as_i64() == Some(0) => {
            amount(&body["data"]["available_balance"]).map(|left| format!("{left:.2} available"))
        }
        Adapter::Moonshot | Adapter::Codex | Adapter::Claude | Adapter::NotReported => None,
    };
    text.map_or(Field::Absent, words)
}

fn kind_name(kind: ResetKind) -> &'static str {
    match kind {
        ResetKind::FiveHour => "five_hour",
        ResetKind::Weekly => "weekly",
        ResetKind::Model => "model",
        ResetKind::AllLimits => "all_limits",
    }
}

fn kinds_text(kinds: &[ResetKind]) -> String {
    kinds
        .iter()
        .map(|kind| kind_name(*kind))
        .collect::<Vec<_>>()
        .join(",")
}

fn kinds_of(text: Option<&str>) -> Vec<ResetKind> {
    let all = [
        ResetKind::FiveHour,
        ResetKind::Weekly,
        ResetKind::Model,
        ResetKind::AllLimits,
    ];
    let mut kinds: Vec<ResetKind> = text
        .unwrap_or_default()
        .split(',')
        .filter_map(|name| all.into_iter().find(|kind| kind_name(*kind) == name))
        .collect();
    kinds.dedup();
    kinds.truncate(KINDS_MAX);
    kinds
}

/// One stored extras row as written, before the expiry rule.
#[derive(Default)]
struct Row {
    count: Option<u32>,
    kinds: Option<String>,
    expire_ms: Option<i64>,
    resets_checked_ms: Option<i64>,
    resets_gained_ms: Option<i64>,
    credits: Option<String>,
    credits_checked_ms: Option<i64>,
}

impl Row {
    fn unexpired(&self, now_ms: i64) -> Option<u32> {
        self.count
            .filter(|_| self.expire_ms.is_none_or(|at| at > now_ms))
    }

    fn clear_resets(&mut self, now_ms: i64) {
        self.count = None;
        self.kinds = None;
        self.expire_ms = None;
        self.resets_checked_ms = Some(now_ms);
    }
}

fn read_row(db: &Connection, account: &CredentialRef) -> Result<Option<Row>, EngineError> {
    db.query_row(
        "SELECT resets_count,reset_kinds,resets_expire_ms,resets_checked_ms,resets_gained_ms,
                credits,credits_checked_ms
         FROM account_limit_extras WHERE account=?1",
        params![account.as_str()],
        |found| {
            Ok(Row {
                count: found.get(0)?,
                kinds: found.get(1)?,
                expire_ms: found.get(2)?,
                resets_checked_ms: found.get(3)?,
                resets_gained_ms: found.get(4)?,
                credits: found.get(5)?,
                credits_checked_ms: found.get(6)?,
            })
        },
    )
    .optional()
    .map_err(storage)
}

fn resets_bounded(resets: &Resets) -> bool {
    resets.count <= RESETS_MAX
        && kinds_bounded(&resets.kinds)
        && resets
            .next_expiry_ms
            .is_none_or(|at| (1..=RESET_MS_MAX).contains(&at))
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
    let before = read_row(db, account)?.unwrap_or_default();
    let previous = before.unexpired(now_ms);
    if extras.resets == Field::NotAsked && extras.credits == Field::NotAsked {
        return Ok(previous);
    }
    let mut next = before;
    match &extras.resets {
        Field::NotAsked => {}
        Field::Known(resets) if resets_bounded(resets) => {
            if previous.is_some_and(|count| resets.count > count) {
                next.resets_gained_ms = Some(now_ms);
            }
            next.count = Some(resets.count);
            next.kinds = Some(kinds_text(&resets.kinds));
            next.expire_ms = resets.next_expiry_ms;
            next.resets_checked_ms = Some(now_ms);
        }
        Field::Known(_) | Field::Absent => next.clear_resets(now_ms),
    }
    match &extras.credits {
        Field::NotAsked => {}
        Field::Known(text) if checked_text(Some(text.as_str()), CREDITS_BYTES_MAX).is_ok() => {
            next.credits = Some(text.clone());
            next.credits_checked_ms = Some(now_ms);
        }
        Field::Known(_) | Field::Absent => {
            next.credits = None;
            next.credits_checked_ms = Some(now_ms);
        }
    }
    db.execute(
        "INSERT INTO account_limit_extras (account,resets_count,reset_kinds,resets_expire_ms,
             resets_checked_ms,resets_gained_ms,credits,credits_checked_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(account) DO UPDATE SET resets_count=excluded.resets_count,
             reset_kinds=excluded.reset_kinds,resets_expire_ms=excluded.resets_expire_ms,
             resets_checked_ms=excluded.resets_checked_ms,
             resets_gained_ms=excluded.resets_gained_ms,credits=excluded.credits,
             credits_checked_ms=excluded.credits_checked_ms",
        params![
            account.as_str(),
            next.count,
            next.kinds,
            next.expire_ms,
            next.resets_checked_ms,
            next.resets_gained_ms,
            next.credits,
            next.credits_checked_ms
        ],
    )
    .map_err(storage)?;
    Ok(previous)
}

/// An account's stored extras at `now_ms`, a count whose next expiry has passed read as not
/// reported. Inputs: the store, the account and the time. Output: the extras, empty for an
/// account never read, or `usage_unavailable`.
pub fn extras_of(
    db: &Connection,
    account: &CredentialRef,
    now_ms: i64,
) -> Result<StoredExtras, EngineError> {
    let Some(row) = read_row(db, account)? else {
        return Ok(StoredExtras::default());
    };
    let resets = row.unexpired(now_ms).map(|count| Resets {
        count,
        kinds: kinds_of(row.kinds.as_deref()),
        next_expiry_ms: row.expire_ms,
    });
    Ok(StoredExtras {
        resets,
        credits: row.credits,
        resets_checked_ms: row.resets_checked_ms,
        credits_checked_ms: row.credits_checked_ms,
        resets_gained_ms: row.resets_gained_ms,
        resets_unread: false,
    })
}

/// The limits contract for one account from its connection and stored extras. A field the
/// adapter does not read is none even when an old value is stored; where to spend and every
/// label come from constants, never from the account's URL or a reply. Inputs: the
/// connection and the stored extras. Output: the contract.
pub fn limit_extras(connection: &ModelConnection, stored: &StoredExtras) -> LimitExtras {
    let harness = connection.harness.as_str();
    let adapter = account_adapter(connection);
    let resets = matches!(adapter, Adapter::Codex | Adapter::Claude);
    let credits = adapter != Adapter::NotReported;
    let spend = match adapter {
        Adapter::Codex | Adapter::Claude => Spend::InApp,
        Adapter::OpenRouter | Adapter::DeepSeek | Adapter::Moonshot => {
            key_of(connection).map_or(Spend::NotReported, |key| Spend::Elsewhere {
                place: key.place.to_owned(),
            })
        }
        Adapter::NotReported => Spend::NotReported,
    };
    LimitExtras {
        harness: crate::harnesses::spec(harness)
            .map_or(harness, |spec| spec.name)
            .to_owned(),
        adapter,
        may_break: resets,
        resets: stored.resets.clone().filter(|_| resets),
        credits: stored.credits.clone().filter(|_| credits),
        spend,
        not_reported: not_reported(harness),
        resets_checked_ms: stored.resets_checked_ms.filter(|_| resets),
        credits_checked_ms: stored.credits_checked_ms.filter(|_| credits),
        resets_gained_ms: stored.resets_gained_ms.filter(|_| resets),
        resets_unread: false,
    }
}

/// Keeps the private detail reads on for `DETAIL_LEASE_MS` from `now_ms`, as an open Pools
/// page asks on each reread; an older ask never shortens the lease. Inputs: the store and the
/// time. Output: done, or `usage_unavailable`.
pub fn request_detail(db: &Connection, now_ms: i64) -> Result<(), EngineError> {
    assert!(now_ms >= 0);
    db.execute(
        "INSERT INTO usage_detail (singleton,until_ms) VALUES (1,?1)
         ON CONFLICT(singleton) DO UPDATE SET until_ms=MAX(until_ms,excluded.until_ms)",
        params![now_ms.saturating_add(DETAIL_LEASE_MS)],
    )
    .map_err(storage)?;
    Ok(())
}

/// Tells whether the next poll makes the private detail reads: a Pools lease runs at `now_ms`
/// and the last detail read of this engine run, if any, is `DETAIL_EVERY_MS` old or stamped
/// after `now_ms` by a clock that went back. Inputs: the store, the last detail read and the
/// time. Output: the answer, or `usage_unavailable`.
pub fn detail_due(
    db: &Connection,
    detail_ms: Option<i64>,
    now_ms: i64,
) -> Result<bool, EngineError> {
    let until: Option<i64> = db
        .query_row(
            "SELECT until_ms FROM usage_detail WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    let leased = until.is_some_and(|until| until > now_ms);
    let rested =
        detail_ms.is_none_or(|at| at > now_ms || now_ms.saturating_sub(at) >= DETAIL_EVERY_MS);
    Ok(leased && rested)
}

#[cfg(test)]
mod tests;
