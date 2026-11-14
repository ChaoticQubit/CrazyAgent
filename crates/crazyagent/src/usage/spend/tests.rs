use super::*;
use crate::config::{
    AccountPool, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};
use crate::usage::extras::{Extras, Field, Resets, record_extras};
use crate::usage::poll::Provider;
use crate::usage::{LimitSource, LimitWindow, Observation, record_observation};

const NOW: i64 = 1_793_000_000_000;
const HOUR: i64 = 60 * 60 * 1000;

fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    (root, db)
}

fn r(text: &str) -> CredentialRef {
    CredentialRef::new(text).unwrap()
}

fn managed(id: &str, harness: &str) -> ModelConnection {
    ModelConnection {
        id: r(id),
        provider: r(if harness == "codex" {
            "openai"
        } else {
            "anthropic"
        }),
        harness: r(harness),
        protocol: r("native"),
        account_ref: r(id),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "e2e-model".into(),
            author: r("openai"),
        },
        auth: ModelAuthentication::NativeManaged { profile_ref: r(id) },
    }
}

fn access(members: &[(&str, &str)]) -> ModelAccess {
    ModelAccess {
        accounts: members.iter().map(|(id, _)| r(id)).collect(),
        connections: members
            .iter()
            .map(|(id, harness)| managed(id, harness))
            .collect(),
        pools: vec![AccountPool {
            id: r("pool-a"),
            name: Some("Daily work".into()),
            members: members.iter().map(|(id, _)| r(id)).collect(),
            projects: vec![r("demo")],
            roles: Vec::new(),
            fallback_allowed: false,
            provider_quota_policy: Default::default(),
        }],
    }
}

fn polled(db: &Connection, account: &str, used: f64, source: LimitSource, at: i64, resets_at: i64) {
    let window = LimitWindow {
        used_percent: used,
        resets_at_ms: Some(resets_at),
    };
    record_observation(
        db,
        &r(account),
        &Observation {
            plan: None,
            five_hour: Some(window),
            weekly: None,
            provider: Some(Provider::Codex),
        },
        source,
        at,
    )
    .unwrap();
}

fn resets(db: &Connection, account: &str, count: u32, at: i64) {
    record_extras(
        db,
        &r(account),
        &Extras {
            resets: Field::Known(Resets {
                count,
                kinds: Vec::new(),
                next_expiry_ms: None,
            }),
            credits: Field::NotAsked,
        },
        at,
    )
    .unwrap();
}

fn settings(confirm: SpendConfirm, all_limited: AllLimited) -> SpendSettings {
    SpendSettings {
        confirm,
        all_limited,
        resets_source: ResetsSource::ThroughClaudeCode,
    }
}

