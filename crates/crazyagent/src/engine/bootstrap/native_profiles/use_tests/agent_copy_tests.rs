use super::*;
#[cfg(target_os = "macos")]
use crate::credentials::test_keychain::TestKeychain;
use base64::Engine;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const TURN: Duration = Duration::from_secs(3600);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn token(expires: u64) -> String {
    let part =
        |value: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string());
    format!(
        "{}.{}.c2ln",
        part(json!({"alg": "RS256", "typ": "JWT"})),
        part(json!({
            "exp": expires,
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-unit"}
        }))
    )
}

fn codex_auth(expires: u64) -> Vec<u8> {
    json!({
        "OPENAI_API_KEY": "sk-unit-api-key",
        "tokens": {
            "id_token": token(expires),
            "access_token": token(expires),
            "refresh_token": "unit-refresh-secret",
            "account_id": "acct-unit"
        },
        "last_refresh": "2026-10-26T00:00:00Z",
        "unit_extra": "unit-extra-secret"
    })
    .to_string()
    .into_bytes()
}

fn agent_home(case: &UseCase) -> PathBuf {
    let home = case.root.path().join("agent-home");
    private_directory(&home);
    home
}

fn execution(case: &mut UseCase) -> NativeProfileUse {
    case.custody
        .as_mut()
        .unwrap()
        .acquire_use(&case.host, &case.binding, &case.operation_id, 1, purpose(3))
        .unwrap()
}

fn no_harness(case: &UseCase) -> PathBuf {
    case.root.path().join("no-harness")
}

fn entries(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn refused(result: Result<agent_copy::AgentCredential, EngineError>) -> String {
    match result {
        Ok(_) => panic!("the copy was made"),
        Err(problem) => problem.0,
    }
}

#[test]
fn an_agent_gets_a_codex_copy_that_can_never_refresh_the_account() {
    let mut case = UseCase::new();
    let expires = now() + 3 * 86_400;
    let source = case.home().join("auth.json");
    private_file(&source, &codex_auth(expires));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    let path = agent.join("auth.json");
    let metadata = std::fs::symlink_metadata(&path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        written,
        json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": token(expires),
                "access_token": token(expires),
                "refresh_token": "",
                "account_id": "acct-unit"
            },
            "last_refresh": "2026-10-26T00:00:00Z"
        })
    );
    assert_eq!(entries(&agent), ["auth.json"]);
    assert_eq!(std::fs::read(&source).unwrap(), codex_auth(expires));
    drop(copy);
    assert!(entries(&agent).is_empty());
    assert_eq!(std::fs::read(&source).unwrap(), codex_auth(expires));
}

#[test]
fn a_record_lapsing_within_a_minute_that_cannot_be_renewed_is_refused_and_nothing_is_written() {
    let mut case = UseCase::new();
    let agent = agent_home(&case);
    for expires in [now() + 30, now() - 60] {
        private_file(&case.home().join("auth.json"), &codex_auth(expires));
        let guard = execution(&mut case);
        assert_eq!(
            refused(guard.hand_to_agent(&agent, TURN, &no_harness(&case))),
            "native_profile_renewal_failed"
        );
        assert!(entries(&agent).is_empty());
    }
}

#[test]
fn a_failed_renewal_is_a_transient_refusal_even_for_a_turn_shorter_than_a_minute() {
    let mut case = UseCase::new();
    let agent = agent_home(&case);
    let harness = case.root.path().join("offline-codex");
    std::fs::write(&harness, "#!/bin/sh\n: > \"$0.ran\"\nexit 1\n").unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&case.home().join("auth.json"), &codex_auth(now() + 30));
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, Duration::from_secs(10), &harness)),
        "native_profile_renewal_failed"
    );
    assert!(
        case.root.path().join("offline-codex.ran").is_file(),
        "the renewal never ran before the refusal"
    );
    assert!(entries(&agent).is_empty());
}

fn scripted_codex(case: &UseCase, name: &str, script: &str, next: &[u8]) -> PathBuf {
    let harness = case.root.path().join(name);
    private_file(&case.root.path().join(format!("{name}.next")), next);
    std::fs::write(&harness, script).unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o700)).unwrap();
    harness
}

