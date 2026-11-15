use super::*;
use crate::adapters::{FIELD_BYTES_MAX, LINE_BYTES_MAX};
use crate::harnesses::launch::PROMPT_BYTES_MAX;
use crate::usage::capture::Tokens;
use serde_json::{Value, json};
use std::path::Path;

const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-turn.yaml");
const HOME: &str = "/agent/home";
const CWD: &str = "/work/project";

fn manifest_of(text: &str) -> Manifest {
    manifest::parse(text.as_bytes()).unwrap()
}

fn config_of(text: &str) -> Config {
    Config::read(&manifest_of(text)).unwrap()
}

fn begin_with(config: Config, model: Option<&str>, tools: bool, resume: Option<&str>) -> JsonlTurn {
    let (turns, ready) = JsonlTurn::new(
        config,
        Path::new(HOME),
        Path::new(CWD),
        model,
        tools,
        resume,
    )
    .unwrap();
    assert_eq!(
        ready,
        [LaunchEvent::Ready {
            native_session: resume.map(str::to_owned),
        }]
    );
    assert_eq!(turns.phase(), Phase::Idle);
    turns
}

fn begin(model: Option<&str>, tools: bool, resume: Option<&str>) -> JsonlTurn {
    begin_with(config_of(STANDIN), model, tools, resume)
}

