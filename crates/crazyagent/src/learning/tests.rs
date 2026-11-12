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
    initialize(db).unwrap();
}

fn read(db: &mut Connection) -> LearningProgress {
    let tx = db.transaction().unwrap();
    progress(&tx).unwrap()
}

fn count(progress: &LearningProgress, id: StepId) -> u32 {
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
    assert!(progress.steps.iter().all(|step| step.count == 0));
    assert!(!progress.skipped);
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
    assert_eq!(count(&progress, StepId::Project), 2);
    assert_eq!(
        count(&progress, StepId::Master),
        1,
        "a brief is not a message"
    );
    assert_eq!(
        count(&progress, StepId::Agents),
        2,
        "the master is not a team agent"
    );
    assert_eq!(count(&progress, StepId::Question), 2);
    assert_eq!(count(&progress, StepId::ChangeSet), 1);
    assert_eq!(count(&progress, StepId::Ide), 0);
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
    assert_eq!(count(&read(&mut db), StepId::Project), COUNT_MAX);
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

fn set(db: &mut Connection, step: MarkStep, on: bool) {
    let tx = db.transaction().unwrap();
    assert_eq!(mark(&tx, step, on).unwrap(), Outcome::Applied);
    tx.commit().unwrap();
}

fn marks(db: &Connection) -> Vec<(String, i64)> {
    let mut statement = db
        .prepare("SELECT step, marked_ms FROM learning_marks ORDER BY step")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn done(progress: &LearningProgress) -> Vec<StepId> {
    progress
        .steps
        .iter()
        .filter(|step| step.done())
        .map(|step| step.id)
        .collect()
}

#[test]
fn each_mark_ticks_only_its_own_step() {
    for (step, id) in [
        (MarkStep::Terminal, StepId::Terminal),
        (MarkStep::Process, StepId::Process),
        (MarkStep::Metrics, StepId::Metrics),
        (MarkStep::Usage, StepId::Usage),
    ] {
        let mut db = Connection::open_in_memory().unwrap();
        tables(&db);
        set(&mut db, step, true);
        let progress = read(&mut db);
        assert_eq!(done(&progress), [id], "{step:?}");
        assert_eq!(count(&progress, id), 1);
        assert!(progress.marked(step));
        assert!(!progress.skipped);
        assert!(!progress.marked(MarkStep::Skipped));
    }
}

#[test]
fn a_repeat_mark_keeps_one_row_and_its_first_time() {
    let mut db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let before = crate::team::now_ms();
    set(&mut db, MarkStep::Process, true);
    let after = crate::team::now_ms();
    let first = marks(&db);
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].0, "process");
    assert!(before <= first[0].1 && first[0].1 <= after, "{first:?}");
    db.execute("UPDATE learning_marks SET marked_ms=5", [])
        .unwrap();
    set(&mut db, MarkStep::Process, true);
    assert_eq!(marks(&db), [("process".to_owned(), 5)]);
}

#[test]
fn skip_ticks_no_step_and_clearing_it_shows_the_guide_again_with_the_ticks_kept() {
    let mut db = Connection::open_in_memory().unwrap();
    tables(&db);
    set(&mut db, MarkStep::Terminal, true);
    set(&mut db, MarkStep::Skipped, true);
    let skipped = read(&mut db);
    assert!(skipped.skipped);
    assert!(skipped.marked(MarkStep::Skipped));
    assert_eq!(done(&skipped), [StepId::Terminal], "a skip ticks no step");
    set(&mut db, MarkStep::Skipped, false);
    let again = read(&mut db);
    assert!(!again.skipped);
    assert_eq!(done(&again), [StepId::Terminal]);
    set(&mut db, MarkStep::Skipped, false);
    let names: Vec<String> = marks(&db).into_iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        ["terminal"],
        "clearing a mark never set changes nothing"
    );
}

#[test]
fn a_mark_read_carries_no_time() {
    let mut db = Connection::open_in_memory().unwrap();
    tables(&db);
    set(&mut db, MarkStep::Usage, true);
    db.execute("UPDATE learning_marks SET marked_ms=1234567", [])
        .unwrap();
    let wire = serde_json::to_string(&read(&mut db)).unwrap();
    assert!(!wire.contains("1234567"), "{wire}");
}

#[test]
fn a_latch_keeps_every_ticked_step_once_its_rows_go_and_ticks_nothing_else() {
    let mut db = Connection::open_in_memory().unwrap();
    tables(&db);
    db.execute_batch(
        "INSERT INTO projects VALUES ('p1','{}');
         INSERT INTO conversation_entries VALUES ('p1','message','Hi.');
         INSERT INTO team_agents(project,role) VALUES ('p1','sub');
         INSERT INTO questions_log(project,status) VALUES ('p1','pending');",
    )
    .unwrap();
    set(&mut db, MarkStep::Process, true);
    for _ in 0..2 {
        let tx = db.transaction().unwrap();
        latch(&tx).unwrap();
        tx.commit().unwrap();
    }
    db.execute_batch(
        "DELETE FROM projects; DELETE FROM conversation_entries; DELETE FROM team_agents;
         DELETE FROM questions_log;",
    )
    .unwrap();
    let progress = read(&mut db);
    assert_eq!(
        done(&progress),
        [
            StepId::Project,
            StepId::Master,
            StepId::Agents,
            StepId::Process
        ]
    );
    assert!(!progress.skipped);
    let names: Vec<String> = marks(&db).into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["agents", "master", "process", "project"]);
}

#[test]
fn a_failed_latch_keeps_none_of_its_marks_and_leaves_the_transaction_usable() {
    let mut db = Connection::open_in_memory().unwrap();
    tables(&db);
    db.execute_batch(
        "INSERT INTO projects VALUES ('p1','{}');
         INSERT INTO conversation_entries VALUES ('p1','message','Hi.');
         CREATE TRIGGER learning_marks_fail BEFORE INSERT ON learning_marks WHEN NEW.step='master'
         BEGIN SELECT RAISE(ABORT, 'learning_marks_fail'); END;",
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    let _ = latch(&tx);
    tx.execute("DELETE FROM projects", []).unwrap();
    tx.commit().unwrap();
    assert!(
        marks(&db).is_empty(),
        "a failed latch keeps none of its marks"
    );
    assert_eq!(count(&read(&mut db), StepId::Project), 0);
}
