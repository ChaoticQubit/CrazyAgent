use super::*;

const RESPONSES_LAUNCH_MANIFEST: &str = concat!(
    "schema: 1\nkind: harness\nid: \"responses-launch\"\nname: \"Responses Launch\"\n",
    "binaries:\n  - \"responses-launch\"\nheadless: \"responses-launch for the T22 rows\"\n",
    "model_selection: true\naccount_kinds:\n  - \"api_key\"\n",
    "  - \"open_ai_compatible_url\"\n  - \"local\"\n",
    "roles:\n  - \"agent\"\nsub_agents: \"app\"\npiped:\n  driver: \"jsonl-turn\"\n",
    "account_protocols:\n  - \"openai-responses\"\n",
    "accounts:\n  openai-responses:\n    env:\n",
    "      OPENAI_BASE_URL: \"{relay_url}\"\n      OPENAI_API_KEY: \"{token}\"\n",
);
const CHAT_LAUNCH_MANIFEST: &str = concat!(
    "schema: 1\nkind: harness\nid: \"chat-launch\"\nname: \"Chat Launch\"\n",
    "binaries:\n  - \"chat-launch\"\nheadless: \"chat-launch for the T22 rows\"\n",
    "model_selection: true\naccount_kinds:\n  - \"api_key\"\n",
    "  - \"open_ai_compatible_url\"\n  - \"local\"\n",
    "roles:\n  - \"agent\"\nsub_agents: \"app\"\npiped:\n  driver: \"jsonl-turn\"\n",
    "account_protocols:\n  - \"openai-chat\"\n",
    "accounts:\n  openai-chat:\n    env:\n",
    "      OPENAI_BASE_URL: \"{relay_url}\"\n      OPENAI_API_KEY: \"{token}\"\n",
);
const LAUNCH_ACCESS: &str = "chatgpt-launch-access-0123456789abcdef";

fn launch_source() -> std::sync::Arc<crate::model_gateway::relay::chatgpt::ChatGptSource> {
    launch_source_of("row-chatgpt-profile")
}

/// A ChatGPT source for these rows holding a fresh sign-in of one native profile. Input:
/// the profile reference. Output: the source.
fn launch_source_of(
    profile: &str,
) -> std::sync::Arc<crate::model_gateway::relay::chatgpt::ChatGptSource> {
    use crate::model_gateway::relay::chatgpt::{ChatGptSource, ChatGptTokens};
    let tokens = ChatGptTokens {
        access: crate::credentials::SecretString::bearer(LAUNCH_ACCESS.as_bytes().to_vec())
            .unwrap(),
        account_id: Some("acct-launch".to_owned()),
        expires: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            + std::time::Duration::from_secs(3600),
    };
    std::sync::Arc::new(ChatGptSource::new(
        crate::protocol::CredentialRef::new(profile).unwrap(),
        tokens,
        Box::new(|_: std::time::Duration, _: &dyn Fn() -> bool| {
            Err(crate::engine::EngineError("no renewal expected".into()))
        }),
    ))
}

/// The ChatGPT sign-in these rows launch on: account `row-chatgpt`, native profile
/// `row-chatgpt-profile`, made with Codex.
fn chatgpt_connection() -> crate::config::ModelConnection {
    serde_json::from_value(json!({
        "id": "row-chatgpt",
        "provider": "openai",
        "harness": "codex",
        "protocol": "native",
        "account_ref": "row-chatgpt-login",
        "endpoint": {"kind": "native"},
        "model": {"id": "native-default", "author": "openai"},
        "auth": {"method": "native_managed", "profile_ref": "row-chatgpt-profile"},
    }))
    .unwrap()
}

fn responses_registry() -> crate::harnesses::registry::Registry {
    crate::harnesses::registry::Registry::build(&[("responses-launch", RESPONSES_LAUNCH_MANIFEST)])
        .expect("responses launch manifest loads")
}

/// Makes a folder 0700, as the agent's home and the private homes inside it are.
/// Input: the folder, made with its parents when missing.
#[cfg(unix)]
fn private_dir(folder: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(folder).unwrap();
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// The agent's own home for these rows, a private folder the launch's home is made in.
/// Output: the folder's guard and its canonical path.
#[cfg(unix)]
fn agent_base() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let base = folder.path().canonicalize().unwrap();
    private_dir(&base);
    (folder, base)
}

fn chatgpt_request(harness: &str, account: AccountBinding) -> LaunchRequest {
    retry_request(
        harness,
        account,
        crate::provider_retry::ProviderRetry::HarnessDefault,
    )
}

