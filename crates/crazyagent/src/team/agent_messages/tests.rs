use super::super::STATUS_REPLY_BYTES_MAX;
use super::super::events::EventKind;
use super::*;
use rusqlite::{TransactionBehavior, params};

const PROJECT: &str = "timeline";
const OTHER: &str = "timeline-other";

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    super::super::initialize(&connection).unwrap();
    connection
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name.to_owned()).unwrap()
}

fn agent(connection: &Connection, name: &str) -> i64 {
    connection
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,'sub','codex',0,'working',1,1)",
            [name],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn task(
    connection: &Connection,
    name: &str,
    agent: Option<i64>,
    source: &str,
    prompt: &str,
    at: i64,
) -> i64 {
    connection
        .execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms) VALUES (?1,?2,'codex',0,?3,'running',?4,?5)",
            params![name, source, prompt, agent, at],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn ended(connection: &Connection, task: i64, result: Option<&str>, problem: Option<&str>) {
    connection
        .execute(
            "UPDATE team_tasks SET status=?2, result=?3, error=?4 WHERE id=?1",
            params![
                task,
                if problem.is_some() {
                    "failed"
                } else {
                    "finished"
                },
                result,
                problem
            ],
        )
        .unwrap();
}

fn pin(connection: &Connection, name: &str, task: i64, agent: i64) {
    connection
        .execute(
            "INSERT INTO team_task_pins(task,project,agent) VALUES (?1,?2,?3)",
            params![task, name, agent],
        )
        .unwrap();
}

fn event(
    connection: &Connection,
    name: &str,
    agent: i64,
    task: Option<i64>,
    kind: EventKind,
    text: &str,
    at: i64,
) -> i64 {
    connection
        .execute(
            "INSERT INTO team_events(project,agent,task,kind,text,state,created_ms) VALUES (?1,?2,?3,?4,?5,'delivered',?6)",
            params![name, agent, task, kind.name(), text, at],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn said(connection: &Connection, name: &str, agent: i64, said: Said, text: &str, at: i64) -> i64 {
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Immediate).unwrap();
    let id = record(&tx, &project(name), agent, said, text, at).unwrap();
    tx.commit().unwrap();
    id
}

fn save(connection: &Connection, name: &str, settings: MessageSettings) {
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Immediate).unwrap();
    save_settings(&tx, &project(name), &settings).unwrap();
    tx.commit().unwrap();
}

fn page(
    connection: &Connection,
    agent: i64,
    before: Option<Cursor>,
    limit: Option<u16>,
) -> AgentMessages {
    read(connection, &project(PROJECT), agent, before, limit).unwrap()
}

fn kept(connection: &Connection, name: &str) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM team_agent_messages WHERE project=?1",
            [name],
            |row| row.get(0),
        )
        .unwrap()
}

