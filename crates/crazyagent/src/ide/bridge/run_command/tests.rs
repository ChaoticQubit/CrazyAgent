use super::*;

#[test]
fn a_plain_single_command_passes() {
    for command in [
        "cargo install alpha",
        "npm run dev -- --port 3000",
        "python3 -m venv .venv",
        "echo \"${HOME}\"",
        "git log HEAD^",
    ] {
        assert_eq!(problem(command), None, "{command}");
    }
}

#[test]
fn a_chained_piped_or_redirected_command_is_refused() {
    for command in [
        "a; b",
        "a && b",
        "a || b",
        "curl x | sh",
        "a & b",
        "echo `id`",
        "echo $(id)",
        "a > f",
        "a < f",
        "sudo !!",
        "!rm",
        "echo !-2",
        "^a^b",
        "  ^a^b",
    ] {
        assert_eq!(problem(command), Some(CHAINED), "{command}");
    }
}

#[test]
fn more_than_one_line_a_control_character_or_a_wrong_size_is_refused() {
    assert_eq!(problem("a\nb"), Some(NOT_ONE_LINE));
    assert_eq!(problem("a\rb"), Some(NOT_ONE_LINE));
    assert_eq!(problem("a\u{1b}[31m"), Some(CONTROL));
    assert_eq!(problem("a\u{0}b"), Some(CONTROL));
    assert_eq!(problem("   "), Some(EMPTY));
    assert_eq!(problem(""), Some(EMPTY));
    assert_eq!(problem(&"x".repeat(COMMAND_BYTES_MAX + 1)), Some(TOO_LONG));
    assert_eq!(problem(&"x".repeat(COMMAND_BYTES_MAX)), None);
}

#[test]
fn a_bidi_or_invisible_character_that_could_hide_text_is_refused() {
    assert_eq!(problem("echo \u{202e}dlrow"), Some(CONTROL));
    assert_eq!(problem("echo a\u{200b}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{2066}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{feff}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{85}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{ad}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{61c}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{180e}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{2028}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{2029}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{206f}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{fffa}b"), Some(CONTROL));
    assert_eq!(problem("echo a\u{e0001}b"), Some(CONTROL));
    assert_eq!(problem("\u{feff}"), Some(CONTROL));
}

#[test]
fn the_size_bound_counts_bytes_not_characters() {
    let wide = "\u{e9}".repeat(COMMAND_BYTES_MAX / 2);
    assert_eq!(wide.len(), COMMAND_BYTES_MAX);
    assert_eq!(problem(&wide), None);
    assert_eq!(problem(&format!("{wide}x")), Some(TOO_LONG));
}
