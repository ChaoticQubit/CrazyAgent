use super::extras::extras_of;
use super::poll::{PollCredential, Provider};
use super::{AccountLimits, LimitWindow, all_limits, failure, storage};
use crate::config::{ModelAccess, ModelConnection};
use crate::engine::EngineError;
use crate::pools::MemberKind;
use crate::protocol::CredentialRef;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod wire;

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, target_os = "macos"))]
mod option_tests;

/// The settled spend attempts kept, the latest first; open ones are always kept.
pub const ATTEMPTS_KEPT: i64 = 64;
/// How many times a spend that could not be sent yet, because its account was busy or not
/// signed in, is tried before it settles.
pub const TRIES_MAX: i64 = 3;
/// The most limited pools tracked and shown.
pub const LIMITED_POOLS_MAX: usize = 64;
/// The most auto-spends one pool makes in `AUTO_SPEND_EVERY_MS`.
pub const AUTO_SPENDS_PER_HOUR: i64 = 1;
const AUTO_SPEND_EVERY_MS: i64 = 60 * 60 * 1000;
const AUTO_SPENDS_KEPT_MS: i64 = 8 * 24 * 60 * 60 * 1000;
const QUEUED_PER_STEP: i64 = 8;
const ID_BYTES_MAX: usize = 128;
const CODE_BYTES_MAX: usize = super::FAILURE_BYTES_MAX;
const FULL_PERCENT: f64 = 100.0;
const NAME_BYTES_MAX: usize = 64;
/// The refusals that settle a first send, since none can have used a reset: a request that
/// never left, a 400, 404 or 409 (`usage_spend_refused`), a 401 or 403 and a 429.
const SETTLING_REFUSALS: [&str; 4] = [
    "usage_request_invalid",
    "usage_spend_refused",
    "usage_sign_in_lapsed",
    "usage_rate_limited",
];

const _: () = assert!(TRIES_MAX >= 1 && ATTEMPTS_KEPT >= 1);
const _: () = assert!(AUTO_SPENDS_PER_HOUR >= 1);

/// Adds the Claude free resets source to a spend table made before it, with the default and
/// the same check as `SCHEMA`.
pub(super) const ADD_SOURCE: &str = "ALTER TABLE usage_spend ADD COLUMN resets_source TEXT
    NOT NULL DEFAULT 'through_claude_code'
    CHECK(resets_source IN ('through_claude_code','as_claude_code','off'));";

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_spend (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    confirm TEXT NOT NULL CHECK(confirm IN ('confirm_each','one_click','off')),
    all_limited TEXT NOT NULL CHECK(all_limited IN ('notice','no_hint','auto_spend')),
    resets_source TEXT NOT NULL DEFAULT 'through_claude_code'
        CHECK(resets_source IN ('through_claude_code','as_claude_code','off'))
);
CREATE TABLE IF NOT EXISTS usage_spend_attempts (
    id INTEGER PRIMARY KEY,
    account TEXT NOT NULL,
    attempt_key TEXT NOT NULL UNIQUE CHECK(length(attempt_key)=36),
    origin TEXT NOT NULL CHECK(origin IN ('owner','auto')),
    state TEXT NOT NULL CHECK(state IN ('queued','sent','settled')),
    tries INTEGER NOT NULL DEFAULT 0 CHECK(tries>=0),
    outcome TEXT CHECK(outcome IS NULL OR length(outcome)<=64),
    requested_ms INTEGER NOT NULL,
    sent_ms INTEGER,
    settled_ms INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS usage_spend_attempts_open
    ON usage_spend_attempts(account) WHERE state IN ('queued','sent');
CREATE TABLE IF NOT EXISTS usage_auto_spends (
    account TEXT NOT NULL,
    window_reset_ms INTEGER NOT NULL,
    pool TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    PRIMARY KEY(account,window_reset_ms)
);
CREATE TABLE IF NOT EXISTS usage_limited_pools (
    pool TEXT PRIMARY KEY,
    name TEXT,
    since_ms INTEGER NOT NULL,
    with_resets TEXT NOT NULL
);";

/// How the app spends a free reset (DEC-154 item 5): after a confirm each time (the
/// default), at one click, or never (Off: no spend button, and the engine refuses).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendConfirm {
    #[default]
    ConfirmEach,
    OneClick,
    Off,
}

