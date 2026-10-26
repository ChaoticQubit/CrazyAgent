use super::*;
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
    let copy = guard.hand_to_agent(&agent, TURN).unwrap();
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
fn a_copy_that_would_lapse_during_the_turn_is_refused_and_nothing_is_written() {
    let mut case = UseCase::new();
    let agent = agent_home(&case);
    for expires in [now() + 600, now() - 60] {
        private_file(&case.home().join("auth.json"), &codex_auth(expires));
        let guard = execution(&mut case);
        assert_eq!(
            refused(guard.hand_to_agent(&agent, TURN)),
            "native_profile_credential_expiring"
        );
        assert!(entries(&agent).is_empty());
    }
}

#[test]
fn a_missing_or_unusable_codex_record_is_refused() {
    let mut case = UseCase::new();
    let agent = agent_home(&case);
    let source = case.home().join("auth.json");
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN)),
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
            refused(guard.hand_to_agent(&agent, TURN)),
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
    let copy = guard.hand_to_agent(&agent, TURN).unwrap();
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
        refused(guard.hand_to_agent(&agent, TURN)),
        "native_profile_credential_unavailable"
    );
    drop(guard);
    std::fs::remove_file(&source).unwrap();
    std::fs::hard_link(&elsewhere, &source).unwrap();
    let guard = execution(&mut case);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN)),
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
            refused(guard.hand_to_agent(home, TURN)),
            "native_profile_agent_home_unsafe"
        );
    }
    assert!(entries(&agent).is_empty());
    assert!(entries(&open).is_empty());
}

#[cfg(target_os = "macos")]
fn security(home: &Path, args: &[&str], input: Option<&str>) -> String {
    let mut child = crate::spawn_gate::spawn(
        Command::new("/usr/bin/security")
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    if let Some(input) = input {
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "security {args:?} failed");
    String::from_utf8(output.stdout).unwrap()
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

#[cfg(target_os = "macos")]
fn claude_keychain(home: &Path, record: &Value) -> (PathBuf, String) {
    let folder = home.join("Library/Keychains");
    std::fs::create_dir_all(&folder).unwrap();
    for private in [home.join("Library"), folder.clone()] {
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let keychain = folder.join("login.keychain-db");
    let path = keychain.to_str().unwrap();
    security(home, &["create-keychain", "-p", "", path], None);
    let service = format!(
        "Claude Code-credentials-{}",
        &digest(home.to_str().unwrap().as_bytes())[..8]
    );
    let hex: String = record
        .to_string()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    security(
        home,
        &["-i"],
        Some(&format!(
            "add-generic-password -a unit -s \"{service}\" -X {hex} \"{path}\"\n"
        )),
    );
    (keychain, service)
}

#[cfg(target_os = "macos")]
#[test]
fn an_agent_gets_a_claude_copy_from_the_accounts_keychain_without_its_refresh_token() {
    let owner = PathBuf::from(std::env::var_os("HOME").unwrap());
    let owner_list = security(&owner, &["list-keychains", "-d", "user"], None);
    let mut case = UseCase::with_harness("claude-code");
    let home = case.home();
    let expires = (now() + 8 * 3600) * 1000;
    let (keychain, service) = claude_keychain(&home, &claude_record(expires));
    let agent = agent_home(&case);
    let guard = execution(&mut case);
    let copy = guard.hand_to_agent(&agent, TURN).unwrap();
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
    let kept = security(
        &home,
        &[
            "find-generic-password",
            "-s",
            &service,
            "-w",
            keychain.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        serde_json::from_str::<Value>(kept.trim_end()).unwrap(),
        claude_record(expires)
    );
    drop(copy);
    assert!(entries(&agent).is_empty());
    drop(guard);

    let soon = (now() + 600) * 1000;
    let mut late = UseCase::with_harness("claude-code");
    claude_keychain(&late.home(), &claude_record(soon));
    let agent = agent_home(&late);
    let guard = execution(&mut late);
    assert_eq!(
        refused(guard.hand_to_agent(&agent, TURN)),
        "native_profile_credential_expiring"
    );
    assert!(entries(&agent).is_empty());
    assert_eq!(
        security(&owner, &["list-keychains", "-d", "user"], None),
        owner_list
    );
}
