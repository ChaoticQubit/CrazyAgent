use super::*;

fn changed(cursor: u64, mark: u64, topics: &[Topic]) -> Result<WatchReply, EngineError> {
    Ok(WatchReply::Changed {
        cursor: Cursor::new(cursor),
        mark,
        topics: topics.to_vec(),
    })
}

fn quiet(cursor: u64, mark: u64) -> Result<WatchReply, EngineError> {
    Ok(WatchReply::Quiet {
        cursor: Cursor::new(cursor),
        mark,
    })
}

fn failed(code: &str) -> Result<WatchReply, EngineError> {
    Err(EngineError(code.into()))
}

fn at(start: Instant, ms: u64) -> Instant {
    start + Duration::from_millis(ms)
}

fn turn(topics: &[Topic], pause_ms: u64) -> Turn {
    Turn {
        topics: topics.to_vec(),
        pause: Duration::from_millis(pause_ms),
    }
}

fn ask(after: u64, mark: Option<u64>, wait_ms: u32) -> Ask {
    Ask {
        after: Cursor::new(after),
        mark,
        wait_ms,
    }
}

#[test]
fn a_first_watch_starts_from_nothing_and_named_topics_go_out_at_once() {
    let start = Instant::now();
    let mut feed = Feed::default();
    assert_eq!(feed.ask(start), ask(0, None, WAIT_MS));
    assert_eq!(
        feed.answer(changed(3, 7, &[Topic::Projects]), start),
        turn(&[Topic::Projects], 0)
    );
    assert_eq!(feed.ask(start), ask(3, Some(7), WAIT_MS));
    assert_eq!(
        feed.answer(changed(4, 8, &[Topic::Landing, Topic::Team]), at(start, 10)),
        turn(&[Topic::Landing, Topic::Team], 0)
    );
    assert_eq!(feed.answer(quiet(4, 8), at(start, 25_010)), turn(&[], 0));
    assert_eq!(feed.ask(at(start, 25_010)), ask(4, Some(8), WAIT_MS));
}

#[test]
fn an_unknown_change_goes_out_at_once_then_at_most_once_a_period_and_on_time() {
    let start = Instant::now();
    let mut feed = Feed::default();
    assert_eq!(
        feed.answer(changed(1, 5, &[Topic::Unknown]), start),
        turn(&[Topic::Unknown], 0)
    );
    assert_eq!(feed.ask(start).wait_ms, WAIT_MS, "nothing is owed yet");
    assert_eq!(
        feed.answer(changed(1, 6, &[Topic::Unknown]), at(start, 250)),
        turn(&[], 0)
    );
    assert_eq!(feed.ask(at(start, 250)), ask(1, Some(6), 4_750));
    assert_eq!(
        feed.ask(start + Duration::from_micros(250_500)).wait_ms,
        4_750,
        "the wait ends at or past the period's end, never just before it"
    );
    assert_eq!(
        feed.answer(changed(1, 7, &[Topic::Unknown]), at(start, 500)),
        turn(&[], 0)
    );
    assert_eq!(
        feed.answer(quiet(1, 7), at(start, 5_000)),
        turn(&[Topic::Unknown], 0)
    );
    assert_eq!(
        feed.ask(at(start, 5_000)).wait_ms,
        WAIT_MS,
        "the owed one went out"
    );
    assert_eq!(feed.answer(quiet(1, 7), at(start, 30_000)), turn(&[], 0));
    assert_eq!(
        feed.answer(changed(1, 8, &[Topic::Unknown]), at(start, 30_001)),
        turn(&[Topic::Unknown], 0)
    );
}