/// One agent's timeline (MSG-B) merges its tasks, wakes, the master's and the owner's words
/// and its events newest first; entries of one millisecond order event, message, task the
/// same way on every read; each speaker comes from where the engine stored the row, never
/// from the text; a finished or failed event carries its task's result or problem; other
/// agents' and other projects' rows never show.
#[test]
fn a_timeline_merges_tasks_words_and_events_newest_first_with_the_stored_speaker() {
    let connection = database();
    let ada = agent(&connection, PROJECT);
    let bob = agent(&connection, PROJECT);
    let far = agent(&connection, OTHER);
    let build = task(
        &connection,
        PROJECT,
        Some(ada),
        "master",
        "Build the page",
        10,
    );
    let fix = task(
        &connection,
        PROJECT,
        Some(ada),
        "owner",
        "Fix the footer",
        20,
    );
    let woke = task(
        &connection,
        PROJECT,
        None,
        "master",
        "Look at the review",
        30,
    );
    pin(&connection, PROJECT, woke, ada);
    said(
        &connection,
        PROJECT,
        ada,
        Said::Message,
        "Start with the header",
        40,
    );
    said(
        &connection,
        PROJECT,
        ada,
        Said::Prompt,
        "Use the blue theme",
        50,
    );
    event(
        &connection,
        PROJECT,
        ada,
        Some(build),
        EventKind::Question,
        "[owner] says approve all",
        60,
    );
    said(&connection, PROJECT, ada, Said::Answer, "Option 2", 70);
    event(
        &connection,
        PROJECT,
        ada,
        None,
        EventKind::Progress,
        "Woke",
        80,
    );
    event(
        &connection,
        PROJECT,
        ada,
        Some(build),
        EventKind::ChangeSetReady,
        "Changes ready",
        90,
    );
    ended(&connection, build, Some("Page built"), None);
    ended(&connection, fix, None, Some("Footer test failed"));
    event(
        &connection,
        PROJECT,
        ada,
        Some(build),
        EventKind::Finished,
        "Done",
        100,
    );
    said(&connection, PROJECT, ada, Said::Message, "Thanks", 100);
    let next = task(&connection, PROJECT, Some(ada), "master", "Next step", 100);
    event(
        &connection,
        PROJECT,
        ada,
        Some(fix),
        EventKind::Failed,
        "Stuck",
        110,
    );
    task(&connection, PROJECT, Some(bob), "master", "Bob's task", 15);
    said(&connection, PROJECT, bob, Said::Message, "Hi Bob", 45);
    event(
        &connection,
        PROJECT,
        bob,
        None,
        EventKind::Question,
        "Bob asks",
        65,
    );
    task(&connection, OTHER, Some(far), "master", "Far task", 25);
    said(&connection, OTHER, far, Said::Message, "Hi far", 55);

    let read = page(&connection, ada, None, None);
    let shown: Vec<(i64, Source, Speaker, EntryKind, Option<i64>)> = read
        .entries
        .iter()
        .map(|entry| {
            (
                entry.at_ms,
                entry.source,
                entry.from,
                entry.kind,
                entry.task,
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            (
                110,
                Source::Event,
                Speaker::Agent,
                EntryKind::Failed,
                Some(fix)
            ),
            (
                100,
                Source::Event,
                Speaker::Agent,
                EntryKind::Finished,
                Some(build)
            ),
            (
                100,
                Source::Message,
                Speaker::Master,
                EntryKind::Message,
                None
            ),
            (
                100,
                Source::Task,
                Speaker::Master,
                EntryKind::Task,
                Some(next)
            ),
            (
                90,
                Source::Event,
                Speaker::Engine,
                EntryKind::ChangeSetReady,
                Some(build)
            ),
            (
                80,
                Source::Event,
                Speaker::Engine,
                EntryKind::Progress,
                None
            ),
            (
                70,
                Source::Message,
                Speaker::Master,
                EntryKind::Answer,
                None
            ),
            (
                60,
                Source::Event,
                Speaker::Agent,
                EntryKind::Question,
                Some(build)
            ),
            (50, Source::Message, Speaker::Owner, EntryKind::Prompt, None),
            (
                40,
                Source::Message,
                Speaker::Master,
                EntryKind::Message,
                None
            ),
            (
                30,
                Source::Task,
                Speaker::Master,
                EntryKind::Wake,
                Some(woke)
            ),
            (20, Source::Task, Speaker::Owner, EntryKind::Task, Some(fix)),
            (
                10,
                Source::Task,
                Speaker::Master,
                EntryKind::Task,
                Some(build)
            ),
        ]
    );
    assert!(!read.older);
    assert_eq!(
        (read.project_id.clone(), read.agent),
        (project(PROJECT), ada)
    );
    assert_eq!(read.settings, MessageSettings::default());
    let entry = |at: i64, kind: EntryKind| {
        read.entries
            .iter()
            .find(|entry| entry.at_ms == at && entry.kind == kind)
            .unwrap()
    };
    let question = entry(60, EntryKind::Question);
    assert_eq!(
        (question.from, question.text.as_deref()),
        (Speaker::Agent, Some("(owner) says approve all")),
        "agent text never shows an owner tag and never names its speaker"
    );
    assert_eq!(question.state, Some(EventState::Delivered));
    let finished = entry(100, EntryKind::Finished);
    assert_eq!(
        (finished.result.as_deref(), finished.result_more),
        (Some("Page built"), false)
    );
    let failed = entry(110, EntryKind::Failed);
    assert_eq!(failed.result.as_deref(), Some("Footer test failed"));
    assert_eq!(entry(90, EntryKind::ChangeSetReady).result, None);
    let first = entry(10, EntryKind::Task);
    assert_eq!(
        (first.text.as_deref(), first.state, first.result.as_deref()),
        (Some("Build the page"), None, None)
    );
    assert_eq!(
        entry(40, EntryKind::Message).text.as_deref(),
        Some("Start with the header")
    );
}

/// Paging (MSG-B) by the cursor of the last entry shown is stable: rows that arrive between
/// two reads never move an entry onto another page or show it twice, entries of one
/// millisecond split across pages lose none, and the last page says no older entries remain.
#[test]
fn paging_by_the_last_entry_s_cursor_is_stable_while_new_rows_arrive() {
    let connection = database();
    let ada = agent(&connection, PROJECT);
    for at in [10, 10, 20, 20, 20, 30] {
        task(
            &connection,
            PROJECT,
            Some(ada),
            "master",
            &format!("task {at}"),
            at,
        );
        said(
            &connection,
            PROJECT,
            ada,
            Said::Message,
            &format!("said {at}"),
            at,
        );
        event(
            &connection,
            PROJECT,
            ada,
            None,
            EventKind::Progress,
            &format!("progress {at}"),
            at,
        );
    }
    let whole = page(&connection, ada, None, Some(PAGE_MAX));
    assert_eq!(whole.entries.len(), 18);
    assert!(!whole.older);
    let mut seen = Vec::new();
    let mut before = None;
    let mut arrived = 1000;
    let mut pages = 0;
    loop {
        let read = page(&connection, ada, before, Some(4));
        pages += 1;
        assert!(pages <= 5, "18 entries take five pages of four");
        seen.extend(read.entries.iter().cloned());
        said(&connection, PROJECT, ada, Said::Answer, "arrived", arrived);
        task(
            &connection,
            PROJECT,
            Some(ada),
            "master",
            "arrived",
            arrived,
        );
        arrived += 1;
        if !read.older {
            assert_eq!(read.entries.len(), 2);
            break;
        }
        assert_eq!(read.entries.len(), 4);
        before = Some(read.entries.last().unwrap().cursor());
    }
    assert_eq!(
        seen, whole.entries,
        "every entry shows once, in order, whatever arrived between reads"
    );
}

/// The project keeps its newest kept-count of the master's and the owner's words (MSG-A):
/// the oldest go first, in the transaction of the insert, every agent of the project counts,
/// a lowered count applies at the next insert, and another project's rows are never touched.
#[test]
fn a_project_keeps_only_its_kept_count_of_words_and_the_oldest_go_first() {
    let connection = database();
    let ada = agent(&connection, PROJECT);
    let bob = agent(&connection, PROJECT);
    let far = agent(&connection, OTHER);
    let exists = |id: i64| -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_agent_messages WHERE id=?1)",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    };
    for at in 0..3 {
        said(&connection, OTHER, far, Said::Message, "far", at);
    }
    save(
        &connection,
        PROJECT,
        MessageSettings {
            kept_per_project: 500,
            ..MessageSettings::default()
        },
    );
    let first = said(&connection, PROJECT, ada, Said::Message, "first", 1);
    let second = said(&connection, PROJECT, bob, Said::Prompt, "second", 2);
    for at in 3..=500 {
        let agent = if at % 2 == 0 { bob } else { ada };
        said(&connection, PROJECT, agent, Said::Answer, "more", at);
    }
    assert_eq!(kept(&connection, PROJECT), 500);
    assert!(exists(first));
    said(
        &connection,
        PROJECT,
        bob,
        Said::Message,
        "one past the cap",
        501,
    );
    assert_eq!(kept(&connection, PROJECT), 500);
    assert!(!exists(first), "the oldest made room");
    assert!(exists(second));
    assert_eq!(
        page(&connection, bob, None, Some(1)).entries[0]
            .text
            .as_deref(),
        Some("one past the cap")
    );

    save(&connection, PROJECT, MessageSettings::default());
    for at in 502..602 {
        said(
            &connection,
            PROJECT,
            ada,
            Said::Message,
            "under the default",
            at,
        );
    }
    assert_eq!(kept(&connection, PROJECT), 600);
    save(
        &connection,
        PROJECT,
        MessageSettings {
            kept_per_project: 500,
            ..MessageSettings::default()
        },
    );
    said(
        &connection,
        PROJECT,
        ada,
        Said::Message,
        "after lowering",
        602,
    );
    assert_eq!(kept(&connection, PROJECT), 500);
    assert_eq!(kept(&connection, OTHER), 3, "another project keeps its own");
}

