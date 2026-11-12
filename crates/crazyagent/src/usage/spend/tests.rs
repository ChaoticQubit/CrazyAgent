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
        request(&db, &r("codex-a"), NOW).unwrap_err().0,
        "usage_spend_off"
    );
    assert!(open_attempts(&db).is_empty());
}

#[test]
fn an_owner_spend_is_queued_once_per_account_with_one_engine_made_key() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), NOW).unwrap();
    request(&db, &r("codex-a"), NOW + 1).unwrap();
    request(&db, &r("claude-a"), NOW + 2).unwrap();
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
fn a_spend_not_yet_sent_is_tried_again_with_the_same_key_up_to_a_bound() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), NOW).unwrap();
    let first = queued(&db).unwrap().remove(0);
    for n in 1..TRIES_MAX {
        not_sent(&db, &first, None, NOW + n).unwrap();
        let again = queued(&db).unwrap().remove(0);
        assert_eq!(again.key, first.key);
        assert_eq!(again.tries, n);
    }
    let last = queued(&db).unwrap().remove(0);
    not_sent(&db, &last, None, NOW + 9).unwrap();
    assert!(queued(&db).unwrap().is_empty());
    let shown = view(&db).unwrap();
    assert_eq!(
        shown.attempts[0].outcome.as_deref(),
        Some("usage_spend_not_signed_in")
    );
    request(&db, &r("codex-b"), NOW).unwrap();
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
    request(&db, &r("codex-a"), NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    sent(
        &db,
        attempt.id,
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
    request(&db, &r("codex-a"), NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    request(&db, &r("codex-a"), NOW + 60_000).unwrap();
    let retry = queued(&db).unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].id, attempt.id);
    assert_eq!(retry[0].key, attempt.key);
}

#[test]
fn a_reply_settles_the_attempt_with_a_fixed_code_and_a_failure_with_its_code() {
    let (_root, db) = store();
    for (n, (result, code)) in [
        (Ok(Outcome::Reset), "reset"),
        (Ok(Outcome::NothingToReset), "nothing_to_reset"),
        (Ok(Outcome::NoCredit), "no_credit"),
        (Ok(Outcome::AlreadyRedeemed), "already_redeemed"),
        (Ok(Outcome::Unknown), "outcome_unknown"),
        (
            Err(EngineError("usage_rate_limited".into())),
            "usage_rate_limited",
        ),
        (
            Err(EngineError("some raw text: /Users/x".into())),
            "usage_spend_failed",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let account = format!("codex-{n}");
        request(&db, &r(&account), NOW).unwrap();
        let attempt = queued(&db).unwrap().remove(0);
        mark_sent(&db, attempt.id, NOW + 1).unwrap();
        sent(&db, attempt.id, &result, NOW + 2).unwrap();
        let shown = view(&db).unwrap();
        let line = shown
            .attempts
            .iter()
            .find(|line| line.account.as_str() == account)
            .unwrap();
        assert_eq!(line.state, AttemptState::Settled);
        assert_eq!(line.outcome.as_deref(), Some(code));
    }
}

#[test]
fn a_poll_newer_than_the_send_settles_a_sent_attempt_whose_reply_never_came() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    sent(
        &db,
        attempt.id,
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
    request(&db, &r("codex-a"), NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    mark_sent(&db, attempt.id, NOW + 1).unwrap();
    sent(&db, attempt.id, &Ok(Outcome::NoCredit), NOW + 2).unwrap();
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
        request(&db, &r(&format!("codex-{n}")), NOW + n).unwrap();
        let attempt = queued(&db).unwrap().remove(0);
        mark_sent(&db, attempt.id, NOW + n).unwrap();
        sent(&db, attempt.id, &Ok(Outcome::Reset), NOW + n).unwrap();
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
    sent(&db, attempt.id, &Ok(Outcome::Reset), NOW + 3).unwrap();
    track_pools(&db, &pool, NOW + 4).unwrap();
    assert!(
        open_attempts(&db).is_empty(),
        "the poll and count are older than the spend"
    );
    polled(
        &db,
        "codex-a",
        100.0,
        LimitSource::Poll,
        NOW + 5,
        window_end,
    );
    resets(&db, "codex-a", 2, NOW + 5);
    track_pools(&db, &pool, NOW + 6).unwrap();
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
    sent(&db, first.id, &Ok(Outcome::NothingToReset), NOW + 3).unwrap();
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
    request(&db, &r("codex-a"), NOW).unwrap();
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
