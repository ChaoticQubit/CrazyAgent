use super::RegisteredNativeAuthHost;
use crate::engine::EngineError;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// The most words one handed launch holds: the interpreters, their first-line words and the
/// harness file.
pub(crate) const LAUNCH_WORDS_MAX: usize = 12;
/// The longest first-line word a handed launch carries.
const ARGUMENT_BYTES_MAX: usize = 256;
/// How often a sign-in waiting for the engine's first probe run checks whether it ended.
const PROBE_POLL: Duration = Duration::from_millis(50);

/// One word of the launch the engine resolved for a sign-in custodian: a file the launch runs
/// or reads, pinned by its real path, identity and hash, or a word the harness's first line
/// gives its interpreter.
pub(crate) enum LaunchWord {
    File(Box<RegisteredNativeAuthHost>),
    Argument(String),
}

/// One word of a handed launch as a custodian's command line names it: a file by its real
/// path and the SHA-256 the engine hashed, or a first-line word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandedWord<'a> {
    File { path: &'a Path, sha256: &'a str },
    Argument(&'a str),
}

/// Resolves how a sign-in custodian starts a pinned harness, with the engine's own places,
/// which hold the HOME folders and the login shell's PATH a custodian never has, and pins
/// every file of the words. Input: the pinned harness. Output: no words for a harness that
/// starts as itself, the pinned words, or none when the resolver refuses it or a file of its
/// words cannot be pinned.
pub(super) fn resolved(harness: &RegisteredNativeAuthHost) -> Option<Vec<LaunchWord>> {
    let path = harness.verified_path().ok()?;
    let places = crate::tools::Places::from_environment(None);
    let tool = crate::harnesses::interactive::launch_tool(path, &places).ok()?;
    let words = crate::harnesses::interactive::start_words(&tool);
    let grants = tool.grants();
    let real = |word: &OsStr| {
        Path::new(word)
            .is_absolute()
            .then(|| std::fs::canonicalize(word).ok())
            .flatten()
            .filter(|real| grants.contains(real))
    };
    if let [only] = words.as_slice()
        && real(only).as_deref() == Some(path)
    {
        return Some(Vec::new());
    }
    let mut launch = Vec::with_capacity(words.len());
    for word in &words {
        launch.push(match real(word) {
            Some(file) => {
                LaunchWord::File(Box::new(RegisteredNativeAuthHost::capture(&file).ok()?))
            }
            None => LaunchWord::Argument(argument(word.to_str()?)?.to_owned()),
        });
    }
    well_formed(&launch).then_some(launch)
}

/// Resolves a harness launch as [`resolved`] does and, for a sign-in that misses while the
/// engine's first probe run is still going, as on the first sign-in right after the app
/// starts, waits for that run, at most as long as its own probes may take, and resolves once
/// more, logging `native_auth_harness_waiting harness=<kind>` as it starts waiting. A
/// sign-in's window is at least ten minutes; a logout, whose bound is shorter, never waits.
/// Each resolve, which hashes interpreters of hundreds of MB, runs off the async worker.
/// Inputs: the pinned harness, `codex` or `claude`, and whether this action may wait.
/// Output: the pinned words, or none.
pub(super) async fn resolved_after_probe(
    harness: &RegisteredNativeAuthHost,
    kind: &str,
    may_wait: bool,
) -> Option<Vec<LaunchWord>> {
    assert!(kind == "codex" || kind == "claude");
    if let Some(launch) = crate::engine::off_worker(|| resolved(harness)) {
        return Some(launch);
    }
    if !may_wait || !crate::tools::probe::first_run_going() {
        return None;
    }
    eprintln!("native_auth_harness_waiting harness={kind}");
    let deadline = Instant::now() + crate::tools::probe::first_run_bound();
    while crate::tools::probe::first_run_going() && Instant::now() < deadline {
        tokio::time::sleep(PROBE_POLL).await;
    }
    crate::engine::off_worker(|| resolved(harness))
}

/// Names one pinned harness and the launch the engine resolved for it on a custodian's
/// command line: `--<kind>-executable` and `--<kind>-sha256`, then each word as
/// `--launch-file` with `--launch-sha256`, or `--launch-argument`. No environment is added.
/// Inputs: the command, `codex` or `claude`, the harness and its words. Output: Ok, or the
/// pin error of a file that changed.
pub(super) fn hand_over(
    command: &mut Command,
    kind: &str,
    harness: &RegisteredNativeAuthHost,
    launch: &[LaunchWord],
) -> Result<(), EngineError> {
    assert!(kind == "codex" || kind == "claude");
    assert!(launch.is_empty() || well_formed(launch));
    command
        .arg(format!("--{kind}-executable"))
        .arg(harness.verified_path()?)
        .arg(format!("--{kind}-sha256"))
        .arg(harness.verified_sha256()?);
    for word in launch {
        match word {
            LaunchWord::File(file) => command
                .arg("--launch-file")
                .arg(file.verified_path()?)
                .arg("--launch-sha256")
                .arg(file.verified_sha256()?),
            LaunchWord::Argument(argument) => command.arg("--launch-argument").arg(argument),
        };
    }
    Ok(())
}

