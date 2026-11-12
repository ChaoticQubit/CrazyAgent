use super::*;
use rusqlite::{TransactionBehavior, params};

const PROJECT: &str = "timeline-scale";
const AGENTS: i64 = 15;
const TASKS: i64 = 2000;
const EVENTS: i64 = 20_000;
const KEPT: u32 = 5000;
const SOURCE: &str = include_str!("../agent_messages.rs");

const _: () = assert!(KEPT == KEPT_CHOICES[KEPT_CHOICES.len() - 1]);

fn project() -> ProjectId {
    ProjectId::new(PROJECT.to_owned()).unwrap()
}

/// One project at the timeline's largest scale: 15 sub-agents share 2000 finished tasks with
/// 4000-byte prompts and 16 KiB results, 20000 events of every kind with 1000-byte texts, a
/// finished or failed one on a task of its own agent, and the 5000 words of the largest kept
/// choice, 4096 bytes each, kept through the engine's own recorder. Results show whole.
/// Returns the connection.
fn scaled() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    super::super::initialize(&connection).unwrap();
    for _ in 0..AGENTS {
        connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,'sub','codex',0,'working',1,1)",
                [PROJECT],
            )
            .unwrap();
    }
    let agents: Vec<i64> = connection
        .prepare("SELECT id FROM team_agents WHERE project=?1 ORDER BY id")
        .unwrap()
        .query_map([PROJECT], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(agents, (1..=AGENTS).collect::<Vec<_>>());
    connection
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i+1<?2)
             INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,result,created_ms,finished_ms)
             SELECT ?1, CASE i%3 WHEN 0 THEN 'owner' ELSE 'master' END, 'codex', 0,
                    substr(hex(zeroblob(2000)),1,4000), 'done', 1+i%?3, hex(zeroblob(8192)), i*10, i*10
             FROM n",
            params![PROJECT, TASKS, AGENTS],
        )
        .unwrap();
    connection
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i+1<?2)
             INSERT INTO team_events(project,agent,task,kind,text,state,created_ms)
             SELECT ?1, 1+i%?3,
                    CASE WHEN i%5>=3 THEN 1+((i/?3)%(?4/?3))*?3+i%?3 END,
                    CASE i%5 WHEN 0 THEN 'question' WHEN 1 THEN 'progress' WHEN 2 THEN 'change_set_ready'
                             WHEN 3 THEN 'finished' ELSE 'failed' END,
                    substr(hex(zeroblob(500)),1,1000), 'delivered', i*2+1
             FROM n",
            params![PROJECT, EVENTS, AGENTS, TASKS],
        )
        .unwrap();
    let tx = Transaction::new_unchecked(&connection, TransactionBehavior::Immediate).unwrap();
    save_settings(
        &tx,
        &project(),
        &MessageSettings {
            kept_per_project: KEPT,
            results: ResultsShown::Full,
            ..MessageSettings::default()
        },
    )
    .unwrap();
    let word = "w".repeat(SAID_BYTES_MAX);
    for n in 0..i64::from(KEPT) {
        let said = [Said::Message, Said::Answer, Said::Prompt][usize::try_from(n % 3).unwrap()];
        record(&tx, &project(), 1 + n % AGENTS, said, &word, n * 4 + 3).unwrap();
    }
    tx.commit().unwrap();
    let count = |sql: &str| -> i64 { connection.query_row(sql, [], |row| row.get(0)).unwrap() };
    assert_eq!(count("SELECT COUNT(*) FROM team_tasks"), TASKS);
    assert_eq!(count("SELECT COUNT(*) FROM team_events"), EVENTS);
    assert_eq!(
        count("SELECT COUNT(*) FROM team_agent_messages"),
        i64::from(KEPT)
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM team_events e JOIN team_tasks t ON t.id=e.task WHERE e.agent<>t.agent"
        ),
        0
    );
    connection
}

