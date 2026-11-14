use super::*;
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use serde_json::json;

const NOW: i64 = 1_793_000_000_000;
const HOUR: i64 = 3_600_000;
const NEXT_DAY_8AM: i64 = 1_793_088_000_000;
const OPENROUTER_PLACE: &str = "openrouter.ai";

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    (root, db)
}

fn managed(harness: &str) -> ModelConnection {
    ModelConnection {
        id: reference(&format!("{harness}-main")),
        provider: reference("vendor"),
        harness: reference(harness),
        protocol: reference("native"),
        account_ref: reference("owner-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "native-default".into(),
            author: reference("vendor"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference(&format!("{harness}-profile")),
        },
    }
}

fn keyed(url: &str, key: bool) -> ModelConnection {
    ModelConnection {
        id: reference("keyed-main"),
        provider: reference("openai-compatible"),
        harness: reference("rcli"),
        protocol: reference("openai-chat"),
        account_ref: reference("owner-account"),
        endpoint: ModelEndpoint::Http { url: url.into() },
        model: ModelReference {
            id: "some-model".into(),
            author: reference("openai-compatible"),
        },
        auth: if key {
            ModelAuthentication::ApiKey {
                credential_ref: reference("keyed-key"),
            }
        } else {
            ModelAuthentication::None {}
        },
    }
}

fn resets(count: u32, kinds: Vec<ResetKind>, next_expiry_ms: Option<i64>) -> Resets {
    Resets {
        count,
        kinds,
        next_expiry_ms,
    }
}

fn name(harness: &str) -> &'static str {
    crate::harnesses::spec(harness).unwrap().name
}

#[test]
fn every_catalog_harness_has_exactly_one_adapter_entry() {
    let mut catalog: Vec<&str> = crate::harnesses::specs()
        .iter()
        .map(|spec| spec.id)
        .collect();
    let mut table: Vec<&str> = ADAPTERS.iter().map(|(id, _)| *id).collect();
    catalog.sort_unstable();
    table.sort_unstable();
    assert_eq!(
        table, catalog,
        "a harness added to the catalog needs its own entry"
    );
    table.dedup();
    assert_eq!(table.len(), ADAPTERS.len());
    for (id, entry) in ADAPTERS {
        assert!(
            matches!(
                entry,
                Adapter::Codex | Adapter::Claude | Adapter::NotReported
            ),
            "{id}: key adapters are chosen by the account's host, never by its harness"
        );
        assert_eq!(adapter(id), Some(entry), "{id}");
    }
    assert_eq!(adapter("codex"), Some(Adapter::Codex));
    assert_eq!(adapter("claude"), Some(Adapter::Claude));
    assert_eq!(adapter("claude-code"), Some(Adapter::Claude));
    assert_eq!(adapter("gemini-cli"), Some(Adapter::NotReported));
    assert_eq!(adapter("made-up"), None);
}

#[test]
fn a_field_a_harness_does_not_report_names_that_harness() {
    assert_eq!(
        not_reported("codex"),
        format!("not reported by {}", name("codex"))
    );
    assert_eq!(
        not_reported("claude"),
        format!("not reported by {}", name("claude-code"))
    );
    assert_eq!(
        not_reported("gemini-cli"),
        format!("not reported by {}", name("gemini-cli"))
    );
    assert_eq!(not_reported("made-up"), "not reported by made-up");
}

#[test]
fn codex_usage_reply_gives_its_free_resets_and_credits_in_engine_words() {
    let body = json!({
        "plan_type": "pro",
        "rate_limit": {"primary_window": {"used_percent": 10, "limit_window_seconds": 18000}},
        "rate_limit_reset_credits": {"available_count": 2},
        "credits": {"has_credits": true, "unlimited": false, "balance": "12.5"}
    });
    assert_eq!(
        codex_from_usage(&body),
        Extras {
            resets: Field::Known(resets(2, vec![ResetKind::AllLimits], None)),
            credits: Field::Known("12.5 credits".into()),
        }
    );
    let none = json!({
        "rate_limit_reset_credits": {"available_count": 0},
        "credits": {"has_credits": false, "unlimited": false, "balance": null}
    });
    assert_eq!(
        codex_from_usage(&none),
        Extras {
            resets: Field::Known(resets(0, vec![], None)),
            credits: Field::Known("No credits".into()),
        }
    );
    let unlimited = json!({"credits": {"has_credits": true, "unlimited": true}});
    assert_eq!(
        codex_from_usage(&unlimited).credits,
        Field::Known("Unlimited credits".into())
    );
    let number = json!({"credits": {"has_credits": true, "unlimited": false, "balance": 7}});
    assert_eq!(
        codex_from_usage(&number).credits,
        Field::Known("7 credits".into())
    );
}

#[test]
fn a_codex_reply_without_the_fields_reads_absent_never_zero() {
    let bare = json!({"rate_limit": {"primary_window": {"used_percent": 10}}});
    assert_eq!(
        codex_from_usage(&bare),
        Extras {
            resets: Field::Absent,
            credits: Field::Absent,
        }
    );
}

