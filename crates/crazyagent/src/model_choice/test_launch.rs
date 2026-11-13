use crate::protocol::CredentialRef;
use serde::{Deserialize, Serialize};
use std::path::Path;
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
    use crate::harnesses::launch::{
        CLAUDE_MODEL_REFUSED, CODEX_MODEL_REFUSED, CODEX_SERVER_MODEL_REFUSED,
    };
    assert!(supported(harness));
    let codes: &[&str] = match harness {
        "claude-code" => &[CLAUDE_MODEL_REFUSED],
        _ => &[CODEX_MODEL_REFUSED, CODEX_SERVER_MODEL_REFUSED],
    };
    codes.iter().any(|code| word(output, code))
}

/// Tells whether a text holds a code as a whole word: no letter, digit or underscore on
/// either side of it. Inputs: the text and the non-empty code. Output: true when it does.
fn word(text: &str, code: &str) -> bool {
    assert!(!code.is_empty());
    let joins = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let bytes = text.as_bytes();
    text.match_indices(code).any(|(at, _)| {
        let end = at + code.len();
        assert!(end <= bytes.len());
        !(at > 0 && joins(bytes[at - 1])) && !(end < bytes.len() && joins(bytes[end]))
    })
}

/// Decides a test launch from how its run ended and, for a failed run, its output.
/// Inputs: the supported harness, the ending and the output when it could be read.
/// Output: the decision and, for a pending one, its reason of at most `REASON_BYTES_MAX`.
pub fn outcome(harness: &str, ended: Ended, output: Option<&str>) -> (Tested, Option<String>) {
    assert!(supported(harness));
    let decided = match ended {
        Ended::Answered => (Tested::Verified, None),
        Ended::Failed if output.is_some_and(|output| refused(harness, output)) => {
            (Tested::Failed, None)
        }
        Ended::Failed => (
            Tested::Pending,
            Some(format!(
                "Not reported by {}: the test launch ended without an answer or a refusal.",
                provider(harness)
            )),
        ),
        Ended::TimedOut => (
            Tested::Pending,
            Some("The test launch timed out.".to_owned()),
        ),
        Ended::Stopped => (
            Tested::Pending,
            Some("The engine stopped the test launch.".to_owned()),
        ),
    };
    assert_eq!(decided.0 == Tested::Pending, decided.1.is_some());
    assert!(
        decided
            .1
            .as_ref()
            .is_none_or(|reason| reason.len() <= REASON_BYTES_MAX)
    );
    decided
}

/// Words the pending reason of a test launch that started no run. Inputs: the supported
/// harness and why. Output: the reason, at most `REASON_BYTES_MAX` bytes.
pub fn not_started(harness: &str, why: NotStarted) -> String {
    assert!(supported(harness));
    let name = crate::harnesses::spec(harness).map_or(harness, |spec| spec.name);
    let reason = match why {
        NotStarted::NotInstalled => format!("{name} is not installed, so the id was not tested."),
        NotStarted::NoAccount => format!("No signed-in {name} account in this pool."),
        NotStarted::Paused => format!("Every {name} account in this pool is paused after a limit."),
        NotStarted::UrlAccount => format!(
            "This pool's first {name} account is a URL account, which runs only its own model."
        ),
        NotStarted::CouldNotStart => format!("The {name} test launch could not start."),
    };
    assert!(!reason.is_empty() && reason.len() <= REASON_BYTES_MAX);
    reason
}

/// How an RCLI test launch's run ended (DEC-150 addendum 9e): the host's answer, a failed
/// turn with the provider failure the host named, if any, no answer in time, or stopped by
/// the engine. The host's error text is never kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcliEnded {
    Answered,
    Failed(Option<crate::rcli_host::RcliProviderFailure>),
    TimedOut,
    Stopped,
}

/// Decides an RCLI test launch by the provider of the account it ran on (DEC-150 addendum
/// 9e): an answer verifies the id, a 404 from OpenAI or Anthropic fails it, and anything
/// else, an OpenRouter 404 included, leaves it pending with a fixed reason. Inputs: the
/// RCLI provider id and the ending. Output: the decision and its pending reason.
pub fn rcli_outcome(provider: &str, ended: RcliEnded) -> (Tested, Option<String>) {
    let _ = (provider, ended);
    (Tested::Pending, Some(String::new()))
}

/// Words the pending reason of an RCLI test launch that started no run, by the provider
/// the id is tested on. Inputs: the RCLI provider id and why. Output: the reason, at most
/// `REASON_BYTES_MAX` bytes.
pub fn rcli_not_started(provider: &str, why: NotStarted) -> String {
    let _ = (provider, why);
    String::new()
}

/// One test launch to run: the engine's data folder, the two settings files, the
/// installed harness executables, the harness, the owner's id and the pool to run it on.
pub struct Request<'a> {
    pub root: &'a Path,
    pub settings: (&'a Path, &'a Path),
    pub codex: Option<&'a Path>,
    pub claude: Option<&'a Path>,
    pub harness: &'a str,
    pub model: &'a str,
    pub pool: &'a CredentialRef,
}

#[cfg(unix)]
mod run {
    use super::{
        BUSY, Ended, NotStarted, OUTPUT_TOKENS, POOL_UNKNOWN, PROMPT, Request, Tested, WALL_TIME,
        not_started, outcome, supported,
    };
    use crate::instruction_cleanup::private_folder;
    use crate::research::accounts::{AccountPick, SettingsResolver};
    use crate::research::runner::{
        ReadOnlyEnding, ReadOnlyRun, ResearchAccount, ResearchHarness, ResearchLimits,
        ResearchRunner,
    };
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    const FOLDER: &str = "model-test";
    const UNAVAILABLE: &str = "model_choice_unavailable";
    const RUNNING_MAX: usize = 64;

