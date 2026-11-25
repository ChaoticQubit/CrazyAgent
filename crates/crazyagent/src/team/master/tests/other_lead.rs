use super::*;

/// A lead's `merge` reaches only its own workers: a worker of another lead is not one of
/// its sub-agents, whatever it is called (feat/coord-spawn 372ee630).
#[test]
fn merge_refuses_a_worker_of_another_lead() {
    let mut fixture = Fixture::new();
    let access = codex_pool();
    let agents = fixture.base.join("team/agents");
    team_agent(&fixture, LEAD, "claude-code", "working", None);
    team_agent(&fixture, 61, "claude-code", "working", None);
    team_agent(&fixture, WORKER, "codex", "idle", Some(LEAD));
    let answer = lead_call(
        &mut fixture,
        &access,
        &agents,
        61,
        Call::Merge {
            agent: WORKER.to_string(),
        },
    );
    assert_eq!(
        answer,
        (
            format!(
                "Not merged: none of your own sub-agents is called {WORKER}. Call {}() for their names.",
                tool::INSPECT_WORK_TOOL
            ),
            true
        )
    );
}