fn open_attempts(db: &Connection) -> Vec<(String, String, String)> {
    let mut statement = db
        .prepare("SELECT account,origin,attempt_key FROM usage_spend_attempts WHERE state IN ('queued','sent') ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn the_defaults_are_confirm_each_spend_and_a_notice_when_a_pool_is_limited() {
    let (_root, db) = store();
    assert_eq!(
        read(&db).unwrap(),
        settings(SpendConfirm::ConfirmEach, AllLimited::Notice)
    );
    assert!(read(&db).unwrap().spends());
    assert!(!read(&db).unwrap().auto_spends());
}

#[test]
fn every_spend_option_is_stored_and_read_back() {
    let (_root, db) = store();
    for confirm in [
        SpendConfirm::OneClick,
        SpendConfirm::Off,
        SpendConfirm::ConfirmEach,
    ] {
        for all_limited in [
            AllLimited::NoHint,
            AllLimited::AutoSpend,
            AllLimited::Notice,
        ] {
            write(&db, settings(confirm, all_limited)).unwrap();
            assert_eq!(read(&db).unwrap(), settings(confirm, all_limited));
        }
    }
}

/// Claude free resets are read through Claude Code until the owner picks another source;
/// each source is stored and read back beside the spend settings, which it never changes.
#[test]
fn every_claude_free_resets_source_is_stored_and_read_back() {
    let (_root, db) = store();
    assert_eq!(
        read(&db).unwrap().resets_source,
        ResetsSource::ThroughClaudeCode
    );
    for source in [
        ResetsSource::AsClaudeCode,
        ResetsSource::Off,
        ResetsSource::ThroughClaudeCode,
    ] {
        let chosen = SpendSettings {
            resets_source: source,
            ..settings(SpendConfirm::OneClick, AllLimited::NoHint)
        };
        write(&db, chosen).unwrap();
        assert_eq!(read(&db).unwrap(), chosen, "{source:?}");
    }
}

/// A store made before the source column gains it on open with the default, keeps the
/// owner's spend settings and takes only the three source codes.
#[test]
fn a_spend_store_from_before_the_source_column_gains_it_with_the_default() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("usage.db");
    {
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE usage_spend (
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                confirm TEXT NOT NULL CHECK(confirm IN ('confirm_each','one_click','off')),
                all_limited TEXT NOT NULL CHECK(all_limited IN ('notice','no_hint','auto_spend'))
            );
            INSERT INTO usage_spend (singleton,confirm,all_limited) VALUES (1,'one_click','auto_spend');",
        )
        .unwrap();
    }
    let db = Connection::open(&path).unwrap();
    crate::usage::initialize(&db).unwrap();
    crate::usage::initialize(&db).unwrap();
    let source: String = db
        .query_row("SELECT resets_source FROM usage_spend", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(source, "through_claude_code");
    assert_eq!(
        read(&db).unwrap(),
        settings(SpendConfirm::OneClick, AllLimited::AutoSpend)
    );
    assert!(
        db.execute("UPDATE usage_spend SET resets_source='maybe'", [])
            .is_err()
    );
}

/// A client from before the source sends the spend settings without it, which reads as the
/// default; the wire names each source in snake case and refuses any other.
#[test]
fn the_source_rides_the_spend_settings_on_the_wire_and_an_older_client_reads_the_default() {
    let older: SpendSettings =
        serde_json::from_str(r#"{"confirm":"one_click","all_limited":"no_hint"}"#).unwrap();
    assert_eq!(older.resets_source, ResetsSource::ThroughClaudeCode);
    for (source, wire) in [
        (ResetsSource::ThroughClaudeCode, "through_claude_code"),
        (ResetsSource::AsClaudeCode, "as_claude_code"),
        (ResetsSource::Off, "off"),
    ] {
        let sent = serde_json::to_value(SpendSettings {
            resets_source: source,
            ..SpendSettings::default()
        })
        .unwrap();
        assert_eq!(sent["resets_source"], wire);
        assert_eq!(
            serde_json::from_value::<SpendSettings>(sent)
                .unwrap()
                .resets_source,
            source
        );
    }
    assert!(
        serde_json::from_str::<SpendSettings>(
            r#"{"confirm":"one_click","all_limited":"no_hint","resets_source":"maybe"}"#
        )
        .is_err()
    );
}

#[test]
fn spend_off_means_the_app_never_spends_even_with_auto_spend_chosen() {
    let off = settings(SpendConfirm::Off, AllLimited::AutoSpend);
    assert!(!off.spends());
    assert!(!off.auto_spends());
    assert!(settings(SpendConfirm::OneClick, AllLimited::AutoSpend).auto_spends());
    assert!(!settings(SpendConfirm::OneClick, AllLimited::Notice).auto_spends());
    let (_root, db) = store();
    write(&db, off).unwrap();
    assert_eq!(
        request(&db, &r("codex-a"), true, NOW).unwrap_err().0,
        "usage_spend_off"
    );
    assert!(open_attempts(&db).is_empty());
}

#[test]
fn an_owner_spend_is_queued_once_per_account_with_one_engine_made_key() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), true, NOW).unwrap();
    request(&db, &r("codex-a"), true, NOW + 1).unwrap();
    request(&db, &r("claude-a"), true, NOW + 2).unwrap();
    let open = open_attempts(&db);
    assert_eq!(open.len(), 2);
    assert_eq!(open[0].0, "codex-a");
    assert_eq!(open[0].1, "owner");
    assert!(key_shaped(&open[0].2), "{}", open[0].2);
    assert!(key_shaped(&open[1].2));
    assert_ne!(open[0].2, open[1].2);
    let queued = queued(&db).unwrap();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].key, open[0].2);
    assert_eq!(queued[0].tries, 0);
}

