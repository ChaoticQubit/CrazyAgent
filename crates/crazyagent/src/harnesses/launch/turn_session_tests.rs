use super::TurnSession;
use crate::harnesses::drivers::jsonl_turn::Config;
use crate::harnesses::launch::{
    AccountBinding, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest, Secret,
};
use crate::harnesses::manifest;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-turn.yaml");

const HARNESS: &str = r#"#!/bin/sh
prompt=
resume=
while [ "$#" -gt 1 ]; do
  case "$1" in
    --prompt) prompt=$2 ;;
    --resume) resume=$2 ;;
  esac
  shift
done
printf '%s\n' '{"type":"session","id":"s-5"}'
printf '{"type":"text","delta":"%s|%s"}\n' "$prompt" "$resume"
case "$prompt" in
  fail) exit 3 ;;
  wait) exec /bin/sleep 30 ;;
  tool)
    printf '%s\n' '{"type":"tool","name":"Bash"}'
    exec /bin/sleep 30
    ;;
esac
printf '%s\n' '{"type":"done","ok":true}'
"#;

/// Builds a sandboxed launch of the stand-in harness script: a 0700 home and working
/// folder under the real /tmp, an allow-all profile, a local account with no key and model
/// `m-1`. Output: the folder that holds it all, kept for the row's life, and the request.
fn launch() -> (tempfile::TempDir, LaunchRequest) {
    let directory = tempfile::Builder::new()
        .prefix("ca-turn-session-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = directory.path().canonicalize().unwrap();
    let home = base.join("home");
    let work = base.join("work");
    for folder in [&home, &work] {
        std::fs::create_dir(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = base.join("project.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let harness = base.join("standin-jsonl-turn");
    std::fs::write(&harness, HARNESS).unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o700)).unwrap();
    let request = LaunchRequest {
        harness: "standin-jsonl-turn".into(),
        executable: harness,
        working_dir: work,
        account: AccountBinding::Url {
            home,
            base_url: "http://127.0.0.1:11434".into(),
            key: None,
        },
        model: Some("m-1".into()),
        tools: false,
        sandbox_profile: Some(profile),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    (directory, request)
}

/// The stand-in manifest's jsonl-turn settings. Output: the settings.
fn config() -> Config {
    Config::read(&manifest::parse(STANDIN.as_bytes()).unwrap()).unwrap()
}

/// Starts a session on the stand-in and takes its `Ready`. Output: the launch folder and
/// the session.
fn begin() -> (tempfile::TempDir, TurnSession) {
    let (directory, request) = launch();
    let mut session = TurnSession::start_with(request, config()).unwrap();
    assert_eq!(
        session.next_event(Duration::ZERO).unwrap(),
        Some(LaunchEvent::Ready {
            native_session: None
        })
    );
    (directory, session)
}

/// Collects events until the turn's `TurnFinished`, for at most 30 seconds. Input: the
/// session. Output: the events, the last one `TurnFinished`.
fn finish(session: &mut TurnSession) -> Vec<LaunchEvent> {
    finish_within(session, Duration::from_secs(30))
}

/// Collects events until the turn's `TurnFinished`. Inputs: the session and the longest
/// wait. Output: the events, the last one `TurnFinished`.
fn finish_within(session: &mut TurnSession, wait: Duration) -> Vec<LaunchEvent> {
    let deadline = Instant::now() + wait;
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|event| matches!(event, LaunchEvent::TurnFinished { .. }))
    {
        assert!(Instant::now() < deadline, "{events:?}");
        if let Some(event) = session.next_event(Duration::from_millis(200)).unwrap() {
            events.push(event);
        }
    }
    events
}

/// Waits up to 30 seconds for the turn's first event, so a fresh script's first start is
/// not timed. Input: the session. Output: the event.
fn first(session: &mut TurnSession) -> LaunchEvent {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline);
        if let Some(event) = session.next_event(Duration::from_millis(200)).unwrap() {
            return event;
        }
    }
}

/// Runs one whole turn. Inputs: the session and the prompt. Output: the turn's events.
fn turn(session: &mut TurnSession, prompt: &str) -> Vec<LaunchEvent> {
    session.send(prompt).unwrap();
    finish(session)
}

fn code<T: std::fmt::Debug>(result: Result<T, LaunchError>) -> String {
    result.unwrap_err().0
}

fn text(text: &str) -> LaunchEvent {
    LaunchEvent::Text { text: text.into() }
}

fn finished(success: bool) -> LaunchEvent {
    LaunchEvent::TurnFinished { success }
}