#[test]
fn dropping_a_copy_stops_its_keeper_mid_renewal_at_once_and_ends_the_renewal_child() {
    let mut case = UseCase::new();
    let harness = scripted_codex(
        &case,
        "codex-renews-once",
        "#!/bin/sh\nif [ ! -f \"$0.renewed\" ]; then\n  : > \"$0.renewed\"\n  cp \"$0.next\" \"$CODEX_HOME/auth.json\"\n  exit 0\nfi\necho $$ > \"$0.pid.part\"\nmv \"$0.pid.part\" \"$0.pid\"\nexec /bin/sleep 120\n",
        &codex_auth(now() + 200),
    );
    private_file(&case.home().join("auth.json"), &codex_auth(now() + 30));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard.hand_to_agent(&agent, TURN, &harness).unwrap();
    let marker = case.root.path().join("codex-renews-once.pid");
    let waited = Instant::now();
    while !marker.is_file() {
        assert!(
            waited.elapsed() < Duration::from_secs(20),
            "the keeper never started its renewal"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid: i32 = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let child = rustix::process::Pid::from_raw(pid).unwrap();
    let dropping = Instant::now();
    drop(copy);
    let took = dropping.elapsed();
    assert!(took < Duration::from_secs(5), "{took:?}");
    let ending = Instant::now();
    while rustix::process::test_kill_process(child).is_ok() {
        assert!(
            ending.elapsed() < Duration::from_secs(2),
            "the renewal child outlived its keeper"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(entries(&agent).is_empty());
    drop(guard);
}

#[test]
fn a_renewal_in_the_handoff_leaves_the_async_runtime_free() {
    let mut case = UseCase::new();
    let harness = scripted_codex(
        &case,
        "codex-slow",
        "#!/bin/sh\n/bin/sleep 2\ncp \"$0.next\" \"$CODEX_HOME/auth.json\"\n",
        &codex_auth(now() + 3 * 86_400),
    );
    private_file(&case.home().join("auth.json"), &codex_auth(now() + 30));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_time()
        .build()
        .unwrap();
    let (widest, handed) = runtime.block_on(async move {
        let ticker = tokio::spawn(async {
            let mut last = Instant::now();
            let mut widest = Duration::ZERO;
            for _ in 0..60 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                widest = widest.max(last.elapsed());
                last = Instant::now();
            }
            widest
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let handed = tokio::spawn(async move {
            let handed = guard.hand_to_agent(&agent, TURN, &harness).map(drop);
            drop(guard);
            handed
        })
        .await
        .unwrap();
        (ticker.await.unwrap(), handed)
    });
    assert!(handed.is_ok(), "{:?}", handed.err());
    assert!(widest < Duration::from_secs(1), "{widest:?}");
}

#[test]
fn a_missing_or_unusable_codex_record_is_refused() {
    let mut case = UseCase::new();
    let agent = agent_home(&case);
    let source = case.home().join("auth.json");
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN, &no_harness(&case))),
        "native_profile_credential_unavailable"
    );
    drop(guard);
    for record in [
        b"not json".to_vec(),
        json!({"OPENAI_API_KEY": "sk-unit-api-key"})
            .to_string()
            .into_bytes(),
        json!({"tokens": {"id_token": "a", "access_token": "no-claims", "refresh_token": "r"}})
            .to_string()
            .into_bytes(),
    ] {
        private_file(&source, &record);
        let guard = execution(&mut case);
        assert_eq!(
            refused(guard.hand_to_agent(&agent, TURN, &no_harness(&case))),
            "native_profile_credential_unavailable"
        );
        assert!(entries(&agent).is_empty());
    }
}

#[test]
fn links_at_either_end_are_refused_or_replaced_and_never_followed() {
    let mut case = UseCase::new();
    let expires = now() + 3 * 86_400;
    let source = case.home().join("auth.json");
    let agent = agent_home(&case);
    let outside = case.root.path().join("outside.json");
    private_file(&outside, b"untouched");
    symlink(&outside, agent.join("auth.json")).unwrap();
    private_file(&source, &codex_auth(expires));
    let guard = execution(&mut case);
    let copy = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    assert!(
        std::fs::symlink_metadata(agent.join("auth.json"))
            .unwrap()
            .is_file()
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
    drop(copy);
    drop(guard);
    assert!(entries(&agent).is_empty());
    assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");

    let elsewhere = case.root.path().join("elsewhere.json");
    private_file(&elsewhere, &codex_auth(expires));
    std::fs::remove_file(&source).unwrap();
    symlink(&elsewhere, &source).unwrap();
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN, &no_harness(&case))),
        "native_profile_credential_unavailable"
    );
    drop(guard);
    std::fs::remove_file(&source).unwrap();
    std::fs::hard_link(&elsewhere, &source).unwrap();
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN, &no_harness(&case))),
        "native_profile_credential_unavailable"
    );
    assert!(entries(&agent).is_empty());
    drop(guard);

    std::fs::remove_file(&source).unwrap();
    private_file(&source, &codex_auth(expires));
    let linked = case.root.path().join("linked-home");
    symlink(&agent, &linked).unwrap();
    let open = case.root.path().join("open-home");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    let guard = execution(&mut case);
    for home in [&linked, &open] {
        assert_eq!(
            refused(guard.hand_to_agent(home, TURN, &no_harness(&case))),
            "native_profile_agent_home_unsafe"
        );
    }
    assert!(entries(&agent).is_empty());
    assert!(entries(&open).is_empty());
}