#[test]
fn an_unconfirmed_spend_is_refused_while_each_spend_asks_first() {
    let (_root, db) = store();
    assert_eq!(read(&db).unwrap().confirm, SpendConfirm::ConfirmEach);
    assert_eq!(
        request(&db, &r("codex-a"), false, NOW).unwrap_err().0,
        "usage_spend_unconfirmed"
    );
    assert!(open_attempts(&db).is_empty(), "nothing is queued");
    request(&db, &r("codex-a"), true, NOW + 1).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 2).unwrap();
    assert_eq!(
        request(&db, &r("codex-a"), false, NOW + 3).unwrap_err().0,
        "usage_spend_unconfirmed",
        "a re-ask of a sent spend needs the confirm too"
    );
    assert!(queued(&db).unwrap().is_empty());
    write(&db, settings(SpendConfirm::OneClick, AllLimited::Notice)).unwrap();
    request(&db, &r("codex-b"), false, NOW + 4).unwrap();
    request(&db, &r("codex-c"), true, NOW + 5).unwrap();
    assert_eq!(queued(&db).unwrap().len(), 2, "one click takes either");
    write(&db, settings(SpendConfirm::Off, AllLimited::Notice)).unwrap();
    for confirmed in [false, true] {
        assert_eq!(
            request(&db, &r("codex-d"), confirmed, NOW + 6)
                .unwrap_err()
                .0,
            "usage_spend_off"
        );
    }
}

#[test]
fn a_spend_not_yet_sent_is_tried_again_with_the_same_key_up_to_a_bound() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), true, NOW).unwrap();
    let first = queued(&db).unwrap().remove(0);
    let mut latest = first.clone();
    for n in 1..TRIES_MAX {
        not_sent(&db, &latest, None, NOW + n).unwrap();
        latest = queued(&db).unwrap().remove(0);
        assert_eq!(latest.key, first.key);
        assert_eq!(latest.tries, n);
    }
    not_sent(&db, &latest, None, NOW + 9).unwrap();
    assert!(queued(&db).unwrap().is_empty());
    let shown = view(&db).unwrap();
    assert_eq!(
        shown.attempts[0].outcome.as_deref(),
        Some("usage_spend_not_signed_in")
    );
    request(&db, &r("codex-b"), true, NOW).unwrap();
    let refused = queued(&db).unwrap().remove(0);
    not_sent(
        &db,
        &refused,
        Some(&EngineError("usage_sign_in_rejected".into())),
        NOW + 1,
    )
    .unwrap();
    assert!(queued(&db).unwrap().is_empty());
}

#[test]
fn a_sent_attempt_is_never_sent_again_by_the_engine_even_after_a_restart() {
    let (root, db) = store();
    request(&db, &r("codex-a"), true, NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    sent(
        &db,
        &attempt,
        &Err(EngineError("usage_unreachable".into())),
        NOW + 31_000,
    )
    .unwrap();
    assert!(queued(&db).unwrap().is_empty());
    drop(db);
    let db = crate::usage::open(root.path()).unwrap();
    assert!(
        queued(&db).unwrap().is_empty(),
        "no re-send after a restart"
    );
    let open = open_attempts(&db);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].2, attempt.key);
    assert_eq!(view(&db).unwrap().attempts[0].state, AttemptState::Sent);
}

#[test]
fn an_owner_retry_of_a_sent_attempt_carries_the_same_key() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), true, NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    request(&db, &r("codex-a"), true, NOW + 60_000).unwrap();
    let retry = queued(&db).unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].id, attempt.id);
    assert_eq!(retry[0].key, attempt.key);
}

