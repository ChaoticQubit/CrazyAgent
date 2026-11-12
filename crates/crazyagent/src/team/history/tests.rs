use super::*;
use serde_json::json;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};

const PROJECT: &str = "history-project";

const OTHER: &str = "other-project";

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
             CREATE TABLE session_marks (session TEXT PRIMARY KEY, active_ms INTEGER NOT NULL);
             CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL);
             INSERT INTO projects(id,body) VALUES ('history-project','{}'),('other-project','{}');",
        )
        .unwrap();
        crate::search_index::initialize(&db).unwrap();
        let agents = base.join("agents");
        std::fs::create_dir(&agents).unwrap();
        Self {
            _root: root,
            agents,
            db,
        }
    }

    fn agent(&self, harness: &str) -> (i64, PathBuf) {
        self.agent_of(PROJECT, harness)
    }

    /// Records a sub-agent of a project with its home in the engine's one agents folder.
    /// Inputs: the project and the harness. Output: the agent id and its home.
    fn agent_of(&self, project: &str, harness: &str) -> (i64, PathBuf) {
        self.db
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
                 VALUES (?1,'sub',?2,1,'idle',1,1)",
                params![project, harness],
            )
            .unwrap();
        let id = self.db.last_insert_rowid();
        let home = self.agents.join(format!("agent-{id}")).join("home");
        std::fs::create_dir_all(&home).unwrap();
        (id, home)
    }

    fn turn(&self, session: &str, ordinal: i64, owner: &str, reply: &str, active_ms: i64) {
        self.turn_of(PROJECT, session, ordinal, owner, reply, active_ms);
    }

    /// Records one finished engine turn of a project's master session: the owner's message
    /// and the master's reply, with the session's last activity. Inputs: the project, the
    /// session, the turn's ordinal, both texts and the activity time.
    fn turn_of(
        &self,
        project: &str,
        session: &str,
        ordinal: i64,
        owner: &str,
        reply: &str,
        active_ms: i64,
    ) {
        self.db
            .execute(
                "INSERT OR IGNORE INTO master_sessions(id,project,created,version,phase,body)
                 VALUES (?1,?2,?3,?3,'parked','{}')",
                params![session, project, active_ms],
            )
            .unwrap();
        let input = active_ms + ordinal;
        self.db
            .execute(
                "INSERT INTO conversation_entries(event_cursor,project,kind,text) VALUES (?1,?2,'message',?3)",
                params![input, project, owner],
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
        self.team_message_of(PROJECT, author, text, at_ms);
    }

    /// Records one read team message of a project. Inputs: the project, the author, the
    /// text and the time.
    fn team_message_of(&self, project: &str, author: &str, text: &str, at_ms: i64) {
        self.db
            .execute(
                "INSERT INTO team_messages(project,author,text,state,created_ms) VALUES (?1,?2,?3,'read',?4)",
                params![project, author, text, at_ms],
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

/// Reads the conversation ids of a `sessions("")` listing, in its order.
fn listed_ids(text: &str) -> Vec<&str> {
    hit_lines(text)
        .iter()
        .map(|line| line[2..].split(' ').next().unwrap())
        .collect()
}

/// S1, project scope: another project's master session, with a turn holding the search
/// word, is never listed, found or read from this project; its id reads as not found.
#[test]
fn another_projects_master_session_is_never_listed_found_or_read() {
    let fixture = Fixture::new();
    fixture.turn("mine", 1, "Plan the page", "Planned", 1_000);
    fixture.turn_of(
        OTHER,
        "theirs",
        1,
        "The okapi plan",
        "The okapi plan is ready",
        2_000,
    );
    let listed = fixture.sessions("");
    assert_eq!(listed_ids(&listed), ["session/mine"], "{listed}");
    let found = fixture.sessions("okapi");
    assert!(hit_lines(&found).is_empty(), "{found}");
    assert_eq!(
        fixture.session("session/theirs", 0),
        (NOT_FOUND.to_owned(), true)
    );
}

/// S1, project scope: another project's team messages never join this project's team
/// conversation, in a search or a read of it.
#[test]
fn another_projects_team_messages_are_never_found_or_read() {
    let fixture = Fixture::new();
    fixture.team_message("engine", "Worker found a bug", 1_000);
    fixture.team_message_of(OTHER, "engine", "The okapi plan is ready", 2_000);
    let found = fixture.sessions("okapi");
    assert!(hit_lines(&found).is_empty(), "{found}");
    let (page, error) = fixture.session("team", 0);
    assert!(!error, "{page}");
    assert!(page.contains("Worker found a bug"), "{page}");
    assert!(!page.contains("okapi"), "{page}");
}

/// S1, project scope: another project's team agent record, in the same engine agents
/// folder and holding the search word, is never listed, found or read from this project;
/// its id reads as not found.
#[test]
fn another_projects_agent_records_are_never_listed_found_or_read() {
    let fixture = Fixture::new();
    let (mine, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("p").join("mine.jsonl"),
        &[claude("user", "Plan the page")],
    );
    let (theirs, their_home) = fixture.agent_of(OTHER, "claude-code");
    record(
        &their_home.join("projects").join("p").join("theirs.jsonl"),
        &[claude("user", "The okapi plan")],
    );
    let listed = fixture.sessions("");
    assert_eq!(
        listed_ids(&listed),
        [format!("agent-{mine}/mine")],
        "{listed}"
    );
    let found = fixture.sessions("okapi");
    assert!(hit_lines(&found).is_empty(), "{found}");
    assert_eq!(
        fixture.session(&format!("agent-{theirs}/theirs"), 0),
        (NOT_FOUND.to_owned(), true)
    );
}

/// S1, project scope: another project's team activity never lists this project's team
/// conversation, and once this project's team has written, its entry carries this
/// project's last team time, never the other project's newer one.
#[test]
fn another_projects_team_activity_never_lists_or_dates_this_projects_team() {
    const DAY_MS: i64 = 86_400_000;
    let fixture = Fixture::new();
    fixture.team_message_of(OTHER, "engine", "Their note", 3 * DAY_MS);
    let listed = fixture.sessions("");
    assert!(listed_ids(&listed).is_empty(), "{listed}");
    fixture.team_message("engine", "Our note", DAY_MS);
    let listed = fixture.sessions("");
    assert_eq!(
        hit_lines(&listed),
        ["- team (team messages and engine notes, last active 1970-01-02 00:00 UTC)"],
        "{listed}"
    );
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

/// OP25-S5: one agent's record tree never crowds the others out of the search. A newer
/// agent whose home holds as many entries as the whole visit budget, or more newer records
/// than the search keeps, still leaves an older agent its share, so the older agent's one
/// record is found.
#[test]
fn one_agent_tree_over_the_budgets_leaves_the_other_agents_their_share() {
    for shape in ["visits", "records"] {
        let fixture = Fixture::new();
        let (quiet, quiet_home) = fixture.agent("claude-code");
        let path = quiet_home.join("projects").join("p").join("quiet.jsonl");
        record(&path, &[claude("user", "The okapi plan")]);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_000_000))
            .unwrap();
        let (_, busy_home) = fixture.agent("claude-code");
        let folder = busy_home.join("projects").join("p");
        std::fs::create_dir_all(&folder).unwrap();
        let (entries, extension) = match shape {
            "visits" => (VISITS_MAX, "txt"),
            _ => (RECORDS_MAX + 1, "jsonl"),
        };
        for index in 0..entries {
            std::fs::File::create(folder.join(format!("{index}.{extension}"))).unwrap();
        }

        let found = fixture.sessions("okapi");

        let hits = hit_lines(&found);
        assert_eq!(hits.len(), 1, "{shape}: {found}");
        assert!(
            hits[0].starts_with(&format!("- agent-{quiet}/quiet #")),
            "{shape}: {found}"
        );
    }
}

/// OP25-S5 r1 M1: the newest agent, whose work people search most, gets every part of the
/// budgets its older agents leave. Many quiet older agents hold one record each and the
/// newest busy agent holds `RECORDS_MAX` records: every quiet record is found, and so is
/// the busy agent's oldest record that still fits the record budget after them.
#[test]
fn the_newest_busy_agent_keeps_what_the_quiet_older_agents_leave_of_the_budgets() {
    const QUIET: usize = 9;
    let fixture = Fixture::new();
    let mut quiet = Vec::new();
    for _ in 0..QUIET {
        let (agent, home) = fixture.agent("claude-code");
        let path = home.join("projects").join("p").join("quiet.jsonl");
        record(&path, &[claude("user", "The okapi plan")]);
        quiet.push(format!("- agent-{agent}/quiet #"));
    }
    let (busy, busy_home) = fixture.agent("claude-code");
    let folder = busy_home.join("projects").join("p");
    let last = RECORDS_MAX - QUIET;
    for rank in 1..=RECORDS_MAX {
        let path = folder.join(format!("{rank}.jsonl"));
        if rank == last {
            record(&path, &[claude("user", "The zebra plan")]);
        } else {
            record(&path, &[]);
        }
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(2_000_000 - rank as u64))
            .unwrap();
    }

    let okapi = fixture.sessions("okapi");
    let zebra = fixture.sessions("zebra");

    let mut found: Vec<&str> = hit_lines(&okapi)
        .into_iter()
        .map(|hit| &hit[..hit.find('#').unwrap() + 1])
        .collect();
    found.sort_unstable();
    quiet.sort_unstable();
    assert_eq!(found, quiet, "{okapi}");
    let hits = hit_lines(&zebra);
    assert_eq!(hits.len(), 1, "{zebra}");
    assert!(
        hits[0].starts_with(&format!("- agent-{busy}/{last} #")),
        "{zebra}"
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

/// A Claude Code `assistant` record line holding one tool call, as Claude Code writes it.
/// Inputs: the call id, the tool name and its input. Output: the JSON line.
fn claude_call(id: &str, name: &str, input: serde_json::Value) -> String {
    json!({"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": id, "name": name, "input": input}], "stop_reason": "tool_use"}})
    .to_string()
}