/// What the app does when every account in a pool is limited (DEC-154 item 6): a notice
/// naming the accounts with free resets (the default), nothing, or a spend on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllLimited {
    #[default]
    Notice,
    NoHint,
    AutoSpend,
}

/// Where the engine reads Claude free resets from (DEC-154 addendum 2026-11-13, owner):
/// through Claude Code (the default: its own report when its version has one, else the detail
/// call identifying as Claude Code), always as Claude Code, or not at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetsSource {
    #[default]
    ThroughClaudeCode,
    AsClaudeCode,
    Off,
}

/// The owner's Pools settings: the two spend settings and where Claude free resets are read
/// from, which an older client leaves out and so reads as the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpendSettings {
    pub confirm: SpendConfirm,
    pub all_limited: AllLimited,
    #[serde(default)]
    pub resets_source: ResetsSource,
}

impl SpendSettings {
    /// Whether the app may spend at all. Spend confirmation Off means it never does, so
    /// this is the one place that reading lives. Output: the answer.
    pub fn spends(self) -> bool {
        self.confirm != SpendConfirm::Off
    }

    /// Whether the engine spends on its own when a pool is limited: only when the owner
    /// chose auto-spend and spending is not off. Output: the answer.
    pub fn auto_spends(self) -> bool {
        self.spends() && self.all_limited == AllLimited::AutoSpend
    }

    /// Whether the clients show the limited pool notice. Output: the answer.
    pub fn notices(self) -> bool {
        self.all_limited == AllLimited::Notice
    }
}

/// Who asked for a spend: the owner, or the engine under auto-spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Owner,
    Auto,
}

/// Where a spend attempt stands: waiting to be sent, sent with no reply read yet, or
/// settled with its outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Queued,
    Sent,
    Settled,
}

/// What a sent spend's reply said, from a fixed set; reply text is never kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
    Unknown,
}

impl Outcome {
    /// The stored code of the outcome. Output: the code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::NothingToReset => "nothing_to_reset",
            Self::NoCredit => "no_credit",
            Self::AlreadyRedeemed => "already_redeemed",
            Self::Unknown => "outcome_unknown",
        }
    }
}

/// One account's latest spend attempt as the clients show it: never its key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpendAttempt {
    pub account: CredentialRef,
    pub origin: Origin,
    pub state: AttemptState,
    pub outcome: Option<String>,
    pub requested_ms: i64,
    pub settled_ms: Option<i64>,
}

/// A pool whose every account a poll read at its limit, with the accounts that hold a free
/// reset, for the notice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitedPool {
    pub pool: CredentialRef,
    pub name: Option<String>,
    pub since_ms: i64,
    pub with_resets: Vec<CredentialRef>,
}

/// What the clients read about spending: the settings, each account's latest attempt and,
/// under the notice setting, the limited pools.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpendView {
    pub settings: SpendSettings,
    pub attempts: Vec<SpendAttempt>,
    pub limited_pools: Vec<LimitedPool>,
}

/// A spend waiting to be sent, as the usage thread reads it: the key goes only into the
/// request's stdin config. `resend` is set when an earlier send of this attempt left, so
/// its reset may already be used and only a definite outcome may settle it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    pub id: i64,
    pub account: CredentialRef,
    pub key: String,
    pub tries: i64,
    pub resend: bool,
}

/// Reads the owner's spend settings, the defaults until one was made. Input: the store.
/// Output: the settings, or `usage_unavailable`.
pub fn read(db: &Connection) -> Result<SpendSettings, EngineError> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT confirm,all_limited,resets_source FROM usage_spend WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(storage)?;
    let (confirm, all_limited, source) = row.unwrap_or_default();
    Ok(SpendSettings {
        confirm: match confirm.as_str() {
            "one_click" => SpendConfirm::OneClick,
            "off" => SpendConfirm::Off,
            _ => SpendConfirm::ConfirmEach,
        },
        all_limited: match all_limited.as_str() {
            "no_hint" => AllLimited::NoHint,
            "auto_spend" => AllLimited::AutoSpend,
            _ => AllLimited::Notice,
        },
        resets_source: match source.as_str() {
            "as_claude_code" => ResetsSource::AsClaudeCode,
            "off" => ResetsSource::Off,
            _ => ResetsSource::ThroughClaudeCode,
        },
    })
}

