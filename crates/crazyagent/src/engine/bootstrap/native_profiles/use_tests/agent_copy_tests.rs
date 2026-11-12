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

fn id_claims(expires: u64) -> String {
    let full = token(expires);
    let (head, _) = full.rsplit_once('.').unwrap();
    format!("{head}.unexchangeable")
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
                "id_token": id_claims(expires),
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
fn a_codex_copy_replaces_the_id_token_signature_but_keeps_its_claims_and_the_access_token() {
    let mut case = UseCase::new();
    let expires = now() + 3 * 86_400;
    let part =
        |value: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string());
    let header = part(json!({"alg": "RS256", "typ": "JWT"}));
    let payload = part(json!({
        "exp": expires,
        "https://api.openai.com/auth": {"chatgpt_account_id": "acct-unit"}
    }));
    let id_token = format!("{header}.{payload}.ZXhjaGFuZ2VhYmxlc2lnbmF0dXJl");
    let access = token(expires);
    let source = case.home().join("auth.json");
    private_file(
        &source,
        &json!({
            "OPENAI_API_KEY": "sk-unit-api-key",
            "tokens": {
                "id_token": id_token,
                "access_token": access,
                "refresh_token": "unit-refresh-secret",
                "account_id": "acct-unit"
            },
            "last_refresh": "2026-10-26T00:00:00Z"
        })
        .to_string()
        .into_bytes(),
    );
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    let written: Value =
        serde_json::from_slice(&std::fs::read(agent.join("auth.json")).unwrap()).unwrap();
    let copied = written["tokens"]["id_token"].as_str().unwrap();
    let (claims_prefix, signature) = copied.rsplit_once('.').unwrap();
    assert_eq!(claims_prefix, format!("{header}.{payload}"));
    assert_ne!(copied, id_token);
    assert!(!signature.is_empty());
    assert_ne!(signature, "ZXhjaGFuZ2VhYmxlc2lnbmF0dXJl");
    let claims_segment = copied.split('.').nth(1).unwrap();
    let claims: Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(claims_segment)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        claims["https://api.openai.com/auth"]["chatgpt_account_id"],
        "acct-unit"
    );
    assert_eq!(claims["exp"], expires);
    assert_eq!(written["tokens"]["access_token"], access);
    drop(copy);
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