/// A Claude Code `user` record line holding one tool's result, as Claude Code writes it.
/// Inputs: the call id and the output text. Output: the JSON line.
fn claude_result(id: &str, output: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": [
        {"tool_use_id": id, "type": "tool_result", "content": output, "is_error": false}]},
        "toolUseResult": {"stdout": output, "stderr": "", "interrupted": false}})
    .to_string()
}

/// OP-25: a word that is only in a tool's output finds that call in Claude Code and Codex
/// records, and the conversation page shows the call's input and its output.
#[test]
fn a_word_only_in_a_tool_output_finds_the_call_and_the_page_shows_its_input_and_output() {
    let fixture = Fixture::new();
    let (claude_agent, claude_home) = fixture.agent("claude-code");
    record(
        &claude_home
            .join("projects")
            .join("proj")
            .join("tools.jsonl"),
        &[
            claude("user", "Read today's note"),
            claude_call(
                "toolu_01A",
                "Bash",
                json!({"command": "cat notes/today.md", "description": "Read the note"}),
            ),
            claude_result("toolu_01A", "Meeting moved: quokkafern review at 3pm"),
            claude("assistant", "The review moved to 3pm."),
        ],
    );
    let (codex_agent, codex_home) = fixture.agent("codex");
    record(
        &codex_home
            .join("sessions")
            .join("2026")
            .join("11")
            .join("rollout-tools.jsonl"),
        &[
            json!({"timestamp": "2026-11-03T13:00:00.000Z", "type": "response_item", "payload": {
                "type": "function_call", "name": "shell", "call_id": "call_9",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"cat ledger.txt\"],\"workdir\":\"/work\"}"}})
            .to_string(),
            json!({"timestamp": "2026-11-03T13:00:01.000Z", "type": "response_item", "payload": {
                "type": "function_call_output", "call_id": "call_9",
                "output": "{\"output\":\"wombatlynx total 42\\nsecond line\\n\",\"metadata\":{\"exit_code\":0,\"duration_seconds\":0.1}}"}})
            .to_string(),
        ],
    );

    let claude_id = format!("agent-{claude_agent}/tools");
    let found = fixture.sessions("quokkafern");
    let hits = hit_lines(&found);
    assert_eq!(hits.len(), 1, "{found}");
    assert!(
        hits[0].starts_with(&format!("- {claude_id} #2 tool ")),
        "{found}"
    );
    let (page, missing) = fixture.session(&claude_id, 0);
    assert!(!missing);
    assert!(page.contains("#0 typed: Read today's note"), "{page}");
    assert!(
        page.contains(
            "#1 agent: Tool call Bash:\ncommand: cat notes/today.md\ndescription: Read the note"
        ),
        "{page}"
    );
    assert!(
        page.contains("#2 tool: Tool output:\nMeeting moved: quokkafern review at 3pm"),
        "{page}"
    );
    assert!(
        page.contains("#3 agent: The review moved to 3pm."),
        "{page}"
    );

    let codex_id = format!("agent-{codex_agent}/rollout-tools");
    let found = fixture.sessions("WOMBATLYNX");
    let hits = hit_lines(&found);
    assert_eq!(hits.len(), 1, "{found}");
    assert!(
        hits[0].starts_with(&format!("- {codex_id} #1 tool ")),
        "{found}"
    );
    let (page, missing) = fixture.session(&codex_id, 0);
    assert!(!missing);
    assert!(
        page.contains(
            "#0 agent: Tool call shell:\ncommand: bash -lc cat ledger.txt\nworkdir: /work"
        ),
        "{page}"
    );
    assert!(
        page.contains("#1 tool: Tool output:\nwombatlynx total 42\nsecond line\n\n"),
        "{page}"
    );
}

