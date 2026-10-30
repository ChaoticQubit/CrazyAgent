use super::*;
use serde_json::json;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};

const PROJECT: &str = "history-project";

struct Fixture {
    _root: tempfile::TempDir,
    agents: PathBuf,
    db: Connection,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-history-")
            .tempdir()
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let db = Connection::open(base.join("engine.db")).unwrap();
        crate::team::initialize(&db).unwrap();
        db.execute_batch(
            "CREATE TABLE master_sessions (id TEXT PRIMARY KEY, project TEXT NOT NULL,
                 created INTEGER NOT NULL UNIQUE, version INTEGER NOT NULL, phase TEXT NOT NULL,
                 body TEXT NOT NULL);
             CREATE TABLE master_turns (session TEXT NOT NULL, ordinal INTEGER NOT NULL,
                 id TEXT NOT NULL, input INTEGER NOT NULL UNIQUE, phase TEXT NOT NULL,
                 body TEXT NOT NULL, PRIMARY KEY(session,ordinal));
             CREATE TABLE conversation_entries (event_cursor INTEGER PRIMARY KEY,
                 project TEXT NOT NULL, kind TEXT NOT NULL, text TEXT);
             CREATE TABLE session_marks (session TEXT PRIMARY KEY, active_ms INTEGER NOT NULL);",
        )
        .unwrap();
        let agents = base.join("agents");
        std::fs::create_dir(&agents).unwrap();
        Self {
            _root: root,
            agents,
            db,
        }
    }

    fn agent(&self, harness: &str) -> (i64, PathBuf) {
        self.db
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
                 VALUES (?1,'sub',?2,1,'idle',1,1)",
                params![PROJECT, harness],
            )
            .unwrap();
        let id = self.db.last_insert_rowid();
        let home = self.agents.join(format!("agent-{id}")).join("home");
        std::fs::create_dir_all(&home).unwrap();
        (id, home)
    }

    fn turn(&self, session: &str, ordinal: i64, owner: &str, reply: &str, active_ms: i64) {
        self.db
            .execute(
                "INSERT OR IGNORE INTO master_sessions(id,project,created,version,phase,body)
                 VALUES (?1,?2,?3,?3,'parked','{}')",
                params![session, PROJECT, active_ms],
            )
            .unwrap();
        let input = active_ms + ordinal;
        self.db
            .execute(
                "INSERT INTO conversation_entries(event_cursor,project,kind,text) VALUES (?1,?2,'message',?3)",
                params![input, PROJECT, owner],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO master_turns(session,ordinal,id,input,phase,body) VALUES (?1,?2,?3,?4,'succeeded',?5)",
                params![
                    session,
                    ordinal,
                    format!("turn-{ordinal}"),
                    input,
                    json!({"reply": {"text": reply}}).to_string()
                ],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT OR REPLACE INTO session_marks(session,active_ms) VALUES (?1,?2)",
                params![session, active_ms],
            )
            .unwrap();
    }

    fn team_message(&self, author: &str, text: &str, at_ms: i64) {
        self.db
            .execute(
                "INSERT INTO team_messages(project,author,text,state,created_ms) VALUES (?1,?2,?3,'read',?4)",
                params![PROJECT, author, text, at_ms],
            )
            .unwrap();
    }

    fn sessions(&self, query: &str) -> String {
        let (text, error) = sessions(&self.db, &project(), &self.agents, query);
        assert!(!error);
        text
    }

    fn session(&self, id: &str, offset: u64) -> (String, bool) {
        session(&self.db, &project(), &self.agents, id, offset)
    }
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn claude(speaker: &str, text: &str) -> String {
    json!({"type": speaker, "message": {"role": speaker, "content": [{"type": "text", "text": text}]}})
        .to_string()
}

fn codex(role: &str, text: &str) -> String {
    let part = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    json!({"type": "response_item", "payload": {"type": "message", "role": role, "content": [{"type": part, "text": text}]}})
        .to_string()
}

