use super::*;

/// The status log keeps only its newest 20000 rows, whichever project owns them: a new row
/// drops every row that far behind it and keeps the next one (HB-S5-I, TEAM-11-L2).
#[test]
fn the_status_log_keeps_only_its_newest_rows() {
    let fixture = Fixture::new(1);
    let agent = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Working);
    for (id, project) in [
        (10_001, "another-project"),
        (10_002, PROJECT),
        (30_000, PROJECT),
    ] {
        fixture
            .connection
            .execute(
                "INSERT INTO team_status_log(id,project,agent,status,at_ms) VALUES (?1,?2,?3,'idle',1)",
                params![id, project, agent],
            )
            .unwrap();
    }
    fixture
        .connection
        .execute("UPDATE team_agents SET status='idle' WHERE id=?1", [agent])
        .unwrap();
    let ids: Vec<i64> = fixture
        .connection
        .prepare("SELECT id FROM team_status_log ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ids, [10_002, 30_000, 30_001]);
}
