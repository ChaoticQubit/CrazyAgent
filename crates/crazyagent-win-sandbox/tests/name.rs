use crazyagent_win_sandbox::name;

/// V244-N6-W1-SEC-L1 (#751, QUESTION-1510): the one name rule refuses, on every system, an
/// empty name, `.`, `..`, a separator, a colon, NUL and other controls, Windows' other
/// invalid characters, a trailing dot or space, every device name with or without an
/// extension in any case, and a name over 255 UTF-16 units, while a space inside a name, a
/// non-ASCII letter and a 255-unit name pass.
#[test]
fn the_name_rule_refuses_every_unsafe_spelling_and_keeps_plain_ones() {
    let long = "a".repeat(256);
    let refused = [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a:b",
        "x::$DATA",
        "a\0b",
        "a\u{1}b",
        "a\u{7f}b",
        "a\u{9f}b",
        "a<b",
        "a>b",
        "a\"b",
        "a|b",
        "a?b",
        "a*b",
        "a.",
        "a ",
        "CON",
        "con.txt",
        "Prn",
        "AUX.json",
        "nul",
        "COM1",
        "com0.log",
        "LPT9",
        "COM\u{b9}",
        "lpt\u{b2}.txt",
        "CONIN$",
        "conout$",
        "CON .txt",
        long.as_str(),
    ];
    for text in refused {
        assert!(name::refused(text), "{text:?}");
    }
    let widest = "a".repeat(255);
    let widest_units = "\u{e9}".repeat(255);
    for text in [
        "a b",
        "skill-\u{fc}.md",
        "COM10",
        "console",
        "nul-free",
        "a.b",
        widest.as_str(),
        widest_units.as_str(),
    ] {
        assert!(!name::refused(text), "{text:?}");
    }
}