/// Stores the owner's spend settings and Claude free resets source, all or none. Inputs:
/// the store and the settings. Output: done, or `usage_unavailable`.
pub fn write(db: &Connection, settings: SpendSettings) -> Result<(), EngineError> {
    let confirm = match settings.confirm {
        SpendConfirm::ConfirmEach => "confirm_each",
        SpendConfirm::OneClick => "one_click",
        SpendConfirm::Off => "off",
    };
    let all_limited = match settings.all_limited {
        AllLimited::Notice => "notice",
        AllLimited::NoHint => "no_hint",
        AllLimited::AutoSpend => "auto_spend",
    };
    let source = match settings.resets_source {
        ResetsSource::ThroughClaudeCode => "through_claude_code",
        ResetsSource::AsClaudeCode => "as_claude_code",
        ResetsSource::Off => "off",
    };
    db.execute(
        "INSERT INTO usage_spend (singleton,confirm,all_limited,resets_source) VALUES (1,?1,?2,?3)
         ON CONFLICT(singleton) DO UPDATE SET confirm=excluded.confirm,
             all_limited=excluded.all_limited,resets_source=excluded.resets_source",
        params![confirm, all_limited, source],
    )
    .map_err(storage)?;
    Ok(())
}

/// A fresh spend key: a version 4 UUID from the system's random source, 122 random bits.
/// Output: the key, or `usage_unavailable` when no random bytes came.
pub fn attempt_key() -> Result<String, EngineError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| failure("usage_unavailable"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let key = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    assert!(key_shaped(&key));
    Ok(key)
}

