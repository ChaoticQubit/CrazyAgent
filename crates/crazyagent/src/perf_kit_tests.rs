use crate::perf_kit::{FLAME_SECS_DEFAULT, Plan, Row, flame, plan};

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "common",
        name,
        batch,
    }
}

#[test]
fn the_flame_mode_times_every_row_unset_and_runs_only_the_row_it_names() {
    assert_eq!(plan(None, Some("5"), Some("3"), "hash"), Plan::Time);
    assert_eq!(plan(Some(""), None, None, "hash"), Plan::Time);
    assert_eq!(plan(Some("other"), None, None, "hash"), Plan::Skip);
    assert_eq!(
        plan(Some("hash"), None, None, "hash"),
        Plan::Flame {
            calls: None,
            secs: FLAME_SECS_DEFAULT
        }
    );
    assert_eq!(
        plan(Some("hash"), Some("5"), Some("3"), "hash"),
        Plan::Flame {
            calls: Some(5),
            secs: 3
        }
    );
}

#[test]
#[should_panic(expected = "is not a count")]
fn a_flame_call_count_that_is_not_a_number_is_refused() {
    let _ = plan(Some("hash"), Some("many"), None, "hash");
}

#[test]
#[should_panic(expected = "out of range")]
fn a_flame_run_of_no_seconds_is_refused() {
    let _ = plan(Some("hash"), None, Some("0"), "hash");
}

#[test]
fn a_flame_run_calls_the_step_exactly_the_count_given_or_for_the_seconds_given() {
    let mut calls = 0_u64;
    let ended = flame(&row("names.is_dotted", 3), Some(7), 1, || calls += 1);
    assert_eq!(calls, 7);
    assert!(
        ended.starts_with("FLAME-END 1 common names.is_dotted calls=7 ns="),
        "{ended}"
    );
    let mut timed = 0_u64;
    let started = std::time::Instant::now();
    let ended = flame(&row("names.is_dotted", 3), None, 1, || timed += 1);
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    assert!(
        timed > 0 && ended.contains(&format!(" calls={timed} ")),
        "{ended}"
    );
}
