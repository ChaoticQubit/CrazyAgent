use super::*;

fn account(name: &str) -> CredentialRef {
    CredentialRef::new(name).unwrap()
}

fn window(used_percent: f64, resets_at_ms: i64) -> Option<LimitWindow> {
    Some(LimitWindow {
        used_percent,
        resets_at_ms: Some(resets_at_ms),
    })
}

fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = open(root.path()).unwrap();
    (root, db)
}

#[test]
fn limits_round_trip_through_the_engine_database_and_reopen() {
    let (root, db) = store();
    let claude = account("claude-main");
    assert_eq!(limits(&db, &claude).unwrap(), None);
    let observed = Observation {
        plan: Some("max".into()),
        five_hour: window(42.5, 1_000_000),
        weekly: window(80.0, 9_000_000),
        provider: Some(crate::usage::poll::Provider::Claude),
    };
    record_observation(&db, &claude, &observed, LimitSource::Poll, 500).unwrap();
    drop(db);
    let db = open(root.path()).unwrap();
    let stored = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(
        stored,
        AccountLimits {
            account: claude.clone(),
            plan: Some("max".into()),
            five_hour: window(42.5, 1_000_000),
            weekly: window(80.0, 9_000_000),
            polled_five_hour: window(42.5, 1_000_000),
            polled_weekly: window(80.0, 9_000_000),
            five_hour_reset_override_ms: None,
            weekly_reset_override_ms: None,
            source: Some(LimitSource::Poll),
            observed_ms: Some(500),
            checked_ms: Some(500),
            failure: None,
            five_hour_left_out: None,
            weekly_left_out: None,
            five_hour_left_out_label: None,
            weekly_left_out_label: None,
        }
    );
    assert_eq!(limits(&db, &account("codex-other")).unwrap(), None);
    assert_eq!(all_limits(&db).unwrap(), vec![stored]);
}

/// N-D16 run 6: a Codex plan whose poll reports only the weekly window, as prolite's does,
/// clears the 5-hour window and its manual reset, marks it left out by Codex and keeps the plan.
#[test]
fn a_poll_keeps_the_plan_and_clears_a_window_it_left_out_with_its_manual_reset() {
    let (_root, db) = store();
    let codex = account("codex-a");
    let first = Observation {
        plan: Some("pro".into()),
        five_hour: window(10.0, 2_000),
        weekly: window(20.0, 3_000),
        provider: Some(crate::usage::poll::Provider::Codex),
    };
    record_observation(&db, &codex, &first, LimitSource::Poll, 100).unwrap();
    set_reset_override(&db, &codex, WindowKind::FiveHour, Some(7_000)).unwrap();
    set_reset_override(&db, &codex, WindowKind::Weekly, Some(8_000)).unwrap();
    let later = Observation {
        plan: None,
        five_hour: None,
        weekly: window(25.0, 5_000),
        provider: Some(crate::usage::poll::Provider::Codex),
    };
    record_observation(&db, &codex, &later, LimitSource::Poll, 200).unwrap();
    let stored = limits(&db, &codex).unwrap().unwrap();
    assert_eq!(stored.plan.as_deref(), Some("pro"));
    assert_eq!(stored.five_hour, None);
    assert_eq!(stored.polled_five_hour, None);
    assert_eq!(stored.five_hour_reset_override_ms, None);
    assert_eq!(
        stored.five_hour_left_out,
        Some(crate::usage::poll::Provider::Codex)
    );
    assert_eq!(
        stored.five_hour_left_out_label.as_deref(),
        Some("Codex reports no 5-hour limit for this plan.")
    );
    assert_eq!(stored.weekly, window(25.0, 5_000));
    assert_eq!(stored.weekly_reset_override_ms, None);
    assert_eq!(stored.weekly_left_out, None);
    assert_eq!(stored.weekly_left_out_label, None);
    assert_eq!(stored.source, Some(LimitSource::Poll));
    assert_eq!(stored.observed_ms, Some(200));
    assert_eq!(stored.checked_ms, Some(200));
}

