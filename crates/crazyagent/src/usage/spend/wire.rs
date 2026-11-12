use super::{Outcome, id_shaped, key_shaped};
use crate::engine::EngineError;
use crate::github::http::Request;
use crate::usage::poll::PollCredential;
use serde_json::Value;

#[cfg(all(test, target_os = "macos"))]
mod wire_tests;

/// The Codex consume call, beside its usage call: `wham/rate-limit-reset-credits/consume`,
/// the path the Codex CLI 0.167.1 binary names (w-v240-l8 evidence codex-scan.txt). A
/// private call that may break (DEC-154 addendum, leader 13:02 route (a)).
pub const CODEX_CONSUME_PATH: &str = "rate-limit-reset-credits/consume";
const OUTCOME_FIELDS: [&str; 3] = ["outcome", "result", "status"];

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

/// The consume call's URL from the usage call's, so a loopback test server serves both.
/// Input: the usage URL. Output: the consume URL, or `usage_request_invalid`.
pub fn codex_consume_url(usage: &url::Url) -> Result<url::Url, EngineError> {
    let url = usage
        .join(CODEX_CONSUME_PATH)
        .map_err(|_| failure("usage_request_invalid"))?;
    assert!(url.path().ends_with("/rate-limit-reset-credits/consume"));
    assert!(url.query().is_none());
    Ok(url)
}

/// Builds the consume request: the token, the key as `Idempotency-Key` and the shape-checked
/// ChatGPT account id, all for curl's stdin only. Inputs: the URL, the credential, the key
/// and whether a loopback test URL is allowed. Output: the request, or
/// `usage_request_invalid` for a key or account id of the wrong shape.
pub(super) fn codex_request<'a>(
    url: &url::Url,
    credential: &'a PollCredential,
    key: &'a str,
    loopback: bool,
) -> Result<Request<'a>, EngineError> {
    if !key_shaped(key) {
        return Err(failure("usage_request_invalid"));
    }
    let mut headers = vec![("Idempotency-Key", key)];
    if let Some(id) = credential.account_id.as_deref() {
        if !id_shaped(id) {
            return Err(failure("usage_request_invalid"));
        }
        headers.push(("ChatGPT-Account-Id", id));
    }
    Ok(Request {
        url: url.clone(),
        accept: "application/json",
        token: Some(&credential.token),
        form: None,
        headers,
        allow_http: loopback,
    })
}

/// Spends one Codex free reset: a POST of `{}` with the key, so the backend picks the reset
/// and no reset id is ever handled. Inputs: the consume URL, the credential, the key and
/// whether a loopback test URL is allowed. Output: the outcome; `usage_sign_in_lapsed` for
/// 401 or 403, `usage_rate_limited` for 429, `usage_spend_failed` for another status,
/// `usage_unreachable` when no reply came, or `usage_request_invalid`.
pub fn codex_consume(
    url: url::Url,
    credential: &PollCredential,
    key: &str,
    loopback: bool,
) -> Result<Outcome, EngineError> {
    let request = codex_request(&url, credential, key, loopback)?;
    let reply = request.send_json("{}").map_err(|error| {
        failure(match error.0.as_str() {
            "github_request_invalid" => "usage_request_invalid",
            "github_reply_invalid" => "usage_reply_invalid",
            _ => "usage_unreachable",
        })
    })?;
    match reply.status {
        200..=299 => Ok(consume_outcome(&reply.body)),
        401 | 403 => Err(failure("usage_sign_in_lapsed")),
        429 => Err(failure("usage_rate_limited")),
        _ => Err(failure("usage_spend_failed")),
    }
}

/// Reads a consume reply's outcome word in either case style, from `outcome`, `result` or
/// `status`; any other reply is an unknown outcome. Input: the body. Output: the outcome.
pub(super) fn consume_outcome(body: &[u8]) -> Outcome {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Outcome::Unknown;
    };
    let Some(word) = OUTCOME_FIELDS
        .iter()
        .find_map(|name| value.get(name)?.as_str())
    else {
        return Outcome::Unknown;
    };
    let squashed: String = word
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match squashed.as_str() {
        "reset" => Outcome::Reset,
        "nothingtoreset" => Outcome::NothingToReset,
        "nocredit" => Outcome::NoCredit,
        "alreadyredeemed" => Outcome::AlreadyRedeemed,
        _ => Outcome::Unknown,
    }
}
