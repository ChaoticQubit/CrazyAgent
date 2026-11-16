use super::*;
use crate::adapters::LINE_BYTES_MAX;
use crate::adapters::codex::FRAME_BYTES_MAX;
use crate::harnesses::drivers::jsonl_turn::SESSION_BYTES_MAX;
use crate::harnesses::launch::PROMPT_BYTES_MAX;
use crate::usage::capture::Tokens;
use serde_json::{Value, json};
use std::path::Path;

const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-rpc.yaml");
const HOME: &str = "/agent/home";
const CWD: &str = "/work/project";
const ABORT: &[u8] = b"{\"type\":\"abort\"}\n";
const PROMPT_LINE: &str = "  prompt: '{\"type\":\"prompt\",\"message\":\"{prompt}\"}'\n";
const INTERRUPT_LINE: &str = "  interrupt: '{\"type\":\"abort\"}'\n";

fn manifest_of(text: &str) -> Manifest {
    manifest::parse(text.as_bytes()).unwrap()
}

fn config_of(text: &str) -> Config {
    Config::read(&manifest_of(text)).unwrap()
}

fn begin_with(config: Config, tools: bool, resume: Option<&str>) -> JsonlRpc {
    let (rpc, ready) = JsonlRpc::new(
        config,
        Path::new(HOME),
        Path::new(CWD),
        Some("m-1"),
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
    assert_eq!(rpc.phase(), Phase::Idle);
    rpc
}

fn begin(tools: bool) -> JsonlRpc {
    begin_with(config_of(STANDIN), tools, None)
}

fn running(tools: bool) -> JsonlRpc {
    let mut rpc = begin(tools);
    rpc.prompt("hello").unwrap();
    assert_eq!(rpc.phase(), Phase::Running);
    rpc
}

fn line(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

/// Feeds one line that must write nothing back. Inputs: the conversation and the line.
/// Output: the events it gives.
fn events_of(rpc: &mut JsonlRpc, value: Value) -> Vec<LaunchEvent> {
    let (events, writes) = rpc.receive(&line(value)).unwrap();
    assert!(writes.is_empty(), "{writes:?}");
    events
}

fn code<T: std::fmt::Debug>(result: Result<T, LaunchError>) -> String {
    result.unwrap_err().0
}

fn finished(success: bool) -> LaunchEvent {
    LaunchEvent::TurnFinished { success }
}

fn text(text: &str) -> LaunchEvent {
    LaunchEvent::Text { text: text.into() }
}

/// The prompt one frame carries, after checking the frame is one JSON object line of type
/// `prompt`. Input: the frame. Output: its `message`.
fn message(frame: &[u8]) -> String {
    let body = std::str::from_utf8(frame)
        .unwrap()
        .strip_suffix('\n')
        .unwrap();
    assert!(!body.contains('\n'));
    let value: Value = serde_json::from_str(body).unwrap();
    assert_eq!(value["type"], "prompt");
    value["message"].as_str().unwrap().to_owned()
}

/// DEC-180 Task 12: the one process gets the manifest's arguments; a session to continue
/// adds `piped.resume`, tools off adds `piped.tools_off`, both before a `--` entry, and a
/// `{session}` in `piped.argv` takes the session to continue or is refused without one.
#[test]
fn the_stand_in_manifest_gives_the_process_its_arguments_and_continues_a_session() {
    let fresh = begin(true);
    assert_eq!(fresh.argv(), ["--home", HOME, "--model", "m-1"]);
    assert_eq!(fresh.session(), None);

    let resumed = begin_with(config_of(STANDIN), false, Some("s-9"));
    assert_eq!(
        resumed.argv(),
        [
            "--home",
            HOME,
            "--model",
            "m-1",
            "--session",
            "s-9",
            "--no-tools"
        ]
    );
    assert_eq!(resumed.session(), Some("s-9"));

    let ended = STANDIN.replace(
        "    - \"{model}\"\n",
        "    - \"{model}\"\n    - \"--\"\n    - \"{cwd}\"\n",
    );
    let split = begin_with(config_of(&ended), false, Some("s-9"));
    assert_eq!(
        split.argv(),
        [
            "--home",
            HOME,
            "--model",
            "m-1",
            "--session",
            "s-9",
            "--no-tools",
            "--",
            CWD
        ]
    );

    let named = STANDIN.replace("    - \"--home\"\n", "    - \"{session}\"\n");
    let start = |config: Config, model: Option<&str>, tools: bool, resume: Option<&str>| {
        JsonlRpc::new(
            config,
            Path::new(HOME),
            Path::new(CWD),
            model,
            tools,
            resume,
        )
        .map(|(rpc, _)| rpc)
    };
    assert_eq!(
        code(start(config_of(&named), Some("m-1"), true, None)),
        "launch_session_id_invalid"
    );
    let pinned = start(config_of(&named), Some("m-1"), true, Some("s-1")).unwrap();
    assert_eq!(pinned.argv()[0], "s-1");
    assert_eq!(
        code(start(config_of(STANDIN), None, true, None)),
        "launch_model_unsupported"
    );
    let relay = STANDIN.replace("    - \"{home}\"\n", "    - \"{relay_url}\"\n");
    assert_eq!(
        code(start(config_of(&relay), Some("m-1"), true, None)),
        "launch_harness_unsupported"
    );
    let no_switch = STANDIN.replace("  tools_off:\n    - \"--no-tools\"\n", "");
    assert_eq!(
        code(start(config_of(&no_switch), Some("m-1"), false, None)),
        "launch_tools_off_unsupported"
    );
}

/// DEC-180 Task 12: each prompt is framed from `piped.prompt` as one JSON line whose string
/// carries the prompt exactly: quotes, backslashes, line breaks, control characters and
/// placeholder names in the prompt are escaped or kept as text, never filled.
#[test]
fn each_prompt_is_framed_from_the_template_as_one_json_line() {
    let mut rpc = begin(true);
    let prompts = [
        "say \"hi\"",
        "back\\slash",
        "two\nlines\r\n",
        "tab\tand \u{1} and \u{e9} and \u{1f600}",
        "{session} {home} {prompt}",
    ];
    for prompt in prompts {
        let frame = rpc.prompt(prompt).unwrap();
        assert_eq!(message(&frame), prompt);
        assert_eq!(
            events_of(&mut rpc, json!({"type": "done"})),
            [finished(true)]
        );
    }
    assert_eq!(
        rpc.prompt("plain").unwrap(),
        b"{\"type\":\"prompt\",\"message\":\"plain\"}\n"
    );
}

/// DEC-180 Task 12: one turn runs at a time, and a prompt that is empty, holds a NUL byte,
/// is over `PROMPT_BYTES_MAX` or whose frame would pass the frame bound is refused before
/// anything is written.
#[test]
fn a_second_prompt_waits_for_the_turn_to_end_and_a_bad_prompt_is_refused() {
    let mut rpc = begin(true);
    let long = "c".repeat(PROMPT_BYTES_MAX + 1);
    let escaped = "\u{1}".repeat(FRAME_BYTES_MAX / 6 + 1);
    assert!(escaped.len() <= PROMPT_BYTES_MAX);
    for prompt in [" \n", "a\0b", long.as_str(), escaped.as_str()] {
        assert_eq!(code(rpc.prompt(prompt)), "launch_prompt_invalid");
        assert_eq!(rpc.phase(), Phase::Idle);
    }
    rpc.prompt("first").unwrap();
    assert_eq!(code(rpc.prompt("second")), "launch_busy");
    assert_eq!(
        events_of(&mut rpc, json!({"type": "done", "ok": true})),
        [finished(true)]
    );
    assert!(rpc.prompt("second").is_ok());
}

/// DEC-180 Task 12: an interrupt gives the `piped.interrupt` line once per turn, and the
/// turn the harness then ends, by an error or by its interrupted fact, ends as interrupted;
/// an interrupted fact nobody asked for ends the turn the same way.
#[test]
fn an_interrupt_writes_the_interrupt_frame_and_the_turn_ends_interrupted() {
    let mut rpc = begin(true);
    assert_eq!(code(rpc.interrupt()), "launch_not_running");
    rpc.prompt("long job").unwrap();
    assert_eq!(rpc.interrupt().unwrap(), ABORT);
    assert_eq!(code(rpc.interrupt()), "launch_not_running");
    assert_eq!(
        events_of(&mut rpc, json!({"type": "text", "delta": "partial"})),
        [text("partial")]
    );
    assert_eq!(
        events_of(&mut rpc, json!({"type": "error", "message": "aborted"})),
        [LaunchEvent::Interrupted, finished(false)]
    );
    assert_eq!(rpc.phase(), Phase::Idle);
    assert_eq!(code(rpc.interrupt()), "launch_not_running");

    rpc.prompt("again").unwrap();
    assert_eq!(
        events_of(&mut rpc, json!({"type": "stopped"})),
        [LaunchEvent::Interrupted, finished(false)]
    );
    assert_eq!(rpc.phase(), Phase::Idle);
}

/// DEC-180 Task 12 and rv-sa241 L1: with tools off, a tool the harness reports using is a
/// breach: one `ApprovalDenied` and the interrupt line once, then nothing read and no
/// interrupt until an unsuccessful end with no `Interrupted`, after which the process takes
/// no further turn. After an asked interrupt a breach writes no second line. With tools on
/// the same line is a plain tool use.
#[test]
fn a_tool_used_with_tools_off_is_a_breach_that_interrupts_the_turn() {
    let tool = json!({"type": "tool", "name": "Bash"});
    let mut rpc = running(false);
    assert!(!rpc.breached());
    let (events, writes) = rpc.receive(&line(tool.clone())).unwrap();
    assert_eq!(events, [LaunchEvent::ApprovalDenied]);
    assert_eq!(writes, [ABORT.to_vec()]);
    assert!(rpc.breached());
    for value in [
        json!({"type": "tool", "name": "Read"}),
        json!({"type": "text", "delta": "after"}),
        json!({"type": "usage", "usage": {"input": 1, "output": 1}}),
    ] {
        assert!(events_of(&mut rpc, value.clone()).is_empty(), "{value}");
    }
    assert_eq!(code(rpc.interrupt()), "launch_not_running");
    assert_eq!(
        events_of(&mut rpc, json!({"type": "stopped"})),
        [finished(false)]
    );
    assert!(!rpc.breached());
    assert_eq!(rpc.phase(), Phase::Failed);
    assert_eq!(code(rpc.prompt("again")), "launch_protocol_failed");

    let mut asked = running(false);
    asked.interrupt().unwrap();
    let (events, writes) = asked.receive(&line(tool.clone())).unwrap();
    assert_eq!(events, [LaunchEvent::ApprovalDenied]);
    assert!(writes.is_empty());
    assert_eq!(
        events_of(&mut asked, json!({"type": "done", "ok": true})),
        [LaunchEvent::Interrupted, finished(false)]
    );
    assert_eq!(asked.phase(), Phase::Failed);
    assert_eq!(code(asked.prompt("again")), "launch_protocol_failed");

    let mut on = running(true);
    assert_eq!(
        events_of(&mut on, tool),
        [LaunchEvent::ToolUse {
            name: "Bash".into()
        }]
    );
    assert!(!on.breached());
}

/// rv-sa241 L1 on Task 12: with tools off, a tool the harness reports between turns, before
/// the first prompt or after a turn ended, is a breach too. It gives one `ApprovalDenied`,
/// writes the interrupt line and fails the conversation for good, so the process takes no
/// further turn. With tools on the same line between turns gives nothing.
#[test]
fn a_tool_used_with_tools_off_between_turns_is_a_breach_that_fails_the_conversation() {
    let tool = json!({"type": "tool", "name": "Bash"});
    let mut after = running(false);
    assert_eq!(
        events_of(&mut after, json!({"type": "done", "ok": true})),
        [finished(true)]
    );
    assert_eq!(after.phase(), Phase::Idle);
    for (state, mut rpc) in [("before", begin(false)), ("after", after)] {
        let (events, writes) = rpc.receive(&line(tool.clone())).unwrap();
        assert_eq!(events, [LaunchEvent::ApprovalDenied], "{state}");
        assert_eq!(writes, [ABORT.to_vec()], "{state}");
        assert_eq!(rpc.phase(), Phase::Failed, "{state}");
        assert!(!rpc.breached(), "{state}");
        assert_eq!(code(rpc.prompt("x")), "launch_protocol_failed", "{state}");
        assert_eq!(code(rpc.interrupt()), "launch_not_running", "{state}");
        assert_eq!(
            code(rpc.receive(&line(json!({"type": "done", "ok": true})))),
            "launch_protocol_failed",
            "{state}"
        );
    }

    let mut on = begin(true);
    assert!(events_of(&mut on, tool).is_empty());
    assert_eq!(on.phase(), Phase::Idle);
}

/// DEC-180 Task 12: every fact a rule reports during a turn gives its event, and the turn
/// ends with exactly one `TurnFinished` on a finished fact, with its success and tokens, or
/// on a turn error, after the error; a line after the end gives nothing.
#[test]
fn each_fact_gives_its_event_and_an_ending_fact_ends_the_turn() {
    let mut rpc = running(true);
    let usage = |input, output| LaunchEvent::Usage {
        tokens: Tokens {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
        },
    };
    let lines = [
        (json!({"type": "session", "id": "s-1"}), Vec::new()),
        (json!({"type": "started"}), Vec::new()),
        (json!({"type": "text", "delta": "hi"}), vec![text("hi")]),
        (
            json!({"type": "tool", "name": "Read"}),
            vec![LaunchEvent::ToolUse {
                name: "Read".into(),
            }],
        ),
        (json!({"type": "waiting"}), Vec::new()),
        (
            json!({"type": "usage", "usage": {"input": 10, "output": 5}}),
            vec![usage(10, 5)],
        ),
        (json!({"type": "exit"}), Vec::new()),
        (
            json!({"type": "done", "ok": true, "usage": {"input": 3, "output": 4}}),
            vec![usage(3, 4), finished(true)],
        ),
    ];
    for (value, events) in lines {
        assert_eq!(events_of(&mut rpc, value.clone()), events, "{value}");
    }
    assert_eq!(rpc.phase(), Phase::Idle);
    assert_eq!(rpc.session(), Some("s-1"));

    rpc.prompt("x").unwrap();
    assert_eq!(
        events_of(&mut rpc, json!({"type": "done", "ok": false})),
        [finished(false)]
    );
    rpc.prompt("x").unwrap();
    assert_eq!(
        events_of(
            &mut rpc,
            json!({"type": "error", "message": "Rate limit reached"})
        ),
        [
            LaunchEvent::TurnError { rate_limited: true },
            finished(false)
        ]
    );
    assert!(events_of(&mut rpc, json!({"type": "done", "ok": true})).is_empty());
    assert_eq!(rpc.phase(), Phase::Idle);
}

/// DEC-180 Task 12: a line outside a turn gives nothing but may carry the session id; a
/// line no rule matches, a line that is not JSON, JSON that is not one object and a line
/// over `LINE_BYTES_MAX` give nothing; a CRLF line is read.
#[test]
fn a_line_outside_a_turn_or_over_the_bound_or_not_one_object_gives_nothing() {
    let mut rpc = begin(true);
    assert!(events_of(&mut rpc, json!({"type": "text", "delta": "early"})).is_empty());
    assert!(events_of(&mut rpc, json!({"type": "session", "id": "s-2"})).is_empty());
    assert_eq!(rpc.session(), Some("s-2"));

    rpc.prompt("x").unwrap();
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
        assert_eq!(rpc.receive(bytes).unwrap(), (Vec::new(), Vec::new()));
    }
    assert_eq!(
        rpc.receive(b"{\"type\":\"text\",\"delta\":\"crlf\"}\r\n")
            .unwrap(),
        (vec![text("crlf")], Vec::new())
    );
    assert_eq!(rpc.phase(), Phase::Running);
}

/// DEC-180 Task 12, the T11-1 lesson: a session id that is not plain fails the conversation
/// for good whether it comes between turns, during a turn or after an interrupt, and every
/// later call is refused without a panic; the starting inputs are checked the same.
#[test]
fn a_session_id_that_is_not_plain_fails_the_conversation_for_good() {
    let long = "s".repeat(SESSION_BYTES_MAX + 1);
    let states: [(&str, fn() -> JsonlRpc); 3] = [
        ("idle", || begin(true)),
        ("running", || running(true)),
        ("interrupted", || {
            let mut rpc = running(true);
            rpc.interrupt().unwrap();
            rpc
        }),
    ];
    for id in ["-rf", "", "a\nb", long.as_str()] {
        for (state, make) in states {
            let mut rpc = make();
            let bad = rpc.receive(&line(json!({"type": "session", "id": id})));
            assert_eq!(code(bad), "launch_session_id_invalid", "{state} {id:?}");
            assert_eq!(rpc.phase(), Phase::Failed);
            assert!(!rpc.breached());
            assert_eq!(code(rpc.prompt("x")), "launch_protocol_failed", "{state}");
            assert_eq!(code(rpc.interrupt()), "launch_not_running", "{state}");
            assert_eq!(
                code(rpc.receive(&line(json!({"type": "started"})))),
                "launch_protocol_failed",
                "{state}"
            );
            assert_eq!(rpc.session(), None, "{state}");
        }
    }
    let start = |home: &str, cwd: &str, model: Option<&str>, resume: Option<&str>| {
        JsonlRpc::new(
            config_of(STANDIN),
            Path::new(home),
            Path::new(cwd),
            model,
            true,
            resume,
        )
        .map(|(rpc, _)| rpc)
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
        code(start(HOME, "work", None, None)),
        "launch_working_dir_unavailable"
    );
    assert_eq!(
        code(start("home", CWD, None, None)),
        "launch_account_home_unsafe"
    );
}

/// DEC-180 Task 12: every refused jsonl-rpc setting names the setting in one plain sentence,
/// with the manifest refusal codes; the prompt template must hold `{prompt}` once inside a
/// JSON string of one JSON object line, and the interrupt must be one JSON object line with
/// nothing to fill.
#[test]
fn each_refused_rpc_setting_names_its_setting() {
    let refused = |text: &str| {
        Config::read(&manifest_of(text))
            .err()
            .map(|refusal| (refusal.code, refusal.message))
    };
    let prompt = |template: &str| STANDIN.replace(PROMPT_LINE, &format!("  prompt: {template}\n"));
    let interrupt =
        |template: &str| STANDIN.replace(INTERRUPT_LINE, &format!("  interrupt: {template}\n"));
    let not_argument = |key: &str| {
        format!(
            "piped.{key}: {{prompt}} is not an argument on this driver; each prompt goes to \
             the harness's input through piped.prompt."
        )
    };
    let once = "piped.prompt: this holds {prompt} once and no other value.";
    let inside = "piped.prompt: this is one JSON object with {prompt} inside a JSON string.";
    let still = "piped.interrupt: this is one JSON object with no value to fill.";
    let long = format!(
        "'{{\"a\":\"{{prompt}}{}\"}}'",
        "b".repeat(TEMPLATE_BYTES_MAX)
    );
    let cases = [
        (
            STANDIN.replace("driver: \"jsonl-rpc\"", "driver: \"text-turn\""),
            manifest::KIND,
            "piped.driver: this harness does not run on the jsonl-rpc driver.".to_owned(),
        ),
        (
            STANDIN.replace(
                "  argv:\n    - \"--home\"\n    - \"{home}\"\n    - \"--model\"\n    - \"{model}\"\n",
                "",
            ),
            manifest::MISSING,
            "piped.argv: this setting is needed.".to_owned(),
        ),
        (
            STANDIN.replace("    - \"{model}\"\n", "    - \"{prompt}\"\n"),
            manifest::VALUE,
            not_argument("argv"),
        ),
        (
            STANDIN.replace("    - \"--session\"\n", "    - \"--text={prompt}\"\n"),
            manifest::VALUE,
            not_argument("resume"),
        ),
        (
            STANDIN.replace("    - \"--no-tools\"\n", "    - \"--{prompt}\"\n"),
            manifest::VALUE,
            not_argument("tools_off"),
        ),
        (
            STANDIN.replace("    - \"{session}\"\n", "    - \"last\"\n"),
            manifest::VALUE,
            "piped.resume: {session} is needed, so a launch continues its session.".to_owned(),
        ),
        (
            STANDIN.replace(PROMPT_LINE, ""),
            manifest::MISSING,
            "piped.prompt: this setting is needed.".to_owned(),
        ),
        (
            prompt("3"),
            manifest::TYPE,
            "piped.prompt: this must be text in quotes.".to_owned(),
        ),
        (
            prompt(&long),
            manifest::VALUE,
            "piped.prompt: this is one line of 1 to 1024 bytes.".to_owned(),
        ),
        (
            prompt("\"{\\\"a\\\":\\\"{prompt}\\\"}\\n\""),
            manifest::VALUE,
            "piped.prompt: this is one line of 1 to 1024 bytes.".to_owned(),
        ),
        (
            prompt("'{\"type\":\"prompt\",\"message\":\"hi\"}'"),
            manifest::VALUE,
            once.to_owned(),
        ),
        (
            prompt("'{\"a\":\"{prompt}\",\"b\":\"{prompt}\"}'"),
            manifest::VALUE,
            once.to_owned(),
        ),
        (
            prompt("'{\"a\":\"{prompt}\",\"s\":\"{session}\"}'"),
            manifest::VALUE,
            once.to_owned(),
        ),
        (
            prompt("'{\"type\":\"prompt\",\"message\":{prompt}}'"),
            manifest::VALUE,
            inside.to_owned(),
        ),
        (
            prompt("'[\"{prompt}\"]'"),
            manifest::VALUE,
            inside.to_owned(),
        ),
        (
            STANDIN.replace(INTERRUPT_LINE, ""),
            manifest::MISSING,
            "piped.interrupt: this setting is needed.".to_owned(),
        ),
        (interrupt("'abort'"), manifest::VALUE, still.to_owned()),
        (
            interrupt("'{\"type\":\"abort\",\"id\":\"{session}\"}'"),
            manifest::VALUE,
            still.to_owned(),
        ),
        (
            STANDIN.replace("  rules: \"standin-events\"\n", ""),
            manifest::MISSING,
            "piped.rules: this setting is needed.".to_owned(),
        ),
        (
            STANDIN.replace("  rules: \"standin-events\"\n", "  rules: 3\n"),
            manifest::TYPE,
            "piped.rules: this must be text in quotes.".to_owned(),
        ),
    ];
    for (text, code, message) in cases {
        assert_eq!(refused(&text), Some((code, message.clone())), "{message}");
    }
    assert_eq!(refused(STANDIN), None);
}
