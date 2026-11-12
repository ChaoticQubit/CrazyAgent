use super::{cut, pad, safe, width, wrap};

#[test]
fn safe_replaces_terminal_controls_and_hidden_characters_with_one_mark_each() {
    let text = "a\u{1b}[2Jb\u{7}c\u{9b}d\u{7f}e\u{202e}f\u{200b}g\u{e0041}h\u{feff}i\tj\u{2028}k";
    assert_eq!(
        safe(text),
        "a\u{fffd}[2Jb\u{fffd}c\u{fffd}d\u{fffd}e\u{fffd}f\u{fffd}g\u{fffd}h\u{fffd}i\u{fffd}j\u{fffd}k"
    );
    assert_eq!(safe("Plain words, 3 > 2."), "Plain words, 3 > 2.");
    assert_eq!(safe("Ünïcode 日本"), "Ünïcode 日本");
}

#[test]
fn cut_ends_in_an_ellipsis_only_when_text_was_cut_and_counts_wide_characters() {
    assert_eq!(cut("short", 10), "short");
    assert_eq!(cut("exactly10!", 10), "exactly10!");
    assert_eq!(cut("eleven char", 10), "eleven ch…");
    assert_eq!(cut("日本語の題名", 7), "日本語…");
    assert_eq!(width(&cut("日本語の題名", 7)), 7);
    assert_eq!(cut("anything", 0), "");
    assert_eq!(cut("ab", 1), "…");
}

#[test]
fn pad_fills_to_the_width_and_never_returns_wider_text() {
    assert_eq!(pad("ab", 5), "ab   ");
    assert_eq!(pad("日本", 5), "日本 ");
    assert_eq!(pad("too long text", 6), "too l…");
    assert_eq!(width(&pad("日本語", 5)), 5);
}

#[test]
fn wrap_breaks_on_spaces_splits_long_words_and_keeps_lines_within_the_width() {
    assert_eq!(
        wrap("one two three four", 9),
        vec!["one two", "three", "four"]
    );
    assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    assert_eq!(wrap("日本語の題名", 5), vec!["日本", "語の", "題名"]);
    assert_eq!(wrap("", 10), Vec::<String>::new());
    assert_eq!(wrap("   ", 10), Vec::<String>::new());
    for line in wrap("a fairly long sentence with several words in it", 7) {
        assert!(width(&line) <= 7, "{line}");
    }
}
