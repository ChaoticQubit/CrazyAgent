use super::*;
use crate::harnesses::launch::PROMPT_BYTES_MAX;

const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-text-turn.yaml");
const HOME: &str = "/agent/home";
const CWD: &str = "/work/project";

fn manifest_of(text: &str) -> Manifest {
    manifest::parse(text.as_bytes()).unwrap()
}

fn config_of(text: &str) -> Config {
    Config::read(&manifest_of(text)).unwrap()
}

fn with_tools_off() -> String {
    STANDIN.replace(
        "  resume:\n",
        "  tools_off:\n    - \"--no-tools\"\n  resume:\n",
    )
}

fn with_final_json() -> String {
    format!("{STANDIN}  reply: \"result.text\"\n  session: \"session_id\"\n")
}

fn begin_with(config: Config, model: Option<&str>, tools: bool) -> TextTurn {
    let (turns, ready) =
        TextTurn::new(config, Path::new(HOME), Path::new(CWD), model, tools).unwrap();
    assert_eq!(
        ready,
        [LaunchEvent::Ready {
            native_session: None,
        }]
    );
    assert_eq!(turns.phase(), Phase::Idle);
    turns
}

fn running_with(config: Config) -> TextTurn {
    let mut turns = begin_with(config, Some("m-1"), true);
    turns.start("hello").unwrap();
    assert_eq!(turns.phase(), Phase::Running);
    turns
}

fn running() -> TextTurn {
    running_with(config_of(STANDIN))
}

fn feed(turns: &mut TextTurn, lines: &[&str]) {
    for line in lines {
        assert_eq!(turns.receive(line.as_bytes()).unwrap(), Vec::new());
    }
}

fn code<T: std::fmt::Debug>(result: Result<T, LaunchError>) -> String {
    result.unwrap_err().0
}

fn finished(success: bool) -> LaunchEvent {
    LaunchEvent::TurnFinished { success }
}

fn failed() -> Vec<LaunchEvent> {
    vec![
        LaunchEvent::TurnError {
            rate_limited: false,
        },
        finished(false),
    ]
}

fn text(reply: &str) -> LaunchEvent {
    LaunchEvent::Text { text: reply.into() }
}

/// DEC-180 Task 13: each turn gets the manifest's arguments with its prompt and model, and
/// tools off adds `piped.tools_off`; with no session id known, `piped.resume` is left out.
#[test]
fn the_stand_in_manifest_gives_each_turn_its_arguments() {
    let mut turns = begin_with(config_of(&with_tools_off()), Some("m-1"), false);

    let argv = turns.start("hi there").unwrap();

    assert_eq!(
        argv,
        [
            "--home",
            HOME,
            "--model",
            "m-1",
            "--prompt",
            "hi there",
            "--no-tools"
        ]
    );
    assert_eq!(turns.session(), None);
}

/// DEC-180 Task 13: with no reply path the reply is the whole output, without its leading
/// blank lines and trailing white space, given once the process exits; a turn with no
/// output gives only its end, and no turn ever reports usage, so usage reads "not reported".
#[test]
fn plain_output_is_the_reply_and_usage_is_never_reported() {
    let mut turns = running();
    feed(&mut turns, &["\n", "Hello there.\n", "  indented\n", "\n"]);

    let events = turns.exited(Some(0)).unwrap();
    turns.start("quiet").unwrap();
    let quiet = turns.exited(Some(0)).unwrap();

    assert_eq!(events, [text("Hello there.\n  indented"), finished(true)]);
    assert_eq!(quiet, [finished(true)]);
    assert!(
        events
            .iter()
            .chain(&quiet)
            .all(|event| !matches!(event, LaunchEvent::Usage { .. }))
    );
    assert_eq!(turns.phase(), Phase::Idle);
}

