use super::{LimitWindow, Observation, PLAN_BYTES_MAX, RESET_MS_MAX, checked_text, failure};
use crate::credentials::SecretString;
use crate::engine::EngineError;
use crate::github::http::Request;
use serde_json::Value;

pub const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CLAUDE_BETA: &str = "oauth-2025-04-20";
const FIVE_HOUR_SECONDS: u64 = 5 * 60 * 60;
const WEEK_SECONDS: u64 = 7 * 24 * 60 * 60;
const ACCOUNT_ID_BYTES_MAX: usize = 128;

/// Whose usage call a poll makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
}

/// What a poll sends for one account, read from the engine's credential custody for this
/// call only: the OAuth access token, the ChatGPT account id Codex sends with it, and the
/// plan the credential record names. The token is never logged or stored.
pub struct PollCredential {
    pub token: SecretString,
    pub account_id: Option<String>,
    pub plan: Option<String>,
}

/// Reads an RFC 3339 time such as `2026-10-27T08:00:00.123456+00:00` or `...Z`.
/// Input: the text. Output: milliseconds since the epoch, or None for any other shape.
pub fn rfc3339_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes.len() > 40 || !text.is_ascii() {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    if bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 60 {
        return None;
    }
    let mut rest = &text[19..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let shown = &fraction[..digits.min(3)];
        millis = shown.parse::<i64>().ok()? * 10_i64.pow(3 - shown.len() as u32);
        rest = &fraction[digits..];
    }
    let offset_minutes = match rest {
        "Z" | "z" => 0,
        zone if zone.len() == 6 && zone.as_bytes()[3] == b':' => {
            let sign = match zone.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hours: i64 = zone[1..3].parse().ok()?;
            let minutes: i64 = zone[4..6].parse().ok()?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 60 + minutes)
        }
        _ => return None,
    };
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60;
    let at = seconds.checked_mul(1000)?.checked_add(millis)?;
    (1..=RESET_MS_MAX).contains(&at).then_some(at)
}

fn share(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|used| used.is_finite())
        .map(|used| used.clamp(0.0, 100.0))
}

fn claude_window(value: &Value) -> Result<Option<LimitWindow>, EngineError> {
    if value.is_null() {
        return Ok(None);
    }
    let used = share(&value["utilization"]).ok_or_else(invalid)?;
    let resets_at_ms = match &value["resets_at"] {
        Value::Null => None,
        Value::String(text) => Some(rfc3339_ms(text).ok_or_else(invalid)?),
        Value::Number(seconds) => Some(
            seconds
                .as_i64()
                .and_then(|seconds| seconds.checked_mul(1000))
                .filter(|at| (1..=RESET_MS_MAX).contains(at))
                .ok_or_else(invalid)?,
        ),
        _ => return Err(invalid()),
    };
    Ok(Some(LimitWindow {
        used_percent: used,
        resets_at_ms,
    }))
}

fn invalid() -> EngineError {
    failure("usage_reply_invalid")
}

fn plan(text: Option<&str>) -> Option<String> {
    text.map(|plan| plan.trim().to_ascii_lowercase())
        .filter(|plan| checked_text(Some(plan), PLAN_BYTES_MAX).is_ok())
}

/// Reads the reply of Claude's usage call: `five_hour` and `seven_day`, each with the share
/// used as `utilization` (0 to 100) and `resets_at`. Input: the body.
/// Output: the windows, or `usage_reply_invalid`.
pub fn parse_claude(body: &[u8]) -> Result<Observation, EngineError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    let observation = Observation {
        plan: None,
        five_hour: claude_window(&value["five_hour"])?,
        weekly: claude_window(&value["seven_day"])?,
    };
    if observation.five_hour.is_none() && observation.weekly.is_none() {
        return Err(invalid());
    }
    Ok(observation)
}