/// Whether text is a spend key: a lowercase UUID, 36 bytes. Input: the text. Output: the
/// answer.
pub fn key_shaped(key: &str) -> bool {
    key.len() == 36
        && key.bytes().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

/// Whether an id read from a reply or a credential may go into a URL path or a header:
/// letters, digits, `-` or `_`, 1 to 128 bytes. Input: the id. Output: the answer.
pub fn id_shaped(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ID_BYTES_MAX
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// The stored code of a failed spend: the engine's own code when it is one, so raw text
/// never reaches the store or a client. Input: the failure. Output: the code.
fn plain_code(problem: &EngineError) -> &str {
    let code = problem.0.as_str();
    let shaped = code.starts_with("usage_")
        && code.len() <= CODE_BYTES_MAX
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
    if shaped { code } else { "usage_spend_failed" }
}

/// Asks to spend one free reset on an account; the stored Spend confirmation decides (off
/// refuses, confirm each takes only a confirmed spend). One attempt per account is open:
/// asking again after a send whose reply never came sends it again with its same key.
/// Inputs: the store, the account, whether the owner confirmed it and the time. Output:
/// done, `usage_spend_off`, `usage_spend_unconfirmed`, or `usage_unavailable`.
pub fn request(
    db: &Connection,
    account: &CredentialRef,
    confirmed: bool,
    now_ms: i64,
) -> Result<(), EngineError> {
    let settings = read(db)?;
    if !settings.spends() {
        return Err(failure("usage_spend_off"));
    }
    if settings.confirm == SpendConfirm::ConfirmEach && !confirmed {
        return Err(failure("usage_spend_unconfirmed"));
    }
    let retried = db
        .execute(
            "UPDATE usage_spend_attempts SET state='queued',origin='owner'
             WHERE account=?1 AND state='sent'",
            params![account.as_str()],
        )
        .map_err(storage)?;
    assert!(retried <= 1);
    if retried == 0 {
        queue(db, account, Origin::Owner, now_ms)?;
    }
    Ok(())
}

/// Queues a new attempt with a fresh key unless the account has one open. Inputs: the
/// store, the account, who asked and the time. Output: whether one was queued.
fn queue(
    db: &Connection,
    account: &CredentialRef,
    origin: Origin,
    now_ms: i64,
) -> Result<bool, EngineError> {
    let origin = match origin {
        Origin::Owner => "owner",
        Origin::Auto => "auto",
    };
    let queued = db
        .execute(
            "INSERT INTO usage_spend_attempts (account,attempt_key,origin,state,requested_ms)
             VALUES (?1,?2,?3,'queued',?4)
             ON CONFLICT(account) WHERE state IN ('queued','sent') DO NOTHING",
            params![account.as_str(), attempt_key()?, origin, now_ms],
        )
        .map_err(storage)?;
    assert!(queued <= 1);
    Ok(queued == 1)
}

/// The attempts waiting to be sent, oldest first, a few per step. Input: the store.
/// Output: the attempts, or `usage_unavailable`.
pub fn queued(db: &Connection) -> Result<Vec<Queued>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT id,account,attempt_key,tries,sent_ms IS NOT NULL FROM usage_spend_attempts
             WHERE state='queued' ORDER BY id LIMIT ?1",
        )
        .map_err(storage)?;
    let rows: Vec<(i64, String, String, i64, bool)> = statement
        .query_map([QUEUED_PER_STEP], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    rows.into_iter()
        .map(|(id, account, key, tries, resend)| {
            let account = CredentialRef::new(account).map_err(storage)?;
            if !key_shaped(&key) {
                return Err(failure("usage_unavailable"));
            }
            Ok(Queued {
                id,
                account,
                key,
                tries,
                resend,
            })
        })
        .collect()
}

/// Marks an attempt sent before its request leaves, so a crash or a lost reply never sends
/// it again on its own. Inputs: the store, the attempt and the time. Output: done.
pub fn mark_sent(db: &Connection, id: i64, now_ms: i64) -> Result<(), EngineError> {
    let marked = db
        .execute(
            "UPDATE usage_spend_attempts SET state='sent',sent_ms=?2 WHERE id=?1 AND state='queued'",
            params![id, now_ms],
        )
        .map_err(storage)?;
    if marked != 1 {
        return Err(failure("usage_unavailable"));
    }
    Ok(())
}

fn settle(db: &Connection, id: i64, code: &str, now_ms: i64) -> Result<(), EngineError> {
    assert!(!code.is_empty() && code.len() <= CODE_BYTES_MAX);
    db.execute(
        "UPDATE usage_spend_attempts SET state='settled',outcome=?2,settled_ms=?3
         WHERE id=?1 AND state<>'settled'",
        params![id, code, now_ms],
    )
    .map_err(storage)?;
    db.execute(
        "DELETE FROM usage_spend_attempts WHERE state='settled' AND id NOT IN
         (SELECT id FROM usage_spend_attempts WHERE state='settled' ORDER BY id DESC LIMIT ?1)",
        params![ATTEMPTS_KEPT],
    )
    .map_err(storage)?;
    Ok(())
}

/// Records a sent attempt's result. Only a definite answer settles it: one of the four
/// outcome words, or on a first send a refusal that cannot have used a reset. Anything
/// else (no reply, a 5xx, an unreadable reply, an unknown 2xx, or any refusal of a resend)
/// leaves it sent with its key: the owner's next Spend reuses that key, and a later
/// successful poll settles it. Inputs: the store, the attempt, the result and the time.
/// Output: done.
pub fn sent(
    db: &Connection,
    attempt: &Queued,
    result: &Result<Outcome, EngineError>,
    now_ms: i64,
) -> Result<(), EngineError> {
    match result {
        Ok(Outcome::Unknown) => Ok(()),
        Ok(outcome) => settle(db, attempt.id, outcome.code(), now_ms),
        Err(problem) if !attempt.resend && SETTLING_REFUSALS.contains(&problem.0.as_str()) => {
            settle(db, attempt.id, &problem.0, now_ms)
        }
        Err(_) => Ok(()),
    }
}

/// Records an attempt that was not sent: with no problem its account was busy or not
/// signed in, and it waits for the next step until `TRIES_MAX` tries; a problem settles it
/// with that problem's code. A resend goes back to sent with its key instead, since its
/// earlier send may have used a reset. Inputs: the store, the attempt, the problem and the
/// time. Output: done.
pub fn not_sent(
    db: &Connection,
    attempt: &Queued,
    problem: Option<&EngineError>,
    now_ms: i64,
) -> Result<(), EngineError> {
    if attempt.resend {
        let back = db
            .execute(
                "UPDATE usage_spend_attempts SET state='sent' WHERE id=?1 AND state='queued'",
                params![attempt.id],
            )
            .map_err(storage)?;
        assert!(back <= 1);
        return Ok(());
    }
    if let Some(problem) = problem {
        return settle(db, attempt.id, plain_code(problem), now_ms);
    }
    let tries = attempt.tries + 1;
    if tries >= TRIES_MAX {
        return settle(db, attempt.id, "usage_spend_not_signed_in", now_ms);
    }
    db.execute(
        "UPDATE usage_spend_attempts SET tries=?2 WHERE id=?1 AND state='queued'",
        params![attempt.id, tries],
    )
    .map_err(storage)?;
    Ok(())
}

/// Settles each sent attempt with no definite answer once a successful poll of its account
/// newer than the send has run (a failed poll reads no limits): its outcome stays unknown
/// and the poll shows where the limits stand. Inputs: the store and the time. Output: done.
pub fn settle_sent(db: &Connection, now_ms: i64) -> Result<(), EngineError> {
    let ids: Vec<i64> = {
        let mut statement = db
            .prepare(
                "SELECT a.id FROM usage_spend_attempts a JOIN account_limits l ON l.account=a.account
                 WHERE a.state='sent' AND a.sent_ms IS NOT NULL AND l.checked_ms>a.sent_ms
                   AND l.failure IS NULL",
            )
            .map_err(storage)?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?
    };
    for id in ids {
        settle(db, id, Outcome::Unknown.code(), now_ms)?;
    }
    Ok(())
}

/// Why a queued spend cannot go to its account's connection, checked before any token is
/// read: gone from the settings, a Claude sign-in whose spend call has not landed yet
/// (DEC-154 addendum S9), or not a Claude or Codex sign-in. Input: the connection, none
/// when it was removed. Output: the code, none when the spend may be sent.
pub fn not_spendable(connection: Option<&ModelConnection>) -> Option<&'static str> {
    match connection.map(MemberKind::of) {
        None => Some("usage_spend_no_account"),
        Some(MemberKind::Chatgpt) => None,
        Some(MemberKind::Claude) => Some("usage_spend_unavailable"),
        Some(_) => Some("usage_spend_not_in_app"),
    }
}