#[test]
fn a_transcript_reading_leaves_the_windows_the_last_poll_read() {
    let (_root, db) = store();
    let codex = account("codex-p");
    let polled = Observation {
        plan: None,
        five_hour: window(100.0, 9_000_000),
        weekly: window(30.0, 90_000_000),
        provider: Some(crate::usage::poll::Provider::Codex),
    };
    record_observation(&db, &codex, &polled, LimitSource::Poll, 1_000).unwrap();
    let stale = 1_000 + service::POLL_EVERY_MS;
    let read = Observation {
        plan: None,
        five_hour: window(0.0, 8_000_000),
        weekly: window(99.0, 80_000_000),
        provider: None,
    };
    record_observation(&db, &codex, &read, LimitSource::Transcript, stale).unwrap();
    let stored = limits(&db, &codex).unwrap().unwrap();
    assert_eq!(stored.source, Some(LimitSource::Transcript));
    assert_eq!(stored.five_hour, read.five_hour);
    assert_eq!(stored.weekly, read.weekly);
    assert_eq!(stored.polled_five_hour, polled.five_hour);
    assert_eq!(stored.polled_weekly, polled.weekly);
    assert_eq!(stored.polled_five_hour_at(stale), polled.five_hour);
    set_reset_override(&db, &codex, WindowKind::FiveHour, Some(stale + 1)).unwrap();
    let overridden = limits(&db, &codex).unwrap().unwrap();
    assert_eq!(
        overridden.polled_five_hour_at(stale + 1),
        Some(LimitWindow {
            used_percent: 0.0,
            resets_at_ms: None,
        })
    );
    let partial = Observation {
        plan: None,
        five_hour: window(40.0, 9_500_000),
        weekly: None,
        provider: Some(crate::usage::poll::Provider::Codex),
    };
    record_observation(&db, &codex, &partial, LimitSource::Poll, stale + 2).unwrap();
    let after = limits(&db, &codex).unwrap().unwrap();
    assert_eq!(after.polled_five_hour, partial.five_hour);
    assert_eq!(after.five_hour, partial.five_hour);
    assert_eq!(after.polled_weekly, None);
    assert_eq!(after.weekly, None);
    assert_eq!(
        after.weekly_left_out,
        Some(crate::usage::poll::Provider::Codex)
    );
    assert_eq!(
        after.weekly_left_out_label.as_deref(),
        Some("Codex reports no weekly limit for this plan.")
    );
    assert_eq!(after.five_hour_reset_override_ms, None);
}

/// N-D16 run 6, Claude accounts 3 and 4: a Claude poll that leaves the 5-hour window out
/// clears the window an older poll stored, whose reset passed long ago, and marks it left out
/// by Claude. A failed poll or a run's own record that leaves it out keeps the mark, a run's
/// own record or a poll that reports the window clears it, and only a poll sets it again.
#[test]
fn a_left_out_window_is_cleared_and_marked_until_a_read_reports_it() {
    use crate::usage::poll::Provider;
    const HOUR: i64 = 3_600_000;
    let (_root, db) = store();
    let claude = account("claude-idle");
    let busy = Observation {
        plan: Some("max".into()),
        five_hour: window(60.0, 2 * HOUR),
        weekly: window(30.0, 400 * HOUR),
        provider: Some(Provider::Claude),
    };
    record_observation(&db, &claude, &busy, LimitSource::Poll, HOUR).unwrap();
    set_reset_override(&db, &claude, WindowKind::FiveHour, Some(3 * HOUR)).unwrap();
    let later = 151 * HOUR;
    let idle = Observation {
        plan: None,
        five_hour: None,
        weekly: window(31.0, 400 * HOUR),
        provider: Some(Provider::Claude),
    };
    record_observation(&db, &claude, &idle, LimitSource::Poll, later).unwrap();
    let stored = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(stored.five_hour, None);
    assert_eq!(stored.polled_five_hour, None);
    assert_eq!(stored.five_hour_reset_override_ms, None);
    assert_eq!(stored.five_hour_at(later), None);
    assert_eq!(stored.five_hour_left_out, Some(Provider::Claude));
    assert_eq!(
        stored.five_hour_left_out_label.as_deref(),
        Some("Claude reports no 5-hour window right now.")
    );
    assert_eq!(stored.weekly, window(31.0, 400 * HOUR));
    assert_eq!(stored.weekly_left_out, None);
    assert_eq!(stored.plan.as_deref(), Some("max"));
    record_failure(&db, &claude, "usage_unreachable", later + 1).unwrap();
    let failed = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(failed.five_hour_left_out, Some(Provider::Claude));
    let recorded = later + service::POLL_EVERY_MS + 2;
    let record = Observation {
        plan: None,
        five_hour: None,
        weekly: window(32.0, 400 * HOUR),
        provider: None,
    };
    record_observation(&db, &claude, &record, LimitSource::Transcript, recorded).unwrap();
    let kept = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(kept.source, Some(LimitSource::Transcript));
    assert_eq!(kept.five_hour, None);
    assert_eq!(kept.five_hour_left_out, Some(Provider::Claude));
    let ran = Observation {
        plan: None,
        five_hour: window(8.0, recorded + 5 * HOUR),
        weekly: window(32.0, 400 * HOUR),
        provider: None,
    };
    record_observation(&db, &claude, &ran, LimitSource::Transcript, recorded + 1).unwrap();
    let reported = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(reported.five_hour, ran.five_hour);
    assert_eq!(reported.polled_five_hour, None);
    assert_eq!(reported.five_hour_left_out, None);
    record_observation(&db, &claude, &idle, LimitSource::Poll, recorded + 2).unwrap();
    let marked = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(marked.five_hour, None);
    assert_eq!(marked.five_hour_left_out, Some(Provider::Claude));
    let back = Observation {
        plan: None,
        five_hour: window(5.0, recorded + 4 * HOUR),
        weekly: window(33.0, 400 * HOUR),
        provider: Some(Provider::Claude),
    };
    record_observation(&db, &claude, &back, LimitSource::Poll, recorded + 3).unwrap();
    let again = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(again.five_hour, back.five_hour);
    assert_eq!(again.five_hour_left_out, None);
    assert_eq!(again.five_hour_left_out_label, None);
    assert_eq!(again.weekly_left_out, None);
}

