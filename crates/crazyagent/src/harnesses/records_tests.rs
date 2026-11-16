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

fn unsupported() -> Result<Reader, LaunchError> {
    Err(failure("launch_harness_unsupported"))
}

#[test]
fn each_record_kind_has_its_own_answer() {
    assert_eq!(of(RecordKind::CodexRollouts), Ok(Reader::CodexRollouts));
    assert_eq!(of(RecordKind::ClaudeSession), Ok(Reader::ClaudeSession));
    assert_eq!(of(RecordKind::JsonlAppend), Ok(Reader::JsonlAppend));
    assert_eq!(of(RecordKind::JsonRewrite), Ok(Reader::JsonRewrite));
    for kind in [
        RecordKind::SqlitePoll,
        RecordKind::MarkdownHistory,
        RecordKind::Screen,
        RecordKind::Unavailable,
    ] {
        assert_eq!(of(kind), unsupported(), "{kind:?}");
    }
}

#[test]
fn a_launch_finds_the_reader_its_manifest_names_by_id_or_alias() {
    assert_eq!(reader("codex"), Ok(Reader::CodexRollouts));
    assert_eq!(reader("claude-code"), Ok(Reader::ClaudeSession));
    assert_eq!(reader("claude"), Ok(Reader::ClaudeSession));
    for name in UNWIRED.into_iter().chain(["unknown", "Codex", ""]) {
        assert_eq!(reader(name), unsupported(), "{name}");
    }
    assert_eq!(registry().all().len(), UNWIRED.len() + 2);
}

#[test]
fn a_stored_record_names_its_harness_by_catalog_id_only() {
    assert_eq!(stored("codex"), Some(Reader::CodexRollouts));
    assert_eq!(stored("claude-code"), Some(Reader::ClaudeSession));
    for name in UNWIRED
        .into_iter()
        .chain(["claude", "unknown", "Codex", ""])
    {
        assert_eq!(stored(name), None, "{name}");
    }
}
