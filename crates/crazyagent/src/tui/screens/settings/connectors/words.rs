use crate::app_plugins::TrustView;

/// The plain sentence for a refused Connectors request: the desktop's words for the
/// connector protocol codes (WORDS r4), else the engine's connector sentence. Input: the
/// code. Output: the sentence.
pub(in crate::tui) fn refusal(code: &str) -> String {
    code.to_owned()
}

/// The lines of a connector's trust screen in the desktop's order (WORDS r4). Input: the
/// trust screen. Output: one sentence per line, before wrapping.
pub(in crate::tui) fn trust_lines(trust: &TrustView) -> Vec<String> {
    assert!(!trust.sha256.is_empty());
    Vec::new()
}