/// N-D16 r1 F1: the engine holds the one sentence each client shows for a left-out window.
#[test]
fn the_engine_words_each_left_out_window_for_its_provider() {
    use crate::usage::poll::Provider;
    for (provider, kind, sentence) in [
        (
            Provider::Codex,
            WindowKind::FiveHour,
            "Codex reports no 5-hour limit for this plan.",
        ),
        (
            Provider::Codex,
            WindowKind::Weekly,
            "Codex reports no weekly limit for this plan.",
        ),
        (
            Provider::Claude,
            WindowKind::FiveHour,
            "Claude reports no 5-hour window right now.",
        ),
        (
            Provider::Claude,
            WindowKind::Weekly,
            "Claude reports no weekly window right now.",
        ),
    ] {
        assert_eq!(left_out_label(provider, kind), sentence);
    }
}

#[test]
#[should_panic(expected = "a poll names whose usage call it made")]
fn a_poll_that_names_no_provider_is_a_bug() {
    let (_root, db) = store();
    let nameless = Observation {
        five_hour: window(10.0, 5_000),
        ..Observation::default()
    };
    let _ = record_observation(&db, &account("nameless"), &nameless, LimitSource::Poll, 1);
}

#[test]
fn a_store_made_before_polled_copies_reads_and_gains_them_from_rows_a_poll_wrote() {
    let root = tempfile::tempdir().unwrap();
    let db = Connection::open(root.path().join("engine.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE account_limits (account TEXT PRIMARY KEY, plan TEXT,
            five_hour_percent REAL, five_hour_resets_ms INTEGER, weekly_percent REAL,
            weekly_resets_ms INTEGER, five_hour_reset_override_ms INTEGER,
            weekly_reset_override_ms INTEGER, source TEXT, observed_ms INTEGER,
            checked_ms INTEGER, failure TEXT);
         INSERT INTO account_limits (account,five_hour_percent,five_hour_resets_ms,
            weekly_percent,weekly_resets_ms,source,observed_ms)
         VALUES ('polled',100.0,9000,40.0,90000,'poll',10),
                ('read',50.0,8000,60.0,80000,'transcript',20);",
    )
    .unwrap();
    let expect = |db: &Connection| {
        let polled = limits(db, &account("polled")).unwrap().unwrap();
        assert_eq!(polled.five_hour, window(100.0, 9_000));
        assert_eq!(polled.polled_five_hour, window(100.0, 9_000));
        assert_eq!(polled.polled_weekly, window(40.0, 90_000));
        let read = limits(db, &account("read")).unwrap().unwrap();
        assert_eq!(read.five_hour, window(50.0, 8_000));
        assert_eq!(read.polled_five_hour, None);
        assert_eq!(read.polled_weekly, None);
        assert_eq!(all_limits(db).unwrap(), vec![polled, read]);
    };
    expect(&db);
    initialize(&db).unwrap();
    expect(&db);
    initialize(&db).unwrap();
    let columns: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('account_limits')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 18);
    expect(&db);
}

#[test]
fn a_failed_poll_keeps_the_last_reading_until_a_poll_succeeds() {
    let (_root, db) = store();
    let claude = account("claude-b");
    record_failure(&db, &claude, "usage_unreachable", 50).unwrap();
    let fresh = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(fresh.failure.as_deref(), Some("usage_unreachable"));
    assert_eq!(fresh.five_hour, None);
    assert_eq!(fresh.source, None);
    let observed = Observation {
        plan: None,
        five_hour: window(5.0, 9_000),
        weekly: window(6.0, 10_000),
        provider: Some(crate::usage::poll::Provider::Claude),
    };
    record_observation(&db, &claude, &observed, LimitSource::Poll, 60).unwrap();
    record_failure(&db, &claude, "usage_sign_in_lapsed", 70).unwrap();
    let failed = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(failed.five_hour, window(5.0, 9_000));
    assert_eq!(failed.failure.as_deref(), Some("usage_sign_in_lapsed"));
    assert_eq!(failed.checked_ms, Some(70));
    record_observation(&db, &claude, &observed, LimitSource::Transcript, 80).unwrap();
    let still = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(still.failure.as_deref(), Some("usage_sign_in_lapsed"));
    assert_eq!(still.checked_ms, Some(70));
    record_observation(&db, &claude, &observed, LimitSource::Poll, 90).unwrap();
    let cleared = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(cleared.failure, None);
    assert_eq!(cleared.checked_ms, Some(90));
}

