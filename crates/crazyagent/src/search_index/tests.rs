use super::*;
use rusqlite::Connection;

const NOW: i64 = 1_762_950_000_000;

fn project(id: &str) -> ProjectId {
    ProjectId::new(id).unwrap()
}

fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE projects(id TEXT PRIMARY KEY); INSERT INTO projects(id) VALUES ('alpha'),('bravo');")
        .unwrap();
    initialize(&db).unwrap();
    db
}

fn texts(db: &Connection, ids: &[i64]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            db.query_row("SELECT text FROM search_chunks WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap()
        })
        .collect()
}

#[test]
fn the_schema_is_made_once_and_full_text_is_found_on_this_sqlite() {
    let db = store();
    initialize(&db).unwrap();
    assert!(
        full_text(&db),
        "the system SQLite of a supported Mac has FTS5"
    );
    for table in [
        "search_chunks",
        "search_chunks_fts",
        "search_sources",
        "search_projects",
    ] {
        let found: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(found, "{table}");
    }
    assert_eq!(Source::Session.as_str(), "session");
    assert_eq!(Source::Memory.as_str(), "memory");
}

#[test]
fn a_chunk_is_found_by_its_words_only_in_its_own_project_and_source() {
    let db = store();
    let (alpha, bravo) = (project("alpha"), project("bravo"));
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "team",
        &[(0, "the deploy script failed on staging")],
        NOW,
    )
    .unwrap();
    append(
        &db,
        Some(&bravo),
        Source::Session,
        "team",
        &[(0, "the deploy script failed on bravo")],
        NOW,
    )
    .unwrap();
    append(
        &db,
        Some(&alpha),
        Source::Memory,
        "memory-1",
        &[(0, "deploy only on Fridays")],
        NOW,
    )
    .unwrap();
    let found = best(&db, &alpha, Source::Session, "deploy failed").unwrap();
    assert_eq!(texts(&db, &found), ["the deploy script failed on staging"]);
    let memories = best(&db, &alpha, Source::Memory, "deploy").unwrap();
    assert_eq!(texts(&db, &memories), ["deploy only on Fridays"]);
    assert!(
        best(&db, &alpha, Source::Session, "fridays")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        texts(&db, &best(&db, &alpha, Source::Session, "depl").unwrap()).len(),
        1,
        "a word also finds what it starts"
    );
}

#[test]
fn an_app_wide_memory_is_found_from_every_project() {
    let db = store();
    append(
        &db,
        None,
        Source::Memory,
        "memory-app",
        &[(0, "always run cargo fmt")],
        NOW,
    )
    .unwrap();
    for id in ["alpha", "bravo"] {
        assert_eq!(
            best(&db, &project(id), Source::Memory, "fmt")
                .unwrap()
                .len(),
            1,
            "{id}"
        );
    }
}

#[test]
fn words_with_full_text_syntax_are_searched_as_plain_words() {
    let db = store();
    let alpha = project("alpha");
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "team",
        &[(
            0,
            "NEAR the AND gate, col: x * y - z \"quoted\" src/main.rs",
        )],
        NOW,
    )
    .unwrap();
    for query in [
        "AND",
        "NEAR(",
        "col:x",
        "*",
        "\"",
        "-z",
        "a\"b",
        "OR NOT",
        "^start",
        "{x y}",
        "main.rs",
        "\"quoted\"",
    ] {
        assert!(best(&db, &alpha, Source::Session, query).is_ok(), "{query}");
    }
    assert_eq!(
        best(&db, &alpha, Source::Session, "main.rs").unwrap().len(),
        1
    );
    assert_eq!(
        best(&db, &alpha, Source::Session, "NEAR(").unwrap().len(),
        1
    );
    for nothing in ["", "   ", "...", "* - ^"] {
        assert!(
            best(&db, &alpha, Source::Session, nothing)
                .unwrap()
                .is_empty(),
            "{nothing:?}"
        );
    }
    assert_eq!(match_expression("a\"b"), Some("\"a\"\"b\"*".to_owned()));
    assert_eq!(
        match_expression("one ... two"),
        Some("\"one\"* \"two\"*".to_owned())
    );
    assert_eq!(match_expression("--"), None);
}

