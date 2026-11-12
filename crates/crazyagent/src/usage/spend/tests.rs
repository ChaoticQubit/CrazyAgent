use super::*;
use crate::config::{
    AccountPool, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint,
    ModelReference,
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

fn polled(db: &Connection, account: &str, used: f64, source: LimitSource) {
    let window = LimitWindow {
        used_percent: used,
        resets_at_ms: Some(NOW + 2 * HOUR),
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
        NOW,
    )
    .unwrap();
}

fn resets(db: &Connection, account: &str, count: u32) {
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
        NOW,
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
        .prepare("SELECT account,origin,attempt_key FROM usage_spend_attempts WHERE state='queued' ORDER BY id")
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
fn a_retry_keeps_the_same_key_and_a_settled_attempt_frees_the_account() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), NOW).unwrap();
    let first = queued(&db).unwrap().remove(0);
    tried(&db, first.id).unwrap();
    let again = queued(&db).unwrap().remove(0);
    assert_eq!(again.key, first.key);
    assert_eq!(again.tries, 1);
    settle(&db, first.id, Outcome::Reset.code(), NOW + 5).unwrap();
    assert!(queued(&db).unwrap().is_empty());
    request(&db, &r("codex-a"), NOW + 10).unwrap();
    let next = queued(&db).unwrap().remove(0);
    assert_ne!(next.key, first.key);
    let shown = view(&db, NOW + 10).unwrap();
    let codex: Vec<_> = shown
        .attempts
        .iter()
        .filter(|attempt| attempt.account.as_str() == "codex-a")
        .collect();
    assert_eq!(codex.len(), 1, "one line per account, the latest");
    assert_eq!(codex[0].state, AttemptState::Queued);
}

#[test]
fn the_view_shows_outcomes_but_never_a_key() {
    let (_root, db) = store();
    request(&db, &r("codex-a"), NOW).unwrap();
    let attempt = queued(&db).unwrap().remove(0);
    settle(&db, attempt.id, Outcome::NoCredit.code(), NOW + 1).unwrap();
    let shown = view(&db, NOW + 1).unwrap();
    assert_eq!(shown.attempts.len(), 1);
    assert_eq!(shown.attempts[0].outcome.as_deref(), Some("no_credit"));
    assert_eq!(shown.attempts[0].state, AttemptState::Settled);
    assert_eq!(shown.attempts[0].settled_ms, Some(NOW + 1));
    let wire = serde_json::to_string(&shown).unwrap();
    assert!(!wire.contains(&attempt.key));
}

#[test]
fn settled_attempts_are_kept_to_a_bound() {
    let (_root, db) = store();
    for n in 0..(ATTEMPTS_KEPT + 8) {
        request(&db, &r(&format!("codex-{n}")), NOW + n).unwrap();
        let attempt = queued(&db).unwrap().remove(0);
        settle(&db, attempt.id, Outcome::Reset.code(), NOW + n).unwrap();
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
    polled(&db, "codex-a", 100.0, LimitSource::Poll);
    polled(&db, "codex-b", 99.0, LimitSource::Poll);
    track_pools(&db, &pool, NOW).unwrap();
    assert!(view(&db, NOW).unwrap().limited_pools.is_empty());
    polled(&db, "codex-b", 100.0, LimitSource::Poll);
    resets(&db, "codex-b", 2);
    track_pools(&db, &pool, NOW).unwrap();
    let shown = view(&db, NOW).unwrap().limited_pools;
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
}

#[test]
fn an_agent_reported_limit_never_makes_a_pool_limited() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex")]);
    polled(&db, "codex-a", 40.0, LimitSource::Poll);
    polled(&db, "codex-a", 100.0, LimitSource::Transcript);
    resets(&db, "codex-a", 1);
    write(&db, settings(SpendConfirm::OneClick, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW).unwrap();
    assert!(view(&db, NOW).unwrap().limited_pools.is_empty());
    assert!(open_attempts(&db).is_empty());
}

#[test]
fn no_hint_shows_no_limited_pool() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex")]);
    polled(&db, "codex-a", 100.0, LimitSource::Poll);
    resets(&db, "codex-a", 1);
    write(&db, settings(SpendConfirm::ConfirmEach, AllLimited::NoHint)).unwrap();
    track_pools(&db, &pool, NOW).unwrap();
    assert!(view(&db, NOW).unwrap().limited_pools.is_empty());
    assert!(open_attempts(&db).is_empty());
}

#[test]
fn auto_spend_queues_one_spend_per_limited_episode_on_an_account_with_free_resets() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex"), ("codex-b", "codex")]);
    polled(&db, "codex-a", 100.0, LimitSource::Poll);
    polled(&db, "codex-b", 100.0, LimitSource::Poll);
    resets(&db, "codex-b", 3);
    write(&db, settings(SpendConfirm::ConfirmEach, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW).unwrap();
    let open = open_attempts(&db);
    assert_eq!(open.len(), 1);
    assert_eq!((open[0].0.as_str(), open[0].1.as_str()), ("codex-b", "auto"));
    let attempt = queued(&db).unwrap().remove(0);
    settle(&db, attempt.id, Outcome::NothingToReset.code(), NOW + 1).unwrap();
    track_pools(&db, &pool, NOW + 2).unwrap();
    assert!(open_attempts(&db).is_empty(), "one auto-spend per episode");
    polled(&db, "codex-a", 10.0, LimitSource::Poll);
    track_pools(&db, &pool, NOW + 3).unwrap();
    polled(&db, "codex-a", 100.0, LimitSource::Poll);
    track_pools(&db, &pool, NOW + 4).unwrap();
    assert_eq!(open_attempts(&db).len(), 1, "a new episode may spend again");
}

#[test]
fn auto_spend_never_queues_while_spend_is_off_or_no_account_has_a_free_reset() {
    let (_root, db) = store();
    let pool = access(&[("codex-a", "codex")]);
    polled(&db, "codex-a", 100.0, LimitSource::Poll);
    write(&db, settings(SpendConfirm::OneClick, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW).unwrap();
    assert!(open_attempts(&db).is_empty(), "no account holds a free reset");
    resets(&db, "codex-a", 1);
    write(&db, settings(SpendConfirm::Off, AllLimited::AutoSpend)).unwrap();
    track_pools(&db, &pool, NOW + 1).unwrap();
    assert!(open_attempts(&db).is_empty(), "spend is off");
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
