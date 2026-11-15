use super::*;

const APPEND: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-append.yaml");
const REWRITE: &str = include_str!("../../../tests/fixtures/harness/standin-json-rewrite.yaml");
const TURN: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-turn.yaml");
const HOME: &str = "/agent/home";
const SESSION: &str = "s-1";
const CWD: &str = "/work/project";
const ARGV_LINES: &str = concat!(
    "  argv:\n",
    "    - \"--home\"\n",
    "    - \"{home}\"\n",
    "    - \"--session\"\n",
    "    - \"{session}\"\n",
);
const RESUME_LINES: &str = concat!(
    "  resume:\n",
    "    - \"--home\"\n",
    "    - \"{home}\"\n",
    "    - \"--resume\"\n",
    "    - \"{session}\"\n",
);
const SESSION_FLAG: &str = "    - \"--session\"\n";
const RESUMED_SESSION: &str = "    - \"--resume\"\n    - \"{session}\"\n";
const SUBMIT_LINE: &str = "  submit: \"enter\"\n";
const INTERRUPT_LINE: &str = "  interrupt: \"ctrl-c\"\n";
const SEEN_LINE: &str = "      seen: \"Trust this folder?\"\n";
const PRESS_LINE: &str = "      press: \"enter\"\n";
const READER_LINE: &str = "    reader: \"jsonl-append\"\n";
const RULES_LINE: &str = "    rules: \"standin-record\"\n";

fn settings_of(text: &str) -> Result<TerminalSettings, Refused> {
    TerminalSettings::read(&manifest::parse(text.as_bytes()).unwrap())
}

fn read_ok(text: &str) -> TerminalSettings {
    let read = settings_of(text);
    assert!(read.is_ok(), "{read:?}");
    read.unwrap()
}

/// Takes one piece out of a manifest text that holds it once. Inputs: the text and the
/// piece. Output: the text without it.
fn cut(text: &str, piece: &str) -> String {
    assert_eq!(text.matches(piece).count(), 1, "{piece}");
    text.replace(piece, "")
}

/// Puts other text where a manifest text holds a piece once. Inputs: the text, the piece
/// and its replacement. Output: the new text.
fn swap(text: &str, piece: &str, with: &str) -> String {
    assert_eq!(text.matches(piece).count(), 1, "{piece}");
    text.replace(piece, with)
}

/// Writes one more first-screen step that presses Tab. Input: its number. Output: its
/// manifest lines.
fn step(number: usize) -> String {
    let press = "      press: \"tab\"\n";
    format!("    step{number}:\n      seen: \"s{number}\"\n{press}")
}

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

/// DEC-180 Task 16: each stand-in manifest gives its keys, its one first-screen step and
/// the record kind and rules its turns are read back by.
#[test]
fn each_stand_in_manifest_gives_its_terminal_settings() {
    let kinds = [
        (APPEND, RecordKind::JsonlAppend),
        (REWRITE, RecordKind::JsonRewrite),
    ];
    for (text, record) in kinds {
        let settings = read_ok(text);
        assert_eq!(settings.record(), record);
        assert_eq!(settings.submit(), Key::Enter);
        assert_eq!(settings.interrupt(), Key::CtrlC);
        let trust = Step {
            seen: "Trust this folder?".to_owned(),
            press: Key::Enter,
        };
        assert_eq!(settings.first_screen(), [trust]);
        assert_eq!(settings.rules().len(), 6);
        assert!(settings.harness().starts_with("standin-"));
    }
}

/// DEC-180 Task 16: a fresh run gets `terminal.argv` and a resumed one the whole
/// `terminal.resume` list, each placeholder filled once and never inside a filled value.
#[test]
fn a_run_fills_its_arguments_once_and_a_resumed_run_takes_the_resume_list() {
    let settings = read_ok(APPEND);
    let fresh = settings.arguments(false, HOME, SESSION, CWD, None);
    assert_eq!(fresh, Ok(owned(&["--home", HOME, "--session", SESSION])));
    let resumed = settings.arguments(true, HOME, SESSION, CWD, None);
    assert_eq!(resumed, Ok(owned(&["--home", HOME, "--resume", SESSION])));
    let odd = settings.arguments(false, "/h/{session}", SESSION, CWD, None);
    let kept = owned(&["--home", "/h/{session}", "--session", SESSION]);
    assert_eq!(odd, Ok(kept));
}