#[test]
fn a_query_keeps_at_most_eight_words_of_at_most_64_bytes() {
    let long = "x".repeat(65);
    let query = format!("a b c d e f g h i j {long}");
    let expression = match_expression(&query).unwrap();
    assert_eq!(expression.matches('*').count(), 8);
    assert!(!expression.contains(&long));
}

#[test]
fn the_closer_match_ranks_first() {
    let db = store();
    let alpha = project("alpha");
    let padding = "filler words about nothing in particular ".repeat(20);
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "agent-1/a",
        &[(0, &format!("timeout {padding}"))],
        NOW,
    )
    .unwrap();
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "agent-1/b",
        &[(0, "timeout timeout timeout in the runner")],
        NOW,
    )
    .unwrap();
    let found = best(&db, &alpha, Source::Session, "timeout").unwrap();
    assert_eq!(
        texts(&db, &found)[0],
        "timeout timeout timeout in the runner"
    );
}

#[test]
fn at_most_fifty_chunks_come_back() {
    let db = store();
    let alpha = project("alpha");
    let many: Vec<(i64, String)> = (0..60)
        .map(|at| (at, format!("needle number {at}")))
        .collect();
    let chunks: Vec<(i64, &str)> = many.iter().map(|(at, text)| (*at, text.as_str())).collect();
    assert_eq!(
        append(&db, Some(&alpha), Source::Session, "team", &chunks, NOW).unwrap(),
        60
    );
    assert_eq!(
        best(&db, &alpha, Source::Session, "needle").unwrap().len(),
        BEST_MAX
    );
    assert_eq!(BEST_MAX, 50);
}

#[test]
fn a_secret_never_reaches_the_index() {
    let db = store();
    let alpha = project("alpha");
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "team",
        &[(
            0,
            "set OPENAI_API_KEY=sk-live-4f9a8b7c6d5e then deploy\nnext line stays",
        )],
        NOW,
    )
    .unwrap();
    let stored: String = db
        .query_row("SELECT text FROM search_chunks", [], |row| row.get(0))
        .unwrap();
    assert!(!stored.contains("sk-live-4f9a8b7c6d5e"), "{stored}");
    assert!(stored.contains("next line stays"));
    assert!(
        best(&db, &alpha, Source::Session, "sk-live-4f9a8b7c6d5e")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn text_is_cut_to_2000_bytes_on_a_character_after_redaction() {
    let db = store();
    let alpha = project("alpha");
    let long = format!("{}{}", "a".repeat(1999), "é tail");
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "team",
        &[(0, &long), (1, "  \n ")],
        NOW,
    )
    .unwrap();
    let stored: Vec<String> = db
        .prepare("SELECT text FROM search_chunks ORDER BY ordinal")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(stored, ["a".repeat(1999)], "a blank message stores nothing");
    assert!(stored[0].len() <= CHUNK_BYTES_MAX);
}

#[test]
fn a_chunk_is_stored_once_per_message() {
    let db = store();
    let alpha = project("alpha");
    assert_eq!(
        append(
            &db,
            Some(&alpha),
            Source::Session,
            "team",
            &[(0, "first")],
            NOW
        )
        .unwrap(),
        1
    );
    assert_eq!(
        append(
            &db,
            Some(&alpha),
            Source::Session,
            "team",
            &[(0, "first again"), (1, "second")],
            NOW
        )
        .unwrap(),
        1
    );
    assert_eq!(
        best(&db, &alpha, Source::Session, "first").unwrap().len(),
        1
    );
}

