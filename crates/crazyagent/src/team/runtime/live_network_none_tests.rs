use super::TurnEnd;
use super::managed_agent::{ManagedAgent, SignedIn, claude, codex};
use std::path::Path;
use std::time::Duration;

const TURN: Duration = Duration::from_secs(180);

/// Runs one real turn of a signed-in agent with tools under the None network (S4b) and
/// checks what it reached: its profile allows only its model proxy's port and no DNS, the
/// turn succeeds through the proxy, and a fetch of another host from inside the sandbox
/// gets the proxy's 403. Inputs: the held sign-in, the launch harness, its executable and
/// the launch's name. Output: none; a failure fails the test.
fn turn_under_no_network(
    mut signed_in: SignedIn,
    harness: &'static str,
    executable: &Path,
    launch: &str,
) {
    let agent = ManagedAgent::with_no_network(harness, executable);
    let port = agent.model_proxy();
    assert!(
        port.is_some(),
        "an agent with tools under None gets a model proxy"
    );
    let port = port.unwrap();
    let rules = std::fs::read_to_string(&agent.sandbox_profile).unwrap();
    assert!(
        rules.contains(&format!("(remote ip \"localhost:{port}\")"))
            && !rules.contains("(remote ip \"*:")
            && !rules.contains("mDNSResponder"),
        "{rules}"
    );
    let profile = signed_in.acquire(launch);
    let (text, end, cause) = agent.run_turn(
        profile,
        "Reply with exactly the word OK and nothing else.",
        TURN,
    );
    println!("real {harness} turn under no network: end={end:?} cause={cause:?} reply={text:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("OK"), "{text}");
    let fetched = agent.shell(&format!(
        "HTTPS_PROXY=http://127.0.0.1:{port} /usr/bin/curl -sS -o /dev/null --max-time 10 https://example.com; echo exit=$?"
    ));
    let (said, code) = (
        String::from_utf8_lossy(&fetched.stderr),
        String::from_utf8_lossy(&fetched.stdout),
    );
    assert!(
        said.contains("403") && code.trim() == "exit=56",
        "the agent reached a host that is not its model's: {said} {code}"
    );
}

#[test]
#[ignore = "live qualification: needs a signed-in Claude subscription, CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_claude_subscription_turn_under_no_network_reaches_only_its_model() {
    turn_under_no_network(
        SignedIn::claude(),
        "claude-code",
        &claude(),
        "claude-live-network-none",
    );
}

#[test]
#[ignore = "live qualification: needs Codex signed in with ChatGPT, CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_chatgpt_turn_under_no_network_reaches_only_its_model() {
    turn_under_no_network(
        SignedIn::codex(),
        "codex",
        &codex(),
        "codex-live-network-none",
    );
}

#[test]
#[ignore = "live qualification: needs Codex signed in with an OpenAI API key, CRAZYAGENT_MANAGED_CODEX_KEY_PROFILE"]
fn a_real_codex_api_key_turn_under_no_network_reaches_only_its_model() {
    turn_under_no_network(
        SignedIn::codex_api_key(),
        "codex",
        &codex(),
        "codex-key-live-network-none",
    );
}