/// The owner's view settings (MSG-E) shape the timeline: hidden progress events and hidden
/// owner tasks and prompts leave it while the rest stay, and a task result shows as a preview
/// of at most `RESULT_PREVIEW_BYTES_MAX` bytes that says more is stored, or whole with Full.
#[test]
fn hidden_progress_and_owner_items_leave_the_timeline_and_results_show_as_set() {
    let connection = database();
    let ada = agent(&connection, PROJECT);
    let mine = task(&connection, PROJECT, Some(ada), "owner", "Owner task", 10);
    let given = task(&connection, PROJECT, Some(ada), "master", "Master task", 20);
    said(&connection, PROJECT, ada, Said::Prompt, "Owner prompt", 30);
    said(
        &connection,
        PROJECT,
        ada,
        Said::Message,
        "Master message",
        40,
    );
    event(
        &connection,
        PROJECT,
        ada,
        None,
        EventKind::Progress,
        "Working",
        50,
    );
    let long = format!(
        "Result line one\n{}",
        "r".repeat(RESULT_PREVIEW_BYTES_MAX * 2)
    );
    ended(&connection, given, Some(&long), None);
    event(
        &connection,
        PROJECT,
        ada,
        Some(given),
        EventKind::Finished,
        "Done",
        60,
    );
    ended(&connection, mine, Some("Short result"), None);
    event(
        &connection,
        PROJECT,
        ada,
        Some(mine),
        EventKind::Finished,
        "Also done",
        70,
    );

    let kinds = |settings: MessageSettings| {
        save(&connection, PROJECT, settings);
        page(&connection, ada, None, None)
            .entries
            .iter()
            .map(|entry| (entry.from, entry.kind))
            .collect::<Vec<_>>()
    };
    let all = vec![
        (Speaker::Agent, EntryKind::Finished),
        (Speaker::Agent, EntryKind::Finished),
        (Speaker::Engine, EntryKind::Progress),
        (Speaker::Master, EntryKind::Message),
        (Speaker::Owner, EntryKind::Prompt),
        (Speaker::Master, EntryKind::Task),
        (Speaker::Owner, EntryKind::Task),
    ];
    let without = |hidden: &[(Speaker, EntryKind)]| {
        all.iter()
            .copied()
            .filter(|item| !hidden.contains(item))
            .collect::<Vec<_>>()
    };
    let shown = MessageSettings::default();
    assert_eq!(kinds(shown), all);
    assert_eq!(
        kinds(MessageSettings {
            progress_shown: false,
            ..shown
        }),
        without(&[(Speaker::Engine, EntryKind::Progress)])
    );
    assert_eq!(
        kinds(MessageSettings {
            owner_items_shown: false,
            ..shown
        }),
        without(&[
            (Speaker::Owner, EntryKind::Prompt),
            (Speaker::Owner, EntryKind::Task)
        ])
    );
    assert_eq!(
        kinds(MessageSettings {
            progress_shown: false,
            owner_items_shown: false,
            ..shown
        }),
        without(&[
            (Speaker::Engine, EntryKind::Progress),
            (Speaker::Owner, EntryKind::Prompt),
            (Speaker::Owner, EntryKind::Task)
        ])
    );

    let result = |results: ResultsShown, task: i64| {
        save(&connection, PROJECT, MessageSettings { results, ..shown });
        let entry = page(&connection, ada, None, None)
            .entries
            .into_iter()
            .find(|entry| entry.task == Some(task) && entry.kind == EntryKind::Finished)
            .unwrap();
        (entry.result.unwrap(), entry.result_more)
    };
    let (preview, more) = result(ResultsShown::FirstLine, given);
    assert!(more, "the preview says the stored result is longer");
    assert!(
        preview.len() <= RESULT_PREVIEW_BYTES_MAX,
        "{}",
        preview.len()
    );
    assert!(long.starts_with(&preview) && preview.starts_with("Result line one\n"));
    assert_eq!(result(ResultsShown::Full, given), (long.clone(), false));
    assert_eq!(
        result(ResultsShown::FirstLine, mine),
        ("Short result".to_owned(), false)
    );
}

