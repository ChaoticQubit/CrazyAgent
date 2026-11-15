use super::*;
use crate::config::{
    AccountPool, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};
use crate::credentials::SecretString;
use crate::github::test_server::{Answer, Server};
use crate::usage::extras::{Extras, Field, Resets, record_extras};
use crate::usage::{LimitSource, LimitWindow, Observation, record_observation};

const NOW: i64 = 1_793_000_000_000;
const HOUR: i64 = 60 * 60 * 1000;

struct Case {
    _root: tempfile::TempDir,
    db: Connection,
    server: Server,
    claude: url::Url,
    codex: url::Url,
    token: String,
}

impl Case {
    fn new(confirm: SpendConfirm, all_limited: AllLimited) -> Self {
        Self::answering(confirm, all_limited, |_| {
            Answer::json(200, serde_json::json!({"outcome": "reset"}))
        })
    }

    fn answering(
        confirm: SpendConfirm,
        all_limited: AllLimited,
        consume: impl Fn(usize) -> Answer + Send + Sync + 'static,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let db = crate::usage::open(root.path()).unwrap();
        write(
            &db,
            SpendSettings {
                confirm,
                all_limited,
                ..SpendSettings::default()
            },
        )
        .unwrap();
        let consumed = std::sync::atomic::AtomicUsize::new(0);
        let server = Server::start(move |seen, _| {
            if seen.path.ends_with("/rate-limit-reset-credits/consume") {
                consume(consumed.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
            } else {
                Answer::json(404, serde_json::json!({}))
            }
        });
        let base = format!("http://127.0.0.1:{}", server.port);
        let mut bytes = [0_u8; 12];
        getrandom::fill(&mut bytes).unwrap();
        let token = format!(
            "synthetic-{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        Self {
            _root: root,
            db,
            claude: url::Url::parse(&format!("{base}/api/oauth/usage")).unwrap(),
            codex: url::Url::parse(&format!("{base}/backend-api/wham/usage")).unwrap(),
            server,
            token,
        }
    }

    fn credential(&self) -> PollCredential {
        PollCredential {
            token: SecretString::new(self.token.as_bytes().to_vec()).unwrap(),
            account_id: Some("acct-synthetic".into()),
            plan: None,
        }
    }

    fn send_queued(&self, at: i64) -> usize {
        let attempts = queued(&self.db).unwrap();
        for attempt in &attempts {
            send_one(
                &self.db,
                attempt,
                Provider::Codex,
                &self.credential(),
                &self.claude,
                &self.codex,
                true,
                at,
            )
            .unwrap();
        }
        attempts.len()
    }

    fn consumes(&self) -> Vec<crate::github::test_server::Seen> {
        self.server
            .seen()
            .into_iter()
            .filter(|seen| seen.method == "POST")
            .collect()
    }

    fn limited_pool_with_resets(&self, at: i64) -> ModelAccess {
        let window = LimitWindow {
            used_percent: 100.0,
            resets_at_ms: Some(at + 2 * HOUR),
        };
        for account in ["codex-a", "codex-b"] {
            record_observation(
                &self.db,
                &r(account),
                &Observation {
                    plan: None,
                    five_hour: Some(window),
                    weekly: None,
                    provider: Some(Provider::Codex),
                },
                LimitSource::Poll,
                at,
            )
            .unwrap();
        }
        record_extras(
            &self.db,
            &r("codex-b"),
            &Extras {
                resets: Field::Known(Resets {
                    count: 2,
                    kinds: Vec::new(),
                    next_expiry_ms: None,
                }),
                credits: Field::NotAsked,
            },
            at,
        )
        .unwrap();
        ModelAccess {
            accounts: vec![r("codex-a"), r("codex-b")],
            connections: vec![managed("codex-a"), managed("codex-b")],
            pools: vec![AccountPool {
                id: r("pool-a"),
                name: Some("Daily work".into()),
                members: vec![r("codex-a"), r("codex-b")],
                projects: vec![r("demo")],
                roles: Vec::new(),
                fallback_allowed: false,
                provider_quota_policy: Default::default(),
            }],
        }
    }
}

fn r(text: &str) -> CredentialRef {
    CredentialRef::new(text).unwrap()
}

fn managed(id: &str) -> ModelConnection {
    ModelConnection {
        id: r(id),
        provider: r("openai"),
        harness: r("codex"),
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

fn refresh_asked(db: &Connection) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM usage_refresh WHERE singleton=1)",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn spends_one_reset_over_the_loopback_usage_server(confirm: SpendConfirm) {
    let case = Case::new(confirm, AllLimited::Notice);
    request(&case.db, &r("codex-a"), true, NOW).unwrap();
    let key = queued(&case.db).unwrap()[0].key.clone();
    assert_eq!(case.send_queued(NOW + 1), 1);
    let consumes = case.consumes();
    assert_eq!(consumes.len(), 1);
    assert_eq!(consumes[0].header("idempotency-key"), Some(key.as_str()));
    assert_eq!(consumes[0].body, b"{}");
    let shown = view(&case.db).unwrap();
    assert_eq!(shown.settings.confirm, confirm);
    assert_eq!(shown.attempts[0].outcome.as_deref(), Some("reset"));
    assert!(refresh_asked(&case.db));
    assert_eq!(
        case.send_queued(NOW + 2),
        0,
        "a settled spend is never sent again"
    );
    assert_eq!(case.consumes().len(), 1);
}

#[test]
fn option_spend_confirmation_confirm_each_spends_one_reset_over_the_loopback_usage_server() {
    spends_one_reset_over_the_loopback_usage_server(SpendConfirm::ConfirmEach);
}

#[test]
fn option_spend_confirmation_one_click_spends_one_reset_over_the_loopback_usage_server() {
    spends_one_reset_over_the_loopback_usage_server(SpendConfirm::OneClick);
}

#[test]
fn option_spend_confirmation_off_refuses_and_sends_nothing_to_the_loopback_usage_server() {
    let case = Case::new(SpendConfirm::ConfirmEach, AllLimited::Notice);
    request(&case.db, &r("codex-a"), true, NOW).unwrap();
    write(
        &case.db,
        SpendSettings {
            confirm: SpendConfirm::Off,
            all_limited: AllLimited::Notice,
            ..SpendSettings::default()
        },
    )
    .unwrap();
    assert_eq!(
        request(&case.db, &r("codex-b"), true, NOW).unwrap_err().0,
        "usage_spend_off"
    );
    assert_eq!(case.send_queued(NOW + 1), 1);
    assert!(case.consumes().is_empty());
    let shown = view(&case.db).unwrap();
    assert_eq!(shown.attempts.len(), 1);
    assert_eq!(
        shown.attempts[0].outcome.as_deref(),
        Some("usage_spend_off")
    );
}

#[test]
fn option_all_limited_notice_names_the_accounts_with_free_resets_and_sends_nothing() {
    let case = Case::new(SpendConfirm::ConfirmEach, AllLimited::Notice);
    let access = case.limited_pool_with_resets(NOW);
    track_pools(&case.db, &access, NOW + 1).unwrap();
    assert_eq!(case.send_queued(NOW + 2), 0);
    assert!(case.consumes().is_empty());
    let shown = view(&case.db).unwrap().limited_pools;
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].name.as_deref(), Some("Daily work"));
    assert_eq!(shown[0].with_resets, vec![r("codex-b")]);
}