/// OP-25: credential-shaped values in tool inputs, tool outputs and messages (a prefixed
/// token, a key header, a password field) are masked before a search reads them, so no
/// search finds these values and no page shows them. Masking covers known shapes only.
#[test]
fn credential_shaped_values_in_tool_io_and_messages_are_masked_before_search_and_paging() {
    let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
    let key = "sk-proj-AbCdEfGhIjKlMnOpQrStUvWx";
    let fixture = Fixture::new();
    let (agent, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("proj").join("secrets.jsonl"),
        &[
            claude(
                "assistant",
                &format!("Set GITHUB_TOKEN={token} for the push"),
            ),
            claude_call(
                "toolu_02B",
                "Bash",
                json!({"command": format!("curl -H 'X-Key: {key}' https://example.com/api")}),
            ),
            claude_result(
                "toolu_02B",
                &format!("export GITHUB_TOKEN={token}\npassword: hunter2\nquokkafern done"),
            ),
        ],
    );
    let id = format!("agent-{agent}/secrets");

    for secret in [token, key, "hunter2"] {
        let found = fixture.sessions(secret);
        assert!(hit_lines(&found).is_empty(), "{secret}: {found}");
    }
    let (page, missing) = fixture.session(&id, 0);
    assert!(!missing);
    for secret in [token, key, "hunter2"] {
        assert!(!page.contains(secret), "{secret}: {page}");
    }
    assert!(
        page.contains("#0 agent: Set [hidden] for the push"),
        "{page}"
    );
    assert!(
        page.contains("curl -H 'X-Key: [hidden] https://example.com/api"),
        "{page}"
    );
    assert!(
        page.contains("export [hidden]\n[hidden: this line may contain a secret]\nquokkafern done"),
        "{page}"
    );
    assert_eq!(hit_lines(&fixture.sessions("quokkafern")).len(), 1);
}

/// OP-25: one tool output is kept up to its bound, so a word near its start is found and a
/// word far past the bound is not.
#[test]
fn a_huge_tool_output_is_kept_only_up_to_its_bound() {
    let fixture = Fixture::new();
    let (_, home) = fixture.agent("claude-code");
    let output = format!(
        "quokkafern start {} wombatlynx end",
        "filler ".repeat(30_000)
    );
    record(
        &home.join("projects").join("proj").join("huge-tool.jsonl"),
        &[
            claude_call("toolu_03C", "Read", json!({"file_path": "/work/big.log"})),
            claude_result("toolu_03C", &output),
        ],
    );

    assert_eq!(hit_lines(&fixture.sessions("quokkafern")).len(), 1);
    let far = fixture.sessions("wombatlynx");
    assert!(hit_lines(&far).is_empty(), "{far}");
}

/// OP-25 security: rows the engine stored before this version, a master turn and a team
/// message holding credential-shaped values in clear, are masked from the first read after
/// the upgrade: no search finds these values, no page shows them, and other words still
/// find the turn.
#[test]
fn rows_stored_before_the_upgrade_are_masked_from_the_first_read() {
    let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
    let key = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx0123";
    let fixture = Fixture::new();
    fixture.turn(
        "s9",
        1,
        &format!("Use GITHUB_TOKEN={token} quokkafern"),
        &format!("Saved {key} for later"),
        1_000,
    );
    fixture.team_message(
        "engine",
        &format!("Agent printed password: hunter2 and {token}"),
        2_000,
    );

    for secret in [token, key, "hunter2"] {
        let found = fixture.sessions(secret);
        assert!(hit_lines(&found).is_empty(), "{secret}: {found}");
    }
    for id in ["session/s9", "team"] {
        let (page, missing) = fixture.session(id, 0);
        assert!(!missing, "{id}");
        for secret in [token, key, "hunter2"] {
            assert!(!page.contains(secret), "{id} {secret}: {page}");
        }
    }
    let (page, _) = fixture.session("session/s9", 0);
    assert!(page.contains("#0 owner: Use [hidden] quokkafern"), "{page}");
    assert!(
        page.contains("#1 master: Saved [hidden] for later"),
        "{page}"
    );
    assert_eq!(hit_lines(&fixture.sessions("quokkafern")).len(), 1);
}

/// Writes one Claude Code record whose tool output is `output`, then reads it through the
/// real search and page: no value in `secrets`, each one search word, is found or shown, and
/// the plain word `kept` still finds the output. Inputs: the output text, the values and the
/// plain word.
fn assert_masked(output: &str, secrets: &[&str], kept: &str) {
    let fixture = Fixture::new();
    let (agent, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("proj").join("shapes.jsonl"),
        &[
            claude_call("toolu_09S", "Bash", json!({"command": "cat notes.txt"})),
            claude_result("toolu_09S", output),
        ],
    );
    let (page, missing) = fixture.session(&format!("agent-{agent}/shapes"), 0);
    assert!(!missing, "{page}");
    for secret in secrets {
        assert!(
            !secret.is_empty()
                && secret.len() <= WORD_BYTES_MAX
                && !secret.contains(char::is_whitespace),
            "{secret} is not one search word"
        );
        let found = fixture.sessions(secret);
        assert!(hit_lines(&found).is_empty(), "{secret} is found: {found}");
        assert!(!page.contains(secret), "{secret} is shown: {page}");
    }
    let found = fixture.sessions(kept);
    assert_eq!(
        hit_lines(&found).len(),
        1,
        "{kept} is not found: {found} {page}"
    );
}

/// OP-25 F1: a password in a URL's user part, as in a clone or database URL, and a
/// `.pgpass`-style line are masked, and the host stays searchable.
#[test]
fn a_password_in_a_url_user_part_or_a_pgpass_line_is_masked() {
    assert_masked(
        "remote: https://alice:Tr0ub4dor3x@git.example.com/acme/app.git\n\
         postgres://svc:Zq8WmP2v@db.internal:5432/shop\n\
         db.internal:5432:shop:svc:Pg9pass7x",
        &["Tr0ub4dor3x", "alice:Tr0ub4dor3x", "Zq8WmP2v", "Pg9pass7x"],
        "git.example.com",
    );
}

/// OP-25 F1: passwords given as command flags, `-p<password>` and `-u user:password`, are
/// masked, and the command stays searchable.
#[test]
fn passwords_in_command_flags_are_masked() {
    assert_masked(
        "$ mysql -u root -pS3cretPw9 shop\n$ curl -u deploy:Hunter22x https://api.example.com/v1",
        &["S3cretPw9", "Hunter22x"],
        "mysql",
    );
}

/// OP-25 F1: short values of keys named like a token, a password or a credential
/// (`NPM_TOKEN=`, `DB_PASS=`, `--token <value>`, `"credentials": "<value>"`) are masked.
#[test]
fn short_token_password_and_credential_values_are_masked() {
    assert_masked(
        "NPM_TOKEN=npm7Qx2 npm publish\n\
         export DB_PASS=letmein7\n\
         gh auth login --token gq7RtY2\n\
         {\"credentials\": \"cr3dVal9\"}",
        &["npm7Qx2", "letmein7", "gq7RtY2", "cr3dVal9"],
        "publish",
    );
}

