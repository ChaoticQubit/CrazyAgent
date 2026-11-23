use super::*;

/// H-import, Q4: through the owner transport the bundled registry offers no harness home
/// instruction file and refuses a read for Claude Code or Codex, so no request reads a file
/// in the owner's home until a bundled manifest names one with the owner's yes.
#[test]
fn the_bundled_registry_offers_no_harness_instruction_source_through_the_transport() {
    let (_directory, root) = root();

    let listed = run(&root, Action::Sources, None);

    assert!(
        matches!(listed, Ok(Answer::Sources(ref sources)) if sources.is_empty()),
        "{listed:?}"
    );
    for harness in ["claude-code", "codex"] {
        let refused = run(
            &root,
            Action::ReadSource {
                harness: harness.into(),
            },
            None,
        );
        assert_eq!(
            refused.err().map(|refused| refused.0),
            Some("process_request_invalid".to_owned()),
            "{harness}"
        );
    }
}