/// Reads the launch words a custodian's command line names after its harness pin, as
/// [`hand_over`] writes them. Input: the arguments after the pin. Output: the words, empty
/// when none is named, or `native_auth_host_launch_invalid` for any other shape.
pub fn handed_words(arguments: &[OsString]) -> Result<Vec<HandedWord<'_>>, EngineError> {
    let invalid = || EngineError("native_auth_host_launch_invalid".into());
    let mut words = Vec::new();
    let mut rest = arguments;
    while !rest.is_empty() {
        if words.len() == LAUNCH_WORDS_MAX {
            return Err(invalid());
        }
        let flag = rest[0].to_str().ok_or_else(invalid)?;
        let (word, taken) = match (flag, rest) {
            ("--launch-file", [_, path, sha_flag, sha256, ..]) if sha_flag == "--launch-sha256" => {
                let path = Path::new(path);
                if !path.is_absolute() {
                    return Err(invalid());
                }
                let sha256 = sha256_text(sha256).ok_or_else(invalid)?;
                (HandedWord::File { path, sha256 }, 4)
            }
            ("--launch-argument", [_, value, ..]) => (
                HandedWord::Argument(value.to_str().and_then(argument).ok_or_else(invalid)?),
                2,
            ),
            _ => return Err(invalid()),
        };
        words.push(word);
        rest = &rest[taken..];
    }
    let ends = |word: Option<&HandedWord<'_>>| matches!(word, Some(HandedWord::File { .. }));
    if !words.is_empty() && !(ends(words.first()) && ends(words.last())) {
        return Err(invalid());
    }
    assert!(words.len() <= LAUNCH_WORDS_MAX);
    Ok(words)
}

/// A SHA-256 as the pins write it: 64 lowercase hex digits. Input: the word. Output: the
/// text, or none.
fn sha256_text(word: &OsStr) -> Option<&str> {
    word.to_str().filter(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Pins the launch the engine handed a custodian: each file is opened without following a
/// link and must hash to the SHA-256 the engine sent. Input: the handed words. Output: the
/// pinned words, `native_auth_host_executable_changed` for a file whose hash differs, or the
/// pin error of a file that cannot be pinned.
pub(super) fn pinned(handed: &[HandedWord<'_>]) -> Result<Vec<LaunchWord>, EngineError> {
    assert!(handed.len() <= LAUNCH_WORDS_MAX);
    let launch = handed
        .iter()
        .map(|word| match word {
            HandedWord::File { path, sha256 } => {
                let file = RegisteredNativeAuthHost::capture(path)?;
                if file.verified_sha256()? != *sha256 {
                    return Err(EngineError("native_auth_host_executable_changed".into()));
                }
                Ok(LaunchWord::File(Box::new(file)))
            }
            HandedWord::Argument(argument) => Ok(LaunchWord::Argument((*argument).to_owned())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    assert!(launch.is_empty() || well_formed(&launch));
    Ok(launch)
}

/// Starts a sign-in child from its launch words with no search path: the first word is the
/// program and the rest its arguments, the harness file last. Input: the words. Output: the
/// command, or none unless the program and the last word are absolute.
pub(crate) fn handed_command(words: &[OsString]) -> Option<Command> {
    let (program, rest) = words.split_first()?;
    let last = words.last()?;
    if !Path::new(program).is_absolute() || !Path::new(last).is_absolute() {
        return None;
    }
    let mut command = Command::new(program);
    command.args(rest);
    Some(command)
}

impl RegisteredNativeAuthHost {
    /// Gives a harness the launch the engine handed its custodian, already pinned. Input:
    /// the words, none for a harness that starts as itself. Output: the harness.
    pub(super) fn with_launch(mut self, launch: Vec<LaunchWord>) -> Self {
        assert!(self.launch.is_empty());
        assert!(launch.is_empty() || well_formed(&launch));
        self.launch = launch;
        self
    }

    /// The words that start this harness with no search path, every pinned file verified
    /// now: its handed launch, or the harness file alone when none was handed. Output: the
    /// words, or `native_auth_host_executable_changed` for a file that changed.
    pub(crate) fn launch_words(&self) -> Result<Vec<OsString>, EngineError> {
        let harness = self.verified_path()?.as_os_str().to_owned();
        if self.launch.is_empty() {
            return Ok(vec![harness]);
        }
        let words = self
            .launch
            .iter()
            .map(|word| match word {
                LaunchWord::File(file) => file.verified_path().map(|path| path.as_os_str().into()),
                LaunchWord::Argument(argument) => Ok(OsString::from(argument)),
            })
            .collect::<Result<Vec<OsString>, EngineError>>()?;
        assert!(!words.is_empty() && Path::new(&words[0]).is_absolute());
        Ok(words)
    }
}

/// Whether launch words have the shape a custodian runs: one to [`LAUNCH_WORDS_MAX`] words,
/// a pinned file first and last. Input: the words. Output: the answer.
fn well_formed(launch: &[LaunchWord]) -> bool {
    (1..=LAUNCH_WORDS_MAX).contains(&launch.len())
        && matches!(launch.first(), Some(LaunchWord::File(_)))
        && matches!(launch.last(), Some(LaunchWord::File(_)))
}

/// A first-line word a launch may carry: 1 to [`ARGUMENT_BYTES_MAX`] bytes with no NUL.
/// Input: the word. Output: the word, or none.
fn argument(word: &str) -> Option<&str> {
    (!word.is_empty() && word.len() <= ARGUMENT_BYTES_MAX && !word.contains('\0')).then_some(word)
}