#[test]
fn option_all_limited_no_hint_shows_nothing_and_sends_nothing() {
    let case = Case::new(SpendConfirm::ConfirmEach, AllLimited::NoHint);
    let access = case.limited_pool_with_resets(NOW);
    track_pools(&case.db, &access, NOW + 1).unwrap();
    assert_eq!(case.send_queued(NOW + 2), 0);
    assert!(case.consumes().is_empty());
    assert!(view(&case.db).unwrap().limited_pools.is_empty());
}

#[test]
fn option_all_limited_auto_spend_spends_one_reset_once_over_the_loopback_usage_server() {
    let case = Case::new(SpendConfirm::ConfirmEach, AllLimited::AutoSpend);
    let access = case.limited_pool_with_resets(NOW);
    track_pools(&case.db, &access, NOW + 1).unwrap();
    assert_eq!(case.send_queued(NOW + 2), 1);
    let consumes = case.consumes();
    assert_eq!(consumes.len(), 1);
    let shown = view(&case.db).unwrap();
    assert_eq!(shown.attempts[0].account, r("codex-b"));
    assert_eq!(shown.attempts[0].origin, Origin::Auto);
    assert_eq!(shown.attempts[0].outcome.as_deref(), Some("reset"));
    track_pools(&case.db, &access, NOW + HOUR + 3).unwrap();
    assert_eq!(
        case.send_queued(NOW + HOUR + 4),
        0,
        "past the pool's hour a stale reading spends nothing more"
    );
    assert_eq!(case.consumes().len(), 1);
}

