use super::*;

/// SECAUDIT coord-delivery-raw-body-forges-owner-line r2: each of the seven line breaks opens
/// a quoted line, in the whole quote and in the quote after a first line.
#[test]
fn each_of_the_seven_breaks_opens_a_quoted_line() {
    for line_break in [
        '\n', '\r', '\u{0b}', '\u{0c}', '\u{85}', '\u{2028}', '\u{2029}',
    ] {
        let text = format!("a{line_break}b");
        assert_eq!(quoted_lines(&text), format!("> a{line_break}> b"));
        assert_eq!(quoted_after_first_line(&text), format!("a{line_break}> b"));
    }
}
