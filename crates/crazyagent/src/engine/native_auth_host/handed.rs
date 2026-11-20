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