fn sign_in_home() -> AccountBinding {
    AccountBinding::Subscription {
        home: PathBuf::from("/tmp/chatgpt-launch-home"),
    }
}

/// T22 R5, r2 F1 (rv-code4): a listed harness that lists `openai-responses`, launched on a
/// ChatGPT sign-in, is routed through a ChatGPT relay: the request it launches with names
/// only the relay's loopback URL and placeholder, never the access token, the account id
/// or the ChatGPT backend, and runs in the harness's own private home made in the agent's
/// home, `<agent home>/<harness id>/<account>`, never in the sign-in's managed home, where
/// the engine hands Codex the sign-in's own copy.
#[cfg(unix)]
#[test]
fn a_chatgpt_launch_of_a_listed_harness_names_only_the_chatgpt_relay_and_its_placeholder() {
    use std::os::unix::fs::PermissionsExt;
    let registry = responses_registry();
    let harness = registry.get("responses-launch").unwrap();
    let (_folder, base) = agent_base();
    let routed = super::super::route_chatgpt(
        chatgpt_request("responses-launch", sign_in_home()),
        harness,
        &chatgpt_connection(),
        &base,
        launch_source(),
    );
    let (request, relay) = match routed {
        Ok(routed) => routed,
        Err(problem) => panic!("the ChatGPT launch was not routed: {}", problem.0),
    };
    assert!(relay.is_chatgpt());
    match &request.account {
        AccountBinding::Url {
            home,
            base_url,
            key,
        } => {
            assert_eq!(home, &base.join("responses-launch").join("row-chatgpt"));
            assert_ne!(home, &PathBuf::from("/tmp/chatgpt-launch-home"));
            assert_eq!(base_url, &relay.base_url());
            assert!(base_url.starts_with("http://127.0.0.1:"));
            assert_eq!(key.as_ref().map(|key| key.reveal()), Some(relay.token()));
        }
        other => panic!("the launch kept {other:?}"),
    }
    let made = std::fs::symlink_metadata(request.account.home()).unwrap();
    assert!(made.is_dir());
    assert_eq!(made.permissions().mode() & 0o7777, 0o700);
    let shown = format!("{request:?}");
    for secret in [LAUNCH_ACCESS, "acct-launch", "chatgpt.com"] {
        assert!(!shown.contains(secret), "{shown}");
    }
}

/// T22 r2 F1 (rv-code4): a launch on a ChatGPT sign-in never starts where a native
/// sign-in's own record lies. An agent home or a private home that holds Codex's
/// `auth.json` or Claude Code's `.credentials.json`, as a file or as a link, is refused
/// before any relay starts, so no listed harness reads an access token from a file.
#[cfg(unix)]
#[test]
fn a_chatgpt_launch_is_refused_where_a_sign_in_record_lies() {
    let registry = responses_registry();
    let harness = registry.get("responses-launch").unwrap();
    let refused = |base: &std::path::Path| {
        super::super::route_chatgpt(
            chatgpt_request("responses-launch", sign_in_home()),
            harness,
            &chatgpt_connection(),
            base,
            launch_source(),
        )
        .err()
        .map(|problem| problem.0)
    };
    for record in ["auth.json", ".credentials.json"] {
        for inside in [false, true] {
            let (_folder, base) = agent_base();
            let folder = if inside {
                let own = base.join("responses-launch").join("row-chatgpt");
                private_dir(&base.join("responses-launch"));
                private_dir(&own);
                own
            } else {
                base.clone()
            };
            std::fs::write(folder.join(record), "{}").unwrap();
            assert_eq!(
                refused(&base).as_deref(),
                Some("launch_account_home_holds_sign_in"),
                "{record} in the private home: {inside}"
            );
        }
    }
    let (_folder, base) = agent_base();
    std::os::unix::fs::symlink("/private/tmp/no-such-record", base.join("auth.json")).unwrap();
    assert_eq!(
        refused(&base).as_deref(),
        Some("launch_account_home_holds_sign_in")
    );
}

