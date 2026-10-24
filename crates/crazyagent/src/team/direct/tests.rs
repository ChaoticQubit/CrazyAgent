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
fn typed_keys_become_lines_on_enter() {
    let mut lines = InputLines::default();
    assert_eq!(lines.feed(b"use the "), Vec::<String>::new());
    assert_eq!(
        lines.feed(b"staging db\r"),
        vec!["use the staging db".to_owned()]
    );
    assert_eq!(lines.feed(b"\r  \r"), Vec::<String>::new());
    assert_eq!(
        lines.feed(b"two\rlines\n"),
        vec!["two".to_owned(), "lines".to_owned()]
    );
}

#[test]
fn editing_keys_change_the_line_and_other_keys_are_dropped() {
    let mut lines = InputLines::default();
    assert_eq!(lines.feed(b"fixx\x7f it\r"), vec!["fix it".to_owned()]);
    assert_eq!(
        lines.feed("caf\u{e9}\x7fe\r".as_bytes()),
        vec!["cafe".to_owned()]
    );
    assert_eq!(lines.feed(b"wrong\x15right\r"), vec!["right".to_owned()]);
    assert_eq!(lines.feed(b"gone\x03kept\r"), vec!["kept".to_owned()]);
    assert_eq!(
        lines.feed(b"up\x1b[A\x1b[1;5Cand\x1bOB\x1bx done\x01\r"),
        vec!["upand done".to_owned()]
    );
    assert_eq!(
        lines.feed(b"\x1b]0;title\x07\x1b]8;;link\x1b\\shown\r"),
        vec!["shown".to_owned()]
    );
}

#[test]
fn a_paste_keeps_its_newlines_until_enter() {
    let mut lines = InputLines::default();
    assert_eq!(
        lines.feed(b"\x1b[200~first\r\nsecond\x1b[201~"),
        Vec::<String>::new()
    );
    assert_eq!(lines.feed(b"\r"), vec!["first\n\nsecond".to_owned()]);
}

#[test]
fn a_sequence_split_across_inputs_is_still_dropped() {
    let mut lines = InputLines::default();
    assert_eq!(lines.feed(b"a\x1b"), Vec::<String>::new());
    assert_eq!(lines.feed(b"["), Vec::<String>::new());
    assert_eq!(lines.feed(b"Db\r"), vec!["ab".to_owned()]);
}

#[test]
fn a_line_is_bounded() {
    let mut lines = InputLines::default();
    let long = vec![b'x'; DIRECT_INPUT_BYTES_MAX * 2];
    assert_eq!(lines.feed(&long), Vec::<String>::new());
    let line = lines.feed(b"\r");
    assert_eq!(line, vec!["x".repeat(DIRECT_INPUT_BYTES_MAX)]);
}

#[test]
fn input_to_a_sub_agent_is_recorded_on_its_task_and_noted_once() {
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
            "SELECT task FROM team_direct_inputs WHERE agent=?1",
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
            "The owner typed straight into sub-agents since your last turn:\n- Owner told codex-{sub} on task 12: use the staging db\n- Owner told claude-code-{idle}: stop there\n\n"
        )
    );
    delivered(&connection, &project("alpha"), newest.unwrap()).unwrap();
    assert_eq!(
        notes(&connection, &project("alpha")).unwrap(),
        (String::new(), None)
    );
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
