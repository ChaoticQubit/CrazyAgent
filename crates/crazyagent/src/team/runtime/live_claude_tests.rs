use super::TurnEnd;
use super::managed_agent::{ManagedAgent, SignedIn, claude};
use std::time::Duration;

#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_claude_team_agent_turn_is_signed_in_inside_the_agent_sandbox() {
    let mut signed_in = SignedIn::claude();
    let agent = ManagedAgent::new("claude-code", &claude());
    let profile = signed_in.acquire("claude-live-team-turn");
    let managed = profile.profile_path().unwrap();
    let rules = std::fs::read_to_string(&agent.sandbox_profile).unwrap();
    let (text, end, cause) = agent.run_turn(
        profile,
        "Reply with exactly the word OK and nothing else.",
        Duration::from_secs(120),
    );
    println!("real claude team agent turn: end={end:?} cause={cause:?} reply={text:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("OK"), "{text}");
    assert!(!agent.home.join(".credentials.json").exists());
    assert!(!agent.home.starts_with(&managed));
    assert!(!rules.contains(managed.to_str().unwrap()));
}