/// T22 r2b (leader 20:07): the agent home a ChatGPT launch makes its private home in never
/// overlaps the sign-in's managed home, where the engine puts Codex's own copy of the
/// sign-in, even before that copy is there: an agent home that is the managed home, lies
/// inside it or holds it, or a link to it, is refused before any folder is made or relay
/// starts.
#[cfg(unix)]
#[test]
fn a_chatgpt_launch_is_refused_when_its_agent_home_overlaps_the_managed_home() {
    let registry = responses_registry();
    let harness = registry.get("responses-launch").unwrap();
    let refused = |managed: &std::path::Path, base: &std::path::Path| {
        super::super::route_chatgpt(
            chatgpt_request(
                "responses-launch",
                AccountBinding::Subscription {
                    home: managed.to_path_buf(),
                },
            ),
            harness,
            &chatgpt_connection(),
            base,
            launch_source(),
        )
        .err()
        .map(|problem| problem.0)
    };
    let (_folder, managed) = agent_base();
    let inside = managed.join("agent");
    private_dir(&inside);
    let (_linked, links) = agent_base();
    let link = links.join("managed");
    std::os::unix::fs::symlink(&managed, &link).unwrap();
    for base in [managed.as_path(), inside.as_path(), link.as_path()] {
        assert_eq!(
            refused(&managed, base).as_deref(),
            Some("launch_chatgpt_home_is_managed"),
            "{}",
            base.display()
        );
    }
    let (_outer, outer) = agent_base();
    let held = outer.join("managed");
    private_dir(&held);
    assert_eq!(
        refused(&held, &outer).as_deref(),
        Some("launch_chatgpt_home_is_managed")
    );
    assert!(!managed.join("responses-launch").exists());
    assert!(!outer.join("responses-launch").exists());
}

/// T22 r2 F1 (rv-code4): a request that already carries a sandbox profile is refused, since
/// that profile was made for the home the request was bound to, the sign-in's managed
/// home; the sandbox of a ChatGPT launch is made after routing, for its private home.
#[test]
fn a_chatgpt_launch_with_a_sandbox_made_before_routing_is_refused() {
    let registry = responses_registry();
    let mut request = chatgpt_request("responses-launch", sign_in_home());
    request.sandbox_profile = Some(PathBuf::from("/tmp/chatgpt-launch-home.sb"));
    let refused = super::super::route_chatgpt(
        request,
        registry.get("responses-launch").unwrap(),
        &chatgpt_connection(),
        std::path::Path::new("/private/tmp/no-such-agent-home"),
        launch_source(),
    );
    assert_eq!(
        refused.err().map(|problem| problem.0).as_deref(),
        Some("launch_chatgpt_sandbox_premade")
    );
}

/// T22 r2 F2 (rv-code4): a launch is routed only through a source read from its own
/// sign-in. A source of another native profile is a wiring fault that would spend that
/// account's plan for this run, so routing stops on an assertion before any relay starts.
#[test]
#[should_panic(expected = "a ChatGPT relay serves only the sign-in it was read from")]
fn a_chatgpt_launch_stops_on_a_source_of_another_sign_in() {
    let registry = responses_registry();
    let _ = super::super::route_chatgpt(
        chatgpt_request("responses-launch", sign_in_home()),
        registry.get("responses-launch").unwrap(),
        &chatgpt_connection(),
        std::path::Path::new("/private/tmp/no-such-agent-home"),
        launch_source_of("another-chatgpt-profile"),
    );
}

/// T22 R5: Codex and Claude Code keep their own sign-in paths, so a ChatGPT relay is never
/// started for an own adapter, for a listed harness that does not list `openai-responses`,
/// or for a launch that is not bound to a sign-in's home.
#[test]
fn a_chatgpt_relay_is_refused_for_own_adapters_other_formats_and_keyed_launches() {
    let registry = crate::harnesses::registry::Registry::build(&[
        ("responses-launch", RESPONSES_LAUNCH_MANIFEST),
        ("chat-launch", CHAT_LAUNCH_MANIFEST),
    ])
    .expect("launch manifests load");
    let code = |name: &str, harness: &crate::harnesses::registry::Harness, account| {
        super::super::route_chatgpt(
            chatgpt_request(name, account),
            harness,
            &chatgpt_connection(),
            std::path::Path::new("/private/tmp/no-such-agent-home"),
            launch_source(),
        )
        .err()
        .map(|problem| problem.0)
    };
    let bundled = crate::harnesses::registry::registry();
    for own in ["codex", "claude-code", "rcli"] {
        assert_eq!(
            code(own, bundled.get(own).unwrap(), sign_in_home()).as_deref(),
            Some("launch_chatgpt_unsupported"),
            "{own}"
        );
    }
    assert_eq!(
        code(
            "chat-launch",
            registry.get("chat-launch").unwrap(),
            sign_in_home()
        )
        .as_deref(),
        Some("launch_chatgpt_unsupported")
    );
    let keyed = AccountBinding::ApiKey {
        home: PathBuf::from("/tmp/chatgpt-launch-home"),
        key: super::super::Secret::new("sk-launch-key".to_owned()).unwrap(),
        base_url: None,
    };
    assert_eq!(
        code(
            "responses-launch",
            registry.get("responses-launch").unwrap(),
            keyed
        )
        .as_deref(),
        Some("launch_chatgpt_unsupported")
    );
}
