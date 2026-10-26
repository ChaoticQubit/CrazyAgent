use super::*;

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    super::super::initialize(&connection).unwrap();
    connection
}

fn agent(
    connection: &Connection,
    project: &str,
    role: &str,
    harness: &str,
    task: Option<i64>,
) -> i64 {
    connection
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,task,created_ms,heartbeat_ms) VALUES (?1,?2,?3,1,'working',?4,1,1)",
            params![project, role, harness, task],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name.to_owned()).unwrap()
}

#[test]
fn a_prompt_to_a_sub_agent_is_recorded_on_its_task_and_noted_once() {
    let connection = database();
    let sub = agent(&connection, "alpha", "sub", "codex", Some(12));
    let master = agent(&connection, "alpha", "master", "claude-code", None);
    let idle = agent(&connection, "alpha", "sub", "claude-code", None);
    let other = agent(&connection, "beta", "sub", "codex", Some(3));

    let recorded = record(&connection, sub, "  use the staging db ", 50)
        .unwrap()
        .unwrap();
    assert_eq!(
        recorded,
        DirectInput {
            project: project("alpha"),
            agent: sub,
            text: "use the staging db".to_owned(),
            at_ms: 50,
        }
    );
    let task: Option<i64> = connection
        .query_row(
            "SELECT task FROM team_owner_prompts WHERE agent=?1",
            [sub],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(task, Some(12));
    assert_eq!(record(&connection, master, "hello", 51).unwrap(), None);
    assert_eq!(record(&connection, 9999, "hello", 51).unwrap(), None);
    assert_eq!(record(&connection, sub, "   ", 51).unwrap(), None);
    assert!(
        record(&connection, idle, "stop there", 52)
            .unwrap()
            .is_some()
    );
    assert!(
        record(&connection, other, "elsewhere", 53)
            .unwrap()
            .is_some()
    );

    let (note, newest) = notes(&connection, &project("alpha")).unwrap();
    assert_eq!(
        note,
        format!(
            "The owner sent these prompts straight to sub-agents since your last turn:\n- Owner told codex-{sub} on task 12: use the staging db\n- Owner told claude-code-{idle}: stop there\n\n"
        )
    );
    delivered(&connection, &project("alpha"), newest.unwrap()).unwrap();
    assert_eq!(
        notes(&connection, &project("alpha")).unwrap(),
        (String::new(), None)
    );
    let kept: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM team_owner_prompts WHERE project='alpha'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 0, "a prompt the master heard leaves no row behind");
    let (beta, _) = notes(&connection, &project("beta")).unwrap();
    assert!(beta.contains("elsewhere"));
}

#[test]
fn a_project_queues_a_bounded_number_of_unheard_inputs() {
    let connection = database();
    let sub = agent(&connection, "alpha", "sub", "codex", Some(1));
    for index in 0..PENDING_PER_PROJECT_MAX {
        assert!(
            record(&connection, sub, &format!("line {index}"), index)
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(record(&connection, sub, "one more", 999).unwrap(), None);
    let (note, newest) = notes(&connection, &project("alpha")).unwrap();
    assert_eq!(
        note.lines()
            .filter(|line| line.starts_with("- Owner told codex-"))
            .count() as i64,
        NOTES_PER_TURN_MAX
    );
    delivered(&connection, &project("alpha"), newest.unwrap()).unwrap();
    assert!(record(&connection, sub, "one more", 999).unwrap().is_some());
}

#[test]
fn a_long_input_is_cut_in_the_note() {
    let connection = database();
    let sub = agent(&connection, "alpha", "sub", "codex", None);
    let text = "y".repeat(DIRECT_INPUT_BYTES_MAX);
    assert!(record(&connection, sub, &text, 1).unwrap().is_some());
    assert_eq!(
        record(&connection, sub, &format!("{text}z"), 1).unwrap(),
        None
    );
    let (note, _) = notes(&connection, &project("alpha")).unwrap();
    assert!(note.contains(&format!(": {}\n", "y".repeat(NOTE_TEXT_BYTES_MAX))));
    assert!(!note.contains(&"y".repeat(NOTE_TEXT_BYTES_MAX + 1)));
}

#[test]
fn typed_terminal_input_kept_by_older_engines_is_erased_when_the_database_opens() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("engine.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE team_direct_inputs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                agent INTEGER NOT NULL,
                task INTEGER,
                harness TEXT NOT NULL,
                text TEXT NOT NULL,
                created_ms INTEGER NOT NULL,
                delivered INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO team_direct_inputs(project,agent,harness,text,created_ms,delivered)
                VALUES ('alpha',1,'codex','export TOKEN=hunter2-typed',1,1);",
        )
        .unwrap();
    super::super::initialize(&connection).unwrap();
    let left: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'team_direct_inputs%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    super::super::initialize(&connection).unwrap();
    let mut stored = Vec::new();
    for entry in std::fs::read_dir(folder.path()).unwrap() {
        stored.extend(std::fs::read(entry.unwrap().path()).unwrap());
    }
    assert!(
        !stored
            .windows(b"hunter2-typed".len())
            .any(|window| window == b"hunter2-typed"),
        "the erased text is gone from the database files"
    );
}
