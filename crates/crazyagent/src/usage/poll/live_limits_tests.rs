use super::{CLAUDE_USAGE_URL, CODEX_USAGE_URL, Provider, claude_user_agent, fetch};
use crate::team::runtime::managed_agent::{SignedIn, claude, codex};
use crate::usage::extras::CLAUDE_DETAIL_QUERY;
use serde_json::Value;

const DEPTH_MAX: usize = 6;
const LINES_MAX: usize = 200;
const KEY_BYTES_MAX: usize = 40;
const DIGITS_MAX: usize = 2;
const CLAUDE_PROFILE_PATH: &str = "/api/oauth/profile";

/// The shape of a reply as lines of key paths and value types, never a value: a key that is
/// not a short word (a letter first, at most two digits) prints as `<key>`, so no id used as
/// a key prints; an array prints its first item's shape only.
/// Inputs: the value, its path and the lines so far. Output: none; lines are appended.
fn shape(value: &Value, path: &str, lines: &mut Vec<String>) {
    if lines.len() >= LINES_MAX || path.matches('.').count() > DEPTH_MAX {
        return;
    }
    let kind = match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "float",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    lines.push(format!("{path}: {kind}"));
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                let named = key.len() <= KEY_BYTES_MAX
                    && key.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
                    && key.bytes().filter(u8::is_ascii_digit).count() <= DIGITS_MAX
                    && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
                let key = if named { key.as_str() } else { "<key>" };
                shape(field, &format!("{path}.{key}"), lines);
            }
        }
        Value::Array(items) => {
            if let Some(first) = items.first() {
                shape(first, &format!("{path}[]"), lines);
            }
        }
        _ => {}
    }
}

/// Prints one reply's shape, or only its failure code. Inputs: the label and the read.
/// Output: none.
fn print_shape(label: &str, read: Result<Vec<u8>, crate::engine::EngineError>) {
    let body = match read {
        Ok(body) => body,
        Err(problem) => {
            println!("{label}: refused with {}", problem.0);
            return;
        }
    };
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        println!("{label}: not JSON");
        return;
    };
    let mut lines = Vec::new();
    shape(&value, "$", &mut lines);
    println!("{label}:\n{}", lines.join("\n"));
}

/// Prints the key names and value types of a real ChatGPT Codex slot's usage reply, to
/// confirm `rate_limit_reset_credits.available_count` and `credits` (DEC-154 E7). Needs the
/// owner's GO: it reads a real app-managed sign-in through the custody lease.
#[test]
#[ignore = "live probe: owner GO only; needs Codex signed in with ChatGPT, CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn probe_the_codex_usage_reply_shape() {
    let mut signed_in = SignedIn::codex();
    let lease = signed_in.acquire("codex-live-limits-probe");
    let credential = lease.usage_credential(&codex());
    drop(lease);
    let credential = credential.unwrap_or_else(|problem| panic!("BLOCKED: {}", problem.0));
    print_shape(
        "codex usage",
        fetch(
            Provider::Codex,
            CODEX_USAGE_URL.parse().unwrap(),
            &credential,
            false,
            None,
        ),
    );
}

/// Prints the key names and value types of a real Claude account's usage reply, its private
/// detail reply and its OAuth profile reply, to confirm `extra_usage`, `cedar_ember` with its
/// grants and where the spend's organization comes from (DEC-154 E7). A refused read prints
/// its code only. Needs the owner's GO: it reads a real app-managed sign-in through the
/// custody lease and calls a private query.
#[test]
#[ignore = "live probe: owner GO only; needs Claude signed in, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn probe_the_claude_usage_and_detail_reply_shapes() {
    let mut signed_in = SignedIn::claude();
    let lease = signed_in.acquire("claude-live-limits-probe");
    let credential = lease.usage_credential(&claude());
    drop(lease);
    let credential = credential.unwrap_or_else(|problem| panic!("BLOCKED: {}", problem.0));
    let url: url::Url = CLAUDE_USAGE_URL.parse().unwrap();
    print_shape(
        "claude usage",
        fetch(Provider::Claude, url.clone(), &credential, false, None),
    );
    let mut detail = url.clone();
    detail.set_query(Some(CLAUDE_DETAIL_QUERY));
    print_shape(
        "claude detail",
        fetch(
            Provider::Claude,
            detail,
            &credential,
            false,
            Some(&claude_user_agent(None)),
        ),
    );
    let profile = url.join(CLAUDE_PROFILE_PATH).unwrap();
    print_shape(
        "claude profile",
        fetch(Provider::Claude, profile, &credential, false, None),
    );
}