#[test]
fn a_definite_reply_settles_the_attempt_with_its_fixed_code() {
    let (_root, db) = store();
    for (n, (result, code)) in [
        (Ok(Outcome::Reset), "reset"),
        (Ok(Outcome::NothingToReset), "nothing_to_reset"),
        (Ok(Outcome::NoCredit), "no_credit"),
        (Ok(Outcome::AlreadyRedeemed), "already_redeemed"),
        (
            Err(EngineError("usage_request_invalid".into())),
            "usage_request_invalid",
        ),
        (
            Err(EngineError("usage_spend_refused".into())),
            "usage_spend_refused",
        ),
        (
            Err(EngineError("usage_sign_in_lapsed".into())),
            "usage_sign_in_lapsed",
        ),
        (
            Err(EngineError("usage_rate_limited".into())),
            "usage_rate_limited",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let account = format!("codex-{n}");
        request(&db, &r(&account), true, NOW).unwrap();
        let attempt = queued(&db).unwrap().remove(0);
        assert!(!attempt.resend);
        mark_sent(&db, attempt.id, NOW + 1).unwrap();
        sent(&db, &attempt, &result, NOW + 2).unwrap();
        let shown = view(&db).unwrap();
        let line = shown
            .attempts
            .iter()
            .find(|line| line.account.as_str() == account)
            .unwrap();
        assert_eq!(line.state, AttemptState::Settled, "{code}");
        assert_eq!(line.outcome.as_deref(), Some(code));
    }
}

#[test]
fn a_reply_that_may_have_used_a_reset_keeps_the_attempt_sent_with_its_key() {
    let (_root, db) = store();
    for (n, result) in [
        Ok(Outcome::Unknown),
        Err(EngineError("usage_spend_failed".into())),
        Err(EngineError("usage_reply_invalid".into())),
        Err(EngineError("usage_unreachable".into())),
        Err(EngineError("some raw text: /Users/x".into())),
    ]
    .into_iter()
    .enumerate()
    {
        let account = format!("codex-{n}");
        request(&db, &r(&account), true, NOW).unwrap();
        let attempt = queued(&db).unwrap().remove(0);
        mark_sent(&db, attempt.id, NOW + 1).unwrap();
        sent(&db, &attempt, &result, NOW + 2).unwrap();
        let shown = view(&db).unwrap();
        let line = shown
            .attempts
            .iter()
            .find(|line| line.account.as_str() == account)
            .unwrap();
        assert_eq!(line.state, AttemptState::Sent, "{result:?}");
        assert_eq!(line.outcome, None);
        assert!(queued(&db).unwrap().is_empty(), "never re-sent on its own");
        request(&db, &r(&account), true, NOW + 60_000).unwrap();
        let again = queued(&db).unwrap().remove(0);
        assert_eq!(again.id, attempt.id);
        assert_eq!(
            again.key, attempt.key,
            "the owner's next Spend reuses the key"
        );
        assert!(again.resend);
        mark_sent(&db, again.id, NOW + 60_001).unwrap();
        sent(&db, &again, &Ok(Outcome::Reset), NOW + 60_002).unwrap();
    }
}

#[test]
fn a_refusal_of_a_resend_keeps_the_attempt_sent_with_its_key() {
    let (_root, db) = store();
    for (n, code) in [
        "usage_request_invalid",
        "usage_spend_refused",
        "usage_sign_in_lapsed",
        "usage_rate_limited",
    ]
    .into_iter()
    .enumerate()
    {
        let account = format!("codex-{n}");
        request(&db, &r(&account), true, NOW).unwrap();
        let attempt = queued(&db).unwrap().remove(0);
        mark_sent(&db, attempt.id, NOW + 1).unwrap();
        sent(
            &db,
            &attempt,
            &Err(EngineError("usage_spend_failed".into())),
            NOW + 2,
        )
        .unwrap();
        request(&db, &r(&account), true, NOW + 60_000).unwrap();
        let again = queued(&db).unwrap().remove(0);
        assert!(again.resend);
        mark_sent(&db, again.id, NOW + 60_001).unwrap();
        sent(&db, &again, &Err(EngineError(code.into())), NOW + 60_002).unwrap();
        let open = open_attempts(&db);
        assert_eq!(
            open.len(),
            1,
            "{code}: the first send may have used a reset"
        );
        assert_eq!(open[0].2, attempt.key);
        sent(&db, &again, &Ok(Outcome::Reset), NOW + 60_003).unwrap();
    }
}

#[test]
fn a_resend_that_cannot_go_out_goes_back_to_sent_with_its_key() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), true, NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    sent(
        &db,
        &attempt,
        &Err(EngineError("usage_unreachable".into())),
        NOW + 2,
    )
    .unwrap();
    for (n, problem) in [
        None,
        Some(EngineError("usage_spend_off".into())),
        Some(EngineError("usage_spend_no_account".into())),
        Some(EngineError("usage_sign_in_rejected".into())),
    ]
    .into_iter()
    .enumerate()
    {
        request(&db, &r("codex-a"), true, NOW + 60_000).unwrap();
        let again = queued(&db).unwrap().remove(0);
        assert!(again.resend);
        not_sent(&db, &again, problem.as_ref(), NOW + 60_001 + n as i64).unwrap();
        assert!(queued(&db).unwrap().is_empty());
        let open = open_attempts(&db);
        assert_eq!(open.len(), 1, "{problem:?}");
        assert_eq!(open[0].2, attempt.key);
        assert_eq!(view(&db).unwrap().attempts[0].state, AttemptState::Sent);
    }
}

