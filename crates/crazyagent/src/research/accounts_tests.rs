use super::*;
use crate::config::{AccountPool, ModelReference};

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

/// One endpoint account as the add-account screen stores it, under the RCLI harness name.
/// Inputs: the id, URL, wire format and the key reference, none for a keyless one.
/// Output: the stored connection.
fn endpoint(id: &str, url: &str, protocol: &str, key: Option<&str>) -> ModelConnection {
    ModelConnection {
        id: reference(id),
        provider: reference("openai-compatible"),
        harness: reference("rcli"),
        protocol: reference(protocol),
        account_ref: reference("owner-account"),
        endpoint: ModelEndpoint::Http { url: url.into() },
        model: ModelReference {
            id: "some-model".into(),
            author: reference("openai-compatible"),
        },
        auth: match key {
            Some(key) => ModelAuthentication::ApiKey {
                credential_ref: reference(key),
            },
            None => ModelAuthentication::None {},
        },
    }
}

/// The model access of one project whose pool holds only `member`.
/// Input: the member's id. Output: the access.
fn pool_of(member: &str) -> ModelAccess {
    ModelAccess {
        accounts: vec![reference("owner-account")],
        connections: vec![
            endpoint(
                "openai-keyed",
                "https://api.openai.com/v1",
                "openai-chat",
                Some("openai-key"),
            ),
            endpoint(
                "anthropic-keyed",
                "https://api.anthropic.com/v1",
                "anthropic-messages",
                Some("anthropic-key"),
            ),
            endpoint("local", "http://127.0.0.1:11434/v1", "openai-chat", None),
        ],
        pools: vec![AccountPool {
            id: reference("research-pool"),
            name: None,
            members: vec![reference(member)],
            projects: vec![reference("research-project")],
            roles: vec![reference("project_leader")],
            fallback_allowed: false,
            provider_quota_policy: Default::default(),
        }],
    }
}

fn picked(member: &str, harness: &str) -> AccountPick {
    pick_account(&pool_of(member), "research-project", harness, |_| true)
}

fn account(member: &str, account: ResearchAccount) -> AccountPick {
    AccountPick::Account {
        connection: reference(member),
        account: Box::new(account),
    }
}

#[test]
fn research_runs_on_endpoint_accounts_with_or_without_a_key_by_wire_format() {
    assert_eq!(
        picked("openai-keyed", "codex"),
        account(
            "openai-keyed",
            ResearchAccount::Endpoint {
                base_url: "https://api.openai.com/v1".into(),
                model: "some-model".into(),
                key: Some(reference("openai-key")),
            }
        )
    );
    assert_eq!(
        picked("anthropic-keyed", "claude-code"),
        account(
            "anthropic-keyed",
            ResearchAccount::AnthropicEndpoint {
                base_url: "https://api.anthropic.com".into(),
                model: "some-model".into(),
                key: Some(reference("anthropic-key")),
            }
        ),
        "Claude Code adds /v1/messages itself"
    );
    assert_eq!(
        picked("local", "codex"),
        account(
            "local",
            ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "some-model".into(),
                key: None,
            }
        )
    );
    assert_eq!(
        picked("anthropic-keyed", "codex"),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        picked("openai-keyed", "claude-code"),
        AccountPick::NoUsableAccount
    );
    assert_eq!(picked("local", "claude-code"), AccountPick::NoUsableAccount);
}

