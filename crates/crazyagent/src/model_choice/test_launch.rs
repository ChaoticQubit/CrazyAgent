use serde::{Deserialize, Serialize};
use std::time::Duration;

/// What one test launch of an owner model id decided (DEC-150 addendum 7): the harness
/// answered on it, the provider refused it with its known code, or nothing was decided
/// and the id stays pending with a reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tested {
    Verified,
    Failed,
    Pending,
}

/// How a test launch's run ended, as the read-only runner tells it, without its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Answered,
    Failed,
    TimedOut,
    Stopped,
}

/// Why a test launch did not start a run, each told to the owner as a pending reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStarted {
    NotInstalled,
    NoAccount,
    Paused,
    UrlAccount,
    CouldNotStart,
}

/// The longest one test launch's run may take.
pub const WALL_TIME: Duration = Duration::from_secs(120);
/// The most output tokens the run's Claude Code request may use.
pub const OUTPUT_TOKENS: u32 = 16;
/// The one prompt every test launch sends: a fixed text, so the run's output is never
/// the owner's or a project's.
pub const PROMPT: &str = "Reply with the single word OK.";
/// The longest pending reason a test launch gives, so a client row never grows past it.
pub const REASON_BYTES_MAX: usize = 240;
/// A test launch for this harness is already running in this engine.
pub const BUSY: &str = "model_choice_test_busy";
/// The test named no pool.
pub const POOL_NEEDED: &str = "model_choice_pool_needed";
/// The named pool is not in the settings.
pub const POOL_UNKNOWN: &str = "model_choice_pool_unknown";

/// Tells whether a harness has a test launch: the harnesses whose model a read-only run
/// can set. Input: the catalog harness id. Output: true for Claude Code and Codex.
pub fn supported(harness: &str) -> bool {
    matches!(harness, "claude-code" | "codex")
}

/// Names the provider whose refusal a test launch reads, for the pending reason when the
/// provider gave none. Input: a supported harness. Output: the provider's name.
pub fn provider(harness: &str) -> &'static str {
    assert!(supported(harness));
    match harness {
        "claude-code" => "Anthropic",
        _ => "OpenAI",
    }
}

/// Tells whether a failed test run's standard output carries the provider's refusal code
/// for the harness (DEC-150 F8) as a whole word. Inputs: the supported harness and the
/// output, at most the runner's output bound. Output: true when the code is there.
pub fn refused(harness: &str, output: &str) -> bool {
    assert!(supported(harness));
    let _ = output;
    false
}

/// Decides a test launch from how its run ended and, for a failed run, its output.
/// Inputs: the supported harness, the ending and the output when it could be read.
/// Output: the decision and, for a pending one, its reason of at most `REASON_BYTES_MAX`.
pub fn outcome(harness: &str, ended: Ended, output: Option<&str>) -> (Tested, Option<String>) {
    assert!(supported(harness));
    let _ = (ended, output);
    (Tested::Pending, None)
}

/// Words the pending reason of a test launch that started no run. Inputs: the supported
/// harness and why. Output: the reason, at most `REASON_BYTES_MAX` bytes.
pub fn not_started(harness: &str, why: NotStarted) -> String {
    assert!(supported(harness));
    let _ = why;
    String::new()
}

#[cfg(test)]
#[path = "test_launch_tests.rs"]
mod tests;
