use super::{ParseRefusal, ParsedResults, ResultRow};
use crate::tests_model::ResultOutcome;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct AllureResult {
    #[serde(rename = "fullName")]
    full_name: Option<String>,
    name: Option<String>,
    status: Option<Value>,
    #[serde(rename = "statusDetails")]
    status_details: Option<StatusDetails>,
    start: Option<u64>,
    stop: Option<u64>,
}

#[derive(Deserialize)]
struct StatusDetails {
    message: Option<String>,
    trace: Option<String>,
}

/// Reads one Allure `*-result.json` file into one row named by `fullName`, or by `name`
/// when an adapter writes no full name. Status passed, failed, broken (errored) or skipped.
/// Input: the file bytes. Output: the row, or the refusal for anything that is not a result
/// object, a missing name, or any other status.
pub(super) fn parse(bytes: &[u8]) -> Result<ParsedResults, ParseRefusal> {
    let result: AllureResult =
        serde_json::from_slice(bytes).map_err(|_| ParseRefusal::Malformed {
            reason: "the Allure result is not a JSON result object",
        })?;
    let outcome = match result.status.as_ref().and_then(Value::as_str) {
        Some("passed") => ResultOutcome::Passed,
        Some("failed") => ResultOutcome::Failed,
        Some("broken") => ResultOutcome::Errored,
        Some("skipped") => ResultOutcome::Skipped,
        _ => return Err(ParseRefusal::UnknownStatus),
    };
    let name = result
        .full_name
        .filter(|name| !name.trim().is_empty())
        .or(result.name)
        .ok_or(ParseRefusal::BadName {
            reason: "an Allure result has no fullName or name",
        })?;
    let duration_ms = match (result.start, result.stop) {
        (Some(start), Some(stop)) => stop.checked_sub(start),
        _ => None,
    };
    let message = result.status_details.and_then(|details| {
        details
            .message
            .filter(|message| !message.trim().is_empty())
            .or(details.trace)
    });
    Ok(ParsedResults {
        rows: vec![ResultRow {
            name,
            outcome,
            duration_ms,
            message,
        }],
        summary: None,
        build_failed: false,
    })
}