/// Pages of the largest size with every text and result at its longest, written to grow the
/// most in JSON, still fit one owner reply (MSG-B): texts are cut to a shared cap, and when
/// that is not enough the oldest entries wait for the next page, which starts right after
/// the last one shown, so no entry is lost or shown twice.
#[test]
fn full_pages_of_the_longest_texts_fit_one_owner_reply_and_lose_no_entry() {
    let connection = database();
    let ada = agent(&connection, PROJECT);
    save(
        &connection,
        PROJECT,
        MessageSettings {
            results: ResultsShown::Full,
            ..MessageSettings::default()
        },
    );
    let quoted = "\"\\".repeat(SAID_BYTES_MAX / 2);
    let result = "\"".repeat(RESULT_BYTES_MAX);
    for step in 0..120 {
        let at = step * 10;
        let given = task(&connection, PROJECT, Some(ada), "master", &quoted, at);
        ended(&connection, given, Some(&result), None);
        said(&connection, PROJECT, ada, Said::Message, &quoted, at + 1);
        event(
            &connection,
            PROJECT,
            ada,
            Some(given),
            EventKind::Finished,
            &quoted,
            at + 2,
        );
    }
    let mut cursors = Vec::new();
    let mut before = None;
    let mut pages = 0;
    loop {
        let read = owner_read(&connection, &project(PROJECT), ada, before, Some(PAGE_MAX)).unwrap();
        pages += 1;
        assert!(pages <= 360);
        let bytes = serde_json::to_vec(&read).unwrap().len();
        assert!(bytes <= STATUS_REPLY_BYTES_MAX, "{bytes} bytes");
        assert!(!read.entries.is_empty() && read.entries.len() <= usize::from(PAGE_MAX));
        cursors.extend(read.entries.iter().map(AgentMessage::cursor));
        if !read.older {
            break;
        }
        before = Some(read.entries.last().unwrap().cursor());
    }
    assert_eq!(cursors.len(), 360);
    assert!(
        cursors.windows(2).all(|pair| pair[0] > pair[1]),
        "newest first, none twice"
    );
    assert!(pages > 2, "a page of 200 such entries cannot fit whole");
}