#[test]
fn a_failed_poll_never_settles_a_sent_attempt() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), true, NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    sent(
        &db,
        &attempt,
        &Err(EngineError("usage_spend_failed".into())),
        NOW + 2,
    )
    .unwrap();
    crate::usage::record_failure(&db, &r("codex-a"), "usage_unreachable", NOW + 10).unwrap();
    settle_sent(&db, NOW + 11).unwrap();
    assert_eq!(
        open_attempts(&db).len(),
        1,
        "a failed poll read no limits after the send"
    );
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW + 20,
        NOW + 2 * HOUR,
    );
    settle_sent(&db, NOW + 21).unwrap();
    assert!(open_attempts(&db).is_empty());
    assert_eq!(
        view(&db).unwrap().attempts[0].outcome.as_deref(),
        Some("outcome_unknown")
    );
}

#[test]
fn a_poll_newer_than_the_send_settles_a_sent_attempt_whose_reply_never_came() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), true, NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    sent(
        &db,
        &attempt,
        &Err(EngineError("usage_unreachable".into())),
        NOW + 2,
    )
    .unwrap();
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    settle_sent(&db, NOW + 3).unwrap();
    assert_eq!(
        open_attempts(&db).len(),
        1,
        "that poll is older than the send"
    );
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW + 10,
        NOW + 2 * HOUR,
    );
    settle_sent(&db, NOW + 11).unwrap();
    assert!(open_attempts(&db).is_empty());
    assert_eq!(
        view(&db).unwrap().attempts[0].outcome.as_deref(),
        Some("outcome_unknown")
    );
}

#[test]
fn the_view_shows_outcomes_but_never_a_key() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), true, NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    sent(&db, &attempt, &Ok(Outcome::NoCredit), NOW + 2).unwrap();
    let shown = view(&db).unwrap();
    assert_eq!(shown.attempts.len(), 1);
    assert_eq!(shown.attempts[0].settled_ms, Some(NOW + 2));
    let wire = serde_json::to_string(&shown).unwrap();
    assert!(!wire.contains(&attempt.key));
    assert!(!wire.contains("key"));
}

