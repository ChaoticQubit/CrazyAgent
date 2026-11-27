use super::*;

#[test]
fn a_launch_error_that_closes_an_agent_names_its_plain_code_on_the_agent_and_its_task() {
    assert_eq!(
        launch_end(LaunchError("launch_deadline".into(), None)),
        (TurnEnd::TimedOut, Some("launch_deadline".to_owned()))
    );
    let (end, cause) = launch_end(LaunchError("launch_protocol_failed".into(), None));
    assert_eq!(end, TurnEnd::Closed);
    assert_eq!(
        with_code(PROCESS_CLOSED, cause.as_deref()),
        "The agent process closed. (launch_protocol_failed)"
    );
    assert_eq!(
        with_code(
            "The agent could not start",
            Some("No such file: /Users/mira/key")
        ),
        "The agent could not start. (engine_text_withheld)"
    );
    assert_eq!(with_code(PROCESS_CLOSED, None), PROCESS_CLOSED);
    assert_eq!(
        with_code(
            "The master stopped before it replied.",
            Some("launch_input_closed")
        ),
        "The master stopped before it replied. (launch_input_closed)"
    );
    let fixture = Fixture::new(1);
    let mut team = fixture.runtime();
    fixture
        .connection
        .execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms) VALUES (?1,'master','codex',0,'Write the page','running',1)",
            [PROJECT],
        )
        .unwrap();
    let task = fixture.connection.last_insert_rowid();
    team.finish_task(task, "", TurnEnd::Closed, Some("launch_input_closed"))
        .unwrap();
    let problem: String = fixture
        .connection
        .query_row("SELECT error FROM team_tasks WHERE id=?1", [task], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        problem,
        "The sub-agent stopped unexpectedly. (launch_input_closed)"
    );
}

/// A launch whose engine folder is past the entry bound fails with a code of its own, not the
/// plain sandbox refusal, so the owner is told which step to take.
#[cfg(unix)]
#[test]
fn an_engine_folder_past_its_entry_bound_gives_a_launch_its_own_code() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::tempdir().unwrap();
    let base = scratch.path().canonicalize().unwrap();
    let [root, project, home, tmp] =
        ["engine", "project", "home", "tmp"].map(|name| base.join(name));
    for folder in [&root, &project, &home, &tmp] {
        std::fs::create_dir(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for at in 0..1025 {
        std::fs::write(root.join(format!("entry-{at}")), "").unwrap();
    }
    let refused = ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .and_then(|sandbox| sandbox.guard_engine(&root))
    .map(|_| ());
    let error = refused.unwrap_err();
    assert_eq!(sandbox_code(&error), "engine_folder_full", "{error:?}");
}
