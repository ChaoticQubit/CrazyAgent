use super::TurnSession;
use crate::adapters::FIELD_BYTES_MAX;
use crate::harnesses::drivers::jsonl_turn::Config;
use crate::harnesses::drivers::text_turn;
use crate::harnesses::launch::{
    AccountBinding, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest, Secret,
};
use crate::harnesses::manifest;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-turn.yaml");
const TEXT_STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-text-turn.yaml");

const TEXT_HARNESS: &str = r#"#!/bin/sh
prompt=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt=*) prompt=${1#--prompt=} ;;
  esac
  shift
done
case "$prompt" in
  stubborn*) trap '' TERM ;;
esac
case "$prompt" in
  fail)
    printf '%s\n' 'partial'
    exit 3
    ;;
  wait)
    printf '%s\n' 'thinking'
    exec /bin/sleep 30
    ;;
  flood | stubborn-flood) exec /usr/bin/yes flood ;;
esac
printf '\nYou said: %s\n\n' "$prompt"
"#;

const HARNESS: &str = r#"#!/bin/sh
prompt=
resume=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt=*) prompt=${1#--prompt=} ;;
    --resume) resume=$2 ;;
  esac
  shift
done
case "$prompt" in
  stubborn*) trap '' TERM ;;
esac
printf '%s\n' '{"type":"session","id":"s-5"}'
printf '{"type":"text","delta":"%s|%s"}\n' "$prompt" "$resume"
case "$prompt" in
  fail) exit 3 ;;
  wait | stubborn) exec /bin/sleep 30 ;;
  tool | stubborn-tool)
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

/// The longest a stopped turn whose harness ignores TERM may take to end: the stop grace
/// and a margin, well under the stand-in's 30 second sleep. Output: the wait.
fn overdue() -> Duration {
    let wait = LaunchLimits::default().stop_grace + Duration::from_secs(15);
    assert!(wait < Duration::from_secs(30));
    wait
}

/// Waits up to 5 seconds for a process group to be gone, as macOS answers EPERM for a short
/// while after its last member is reaped. Input: the group id. Output: whether it is gone.
fn gone(group: u32) -> bool {
    let group = rustix::process::Pid::from_raw(i32::try_from(group).unwrap()).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match rustix::process::test_kill_process_group(group) {
            Err(rustix::io::Errno::SRCH) => return true,
            _ if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            _ => return false,
        }
    }
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

/// rv-code2 T11-3 and rv-secreview F1, macOS only: an interrupted turn whose harness ignores
/// TERM and keeps its output open gets KILL once the stop grace passes, well before its 30
/// second sleep ends; the turn ends interrupted, its process group is gone and the next
/// prompt runs.
#[test]
fn an_interrupted_turn_that_ignores_term_is_killed_once_the_stop_grace_passes() {
    let (_directory, mut session) = begin();
    session.send("stubborn").unwrap();
    assert_eq!(first(&mut session), text("stubborn|"));
    let group = session.turn_group().unwrap();
    let started = Instant::now();
    session.interrupt().unwrap();
    assert_eq!(
        finish_within(&mut session, overdue()),
        [LaunchEvent::Interrupted, finished(false)]
    );
    assert!(started.elapsed() < overdue());
    assert_eq!(session.turn_group(), None);
    assert!(gone(group));
    assert_eq!(turn(&mut session, "two"), [text("two|s-5"), finished(true)]);
    session.stop().unwrap();
}

