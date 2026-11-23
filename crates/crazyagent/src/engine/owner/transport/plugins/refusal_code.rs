use super::*;

/// The code a refused plugins request is answered with: the engine's code when it is plain
/// lowercase words of at most 96 bytes, else `plugins_outcome_unknown`. A storage answer
/// that did not come within the owner's wait, which the shared engine waits name after a
/// native sign-in read, is this protocol's `plugins_outcome_unknown` too, so a plugins
/// client is never told to read a sign-in operation. Input: the error. Output: the code.
pub(super) fn public_code(error: EngineError) -> String {
    if error == crate::engine::owner::native_auth::outcome_unknown() {
        return unknown().0;
    }
    if error.0.len() <= 96
        && error
            .0
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        error.0
    } else {
        "plugins_outcome_unknown".into()
    }
}