#[test]
fn a_named_topic_beside_unknown_goes_out_now_and_the_unknown_waits_its_turn() {
    let start = Instant::now();
    let mut feed = Feed::default();
    assert_eq!(
        feed.answer(changed(2, 6, &[Topic::Projects, Topic::Unknown]), start),
        turn(&[Topic::Projects, Topic::Unknown], 0)
    );
    assert_eq!(
        feed.answer(
            changed(3, 7, &[Topic::Projects, Topic::Unknown]),
            at(start, 1_000)
        ),
        turn(&[Topic::Projects], 0)
    );
    assert_eq!(feed.ask(at(start, 1_000)), ask(3, Some(7), 4_000));
    assert_eq!(
        feed.answer(changed(4, 8, &[Topic::Settings]), at(start, 2_000)),
        turn(&[Topic::Settings], 0),
        "a named topic never waits for the owed unknown"
    );
    assert_eq!(
        feed.answer(quiet(4, 8), at(start, 5_000)),
        turn(&[Topic::Unknown], 0)
    );
}

#[test]
fn a_refresh_reloads_as_an_unknown_change_and_resumes_from_its_cursor() {
    let start = Instant::now();
    let mut feed = Feed::default();
    let refresh = |cursor: u64, mark: u64| -> Result<WatchReply, EngineError> {
        Ok(WatchReply::Refresh {
            cursor: Cursor::new(cursor),
            mark,
        })
    };
    assert_eq!(
        feed.answer(refresh(9, 4), start),
        turn(&[Topic::Unknown], 0)
    );
    assert_eq!(feed.ask(start), ask(9, Some(4), WAIT_MS));
    assert_eq!(feed.answer(refresh(2, 5), at(start, 100)), turn(&[], 0));
    assert_eq!(feed.ask(at(start, 100)), ask(2, Some(5), 4_900));
}

#[test]
fn a_busy_engine_or_no_connection_polls_once_a_period_and_keeps_the_cursor() {
    let start = Instant::now();
    let mut feed = Feed::default();
    assert_eq!(
        feed.answer(changed(2, 5, &[Topic::Projects]), start),
        turn(&[Topic::Projects], 0)
    );
    assert_eq!(
        feed.answer(failed("watch_busy"), at(start, 10)),
        turn(&[Topic::Unknown], 5_000)
    );
    assert_eq!(
        feed.unwatched(at(start, 5_010)),
        turn(&[Topic::Unknown], 5_000)
    );
    assert_eq!(feed.unwatched(at(start, 6_000)), turn(&[], 5_000));
    assert_eq!(
        feed.answer(
            Ok(WatchReply::Rejected {
                code: "watch_busy".into()
            }),
            at(start, 10_010)
        ),
        turn(&[Topic::Unknown], 5_000)
    );
    assert_eq!(feed.ask(at(start, 10_010)), ask(2, Some(5), WAIT_MS));
}

#[test]
fn a_failed_watch_keeps_the_cursor_and_mark_backs_off_and_still_reloads_each_period() {
    let start = Instant::now();
    let mut feed = Feed::default();
    assert_eq!(
        feed.answer(changed(6, 9, &[Topic::Unknown]), start),
        turn(&[Topic::Unknown], 0)
    );
    let unavailable = || failed("watch_unavailable");
    assert_eq!(feed.answer(unavailable(), at(start, 100)), turn(&[], 1_000));
    assert_eq!(
        feed.answer(unavailable(), at(start, 1_100)),
        turn(&[], 2_000)
    );
    assert_eq!(
        feed.answer(failed("watch_outcome_unknown"), at(start, 3_100)),
        turn(&[], 4_000)
    );
    assert_eq!(
        feed.answer(unavailable(), at(start, 7_100)),
        turn(&[Topic::Unknown], 5_000)
    );
    assert_eq!(
        feed.answer(
            Ok(WatchReply::Rejected {
                code: "owner_authentication_failed".into()
            }),
            at(start, 12_100)
        ),
        turn(&[Topic::Unknown], 5_000)
    );
    assert_eq!(feed.ask(at(start, 12_100)), ask(6, Some(9), WAIT_MS));
    assert_eq!(feed.answer(quiet(6, 9), at(start, 12_200)), turn(&[], 0));
    assert_eq!(
        feed.answer(unavailable(), at(start, 12_300)),
        turn(&[], 1_000),
        "a reply ends the failures in a row"
    );
}
