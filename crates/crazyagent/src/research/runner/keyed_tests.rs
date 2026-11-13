use super::*;
use std::os::unix::fs::PermissionsExt;

const PROVIDER_KEY: &str = "sk-provider-key-never-in-the-agent";

const KEY_REF: &str = "openai-key-0123456789abcdef0123";

const RELAY_URL: &str = "http://127.0.0.1:41234/v1";

fn key_ref() -> CredentialRef {
    CredentialRef::new(KEY_REF).unwrap()
}

/// The arguments and environment one command was given, as text.
/// Input: the command. Output: its arguments and its `NAME=value` variables.
fn seen(command: &Command) -> (Vec<String>, Vec<String>) {
    let args = command
        .get_args()
        .map(|argument| argument.to_str().unwrap().to_owned())
        .collect();
    let envs = command
        .get_envs()
        .map(|(name, value)| {
            format!(
                "{}={}",
                name.to_str().unwrap(),
                value.and_then(|value| value.to_str()).unwrap_or("")
            )
        })
        .collect();
    (args, envs)
}

#[test]
fn a_keyed_research_codex_reads_its_relay_token_from_a_file_in_its_home() {
    let relayed = ResearchAccount::Endpoint {
        base_url: RELAY_URL.into(),
        model: "gpt-test".into(),
        key: Some(key_ref()),
    };
    let mut command = Command::new("/usr/bin/true");
    codex_arguments(
        &mut command,
        Path::new("/work"),
        Path::new("/work/report.md"),
        Path::new("/home/agent"),
        false,
        &relayed,
    )
    .unwrap();
    let (args, _) = seen(&command);
    for setting in [
        "model_providers.crazyagent_endpoint.base_url=\"http://127.0.0.1:41234/v1\"",
        "model_providers.crazyagent_endpoint.auth={command=\"/bin/cat\",args=[\"/home/agent/.crazyagent-key\"]}",
    ] {
        assert!(
            args.windows(2).any(|pair| pair == ["-c", setting]),
            "{setting}: {args:?}"
        );
    }
    let unrelayed = ResearchAccount::Endpoint {
        base_url: "https://api.openai.com/v1".into(),
        model: "gpt-test".into(),
        key: Some(key_ref()),
    };
    let mut command = Command::new("/usr/bin/true");
    assert!(
        codex_arguments(
            &mut command,
            Path::new("/work"),
            Path::new("/work/report.md"),
            Path::new("/home/agent"),
            false,
            &unrelayed,
        )
        .is_err(),
        "a keyed endpoint is reached only through its relay"
    );
}

#[test]
fn a_keyed_research_claude_gets_its_relay_token_from_an_api_key_helper() {
    let relayed = ResearchAccount::AnthropicEndpoint {
        base_url: "http://127.0.0.1:41234".into(),
        model: "claude-test".into(),
        key: Some(key_ref()),
    };
    let mut command = Command::new("/usr/bin/true");
    claude_arguments(
        &mut command,
        Path::new("/home/agent"),
        Path::new("/home/agent/tmp"),
        false,
        None,
        ClaudeOutput::Text,
        &relayed,
    )
    .unwrap();
    let (args, envs) = seen(&command);
    let settings: Vec<&String> = args
        .windows(2)
        .filter(|pair| pair[0] == "--settings")
        .map(|pair| &pair[1])
        .collect();
    assert_eq!(settings.len(), 1, "{args:?}");
    let settings: serde_json::Value = serde_json::from_str(settings[0]).unwrap();
    assert_eq!(
        settings["apiKeyHelper"],
        "/bin/cat '/home/agent/.crazyagent-key'"
    );
    assert_eq!(settings["disableAllHooks"], true);
    assert!(args.iter().any(|argument| argument == "--bare"));
    assert!(envs.contains(&"ANTHROPIC_BASE_URL=http://127.0.0.1:41234".to_owned()));
    assert!(
        envs.iter()
            .all(|env| !env.starts_with("ANTHROPIC_API_KEY=")),
        "the helper is the only key source: {envs:?}"
    );
}

#[test]
fn a_keyed_research_account_is_put_behind_a_key_relay_and_a_keyless_one_is_not() {
    let engine = tempfile::tempdir().unwrap();
    let account = ResearchAccount::Endpoint {
        base_url: "https://api.example.org/v1".into(),
        model: "gpt-test".into(),
        key: Some(key_ref()),
    };
    match relayed(engine.path(), &account) {
        Err(RunError::Account(code)) => assert_eq!(code, "account_key_missing"),
        other => panic!("an account whose key is not saved runs: {:?}", other.err()),
    }
    let folder = engine.path().join(crate::storage::accounts::ACCOUNT_KEYS);
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = folder.join(KEY_REF);
    std::fs::write(&file, PROVIDER_KEY).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (behind, relay) = relayed(engine.path(), &account).unwrap();
    let relay = relay.expect("a keyed account runs through a relay");
    assert_eq!(
        behind,
        ResearchAccount::Endpoint {
            base_url: format!("http://127.0.0.1:{}/v1", relay.port()),
            model: "gpt-test".into(),
            key: Some(key_ref()),
        }
    );
    assert_eq!(
        model_remote(&behind).unwrap(),
        SandboxRemote::Loopback(relay.port())
    );
    let keyless = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:11434/v1".into(),
        model: "qwen2.5:0.5b".into(),
        key: None,
    };
    let (same, none) = relayed(engine.path(), &keyless).unwrap();
    assert_eq!(same, keyless);
    assert!(none.is_none());
}