#[test]
fn a_malformed_codex_field_reads_absent_and_leaves_the_other() {
    for count in [
        json!("two"),
        json!(-1),
        json!(1.5),
        json!(RESETS_MAX + 1),
        json!(null),
        json!(1_000_000_000_u64),
    ] {
        let body = json!({
            "rate_limit_reset_credits": {"available_count": count},
            "credits": {"has_credits": false, "unlimited": false}
        });
        let read = codex_from_usage(&body);
        assert_eq!(read.resets, Field::Absent, "{count}");
        assert_eq!(read.credits, Field::Known("No credits".into()), "{count}");
    }
    for credits in [
        json!({"has_credits": true, "unlimited": false, "balance": "12; rm"}),
        json!({"has_credits": "yes"}),
        json!({"has_credits": true, "unlimited": false, "balance": -3}),
        json!("plenty"),
    ] {
        let body = json!({"rate_limit_reset_credits": {"available_count": 1}, "credits": credits});
        let read = codex_from_usage(&body);
        assert_eq!(read.credits, Field::Absent, "{credits}");
        assert_eq!(
            read.resets,
            Field::Known(resets(1, vec![ResetKind::AllLimits], None)),
            "{credits}"
        );
    }
}

#[test]
fn codex_app_server_resets_give_the_next_expiry_and_never_an_id() {
    let id = "credit-secret-7f3a";
    let seconds = NOW / 1000;
    let result = json!({
        "accountId": "account-secret-1",
        "rateLimits": {},
        "rateLimitResetCredits": {"availableCount": 2, "credits": [
            {"id": id, "status": "available", "resetType": "codexRateLimits",
             "grantedAt": seconds - 3600, "expiresAt": seconds + 7200, "title": "Free reset"},
            {"id": "credit-b", "status": "available", "resetType": "codexRateLimits",
             "grantedAt": seconds - 60, "expiresAt": seconds + 3600},
            {"id": "credit-c", "status": "redeemed", "resetType": "codexRateLimits",
             "grantedAt": seconds - 60, "expiresAt": seconds + 60},
            {"id": "credit-d", "status": "available", "resetType": "codexRateLimits",
             "grantedAt": seconds - 7200, "expiresAt": seconds - 60}
        ]}
    });
    let read = codex_reset_credits(&result, NOW);
    assert_eq!(
        read,
        Field::Known(resets(2, vec![ResetKind::AllLimits], Some(NOW + HOUR)))
    );
    let shown = format!("{read:?}");
    for secret in [id, "credit-b", "account-secret-1"] {
        assert!(!shown.contains(secret), "{secret} leaked into {shown}");
    }
    let Field::Known(known) = read else {
        unreachable!()
    };
    let wire = serde_json::to_string(&known).unwrap();
    assert!(!wire.contains("credit-"), "{wire}");
    let count_only = json!({"rateLimitResetCredits": {"availableCount": 1}});
    assert_eq!(
        codex_reset_credits(&count_only, NOW),
        Field::Known(resets(1, vec![ResetKind::AllLimits], None))
    );
}

#[test]
fn a_missing_or_malformed_codex_app_server_reset_reads_absent() {
    let seconds = NOW / 1000;
    for result in [
        json!({"rateLimits": {}}),
        json!({"rateLimitResetCredits": null}),
        json!({"rateLimitResetCredits": {"availableCount": "1"}}),
        json!({"rateLimitResetCredits": {"availableCount": -1}}),
        json!({"rateLimitResetCredits": {"availableCount": 1, "credits": "many"}}),
        json!({"rateLimitResetCredits": {"availableCount": 1, "credits": [
            {"status": "available", "resetType": "codexRateLimits", "expiresAt": "soon"}
        ]}}),
        json!({"rateLimitResetCredits": {"availableCount": 1, "credits": [
            {"status": "available", "resetType": "codexRateLimits", "expiresAt": seconds * 1_000_000}
        ]}}),
    ] {
        assert_eq!(codex_reset_credits(&result, NOW), Field::Absent, "{result}");
    }
}

#[test]
fn claude_detail_sums_unpaused_grants_and_takes_their_earliest_end() {
    let body = json!({"cedar_ember": {
        "eligible": true,
        "at_limit": false,
        "next_grant_id": "grant-secret-91",
        "weekly_resets_at": "2026-10-30T00:00:00+00:00",
        "grants": [
            {"id": "grant-secret-91", "resets_left": 2, "paused": false, "ends_at": "2026-10-27T08:00:00+00:00"},
            {"resets_left": 1, "ends_at": "2026-10-28T08:00:00+00:00"},
            {"resets_left": 5, "paused": true, "ends_at": "2026-10-26T08:00:00+00:00"},
            {"resets_left": 0, "paused": false, "ends_at": "2026-10-25T08:00:00+00:00"}
        ]
    }});
    let read = claude_resets(&body, NOW);
    assert_eq!(read, Field::Known(resets(3, vec![], Some(NEXT_DAY_8AM))));
    assert!(!format!("{read:?}").contains("grant-secret-91"));
    let ineligible = json!({"cedar_ember": {"eligible": false, "ineligible_reason": "no_grant"}});
    assert_eq!(
        claude_resets(&ineligible, NOW),
        Field::Known(resets(0, vec![], None))
    );
}