/// OP-25 F1: auth headers and fields, cookies and session ids are masked.
#[test]
fn auth_cookie_and_session_id_values_are_masked() {
    assert_masked(
        "< Set-Cookie: sid=c00k1eVal; Path=/\n\
         > X-Auth-Token: xat9K2m\n\
         auth: q9w8e7\n\
         sessionid=s3ss10nV\n\
         quokkafern done",
        &["c00k1eVal", "xat9K2m", "q9w8e7", "s3ss10nV"],
        "quokkafern",
    );
}

/// OP-25 F1: the base64 of a short credential, on a line with no marker word, is masked.
#[test]
fn base64_of_a_short_credential_is_masked() {
    assert_masked(
        "upstream X-Upstream dXNlcm5hbWU6cGFzc3dvcmQ=\nquokkafern done",
        &["dXNlcm5hbWU6cGFzc3dvcmQ"],
        "quokkafern",
    );
}

/// OP-25 F1: AWS-style secrets holding `/` and `+`, on lines with no marker word, are
/// masked.
#[test]
fn slash_and_plus_bearing_aws_style_secrets_are_masked() {
    assert_masked(
        "default Xk9pQ2rT7vLm/Bn4Hs8Jd/Wq3Ze6Yc1Ua5Gf0Ri2\n\
         ci W8+e7Rq2Lp4Zx9/Mn1Bv6Cy3Tk5Hd0Js8Fg2Ka\n\
         quokkafern done",
        &[
            "Xk9pQ2rT7vLm",
            "Bn4Hs8Jd",
            "Wq3Ze6Yc1Ua5Gf0Ri2",
            "e7Rq2Lp4Zx9",
            "Mn1Bv6Cy3Tk5Hd0Js8Fg2Ka",
        ],
        "quokkafern",
    );
}

/// OP-25 F1: paths to the credential files and folders the sandbox guards, as a tool reads
/// or lists them, are masked.
#[test]
fn credential_file_paths_are_masked() {
    assert_masked(
        "cat /Users/alice/.aws/credentials\n\
         ls ~/.ssh/id_ed25519_work\n\
         source ~/.netrc\n\
         quokkafern done",
        &[".aws/credentials", "id_ed25519_work", ".netrc"],
        "quokkafern",
    );
}

/// OP-25 F1: a private key block is masked from its BEGIN line to its END line, body lines
/// full of `/` and `+` and a short last line included, and the text after it stays
/// searchable.
#[test]
fn a_private_key_block_is_masked_from_begin_to_end() {
    assert_masked(
        "-----BEGIN OPENSSH PRIVATE KEY-----\n\
         b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n\
         Qy/Nm+Ab/Zx+Pq/Lm+Kd/Rt+Yw/Hs+Fe/Gv+Bn/Jc+Ux/Ka+Wd/Mi+Op/Ql+Vs/Ty+Ib\n\
         AAAEC4k9w=\n\
         -----END OPENSSH PRIVATE KEY-----\n\
         quokkafern done",
        &[
            "OPENSSH",
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQ",
            "MwAAAAtzc2gtZW",
            "Ab/Zx",
            "AAAEC4k9w",
        ],
        "quokkafern",
    );
}

/// OP-25 F1: a token wrapped across two lines of one output is masked whole: the tail at
/// the next line's start goes with the masked first piece.
#[test]
fn the_wrapped_tail_of_a_masked_token_is_masked() {
    assert_masked(
        "config ghp_0123456789abcdefABCD\nEF0123456789abcd\nquokkafern done",
        &["EF0123456789abcd"],
        "quokkafern",
    );
}

/// OP-25: plain words next to the masking rules stay searchable: token counts, an author
/// field, a path holding digits, a Rust path through an `auth` module and a sha-free
/// command line.
#[test]
fn plain_counts_paths_and_code_next_to_masked_shapes_stay_searchable() {
    let fixture = Fixture::new();
    let (_, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("proj").join("plain.jsonl"),
        &[
            claude_call("toolu_05E", "Bash", json!({"command": "git log -1"})),
            claude_result(
                "toolu_05E",
                "Input tokens: 1234\n\
                 author: alice\n\
                 cd Projects/CrazyAgent/apps/desktop/test/native/trial4\n\
                 crate::auth::login(user)\n\
                 mkdir -p build/out",
            ),
        ],
    );
    for word in [
        "tokens: 1234",
        "author: alice",
        "native/trial4",
        "crate::auth::login",
        "build/out",
    ] {
        let found = fixture.sessions(word);
        assert_eq!(hit_lines(&found).len(), 1, "{word}: {found}");
    }
}

/// OP-25 F2: a native teammate's record, kept in the home its lead's teammates run with
/// beside the lead's own (TPH-2 C), is found by a word in its tool output and read by its
/// id, labelled as the lead's native teammates.
#[test]
fn a_native_teammates_record_in_the_mates_home_is_found_and_read() {
    let fixture = Fixture::new();
    let (lead, home) = fixture.agent("claude-code");
    fixture
        .db
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,parent,pane)
             VALUES (?1,'teammate','claude-code',1,'idle',1,1,?2,'%7')",
            params![PROJECT, lead],
        )
        .unwrap();
    record(
        &crate::launch_context::mates_home(&home)
            .join("projects")
            .join("proj")
            .join("mate.jsonl"),
        &[
            claude_call("toolu_04D", "Bash", json!({"command": "cat todo.txt"})),
            claude_result("toolu_04D", "quokkafern list ready"),
        ],
    );

    let id = format!("agent-{lead}/mates/mate");
    let found = fixture.sessions("quokkafern");
    let hits = hit_lines(&found);
    assert_eq!(hits.len(), 1, "{found}");
    assert!(
        hits[0].starts_with(&format!(
            "- {id} #1 tool (native teammates of agent-{lead} "
        )),
        "{found}"
    );
    let (page, missing) = fixture.session(&id, 0);
    assert!(!missing, "{page}");
    assert!(
        page.contains("#1 tool: Tool output:\nquokkafern list ready"),
        "{page}"
    );
}

/// OP-25 OP25-1: webhook URLs whose path is their secret (Slack, Discord) are masked.
#[test]
fn webhook_url_secrets_are_masked() {
    assert_masked(
        "POST https://hooks.slack.com/services/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\n\
         POST https://discord.com/api/webhooks/123456789012345678/\
         AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_AbCdEfGhIjKlMnOpQrStUvWxYz0123\n\
         quokkafern done",
        &[
            "x9Yz8Wv7Ut6Sr5Qp4On3Ml2K",
            "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789",
        ],
        "quokkafern",
    );
}