/// CA-SEC-003: a running keeper leases the profile itself, so once the turn's own lease ends
/// a sign-out or sign-in still waits for every renewal that could write the account's
/// record, and stopping the keeper ends that lease.
#[test]
fn a_running_keeper_keeps_the_profile_leased_after_the_turn_s_lease_ended() {
    let mut case = UseCase::new();
    private_file(&case.home().join("auth.json"), &codex_auth(now() + 600));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    drop(guard);
    assert_busy(&case.manifest());
    drop(copy);
    let file = File::open(case.manifest()).unwrap();
    let waited = Instant::now();
    while flock(&file, FlockOperation::NonBlockingLockExclusive).is_err() {
        assert!(
            waited.elapsed() < Duration::from_secs(5),
            "the profile stayed leased after its keeper stopped"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn dropping_a_copy_mid_renewal_returns_at_once_and_the_renewal_still_keeps_the_rotated_token() {
    let mut case = UseCase::new();
    let harness = scripted_codex(
        &case,
        "codex-held",
        "#!/bin/sh\nif [ ! -f \"$0.renewed\" ]; then\n  : > \"$0.renewed\"\n  cp \"$0.next\" \"$CODEX_HOME/auth.json\"\n  exit 0\nfi\necho $$ > \"$0.pid.part\"\nmv \"$0.pid.part\" \"$0.pid\"\ni=0\nwhile [ ! -f \"$0.go\" ] && [ $i -lt 400 ]; do /bin/sleep 0.05; i=$((i+1)); done\ncp \"$0.rotated\" \"$CODEX_HOME/auth.json\"\n",
        &codex_auth(now() + 200),
    );
    let rotated = String::from_utf8(codex_auth(now() + 3 * 86_400))
        .unwrap()
        .replace("unit-refresh-secret", "unit-rotated-refresh")
        .into_bytes();
    private_file(&case.root.path().join("codex-held.rotated"), &rotated);
    private_file(&case.home().join("auth.json"), &codex_auth(now() + 30));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard.hand_to_agent(&agent, TURN, &harness).unwrap();
    let marker = case.root.path().join("codex-held.pid");
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
    assert!(
        rustix::process::test_kill_process(child).is_ok(),
        "the turn's end killed the renewal mid-refresh"
    );
    assert_eq!(
        guard.hold_renewal(Duration::ZERO).err().unwrap().0,
        "native_profile_renewal_busy",
        "the renewal keeps the account's renewal lock until it ends"
    );
    assert!(entries(&agent).is_empty());

    private_file(&case.root.path().join("codex-held.go"), b"");
    let ending = Instant::now();
    while rustix::process::test_kill_process(child).is_ok() {
        assert!(
            ending.elapsed() < Duration::from_secs(10),
            "the renewal never finished"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let released = Instant::now();
    let lock = loop {
        match guard.hold_renewal(Duration::ZERO) {
            Ok(lock) => break lock,
            Err(_) => {
                assert!(
                    released.elapsed() < Duration::from_secs(10),
                    "the renewal never released the account's renewal lock"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    drop(lock);
    assert_eq!(
        std::fs::read(case.home().join("auth.json")).unwrap(),
        rotated,
        "the account keeps the rotated refresh token"
    );
    let watched = Instant::now();
    while watched.elapsed() < Duration::from_millis(300) {
        assert!(
            entries(&agent).is_empty(),
            "a copy was written after the turn"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
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
    let unreadable = "native_profile_codex_token_unreadable";
    let api_key = "native_profile_codex_api_key";
    let bytes = |record: Value| record.to_string().into_bytes();
    for (record, code) in [
        (b"not json".to_vec(), unreadable),
        (bytes(json!({"OPENAI_API_KEY": "sk-unit-api-key"})), api_key),
        (
            bytes(json!({"OPENAI_API_KEY": "sk-unit-api-key", "tokens": null})),
            api_key,
        ),
        (
            bytes(json!({"OPENAI_API_KEY": "", "tokens": null})),
            unreadable,
        ),
        (bytes(json!({"OPENAI_API_KEY": null})), unreadable),
        (
            bytes(
                json!({"tokens": {"id_token": "a", "access_token": "no-claims", "refresh_token": "r"}}),
            ),
            unreadable,
        ),
        (
            bytes(json!({"tokens": {"access_token": token(now() + 3600), "refresh_token": "r"}})),
            unreadable,
        ),
        (
            bytes(
                json!({"tokens": {"id_token": token(now() + 3600), "access_token": "", "refresh_token": "r"}}),
            ),
            unreadable,
        ),
    ] {
        private_file(&source, &record);
        let guard = execution(&mut case);
        assert_eq!(
            refused(guard.hand_to_agent(&agent, TURN, &no_harness(&case))),
            code
        );
        assert_eq!(usage_refused(&guard, &no_harness(&case)), code);
        assert!(entries(&agent).is_empty());
    }
}

/// The refusal of a usage read of this account. Inputs: the held account and the harness.
/// Output: the refusal code; panics when a credential was read.
fn usage_refused(guard: &NativeProfileUse, executable: &Path) -> String {
    match guard.usage_credential(executable) {
        Ok(_) => panic!("the usage read got a credential"),
        Err(problem) => problem.0,
    }
}

#[test]
fn a_codex_usage_read_whose_lapsed_token_the_renewal_cannot_refresh_says_the_sign_in_expired() {
    let mut case = UseCase::new();
    let harness = case.root.path().join("offline-codex");
    std::fs::write(&harness, "#!/bin/sh\n: > \"$0.ran\"\nexit 1\n").unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = case.home().join("auth.json");
    private_file(&source, &codex_auth(now() + 3600));
    let guard = execution(&mut case);
    let read = guard.usage_credential(&harness).unwrap();
    assert_eq!(read.account_id.as_deref(), Some("acct-unit"));
    assert!(!case.root.path().join("offline-codex.ran").exists());
    drop((read, guard));
    let lapsing = codex_auth(now() + 30);
    private_file(&source, &lapsing);
    let guard = execution(&mut case);
    assert_eq!(
        usage_refused(&guard, &harness),
        "native_profile_codex_sign_in_expired"
    );
    assert!(
        case.root.path().join("offline-codex.ran").is_file(),
        "the renewal never ran before the refusal"
    );
    assert_eq!(std::fs::read(&source).unwrap(), lapsing);
}

/// An access token shaped like the ones ChatGPT issues Codex: an RS256 JWT whose claims
/// name its audience, client, account, plan, profile, scopes and session and whose
/// signature is 256 bytes, so it runs past 1 KiB. Input: its expiry. Output: the token.
fn chatgpt_token(expires: u64) -> String {
    let part = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let issued = expires - 864_000;
    let header = json!({
        "alg": "RS256",
        "kid": "b1dd3f8f-9926-4f9b-b5a3-2f3c3f0e7a11",
        "typ": "JWT"
    });
    let claims = json!({
        "aud": ["https://api.openai.com/v1"],
        "client_id": "app_unit00000000000000000000",
        "exp": expires,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct-unit",
            "chatgpt_plan_type": "pro",
            "chatgpt_user_id": "user-unit00000000000000000000",
            "user_id": "user-unit00000000000000000000"
        },
        "https://api.openai.com/profile": {"email": "unit@example.com", "email_verified": true},
        "iat": issued,
        "iss": "https://auth.openai.com",
        "jti": "4f0e8c2a-7d6b-4c1e-9a3f-5b2d8e1c0a97",
        "nbf": issued,
        "pwd_auth_time": issued * 1000,
        "scp": ["openid", "profile", "email", "offline_access"],
        "session_id": "authsess_unit000000000000000000000",
        "sub": "auth0|unit000000000000000000000000"
    });
    let signature: Vec<u8> = (0..=255).collect();
    format!(
        "{}.{}.{}",
        part(header.to_string().as_bytes()),
        part(claims.to_string().as_bytes()),
        part(&signature)
    )
}

/// R4 N-D16: a usage read of a Codex account gets its access token, although a ChatGPT
/// access token is longer than the 1 KiB a stored secret may hold.
#[test]
fn a_codex_usage_read_gets_a_full_size_chatgpt_access_token() {
    let mut case = UseCase::new();
    let access = chatgpt_token(now() + 3600);
    assert!(
        access.len() > crate::credentials::SECRET_BYTES_MAX,
        "a ChatGPT access token outgrows a stored secret: {} bytes",
        access.len()
    );
    let mut record: Value = serde_json::from_slice(&codex_auth(now() + 3600)).unwrap();
    record["tokens"]["access_token"] = json!(access);
    private_file(
        &case.home().join("auth.json"),
        record.to_string().as_bytes(),
    );
    let guard = execution(&mut case);
    let read = guard.usage_credential(&no_harness(&case));
    assert_eq!(
        read.as_ref().err().map(|problem| problem.0.as_str()),
        None,
        "the usage read refused the account's access token"
    );
    let read = read.unwrap();
    assert_eq!(read.token.expose(), access);
    assert_eq!(read.account_id.as_deref(), Some("acct-unit"));
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
#[test]
fn an_agent_gets_no_claude_copy_when_the_sign_in_can_create_api_keys() {
    let mut case = UseCase::with_harness("claude-code");
    let expires = (now() + 8 * 3600) * 1000;
    let home = case.home();
    let mut record = claude_record(expires);
    record["claudeAiOauth"]["scopes"] =
        json!(["user:inference", "user:profile", "org:create_api_key"]);
    let (_keychain, _service) = claude_keychain(&home, &record);
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN, &no_harness(&case))),
        "native_profile_scope_too_wide"
    );
    assert!(entries(&agent).is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn an_agent_gets_a_claude_copy_for_a_real_subscription_sign_in() {
    let subscription = json!([
        "user:inference",
        "user:profile",
        "user:sessions:claude_code",
        "user:mcp_servers",
        "user:file_upload"
    ]);
    let mut case = UseCase::with_harness("claude-code");
    let expires = (now() + 8 * 3600) * 1000;
    let home = case.home();
    let mut record = claude_record(expires);
    record["claudeAiOauth"]["scopes"] = subscription.clone();
    let (_keychain, _service) = claude_keychain(&home, &record);
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    let written: Value =
        serde_json::from_slice(&std::fs::read(agent.join(".credentials.json")).unwrap()).unwrap();
    assert_eq!(
        written["claudeAiOauth"]["scopes"], subscription,
        "the subscription's user scopes are kept, not narrowed away"
    );
    assert_eq!(written["claudeAiOauth"]["accessToken"], "sk-ant-oat01-unit");
    assert_eq!(written["claudeAiOauth"]["refreshToken"], Value::Null);
    drop(copy);
}

#[cfg(target_os = "macos")]
#[test]
fn an_agent_gets_no_claude_copy_for_any_org_scope() {
    for scopes in [
        json!(["org:create_api_key", "user:profile"]),
        json!(["user:inference", "user:profile", "org:workspaces:create"]),
    ] {
        let mut case = UseCase::with_harness("claude-code");
        let expires = (now() + 8 * 3600) * 1000;
        let home = case.home();
        let mut record = claude_record(expires);
        record["claudeAiOauth"]["scopes"] = scopes.clone();
        let (_keychain, _service) = claude_keychain(&home, &record);
        let agent = agent_home(&case);
        let guard = execution(&mut case);
        assert_eq!(
            refused(guard.hand_to_agent(&agent, TURN, &no_harness(&case))),
            "native_profile_scope_too_wide",
            "{scopes}"
        );
        assert!(entries(&agent).is_empty(), "{scopes}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn an_agent_gets_no_claude_copy_for_a_malformed_or_inference_less_scope_set() {
    for scopes in [
        json!([]),
        json!(["user:profile"]),
        json!(["user:inference", 123]),
        json!([" org:create_api_key ", "user:inference"]),
        json!(["ORG:create_api_key", "user:inference"]),
        json!(["Org:create_api_key", "user:inference"]),
        json!(["oRg:create_api_key", "user:inference"]),
    ] {
        let mut case = UseCase::with_harness("claude-code");
        let expires = (now() + 8 * 3600) * 1000;
        let home = case.home();
        let mut record = claude_record(expires);
        record["claudeAiOauth"]["scopes"] = scopes.clone();
        let (_keychain, _service) = claude_keychain(&home, &record);
        let agent = agent_home(&case);
        let guard = execution(&mut case);
        assert_eq!(
            refused(guard.hand_to_agent(&agent, TURN, &no_harness(&case))),
            "native_profile_scope_too_wide",
            "{scopes}"
        );
        assert!(entries(&agent).is_empty(), "{scopes}");
    }
}

/// Waits up to five seconds for a profile's lease to end. Input: the manifest path.
/// Output: whether it ended.
fn lease_ended(manifest: &Path) -> bool {
    let file = File::open(manifest).unwrap();
    let waited = Instant::now();
    while flock(&file, FlockOperation::NonBlockingLockExclusive).is_err() {
        if waited.elapsed() >= Duration::from_secs(5) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// OB-5 r2: a turn with no time bound always gets a keeper, whatever time its token has
/// left, so a turn that outlives the token keeps its sign-in. With a token good for thirty
/// days the keeper leases the profile until the copy is dropped.
#[test]
fn a_turn_with_no_time_bound_gets_a_keeper_however_long_its_token_lasts() {
    let mut case = UseCase::new();
    let expires = now() + 30 * 86_400;
    private_file(&case.home().join("auth.json"), &codex_auth(expires));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard
        .hand_to_turn(&agent, None, &no_harness(&case))
        .unwrap();
    drop(guard);
    assert_busy(&case.manifest());
    drop(copy);
    assert!(lease_ended(&case.manifest()), "the keeper kept its lease");
}

/// A bounded turn its token outlasts still gets no keeper: once the turn's own lease ends
/// nothing holds the profile, though the copy is still there.
#[test]
fn a_bounded_turn_its_token_outlasts_gets_no_keeper() {
    let mut case = UseCase::new();
    let expires = now() + 30 * 86_400;
    private_file(&case.home().join("auth.json"), &codex_auth(expires));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard
        .hand_to_turn(&agent, Some(TURN), &no_harness(&case))
        .unwrap();
    drop(guard);
    assert!(lease_ended(&case.manifest()), "a keeper leased the profile");
    drop(copy);
}

#[test]
fn dropping_an_earlier_copy_leaves_a_later_copy_or_another_file_at_the_same_name() {
    let mut case = UseCase::new();
    let expires = now() + 3 * 86_400;
    private_file(&case.home().join("auth.json"), &codex_auth(expires));
    let agent = agent_home(&case);
    let path = agent.join("auth.json");
    let guard = execution(&mut case);
    let earlier = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    let first = std::fs::symlink_metadata(&path).unwrap().ino();
    let later = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    let second = std::fs::symlink_metadata(&path).unwrap().ino();
    assert_ne!(first, second, "each placement is a new file");
    drop(earlier);
    assert_eq!(
        entries(&agent),
        ["auth.json"],
        "the later start's copy stays"
    );
    assert_eq!(std::fs::symlink_metadata(&path).unwrap().ino(), second);
    drop(later);
    assert!(
        entries(&agent).is_empty(),
        "the later copy leaves with its own drop"
    );

    let copy = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    let other = agent.join("other.json");
    private_file(&other, b"another file");
    std::fs::rename(&other, &path).unwrap();
    drop(copy);
    assert_eq!(std::fs::read(&path).unwrap(), b"another file");
    std::fs::remove_file(&path).unwrap();

    let copy = guard
        .hand_to_agent(&agent, TURN, &no_harness(&case))
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    drop(copy);
    assert!(entries(&agent).is_empty(), "a copy already gone is fine");
    drop(guard);
}

#[cfg(target_os = "macos")]
mod renewal_tests;
