use super::{TeamReach, reach_in};
use crate::config::ModelConnection;
use crate::harnesses::registry::{Registry, registry};

const RESPONSES_TEAM_MANIFEST: &str = concat!(
    "schema: 1\nkind: harness\nid: \"responses-team\"\nname: \"Responses Team\"\n",
    "binaries:\n  - \"responses-team\"\nheadless: \"responses-team for the T22 reach row\"\n",
    "model_selection: true\naccount_kinds:\n  - \"api_key\"\n",
    "  - \"open_ai_compatible_url\"\n  - \"local\"\n",
    "roles:\n  - \"agent\"\nsub_agents: \"app\"\npiped:\n  driver: \"jsonl-turn\"\n",
    "account_protocols:\n  - \"openai-responses\"\n",
    "accounts:\n  openai-responses:\n    env:\n",
    "      OPENAI_BASE_URL: \"{relay_url}\"\n      OPENAI_API_KEY: \"{token}\"\n",
);

fn chatgpt_sign_in() -> ModelConnection {
    serde_json::from_value(serde_json::json!({
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

fn responses_key() -> ModelConnection {
    serde_json::from_value(serde_json::json!({
        "id": "row-responses",
        "provider": "openai",
        "harness": "rcli",
        "protocol": "openai-responses",
        "account_ref": "row-responses-login",
        "endpoint": {"kind": "http", "url": "https://api.openai.com/v1"},
        "model": {"id": "gpt-5-codex", "author": "openai"},
        "auth": {"method": "api_key", "credential_ref": "row-key"},
    }))
    .unwrap()
}

/// T22 r1 F3 (rv-code4): a team agent of a listed harness that takes a ChatGPT sign-in only
/// through the ChatGPT relay never gets a managed reach, which would hand it the sign-in's
/// own credential copy. The harness runs team agents and the sign-in fits it, so only the
/// reach guard keeps the reach empty; the same harness reaches a keyed Responses account,
/// and Codex, the harness the sign-in was made with, still gets its managed reach.
#[test]
fn a_chatgpt_sign_in_gives_a_listed_harness_no_managed_reach() {
    let listed = Registry::build(&[("responses-team", RESPONSES_TEAM_MANIFEST)])
        .expect("responses team manifest loads");
    let harness = listed.get("responses-team").unwrap();
    let sign_in = chatgpt_sign_in();
    assert!(harness.runnable());
    assert!(harness.account_fits(&sign_in));
    assert!(!harness.signs_in_natively(&sign_in));

    assert_eq!(reach_in(&listed, &sign_in, "responses-team"), None);
    assert!(matches!(
        reach_in(&listed, &responses_key(), "responses-team"),
        Some(TeamReach::Endpoint { .. })
    ));
    assert!(matches!(
        reach_in(registry(), &sign_in, "codex"),
        Some(TeamReach::Managed { .. })
    ));
}