/// OP-25 OP25-1: an AWS temporary access key id (`ASIA...`) is masked like a long-lived
/// one.
#[test]
fn an_aws_temporary_access_key_id_is_masked() {
    assert_masked(
        "aws_access_key_id = ASIAY34FZKBOKMUTVV7A\nquokkafern done",
        &["ASIAY34FZKBOKMUTVV7A"],
        "quokkafern",
    );
}

/// OP-25 OP25-1: a `private_key` field and a key block written JSON-escaped on one line,
/// as in a cloud service account file, are masked.
#[test]
fn a_private_key_field_and_a_json_escaped_key_block_are_masked() {
    assert_masked(
        "private_key: Kq9xW2LmP4nB7vR\n\
         {\"type\": \"service_account\", \"private_key\": \"-----BEGIN PRIVATE KEY-----\\nMIIEvQIBADAN\\n\
         Bg/kq+hk/iG9w\\n-----END PRIVATE KEY-----\\n\"}\n\
         quokkafern done",
        &[
            "Kq9xW2LmP4nB7vR",
            "MIIEvQIBADAN",
            "Bg/kq+hk/iG9w",
            "service_account",
        ],
        "quokkafern",
    );
}

/// OP-25 r2 residual: a short value after a credential-named key whose `=` or `:` stands
/// apart, as Python, TOML, INI and spaced JSON write it, is masked.
#[test]
fn a_short_value_after_a_spaced_equals_or_colon_is_masked() {
    assert_masked(
        "auth_token = \"tk9Lm2\"\n\
         token = \"tq8Wz1\"\n\
         pass = hunter7x\n\
         {\"token\" : \"tz3Kp9\"}\n\
         quokkafern done",
        &["tk9Lm2", "tq8Wz1", "hunter7x", "tz3Kp9"],
        "quokkafern",
    );
}

/// OP-25 r2 residual: an all-lowercase password attached to `-p` is masked, while known
/// compiler and `find` flags spelled `-p...` stay searchable.
#[test]
fn a_lowercase_password_attached_to_p_is_masked_and_known_p_flags_stay() {
    assert_masked(
        "$ mysql -u root -pletmein shop\n$ cc -pthread -pedantic main.c",
        &["letmein"],
        "-pthread",
    );
}

/// OP-25 r2 residual: a token wrapped onto an indented next line is masked whole.
#[test]
fn an_indented_wrapped_tail_of_a_masked_token_is_masked() {
    assert_masked(
        "config ghp_0123456789abcdefABCD\n    EF0123456789abcd\nquokkafern done",
        &["EF0123456789abcd"],
        "quokkafern",
    );
}

/// OP-25 r2 S1 (Codex scan b4ef1526): `-proot` and `-phuntertwo` are masked, while `find`'s
/// `-print` stays searchable.
#[test]
fn the_codex_lowercase_p_passwords_are_masked_and_find_print_stays() {
    assert_masked(
        "$ mysql -u root -proot shop\n$ mysql -phuntertwo shop\n$ find . -name x -print",
        &["-proot", "-phuntertwo"],
        "-print",
    );
}

/// OP-25 r2 S1: a `-p` password that itself starts with `-` is masked; every compact `-p`
/// value other than a known flag is a password, whatever characters it holds.
#[test]
fn a_p_password_starting_with_a_dash_is_masked() {
    assert_masked(
        "$ mysql -u admin -p-quokka9 shop\nquietheron listed",
        &["-p-quokka9"],
        "quietheron",
    );
}

/// OP-25 r2 S2: a `.pgpass` line is masked by its shape, four host, port, database and user
/// fields and then a password holding any characters, a quoted line and `\:` included.
#[test]
fn a_pgpass_line_with_a_punctuated_password_is_masked() {
    assert_masked(
        "db.internal:5432:shop:svc:p@ss!w0rd\n\
         $ echo 'localhost:5432:*:svc:pa\\:ss9' >> pgfile\n\
         quietheron listed",
        &["p@ss!w0rd", "pa\\:ss9"],
        "quietheron",
    );
}

/// OP-25 r2 S3: a record's folder that an agent swaps for a link after the listing, to a
/// folder outside the agents folder, is never read; the record reads before the swap.
#[test]
fn a_record_folder_swapped_for_a_link_after_the_listing_is_never_read() {
    let fixture = Fixture::new();
    let (agent, home) = fixture.agent("claude-code");
    let folder = home.join("projects").join("p");
    record(
        &folder.join("swap.jsonl"),
        &[claude("user", "insideword here")],
    );
    let outside = fixture.agents.parent().unwrap().join("outside");
    record(
        &outside.join("swap.jsonl"),
        &[claude("user", "outsideword here")],
    );
    let found = conversations(&fixture.db, &project(), &fixture.agents);
    let swap = found
        .iter()
        .find(|one| one.id == format!("agent-{agent}/swap"))
        .unwrap();
    let read = |conversation: &Conversation| {
        let mut texts = Vec::new();
        let mut budget = Budget {
            left: SCAN_BYTES_MAX,
        };
        each_message(
            &fixture.db,
            &project(),
            conversation,
            &mut budget,
            None,
            &mut |_, _, text| {
                texts.push(text.to_owned());
                true
            },
        );
        texts
    };
    let before = read(swap);
    assert!(
        before.iter().any(|text| text.contains("insideword")),
        "{before:?}"
    );
    std::fs::rename(&folder, home.join("projects").join("moved")).unwrap();
    symlink(&outside, &folder).unwrap();
    let after = read(swap);
    assert!(
        after.iter().all(|text| !text.contains("outsideword")),
        "{after:?}"
    );
}