/// Sends one queued spend with its stored key: marks it sent first, makes the provider's
/// call, records the result, and asks for a poll after every send so the limits catch up
/// and an unclear result settles soon.
/// With spending turned off since it was asked for, it settles `usage_spend_off` unsent. A
/// Claude spend waits for its call shape and settles `usage_spend_unavailable` unsent. A
/// resend that does not go out returns to sent with its key.
/// Inputs: the store, the attempt, the provider, the credential the custody lease read,
/// the Claude and Codex usage URLs, whether loopback URLs are allowed and the time.
/// Output: done, or the store's failure.
#[allow(clippy::too_many_arguments)]
pub fn send_one(
    db: &Connection,
    attempt: &Queued,
    provider: Provider,
    credential: &PollCredential,
    claude: &url::Url,
    codex: &url::Url,
    loopback: bool,
    now_ms: i64,
) -> Result<(), EngineError> {
    let _ = claude;
    if !read(db)?.spends() {
        return not_sent(db, attempt, Some(&failure("usage_spend_off")), now_ms);
    }
    if provider == Provider::Claude {
        return not_sent(
            db,
            attempt,
            Some(&failure("usage_spend_unavailable")),
            now_ms,
        );
    }
    let url = wire::codex_consume_url(codex)?;
    mark_sent(db, attempt.id, now_ms)?;
    let result = wire::codex_consume(url, credential, &attempt.key, loopback);
    sent(db, attempt, &result, now_ms)?;
    super::service::request_refresh(db, now_ms + 1)
}

/// The window key of an account a poll read at its limit: the latest reset time among its
/// polled windows at 100% now, None inside when a limited window has no reset time.
/// Inputs: the limits and the time. Output: None when the poll did not read it limited.
fn limited(limits: &AccountLimits, now_ms: i64) -> Option<Option<i64>> {
    let full: Vec<LimitWindow> = [
        limits.polled_five_hour_at(now_ms),
        limits.polled_weekly_at(now_ms),
    ]
    .into_iter()
    .flatten()
    .filter(|window| window.used_percent >= FULL_PERCENT)
    .collect();
    if full.is_empty() {
        return None;
    }
    Some(
        full.iter()
            .map(|window| window.resets_at_ms)
            .collect::<Option<Vec<i64>>>()
            .and_then(|times| times.into_iter().max()),
    )
}