fn codex_window(
    value: &Value,
    now_ms: i64,
) -> Result<Option<(Option<u64>, LimitWindow)>, EngineError> {
    if value.is_null() {
        return Ok(None);
    }
    let used = share(&value["used_percent"]).ok_or_else(invalid)?;
    let resets_at_ms = match (
        value["reset_at"].as_i64(),
        value["reset_after_seconds"].as_i64(),
    ) {
        (Some(at), _) => Some(at.checked_mul(1000).ok_or_else(invalid)?),
        (None, Some(after)) => Some(
            after
                .checked_mul(1000)
                .and_then(|ms| now_ms.checked_add(ms))
                .ok_or_else(invalid)?,
        ),
        (None, None) => None,
    };
    if resets_at_ms.is_some_and(|at| !(1..=RESET_MS_MAX).contains(&at)) {
        return Err(invalid());
    }
    Ok(Some((
        value["limit_window_seconds"].as_u64(),
        LimitWindow {
            used_percent: used,
            resets_at_ms,
        },
    )))
}

/// Reads the reply of Codex's usage call: `plan_type` and `rate_limit` with a primary and
/// a secondary window, each with `used_percent`, `limit_window_seconds` and `reset_at` in
/// seconds since the epoch. A window is placed by its length; without one, primary is the
/// 5-hour window and secondary the weekly one. Inputs: the body and the time.
/// Output: the plan and windows, or `usage_reply_invalid`.
pub fn parse_codex(body: &[u8], now_ms: i64) -> Result<Observation, EngineError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| invalid())?;
    let limits = &value["rate_limit"];
    if !limits.is_object() {
        return Err(invalid());
    }
    let mut observation = Observation {
        plan: plan(value["plan_type"].as_str()),
        five_hour: None,
        weekly: None,
    };
    for (key, fallback) in [
        ("primary_window", FIVE_HOUR_SECONDS),
        ("secondary_window", WEEK_SECONDS),
    ] {
        match codex_window(&limits[key], now_ms)? {
            Some((Some(FIVE_HOUR_SECONDS), window)) => observation.five_hour = Some(window),
            Some((Some(WEEK_SECONDS), window)) => observation.weekly = Some(window),
            Some((None, window)) if fallback == FIVE_HOUR_SECONDS => {
                observation.five_hour = Some(window);
            }
            Some((None, window)) => observation.weekly = Some(window),
            Some((Some(_), _)) | None => {}
        }
    }
    if observation.five_hour.is_none() && observation.weekly.is_none() {
        return Err(invalid());
    }
    Ok(observation)
}

/// Reads one account's limits from its provider's usage call at `url`, through the system
/// curl, so the token travels only on curl's stdin. A plain-http URL is taken only for a
/// loopback test server when `loopback` is set. Inputs: the provider, the URL, the
/// credential, whether loopback is allowed and the time. Output: the observation with the
/// credential's plan when the reply names none; `usage_sign_in_lapsed` for 401 or 403,
/// `usage_rate_limited` for 429, `usage_poll_failed` for another status,
/// `usage_unreachable` when no reply came, or `usage_reply_invalid`.
pub fn poll(
    provider: Provider,
    url: url::Url,
    credential: &PollCredential,
    loopback: bool,
    now_ms: i64,
) -> Result<Observation, EngineError> {
    let account_id = credential.account_id.as_deref().filter(|id| {
        !id.is_empty()
            && id.len() <= ACCOUNT_ID_BYTES_MAX
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    });
    let mut headers = Vec::new();
    match provider {
        Provider::Claude => headers.push(("anthropic-beta", CLAUDE_BETA)),
        Provider::Codex => {
            if let Some(id) = account_id {
                headers.push(("ChatGPT-Account-Id", id));
            }
        }
    }
    let request = Request {
        url,
        accept: "application/json",
        token: Some(&credential.token),
        form: None,
        headers,
        allow_http: loopback,
    };
    let reply = request.send().map_err(|error| {
        failure(match error.0.as_str() {
            "github_request_invalid" => "usage_request_invalid",
            "github_reply_invalid" => "usage_reply_invalid",
            _ => "usage_unreachable",
        })
    })?;
    let mut observation = match reply.status {
        200 => match provider {
            Provider::Claude => parse_claude(&reply.body)?,
            Provider::Codex => parse_codex(&reply.body, now_ms)?,
        },
        401 | 403 => return Err(failure("usage_sign_in_lapsed")),
        429 => return Err(failure("usage_rate_limited")),
        _ => return Err(failure("usage_poll_failed")),
    };
    if observation.plan.is_none() {
        observation.plan = plan(credential.plan.as_deref());
    }
    Ok(observation)
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