/// OP-25 r2 S4: a sparse record with no line break, far larger than the read budget, ends
/// a search at the budget instead of being read to its end.
#[test]
fn a_huge_record_with_no_line_break_is_read_only_up_to_the_budget() {
    const SPARSE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
    let fixture = Fixture::new();
    let (_, home) = fixture.agent("claude-code");
    let path = home.join("projects").join("p").join("sparse.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::File::create(&path)
        .unwrap()
        .set_len(SPARSE_BYTES)
        .unwrap();
    let started = std::time::Instant::now();
    let text = fixture.sessions("needle");
    let took = started.elapsed();
    assert!(
        text.contains("The search stopped at its read limit"),
        "{text}"
    );
    assert!(took < std::time::Duration::from_secs(3), "{took:?}");
}

/// F33: the session search names engine-typed record messages `engine`, and keeps the
/// reader's own roles for instructions, summaries, owner-typed text, replies and tools.
#[test]
fn record_messages_the_engine_typed_are_searched_as_the_engine_s() {
    assert_eq!(record_role("typed", "[team] Agent 4 finished"), "engine");
    assert_eq!(record_role("typed", "Ship the parser"), "typed");
    for role in ["instructions", "summary", "agent", "tool"] {
        assert_eq!(record_role(role, "[team] Agent 4 finished"), role);
    }
}

const INDEXED_AT: i64 = 1_762_950_000_000;

impl Fixture {
    /// Runs one indexer round for the project within the engine's bounds. Output: whether the
    /// project is caught up.
    fn index(&mut self) -> bool {
        crate::search_index::indexer::index_project(
            &mut self.db,
            &self.agents,
            &project(),
            INDEXED_AT,
        )
        .unwrap()
        .caught_up
    }

    /// Runs one indexer round for the project within small bounds. Inputs: the most chunks
    /// and bytes. Output: whether the project is caught up.
    fn index_within(&mut self, chunks: usize, bytes: u64) -> bool {
        crate::search_index::indexer::index_within(
            &mut self.db,
            &self.agents,
            &project(),
            INDEXED_AT,
            chunks,
            bytes,
        )
        .unwrap()
        .caught_up
    }

    /// Reads the indexed chunks of one conversation of the project, by message number.
    fn chunks(&self, key: &str) -> Vec<(i64, String)> {
        self.db
            .prepare("SELECT ordinal, text FROM search_chunks WHERE project=?1 AND key=?2 ORDER BY ordinal")
            .unwrap()
            .query_map(params![PROJECT, key], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Asserts each indexed chunk of a conversation opens the page session(key, ordinal)
    /// reads, so a hit's number always reads the message it names.
    fn assert_numbers_match_session(&self, key: &str) {
        let chunks = self.chunks(key);
        assert!(!chunks.is_empty(), "{key} has chunks");
        for (ordinal, text) in chunks {
            let (page, error) = self.session(key, ordinal as u64);
            assert!(!error, "{page}");
            let first = page.lines().find(|line| line.starts_with('#')).unwrap();
            assert!(first.starts_with(&format!("#{ordinal} ")), "{first}");
            let head: String = text.chars().take(30).collect();
            assert!(
                page.contains(&head),
                "{key} #{ordinal}: {head:?} not in {page}"
            );
        }
    }
}

/// DEC-105 D5: every chunk the indexer stores carries the message number session() prints
/// for it, for engine turns, team messages and Claude Code and Codex records with tool I/O.
#[test]
fn the_index_numbers_every_message_as_session_reads_it() {
    let mut fixture = Fixture::new();
    fixture.turn(
        "s1",
        1,
        "Plan the okapi page",
        "Planned the okapi page",
        1_000,
    );
    fixture.turn("s1", 2, "Ship it", "Shipped", 1_100);
    fixture.team_message("engine", "Worker found an okapi bug", 1_000);
    fixture.team_message("lead", "Fixed the okapi bug", 1_200);
    let (claude_id, claude_home) = fixture.agent("claude-code");
    record(
        &claude_home.join("projects").join("p").join("c1.jsonl"),
        &[
            claude("user", "Look at the okapi tests"),
            claude_call("t1", "Bash", json!({"command": "cargo test okapi"})),
            claude_result("t1", "test okapi ... ok"),
            claude("assistant", "The okapi tests pass"),
        ],
    );
    let (codex_id, codex_home) = fixture.agent("codex");
    record(
        &codex_home.join("sessions").join("2026").join("x1.jsonl"),
        &[
            codex("user", "Count okapi lines"),
            codex("assistant", "There are 4 okapi lines"),
        ],
    );
    assert!(fixture.index());
    fixture.assert_numbers_match_session("session/s1");
    fixture.assert_numbers_match_session("team");
    fixture.assert_numbers_match_session(&format!("agent-{claude_id}/c1"));
    fixture.assert_numbers_match_session(&format!("agent-{codex_id}/x1"));
    assert_eq!(
        fixture.chunks(&format!("agent-{claude_id}/c1")).len(),
        4,
        "tool call and output are indexed"
    );
}

/// DEC-105 D5: a record that grows between rounds gets only its new messages, numbered on
/// from the last, with nothing stored twice.
#[test]
fn a_record_appended_between_rounds_indexes_only_its_new_messages() {
    let mut fixture = Fixture::new();
    let (id, home) = fixture.agent("claude-code");
    let path = home.join("projects").join("p").join("grow.jsonl");
    record(
        &path,
        &[
            claude("user", "first okapi"),
            claude("assistant", "second okapi"),
        ],
    );
    assert!(fixture.index());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{}", claude("user", "third okapi")).unwrap();
    writeln!(file, "{}", claude("assistant", "fourth okapi")).unwrap();
    drop(file);
    assert!(fixture.index());
    let key = format!("agent-{id}/grow");
    let ordinals: Vec<i64> = fixture
        .chunks(&key)
        .into_iter()
        .map(|(ordinal, _)| ordinal)
        .collect();
    assert_eq!(ordinals, [0, 1, 2, 3]);
    fixture.assert_numbers_match_session(&key);
}

/// DEC-105 D5: a last line with no newline yet is never read; once the harness finishes it,
/// the next round indexes the whole message under the next number.
#[test]
fn a_half_written_last_line_waits_for_its_newline() {
    let mut fixture = Fixture::new();
    let (id, home) = fixture.agent("claude-code");
    let path = home.join("projects").join("p").join("half.jsonl");
    record(&path, &[claude("user", "whole okapi line")]);
    let rest = claude("assistant", "the okapi reply written in two parts");
    let (head, tail) = rest.split_at(rest.len() / 2);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    write!(file, "{head}").unwrap();
    file.flush().unwrap();
    fixture.index();
    let key = format!("agent-{id}/half");
    assert_eq!(fixture.chunks(&key).len(), 1);
    writeln!(file, "{tail}").unwrap();
    drop(file);
    fixture.index();
    assert_eq!(
        fixture.chunks(&key),
        [
            (0, "whole okapi line".to_owned()),
            (1, "the okapi reply written in two parts".to_owned())
        ]
    );
}

/// DEC-105 D5: a record replaced by another file under the same name (a new inode) drops
/// its chunks and is indexed again from its first message.
#[test]
fn a_replaced_record_is_indexed_again_from_the_start() {
    let mut fixture = Fixture::new();
    let (id, home) = fixture.agent("claude-code");
    let path = home.join("projects").join("p").join("swap.jsonl");
    record(
        &path,
        &[
            claude("user", "old okapi words"),
            claude("assistant", "old okapi reply"),
            claude("user", "old okapi tail"),
        ],
    );
    fixture.index();
    std::fs::remove_file(&path).unwrap();
    record(&path, &[claude("user", "new zebra words")]);
    fixture.index();
    let key = format!("agent-{id}/swap");
    assert_eq!(fixture.chunks(&key), [(0, "new zebra words".to_owned())]);
}

/// DEC-105 D5: a master turn is indexed only once its phase is final, so its reply is never
/// missed and later turns keep the numbers session() prints.
#[test]
fn a_running_master_turn_waits_until_it_ends() {
    let mut fixture = Fixture::new();
    fixture.turn("s2", 1, "First okapi ask", "First okapi answer", 1_000);
    fixture
        .db
        .execute(
            "INSERT INTO conversation_entries(event_cursor,project,kind,text) VALUES (5000,?1,'message','Second okapi ask')",
            [PROJECT],
        )
        .unwrap();
    fixture
        .db
        .execute(
            "INSERT INTO master_turns(session,ordinal,id,input,phase,body) VALUES ('s2',2,'turn-2',5000,'running','{}')",
            [],
        )
        .unwrap();
    fixture.index();
    assert_eq!(
        fixture.chunks("session/s2").len(),
        2,
        "only the finished turn"
    );
    fixture
        .db
        .execute(
            "UPDATE master_turns SET phase='succeeded', body=?1 WHERE session='s2' AND ordinal=2",
            [json!({"reply": {"text": "Second okapi answer"}}).to_string()],
        )
        .unwrap();
    fixture.index();
    assert_eq!(fixture.chunks("session/s2").len(), 4);
    fixture.assert_numbers_match_session("session/s2");
}

/// DEC-105 D4: no secret of a message or a tool's input or output is ever stored in the
/// index the embedding hook reads.
#[test]
fn the_index_never_stores_a_secret_from_a_message_or_tool_io() {
    let mut fixture = Fixture::new();
    let (id, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("p").join("secret.jsonl"),
        &[
            claude("user", "use password=hunter2okapi for the okapi db"),
            claude_call(
                "t9",
                "Bash",
                json!({"command": "curl -H 'Authorization: Bearer okapitoken0123456789abcdef'"}),
            ),
            claude_result("t9", "OPENAI_API_KEY=sk-okapi-0123456789abcdefghij"),
        ],
    );
    fixture.index();
    let stored: Vec<String> = fixture
        .chunks(&format!("agent-{id}/secret"))
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(stored.len(), 3, "{stored:?}");
    for secret in [
        "hunter2okapi",
        "okapitoken0123456789abcdef",
        "sk-okapi-0123456789abcdefghij",
    ] {
        assert!(
            stored.iter().all(|text| !text.contains(secret)),
            "{secret} in {stored:?}"
        );
    }
}

/// DEC-105 D8: a round stops at its chunk bound and says it is not caught up; the next
/// rounds go on from there until every message is indexed once, numbered in order.
#[test]
fn a_round_stops_at_its_bound_and_the_next_rounds_go_on() {
    let mut fixture = Fixture::new();
    let (id, home) = fixture.agent("claude-code");
    let lines: Vec<String> = (0..5)
        .map(|at| claude("user", &format!("okapi message {at}")))
        .collect();
    record(&home.join("projects").join("p").join("many.jsonl"), &lines);
    assert!(!fixture.index_within(2, 4 << 20));
    let caught: Vec<bool> = (0..3).map(|_| fixture.index_within(2, 4 << 20)).collect();
    assert_eq!(caught.last(), Some(&true));
    let ordinals: Vec<i64> = fixture
        .chunks(&format!("agent-{id}/many"))
        .into_iter()
        .map(|(ordinal, _)| ordinal)
        .collect();
    assert_eq!(ordinals, [0, 1, 2, 3, 4]);
    let marked: Option<i64> = fixture
        .db
        .query_row(
            "SELECT caught_up_ms FROM search_projects WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(marked, Some(INDEXED_AT));
}

/// DEC-105 D7: a conversation whose record is gone leaves the index on the next round.
#[test]
fn a_conversation_whose_record_is_gone_leaves_the_index() {
    let mut fixture = Fixture::new();
    let (id, home) = fixture.agent("claude-code");
    let path = home.join("projects").join("p").join("gone.jsonl");
    record(&path, &[claude("user", "soon gone okapi")]);
    fixture.index();
    let key = format!("agent-{id}/gone");
    assert_eq!(fixture.chunks(&key).len(), 1);
    std::fs::remove_file(&path).unwrap();
    fixture.index();
    assert!(fixture.chunks(&key).is_empty());
    let watermark: i64 = fixture
        .db
        .query_row(
            "SELECT COUNT(*) FROM search_sources WHERE key=?1",
            [key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(watermark, 0);
}

/// DEC-105 D7: chunks of a project that is gone are swept even when its removal never ran
/// the forget, and nothing is indexed for it again.
#[test]
fn a_project_that_is_gone_is_swept_from_the_index() {
    let mut fixture = Fixture::new();
    fixture.team_message("engine", "okapi note", 1_000);
    fixture.index();
    assert_eq!(fixture.chunks("team").len(), 1);
    fixture
        .db
        .execute("DELETE FROM projects WHERE id=?1", [PROJECT])
        .unwrap();
    crate::search_index::indexer::sweep_projects(&fixture.db).unwrap();
    fixture.index();
    assert!(fixture.chunks("team").is_empty());
}

/// DEC-105 D2: an indexed search ranks best first, names each hit by the number session()
/// reads, and tells the agent the hits are data.
#[test]
fn an_indexed_search_ranks_best_first_with_numbers_session_reads() {
    let mut fixture = Fixture::new();
    let (id, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("p").join("rank.jsonl"),
        &[
            claude("user", "an okapi once"),
            claude("assistant", "okapi okapi okapi, the okapi page"),
        ],
    );
    assert!(fixture.index());
    let found = fixture.sessions("okapi");
    assert!(found.contains("best matches first"), "{found}");
    assert!(found.contains(DATA_NOTE), "{found}");
    let hits = hit_lines(&found);
    let key = format!("agent-{id}/rank");
    assert!(hits[0].starts_with(&format!("- {key} #1 ")), "{found}");
    assert!(hits[1].starts_with(&format!("- {key} #0 ")), "{found}");
}

/// DEC-105 D2: scope stays live, so a conversation that left the listing since it was
/// indexed is never shown, though its chunks are still stored.
#[test]
fn an_indexed_search_never_shows_a_conversation_no_longer_listed() {
    let mut fixture = Fixture::new();
    let (id, home) = fixture.agent("claude-code");
    let path = home.join("projects").join("p").join("left.jsonl");
    record(&path, &[claude("user", "an okapi that leaves")]);
    fixture.team_message("engine", "an okapi that stays", 1_000);
    assert!(fixture.index());
    std::fs::remove_file(&path).unwrap();
    let found = fixture.sessions("okapi");
    assert!(!found.contains(&format!("agent-{id}/left")), "{found}");
    assert!(found.contains("- team #0"), "{found}");
}

/// DEC-105 D3: until the project's first full pass is done, sessions(query) scans as
/// before, newest conversations first.
#[test]
fn the_scan_answers_while_the_index_is_not_caught_up() {
    let mut fixture = Fixture::new();
    let (_, home) = fixture.agent("claude-code");
    let lines: Vec<String> = (0..5)
        .map(|at| claude("user", &format!("okapi message {at}")))
        .collect();
    record(&home.join("projects").join("p").join("slow.jsonl"), &lines);
    assert!(!fixture.index_within(2, 4 << 20));
    let found = fixture.sessions("okapi");
    assert!(found.contains("newest conversations first"), "{found}");
    assert_eq!(
        hit_lines(&found).len(),
        3,
        "the scan reads every message: {found}"
    );
}

/// DEC-105 accepted limit: a word inside another word is not an index token, so a caught-up
/// index with no hit hands the query to the scan, which still finds it.
#[test]
fn an_index_with_no_hit_hands_the_query_to_the_scan() {
    let mut fixture = Fixture::new();
    fixture.team_message("engine", "the okapi plan", 1_000);
    assert!(fixture.index());
    let found = fixture.sessions("kapi");
    assert!(found.contains("newest conversations first"), "{found}");
    assert_eq!(hit_lines(&found).len(), 1, "{found}");
}

/// DEC-105 D2 and D4: what the ranking hands back is reloaded with this project and source,
/// so another project's chunk id is never shown, and an excerpt is masked again on its way out.
#[test]
fn a_foreign_chunk_is_never_shown_and_an_excerpt_is_masked_again() {
    let mut fixture = Fixture::new();
    fixture.team_message("engine", "okapi plain", 1_000);
    fixture.team_message_of(OTHER, "engine", "okapi elsewhere", 1_000);
    assert!(fixture.index());
    crate::search_index::indexer::index_project(
        &mut fixture.db,
        &fixture.agents,
        &ProjectId::new(OTHER).unwrap(),
        INDEXED_AT,
    )
    .unwrap();
    fixture
        .db
        .execute(
            "INSERT INTO search_chunks(project,source,key,ordinal,text,at_ms) VALUES (?1,'session','team',7,'okapi password=hunter2okapi',1)",
            [PROJECT],
        )
        .unwrap();
    let foreign: i64 = fixture
        .db
        .query_row(
            "SELECT id FROM search_chunks WHERE project=?1",
            [OTHER],
            |row| row.get(0),
        )
        .unwrap();
    let mine: Vec<i64> = fixture
        .db
        .prepare("SELECT id FROM search_chunks WHERE project=?1 ORDER BY id")
        .unwrap()
        .query_map([PROJECT], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let found = conversations(&fixture.db, &project(), &fixture.agents);
    let ids: Vec<i64> = std::iter::once(foreign).chain(mine).collect();
    let hits = indexed_hits(
        &fixture.db,
        &project(),
        &found,
        &words("okapi"),
        &ids,
        usize::MAX,
    );
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert!(
        hits.iter().all(|hit| !hit.contains("elsewhere")),
        "{hits:?}"
    );
    assert!(
        hits.iter().all(|hit| !hit.contains("hunter2okapi")),
        "{hits:?}"
    );
}

/// Leader 14:34 (rv-v3code J1 note): a round stores what it read only inside one write
/// transaction that still finds its project, so a project removed while the round read its
/// messages keeps no chunk, watermark or pass mark.
#[test]
fn a_round_whose_project_is_removed_while_it_reads_stores_nothing() {
    let mut fixture = Fixture::new();
    fixture.team_message("engine", "okapi note", 1_000);
    let round = crate::search_index::indexer::read_round(
        &fixture.db,
        &fixture.agents,
        &project(),
        INDEXED_AT,
        256,
        4 << 20,
    )
    .unwrap();
    let tx = fixture.db.transaction().unwrap();
    tx.execute("DELETE FROM projects WHERE id=?1", [PROJECT])
        .unwrap();
    crate::search_index::forget_project(&tx, &project()).unwrap();
    tx.commit().unwrap();
    crate::search_index::indexer::store_round(&mut fixture.db, round, INDEXED_AT).unwrap();
    for table in ["search_chunks", "search_sources", "search_projects"] {
        let left: i64 = fixture
            .db
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE project=?1"),
                [PROJECT],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "{table}");
    }
}

/// Same note: the turns a round read from a master session deleted before the round stores
/// them are dropped with their watermark.
#[test]
fn a_round_drops_the_turns_of_a_session_deleted_while_it_reads() {
    let mut fixture = Fixture::new();
    fixture.turn("s3", 1, "okapi ask", "okapi answer", 1_000);
    let round = crate::search_index::indexer::read_round(
        &fixture.db,
        &fixture.agents,
        &project(),
        INDEXED_AT,
        256,
        4 << 20,
    )
    .unwrap();
    fixture
        .db
        .execute_batch("DELETE FROM master_turns WHERE session='s3'; DELETE FROM master_sessions WHERE id='s3';")
        .unwrap();
    crate::search_index::forget_session(&fixture.db, "s3").unwrap();
    crate::search_index::indexer::store_round(&mut fixture.db, round, INDEXED_AT).unwrap();
    assert!(fixture.chunks("session/s3").is_empty());
    let watermark: i64 = fixture
        .db
        .query_row(
            "SELECT COUNT(*) FROM search_sources WHERE key='session/s3'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(watermark, 0);
}

/// DEC-105 D8: a record line longer than the line bound and than a round's bytes is skipped
/// over the next rounds as session() skips it, and the message after it is indexed under
/// the number session() prints.
#[test]
fn a_line_longer_than_a_rounds_bytes_is_skipped_and_the_next_message_indexed() {
    let mut fixture = Fixture::new();
    let (id, home) = fixture.agent("claude-code");
    record(
        &home.join("projects").join("p").join("long.jsonl"),
        &[
            claude("user", "before the long okapi line"),
            claude("user", &"okapi ".repeat(1_000_000)),
            claude("assistant", "after the long okapi line"),
        ],
    );
    let bound = 2 * LINE_BYTES_MAX as u64 + 2;
    let caught = (0..8).any(|_| fixture.index_within(256, bound));
    assert!(caught, "the long line never stalls the record");
    let key = format!("agent-{id}/long");
    assert_eq!(
        fixture.chunks(&key),
        [
            (0, "before the long okapi line".to_owned()),
            (1, "after the long okapi line".to_owned())
        ]
    );
    fixture.assert_numbers_match_session(&key);
}

/// DEC-105 D1: one engine tick indexes every project's conversations, each under its own
/// project, and reports nothing left behind.
#[test]
fn one_engine_tick_indexes_every_project() {
    let mut fixture = Fixture::new();
    fixture.team_message("engine", "okapi here", 1_000);
    fixture.team_message_of(OTHER, "engine", "okapi there", 1_000);
    let root = fixture.agents.parent().unwrap().to_path_buf();
    let behind =
        crate::search_index::indexer::tick(&mut fixture.db, &root, &fixture.agents, INDEXED_AT)
            .unwrap();
    assert!(!behind);
    assert_eq!(fixture.chunks("team"), [(0, "okapi here".to_owned())]);
    let there: String = fixture
        .db
        .query_row(
            "SELECT text FROM search_chunks WHERE project=?1",
            [OTHER],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(there, "okapi there");
}
