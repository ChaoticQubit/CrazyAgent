use super::TurnEnd;
use super::managed_agent::{ManagedAgent, SignedIn, codex};
use std::path::Path;
use std::time::Duration;

/// Reads every rollout Codex wrote below a sessions folder. Input: the folder.
/// Output: the rollouts' text, joined.
fn rollouts(folder: &Path) -> String {
    let mut text = String::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for item in std::fs::read_dir(&folder).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                text.push_str(&std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    text
}

/// A real Codex team agent runs a turn with the launch files of its own home read-only and
/// its skills folder pinned (LL-2): the turn succeeds, so Codex starts without writing them,
/// and the rollout it wrote holds the engine's developer instructions, so it read its
/// `config.toml`.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_agent_reads_its_developer_instructions_with_its_launch_files_read_only() {
    let mut signed_in = SignedIn::codex();
    let agent = ManagedAgent::new("codex", &codex());
    let profile = signed_in.acquire("codex-live-launch-files");
    let instructions = agent.instructions();
    let first = instructions
        .lines()
        .find(|line| line.trim().len() > 20)
        .expect("the engine gives the agent instructions");
    let (text, end, cause) = agent.run_turn(
        profile,
        "Reply with exactly the word OK and nothing else.",
        Duration::from_secs(120),
    );
    println!(
        "real codex agent with read-only launch files: end={end:?} cause={cause:?} reply={text:?}"
    );
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("OK"), "{text}");
    let written = rollouts(&agent.home.join("sessions"));
    let quoted = serde_json::to_string(first).unwrap();
    assert!(
        written.contains(quoted.trim_matches('"')),
        "no rollout holds the developer instructions line {first:?}"
    );
}

#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_team_agent_turn_is_signed_in_inside_the_agent_sandbox() {
    let mut signed_in = SignedIn::codex();
    let agent = ManagedAgent::new("codex", &codex());
    let profile = signed_in.acquire("codex-live-team-turn");
    let managed = profile.profile_path().unwrap();
    let rules = std::fs::read_to_string(&agent.sandbox_profile).unwrap();
    let (text, end, cause) = agent.run_turn(
        profile,
        "Reply with exactly the word OK and nothing else.",
        Duration::from_secs(120),
    );
    println!("real codex team agent turn: end={end:?} cause={cause:?} reply={text:?}");
    assert_eq!(end, TurnEnd::Success, "{cause:?}");
    assert!(text.contains("OK"), "{text}");
    assert!(!agent.home.join("auth.json").exists());
    assert!(!agent.home.starts_with(&managed));
    assert!(!rules.contains(managed.to_str().unwrap()));
}