/// DEC-180 Task 11, macOS only, as every agent launch needs Seatbelt: each prompt starts one
/// sandboxed process, a second prompt waits for the running turn, no process runs between
/// turns, and the session id the first turn reported reaches the second turn's arguments.
#[test]
fn a_turn_session_starts_one_process_per_turn_and_carries_the_session_id() {
    let (_directory, mut session) = begin();
    assert_eq!(session.turn_group(), None);
    assert_eq!(session.native_session(), None);
    session.send("one").unwrap();
    assert!(session.turn_group().is_some());
    assert_eq!(code(session.send("again")), "launch_busy");
    assert_eq!(finish(&mut session), [text("one|"), finished(true)]);
    assert_eq!(session.turn_group(), None);
    assert_eq!(session.native_session(), Some("s-5"));
    assert_eq!(turn(&mut session, "two"), [text("two|s-5"), finished(true)]);
    assert_eq!(session.turn_group(), None);
    session.stop().unwrap();
}

/// DEC-180 Task 11, macOS only: a turn whose process exits with code 3 ends with a turn
/// error and an unsuccessful end after the text it gave, and the next prompt starts a new
/// turn that resumes the session.
#[test]
fn a_turn_whose_process_exits_with_an_error_code_ends_with_a_turn_error() {
    let (_directory, mut session) = begin();
    assert_eq!(
        turn(&mut session, "fail"),
        [
            text("fail|"),
            LaunchEvent::TurnError {
                rate_limited: false
            },
            finished(false),
        ]
    );
    assert_eq!(turn(&mut session, "two"), [text("two|s-5"), finished(true)]);
    session.stop().unwrap();
}

/// DEC-180 Task 11, macOS only: an interrupt stops the running turn's process group well
/// before its 30 second sleep ends, the turn ends interrupted, and a second interrupt with
/// no turn running is refused.
#[test]
fn an_interrupt_stops_the_turn_process_and_ends_the_turn_interrupted() {
    let (_directory, mut session) = begin();
    session.send("wait").unwrap();
    assert_eq!(first(&mut session), text("wait|"));
    let started = Instant::now();
    session.interrupt().unwrap();
    assert_eq!(
        finish_within(&mut session, Duration::from_secs(20)),
        [LaunchEvent::Interrupted, finished(false)]
    );
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(session.turn_group(), None);
    assert_eq!(code(session.interrupt()), "launch_not_running");
    session.stop().unwrap();
}

/// DEC-180 Task 11, macOS only, the jsonl-turn side of rv-sec3 F1 on ACP: with tools off, a
/// turn whose harness reports a tool is stopped by the session well before its 30 second
/// sleep ends, with one `ApprovalDenied` and an unsuccessful end, no `Interrupted`.
#[test]
fn a_tool_used_with_tools_off_stops_the_turn_process() {
    let (_directory, mut session) = begin();
    session.send("tool").unwrap();
    assert_eq!(first(&mut session), text("tool|"));
    let started = Instant::now();
    assert_eq!(
        finish_within(&mut session, Duration::from_secs(20)),
        [LaunchEvent::ApprovalDenied, finished(false)]
    );
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(session.turn_group(), None);
    assert_eq!(turn(&mut session, "two"), [text("two|s-5"), finished(true)]);
    session.stop().unwrap();
}

/// DEC-180 Task 11, macOS only: each turn checks its launch again before its process starts,
/// so a home made unsafe between turns refuses the prompt and starts nothing, and the next
/// prompt runs once the home is safe again.
#[test]
fn each_turn_checks_its_launch_again_before_its_process_starts() {
    let (directory, mut session) = begin();
    let home = directory.path().canonicalize().unwrap().join("home");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code(session.send("one")), "launch_account_home_unsafe");
    assert_eq!(session.turn_group(), None);
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(turn(&mut session, "one"), [text("one|"), finished(true)]);
    session.stop().unwrap();
}

/// DEC-180 Task 11, macOS only: until the tasks that hand them over land, an account that
/// carries a key is refused with `launch_key_relay_unsupported` and a run at Full access
/// with `launch_harness_unsupported`.
#[test]
fn a_turn_session_refuses_an_account_key_and_full_access() {
    let (_keyed, mut keyed) = launch();
    keyed.account = AccountBinding::ApiKey {
        home: keyed.account.home().to_path_buf(),
        key: Secret::new("sk-unit".into()).unwrap(),
        base_url: None,
    };
    assert_eq!(
        code(TurnSession::start_with(keyed, config()).map(drop)),
        "launch_key_relay_unsupported"
    );
    let (_open, mut open) = launch();
    open.sandbox_profile = None;
    open.unconfined = true;
    assert_eq!(
        code(TurnSession::start_with(open, config()).map(drop)),
        "launch_harness_unsupported"
    );
}