#[test]
fn a_missing_or_malformed_claude_detail_reads_absent() {
    for body in [
        json!({"five_hour": {}}),
        json!({"cedar_ember": null}),
        json!({"cedar_ember": {"eligible": "yes"}}),
        json!({"cedar_ember": {"eligible": true, "grants": "some"}}),
        json!({"cedar_ember": {"eligible": true, "grants": [{"resets_left": "2"}]}}),
        json!({"cedar_ember": {"eligible": true, "grants": [{"resets_left": -1}]}}),
        json!({"cedar_ember": {"eligible": true, "grants": [{"resets_left": RESETS_MAX + 1}]}}),
        json!({"cedar_ember": {"eligible": true, "grants": [{"resets_left": 1, "paused": "no"}]}}),
        json!({"cedar_ember": {"eligible": true, "grants": [{"resets_left": 1, "ends_at": "tomorrow"}]}}),
    ] {
        assert_eq!(claude_resets(&body, NOW), Field::Absent, "{body}");
    }
}

#[test]
fn claude_extra_usage_reads_as_engine_words() {
    assert_eq!(
        claude_credits(&json!({"extra_usage": {"is_enabled": false}})),
        Field::Known("Extra usage off".into())
    );
    assert_eq!(
        claude_credits(&json!({"extra_usage": {
            "is_enabled": true, "monthly_limit": 50, "used_credits": 12.5, "currency": "USD"
        }})),
        Field::Known("12.50 of 50.00 USD extra usage used".into())
    );
    assert_eq!(
        claude_credits(&json!({"extra_usage": {"is_enabled": true}})),
        Field::Known("Extra usage on".into())
    );
    for body in [
        json!({"five_hour": {}}),
        json!({"extra_usage": null}),
        json!({"extra_usage": {"is_enabled": "yes"}}),
        json!({"extra_usage": {"is_enabled": true, "monthly_limit": 50, "used_credits": -1, "currency": "USD"}}),
        json!({"extra_usage": {"is_enabled": true, "monthly_limit": 50, "used_credits": 1, "currency": "U$D"}}),
    ] {
        assert_eq!(claude_credits(&body), Field::Absent, "{body}");
    }
}

#[test]
fn a_saved_key_gets_a_balance_call_only_on_its_providers_own_https_host() {
    assert_eq!(
        key_adapter("https://openrouter.ai/api/v1"),
        Some(Adapter::OpenRouter)
    );
    assert_eq!(
        key_adapter("https://api.deepseek.com"),
        Some(Adapter::DeepSeek)
    );
    assert_eq!(
        key_adapter("https://api.deepseek.com/v1"),
        Some(Adapter::DeepSeek)
    );
    assert_eq!(
        key_adapter("https://api.moonshot.ai/v1"),
        Some(Adapter::Moonshot)
    );
    assert_eq!(
        key_adapter("https://api.moonshot.cn/v1"),
        Some(Adapter::Moonshot)
    );
    assert_eq!(
        key_adapter("https://openrouter.ai:443/api/v1"),
        Some(Adapter::OpenRouter)
    );
    assert_eq!(
        key_adapter("https://OpenRouter.AI/api/v1"),
        Some(Adapter::OpenRouter)
    );
    for other in [
        "http://openrouter.ai/api/v1",
        "https://openrouter.ai.example.com/api/v1",
        "https://openrouter.ai.example.net",
        "https://evil-openrouter.ai/api/v1",
        "https://evilopenrouter.ai",
        "https://example.net/openrouter.ai/",
        "https://example.net/?host=openrouter.ai",
        "https://user:secret@openrouter.ai/api/v1",
        "https://user@openrouter.ai/api/v1",
        "https://openrouter.ai:8443/api/v1",
        "https://api.deepseek.com:8443",
        "https://api.deepseek.com.example.com",
        "https://deepseek.com",
        "https://platform.moonshot.ai/v1",
        "https://api.openai.com/v1",
        "not a url",
        "",
    ] {
        assert_eq!(key_adapter(other), None, "{other}");
    }
}