fn line(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn events_of(turns: &mut JsonlTurn, value: Value) -> Vec<LaunchEvent> {
    turns.receive(&line(value)).unwrap()
}

fn code<T: std::fmt::Debug>(result: Result<T, LaunchError>) -> String {
    result.unwrap_err().0
}

fn running(model: Option<&str>) -> JsonlTurn {
    let mut turns = begin(model, true, None);
    turns.start("hello").unwrap();
    assert_eq!(turns.phase(), Phase::Running);
    turns
}

fn finished(success: bool) -> LaunchEvent {
    LaunchEvent::TurnFinished { success }
}

/// DEC-180 Task 11: each turn gets the manifest's arguments with its prompt, and the session
/// id a turn reports is passed to the next turn through `piped.resume`; a conversation that
/// starts from a session resumes it on its first turn.
#[test]
fn the_stand_in_manifest_gives_each_turn_its_arguments_and_resumes_the_reported_session() {
    let mut turns = begin(Some("m-1"), true, None);
    assert_eq!(
        turns.start("hi there").unwrap(),
        ["--home", HOME, "--model", "m-1", "--prompt", "hi there"]
    );
    assert!(events_of(&mut turns, json!({"type": "session", "id": "s-9"})).is_empty());
    assert_eq!(turns.session(), Some("s-9"));
    assert_eq!(turns.exited(Some(0)).unwrap(), [finished(true)]);
    assert_eq!(turns.phase(), Phase::Idle);
    assert_eq!(
        turns.start("again").unwrap(),
        [
            "--home", HOME, "--model", "m-1", "--prompt", "again", "--resume", "s-9"
        ]
    );

    let mut resumed = begin(Some("m-1"), true, Some("s-1"));
    let argv = resumed.start("go on").unwrap();
    assert_eq!(argv[argv.len() - 2..], ["--resume", "s-1"]);
}

/// DEC-180 Task 11: every fact a rule reports gives its event: text, tool use, usage, the
/// finished turn's tokens and a turn error with its rate limit; ready, turn started, waiting
/// and exited give none, and an interrupted fact ends the turn as interrupted at its exit.
#[test]
fn each_fact_from_a_rule_gives_its_event() {
    let mut turns = running(Some("m-1"));
    let lines = [
        (json!({"type": "session", "id": "s-1"}), Vec::new()),
        (json!({"type": "started"}), Vec::new()),
        (
            json!({"type": "text", "delta": "hi"}),
            vec![LaunchEvent::Text { text: "hi".into() }],
        ),
        (
            json!({"type": "tool", "name": "Read"}),
            vec![LaunchEvent::ToolUse {
                name: "Read".into(),
            }],
        ),
        (json!({"type": "waiting"}), Vec::new()),
        (
            json!({"type": "usage", "usage": {"input": 10, "output": 5}}),
            vec![LaunchEvent::Usage {
                tokens: Tokens {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                },
            }],
        ),
        (
            json!({"type": "done", "ok": true, "usage": {"input": 3, "output": 4}}),
            vec![LaunchEvent::Usage {
                tokens: Tokens {
                    input: 3,
                    output: 4,
                    cache_read: 0,
                    cache_write: 0,
                },
            }],
        ),
        (json!({"type": "exit"}), Vec::new()),
    ];
    for (value, events) in lines {
        assert_eq!(events_of(&mut turns, value.clone()), events, "{value}");
    }
    assert_eq!(turns.exited(Some(0)).unwrap(), [finished(true)]);

    let mut turns = running(None);
    assert_eq!(
        events_of(
            &mut turns,
            json!({"type": "error", "message": "Rate limit reached"})
        ),
        [LaunchEvent::TurnError { rate_limited: true }]
    );
    assert!(events_of(&mut turns, json!({"type": "error", "message": "again"})).is_empty());
    assert_eq!(turns.exited(Some(1)).unwrap(), [finished(false)]);

    let mut turns = running(None);
    assert!(events_of(&mut turns, json!({"type": "stopped"})).is_empty());
    assert_eq!(
        turns.exited(Some(0)).unwrap(),
        [LaunchEvent::Interrupted, finished(false)]
    );
}

/// DEC-180 Task 11: a line no rule matches, a line that is not JSON, JSON that is not one
/// object, a line over `LINE_BYTES_MAX` and a line outside a turn give nothing; a CRLF line
/// is read, and a long text is clipped to `FIELD_BYTES_MAX`.
#[test]
fn a_line_no_rule_matches_a_non_json_line_and_a_line_over_the_bound_give_nothing() {
    let mut idle = begin(None, true, None);
    assert!(
        idle.receive(&line(json!({"type": "text", "delta": "early"})))
            .unwrap()
            .is_empty()
    );

    let mut turns = running(None);
    let over = format!(
        "{{\"type\":\"text\",\"delta\":\"{}\"}}\n",
        "a".repeat(LINE_BYTES_MAX)
    );
    let lines: [&[u8]; 6] = [
        b"{\"type\":\"debug\"}\n",
        b"progress 50%\n",
        b"[1,2]\n",
        b"\"text\"\n",
        b"{\"type\":\"text\"\n",
        over.as_bytes(),
    ];
    for bytes in lines {
        assert!(turns.receive(bytes).unwrap().is_empty());
    }
    assert_eq!(
        turns
            .receive(b"{\"type\":\"text\",\"delta\":\"crlf\"}\r\n")
            .unwrap(),
        [LaunchEvent::Text {
            text: "crlf".into()
        }]
    );
    let long = "b".repeat(FIELD_BYTES_MAX + 1);
    let clipped = events_of(&mut turns, json!({"type": "text", "delta": long}));
    let [LaunchEvent::Text { text }] = clipped.as_slice() else {
        panic!("{clipped:?}");
    };
    assert!(text.len() <= FIELD_BYTES_MAX && text.ends_with("more bytes not shown]"));
    assert_eq!(turns.phase(), Phase::Running);
}

/// DEC-180 Task 11: the turn ends when its process exits, with exactly one `TurnFinished`:
/// an exit code other than 0 or a signal gives a turn error first, a turn error a rule
/// already gave is not given twice, and a clean exit takes the success a rule read.
#[test]
fn the_exit_code_ends_the_turn_with_exactly_one_turn_finished() {
    let failed = [
        LaunchEvent::TurnError {
            rate_limited: false,
        },
        finished(false),
    ];
    let mut turns = running(None);
    assert_eq!(turns.exited(Some(3)).unwrap(), failed);
    turns.start("again").unwrap();
    assert_eq!(turns.exited(None).unwrap(), failed);
    turns.start("again").unwrap();
    assert!(events_of(&mut turns, json!({"type": "done", "ok": false})).is_empty());
    assert_eq!(turns.exited(Some(0)).unwrap(), [finished(false)]);
    turns.start("again").unwrap();
    assert_eq!(turns.exited(Some(0)).unwrap(), [finished(true)]);
    turns.start("again").unwrap();
    events_of(&mut turns, json!({"type": "error", "message": "boom"}));
    assert_eq!(turns.exited(Some(0)).unwrap(), [finished(false)]);
    assert_eq!(code(turns.exited(Some(0))), "launch_not_running");
}

/// DEC-180 Task 11: an interrupt marks the running turn once, and the turn's exit, by the
/// signal that stopped it, ends it as interrupted; with no turn there is nothing to stop.
#[test]
fn an_interrupt_marks_the_turn_and_its_exit_ends_it_interrupted() {
    let mut turns = begin(None, true, None);
    assert_eq!(code(turns.interrupt()), "launch_not_running");
    turns.start("long job").unwrap();
    turns.interrupt().unwrap();
    assert_eq!(code(turns.interrupt()), "launch_not_running");
    assert_eq!(
        turns.exited(None).unwrap(),
        [LaunchEvent::Interrupted, finished(false)]
    );
    assert_eq!(code(turns.interrupt()), "launch_not_running");
}

/// DEC-180 Task 11: one turn runs at a time, and a prompt that is empty, holds a NUL byte or
/// is over `PROMPT_BYTES_MAX` is refused before anything starts.
#[test]
fn a_second_prompt_waits_for_the_turn_to_end_and_a_bad_prompt_is_refused() {
    let mut turns = running(None);
    assert_eq!(code(turns.start("second")), "launch_busy");
    let long = "c".repeat(PROMPT_BYTES_MAX + 1);
    for prompt in [" \n", "a\0b", long.as_str()] {
        assert_eq!(code(turns.start(prompt)), "launch_prompt_invalid");
    }
    turns.exited(Some(0)).unwrap();
    assert!(turns.start("second").is_ok());
}

/// DEC-180 Task 11: each placeholder is filled once and never inside a filled value; tools
/// off adds `piped.tools_off`; a value the driver cannot give is refused: `{model}` with no
/// model, `{session}` with no id outside `piped.resume`, and the account values it does not
/// hand out yet.
#[test]
fn placeholders_are_filled_once_and_unknown_values_are_refused() {
    let mut turns = begin(Some("m-1"), true, None);
    let prompt = "say {model} and {session} at {home} {\"a\": 1}";
    let argv = turns.start(prompt).unwrap();
    assert_eq!(argv[5], prompt);

    let mut off = begin(Some("m-1"), false, None);
    assert_eq!(off.start("x").unwrap().last().unwrap(), "--no-tools");

    let mut no_model = begin(None, true, None);
    assert_eq!(code(no_model.start("x")), "launch_model_unsupported");
    assert_eq!(no_model.phase(), Phase::Idle);

    let relay = config_of(&STANDIN.replace("\"{home}\"\n", "\"{relay_url}\"\n"));
    let mut relayed = begin_with(relay, Some("m-1"), true, None);
    assert_eq!(code(relayed.start("x")), "launch_harness_unsupported");

    let session = config_of(&STANDIN.replace("    - \"--home\"\n", "    - \"{session}\"\n"));
    let mut unknown = begin_with(session, Some("m-1"), true, None);
    assert_eq!(code(unknown.start("x")), "launch_session_id_invalid");

    let no_switch = config_of(&STANDIN.replace("  tools_off:\n    - \"--no-tools\"\n", ""));
    let refused = JsonlTurn::new(
        no_switch,
        Path::new(HOME),
        Path::new(CWD),
        None,
        false,
        None,
    );
    assert_eq!(code(refused), "launch_tools_off_unsupported");
}

/// DEC-180 Task 11: a session id the harness reports goes back as an argument, so one that
/// is not plain fails the conversation for good; the starting inputs are checked the same.
#[test]
fn a_session_id_that_is_not_plain_fails_the_conversation_for_good() {
    let long = "s".repeat(SESSION_BYTES_MAX + 1);
    for id in ["-rf", "", "a\nb", long.as_str()] {
        let mut turns = running(None);
        let bad = turns.receive(&line(json!({"type": "session", "id": id})));
        assert_eq!(code(bad), "launch_session_id_invalid", "{id:?}");
        assert_eq!(turns.phase(), Phase::Failed);
        assert_eq!(
            code(turns.receive(&line(json!({"type": "started"})))),
            "launch_protocol_failed"
        );
        assert_eq!(code(turns.start("x")), "launch_protocol_failed");
        assert_eq!(turns.session(), None);
    }
    let start = |home: &str, cwd: &str, model: Option<&str>, resume: Option<&str>| {
        JsonlTurn::new(
            config_of(STANDIN),
            Path::new(home),
            Path::new(cwd),
            model,
            true,
            resume,
        )
    };
    assert_eq!(
        code(start(HOME, CWD, None, Some("-x"))),
        "launch_session_id_invalid"
    );
    assert_eq!(
        code(start(HOME, CWD, Some(""), None)),
        "launch_model_unsupported"
    );
    assert_eq!(
        code(start(HOME, CWD, Some("m\n"), None)),
        "launch_model_unsupported"
    );
    assert_eq!(
        code(start(HOME, "work", None, None)),
        "launch_working_dir_unavailable"
    );
    assert_eq!(
        code(start("home", CWD, None, None)),
        "launch_account_home_unsafe"
    );
}

/// DEC-180 Task 11: every refused jsonl-turn setting names the setting in one plain sentence,
/// with the manifest refusal codes.
#[test]
fn each_refused_turn_setting_names_its_setting() {
    let refused = |text: &str| {
        let refusal = Config::read(&manifest_of(text)).unwrap_err();
        (refusal.code, refusal.message)
    };
    let cases = [
        (
            STANDIN.replace("driver: \"jsonl-turn\"", "driver: \"text-turn\""),
            manifest::KIND,
            "piped.driver: this harness does not run on the jsonl-turn driver.",
        ),
        (
            STANDIN.replace("  rules: \"standin-events\"\n", ""),
            manifest::MISSING,
            "piped.rules: this setting is needed.",
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
            STANDIN.replace(
                "  rules: \"standin-events\"\n",
                "  rules: \"other-events\"\n",
            ),
            manifest::MISSING,
            "rules.other-events: this setting is needed.",
        ),
        (
            STANDIN.replace("  rules: \"standin-events\"\n", "  rules: 3\n"),
            manifest::TYPE,
            "piped.rules: this must be text in quotes.",
        ),
    ];
    for (text, code, message) in cases {
        assert_eq!(refused(&text), (code, message.to_owned()), "{message}");
    }
}