/// A check of one account (DEC-150 addendum 9h) runs only on that account, though another
/// member of its pool comes first in a pool pick, and finds no usable account for one no
/// pool names, one not in the settings or one the harness cannot use.
#[cfg(unix)]
#[test]
fn a_connection_pick_takes_only_the_named_member_of_a_pool() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::Builder::new()
        .prefix("ca-connection-pick-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    crate::storage::initialize_authority_schemas(&Connection::open(&database).unwrap()).unwrap();
    let projects = base.join("projects");
    std::fs::create_dir(&projects).unwrap();
    let global = base.join("global.toml");
    let connection = |id: &str, port: u16| {
        format!(
            r#"{{ id = "{id}", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = {{ kind = "http", url = "http://127.0.0.1:{port}/v1" }}, model = {{ id = "qwen2.5:0.5b", author = "openai-compatible" }}, auth = {{ method = "none" }} }}"#
        )
    };
    std::fs::write(
        &global,
        format!(
            "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\nconnections = [{}, {}, {}]\npools = [{{ id = \"local-pool\", members = [\"first\", \"second\"], projects = [], roles = [], fallback_allowed = false }}]\n",
            connection("first", 11434),
            connection("second", 11435),
            connection("loose", 11436),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let resolver =
        SettingsResolver::new(&global, &base.join("project.toml"), &projects, &database).unwrap();
    let endpoint = |id: &str, port: u16| AccountPick::Account {
        connection: reference(id),
        account: Box::new(ResearchAccount::Endpoint {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "qwen2.5:0.5b".into(),
            key: None,
        }),
    };
    let codex = [ResearchHarness::Codex];
    assert_eq!(
        resolver.pool_pick("", &reference("local-pool"), &codex),
        endpoint("first", 11434)
    );
    assert_eq!(
        resolver.connection_pick("", &reference("second"), &codex),
        endpoint("second", 11435)
    );
    assert_eq!(
        resolver.connection_pick("", &reference("loose"), &codex),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        resolver.connection_pick("", &reference("absent"), &codex),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        resolver.connection_pick("", &reference("second"), &[ResearchHarness::ClaudeCode]),
        AccountPick::NoUsableAccount
    );
}

/// A pick whose hold read fails counts the account as held for that pick
/// (V3-HOLD-FAILOPEN, DEC-150 addendum 9j): the pool pick, the one-account pick and the
/// project resolve each answer paused for `HOLD_UNREAD_RETRY_MS`, never an account a hold
/// they could not read might cover.
#[cfg(unix)]
#[test]
fn a_hold_read_that_fails_counts_the_account_as_held_for_that_pick() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::Builder::new()
        .prefix("ca-hold-unread-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    let db = Connection::open(&database).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    let projects = base.join("projects");
    std::fs::create_dir_all(projects.join("team-panes")).unwrap();
    let global = base.join("global.toml");
    std::fs::write(
        &global,
        "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\nconnections = [{ id = \"first\", provider = \"openai-compatible\", harness = \"rcli\", protocol = \"openai-chat\", account_ref = \"local-account\", endpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }, model = { id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }, auth = { method = \"none\" } }]\npools = [{ id = \"local-pool\", members = [\"first\"], projects = [\"team-panes\"], roles = [], fallback_allowed = false }]\n",
    )
    .unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let resolver =
        SettingsResolver::new(&global, &base.join("project.toml"), &projects, &database).unwrap();
    let codex = [ResearchHarness::Codex];
    assert!(matches!(
        resolver.pool_pick("team-panes", &reference("local-pool"), &codex),
        AccountPick::Account { .. }
    ));

    db.execute_batch("DROP TABLE team_project_pauses").unwrap();
    let before = crate::team::now_ms();
    let picks = [
        resolver.pool_pick("team-panes", &reference("local-pool"), &codex),
        resolver.connection_pick("team-panes", &reference("first"), &codex),
        resolver.resolve("team-panes", "codex").unwrap().1,
    ];
    let after = crate::team::now_ms();
    let held = before + HOLD_UNREAD_RETRY_MS..=after + HOLD_UNREAD_RETRY_MS;
    for pick in picks {
        assert!(
            matches!(pick, AccountPick::Paused { until_ms } if held.contains(&until_ms)),
            "{pick:?}"
        );
    }
}

/// A lasting unread hold logs one `account_hold_unread` line per cause per
/// `HOLD_UNREAD_RETRY_MS`, not one per pick on every research tick (HFO-L1): repeated
/// failing reads within the window log once, the first read past it logs again, each cause
/// keeps its own window, a clock that went back logs again, and racing picks log once.
#[test]
fn repeated_failing_hold_reads_within_the_retry_window_log_once() {
    let read_failed = AtomicI64::new(i64::MIN);
    let no_database = AtomicI64::new(i64::MIN);
    let start = 1_789_000_000_000;
    assert!(unread_log_due(&read_failed, start));
    for later in [
        start,
        start + 1,
        start + 1_000,
        start + HOLD_UNREAD_RETRY_MS - 1,
    ] {
        assert!(!unread_log_due(&read_failed, later), "{later}");
    }
    assert!(unread_log_due(&no_database, start + 1_000));
    assert!(unread_log_due(&read_failed, start + HOLD_UNREAD_RETRY_MS));
    assert!(!unread_log_due(
        &read_failed,
        start + HOLD_UNREAD_RETRY_MS + 1
    ));
    assert!(!unread_log_due(&no_database, start + HOLD_UNREAD_RETRY_MS));
    assert!(unread_log_due(&read_failed, start));
    assert!(!unread_log_due(&read_failed, start + 1));

    let raced = AtomicI64::new(i64::MIN);
    let logged = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                if unread_log_due(&raced, start) {
                    logged.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(logged.into_inner(), 1);
}

/// HFO-L1 r1: a racing thread's slightly older `now` is not a clock that went back, so it
/// neither logs a second `account_hold_unread` line nor rewinds the stamp; only a full
/// `HOLD_UNREAD_RETRY_MS` gap either way logs, and a real clock-back then stamps its time so
/// the next read a second later stays quiet.
#[test]
fn a_racing_pick_with_a_slightly_older_now_neither_logs_nor_rewinds_the_stamp() {
    let start = 1_789_000_000_000;
    let logged = AtomicI64::new(start + 1);
    assert!(!unread_log_due(&logged, start));
    assert_eq!(logged.load(std::sync::atomic::Ordering::Relaxed), start + 1);
    assert!(!unread_log_due(&logged, start + HOLD_UNREAD_RETRY_MS));
    assert!(unread_log_due(&logged, start + 1 + HOLD_UNREAD_RETRY_MS));
    let back = start - 10 * HOLD_UNREAD_RETRY_MS;
    assert!(unread_log_due(&logged, back));
    assert!(!unread_log_due(&logged, back + 1_000));
}