#[test]
fn option_auto_spend_with_spend_confirmation_off_sends_nothing() {
    let case = Case::new(SpendConfirm::Off, AllLimited::AutoSpend);
    let access = case.limited_pool_with_resets(NOW);
    track_pools(&case.db, &access, NOW + 1).unwrap();
    assert_eq!(case.send_queued(NOW + 2), 0);
    assert!(case.consumes().is_empty());
}

fn a_second_spend_after_an_unclear_reply_sends_the_same_key(first: Answer) {
    let first = std::sync::Mutex::new(Some(first));
    let case = Case::answering(SpendConfirm::OneClick, AllLimited::Notice, move |n| match (
        n,
        first.lock().unwrap().take(),
    ) {
        (0, Some(answer)) => answer,
        _ => Answer::json(200, serde_json::json!({"outcome": "reset"})),
    });
    request(&case.db, &r("codex-a"), true, NOW).unwrap();
    assert_eq!(case.send_queued(NOW + 1), 1);
    assert!(refresh_asked(&case.db), "a poll is asked after any reply");
    let shown = view(&case.db).unwrap();
    assert_eq!(shown.attempts[0].state, AttemptState::Sent);
    assert_eq!(shown.attempts[0].outcome, None);
    assert_eq!(case.send_queued(NOW + 2), 0, "never re-sent on its own");
    request(&case.db, &r("codex-a"), true, NOW + 3).unwrap();
    assert_eq!(case.send_queued(NOW + 4), 1);
    let consumes = case.consumes();
    assert_eq!(consumes.len(), 2);
    let key = consumes[0].header("idempotency-key").unwrap();
    assert_eq!(consumes[1].header("idempotency-key"), Some(key));
    assert_eq!(
        view(&case.db).unwrap().attempts[0].outcome.as_deref(),
        Some("reset")
    );
}

#[test]
fn a_502_reply_then_a_second_spend_sends_the_same_idempotency_key() {
    a_second_spend_after_an_unclear_reply_sends_the_same_key(Answer::json(
        502,
        serde_json::json!({}),
    ));
}

#[test]
fn an_unknown_2xx_reply_then_a_second_spend_before_the_next_poll_sends_the_same_key() {
    a_second_spend_after_an_unclear_reply_sends_the_same_key(Answer::json(
        200,
        serde_json::json!({}),
    ));
}

#[test]
fn a_2xx_reply_that_is_not_json_then_a_second_spend_sends_the_same_key() {
    a_second_spend_after_an_unclear_reply_sends_the_same_key(Answer::bytes(
        200,
        b"not json".to_vec(),
    ));
}

#[test]
fn a_refusal_on_the_first_send_settles_and_asks_for_a_poll() {
    let case = Case::answering(SpendConfirm::OneClick, AllLimited::Notice, |_| {
        Answer::json(404, serde_json::json!({}))
    });
    request(&case.db, &r("codex-a"), true, NOW).unwrap();
    assert_eq!(case.send_queued(NOW + 1), 1);
    assert!(refresh_asked(&case.db));
    let shown = view(&case.db).unwrap();
    assert_eq!(shown.attempts[0].state, AttemptState::Settled);
    assert_eq!(
        shown.attempts[0].outcome.as_deref(),
        Some("usage_spend_refused")
    );
}

#[test]
fn the_report_and_spend_view_carry_no_key() {
    let case = Case::new(SpendConfirm::OneClick, AllLimited::Notice);
    request(&case.db, &r("codex-a"), true, NOW).unwrap();
    let key = queued(&case.db).unwrap()[0].key.clone();
    case.send_queued(NOW + 1);
    let report =
        crate::usage::report::report(&case.db, None, None, 0, NOW + 2, &[r("codex-a")]).unwrap();
    let wire = serde_json::to_string(&report).unwrap();
    assert!(!wire.contains(&key));
    assert!(!wire.contains(&case.token));
    assert!(!wire.contains("acct-synthetic"));
    assert_eq!(report.spend.attempts[0].outcome.as_deref(), Some("reset"));
}