#[test]
fn a_chunk_for_a_project_that_is_gone_is_never_stored() {
    let db = store();
    let gone = project("charlie");
    assert_eq!(
        append(
            &db,
            Some(&gone),
            Source::Session,
            "team",
            &[(0, "orphan text")],
            NOW
        )
        .unwrap(),
        0
    );
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM search_chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn forgetting_a_project_or_a_conversation_takes_its_chunks_out_of_search() {
    let db = store();
    let (alpha, bravo) = (project("alpha"), project("bravo"));
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "session/one",
        &[(0, "alpha one words")],
        NOW,
    )
    .unwrap();
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "session/two",
        &[(0, "alpha two words")],
        NOW,
    )
    .unwrap();
    append(
        &db,
        Some(&bravo),
        Source::Session,
        "session/one",
        &[(0, "bravo one words")],
        NOW,
    )
    .unwrap();
    db.execute("INSERT INTO search_sources(project,key,checked_ms) VALUES ('alpha','session/one',1),('alpha','session/two',1)", []).unwrap();
    db.execute(
        "INSERT INTO search_projects(project,caught_up_ms) VALUES ('alpha',1)",
        [],
    )
    .unwrap();
    forget_key(&db, &alpha, "session/one").unwrap();
    assert_eq!(
        texts(&db, &best(&db, &alpha, Source::Session, "words").unwrap()),
        ["alpha two words"]
    );
    assert_eq!(
        best(&db, &bravo, Source::Session, "words").unwrap().len(),
        1,
        "another project's same key stays"
    );
    forget_project(&db, &alpha).unwrap();
    assert!(
        best(&db, &alpha, Source::Session, "words")
            .unwrap()
            .is_empty()
    );
    for table in ["search_chunks", "search_sources", "search_projects"] {
        let left: i64 = db
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE project='alpha'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "{table}");
    }
    let indexed: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM search_chunks_fts WHERE search_chunks_fts MATCH 'alpha'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexed, 0, "the full-text index forgets them too");
}

#[test]
fn without_full_text_the_index_is_emptied_and_not_used_until_it_is_back() {
    let db = store();
    let alpha = project("alpha");
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "team",
        &[(0, "kept before")],
        NOW,
    )
    .unwrap();
    prepare(&db, false).unwrap();
    assert!(!full_text(&db));
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM search_chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    forget_project(&db, &alpha).unwrap();
    prepare(&db, true).unwrap();
    assert!(full_text(&db));
    assert!(
        best(&db, &alpha, Source::Session, "kept")
            .unwrap()
            .is_empty(),
        "a rebuilt index holds nothing stale"
    );
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "team",
        &[(0, "kept after")],
        NOW,
    )
    .unwrap();
    assert_eq!(best(&db, &alpha, Source::Session, "kept").unwrap().len(), 1);
}

#[test]
fn deleting_a_master_session_forgets_its_turns_in_every_project() {
    let db = store();
    let alpha = project("alpha");
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "session/s-1",
        &[(0, "owner asked about caching")],
        NOW,
    )
    .unwrap();
    append(
        &db,
        Some(&alpha),
        Source::Session,
        "session/s-2",
        &[(0, "owner asked about caching too")],
        NOW,
    )
    .unwrap();
    db.execute(
        "INSERT INTO search_sources(project,key,checked_ms) VALUES ('alpha','session/s-1',1)",
        [],
    )
    .unwrap();
    forget_session(&db, "s-1").unwrap();
    assert_eq!(
        texts(&db, &best(&db, &alpha, Source::Session, "caching").unwrap()),
        ["owner asked about caching too"]
    );
    let left: i64 = db
        .query_row("SELECT COUNT(*) FROM search_sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn forgetting_works_on_a_database_that_never_made_the_index() {
    let db = Connection::open_in_memory().unwrap();
    forget_session(&db, "s-1").unwrap();
    forget_project(&db, &project("alpha")).unwrap();
}