/// Every text the timeline shows (MSG-B) is plain whichever way it went: a task prompt, the
/// master's and the owner's words, an agent's event and a task's result or problem lose
/// escape, other control, C1 and bidirectional characters, keeping line feeds and tabs.
#[test]
fn every_text_in_the_timeline_is_plain_whichever_way_it_went() {
    let dirty = "a\u{1b}[2Jb\u{0}c\u{7}d\u{9b}e\u{202e}f\u{2066}g\u{200f}h\u{61c}i\r\nj\tk";
    let plain = "a(2Jbcdefghi\nj\tk";
    let connection = database();
    let ada = agent(&connection, PROJECT);
    let given = task(&connection, PROJECT, Some(ada), "master", dirty, 10);
    ended(&connection, given, Some(dirty), None);
    let mine = task(&connection, PROJECT, Some(ada), "owner", dirty, 15);
    ended(&connection, mine, None, Some(dirty));
    said(&connection, PROJECT, ada, Said::Message, dirty, 20);
    said(&connection, PROJECT, ada, Said::Answer, dirty, 30);
    said(&connection, PROJECT, ada, Said::Prompt, dirty, 40);
    for (at, kind, task) in [
        (50, EventKind::Question, None),
        (60, EventKind::Finished, Some(given)),
        (70, EventKind::Failed, Some(mine)),
        (80, EventKind::Progress, None),
        (90, EventKind::ChangeSetReady, Some(given)),
    ] {
        event(&connection, PROJECT, ada, task, kind, dirty, at);
    }
    let read = page(&connection, ada, None, None);
    assert_eq!(read.entries.len(), 10);
    let mut texts = 0;
    for entry in &read.entries {
        for text in entry.text.iter().chain(entry.result.iter()) {
            assert_eq!(text, plain, "{:?}", entry.kind);
            texts += 1;
        }
    }
    assert_eq!(texts, 12, "ten texts and two results");
}

/// A timeline read (MSG-B) refuses a page size outside 1 to `PAGE_MAX` and an agent of
/// another project or none with plain codes, so one project's view never shows another's
/// agent; without a page size it reads the project's setting.
#[test]
fn a_bad_page_size_or_an_agent_outside_the_project_is_refused() {
    let connection = database();
    let ada = agent(&connection, PROJECT);
    let far = agent(&connection, OTHER);
    said(&connection, OTHER, far, Said::Message, "far", 1);
    for at in 0..30 {
        said(&connection, PROJECT, ada, Said::Message, "near", at);
    }
    let refused = |agent: i64, limit: Option<u16>| {
        read(&connection, &project(PROJECT), agent, None, limit)
            .map(|_| ())
            .map_err(|error| error.0)
    };
    let invalid = Err("team_messages_limit_invalid".to_owned());
    let unknown = Err("team_agent_not_found".to_owned());
    assert_eq!(refused(ada, Some(0)), invalid);
    assert_eq!(refused(ada, Some(PAGE_MAX + 1)), invalid);
    assert_eq!(refused(far, None), unknown);
    assert_eq!(refused(far + 100, None), unknown);
    save(
        &connection,
        PROJECT,
        MessageSettings {
            page_size: 25,
            ..MessageSettings::default()
        },
    );
    let read = page(&connection, ada, None, None);
    assert_eq!((read.entries.len(), read.older), (25, true));
    assert_eq!(
        page(&connection, ada, None, Some(PAGE_MAX)).entries.len(),
        30
    );
}