#[test]
fn a_manual_reset_replaces_the_observed_one_and_a_passed_reset_frees_the_window() {
    let (_root, db) = store();
    let claude = account("claude-c");
    let observed = Observation {
        plan: None,
        five_hour: window(100.0, 5_000),
        weekly: window(70.0, 50_000),
        provider: Some(crate::usage::poll::Provider::Claude),
    };
    record_observation(&db, &claude, &observed, LimitSource::Poll, 1_000).unwrap();
    set_reset_override(&db, &claude, WindowKind::FiveHour, Some(3_000)).unwrap();
    let stored = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(stored.five_hour_at(2_000), window(100.0, 3_000));
    assert_eq!(
        stored.five_hour_at(3_000),
        Some(LimitWindow {
            used_percent: 0.0,
            resets_at_ms: None
        })
    );
    assert_eq!(stored.weekly_at(49_999), window(70.0, 50_000));
    assert_eq!(stored.weekly_at(50_000).unwrap().used_percent, 0.0);
    set_reset_override(&db, &claude, WindowKind::FiveHour, None).unwrap();
    let cleared = limits(&db, &claude).unwrap().unwrap();
    assert_eq!(cleared.five_hour_at(4_000), window(100.0, 5_000));
    assert_eq!(window_at(None, Some(9_000), 1), None);
}

#[test]
fn invalid_shares_times_and_codes_are_refused_and_store_nothing() {
    let (_root, db) = store();
    let claude = account("claude-d");
    for used_percent in [100.5, -1.0, f64::NAN, f64::INFINITY] {
        let observed = Observation {
            plan: None,
            five_hour: Some(LimitWindow {
                used_percent,
                resets_at_ms: None,
            }),
            weekly: None,
            provider: Some(crate::usage::poll::Provider::Claude),
        };
        let refused = record_observation(&db, &claude, &observed, LimitSource::Poll, 1);
        assert_eq!(refused.unwrap_err().0, "usage_limits_invalid");
    }
    let late = Observation {
        plan: None,
        five_hour: window(1.0, RESET_MS_MAX + 1),
        weekly: None,
        provider: Some(crate::usage::poll::Provider::Claude),
    };
    let refused = record_observation(&db, &claude, &late, LimitSource::Poll, 1);
    assert_eq!(refused.unwrap_err().0, "usage_limits_invalid");
    let odd_plan = Observation {
        plan: Some("max\nplan".into()),
        five_hour: None,
        weekly: None,
        provider: Some(crate::usage::poll::Provider::Claude),
    };
    let refused = record_observation(&db, &claude, &odd_plan, LimitSource::Poll, 1);
    assert_eq!(refused.unwrap_err().0, "usage_limits_invalid");
    for at in [0, -5, RESET_MS_MAX + 1] {
        let refused = set_reset_override(&db, &claude, WindowKind::Weekly, Some(at));
        assert_eq!(refused.unwrap_err().0, "usage_limits_invalid");
    }
    let refused = record_failure(&db, &claude, &"x".repeat(FAILURE_BYTES_MAX + 1), 1);
    assert_eq!(refused.unwrap_err().0, "usage_limits_invalid");
    assert_eq!(limits(&db, &claude).unwrap(), None);
    assert_eq!(
        open(Path::new("relative")).unwrap_err().0,
        "usage_unavailable"
    );
}

#[test]
fn a_failed_poll_stores_only_a_code_shaped_failure() {
    let code = |text: &str| crate::usage::service::failure_code(&EngineError(text.into()));
    assert_eq!(code("usage_rate_limited"), "usage_rate_limited");
    assert_eq!(code("native_profile_in_use2"), "native_profile_in_use2");
    assert_eq!(
        code("open /Users/mira/engine.db: denied"),
        "usage_poll_failed"
    );
    assert_eq!(code("Usage Failed"), "usage_poll_failed");
    assert_eq!(code(""), "usage_poll_failed");
    assert_eq!(code(&"a".repeat(65)), "usage_poll_failed");
    assert_eq!(code(&"a".repeat(64)), "a".repeat(64));
}