fn record(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::File::create(path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

fn hit_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|line| line.starts_with("- ")).collect()
}

/// S1: a search finds every word in one message of a master session's engine turns, the
/// team's messages and both harnesses' records in the agents' homes, any case, at most
/// `HITS_PER_CONVERSATION_MAX` hits from one conversation.
#[test]
fn a_search_finds_words_in_turns_team_messages_and_claude_and_codex_records() {
    let fixture = Fixture::new();
    fixture.turn(
        "s1",
        1,
        "Plan the Zebra page",
        "The zebra PLAN is ready",
        1_000,
    );
    fixture.team_message("engine", "Worker found a zebra plan bug", 2_000);
    let (claude_agent, claude_home) = fixture.agent("claude-code");
    record(
        &claude_home
            .join("projects")
            .join("proj")
            .join("rec-one.jsonl"),
        &[
            claude("user", "Find the zebra plan"),
            json!({"type": "progress", "data": "zebra plan"}).to_string(),
            claude("assistant", "zebra only"),
            claude("assistant", "The plan for the zebra is done"),
        ],
    );
    let (codex_agent, codex_home) = fixture.agent("codex");
    record(
        &codex_home
            .join("sessions")
            .join("2026")
            .join("10")
            .join("rollout-two.jsonl"),
        &[
            codex("user", "zebra plan from codex"),
            codex("assistant", "ok"),
        ],
    );
    let (_, many_home) = fixture.agent("claude-code");
    record(
        &many_home.join("projects").join("p").join("many.jsonl"),
        &(0..5)
            .map(|index| claude("assistant", &format!("zebra plan {index}")))
            .collect::<Vec<_>>(),
    );

    let text = fixture.sessions("PLAN zebra");
    let hits = hit_lines(&text);
    let has = |prefix: String| hits.iter().any(|hit| hit.starts_with(&prefix));
    assert!(has("- session/s1 #0 owner".into()), "{text}");
    assert!(has("- session/s1 #1 master".into()), "{text}");
    assert!(has("- team #0 engine".into()), "{text}");
    assert!(
        has(format!("- agent-{claude_agent}/rec-one #0 typed")),
        "{text}"
    );
    assert!(
        has(format!("- agent-{claude_agent}/rec-one #2 agent")),
        "{text}"
    );
    assert!(
        !has(format!("- agent-{claude_agent}/rec-one #1 ")),
        "{text}"
    );
    assert!(
        has(format!("- agent-{codex_agent}/rollout-two #0 typed")),
        "{text}"
    );
    assert_eq!(
        hits.iter().filter(|hit| hit.contains("/many #")).count(),
        HITS_PER_CONVERSATION_MAX,
        "{text}"
    );
    assert!(text.starts_with(&format!("{} hits for \"PLAN zebra\"", hits.len())));
    assert!(text.contains(DATA_NOTE));

    let none = fixture.sessions("zebra giraffe");
    assert!(hit_lines(&none).is_empty(), "{none}");
    assert!(none.starts_with("No message of this project's "), "{none}");
}

/// S1: an empty query lists the project's conversations newest first, records by their
/// change time, the team by its last message and sessions by their last activity.
#[test]
fn an_empty_query_lists_the_conversations_newest_first() {
    let fixture = Fixture::new();
    fixture.turn("old", 1, "Hello", "Hi", 1_000);
    fixture.turn("newer", 1, "Hello again", "Hi again", 5_000);
    fixture.team_message("engine", "A note", 3_000);
    let (agent, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("p").join("now.jsonl"),
        &[claude("user", "today")],
    );
    let text = fixture.sessions("   ");
    let ids: Vec<&str> = hit_lines(&text)
        .iter()
        .map(|line| line[2..].split(' ').next().unwrap())
        .collect();
    let record_id = format!("agent-{agent}/now");
    assert_eq!(
        ids,
        [record_id.as_str(), "session/newer", "team", "session/old"],
        "{text}"
    );
    assert!(text.starts_with("4 past conversations of this project, newest first."));
}