/// DEC-180 Task 13: with a reply path the reply is that field of the output's final JSON
/// object, on its last line after any log lines or as the whole output over several lines;
/// the session id field goes back through `piped.resume` on the next turn.
#[test]
fn the_reply_of_a_final_json_is_its_field_and_its_session_continues_the_next_turn() {
    let mut turns = running_with(config_of(&with_final_json()));
    feed(
        &mut turns,
        &[
            "loading model\n",
            "{\"result\":{\"text\":\"Done.\"},\"session_id\":\"s-7\"}\n",
        ],
    );

    let first = turns.exited(Some(0)).unwrap();
    let argv = turns.start("again").unwrap();
    feed(
        &mut turns,
        &["{\n", "  \"result\": {\"text\": \"Again.\"}\n", "}\n"],
    );
    let second = turns.exited(Some(0)).unwrap();

    assert_eq!(first, [text("Done."), finished(true)]);
    assert_eq!(argv[argv.len() - 2..], ["--resume", "s-7"]);
    assert_eq!(second, [text("Again."), finished(true)]);
    assert_eq!(turns.session(), Some("s-7"));
}

/// DEC-180 Task 13: output whose end is not a JSON object holding text at the reply path
/// ends the turn with an error and no reply.
#[test]
fn a_final_json_without_its_reply_text_ends_the_turn_with_an_error() {
    let config = config_of(&with_final_json());
    for output in [
        "plain words\n",
        "[1, 2]\n",
        "{\"result\":{\"text\":7}}\n",
        "{\"result\":{}}\n",
        "{\"result\":{\"text\":\"cut\"\n",
    ] {
        let mut turns = running_with(config.clone());
        feed(&mut turns, &[output]);

        assert_eq!(turns.exited(Some(0)).unwrap(), failed(), "{output}");
        assert_eq!(turns.phase(), Phase::Idle);
    }
}

/// DEC-180 Task 13: a session id in the final JSON goes back as an argument, so one that is
/// not plain fails the conversation for good.
#[test]
fn a_session_id_that_is_not_plain_fails_the_conversation_for_good() {
    let mut turns = running_with(config_of(&with_final_json()));
    feed(
        &mut turns,
        &["{\"result\":{\"text\":\"x\"},\"session_id\":\"--exec\"}\n"],
    );

    assert_eq!(code(turns.exited(Some(0))), "launch_session_id_invalid");
    assert_eq!(turns.phase(), Phase::Failed);
    assert_eq!(turns.session(), None);
    assert_eq!(code(turns.start("again")), "launch_protocol_failed");
}

/// DEC-180 Task 13: an exit code other than 0, or a signal, ends the turn with an error and
/// gives none of the output as a reply.
#[test]
fn a_non_zero_exit_or_a_signal_ends_the_turn_with_an_error_and_no_reply() {
    for code in [Some(3), None] {
        let mut turns = running();
        feed(&mut turns, &["partial answer\n"]);

        assert_eq!(turns.exited(code).unwrap(), failed(), "{code:?}");
    }
}

/// DEC-180 Task 13: output past the bound marks the turn for stopping; plain output keeps
/// what fit as its reply, cut at a character boundary, before the error, and a final JSON
/// turn gives only the error. A line too long for the reader counts the same.
#[test]
fn output_over_the_bound_stops_the_turn_and_keeps_what_fit() {
    let line = format!("{}\n", "é".repeat(511));
    let mut plain = running();
    while !plain.over() {
        feed(&mut plain, &[line.as_str()]);
    }
    feed(&mut plain, &["after the bound\n"]);
    let mut json = running_with(config_of(&with_final_json()));
    while !json.over() {
        feed(&mut json, &[line.as_str()]);
    }
    let mut skipped = running();
    skipped.skipped();

    let events = plain.exited(None).unwrap();

    assert_eq!(events.len(), 3, "{:?}", events.get(1..));
    let LaunchEvent::Text { text } = &events[0] else {
        panic!("{:?}", events[0]);
    };
    assert!(text.len() <= OUTPUT_BYTES_MAX && text.len() + line.len() > OUTPUT_BYTES_MAX);
    assert!(!text.contains("after"));
    assert_eq!(events[1..], failed());
    assert_eq!(json.exited(None).unwrap(), failed());
    assert!(skipped.over());
    assert_eq!(skipped.exited(None).unwrap(), failed());
}

/// DEC-180 Task 13: an interrupt marks the running turn once and its exit ends it
/// interrupted; with no turn running there is nothing to interrupt.
#[test]
fn an_interrupt_ends_the_turn_interrupted() {
    let mut turns = running();
    feed(&mut turns, &["half\n"]);

    turns.interrupt().unwrap();
    let again = turns.interrupt();
    let events = turns.exited(None).unwrap();

    assert_eq!(code(again), "launch_not_running");
    assert_eq!(events, [LaunchEvent::Interrupted, finished(false)]);
    assert_eq!(code(turns.interrupt()), "launch_not_running");
    assert_eq!(code(turns.exited(Some(0))), "launch_not_running");
}

