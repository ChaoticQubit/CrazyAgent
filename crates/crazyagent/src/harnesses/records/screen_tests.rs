use super::*;

fn ended(reply: &str) -> Vec<TranscriptEvent> {
    vec![
        TranscriptEvent::Text { text: reply.into() },
        TranscriptEvent::TurnFinished { success: true },
    ]
}

/// Plan Task 15: a turn ends once the screen has changed since the prompt, then stayed the
/// same for the quiet time with an empty prompt mark as its last line; the reply is the text
/// between the typed prompt line and that mark, and the turn ends once.
#[test]
fn a_turn_ends_once_the_screen_is_quiet_on_the_prompt_mark() {
    let start = Instant::now();
    let mut screen = ScreenReader::new("> ");
    let done = b"> earlier\nold\n> hello\nHi there.\nBye.\n>\n\n\n";

    let idle = screen.observe(b"> earlier\nold\n> \n", start);
    let started = screen.turn_started(start);
    let typed = screen.observe(b"> earlier\nold\n> hello\n", start + QUIET);
    let replied = screen.observe(done, start + QUIET * 2);
    let early = screen.observe(done, start + QUIET * 3 - Duration::from_millis(1));
    let quiet = screen.observe(done, start + QUIET * 3);
    let after = screen.observe(done, start + QUIET * 9);

    assert_eq!(idle, Vec::new());
    assert_eq!(started, vec![TranscriptEvent::TurnStarted]);
    assert_eq!(typed, Vec::new());
    assert_eq!(replied, Vec::new());
    assert_eq!(early, Vec::new());
    assert_eq!(quiet, ended("Hi there.\nBye."));
    assert_eq!(after, Vec::new());
}

/// Plan Task 15: a screen that goes quiet with text after the mark, such as the typed prompt
/// while the model thinks, or with no mark at all, keeps the turn open.
#[test]
fn a_quiet_screen_without_an_empty_prompt_mark_keeps_the_turn_open() {
    let start = Instant::now();
    let mut screen = ScreenReader::new("> ");
    let started = screen.turn_started(start);

    screen.observe(b"> \n", start);
    let typed = screen.observe(b"> hello\n", start + QUIET);
    let thinking = screen.observe(b"> hello\n", start + QUIET * 10);
    let working = screen.observe(b"> hello\nthinking...\n", start + QUIET * 11);
    let still = screen.observe(b"> hello\nthinking...\n", start + QUIET * 20);

    assert_eq!(started, vec![TranscriptEvent::TurnStarted]);
    assert_eq!(typed, Vec::new());
    assert_eq!(thinking, Vec::new());
    assert_eq!(working, Vec::new());
    assert_eq!(still, Vec::new());
}

/// Plan Task 15: the screen the turn started on, already on an empty mark, never ends the
/// turn however long it stays; the turn ends only after the screen has changed.
#[test]
fn the_turn_does_not_end_before_the_screen_changes() {
    let start = Instant::now();
    let mut screen = ScreenReader::new("> ");
    let before = b"> one\nold reply\n> \n";
    let after = b"> one\nold reply\n> two\nnew reply\n> \n";

    screen.observe(before, start);
    screen.turn_started(start);
    let stale = screen.observe(before, start + QUIET * 10);
    let changed = screen.observe(after, start + QUIET * 11);
    let quiet = screen.observe(after, start + QUIET * 12);

    assert_eq!(stale, Vec::new());
    assert_eq!(changed, Vec::new());
    assert_eq!(quiet, ended("new reply"));
}

/// Plan Task 15: a turn's reply text keeps to its bound, cut at a character boundary.
#[test]
fn the_reply_keeps_to_its_text_bound() {
    let start = Instant::now();
    let mut screen = ScreenReader::new("> ");
    let line = format!("{}\n", "é".repeat(40));
    let lines = line.repeat(SCREEN_TEXT_BYTES_MAX / line.len() + 8);
    let full = format!("> go\n{lines}> \n");

    screen.turn_started(start);
    screen.observe(full.as_bytes(), start);
    let events = screen.observe(full.as_bytes(), start + QUIET);

    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[1], TranscriptEvent::TurnFinished { success: true });
    let TranscriptEvent::Text { text } = &events[0] else {
        panic!("{:?}", events[0]);
    };
    assert!(text.len() <= SCREEN_TEXT_BYTES_MAX, "{}", text.len());
    assert!(text.len() + 2 >= SCREEN_TEXT_BYTES_MAX, "{}", text.len());
    assert!(text.starts_with('é'));
}

/// Plan Task 15: a capture longer than the screen bound is read from its end, so the turn
/// still ends on its last lines.
#[test]
fn a_screen_past_its_byte_bound_is_read_from_its_end() {
    let start = Instant::now();
    let mut screen = ScreenReader::new("> ");
    let full = format!("{}> go\nreply\n> \n", "x\n".repeat(SCREEN_BYTES_MAX));

    screen.turn_started(start);
    screen.observe(full.as_bytes(), start);
    let events = screen.observe(full.as_bytes(), start + QUIET);

    assert_eq!(events, ended("reply"));
}

/// Plan Task 15: with no turn open, the screen gives no facts, whatever it shows.
#[test]
fn no_open_turn_gives_no_facts() {
    let start = Instant::now();
    let mut screen = ScreenReader::new("> ");

    let first = screen.observe(b"> hi\nreply\n> \n", start);
    let quiet = screen.observe(b"> hi\nreply\n> \n", start + QUIET * 2);

    assert_eq!(first, Vec::new());
    assert_eq!(quiet, Vec::new());
}