#[test]
fn settled_attempts_are_kept_to_a_bound() {
    let (_root, db) = store();
    for n in 0..(ATTEMPTS_KEPT + 8) {
        request(&db, &r(&format!("codex-{n}")), true, NOW + n).unwrap();
        let attempt = queued(&db).unwrap().remove(0);
        mark_sent(&db, attempt.id, NOW + n).unwrap();
        sent(&db, &attempt, &Ok(Outcome::Reset), NOW + n).unwrap();
    }
    let kept: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_spend_attempts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(kept, ATTEMPTS_KEPT);
}

#[test]
fn a_pool_is_limited_only_when_every_member_was_polled_at_its_limit() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex"), ("codex-b", "codex")]);
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    polled(&db, "codex-b", 99.0, LimitSource::Poll, NOW, NOW + 2 * HOUR);
    track_pools(&db, &pool, NOW).unwrap();
    assert!(view(&db).unwrap().limited_pools.is_empty());
    polled(
        &db,
        "codex-b",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    resets(&db, "codex-b", 2, NOW);
    track_pools(&db, &pool, NOW).unwrap();
    let shown = view(&db).unwrap().limited_pools;
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].pool.as_str(), "pool-a");
    assert_eq!(shown[0].name.as_deref(), Some("Daily work"));
    assert_eq!(
        shown[0]
            .with_resets
            .iter()
            .map(CredentialRef::as_str)
            .collect::<Vec<_>>(),
        ["codex-b"]
    );
    polled(
        &db,
        "codex-a",
        20.0,
        LimitSource::Poll,
        NOW + 1,
        NOW + 2 * HOUR,
    );
    track_pools(&db, &pool, NOW + 1).unwrap();
    assert!(view(&db).unwrap().limited_pools.is_empty());
}

#[test]
fn an_agent_reported_limit_never_makes_a_pool_limited() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex")]);
    polled(&db, "codex-a", 40.0, LimitSource::Poll, NOW, NOW + 2 * HOUR);
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Transcript,
        NOW + 1,
        NOW + 2 * HOUR,
    );
    resets(&db, "codex-a", 1, NOW + 1);
    write(&db, settings(SpendConfirm::OneClick, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW + 2).unwrap();
    assert!(view(&db).unwrap().limited_pools.is_empty());
    assert!(open_attempts(&db).is_empty());
}

#[test]
fn no_hint_shows_no_limited_pool_and_spends_nothing() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex")]);
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    resets(&db, "codex-a", 1, NOW);
    write(&db, settings(SpendConfirm::ConfirmEach, AllLimited::NoHint)).unwrap();
    track_pools(&db, &pool, NOW).unwrap();
    assert!(view(&db).unwrap().limited_pools.is_empty());
    assert!(open_attempts(&db).is_empty());
}

#[test]
fn the_notice_names_the_accounts_with_free_resets_and_spends_nothing() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex"), ("codex-b", "codex")]);
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    polled(
        &db,
        "codex-b",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    resets(&db, "codex-a", 1, NOW);
    track_pools(&db, &pool, NOW).unwrap();
    let shown = view(&db).unwrap().limited_pools;
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].with_resets, vec![r("codex-a")]);
    assert!(open_attempts(&db).is_empty());
}

#[test]
fn auto_spend_queues_one_spend_on_an_account_with_free_resets_when_every_member_is_limited() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex"), ("codex-b", "codex")]);
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    polled(
        &db,
        "codex-b",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    resets(&db, "codex-b", 3, NOW);
    write(
        &db,
        settings(SpendConfirm::ConfirmEach, AllLimited::AutoSpend),
    )
    .unwrap();
    track_pools(&db, &pool, NOW + 1).unwrap();
    track_pools(&db, &pool, NOW + 2).unwrap();
    let open = open_attempts(&db);
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open[0].0.as_str(), open[0].1.as_str()),
        ("codex-b", "auto")
    );
}

