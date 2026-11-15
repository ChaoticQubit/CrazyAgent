use super::*;
use crate::harnesses::registry::registry;

const UNWIRED: [&str; 16] = [
    "rcli",
    "opencode",
    "gemini-cli",
    "fx",
    "openclaw",
    "pi",
    "goose",
    "aider",
    "qwen-code",
    "crush",
    "droid",
    "copilot",
    "cline",
    "continue",
    "openhands",
    "kimi",
];

fn unsupported() -> Result<Piped, LaunchError> {
    Err(failure("launch_harness_unsupported"))
}

#[test]
fn each_driver_kind_has_its_own_answer() {
    assert_eq!(of(DriverKind::CodexAppServer), Ok(Piped::Codex));
    assert_eq!(of(DriverKind::ClaudeStreamJson), Ok(Piped::Claude));
    assert_eq!(of(DriverKind::Acp), Ok(Piped::Acp));
    assert_eq!(of(DriverKind::JsonlTurn), Ok(Piped::JsonlTurn));
    assert_eq!(of(DriverKind::JsonlRpc), Ok(Piped::JsonlRpc));
    for kind in [DriverKind::Rcli, DriverKind::TextTurn] {
        assert_eq!(of(kind), unsupported(), "{kind:?}");
    }
}

#[test]
fn a_launch_finds_the_driver_its_manifest_names_by_id_or_alias() {
    assert_eq!(piped("codex"), Ok(Piped::Codex));
    assert_eq!(piped("claude-code"), Ok(Piped::Claude));
    assert_eq!(piped("claude"), Ok(Piped::Claude));
    for name in UNWIRED.into_iter().chain(["unknown", "Codex", ""]) {
        assert_eq!(piped(name), unsupported(), "{name}");
    }
    assert_eq!(registry().all().len(), UNWIRED.len() + 2);
}