#[test]
fn a_balance_call_stays_on_the_accounts_own_host() {
    let at = |adapter, base| balance_url(adapter, base).map(|url| url.to_string());
    assert_eq!(
        at(Adapter::OpenRouter, "https://openrouter.ai/api/v1?x=1#top").as_deref(),
        Some("https://openrouter.ai/api/v1/credits")
    );
    assert_eq!(
        at(
            Adapter::OpenRouter,
            "https://openrouter.ai/other/path/../v9?key=sk-or-1"
        )
        .as_deref(),
        Some("https://openrouter.ai/api/v1/credits"),
        "the balance URL is a constant, never joined onto the base URL"
    );
    assert_eq!(
        at(
            Adapter::DeepSeek,
            "https://api.deepseek.com:443/v1?key=sk-1"
        )
        .as_deref(),
        Some("https://api.deepseek.com/user/balance")
    );
    assert_eq!(
        at(Adapter::DeepSeek, "https://api.deepseek.com/v1").as_deref(),
        Some("https://api.deepseek.com/user/balance")
    );
    assert_eq!(
        at(Adapter::Moonshot, "https://api.moonshot.cn/v1").as_deref(),
        Some("https://api.moonshot.cn/v1/users/me/balance")
    );
    assert_eq!(
        at(Adapter::Moonshot, "https://api.moonshot.ai/v1/").as_deref(),
        Some("https://api.moonshot.ai/v1/users/me/balance")
    );
    assert_eq!(at(Adapter::OpenRouter, "https://api.deepseek.com"), None);
    assert_eq!(
        at(
            Adapter::OpenRouter,
            "https://openrouter.ai.example.com/api/v1"
        ),
        None
    );
    assert_eq!(
        at(Adapter::OpenRouter, "https://example.net/openrouter.ai/"),
        None
    );
    assert_eq!(at(Adapter::DeepSeek, "https://api.deepseek.com:8443"), None);
    assert_eq!(
        at(Adapter::DeepSeek, "https://user:pw@api.deepseek.com"),
        None
    );
    assert_eq!(at(Adapter::Codex, "https://openrouter.ai/api/v1"), None);
    assert_eq!(
        at(Adapter::NotReported, "https://openrouter.ai/api/v1"),
        None
    );
}

#[test]
fn key_balances_read_as_engine_words_in_the_providers_currency() {
    assert_eq!(
        key_balance(
            Adapter::OpenRouter,
            &json!({"data": {"total_credits": 20, "total_usage": 7.5}})
        ),
        Field::Known("12.50 USD left".into())
    );
    assert_eq!(
        key_balance(
            Adapter::DeepSeek,
            &json!({"is_available": true, "balance_infos": [
                {"currency": "CNY", "total_balance": "110.00", "granted_balance": "10.00", "topped_up_balance": "100.00"}
            ]})
        ),
        Field::Known("110.00 CNY left".into())
    );
    assert_eq!(
        key_balance(
            Adapter::DeepSeek,
            &json!({"is_available": true, "balance_infos": [
                {"currency": "CNY", "total_balance": "110.00"},
                {"currency": "USD", "total_balance": "2.00"}
            ]})
        ),
        Field::Known("110.00 CNY and 2.00 USD left".into())
    );
    assert_eq!(
        key_balance(
            Adapter::Moonshot,
            &json!({"code": 0, "data": {"available_balance": 49.58894, "voucher_balance": 46.58893, "cash_balance": 3.00001}, "scode": "0x0", "status": true})
        ),
        Field::Known("49.59 available".into())
    );
}

#[test]
fn a_malformed_key_balance_reads_absent() {
    for (adapter, body) in [
        (Adapter::OpenRouter, json!({"data": null})),
        (
            Adapter::OpenRouter,
            json!({"data": {"total_credits": "20", "total_usage": 1}}),
        ),
        (
            Adapter::DeepSeek,
            json!({"is_available": true, "balance_infos": "lots"}),
        ),
        (
            Adapter::DeepSeek,
            json!({"is_available": true, "balance_infos": [{"currency": "CNY", "total_balance": "1e9; x"}]}),
        ),
        (
            Adapter::DeepSeek,
            json!({"is_available": true, "balance_infos": [{"currency": "C N", "total_balance": "1.00"}]}),
        ),
        (
            Adapter::Moonshot,
            json!({"code": 1, "data": {"available_balance": 3}}),
        ),
        (
            Adapter::Moonshot,
            json!({"code": 0, "data": {"available_balance": "3"}}),
        ),
        (
            Adapter::Codex,
            json!({"data": {"total_credits": 20, "total_usage": 1}}),
        ),
        (Adapter::NotReported, json!({})),
    ] {
        assert_eq!(
            key_balance(adapter, &body),
            Field::Absent,
            "{adapter:?} {body}"
        );
    }
}