/// DEC-180 Task 13: a second prompt waits for the turn to end, and a blank prompt, one with
/// a NUL byte or one over the bound is refused.
#[test]
fn a_second_prompt_waits_for_the_turn_to_end_and_a_bad_prompt_is_refused() {
    let mut turns = running();
    assert_eq!(code(turns.start("second")), "launch_busy");
    let long = "c".repeat(PROMPT_BYTES_MAX + 1);
    for prompt in [" \n", "a\0b", long.as_str()] {
        assert_eq!(code(turns.start(prompt)), "launch_prompt_invalid");
    }
    turns.exited(Some(0)).unwrap();
    assert!(turns.start("second").is_ok());
}

/// DEC-180 Task 13: the starting inputs and the placeholders are checked as on jsonl-turn:
/// tools off with no `piped.tools_off`, a home that is not absolute, a model that is not
/// plain, `{model}` with no model, `{session}` with no id outside `piped.resume`, and the
/// account values this driver does not hand out.
#[test]
fn the_starting_inputs_and_placeholders_are_checked() {
    let new = |config: Config, home: &str, model: Option<&str>, tools: bool| {
        TextTurn::new(config, Path::new(home), Path::new(CWD), model, tools)
    };
    assert_eq!(
        code(new(config_of(STANDIN), HOME, None, false)),
        "launch_tools_off_unsupported"
    );
    assert_eq!(
        code(new(config_of(STANDIN), "agent/home", None, true)),
        "launch_account_home_unsafe"
    );
    assert_eq!(
        code(new(config_of(STANDIN), HOME, Some("a\nb"), true)),
        "launch_model_unsupported"
    );

    let mut no_model = begin_with(config_of(STANDIN), None, true);
    let relay = config_of(&STANDIN.replace("\"{home}\"\n", "\"{relay_url}\"\n"));
    let mut relayed = begin_with(relay, Some("m-1"), true);
    let session = config_of(&STANDIN.replace("    - \"--home\"\n", "    - \"{session}\"\n"));
    let mut unknown = begin_with(session, Some("m-1"), true);

    assert_eq!(code(no_model.start("x")), "launch_model_unsupported");
    assert_eq!(no_model.phase(), Phase::Idle);
    assert_eq!(code(relayed.start("x")), "launch_harness_unsupported");
    assert_eq!(code(unknown.start("x")), "launch_session_id_invalid");
}

/// DEC-180 Task 13: each refused text-turn setting names its setting.
#[test]
fn each_refused_turn_setting_names_its_setting() {
    let refused = |text: &str| {
        let refusal = Config::read(&manifest_of(text)).unwrap_err();
        (refusal.code, refusal.message)
    };
    let json = with_final_json();
    let cases = [
        (
            STANDIN.replace("driver: \"text-turn\"", "driver: \"jsonl-turn\""),
            manifest::KIND,
            "piped.driver: this harness does not run on the text-turn driver.",
        ),
        (
            STANDIN.replace("  argv:\n", "  other:\n"),
            manifest::MISSING,
            "piped.argv: this setting is needed.",
        ),
        (
            STANDIN.replace("    - \"{prompt}\"\n", "    - \"hello\"\n"),
            manifest::VALUE,
            "piped.argv: {prompt} is needed, so each turn's prompt reaches the harness.",
        ),
        (
            STANDIN.replace("    - \"{session}\"\n", "    - \"last\"\n"),
            manifest::VALUE,
            "piped.resume: {session} is needed, so a turn continues its session.",
        ),
        (
            json.replace("\"result.text\"", "\"result..text\""),
            manifest::VALUE,
            "piped.reply: a path is 1 to 8 parts of letters, digits, _ or -, joined by dots.",
        ),
        (
            json.replace("  reply: \"result.text\"\n", ""),
            manifest::VALUE,
            "piped.session: a session id is read only from the final JSON a reply path names.",
        ),
        (
            json.replace("\"result.text\"", "3"),
            manifest::TYPE,
            "piped.reply: this must be text in quotes.",
        ),
    ];
    for (text, code, message) in cases {
        assert_eq!(refused(&text), (code, message.to_owned()), "{message}");
    }
}