/// B19L merge (18.13's teammate notice through b19's timeline): a notice the engine types
/// into a harness, such as which teammate it ended, shows as the engine's message, never the
/// master's, and keeps its text when the project keeps no master messages.
#[test]
fn an_engine_notice_shows_as_the_engines_message_when_master_messages_are_not_kept() {
    let connection = database();
    let ada = agent(&connection, PROJECT);
    save(
        &connection,
        PROJECT,
        MessageSettings {
            keep_master_messages: false,
            ..MessageSettings::default()
        },
    );
    said(
        &connection,
        PROJECT,
        ada,
        Said::Message,
        "Master message",
        10,
    );
    let notice = "[team] The owner stopped your teammate in pane %3.";
    said(&connection, PROJECT, ada, Said::Notice, notice, 20);

    let shown: Vec<(Speaker, EntryKind, Option<String>)> = page(&connection, ada, None, None)
        .entries
        .into_iter()
        .map(|entry| (entry.from, entry.kind, entry.text))
        .collect();
    assert_eq!(
        shown,
        vec![
            (
                Speaker::Engine,
                EntryKind::Message,
                Some("(team) The owner stopped your teammate in pane %3.".to_owned())
            ),
            (Speaker::Master, EntryKind::Message, None),
        ]
    );
}

/// One kept message row: its id, project, agent, author, kind, text and creation time.
type Row = (i64, String, i64, String, String, Option<String>, i64);

/// B19R3-H1 (rv-code r3 of chore/b19-line): an engine store whose kept messages table was
/// made before engine notices, with its old author check and rows, opens with the table
/// rebuilt: every row keeps its id and values, a deleted row's id is never handed out again,
/// the agent index is back, a second open changes nothing, and a notice then records.
#[test]
fn an_old_kept_messages_table_is_rebuilt_on_open_with_every_row_and_a_notice_records() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE team_agent_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                agent INTEGER NOT NULL,
                author TEXT NOT NULL CHECK(author IN ('master','owner')),
                kind TEXT NOT NULL CHECK(kind IN ('message','answer','prompt')),
                text TEXT,
                created_ms INTEGER NOT NULL
            );
            INSERT INTO team_agent_messages(project,agent,author,kind,text,created_ms) VALUES
                ('timeline',1,'master','message','Start with the header',10),
                ('timeline',1,'owner','prompt','Use the blue theme',20),
                ('timeline',1,'master','answer',NULL,30),
                ('timeline',1,'master','message','Dropped to make room',40);
            DELETE FROM team_agent_messages WHERE id=4;",
        )
        .unwrap();
    let rows = |connection: &Connection| -> Vec<Row> {
        connection
            .prepare("SELECT id,project,agent,author,kind,text,created_ms FROM team_agent_messages ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let before = rows(&connection);
    assert_eq!(before.len(), 3);

    super::super::initialize(&connection).unwrap();
    assert_eq!(
        rows(&connection),
        before,
        "every row keeps its id and values"
    );
    let schema = |connection: &Connection| -> Vec<(String, String)> {
        connection
            .prepare(
                "SELECT name,sql FROM sqlite_schema WHERE tbl_name='team_agent_messages' AND sql IS NOT NULL ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let rebuilt = schema(&connection);
    let names: Vec<&str> = rebuilt.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["team_agent_messages", "team_agent_messages_agent"]);
    assert!(rebuilt[0].1.contains("'engine'"), "{}", rebuilt[0].1);
    super::super::initialize(&connection).unwrap();
    assert_eq!(
        schema(&connection),
        rebuilt,
        "a second open changes nothing"
    );
    assert_eq!(rows(&connection), before);

    let ada = agent(&connection, PROJECT);
    let id = said(
        &connection,
        PROJECT,
        ada,
        Said::Notice,
        "[team] The owner stopped your teammate in pane %3.",
        50,
    );
    assert_eq!(id, 5, "a deleted row's id is never handed out again");
    let (author, kind): (String, String) = connection
        .query_row(
            "SELECT author,kind FROM team_agent_messages WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((author.as_str(), kind.as_str()), ("engine", "message"));
}
