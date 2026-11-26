use super::*;

#[test]
fn u_shows_the_line_the_engine_has_now_not_the_one_read_when_the_page_opened() {
    let found = found();
    let mut shell = on_claude(&found);
    deliver(&mut shell, newer());
    press(&mut shell, 'u', with_npm());
    let shown = frame(&mut shell);
    assert!(shown.contains(NPM_LINE), "{shown}");
    assert!(shown.contains("Installed with npm (@anthropic-ai/claude-code)."));
    assert!(!shown.contains(BREW_LINE), "{shown}");
}