/// When an account's latest spend attempt last changed, so a reading older than it is stale.
fn last_attempt_ms(db: &Connection, account: &CredentialRef) -> Result<Option<i64>, EngineError> {
    db.query_row(
        "SELECT MAX(MAX(requested_ms,COALESCE(sent_ms,0),COALESCE(settled_ms,0)))
         FROM usage_spend_attempts WHERE account=?1",
        params![account.as_str()],
        |row| row.get(0),
    )
    .map_err(storage)
}

/// Tracks the pools whose every account a poll read at its limit, for the notice, and
/// under auto-spend queues one spend in such a pool. An auto-spend goes to the first member
/// in pool order with a free reset, at most once per account per limit window (keyed by
/// that window's reset time), only when the account's poll and free-reset count are newer
/// than its latest attempt, never while any member has an attempt open, and at most
/// `AUTO_SPENDS_PER_HOUR` per pool per hour. Agent-reported limits never count.
/// Inputs: the store, the live access and the time. Output: done, or `usage_unavailable`.
pub fn track_pools(db: &Connection, access: &ModelAccess, now_ms: i64) -> Result<(), EngineError> {
    let settings = read(db)?;
    let limits: BTreeMap<String, AccountLimits> = all_limits(db)?
        .into_iter()
        .map(|row| (row.account.as_str().to_owned(), row))
        .collect();
    let in_app = |member: &CredentialRef| {
        access
            .connections
            .iter()
            .any(|connection| &connection.id == member && not_spendable(Some(connection)).is_none())
    };
    let mut current = Vec::new();
    for pool in access.pools.iter().take(LIMITED_POOLS_MAX) {
        let keys: Option<Vec<Option<i64>>> = pool
            .members
            .iter()
            .map(|member| limited(limits.get(member.as_str())?, now_ms))
            .collect();
        let Some(keys) = keys.filter(|keys| !keys.is_empty()) else {
            continue;
        };
        let mut with_resets = Vec::new();
        for (member, key) in pool.members.iter().zip(keys) {
            let extras = extras_of(db, member, now_ms)?;
            if extras
                .resets
                .as_ref()
                .is_some_and(|resets| resets.count > 0)
            {
                with_resets.push((member.clone(), key, extras.resets_checked_ms));
            }
        }
        current.push((pool, with_resets));
    }
    let kept: Vec<String> = current
        .iter()
        .map(|(pool, _)| pool.id.as_str().to_owned())
        .collect();
    let stored: Vec<String> = {
        let mut statement = db
            .prepare("SELECT pool FROM usage_limited_pools")
            .map_err(storage)?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?
    };
    for pool in stored.iter().filter(|pool| !kept.contains(pool)) {
        db.execute(
            "DELETE FROM usage_limited_pools WHERE pool=?1",
            params![pool],
        )
        .map_err(storage)?;
    }
    db.execute(
        "DELETE FROM usage_auto_spends WHERE at_ms<?1",
        params![now_ms - AUTO_SPENDS_KEPT_MS],
    )
    .map_err(storage)?;
    for (pool, with_resets) in &current {
        let accounts: Vec<&str> = with_resets
            .iter()
            .map(|(member, _, _)| member.as_str())
            .collect();
        db.execute(
            "INSERT INTO usage_limited_pools (pool,name,since_ms,with_resets) VALUES (?1,?2,?3,?4)
             ON CONFLICT(pool) DO UPDATE SET name=excluded.name,with_resets=excluded.with_resets",
            params![
                pool.id.as_str(),
                pool.name.as_deref().map(bounded_name),
                now_ms,
                serde_json::to_string(&accounts).map_err(storage)?
            ],
        )
        .map_err(storage)?;
        if settings.auto_spends() {
            auto_spend(db, pool, with_resets, &limits, &in_app, now_ms)?;
        }
    }
    Ok(())
}