/// The page query `read` prepares for one store, as the engine's source holds it, so the plan
/// checked is the one the engine runs. Takes the table it reads from. Returns the SQL.
fn page_query(table: &str) -> String {
    let from = format!("FROM {table}");
    let found: Vec<&str> = SOURCE
        .split(".prepare(")
        .skip(1)
        .filter_map(|rest| {
            let start = rest.find('"')? + 1;
            let end = start + rest[start..].find('"')?;
            Some(&rest[start..end])
        })
        .filter(|sql| {
            sql.split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| pair.join(" ") == from)
        })
        .collect();
    assert_eq!(found.len(), 1, "one page query reads {table}: {found:?}");
    found[0].to_owned()
}

/// What SQLite does for a page query of the first agent's newest page of the largest size.
/// Takes the connection and the SQL. Returns each step of the query plan.
fn plan(connection: &Connection, sql: &str) -> Vec<String> {
    connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(
            params![
                PROJECT,
                1,
                true,
                i64::MAX,
                i64::MAX,
                i64::from(PAGE_MAX) + 1
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A page query at scale seeks only one agent's rows through its index and scans no table.
/// Takes the table, the name the plan gives it and the index.
fn seeks_one_agent(table: &str, alias: &str, index: &str) {
    let connection = scaled();
    let steps = plan(&connection, &page_query(table));
    let seek = [
        format!("SEARCH {alias} USING INDEX {index} (project=? AND agent=?"),
        format!("SEARCH {alias} USING COVERING INDEX {index} (project=? AND agent=?"),
    ];
    assert!(
        steps
            .iter()
            .any(|step| seek.iter().any(|prefix| step.starts_with(prefix))),
        "the {table} page query does not seek one agent's rows by {index}: {steps:?}"
    );
    assert!(
        !steps.iter().any(|step| step.starts_with("SCAN ")),
        "the {table} page query scans a whole table: {steps:?}"
    );
}

/// MSG-SEC-1: the tasks page query seeks one agent's tasks by `team_tasks_agent` and scans
/// no table, at 2000 tasks across 15 agents.
#[test]
fn the_tasks_page_query_seeks_one_agent_s_tasks_by_its_index() {
    seeks_one_agent("team_tasks", "t", "team_tasks_agent");
}

/// MSG-SEC-1: the kept words page query seeks one agent's words by
/// `team_agent_messages_agent` and scans no table, at 5000 kept words.
#[test]
fn the_kept_words_page_query_seeks_one_agent_s_words_by_its_index() {
    seeks_one_agent(
        "team_agent_messages",
        "team_agent_messages",
        "team_agent_messages_agent",
    );
}

/// MSG-SEC-1: the events page query seeks one agent's events by `team_events_agent` and scans
/// no table, at 20000 events.
#[test]
fn the_events_page_query_seeks_one_agent_s_events_by_its_index() {
    seeks_one_agent("team_events", "e", "team_events_agent");
}

/// MSG-SEC-1: one owner page of the largest size at scale, every text at its longest and
/// results whole, fits one owner reply under `STATUS_REPLY_BYTES_MAX`, newest first, and
/// says older entries remain; the next page starts after its last entry.
#[test]
fn one_owner_page_of_the_largest_size_at_scale_fits_one_owner_reply() {
    let connection = scaled();
    let page = owner_read(&connection, &project(), 1, None, Some(PAGE_MAX)).unwrap();
    let encoded = serde_json::to_vec(&page).unwrap().len();
    assert!(
        encoded <= STATUS_REPLY_BYTES_MAX,
        "{encoded} bytes for {} entries",
        page.entries.len()
    );
    assert!(super::super::encoded_len(&page).unwrap() <= STATUS_REPLY_BYTES_MAX);
    assert!(!page.entries.is_empty());
    assert!(page.entries.len() <= usize::from(PAGE_MAX));
    assert!(page.older, "the agent has more than one page");
    assert!(
        page.entries
            .windows(2)
            .all(|pair| pair[0].cursor() > pair[1].cursor()),
        "newest first, each entry once"
    );
    let last = page.entries.last().unwrap().cursor();
    let next = owner_read(&connection, &project(), 1, Some(last), Some(PAGE_MAX)).unwrap();
    assert!(!next.entries.is_empty());
    assert!(next.entries.iter().all(|entry| entry.cursor() < last));
    assert!(serde_json::to_vec(&next).unwrap().len() <= STATUS_REPLY_BYTES_MAX);
}
