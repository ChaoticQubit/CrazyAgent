use super::*;
use crate::model_gateway::relay::is_placeholder;

const PROVIDER_KEY: &str = "sk-provider-key-never-in-the-agent";

const KEY_REF: &str = "openai-key-0123456789abcdef0123";

/// Saves one provider key the way the add-account screen does: an owner-only folder
/// `account-keys` in the engine folder and an owner-only file named by its reference.
/// Input: the engine folder. Output: none.
fn save_key(engine: &Path) {
    let folder = engine.join(crate::storage::accounts::ACCOUNT_KEYS);
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = folder.join(KEY_REF);
    std::fs::write(&file, PROVIDER_KEY).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Opens a team runtime whose only harness is a stand-in Codex that does nothing.
/// Input: the fixture. Output: the runtime.
fn runtime(fixture: &Fixture) -> TeamRuntime {
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let access = fixture.access.clone();
    TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap()
}

#[test]
fn a_keyed_endpoint_agent_reaches_its_model_only_through_the_key_relay() {
    let fixture = Fixture::new(1);
    let runtime = runtime(&fixture);
    let account = TeamAccount {
        connection: reference(ENDPOINT),
        reach: TeamReach::Endpoint {
            base_url: "https://api.example.org/v1".into(),
            model: "gpt-test".into(),
            key: Some(reference(KEY_REF)),
        },
    };
    assert_eq!(
        fixture.agent(AgentRole::Sub, "codex", AgentStatus::Starting),
        1
    );
    let prepare = || {
        runtime.prepare(
            &fixture.setup,
            1,
            AgentRole::Sub,
            "codex",
            false,
            &account,
            false,
            crate::agent_git::Layout::Linked,
        )
    };
    let Err(missing) = prepare() else {
        panic!("an agent whose key is not saved starts");
    };
    assert!(missing.contains("account_key_missing"), "{missing}");
    save_key(&fixture.base);
    let (_, plan, mode) = prepare().unwrap();
    let relay = plan
        .relay
        .as_ref()
        .expect("a keyed endpoint runs through a relay");
    let Mode::Kept {
        account:
            AccountBinding::Url {
                base_url,
                key: Some(token),
                ..
            },
        ..
    } = &mode
    else {
        panic!("a keyed endpoint agent is kept on its relay");
    };
    assert_eq!(base_url, &format!("http://127.0.0.1:{}/v1", relay.port()));
    let scratch = tempfile::tempdir().unwrap();
    std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = scratch.path().canonicalize().unwrap();
    let written = crate::harnesses::interactive::write_key(&home, token).unwrap();
    let handed = std::fs::read_to_string(written).unwrap();
    assert!(is_placeholder(&handed));
    assert_eq!(handed, relay.token(), "the agent gets the relay's token");
    let profile = std::fs::read_to_string(&plan.sandbox_profile).unwrap();
    assert!(
        profile.contains(&format!("localhost:{}", relay.port())),
        "the agent reaches the relay"
    );
    assert!(!profile.contains("*:443"), "and no provider host");
    assert!(!profile.contains(PROVIDER_KEY));
}

#[test]
fn a_keyed_endpoint_pane_is_not_taken_over_by_a_later_engine() {
    let keyed = TeamReach::Endpoint {
        base_url: "https://api.example.org/v1".into(),
        model: "gpt-test".into(),
        key: Some(reference(KEY_REF)),
    };
    let keyless = TeamReach::Endpoint {
        base_url: "http://127.0.0.1:11434/v1".into(),
        model: "qwen2.5:0.5b".into(),
        key: None,
    };
    assert_eq!(adoptable(&keyed), Err(KEYED_PANE_NOT_ADOPTED.to_owned()));
    assert_eq!(adoptable(&keyless), Ok(()));
}
