use super::*;
#[cfg(target_os = "macos")]
use crate::team::runtime::TeamLimits;
#[cfg(target_os = "macos")]
use crate::team::runtime::tests::{ENDPOINT, Fixture, hand_off, limited_runtime, owner};

/// Opens a Codex stand-in whose every turn hits its usage limit, written the way a real
/// Codex writes the limit error, then ends the turn with `end`. Input: the rollout line that
/// ends the turn. Output: the stand-in script.
#[cfg(target_os = "macos")]
fn limit_codex(end: &str) -> String {
    format!(
        r#"#!/bin/sh
[ "$1" = app-server ] && exit 0
d="$CODEX_HOME/sessions/2026/10/27"
/bin/mkdir -p "$d" || exit 1
f="$d/rollout-2026-10-27T00-00-00-$$.jsonl"
printf '{{"type":"session_meta","payload":{{"cwd":"%s"}}}}\n' "$(/bin/pwd -P)" > "$f"
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
while IFS= read -r line; do
  case "$line" in
    *"$e[201~"*) open=0 ;;
    *"$e[200~"*) open=1; continue ;;
    *) [ "$open" = 1 ] && continue ;;
  esac
  printf '{{"type":"event_msg","payload":{{"type":"task_started"}}}}\n' >> "$f"
  /bin/sleep 0.3
  printf '{{"type":"event_msg","payload":{{"type":"error","message":"429 Too Many Requests: You have hit your usage limit."}}}}\n' >> "$f"
  printf '%s\n' '{end}' >> "$f"
done
"#
    )
}

/// Hands one task to a sub-agent whose turn hits its usage limit and ends with `end`, and
/// waits for that first try to end. Input: the line that ends the turn. Output: the task's
/// status and attempts, and the project's rested accounts.
#[cfg(target_os = "macos")]
fn limited_task(end: &str) -> ((String, i64), Vec<String>) {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let mut team = limited_runtime(&fixture, &host, &limit_codex(end), TeamLimits::default());
    owner(&fixture, &[hand_off(&fixture, "Write page.md")]);
    let task = |f: &Fixture| -> (String, i64) {
        f.connection
            .query_row(
                "SELECT status, attempts FROM team_tasks WHERE prompt='Write page.md'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    };
    fixture.until(&mut team, "the limited try ends", |f| {
        let (status, attempts) = task(f);
        attempts >= 1 && status != "running"
    });
    let rested: Vec<String> = fixture
        .connection
        .prepare("SELECT connection FROM team_project_pauses")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let ended = task(&fixture);
    team.shutdown().unwrap();
    (ended, rested)
}

/// An engine-typed turn's end: a rate limit wins over every app end, and with no limit a
/// successful end stays a success after an interrupt, as before the limit moved first.
#[test]
fn a_launched_turn_end_puts_the_limit_first_and_keeps_a_success_after_an_interrupt() {
    let ends = [
        (true, true, true),
        (true, false, false),
        (false, true, true),
        (false, true, false),
        (false, false, true),
        (false, false, false),
    ];
    let got =
        ends.map(|(limited, interrupted, success)| launched_end(limited, interrupted, success));
    assert_eq!(
        got,
        [
            TurnEnd::RateLimited,
            TurnEnd::RateLimited,
            TurnEnd::Success,
            TurnEnd::Interrupted,
            TurnEnd::Success,
            TurnEnd::Failed,
        ]
    );
}

/// A sub-agent turn whose transcript reports the usage limit and then completes is a rate
/// limit, not a success: the task goes back to the queue and the account rests, so the pool
/// can hand it to its next member.
#[cfg(target_os = "macos")]
#[test]
fn a_task_turn_that_hits_its_limit_and_then_completes_goes_back_to_the_queue() {
    let (task, rested) = limited_task(r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#);
    assert_eq!(
        task,
        ("queued".to_owned(), 1),
        "the limit sends the task back"
    );
    assert_eq!(rested, [ENDPOINT], "the limited account rests");
}

/// A sub-agent turn whose transcript reports the usage limit and then aborts as interrupted,
/// as Codex ends a turn it could not run, is a rate limit, not an owner interrupt: the task
/// goes back to the queue and the account rests.
#[cfg(target_os = "macos")]
#[test]
fn a_task_turn_that_hits_its_limit_and_then_aborts_goes_back_to_the_queue() {
    let (task, rested) = limited_task(
        r#"{"type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}"#,
    );
    assert_eq!(
        task,
        ("queued".to_owned(), 1),
        "the limit sends the task back"
    );
    assert_eq!(rested, [ENDPOINT], "the limited account rests");
}