/// S1: a conversation reads a page at a time with numbered messages and the offset to
/// read next, ends with "End of this conversation", cuts a message at
/// `MESSAGE_CHARS_MAX` characters, and an unknown id is an error.
#[test]
fn a_conversation_reads_in_pages_with_the_next_offset_and_an_end() {
    let fixture = Fixture::new();
    let (agent, home) = fixture.agent("claude-code");
    let lines: Vec<String> = (0..12)
        .map(|index| {
            claude(
                "assistant",
                &format!("message {index} {}", "x".repeat(2_500)),
            )
        })
        .chain([claude("user", &"y".repeat(MESSAGE_CHARS_MAX + 10))])
        .collect();
    record(&home.join("projects").join("p").join("long.jsonl"), &lines);
    let id = format!("agent-{agent}/long");

    let (first, error) = fixture.session(&id, 0);
    assert!(!error);
    assert!(first.len() <= ANSWER_BYTES_MAX);
    assert!(first.contains("#0 agent: message 0 "), "{first}");
    let next: usize = first
        .lines()
        .last()
        .unwrap()
        .strip_prefix(&format!("More: session(\"{id}\", "))
        .and_then(|rest| rest.strip_suffix(")."))
        .unwrap_or_else(|| panic!("{first}"))
        .parse()
        .unwrap();
    assert!(first.contains(&format!("#{} agent: ", next - 1)));
    assert!(!first.contains(&format!("#{next} agent: ")));

    let mut offset = next;
    let mut last = String::new();
    for _ in 0..12 {
        let (page, error) = fixture.session(&id, offset as u64);
        assert!(!error);
        assert!(page.contains(&format!("#{offset} ")), "{page}");
        last = page;
        if last.ends_with("End of this conversation.\n") {
            break;
        }
        offset = last
            .lines()
            .last()
            .unwrap()
            .strip_prefix(&format!("More: session(\"{id}\", "))
            .and_then(|rest| rest.strip_suffix(")."))
            .unwrap()
            .parse()
            .unwrap();
    }
    assert!(last.ends_with("End of this conversation.\n"), "{last}");
    let cut = format!("#12 typed: {}...", "y".repeat(MESSAGE_CHARS_MAX));
    assert!(last.contains(&cut), "{last}");

    let (beyond, error) = fixture.session(&id, 13);
    assert!(!error);
    assert!(
        beyond.ends_with("This conversation has no message with this number.\n"),
        "{beyond}"
    );
    assert_eq!(
        fixture.session(&format!("agent-{agent}/missing"), 0),
        (NOT_FOUND.to_owned(), true)
    );
}

/// S1: a hit's message number is the number `session` shows that message under, also
/// when the search skips parsing lines without the words.
#[test]
fn a_hit_number_is_the_message_number_its_session_page_shows() {
    let fixture = Fixture::new();
    let (agent, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("p").join("numbers.jsonl"),
        &[
            claude("user", "first"),
            json!({"type": "system", "text": "not a message"}).to_string(),
            claude("assistant", "<command-name>skipped</command-name>"),
            claude("assistant", "second"),
            claude("user", "third has the needle"),
        ],
    );
    let id = format!("agent-{agent}/numbers");
    let text = fixture.sessions("needle");
    assert!(
        hit_lines(&text)
            .iter()
            .any(|hit| hit.starts_with(&format!("- {id} #2 typed"))),
        "{text}"
    );
    let (page, _) = fixture.session(&id, 2);
    assert!(page.contains("#2 typed: third has the needle"), "{page}");
    assert!(!page.contains("#1 "), "{page}");
}

