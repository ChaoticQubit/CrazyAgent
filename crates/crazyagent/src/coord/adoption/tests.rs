use super::*;

#[test]
fn hookless_agent_raises_no_flags() {
    let directory = tempfile::tempdir().unwrap();
    let db = Connection::open(directory.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    let project = ProjectId::new("adoption-hooks").unwrap();
    db.execute(
        "INSERT INTO projects(id, body) VALUES (?1, '{}')",
        [project.as_str()],
    )
    .unwrap();
    let agent = |hookless: bool| -> AgentId {
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,hookless)
             VALUES (?1,'sub','claude-code',1,'working',0,0,?2)",
            params![project.as_str(), hookless],
        )
        .unwrap();
        let id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO coord_tool_use(agent,tool,count,last) VALUES (?1,'Edit',3,1)",
            [id],
        )
        .unwrap();
        id
    };
    let hooked = agent(false);
    let hookless = agent(true);
    let now = UNGROUNDED_MS + 1;

    let view = view(&db, &project, &[], now).unwrap();
    let row = |id: AgentId| {
        view.agents
            .iter()
            .find(|agent| agent.agent == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        row(hooked).flags,
        vec![Flag::EditsWithoutWork, Flag::Ungrounded],
        "an agent with hooks that edits without work for 10 minutes ungrounded raises both"
    );
    let bare = row(hookless);
    assert!(bare.hookless);
    assert!(
        bare.flags.is_empty(),
        "a launch without hooks raises no flag: {:?}",
        bare.flags
    );
    assert!(!row(hooked).hookless);

    tick(&db, &project, now).unwrap();
    let notes = |id: AgentId| -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM coord_messages WHERE to_agent=?1 AND kind='note'",
            [id],
            |found| found.get(0),
        )
        .unwrap()
    };
    assert_eq!(notes(hooked), 2, "one reminder per flag");
    assert_eq!(
        notes(hookless),
        0,
        "no nudge reaches an agent without hooks"
    );
}