#[test]
fn a_model_argument_without_a_model_and_a_resume_without_a_resume_list_are_refused() {
    let lines = "    - \"--model\"\n    - \"{model}\"\n    - \"--session\"\n";
    let settings = read_ok(&swap(APPEND, SESSION_FLAG, lines));
    let unmodelled = settings.arguments(false, HOME, SESSION, CWD, None);
    assert_eq!(unmodelled, Err(failure("launch_model_unsupported")));
    let modelled = settings.arguments(false, HOME, SESSION, CWD, Some("m-1"));
    let filled = owned(&["--home", HOME, "--model", "m-1", "--session", SESSION]);
    assert_eq!(modelled, Ok(filled));
    let unresumable = read_ok(&cut(APPEND, RESUME_LINES));
    let resumed = unresumable.arguments(true, HOME, SESSION, CWD, None);
    assert_eq!(resumed, Err(failure("launch_resume_unavailable")));
}

#[test]
fn each_key_name_presses_its_own_key_and_the_keys_default_to_enter_and_escape() {
    let keys = [
        ("enter", "\r"),
        ("escape", "\x1b"),
        ("ctrl-c", "\x03"),
        ("tab", "\t"),
    ];
    for (name, bytes) in keys {
        let pressed = Key::of(name).map(Key::bytes);
        assert_eq!(pressed, Some(bytes.as_bytes()), "{name}");
    }
    for name in ["Enter", "f1", "", "ctrl-d", "\r"] {
        assert_eq!(Key::of(name), None, "{name}");
    }
    let plain = read_ok(&cut(&cut(APPEND, SUBMIT_LINE), INTERRUPT_LINE));
    assert_eq!(plain.submit(), Key::Enter);
    assert_eq!(plain.interrupt(), Key::Escape);
}

/// DEC-180 Task 16: every terminal setting out of its bounds is refused with one sentence
/// that starts with the setting's place, so the owner knows what to fix.
#[test]
fn each_terminal_setting_out_of_its_bounds_is_refused_with_the_setting_named() {
    let long = format!("      seen: \"{}\"\n", "x".repeat(SEEN_BYTES_MAX + 1));
    let more: String = (1..=STEPS_MAX).map(step).collect();
    let prompt = "    - \"--prompt\"\n    - \"{prompt}\"\n    - \"--session\"\n";
    let waited = format!("{PRESS_LINE}      wait: \"5\"\n");
    let last = "    - \"--resume\"\n    - \"last\"\n";
    let cases = [
        (cut(APPEND, ARGV_LINES), manifest::MISSING, "terminal.argv:"),
        (
            swap(APPEND, SESSION_FLAG, prompt),
            manifest::PLACEHOLDER,
            "terminal.argv:",
        ),
        (
            swap(APPEND, RESUMED_SESSION, last),
            manifest::VALUE,
            "terminal.resume:",
        ),
        (
            swap(APPEND, SUBMIT_LINE, "  submit: \"f1\"\n"),
            manifest::VALUE,
            "terminal.submit:",
        ),
        (
            swap(APPEND, SUBMIT_LINE, "  keys: \"enter\"\n"),
            manifest::UNKNOWN_KEY,
            "terminal.keys:",
        ),
        (
            cut(APPEND, PRESS_LINE),
            manifest::MISSING,
            "terminal.first_screen.trust.press:",
        ),
        (
            swap(APPEND, SEEN_LINE, &long),
            manifest::VALUE,
            "terminal.first_screen.trust.seen:",
        ),
        (
            swap(APPEND, PRESS_LINE, &waited),
            manifest::UNKNOWN_KEY,
            "terminal.first_screen.trust.wait:",
        ),
        (
            swap(APPEND, PRESS_LINE, &format!("{PRESS_LINE}{more}")),
            manifest::VALUE,
            "terminal.first_screen:",
        ),
        (
            swap(APPEND, READER_LINE, "    reader: \"sqlite-poll\"\n"),
            manifest::KIND,
            "terminal.record.reader:",
        ),
        (
            cut(APPEND, RULES_LINE),
            manifest::MISSING,
            "terminal.record.rules:",
        ),
        (
            swap(APPEND, RULES_LINE, "    rules: \"none\"\n"),
            manifest::MISSING,
            "rules.none:",
        ),
        (TURN.to_owned(), manifest::MISSING, "terminal:"),
    ];
    for (text, code, place) in cases {
        let refused = settings_of(&text).err();
        let named = refused.as_ref().map(|refused| refused.code);
        assert_eq!(named, Some(code), "{place} {refused:?}");
        let message = refused.map(|refused| refused.message).unwrap_or_default();
        assert!(message.starts_with(place), "{place} {message}");
    }
}