/// S1: records behind a link, in a linked folder or home, or with a second hard link are
/// never read.
#[test]
fn linked_records_folders_homes_and_hard_linked_records_are_never_read() {
    let fixture = Fixture::new();
    let outside = fixture.agents.parent().unwrap().join("outside");
    record(
        &outside.join("p").join("secret.jsonl"),
        &[claude("user", "linkedword here")],
    );
    let (_, home) = fixture.agent("claude-code");
    let projects = home.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    symlink(outside.join("p"), projects.join("linked-folder")).unwrap();
    std::fs::create_dir_all(projects.join("own")).unwrap();
    symlink(
        outside.join("p").join("secret.jsonl"),
        projects.join("own").join("linked-record.jsonl"),
    )
    .unwrap();
    std::fs::hard_link(
        outside.join("p").join("secret.jsonl"),
        projects.join("own").join("hard.jsonl"),
    )
    .unwrap();

    let (linked_home_agent, linked_home) = fixture.agent("claude-code");
    std::fs::remove_dir(&linked_home).unwrap();
    symlink(home.parent().unwrap().join("home"), &linked_home).unwrap();

    let text = fixture.sessions("linkedword");
    assert!(hit_lines(&text).is_empty(), "{text}");
    let listed = fixture.sessions("");
    assert!(!listed.contains("linked-folder"), "{listed}");
    assert!(!listed.contains("/secret"), "{listed}");
    assert!(!listed.contains("linked-record"), "{listed}");
    assert!(
        !listed.contains(&format!("agent-{linked_home_agent}/")),
        "{listed}"
    );
}

/// S1: a record line over `LINE_BYTES_MAX` is skipped without ending the record, so the
/// next line is read and numbered as the first message.
#[test]
fn a_line_over_the_line_bound_is_skipped_and_the_next_is_read() {
    let fixture = Fixture::new();
    let (agent, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("p").join("big.jsonl"),
        &[
            claude("user", &format!("needle {}", "z".repeat(2 * 1024 * 1024))),
            claude("assistant", "needle after the long line"),
        ],
    );
    let text = fixture.sessions("needle");
    let hits = hit_lines(&text);
    assert_eq!(hits.len(), 1, "{text}");
    assert!(
        hits[0].starts_with(&format!("- agent-{agent}/big #0 agent")),
        "{text}"
    );
}

/// S1: a search reads at most `SCAN_BYTES_MAX` bytes, then says it stopped at its read
/// limit; a page past the limit says so instead of claiming the conversation ended.
#[test]
fn the_read_budget_stops_a_huge_record_and_says_so() {
    let fixture = Fixture::new();
    let (agent, home) = fixture.agent("claude-code");
    let path = home.join("projects").join("p").join("huge.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let filler = json!({"type": "progress", "data": "f".repeat(512 * 1024)}).to_string();
    let lines = (100 * 1024 * 1024) / filler.len() + 1;
    for _ in 0..lines {
        writeln!(file, "{filler}").unwrap();
    }
    writeln!(file, "{}", claude("user", "needle at the very end")).unwrap();
    file.flush().unwrap();
    drop(file);
    assert!(std::fs::metadata(&path).unwrap().len() > 100 * 1024 * 1024);

    let text = fixture.sessions("needle");
    assert!(hit_lines(&text).is_empty(), "{text}");
    assert!(
        text.contains("The search stopped at its read limit"),
        "{text}"
    );
    let (page, error) = fixture.session(&format!("agent-{agent}/huge"), 0);
    assert!(!error);
    assert!(
        page.ends_with("Reading stopped at the read limit before this message number.\n"),
        "{page}"
    );
}

/// S1: a conversation id is plain: letters, digits, `-`, `_`, `.` and `/`, no `..`, at most
/// `ID_BYTES_MAX` bytes.
#[test]
fn only_plain_bounded_ids_are_valid() {
    assert!(id_valid("agent-3/0b1c-rollout_2026.10"));
    assert!(id_valid("session/abc"));
    assert!(id_valid(&"a".repeat(ID_BYTES_MAX)));
    for id in [
        "",
        "agent-3/../etc",
        "agent 3",
        "agent-3/\u{7}",
        "agent-3/é",
        &"a".repeat(ID_BYTES_MAX + 1),
    ] {
        assert!(!id_valid(id), "{id:?}");
    }
}