    static RUNNING: Mutex<Vec<(PathBuf, &'static str)>> = Mutex::new(Vec::new());

    /// Holds the one test slot of a harness in an engine data folder until dropped.
    struct Slot(PathBuf, &'static str);

    impl Slot {
        /// Takes the test slot of a harness. Inputs: the canonical engine folder and the
        /// research harness id. Output: the slot, or none while another test holds it.
        fn take(root: &Path, harness: &'static str) -> Option<Self> {
            let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
            let held = |(folder, id): &(PathBuf, &str)| folder == root && *id == harness;
            if running.len() >= RUNNING_MAX || running.iter().any(held) {
                return None;
            }
            running.push((root.to_path_buf(), harness));
            assert!(running.len() <= RUNNING_MAX);
            Some(Self(root.to_path_buf(), harness))
        }
    }

    impl Drop for Slot {
        fn drop(&mut self) {
            let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
            let before = running.len();
            running.retain(|(folder, id)| !(folder == &self.0 && *id == self.1));
            assert!(running.len() + 1 == before);
        }
    }

    /// Runs one test launch: a tools-off read-only run of the fixed prompt on the first
    /// fitting account of the owner's pool, with the owner's id as its model, in an empty
    /// engine-owned folder. A URL account runs only its own model, so it is not tried.
    /// The run's folder is removed once its output is read. Input: the request. Output:
    /// the decision and its pending reason, or `BUSY`, `POOL_UNKNOWN` or unavailable.
    pub(crate) fn run(request: &Request<'_>) -> Result<(Tested, Option<String>), &'static str> {
        assert!(supported(request.harness));
        let harness = match request.harness {
            "claude-code" => ResearchHarness::ClaudeCode,
            _ => ResearchHarness::Codex,
        };
        let root = request.root.canonicalize().map_err(|_| UNAVAILABLE)?;
        let _slot = Slot::take(&root, harness.id()).ok_or(BUSY)?;
        let home = private_folder(root.join(FOLDER)).ok_or(UNAVAILABLE)?;
        let state = private_folder(home.join("state")).ok_or(UNAVAILABLE)?;
        let empty = private_folder(home.join("empty")).ok_or(UNAVAILABLE)?;
        let database = root.join("engine.db");
        let (global, project) = request.settings;
        let limits = ResearchLimits::default();
        let mut runner = match request.codex {
            Some(codex) => ResearchRunner::new(&database, &state, codex, limits),
            None => ResearchRunner::without_codex(&database, &state, limits),
        }
        .map_err(|_| UNAVAILABLE)?
        .with_settings(global, project);
        if let Some(claude) = request.claude {
            runner = runner.with_claude(claude).map_err(|_| UNAVAILABLE)?;
        }
        let pending = |why| Ok((Tested::Pending, Some(not_started(request.harness, why))));
        if !runner.can_run(harness) {
            return pending(NotStarted::NotInstalled);
        }
        let projects = private_folder(root.join(crate::project_setup::PROJECTS_DIRECTORY))
            .ok_or(UNAVAILABLE)?;
        let resolver = SettingsResolver::new(global, project, &projects, &database)
            .map_err(|_| UNAVAILABLE)?;
        let account = match resolver.pool_pick("", request.pool, &[harness]) {
            AccountPick::NeedsPool => return Err(POOL_UNKNOWN),
            AccountPick::NoUsableAccount => return pending(NotStarted::NoAccount),
            AccountPick::Paused { .. } => return pending(NotStarted::Paused),
            AccountPick::Account { account, .. } => match *account {
                ResearchAccount::ManagedCodex { connection, .. } => ResearchAccount::ManagedCodex {
                    connection,
                    model: Some(request.model.to_owned()),
                },
                ResearchAccount::ManagedClaude { connection, .. } => {
                    ResearchAccount::ManagedClaude {
                        connection,
                        model: Some(request.model.to_owned()),
                    }
                }
                ResearchAccount::Endpoint { .. } | ResearchAccount::AnthropicEndpoint { .. } => {
                    return pending(NotStarted::UrlAccount);
                }
            },
        };
        if account.harness() != harness {
            return pending(NotStarted::NoAccount);
        }
        let name = format!("{FOLDER}-{:013}", crate::team::now_ms());
        let run = ReadOnlyRun {
            name: &name,
            agent: FOLDER,
            project: None,
            folder: &empty,
            prompt: PROMPT,
            account: &account,
            wall_time: WALL_TIME,
            output_tokens: Some(OUTPUT_TOKENS),
        };
        let ended = match runner.run_read_only(&run, &|| false) {
            Ok(ReadOnlyEnding::Answered(_)) => Ended::Answered,
            Ok(ReadOnlyEnding::Failed(_)) => Ended::Failed,
            Ok(ReadOnlyEnding::TimedOut) => Ended::TimedOut,
            Ok(ReadOnlyEnding::Stopped) => Ended::Stopped,
            Err(error) => {
                eprintln!(
                    "model_choice_test_failed harness={} cause={error}",
                    harness.id()
                );
                return pending(NotStarted::CouldNotStart);
            }
        };
        let output = runner.read_only_output(&name, harness);
        runner.forget_read_only(&name);
        Ok(outcome(request.harness, ended, output.as_deref()))
    }
}

#[cfg(unix)]
pub(crate) use run::run;

/// Refuses a test launch where confined model runs do not exist. Input: the request,
/// unused. Output: unavailable.
#[cfg(not(unix))]
pub(crate) fn run(_request: &Request<'_>) -> Result<(Tested, Option<String>), &'static str> {
    Err("model_choice_unavailable")
}

#[cfg(test)]
#[path = "test_launch_tests.rs"]
mod tests;