/// A pool name as stored for the notice: at most `NAME_BYTES_MAX` bytes, cut at a
/// character boundary. Input: the name. Output: the bounded name.
fn bounded_name(name: &str) -> String {
    let mut end = name.len().min(NAME_BYTES_MAX);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

/// A pool member that holds a free reset: its id, its limit window key and when its free
/// reset count was read.
type Holder = (CredentialRef, Option<i64>, Option<i64>);

/// Queues one auto-spend in a limited pool when the rules in `track_pools` allow it.
fn auto_spend(
    db: &Connection,
    pool: &crate::config::AccountPool,
    with_resets: &[Holder],
    limits: &BTreeMap<String, AccountLimits>,
    in_app: &dyn Fn(&CredentialRef) -> bool,
    now_ms: i64,
) -> Result<(), EngineError> {
    let mut open = false;
    for member in &pool.members {
        open |= db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM usage_spend_attempts WHERE account=?1 AND state IN ('queued','sent'))",
                params![member.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage)?;
    }
    let lately: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM usage_auto_spends WHERE pool=?1 AND at_ms>?2",
            params![pool.id.as_str(), now_ms - AUTO_SPEND_EVERY_MS],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if open || lately >= AUTO_SPENDS_PER_HOUR {
        return Ok(());
    }
    for (member, key, counted_ms) in with_resets {
        let Some(window) = *key else { continue };
        let Some(polled_ms) = limits
            .get(member.as_str())
            .filter(|row| row.failure.is_none())
            .and_then(|row| row.checked_ms)
        else {
            continue;
        };
        let Some(counted_ms) = *counted_ms else {
            continue;
        };
        if !in_app(member) {
            continue;
        }
        let used: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM usage_auto_spends WHERE account=?1 AND window_reset_ms=?2)",
                params![member.as_str(), window],
                |row| row.get(0),
            )
            .map_err(storage)?;
        let fresh =
            last_attempt_ms(db, member)?.is_none_or(|last| polled_ms > last && counted_ms > last);
        if used || !fresh {
            continue;
        }
        if queue(db, member, Origin::Auto, now_ms)? {
            db.execute(
                "INSERT INTO usage_auto_spends (account,window_reset_ms,pool,at_ms) VALUES (?1,?2,?3,?4)",
                params![member.as_str(), window, pool.id.as_str(), now_ms],
            )
            .map_err(storage)?;
        }
        return Ok(());
    }
    Ok(())
}

/// What the clients read about spending: the settings, each account's latest attempt with
/// no key, and the limited pools when the notice is the setting. Input: the store.
/// Output: the view, or `usage_unavailable`.
pub fn view(db: &Connection) -> Result<SpendView, EngineError> {
    let settings = read(db)?;
    let mut statement = db
        .prepare(
            "SELECT account,origin,state,outcome,requested_ms,settled_ms FROM usage_spend_attempts a
             WHERE id=(SELECT MAX(id) FROM usage_spend_attempts b WHERE b.account=a.account)
             ORDER BY id DESC LIMIT ?1",
        )
        .map_err(storage)?;
    type Row = (String, String, String, Option<String>, i64, Option<i64>);
    let rows: Vec<Row> = statement
        .query_map([ATTEMPTS_KEPT], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    let mut attempts = Vec::with_capacity(rows.len());
    for (account, origin, state, outcome, requested_ms, settled_ms) in rows {
        attempts.push(SpendAttempt {
            account: CredentialRef::new(account).map_err(storage)?,
            origin: if origin == "auto" {
                Origin::Auto
            } else {
                Origin::Owner
            },
            state: match state.as_str() {
                "queued" => AttemptState::Queued,
                "sent" => AttemptState::Sent,
                _ => AttemptState::Settled,
            },
            outcome,
            requested_ms,
            settled_ms,
        });
    }
    let limited_pools = if settings.notices() {
        limited_pools(db)?
    } else {
        Vec::new()
    };
    Ok(SpendView {
        settings,
        attempts,
        limited_pools,
    })
}

fn limited_pools(db: &Connection) -> Result<Vec<LimitedPool>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT pool,name,since_ms,with_resets FROM usage_limited_pools ORDER BY pool LIMIT ?1",
        )
        .map_err(storage)?;
    let rows: Vec<(String, Option<String>, i64, String)> = statement
        .query_map([LIMITED_POOLS_MAX as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    rows.into_iter()
        .map(|(pool, name, since_ms, with_resets)| {
            let accounts: Vec<String> = serde_json::from_str(&with_resets).map_err(storage)?;
            Ok(LimitedPool {
                pool: CredentialRef::new(pool).map_err(storage)?,
                name,
                since_ms,
                with_resets: accounts
                    .into_iter()
                    .map(|account| CredentialRef::new(account).map_err(storage))
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect()
}