#[test]
fn stored_extras_keep_a_field_not_asked_clear_an_absent_one_and_replace_a_known_one() {
    let (_root, db) = store();
    let account = reference("codex-main");
    assert_eq!(
        extras_of(&db, &account, NOW).unwrap(),
        StoredExtras::default()
    );
    let first = Extras {
        resets: Field::Known(resets(2, vec![ResetKind::AllLimits], Some(NOW + HOUR))),
        credits: Field::Known("12.5 credits".into()),
    };
    assert_eq!(record_extras(&db, &account, &first, NOW).unwrap(), None);
    let stored = StoredExtras {
        resets: Some(resets(2, vec![ResetKind::AllLimits], Some(NOW + HOUR))),
        credits: Some("12.5 credits".into()),
        resets_checked_ms: Some(NOW),
        credits_checked_ms: Some(NOW),
        resets_gained_ms: None,
        resets_unread: false,
    };
    assert_eq!(extras_of(&db, &account, NOW).unwrap(), stored);
    assert_eq!(
        record_extras(&db, &account, &Extras::not_asked(), NOW + 1).unwrap(),
        Some(2)
    );
    assert_eq!(extras_of(&db, &account, NOW + 1).unwrap(), stored);
    let dropped = Extras {
        resets: Field::Absent,
        credits: Field::NotAsked,
    };
    assert_eq!(
        record_extras(&db, &account, &dropped, NOW + 2).unwrap(),
        Some(2)
    );
    let cleared = extras_of(&db, &account, NOW + 2).unwrap();
    assert_eq!(cleared.resets, None);
    assert_eq!(cleared.resets_checked_ms, Some(NOW + 2));
    assert_eq!(cleared.credits.as_deref(), Some("12.5 credits"));
    assert_eq!(cleared.credits_checked_ms, Some(NOW));
    let zero = Extras {
        resets: Field::Known(resets(0, vec![], None)),
        credits: Field::Absent,
    };
    assert_eq!(record_extras(&db, &account, &zero, NOW + 3).unwrap(), None);
    let after = extras_of(&db, &account, NOW + 3).unwrap();
    assert_eq!(
        after.resets,
        Some(resets(0, vec![], None)),
        "0 is known, not missing"
    );
    assert_eq!(after.credits, None);
    assert_eq!(after.credits_checked_ms, Some(NOW + 3));
}

#[test]
fn a_count_whose_next_expiry_passed_reads_not_reported() {
    let (_root, db) = store();
    let account = reference("claude-main");
    let read = Extras {
        resets: Field::Known(resets(1, vec![], Some(NOW + HOUR))),
        credits: Field::NotAsked,
    };
    record_extras(&db, &account, &read, NOW).unwrap();
    assert_eq!(
        extras_of(&db, &account, NOW + HOUR - 1).unwrap().resets,
        Some(resets(1, vec![], Some(NOW + HOUR)))
    );
    let expired = extras_of(&db, &account, NOW + HOUR).unwrap();
    assert_eq!(expired.resets, None);
    assert_eq!(expired.resets_checked_ms, Some(NOW));
    assert_eq!(
        record_extras(&db, &account, &Extras::not_asked(), NOW + HOUR).unwrap(),
        None,
        "an expired count is no count before"
    );
}

#[test]
fn only_a_count_above_the_unexpired_one_before_stamps_new_resets() {
    let (_root, db) = store();
    let account = reference("codex-main");
    let known = |count, expiry| Extras {
        resets: Field::Known(resets(count, vec![ResetKind::AllLimits], expiry)),
        credits: Field::NotAsked,
    };
    let gained = |at| extras_of(&db, &account, at).unwrap().resets_gained_ms;
    record_extras(&db, &account, &known(1, None), NOW).unwrap();
    assert_eq!(gained(NOW), None, "a first read is not an arrival");
    assert_eq!(
        record_extras(&db, &account, &known(3, None), NOW + 1).unwrap(),
        Some(1)
    );
    assert_eq!(gained(NOW + 1), Some(NOW + 1));
    record_extras(&db, &account, &known(2, Some(NOW + 10)), NOW + 2).unwrap();
    assert_eq!(
        gained(NOW + 2),
        Some(NOW + 1),
        "a spent reset keeps the stamp"
    );
    assert_eq!(
        record_extras(&db, &account, &known(5, None), NOW + 20).unwrap(),
        None
    );
    assert_eq!(
        gained(NOW + 20),
        Some(NOW + 1),
        "a read after an expiry is not an arrival"
    );
    let gone = Extras {
        resets: Field::Absent,
        credits: Field::NotAsked,
    };
    record_extras(&db, &account, &gone, NOW + 21).unwrap();
    record_extras(&db, &account, &known(9, None), NOW + 22).unwrap();
    assert_eq!(
        gained(NOW + 22),
        Some(NOW + 1),
        "a read after an absent one is not an arrival"
    );
}

#[test]
fn an_out_of_bounds_value_clears_its_field_and_never_fails_the_store() {
    let (_root, db) = store();
    let account = reference("codex-main");
    let good = Extras {
        resets: Field::Known(resets(2, vec![ResetKind::AllLimits], None)),
        credits: Field::Known("12.5 credits".into()),
    };
    for bad in [
        Extras {
            resets: Field::Known(resets(RESETS_MAX + 1, vec![], None)),
            credits: Field::Known("x".repeat(CREDITS_BYTES_MAX + 1)),
        },
        Extras {
            resets: Field::Known(resets(1, vec![], Some(0))),
            credits: Field::Known("12; DROP TABLE".into()),
        },
        Extras {
            resets: Field::Known(resets(1, vec![], Some(crate::usage::RESET_MS_MAX + 1))),
            credits: Field::Known(String::new()),
        },
    ] {
        record_extras(&db, &account, &good, NOW).unwrap();
        assert!(extras_of(&db, &account, NOW).unwrap().resets.is_some());
        record_extras(&db, &account, &bad, NOW + 1).unwrap();
        let stored = extras_of(&db, &account, NOW + 1).unwrap();
        assert_eq!(stored.resets, None, "{bad:?}");
        assert_eq!(stored.credits, None, "{bad:?}");
    }
}

