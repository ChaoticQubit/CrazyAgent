use super::*;

#[test]
fn teammate_cannot_lead() {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join("engine.db")).unwrap();
    crate::team::initialize(&connection).unwrap();
    let project = ProjectId::new("team-caps").unwrap();
    let agent = |role: &str, parent: Option<i64>| {
        connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,parent)
                 VALUES (?1,?2,'claude-code',1,'working',1,1,?3)",
                params![project.as_str(), role, parent],
            )
            .unwrap();
        connection.last_insert_rowid()
    };
    let lead = agent("sub", None);
    let teammate = agent("teammate", Some(lead));
    assert_eq!(
        admit_sub_agent(&connection, &project, teammate),
        Err("A teammate does not start sub-agents. Its lead starts them.".to_owned())
    );
    assert_eq!(
        admit_sub_agent(&connection, &project, lead),
        Ok(()),
        "its lead still starts sub-agents with no limit set"
    );
    let other = ProjectId::new("team-caps-other").unwrap();
    assert_eq!(
        admit_sub_agent(&connection, &other, teammate),
        Err("That lead is not an agent of this project.".to_owned())
    );
}