#[test]
fn a_stale_count_after_a_spend_gives_no_second_auto_spend_in_the_same_limit_window() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex")]);
    let window_end = NOW + 2 * HOUR;
    polled(&db, "codex-a", 100.0, LimitSource::Poll, NOW, window_end);
    resets(&db, "codex-a", 3, NOW);
    write(&db, settings(SpendConfirm::OneClick, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW + 1).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 2).unwrap();
    sent(&db, &attempt, &Ok(Outcome::Reset), NOW + 3).unwrap();
    let past_the_hour = NOW + HOUR + 1;
    track_pools(&db, &pool, past_the_hour + 4).unwrap();
    assert!(
        open_attempts(&db).is_empty(),
        "the poll and count are older than the spend"
    );
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        past_the_hour + 5,
        window_end,
    );
    track_pools(&db, &pool, past_the_hour + 6).unwrap();
    assert!(
        open_attempts(&db).is_empty(),
        "a fresh poll but the count is older than the spend"
    );
    resets(&db, "codex-a", 2, past_the_hour + 7);
    track_pools(&db, &pool, past_the_hour + 8).unwrap();
    assert!(open_attempts(&db).is_empty(), "the same limit window");
    let later = NOW + 3 * HOUR;
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        later,
        later + 5 * HOUR,
    );
    resets(&db, "codex-a", 2, later);
    track_pools(&db, &pool, later + 1).unwrap();
    assert_eq!(
        open_attempts(&db).len(),
        1,
        "a new limit window, a fresh poll and count"
    );
}

#[test]
fn only_a_poll_and_a_count_both_newer_than_the_last_spend_let_auto_spend_run() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex")]);
    let window_end = NOW + 3 * HOUR;
    polled(&db, "codex-a", 100.0, LimitSource::Poll, NOW, window_end);
    resets(&db, "codex-a", 2, NOW);
    request(&db, &r("codex-a"), true, NOW + 1).unwrap();
    let owner = queued(&db).unwrap().remove(0);
    mark_sent(&db, owner.id, NOW + 2).unwrap();
    sent(&db, &owner, &Ok(Outcome::Reset), NOW + 3).unwrap();
    write(&db, settings(SpendConfirm::OneClick, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW + 4).unwrap();
    assert!(
        open_attempts(&db).is_empty(),
        "the poll and the count are older than the owner's spend"
    );
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW + 5,
        window_end,
    );
    track_pools(&db, &pool, NOW + 6).unwrap();
    assert!(
        open_attempts(&db).is_empty(),
        "a fresh poll but the count is older than the owner's spend"
    );
    resets(&db, "codex-a", 1, NOW + 7);
    track_pools(&db, &pool, NOW + 8).unwrap();
    let open = open_attempts(&db);
    assert_eq!(open.len(), 1, "a fresh poll and a fresh count");
    assert_eq!(open[0].1, "auto");
}

#[test]
fn a_failed_poll_is_never_fresh_enough_for_an_auto_spend() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex")]);
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 3 * HOUR,
    );
    resets(&db, "codex-a", 2, NOW);
    request(&db, &r("codex-a"), true, NOW + 1).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 2).unwrap();
    sent(&db, &attempt, &Ok(Outcome::Reset), NOW + 3).unwrap();
    write(&db, settings(SpendConfirm::OneClick, AllLimited::AutoSpend)).unwrap();
    let later = NOW + HOUR;
    crate::usage::record_failure(&db, &r("codex-a"), "usage_unreachable", later).unwrap();
    resets(&db, "codex-a", 1, later);
    track_pools(&db, &pool, later + 1).unwrap();
    assert!(
        open_attempts(&db).is_empty(),
        "the limits are still the ones read before the owner's spend"
    );
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        later + 2,
        NOW + 3 * HOUR,
    );
    track_pools(&db, &pool, later + 3).unwrap();
    assert_eq!(open_attempts(&db).len(), 1, "a poll that read the limits");
}