#[test]
fn the_extras_store_has_no_column_for_any_id() {
    let (_root, db) = store();
    let mut columns: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_info('account_limit_extras')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    columns.sort();
    assert_eq!(
        columns,
        [
            "account",
            "credits",
            "credits_checked_ms",
            "reset_kinds",
            "resets_checked_ms",
            "resets_count",
            "resets_expire_ms",
            "resets_gained_ms",
        ]
    );
}

#[test]
fn removing_an_account_drops_its_extras() {
    let (_root, db) = store();
    let account = reference("codex-main");
    let kept = reference("claude-main");
    let read = Extras {
        resets: Field::Known(resets(2, vec![ResetKind::AllLimits], None)),
        credits: Field::Known("No credits".into()),
    };
    record_extras(&db, &account, &read, NOW).unwrap();
    record_extras(&db, &kept, &read, NOW).unwrap();
    assert_ne!(
        extras_of(&db, &account, NOW).unwrap(),
        StoredExtras::default()
    );
    crate::usage::forget_limits(&db, &account).unwrap();
    assert_eq!(
        extras_of(&db, &account, NOW).unwrap(),
        StoredExtras::default()
    );
    assert_ne!(extras_of(&db, &kept, NOW).unwrap(), StoredExtras::default());
}

#[test]
fn an_open_pools_page_turns_the_detail_reads_on_at_most_once_per_floor() {
    let (_root, db) = store();
    assert!(
        !detail_due(&db, None, NOW).unwrap(),
        "no Pools lease, no private read"
    );
    request_detail(&db, NOW).unwrap();
    assert!(detail_due(&db, None, NOW).unwrap());
    assert!(detail_due(&db, Some(NOW - DETAIL_EVERY_MS), NOW).unwrap());
    assert!(
        !detail_due(&db, Some(NOW), NOW + 60_000).unwrap(),
        "a 60 s reread never means a private read each time"
    );
    assert!(
        !detail_due(&db, None, NOW + DETAIL_LEASE_MS).unwrap(),
        "the lease ran out"
    );
    for minute in 1..=10 {
        request_detail(&db, NOW + DETAIL_LEASE_MS + minute * 60_000).unwrap();
    }
    let later = NOW + DETAIL_LEASE_MS + 10 * 60_000;
    assert!(detail_due(&db, Some(later - DETAIL_EVERY_MS), later).unwrap());
    assert!(!detail_due(&db, Some(later - DETAIL_EVERY_MS + 1), later).unwrap());
    request_detail(&db, NOW).unwrap();
    assert!(
        detail_due(&db, None, later + DETAIL_LEASE_MS - 1).unwrap(),
        "an older ask never shortens the lease"
    );
}

