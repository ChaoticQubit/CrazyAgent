use super::*;
use rusqlite::Connection;

/// Makes the tables the learning signals read, with only the columns they use.
/// Input: the connection. Output: none.
fn tables(db: &Connection) {
    db.execute_batch(
        "CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL);
         CREATE TABLE conversation_entries (project TEXT NOT NULL, kind TEXT NOT NULL, text TEXT NOT NULL);
         CREATE TABLE team_agents (id INTEGER PRIMARY KEY, project TEXT NOT NULL, role TEXT NOT NULL);
         CREATE TABLE questions_log (id INTEGER PRIMARY KEY, project TEXT NOT NULL, status TEXT NOT NULL);
         CREATE TABLE landing_change_sets (id TEXT PRIMARY KEY, project TEXT NOT NULL, state TEXT NOT NULL);
         CREATE TABLE ide_workspaces (project TEXT PRIMARY KEY, show_all INTEGER NOT NULL, body TEXT NOT NULL);",
    )
    .unwrap();
}

fn read(db: &mut Connection) -> LearningProgress {
    let tx = db.transaction().unwrap();
    progress(&tx).unwrap()
}

fn count(progress: &LearningProgress, id: StepId) -> Option<u32> {
    progress
        .steps
        .iter()
        .find(|step| step.id == id)
        .unwrap()
        .count
}

#[test]
fn a_fresh_engine_lists_every_step_in_order_with_nothing_done() {
    let mut db = Connection::open_in_memory().unwrap();
    let progress = read(&mut db);
    let ids: Vec<StepId> = progress.steps.iter().map(|step| step.id).collect();
    assert_eq!(
        ids,
        [
            StepId::Project,
            StepId::Master,
            StepId::Agents,
            StepId::Question,
            StepId::ChangeSet,
            StepId::Ide,
            StepId::Terminal,
            StepId::Process,
            StepId::Metrics,
            StepId::Usage,
        ]
    );
    assert!(progress.steps.iter().all(|step| !step.done()));
    for id in [StepId::Project, StepId::Master, StepId::Ide] {
        assert_eq!(
            count(&progress, id),
            Some(0),
            "{id:?} reads 0 with no table"
        );
    }
    for id in [
        StepId::Terminal,
        StepId::Process,
        StepId::Metrics,
        StepId::Usage,
    ] {
        assert_eq!(count(&progress, id), None, "{id:?} has no signal yet");
    }
}

#[test]
fn each_step_counts_only_the_rows_that_show_the_owner_did_it() {
    let mut db = Connection::open_in_memory().unwrap();
    tables(&db);
    db.execute_batch(
        "INSERT INTO projects VALUES ('p1','{}'),('p2','{}');
         INSERT INTO conversation_entries VALUES ('p1','brief','Goal.'),('p1','message','Hi.');
         INSERT INTO team_agents(project,role) VALUES ('p1','master'),('p1','sub'),('p1','sub');
         INSERT INTO questions_log(project,status) VALUES ('p1','pending'),('p1','dismissed'),
             ('p1','answered'),('p1','answered_in_terminal');
         INSERT INTO landing_change_sets VALUES ('c1','p1','pending'),('c2','p1','discarded'),
             ('c3','p1','accepted');",
    )
    .unwrap();
    let progress = read(&mut db);
    assert_eq!(count(&progress, StepId::Project), Some(2));
    assert_eq!(
        count(&progress, StepId::Master),
        Some(1),
        "a brief is not a message"
    );
    assert_eq!(
        count(&progress, StepId::Agents),
        Some(2),
        "the master is not a team agent"
    );
    assert_eq!(count(&progress, StepId::Question), Some(2));
    assert_eq!(count(&progress, StepId::ChangeSet), Some(1));
    assert_eq!(count(&progress, StepId::Ide), Some(0));
    db.execute("INSERT INTO ide_workspaces VALUES ('p1',0,'{}')", [])
        .unwrap();
    assert!(
        read(&mut db)
            .steps
            .iter()
            .find(|step| step.id == StepId::Ide)
            .unwrap()
            .done()
    );
}

#[test]
fn a_count_stops_at_its_cap() {
    let mut db = Connection::open_in_memory().unwrap();
    tables(&db);
    for index in 0..150 {
        db.execute(
            "INSERT INTO projects VALUES (?1,'{}')",
            [format!("p{index}")],
        )
        .unwrap();
    }
    assert_eq!(count(&read(&mut db), StepId::Project), Some(COUNT_MAX));
}

#[test]
fn the_read_carries_counts_and_fixed_words_only_never_row_text() {
    let mut db = Connection::open_in_memory().unwrap();
    tables(&db);
    db.execute_batch(
        "INSERT INTO projects VALUES ('secret-project','{\"name\":\"secret-name\"}');
         INSERT INTO conversation_entries VALUES ('secret-project','message','secret-text');",
    )
    .unwrap();
    let wire = serde_json::to_string(&read(&mut db)).unwrap();
    assert!(!wire.contains("secret"), "{wire}");
    let back: LearningProgress = serde_json::from_str(&wire).unwrap();
    assert_eq!(back, read(&mut db));
}

#[test]
fn every_step_has_a_title_and_two_sentences() {
    let mut db = Connection::open_in_memory().unwrap();
    for step in read(&mut db).steps {
        assert!(
            !step.title.is_empty() && !step.title.ends_with('.'),
            "{step:?}"
        );
        assert_eq!(step.body.matches(". ").count(), 1, "{step:?}");
        assert!(step.body.ends_with('.'), "{step:?}");
    }
}
