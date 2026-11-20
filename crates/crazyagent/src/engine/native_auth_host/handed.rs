use crate::engine::EngineError;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// The most words one handed launch holds: the interpreters, their first-line words and the
/// harness file.
pub(crate) const LAUNCH_WORDS_MAX: usize = 12;
/// The most command-line entries a sign-in custodian takes: its six-entry prefix (the
/// command, the runtime folder, and the harness executable with its hash) and
/// [`LAUNCH_WORDS_MAX`] words of at most four entries each (RV-DS241-52).
pub const CUSTODIAN_ARGUMENTS_MAX: usize = 6 + 4 * LAUNCH_WORDS_MAX;
/// The longest first-line word a handed launch carries.
const ARGUMENT_BYTES_MAX: usize = 256;
/// The answer to a custodian command line of any other shape.
const CUSTODIAN_USAGE: &str = "usage: crazyagent native-auth-custodian RUNTIME_DIR";

/// The harness a custodian's command line pins: `codex` or `claude`, its absolute file and
/// the SHA-256 the engine hashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandedHarness<'a> {
    pub kind: &'static str,
    pub executable: &'a Path,
    pub sha256: &'a str,
}

/// A sign-in custodian's command line as the engine writes it: the runtime folder, then for
/// a Codex or Claude Code sign-in the pinned harness and the launch resolved for it.
#[derive(Debug, PartialEq, Eq)]
pub struct CustodianArguments<'a> {
    pub runtime_dir: &'a Path,
    pub harness: Option<HandedHarness<'a>>,
    pub launch: Vec<HandedWord<'a>>,
}

/// Reads a sign-in custodian's command line, the one parser every OS's custodian runs before
/// it reads its input. Input: the entries after the program, `native-auth-custodian` first,
/// at most [`CUSTODIAN_ARGUMENTS_MAX`] plus one read. Output: the arguments, or
/// `native_auth_host_launch_too_long`, the usage line, `native_auth_host_executable_unsafe`
/// for a relative harness, `native_auth_host_executable_digest_invalid` or
/// `native_auth_host_launch_invalid`.
pub fn custodian_arguments(args: &[OsString]) -> Result<CustodianArguments<'_>, EngineError> {
    if args.len() > CUSTODIAN_ARGUMENTS_MAX {
        return Err(EngineError("native_auth_host_launch_too_long".into()));
    }
    let usage = || EngineError(CUSTODIAN_USAGE.into());
    let harness = match args.len() {
        2 => None,
        count if count >= 6 => {
            let kind = ["codex", "claude"]
                .into_iter()
                .find(|kind| {
                    args[2] == *format!("--{kind}-executable")
                        && args[4] == *format!("--{kind}-sha256")
                })
                .ok_or_else(usage)?;
            let executable = Path::new(&args[3]);
            if !executable.is_absolute() {
                return Err(EngineError("native_auth_host_executable_unsafe".into()));
            }
            let sha256 = sha256_text(&args[5])
                .ok_or_else(|| EngineError("native_auth_host_executable_digest_invalid".into()))?;
            Some(HandedHarness {
                kind,
                executable,
                sha256,
            })
        }
        _ => return Err(usage()),
    };
    assert!(args.len() >= 2);
    let launch = handed_words(args.get(6..).unwrap_or_default())?;
    assert!(harness.is_some() || launch.is_empty());
    Ok(CustodianArguments {
        runtime_dir: Path::new(&args[1]),
        harness,
        launch,
    })
}

/// One word of a handed launch as a custodian's command line names it: a file by its real
/// path and the SHA-256 the engine hashed, or a first-line word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandedWord<'a> {
    File { path: &'a Path, sha256: &'a str },
    Argument(&'a str),
}

/// Reads the launch words a custodian's command line names after its harness pin, as
/// [`super::launch::hand_over`] writes them. Input: the arguments after the pin. Output:
/// the words, empty when none is named, or `native_auth_host_launch_invalid` for any other
/// shape.
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

/// A first-line word a launch may carry: 1 to [`ARGUMENT_BYTES_MAX`] bytes with no NUL.
/// Input: the word. Output: the word, or none.
pub(super) fn argument(word: &str) -> Option<&str> {
    (!word.is_empty() && word.len() <= ARGUMENT_BYTES_MAX && !word.contains('\0')).then_some(word)
}