#[test]
fn each_account_kind_gets_its_contract_from_the_engine() {
    let stored = StoredExtras {
        resets: Some(resets(2, vec![ResetKind::AllLimits], Some(NOW + HOUR))),
        credits: Some("12.5 credits".into()),
        resets_checked_ms: Some(NOW),
        credits_checked_ms: Some(NOW + 1),
        resets_gained_ms: Some(NOW - 1),
        resets_unread: false,
    };
    let codex = limit_extras(&managed("codex"), &stored);
    assert_eq!(
        codex,
        LimitExtras {
            harness: name("codex").into(),
            adapter: Adapter::Codex,
            may_break: true,
            resets: stored.resets.clone(),
            credits: stored.credits.clone(),
            spend: Spend::InApp,
            not_reported: format!("not reported by {}", name("codex")),
            resets_checked_ms: Some(NOW),
            credits_checked_ms: Some(NOW + 1),
            resets_gained_ms: Some(NOW - 1),
            resets_unread: false,
        }
    );
    let wire = serde_json::to_value(&codex).unwrap();
    assert_eq!(wire["adapter"], "codex");
    assert_eq!(wire["spend"], json!({"kind": "in_app"}));
    assert_eq!(wire["resets"]["kinds"], json!(["all_limits"]));
    let claude = limit_extras(&managed("claude"), &stored);
    assert_eq!(claude.adapter, Adapter::Claude);
    assert!(
        claude.may_break,
        "the Claude detail and spend are private calls"
    );
    assert_eq!(claude.harness, name("claude-code"));
    assert_eq!(claude.spend, Spend::InApp);
    let openrouter = limit_extras(&keyed("https://openrouter.ai/api/v1", true), &stored);
    assert_eq!(openrouter.adapter, Adapter::OpenRouter);
    assert!(!openrouter.may_break);
    assert_eq!(openrouter.resets, None, "a key balance has no free resets");
    assert_eq!(openrouter.resets_gained_ms, None);
    assert_eq!(openrouter.credits, stored.credits);
    assert_eq!(
        openrouter.spend,
        Spend::Elsewhere {
            place: OPENROUTER_PLACE.into()
        }
    );
    assert_eq!(
        serde_json::to_value(&openrouter.spend).unwrap(),
        json!({"kind": "elsewhere", "place": OPENROUTER_PLACE})
    );
    assert_eq!(
        openrouter.not_reported,
        format!("not reported by {}", name("rcli"))
    );
    for (connection, harness) in [
        (
            keyed("https://api.openai.com/v1", true),
            name("rcli").to_owned(),
        ),
        (
            keyed("https://openrouter.ai/api/v1", false),
            name("rcli").to_owned(),
        ),
        (managed("gemini-cli"), name("gemini-cli").to_owned()),
        (managed("made-up"), "made-up".to_owned()),
    ] {
        let contract = limit_extras(&connection, &stored);
        assert_eq!(contract.adapter, Adapter::NotReported, "{harness}");
        assert_eq!(contract.harness, harness);
        assert_eq!(contract.resets, None, "{harness}: stale values never show");
        assert_eq!(contract.credits, None, "{harness}");
        assert_eq!(contract.resets_gained_ms, None, "{harness}");
        assert_eq!(contract.spend, Spend::NotReported, "{harness}");
        assert_eq!(contract.not_reported, format!("not reported by {harness}"));
    }
}

#[test]
fn an_account_is_read_by_its_harness_when_managed_and_by_its_host_when_keyed() {
    assert_eq!(account_adapter(&managed("codex")), Adapter::Codex);
    assert_eq!(account_adapter(&managed("claude")), Adapter::Claude);
    assert_eq!(account_adapter(&managed("kimi")), Adapter::NotReported);
    assert_eq!(
        account_adapter(&keyed("https://api.deepseek.com/v1", true)),
        Adapter::DeepSeek
    );
    assert_eq!(
        account_adapter(&keyed("https://api.moonshot.ai/v1", true)),
        Adapter::Moonshot
    );
    assert_eq!(
        account_adapter(&keyed("https://api.moonshot.ai/v1", false)),
        Adapter::NotReported,
        "no saved key, nothing to read the balance with"
    );
}

#[test]
fn where_to_spend_a_key_balance_comes_from_constants_never_from_the_base_url() {
    let stored = StoredExtras {
        credits: Some("12.50 USD left".into()),
        ..StoredExtras::default()
    };
    for (url, place) in [
        (
            "https://openrouter.ai/api/v1?key=sk-or-secret",
            OPENROUTER_PLACE,
        ),
        (
            "https://api.deepseek.com/v1?api_key=sk-ds-secret#sk-frag",
            "platform.deepseek.com",
        ),
        (
            "https://api.moonshot.ai/v1?token=sk-ms-secret",
            "platform.moonshot.ai",
        ),
        (
            "https://api.moonshot.cn/v1?token=sk-ms-secret",
            "platform.moonshot.cn",
        ),
    ] {
        let contract = limit_extras(&keyed(url, true), &stored);
        assert_eq!(
            contract.spend,
            Spend::Elsewhere {
                place: place.into()
            },
            "{url}"
        );
        let wire = serde_json::to_string(&contract).unwrap();
        for secret in [
            "sk-or-secret",
            "sk-ds-secret",
            "sk-frag",
            "sk-ms-secret",
            "key=",
            "token=",
        ] {
            assert!(
                !wire.contains(secret),
                "{secret} reached the contract: {wire}"
            );
        }
    }
    let userinfo = limit_extras(
        &keyed("https://owner:hunter2@openrouter.ai/api/v1", true),
        &stored,
    );
    assert_eq!(userinfo.adapter, Adapter::NotReported);
    let wire = serde_json::to_string(&userinfo).unwrap();
    assert!(
        !wire.contains("hunter2") && !wire.contains("owner:"),
        "{wire}"
    );
}