#[cfg(target_os = "macos")]
fn claude_record(expires_ms: u64) -> Value {
    json!({
        "claudeAiOauth": {
            "accessToken": "sk-ant-oat01-unit",
            "refreshToken": "sk-ant-ort01-unit-secret",
            "expiresAt": expires_ms,
            "scopes": ["user:inference", "user:profile"],
            "subscriptionType": "max"
        },
        "mcpOAuth": {"unit-server": {"accessToken": "mcp-unit-secret"}}
    })
}

/// The keychain account Claude Code 2.1.309 files a sign-in under when it runs, as every
/// managed Claude child does, with no USER in its environment.
#[cfg(target_os = "macos")]
const CLAUDE_ACCOUNT: &str = "unknown";

#[cfg(target_os = "macos")]
fn claude_keychain(home: &Path, record: &Value) -> (TestKeychain, String) {
    let keychain = TestKeychain::create(home);
    let service = format!(
        "Claude Code-credentials-{}",
        &digest(home.to_str().unwrap().as_bytes())[..8]
    );
    keychain.add(CLAUDE_ACCOUNT, &service, &record.to_string());
    (keychain, service)
}

#[cfg(target_os = "macos")]
#[test]
fn an_agent_gets_a_claude_copy_from_the_accounts_keychain_without_its_refresh_token() {
    let mut case = UseCase::with_harness("claude-code");
    let home = case.home();
    let expires = (now() + 8 * 3600) * 1000;
    let (keychain, service) = claude_keychain(&home, &claude_record(expires));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    let path = agent.join(".credentials.json");
    let metadata = std::fs::symlink_metadata(&path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        written,
        json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-unit",
                "refreshToken": null,
                "expiresAt": expires,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "max"
            }
        })
    );
    assert_eq!(
        serde_json::from_str::<Value>(&keychain.find(&service)).unwrap(),
        claude_record(expires)
    );
    drop(copy);
    assert!(entries(&agent).is_empty());
    drop(guard);

    let soon = (now() + 30) * 1000;
    let mut late = UseCase::with_harness("claude-code");
    let (_late_keychain, _) = claude_keychain(&late.home(), &claude_record(soon));
    let agent = agent_home(&late);
    let guard = execution(&mut late);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN, &no_harness(&late))),
        "native_profile_renewal_failed"
    );
    assert!(entries(&agent).is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn a_claude_record_larger_than_a_pipe_buffer_is_read_whole_from_the_keychain() {
    let mut case = UseCase::with_harness("claude-code");
    let expires = (now() + 8 * 3600) * 1000;
    let home = case.home();
    let (keychain, service) = claude_keychain(&home, &claude_record(expires));
    let mut record = claude_record(expires);
    record["mcpOAuth"]["unit-server"]["accessToken"] = json!("m".repeat(40_000));
    let hex: String = record
        .to_string()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = keychain.path().to_str().unwrap().to_owned();
    keychain.security(
        &[
            "add-generic-password",
            "-U",
            "-a",
            CLAUDE_ACCOUNT,
            "-s",
            &service,
            "-X",
            &hex,
            &path,
        ],
        None,
    );
    let stored = keychain.find(&service);
    assert!(stored.len() > 40_000);
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let started = std::time::Instant::now();
    let copy = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let written: Value =
        serde_json::from_slice(&std::fs::read(agent.join(".credentials.json")).unwrap()).unwrap();
    assert_eq!(written["claudeAiOauth"]["accessToken"], "sk-ant-oat01-unit");
    assert_eq!(written["claudeAiOauth"]["refreshToken"], Value::Null);
    assert!(written.get("mcpOAuth").is_none());
    drop(copy);
}

#[cfg(target_os = "macos")]
mod renewal_tests;