#[test]
fn auto_spend_passes_over_a_claude_member_until_its_spend_call_lands() {
    let (_root, db) = store();
    let pool = access(&[("claude-x", "claude"), ("codex-y", "codex")]);
    for account in ["claude-x", "codex-y"] {
        polled(&db, account, 100.0, LimitSource::Poll, NOW, NOW + 2 * HOUR);
        resets(&db, account, 1, NOW);
    }
    write(&db, settings(SpendConfirm::OneClick, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW + 1).unwrap();
    let open = open_attempts(&db);
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open[0].0.as_str(), open[0].1.as_str()),
        ("codex-y", "auto")
    );
}

#[test]
fn a_claude_spend_is_settled_unavailable_before_its_token_is_read() {
    assert_eq!(
        not_spendable(Some(&managed("claude-x", "claude"))),
        Some("usage_spend_unavailable")
    );
    assert_eq!(not_spendable(Some(&managed("codex-y", "codex"))), None);
    assert_eq!(not_spendable(None), Some("usage_spend_no_account"));
}

#[test]
fn a_pool_auto_spends_at_most_once_an_hour() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex"), ("codex-b", "codex")]);
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    polled(
        &db,
        "codex-b",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 3 * HOUR,
    );
    resets(&db, "codex-a", 1, NOW);
    resets(&db, "codex-b", 1, NOW);
    write(&db, settings(SpendConfirm::OneClick, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW + 1).unwrap();
    let first = queued(&db).unwrap().remove(0);
    mark_sent(&db, first.id, NOW + 2).unwrap();
    sent(&db, &first, &Ok(Outcome::NothingToReset), NOW + 3).unwrap();
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW + 10,
        NOW + 2 * HOUR,
    );
    polled(
        &db,
        "codex-b",
        100.0,
        LimitSource::Poll,
        NOW + 10,
        NOW + 3 * HOUR,
    );
    resets(&db, "codex-a", 1, NOW + 10);
    resets(&db, "codex-b", 1, NOW + 10);
    track_pools(&db, &pool, NOW + 11).unwrap();
    assert!(open_attempts(&db).is_empty(), "within the pool's hour");
}

#[test]
fn auto_spend_never_queues_while_spend_is_off_or_no_account_has_a_free_reset() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex")]);
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW,
        NOW + 2 * HOUR,
    );
    write(&db, settings(SpendConfirm::OneClick, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW + 1).unwrap();
    assert!(
        open_attempts(&db).is_empty(),
        "no account holds a free reset"
    );
    resets(&db, "codex-a", 1, NOW + 2);
    write(&db, settings(SpendConfirm::Off, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW + 3).unwrap();
    assert!(open_attempts(&db).is_empty(), "spend is off");
}

#[test]
fn a_queued_spend_settles_as_off_when_the_owner_turned_spend_off_after_asking() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), true, NOW).unwrap();
    write(&db, settings(SpendConfirm::Off, AllLimited::Notice)).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    not_sent(
        &db,
        &attempt,
        Some(&EngineError("usage_spend_off".into())),
        NOW + 1,
    )
    .unwrap();
    assert_eq!(
        view(&db).unwrap().attempts[0].outcome.as_deref(),
        Some("usage_spend_off")
    );
}

#[test]
fn engine_made_keys_are_version_4_uuids() {
    for _ in 0..32 {
        let key = attempt_key().unwrap();
        assert!(key_shaped(&key), "{key}");
        assert_eq!(&key[14..15], "4");
        assert!(matches!(&key[19..20], "8" | "9" | "a" | "b"));
    }
    assert!(!key_shaped(""));
    assert!(!key_shaped("00000000-0000-4000-8000-00000000000G"));
    assert!(!key_shaped("00000000-0000-4000-8000-0000000000000"));
}

#[test]
fn ids_sent_in_a_url_or_header_are_shape_checked() {
    assert!(id_shaped("acct-synthetic_01"));
    for bad in [
        "",
        "a/b",
        "a?b",
        "..",
        "a%2f",
        "a b",
        "a\nb",
        &"x".repeat(129),
    ] {
        assert!(!id_shaped(bad), "{bad:?}");
    }
}