#[test]
fn reset_kinds_are_deduped_and_at_most_four_on_the_wire_and_in_the_store() {
    let read = |kinds: serde_json::Value| {
        serde_json::from_value::<Resets>(
            json!({"count": 1, "kinds": kinds, "next_expiry_ms": null}),
        )
    };
    assert_eq!(
        read(json!(["five_hour", "weekly", "model", "all_limits"]))
            .unwrap()
            .kinds
            .len(),
        KINDS_MAX
    );
    assert!(
        read(json!(["weekly", "weekly"])).is_err(),
        "duplicates are refused"
    );
    assert!(
        read(json!([
            "five_hour",
            "weekly",
            "model",
            "all_limits",
            "weekly"
        ]))
        .is_err(),
        "more than {KINDS_MAX} is refused"
    );
    let (_root, db) = store();
    let account = reference("codex-main");
    let good = Extras {
        resets: Field::Known(resets(1, vec![ResetKind::AllLimits], None)),
        credits: Field::NotAsked,
    };
    record_extras(&db, &account, &good, NOW).unwrap();
    assert!(extras_of(&db, &account, NOW).unwrap().resets.is_some());
    let doubled = Extras {
        resets: Field::Known(resets(1, vec![ResetKind::Weekly, ResetKind::Weekly], None)),
        credits: Field::NotAsked,
    };
    record_extras(&db, &account, &doubled, NOW + 1).unwrap();
    assert_eq!(extras_of(&db, &account, NOW + 1).unwrap().resets, None);
}

/// An ineligible Claude account holds no free resets only when Anthropic's reason is about the
/// account; a reason about who asks, a missing one or one this version does not know reads
/// absent, never 0 (owner bug A, DEC-154 addendum on ineligible reasons).
#[test]
fn an_ineligible_claude_account_reads_none_only_for_a_reason_about_the_account() {
    for reason in ["no_grant", "tier", "seat", "tenure"] {
        let body = json!({"cedar_ember": {"eligible": false, "ineligible_reason": reason}});
        assert_eq!(
            claude_resets(&body, NOW),
            Field::Known(resets(0, vec![], None)),
            "{reason}"
        );
    }
    for reason in [
        "surface",
        "cli_version",
        "mobile",
        "config_off",
        "other_experiment",
        "unavailable",
        "unknown",
        "plan",
    ] {
        let body = json!({"cedar_ember": {"eligible": false, "ineligible_reason": reason}});
        assert_eq!(claude_resets(&body, NOW), Field::Absent, "{reason}");
    }
    for body in [
        json!({"cedar_ember": {"eligible": false}}),
        json!({"cedar_ember": {"eligible": false, "ineligible_reason": null}}),
        json!({"cedar_ember": {"eligible": false, "ineligible_reason": 7}}),
    ] {
        assert_eq!(claude_resets(&body, NOW), Field::Absent, "{body}");
    }
}

/// A free resets read that found no count leaves the account unread until a count is read
/// again; an account never read, a credits read alone and an expired count are not unread.
#[test]
fn a_resets_read_that_found_no_count_is_unread_and_an_expired_count_is_not() {
    let (_root, db) = store();
    let account = reference("claude-main");
    assert!(!extras_of(&db, &account, NOW).unwrap().resets_unread);
    let credits = Extras {
        resets: Field::NotAsked,
        credits: Field::Known("Extra usage: off".into()),
    };
    record_extras(&db, &account, &credits, NOW).unwrap();
    assert!(
        !extras_of(&db, &account, NOW).unwrap().resets_unread,
        "a credits read is no resets read"
    );
    let absent = Extras {
        resets: Field::Absent,
        credits: Field::NotAsked,
    };
    record_extras(&db, &account, &absent, NOW + 1).unwrap();
    let unread = extras_of(&db, &account, NOW + 1).unwrap();
    assert_eq!(unread.resets, None);
    assert_eq!(unread.resets_checked_ms, Some(NOW + 1));
    assert!(unread.resets_unread);
    record_extras(&db, &account, &credits, NOW + 2).unwrap();
    assert!(extras_of(&db, &account, NOW + 2).unwrap().resets_unread);
    let one = Extras {
        resets: Field::Known(resets(1, vec![], Some(NOW + HOUR))),
        credits: Field::NotAsked,
    };
    record_extras(&db, &account, &one, NOW + 3).unwrap();
    assert!(!extras_of(&db, &account, NOW + 3).unwrap().resets_unread);
    let expired = extras_of(&db, &account, NOW + HOUR).unwrap();
    assert_eq!(expired.resets, None);
    assert!(!expired.resets_unread, "an expired count is no failed read");
}

/// The contract says unread only for an adapter that reads free resets, and a reply from an
/// engine without the flag reads as not unread, so a newer client keeps working on it.
#[test]
fn the_contract_says_unread_only_where_free_resets_are_read_and_an_older_reply_reads_not_unread() {
    let stored = StoredExtras {
        resets_checked_ms: Some(NOW),
        resets_unread: true,
        ..StoredExtras::default()
    };
    assert!(limit_extras(&managed("claude"), &stored).resets_unread);
    assert!(limit_extras(&managed("codex"), &stored).resets_unread);
    assert!(!limit_extras(&keyed("https://openrouter.ai/api/v1", true), &stored).resets_unread);
    let mut wire = serde_json::to_value(limit_extras(&managed("claude"), &stored)).unwrap();
    assert_eq!(
        wire.as_object_mut().unwrap().remove("resets_unread"),
        Some(true.into())
    );
    let older: LimitExtras = serde_json::from_value(wire).unwrap();
    assert!(!older.resets_unread);
}