/// rv-code2 T11-3 and rv-secreview F1, macOS only: with tools off, a turn whose harness
/// reports a tool, ignores TERM and keeps its output open gets KILL once the stop grace after
/// the breach passes, with one `ApprovalDenied` and an unsuccessful end, and its process
/// group is gone.
#[test]
fn a_tools_off_breach_that_ignores_term_is_killed_once_the_stop_grace_passes() {
    let (_directory, mut session) = begin();
    session.send("stubborn-tool").unwrap();
    assert_eq!(first(&mut session), text("stubborn-tool|"));
    let group = session.turn_group().unwrap();
    let started = Instant::now();
    assert_eq!(
        finish_within(&mut session, overdue()),
        [LaunchEvent::ApprovalDenied, finished(false)]
    );
    assert!(started.elapsed() < overdue());
    assert_eq!(session.turn_group(), None);
    assert!(gone(group));
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

/// Starts a session on the text-turn stand-in script, with tools off, and takes its
/// `Ready`. Output: the launch folder and the session.
fn begin_text() -> (tempfile::TempDir, TurnSession) {
    let (directory, mut request) = launch();
    std::fs::write(&request.executable, TEXT_HARNESS).unwrap();
    request.harness = "standin-text-turn".into();
    let manifest = TEXT_STANDIN.replace(
        "  resume:\n",
        "  tools_off:\n    - \"--no-tools\"\n  resume:\n",
    );
    let config = text_turn::Config::read(&manifest::parse(manifest.as_bytes()).unwrap()).unwrap();
    let mut session = TurnSession::start_text(request, config).unwrap();
    assert_eq!(
        session.next_event(Duration::ZERO).unwrap(),
        Some(LaunchEvent::Ready {
            native_session: None
        })
    );
    (directory, session)
}

fn turn_error() -> LaunchEvent {
    LaunchEvent::TurnError {
        rate_limited: false,
    }
}

/// DEC-180 Task 13, macOS only: a text-turn session runs one sandboxed process per turn and
/// gives its whole output as the reply once the process exits, with no usage, and no
/// process runs between turns.
#[test]
fn a_text_turn_session_gives_the_whole_output_as_the_reply_when_the_turn_ends() {
    let (_directory, mut session) = begin_text();
    session.send("hello").unwrap();
    assert!(session.turn_group().is_some());
    assert_eq!(code(session.send("again")), "launch_busy");

    let events = finish(&mut session);

    assert_eq!(events, [text("You said: hello"), finished(true)]);
    assert_eq!(session.turn_group(), None);
    assert_eq!(session.native_session(), None);
    assert_eq!(
        turn(&mut session, "two"),
        [text("You said: two"), finished(true)]
    );
    session.stop().unwrap();
}

/// DEC-180 Task 13, macOS only: a text turn whose process exits with code 3 ends with a turn
/// error and no reply, and the next prompt starts a new turn.
#[test]
fn a_text_turn_that_exits_with_an_error_code_ends_with_a_turn_error() {
    let (_directory, mut session) = begin_text();

    let events = turn(&mut session, "fail");

    assert_eq!(events, [turn_error(), finished(false)]);
    assert_eq!(
        turn(&mut session, "two"),
        [text("You said: two"), finished(true)]
    );
    session.stop().unwrap();
}

/// DEC-180 Task 13, macOS only: a text turn whose output never ends is stopped once it
/// passes the output bound, and ends with what fit, a turn error and an unsuccessful end.
#[test]
fn a_text_turn_whose_output_passes_the_bound_is_stopped() {
    let (_directory, mut session) = begin_text();
    let started = Instant::now();
    session.send("flood").unwrap();

    let events = finish_within(&mut session, Duration::from_secs(20));

    assert!(started.elapsed() < Duration::from_secs(20));
    let kept = kept_before_the_error(&events);
    assert!(kept.starts_with("flood\nflood") && kept.len() <= text_turn::OUTPUT_BYTES_MAX);
    assert_eq!(session.turn_group(), None);
    session.stop().unwrap();
}

/// rv-ds241 T13-sec F2, macOS only: a text turn whose harness ignores TERM and floods its
/// output gets KILL once the stop grace after the bound passes, well before the session
/// deadline; the turn ends with what fit, a turn error and an unsuccessful end, its process
/// group is gone and the next prompt runs. A first turn runs before the timed one, so the
/// wait macOS may add to a new program's first start is not timed.
#[test]
fn a_text_turn_past_the_bound_that_ignores_term_is_killed_once_the_stop_grace_passes() {
    let (_directory, mut session) = begin_text();
    assert_eq!(
        turn(&mut session, "one"),
        [text("You said: one"), finished(true)]
    );
    let started = Instant::now();
    session.send("stubborn-flood").unwrap();
    let group = session.turn_group().unwrap();

    let events = finish_within(&mut session, overdue());

    assert!(started.elapsed() < overdue());
    let kept = kept_before_the_error(&events);
    assert!(kept.starts_with("flood\nflood") && kept.len() <= text_turn::OUTPUT_BYTES_MAX);
    assert_eq!(session.turn_group(), None);
    assert!(gone(group));
    assert_eq!(
        turn(&mut session, "two"),
        [text("You said: two"), finished(true)]
    );
    session.stop().unwrap();
}

/// The reply a text turn kept before it ended with a turn error and an unsuccessful end:
/// its text events, each 1 to `FIELD_BYTES_MAX` bytes, joined. Input: the turn's events.
/// Output: the joined text.
fn kept_before_the_error(events: &[LaunchEvent]) -> String {
    assert!(events.len() > 2, "{}", events.len());
    let (texts, end) = events.split_at(events.len() - 2);
    assert_eq!(end, [turn_error(), finished(false)]);
    texts
        .iter()
        .map(|event| match event {
            LaunchEvent::Text { text } => {
                assert!(
                    !text.is_empty() && text.len() <= FIELD_BYTES_MAX,
                    "{}",
                    text.len()
                );
                text.as_str()
            }
            other => panic!("{other:?}"),
        })
        .collect()
}

/// DEC-180 Task 13, macOS only: an interrupt stops a text turn's process group well before
/// its 30 second sleep ends, and the turn ends interrupted with no reply.
#[test]
fn an_interrupted_text_turn_ends_interrupted() {
    let (_directory, mut session) = begin_text();
    session.send("wait").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let started = Instant::now();

    session.interrupt().unwrap();
    let events = finish_within(&mut session, Duration::from_secs(20));

    assert_eq!(events, [LaunchEvent::Interrupted, finished(false)]);
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(session.turn_group(), None);
    session.stop().unwrap();
}
