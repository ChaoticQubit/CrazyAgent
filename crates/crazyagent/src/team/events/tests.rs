use super::*;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::terminal::FOLDER;
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

const PROJECT: &str = "team-events";
const OTHER: &str = "team-other";
const WAIT: Duration = Duration::from_secs(20);
const ASK: &str = r#"printf 'Working on the page\n\n Do you want to proceed?\n \342\235\257 1. Yes\n   2. No, and tell Claude what to do differently\n\n Esc to cancel\n'
IFS= read -r answer
printf '\033[2J\033[H'
printf 'answer:%s\n' "$answer" > "$1"
printf 'thanks\n'
exec sleep 60
"#;
const TICK: &str = r#"printf 'Working on the page\n\n Do you want to proceed?\n \342\235\257 1. Yes\n   2. No\n\n Esc to cancel\n'
i=0
while [ "$i" -lt 40 ]; do
  i=$((i+1))
  printf '\033[1;1Htick %s' "$i"
  sleep 0.05
done
: > "$1"
exec sleep 60
"#;
const LONG: &str = r#"i=0
while [ "$i" -lt 3000 ]; do
  i=$((i+1))
  printf 'line %s\n' "$i"
done
printf ' Do you want to proceed?\n \342\235\257 1. Yes\n   2. No\n'
: > "$1"
exec sleep 60
"#;

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    tmux: PathBuf,
    connection: Connection,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-events-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let tmux = installed(&base).expect("tmux must be installed on this machine");
        let connection = Connection::open(base.join("engine.db")).unwrap();
        super::super::initialize(&connection).unwrap();
        Self {
            _root: root,
            base,
            tmux,
            connection,
        }
    }

    fn host(&self) -> Arc<dyn Host> {
        Arc::new(Tmux::open(Some(self.tmux.clone()), &self.base.join(FOLDER)))
    }

    fn agent(&self, project: &str, role: &str, harness: &str, status: &str) -> i64 {
        self.connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,?2,?3,0,?4,1,1)",
                params![project, role, harness, status],
            )
            .unwrap();
        self.connection.last_insert_rowid()
    }

    fn task(&self, agent: i64, source: &str, status: &str, result: Option<&str>) -> i64 {
        self.connection
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,result,error,created_ms) VALUES (?1,?2,'codex',0,'Build the page',?3,?4,?5,?6,1)",
                params![
                    PROJECT,
                    source,
                    status,
                    agent,
                    result,
                    (status == "failed").then_some("The sub-agent could not finish the task")
                ],
            )
            .unwrap();
        self.connection.last_insert_rowid()
    }

    fn pane(&self, host: &Arc<dyn Host>, agent: i64, argv: &[&str]) {
        let command = InteractiveCommand {
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: self.base.clone(),
            env: Vec::new(),
            transcript: Transcript::Unavailable,
        };
        host.spawn(&AgentId::team(agent).unwrap(), &command)
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(self.base.join(FOLDER).join(SOCKET))
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_question_on_a_real_sub_agent_screen_is_recorded_once_and_its_typed_answer_reaches_the_app() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = fixture.agent(PROJECT, "sub", "codex", "working");
    let script = fixture.base.join("ask.sh");
    let answered = fixture.base.join("answered.txt");
    std::fs::write(&script, ASK).unwrap();
    fixture.pane(
        &host,
        agent,
        &[
            "/bin/sh",
            script.to_str().unwrap(),
            answered.to_str().unwrap(),
        ],
    );
    let watched = [Watched {
        agent,
        project: project(),
        task: Some(5),
    }];
    let mut watch = Watch::default();
    let asked = "Do you want to proceed? | 1. Yes | 2. No, and tell Claude what to do differently";
    until("the question event", || {
        watch
            .scan(&fixture.connection, host.as_ref(), &watched, 10)
            .unwrap();
        !events(&fixture.connection, &project(), 10)
            .unwrap()
            .is_empty()
    });
    watch
        .scan(&fixture.connection, host.as_ref(), &watched, 11)
        .unwrap();
    let recorded = events(&fixture.connection, &project(), 10).unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "a question still on screen is recorded once"
    );
    let event = &recorded[0];
    assert_eq!(event.kind, EventKind::Question);
    assert_eq!(event.text, asked);
    assert_eq!(event.task, Some(5));
    assert_eq!(event.state, EventState::Queued);
    assert_eq!(event.label, format!("Sub-agent {agent}"));
    let shown = status(&fixture.connection, &project()).unwrap();
    assert_eq!(shown.agents.len(), 1);
    assert_eq!(shown.agents[0].waiting.as_deref(), Some(asked));
    assert!(shown.text().contains(&format!(
        "Sub-agent {agent} (codex): Working, waiting on: {asked}"
    )));

    assert_eq!(
        send_to_agent(
            &fixture.connection,
            host.as_ref(),
            &project(),
            &format!("codex-{agent}"),
            "1"
        )
        .unwrap(),
        Ok(agent)
    );
    until("the typed answer", || {
        std::fs::read_to_string(&answered).is_ok_and(|text| text == "answer:1\n")
    });
    assert!(waiting(&fixture.connection, &project()).unwrap().is_empty());
    until("the question to leave the screen", || {
        watch
            .scan(&fixture.connection, host.as_ref(), &watched, 12)
            .unwrap();
        watch.seen.is_empty()
    });
    assert_eq!(
        events(&fixture.connection, &project(), 10).unwrap().len(),
        1
    );
    assert!(waiting(&fixture.connection, &project()).unwrap().is_empty());
}

#[test]
fn a_question_on_a_screen_that_still_changes_is_recorded_only_once_the_pane_is_idle_for_a_scan() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = fixture.agent(PROJECT, "sub", "codex", "working");
    let script = fixture.base.join("tick.sh");
    let still = fixture.base.join("still");
    std::fs::write(&script, TICK).unwrap();
    fixture.pane(
        &host,
        agent,
        &["/bin/sh", script.to_str().unwrap(), still.to_str().unwrap()],
    );
    let watched = [Watched {
        agent,
        project: project(),
        task: None,
    }];
    let name = AgentId::team(agent).unwrap();
    let mut watch = Watch::default();
    let mut busy = 0;
    until("the pane to stop changing", || {
        watch
            .scan(&fixture.connection, host.as_ref(), &watched, 10)
            .unwrap();
        if still.exists() {
            return true;
        }
        if question(&host.screen(&name).unwrap()).is_some() {
            busy += 1;
        }
        assert!(
            events(&fixture.connection, &project(), 10)
                .unwrap()
                .is_empty(),
            "a question on a screen that is still changing is not recorded"
        );
        assert!(waiting(&fixture.connection, &project()).unwrap().is_empty());
        std::thread::sleep(Duration::from_millis(150));
        false
    });
    assert!(
        busy >= 3,
        "the question was on the changing screen for {busy} scans"
    );
    until("the question event", || {
        watch
            .scan(&fixture.connection, host.as_ref(), &watched, 11)
            .unwrap();
        !events(&fixture.connection, &project(), 10)
            .unwrap()
            .is_empty()
    });
    let recorded = events(&fixture.connection, &project(), 10).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].kind, EventKind::Question);
    assert_eq!(recorded[0].text, "Do you want to proceed? | 1. Yes | 2. No");
}

#[test]
fn the_watch_reads_only_the_visible_screen_of_an_agent_with_long_scrollback_and_opens_no_viewer() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = fixture.agent(PROJECT, "sub", "codex", "working");
    let script = fixture.base.join("long.sh");
    let printed = fixture.base.join("printed");
    std::fs::write(&script, LONG).unwrap();
    fixture.pane(
        &host,
        agent,
        &[
            "/bin/sh",
            script.to_str().unwrap(),
            printed.to_str().unwrap(),
        ],
    );
    until("the long output", || printed.exists());
    let name = AgentId::team(agent).unwrap();
    let viewers: Vec<crate::terminal::TerminalStream> = (0..crate::terminal::VIEWERS_MAX)
        .map(|_| host.attach(&name).unwrap())
        .collect();
    assert!(
        screen_rows(&viewers[0].history).len() > 2000,
        "the pane keeps a long scrollback"
    );
    assert!(
        matches!(host.attach(&name), Err(problem) if problem.0 == "terminal_viewers_full"),
        "every viewer slot is taken"
    );
    let rows = usize::from(viewers[0].rows);
    let watched = [Watched {
        agent,
        project: project(),
        task: None,
    }];
    let mut watch = Watch::default();
    until("the question event", || {
        watch
            .scan(&fixture.connection, host.as_ref(), &watched, 10)
            .unwrap();
        !events(&fixture.connection, &project(), 10)
            .unwrap()
            .is_empty()
    });
    let screen = host.screen(&name).unwrap();
    let shown = screen_rows(&screen);
    assert!(
        shown.len() <= rows,
        "{} rows read of a {rows} row pane",
        shown.len()
    );
    assert!(shown.iter().any(|row| row == "line 3000"));
    assert!(!shown.iter().any(|row| row == "line 1"));
    assert_eq!(
        events(&fixture.connection, &project(), 10).unwrap()[0].text,
        "Do you want to proceed? | 1. Yes | 2. No"
    );
    drop(viewers);
}

#[test]
fn the_live_master_tool_handlers_type_into_a_named_sub_agent_and_report_the_team() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = fixture.agent(PROJECT, "sub", "codex", "working");
    let script = fixture.base.join("ask.sh");
    let answered = fixture.base.join("answered.txt");
    std::fs::write(&script, ASK).unwrap();
    fixture.pane(
        &host,
        agent,
        &[
            "/bin/sh",
            script.to_str().unwrap(),
            answered.to_str().unwrap(),
        ],
    );
    let name = AgentId::team(agent).unwrap();
    until("the question on screen", || {
        question(&host.screen(&name).unwrap()).is_some()
    });
    assert_eq!(
        (HANDLERS.answer)(
            &fixture.connection,
            host.as_ref(),
            &project(),
            "Sub-agent 999",
            "1"
        ),
        Err("Not sent: no live sub-agent of this project has that name.".to_owned())
    );
    assert_eq!(
        (HANDLERS.answer)(
            &fixture.connection,
            host.as_ref(),
            &project(),
            &format!("Sub-agent {agent}"),
            "1"
        ),
        Ok(format!("Typed into Sub-agent {agent}."))
    );
    until("the typed answer", || {
        std::fs::read_to_string(&answered).is_ok_and(|text| text == "answer:1\n")
    });
    let told = (HANDLERS.status)(&fixture.connection, &project()).unwrap();
    assert!(
        told.contains(&format!("Sub-agent {agent} (codex): Working")),
        "{told}"
    );
}

#[test]
fn a_watched_agent_that_stops_being_busy_loses_its_waiting_mark() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = fixture.agent(PROJECT, "sub", "codex", "working");
    let script = fixture.base.join("ask.sh");
    std::fs::write(&script, ASK).unwrap();
    fixture.pane(
        &host,
        agent,
        &["/bin/sh", script.to_str().unwrap(), "/dev/null"],
    );
    let watched = [Watched {
        agent,
        project: project(),
        task: None,
    }];
    let mut watch = Watch::default();
    until("the waiting mark", || {
        watch
            .scan(&fixture.connection, host.as_ref(), &watched, 10)
            .unwrap();
        !waiting(&fixture.connection, &project()).unwrap().is_empty()
    });
    watch
        .scan(&fixture.connection, host.as_ref(), &[], 11)
        .unwrap();
    assert!(watch.seen.is_empty());
    assert!(waiting(&fixture.connection, &project()).unwrap().is_empty());
    assert!(watch.due());
    assert!(!watch.due(), "a second scan waits for SCAN_EVERY");
}

#[test]
fn choice_prompts_of_claude_code_and_codex_are_questions_and_plain_lists_are_not() {
    let claude = "\x1b[0m\x1b[H\x1b[2J\x1b]0;claude\x07\u{256d}\u{2500}\u{2500}\u{256e}\r\n\u{2502} Bash command                    \u{2502}\r\n\u{2502}   cargo test                    \u{2502}\r\n\u{2502} Do you want to proceed?         \u{2502}\r\n\u{2502} \x1b[36m\u{276f} 1. Yes\x1b[0m                      \u{2502}\r\n\u{2502}   2. Yes, and don't ask again   \u{2502}\r\n\u{2502}   3. No, and tell Claude what to do differently (esc) \u{2502}\r\n\u{2570}\u{2500}\u{2500}\u{256f}\x1b[0m\x1b[9;3H";
    assert_eq!(
        question(claude.as_bytes()).as_deref(),
        Some(
            "Do you want to proceed? | 1. Yes | 2. Yes, and don't ask again | 3. No, and tell Claude what to do differently (esc)"
        )
    );
    let codex = "Would you like to run the following command?\r\n\r\n  $ cargo test -p crazyagent\r\n\r\n\x1b[1m\u{203a} 1. Yes, proceed (y)\x1b[22m\r\n  2. Yes, and don't ask again for this command (a)\r\n  3. No, and tell Codex what to do differently (esc)\r\n\r\n  Press enter to confirm or esc to cancel";
    assert_eq!(
        question(codex.as_bytes()).as_deref(),
        Some(
            "Would you like to run the following command? | 1. Yes, proceed (y) | 2. Yes, and don't ask again for this command (a) | 3. No, and tell Codex what to do differently (esc)"
        )
    );
    let picked = "Which layout should the page use?\r\n  1. Grid\r\n     Cards in rows\r\n\u{276f} 2. List\r\n     One column\r\n  3. Type something.\r\nEnter to select";
    assert_eq!(
        question(picked.as_bytes()).as_deref(),
        Some("Which layout should the page use? | 1. Grid | 2. List | 3. Type something.")
    );
    for plain in [
        "What next?\r\n1. Add tests\r\n2. Ship it",
        "Continue?\r\n\u{276f} 1. Yes",
        "",
        "\x1b[2J",
        "Steps done:\r\n\u{276f} 2. Second\r\n  3. Third",
    ] {
        assert_eq!(question(plain.as_bytes()), None, "{plain:?}");
    }
    let long = format!(
        "{}?\r\n\u{276f} 1. Yes\r\n  2. No",
        "Should I go on ".repeat(200)
    );
    let bounded = question(long.as_bytes()).unwrap();
    assert!(bounded.len() <= EVENT_TEXT_BYTES_MAX);
    assert_eq!(
        screen_rows(b"a\x1b[31mb\x1b[0m\r\nc\x07d  \r\n\x1b]2;t\x1b\\e"),
        ["ab", "cd", "e"]
    );
}

#[test]
fn a_master_reaches_only_its_own_live_sub_agents_by_name() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let live = fixture.agent(PROJECT, "sub", "codex", "working");
    let master = fixture.agent(PROJECT, "master", "codex", "idle");
    let stopped = fixture.agent(PROJECT, "sub", "codex", "stopped");
    let foreign = fixture.agent(OTHER, "sub", "codex", "idle");
    let paneless = fixture.agent(PROJECT, "sub", "codex", "idle");
    fixture.pane(&host, live, &["/bin/sh", "-c", "exec cat"]);
    for name in [
        live.to_string(),
        format!("team-{live}"),
        format!("codex-{live}"),
        format!("@codex-{live}"),
        format!("Sub-agent {live}"),
        format!("sub-agent {live}"),
    ] {
        assert_eq!(
            resolve(&fixture.connection, &project(), &name).unwrap(),
            Some(live),
            "{name}"
        );
    }
    for name in [
        format!("0{live}"),
        format!("claude-code-{live}"),
        format!("codex-{master}"),
        format!("codex-{foreign}"),
        "-1".to_owned(),
        "codex".to_owned(),
        String::new(),
    ] {
        assert_eq!(
            resolve(&fixture.connection, &project(), &name).unwrap(),
            None,
            "{name}"
        );
    }
    let persona =
        super::super::names::assign_persona(&fixture.connection, &project(), live).unwrap();
    assert_eq!(label(&fixture.connection, live).unwrap(), persona);
    assert_eq!(
        resolve(&fixture.connection, &project(), &persona.to_lowercase()).unwrap(),
        Some(live)
    );
    assert_eq!(
        resolve(&fixture.connection, &project(), "@master").unwrap(),
        None,
        "the master is not a sub-agent to send to"
    );
    let send = |agent: String, text: &str| {
        send_to_agent(&fixture.connection, host.as_ref(), &project(), &agent, text).unwrap()
    };
    assert_eq!(send(live.to_string(), "  \n"), Err("team_message_invalid"));
    assert_eq!(
        send(live.to_string(), &"x".repeat(SEND_BYTES_MAX + 1)),
        Err("team_message_invalid")
    );
    assert_eq!(send(master.to_string(), "hi"), Err("team_agent_not_found"));
    assert_eq!(send(foreign.to_string(), "hi"), Err("team_agent_not_found"));
    assert_eq!(
        send(stopped.to_string(), "hi"),
        Err("team_agent_not_running")
    );
    assert_eq!(
        send(paneless.to_string(), "hi"),
        Err("team_agent_not_running")
    );
    assert_eq!(send(live.to_string(), "hi"), Ok(live));
}

#[test]
fn a_task_end_records_finished_or_failed_whatever_its_last_line_asks_and_master_delegations_are_carried()
 {
    let fixture = Fixture::new();
    let agent = fixture.agent(PROJECT, "sub", "codex", "working");
    let done = fixture.task(agent, "owner", "done", Some("Built the page."));
    let asks = fixture.task(
        agent,
        "owner",
        "done",
        Some("Built the page.\nShould I also add tests?\n"),
    );
    let failed = fixture.task(agent, "owner", "failed", None);
    let delegated = fixture.task(agent, "master", "done", Some("Done."));
    let cancelled = fixture.task(agent, "owner", "cancelled", None);
    let queued = fixture.task(agent, "owner", "queued", None);
    fixture
        .connection
        .execute(
            "INSERT INTO team_event_waiting(agent,project,text,since_ms) VALUES (?1,?2,'Proceed?',1)",
            params![agent, PROJECT],
        )
        .unwrap();
    for task in [done, asks, failed, delegated] {
        assert!(task_ended(&fixture.connection, task, 20).unwrap().is_some());
    }
    for task in [cancelled, queued, 9999] {
        assert_eq!(task_ended(&fixture.connection, task, 20).unwrap(), None);
    }
    assert!(waiting(&fixture.connection, &project()).unwrap().is_empty());
    let recorded: Vec<(EventKind, String, EventState, Option<i64>)> =
        events(&fixture.connection, &project(), 10)
            .unwrap()
            .into_iter()
            .map(|event| (event.kind, event.text, event.state, event.task))
            .collect();
    assert_eq!(
        recorded,
        [
            (
                EventKind::Finished,
                "Built the page.".to_owned(),
                EventState::Queued,
                Some(done)
            ),
            (
                EventKind::Finished,
                "Built the page. Should I also add tests?".to_owned(),
                EventState::Queued,
                Some(asks)
            ),
            (
                EventKind::Failed,
                "The sub-agent could not finish the task".to_owned(),
                EventState::Queued,
                Some(failed)
            ),
            (
                EventKind::Finished,
                "Done.".to_owned(),
                EventState::Carried,
                Some(delegated)
            ),
        ]
    );
}

#[test]
fn records_are_bounded_one_line_and_only_for_the_project_s_own_agents() {
    let fixture = Fixture::new();
    let agent = fixture.agent(PROJECT, "sub", "codex", "working");
    let foreign = fixture.agent(OTHER, "sub", "codex", "working");
    let record_text = |agent: i64, text: &str| {
        record(
            &fixture.connection,
            &project(),
            agent,
            None,
            EventKind::Progress,
            text,
            1,
        )
        .unwrap()
    };
    assert_eq!(record_text(agent, " \n\t "), None);
    assert_eq!(record_text(foreign, "hi"), None);
    let id = record_text(
        agent,
        &format!("a\nb\x07\t{}", "é".repeat(EVENT_TEXT_BYTES_MAX)),
    )
    .unwrap();
    let text: String = fixture
        .connection
        .query_row("SELECT text FROM team_events WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(text.starts_with("a b "));
    assert!(text.len() <= EVENT_TEXT_BYTES_MAX && !text.contains(['\n', '\x07', '\t']));
    assert_eq!(
        record_text(agent, "newer"),
        Some(id),
        "a newer progress replaces the queued one"
    );
    let queued: Vec<(i64, String)> = events(&fixture.connection, &project(), 10)
        .unwrap()
        .into_iter()
        .map(|event| (event.id, event.text))
        .collect();
    assert_eq!(queued, [(id, "newer".to_owned())]);
}

fn queued_kinds(connection: &Connection) -> Vec<(i64, EventKind)> {
    let mut statement = connection
        .prepare(
            "SELECT agent,kind FROM team_events WHERE project=?1 AND state='queued' ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([PROJECT], |row| {
            Ok((
                row.get(0)?,
                EventKind::parse(&row.get::<_, String>(1)?).unwrap(),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn dropped(connection: &Connection) -> Vec<(i64, EventKind)> {
    let mut statement = connection
        .prepare(
            "SELECT agent,kind FROM team_events WHERE project=?1 AND state='dropped' ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([PROJECT], |row| {
            Ok((
                row.get(0)?,
                EventKind::parse(&row.get::<_, String>(1)?).unwrap(),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn queue_notices(connection: &Connection) -> usize {
    notifications(connection, Some(&project()), 100)
        .unwrap()
        .iter()
        .filter(|note| note.kind == NotificationKind::QueueFull)
        .count()
}

#[test]
fn a_flood_of_questions_from_one_agent_keeps_its_latest_and_another_agent_s_failure_still_reaches_the_master()
 {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let ada = fixture.agent(PROJECT, "sub", "codex", "working");
    let bo = fixture.agent(PROJECT, "sub", "claude-code", "working");
    let add = |agent: i64, kind: EventKind, text: &str, at: i64| {
        record(connection, &project(), agent, None, kind, text, at).unwrap()
    };
    let asked = add(ada, EventKind::Question, "Question 0?", 1).unwrap();
    for n in 1..2 * EVENTS_QUEUED_MAX {
        assert_eq!(
            add(ada, EventKind::Question, &format!("Question {n}?"), 1 + n),
            Some(asked)
        );
        add(ada, EventKind::Progress, &format!("Step {n}"), 1 + n).unwrap();
    }
    assert_eq!(
        queued_kinds(connection),
        [(ada, EventKind::Question), (ada, EventKind::Progress)]
    );
    for n in 0..EVENTS_QUEUED_MAX + EVENTS_QUEUED_PER_AGENT_MAX {
        let id = add(
            ada,
            EventKind::Finished,
            &format!("Task {n} done."),
            1_000 + n,
        );
        if n < EVENTS_QUEUED_MAX - 2 {
            assert!(
                id.is_some(),
                "finished {n} queues while the project has room"
            );
        }
    }
    let ada_queued = queued_kinds(connection).len() as i64;
    assert!((EVENTS_QUEUED_MAX..=EVENTS_QUEUED_MAX + 1).contains(&ada_queued));
    assert!(
        dropped(connection).iter().all(|(agent, kind)| *agent == ada
            && matches!(kind, EventKind::Question | EventKind::Progress))
    );
    assert_eq!(queue_notices(connection), 1);
    let failed = add(bo, EventKind::Failed, "Tests failed.", 5_000).unwrap();
    assert_eq!(
        queue_notices(connection),
        1,
        "the owner is told once while unseen"
    );
    let mut delivered = Vec::new();
    for at in 0..EVENTS_QUEUED_HARD_MAX {
        if !deliver_by_message(connection, &project(), true, 6_000 + at).unwrap() {
            break;
        }
        delivered.extend(wakes(connection).last().unwrap().lines().map(str::to_owned));
        answer_all(connection);
    }
    assert_eq!(
        delivered.last().unwrap(),
        &format!("[team] Sub-agent {bo} failed: Tests failed.")
    );
    assert!(
        events(connection, &project(), 10)
            .unwrap()
            .iter()
            .any(|event| event.id == failed && event.state == EventState::Delivered)
    );
    assert!(queued_kinds(connection).is_empty());
}

#[test]
fn a_full_queue_drops_the_oldest_progress_then_question_and_never_a_finished_failed_or_change_set_event()
 {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let add = |agent: i64, kind: EventKind, text: &str| {
        record(connection, &project(), agent, None, kind, text, 1).unwrap()
    };
    let old = fixture.agent(PROJECT, "sub", "codex", "working");
    add(old, EventKind::Progress, "Old step").unwrap();
    add(old, EventKind::Question, "Old question?").unwrap();
    let fillers: Vec<i64> = (0..4)
        .map(|_| fixture.agent(PROJECT, "sub", "codex", "working"))
        .collect();
    for filler in &fillers {
        for n in 0..EVENTS_QUEUED_PER_AGENT_MAX - 1 {
            add(*filler, EventKind::Finished, &format!("Task {n} done.")).unwrap();
        }
    }
    let bo = fixture.agent(PROJECT, "sub", "codex", "working");
    add(bo, EventKind::Progress, "Bo step").unwrap();
    add(bo, EventKind::Question, "Bo question?").unwrap();
    assert_eq!(queued_kinds(connection).len() as i64, EVENTS_QUEUED_MAX);
    assert!(dropped(connection).is_empty());

    let cy = fixture.agent(PROJECT, "sub", "codex", "working");
    add(cy, EventKind::Failed, "Tests failed.").unwrap();
    add(cy, EventKind::ChangeSetReady, "Edits wait in Changes.").unwrap();
    assert_eq!(
        dropped(connection),
        [(old, EventKind::Progress), (bo, EventKind::Progress)]
    );
    add(cy, EventKind::Question, "Cy question?").unwrap();
    assert_eq!(
        dropped(connection),
        [
            (old, EventKind::Progress),
            (old, EventKind::Question),
            (bo, EventKind::Progress)
        ]
    );
    assert_eq!(queue_notices(connection), 0);
    assert_eq!(
        add(cy, EventKind::Progress, "Cy step"),
        None,
        "progress never displaces a question"
    );
    assert_eq!(queue_notices(connection), 1);
    assert_eq!(add(bo, EventKind::Progress, "Bo step again"), None);
    assert_eq!(queue_notices(connection), 1, "one notice while unseen");
    assert_eq!(queued_kinds(connection).len() as i64, EVENTS_QUEUED_MAX);

    let mut important = 0;
    let mut agents = Vec::new();
    while queued_kinds(connection).len() < EVENTS_QUEUED_HARD_MAX as usize {
        if important % (EVENTS_QUEUED_PER_AGENT_MAX - 1) == 0 {
            agents.push(fixture.agent(PROJECT, "sub", "codex", "working"));
        }
        let kind = [
            EventKind::Finished,
            EventKind::Failed,
            EventKind::ChangeSetReady,
        ][important as usize % 3];
        assert!(
            add(
                *agents.last().unwrap(),
                kind,
                &format!("Important {important}")
            )
            .is_some(),
            "important event {important} queues under the hard bound"
        );
        important += 1;
    }
    assert!(
        queued_kinds(connection)
            .iter()
            .all(|(_, kind)| !kind.replaceable()),
        "questions are dropped before the queue passes its bound"
    );
    let dz = fixture.agent(PROJECT, "sub", "codex", "working");
    assert_eq!(add(dz, EventKind::Failed, "One too many."), None);
    assert_eq!(queue_notices(connection), 1);
    assert!(
        dropped(connection)
            .iter()
            .all(|(_, kind)| kind.replaceable())
    );
    assert_eq!(
        queued_kinds(connection).len() as i64,
        EVENTS_QUEUED_HARD_MAX,
        "the queue stays bounded"
    );
}

#[test]
fn an_escalation_is_an_unseen_notification_until_the_owner_sees_it() {
    let fixture = Fixture::new();
    let other = ProjectId::new(OTHER).unwrap();
    assert_eq!(
        escalate(&fixture.connection, &project(), " ", 1).unwrap(),
        Err("team_escalation_invalid")
    );
    assert_eq!(
        escalate(
            &fixture.connection,
            &project(),
            &"x".repeat(ESCALATION_BYTES_MAX + 1),
            1
        )
        .unwrap(),
        Err("team_escalation_invalid")
    );
    let first = escalate(
        &fixture.connection,
        &project(),
        "Delete the old release branch?",
        5,
    )
    .unwrap()
    .unwrap();
    let second = escalate(&fixture.connection, &other, "Spend on a new provider?", 6)
        .unwrap()
        .unwrap();
    let all = notifications(&fixture.connection, None, 10).unwrap();
    assert_eq!(
        all.iter().map(|n| n.id).collect::<Vec<_>>(),
        [second, first]
    );
    assert_eq!(all[1].kind, NotificationKind::Escalation);
    assert_eq!(all[1].text, "Delete the old release branch?");
    assert!(all.iter().all(|n| n.seen_ms.is_none()));
    assert_eq!(
        seen(&fixture.connection, Some(&project()), second, 9).unwrap(),
        1
    );
    let mine = notifications(&fixture.connection, Some(&project()), 10).unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].seen_ms, Some(9));
    assert!(
        notifications(&fixture.connection, Some(&other), 10).unwrap()[0]
            .seen_ms
            .is_none()
    );
    for at in 0..NOTIFICATIONS_PER_PROJECT_MAX {
        escalate(&fixture.connection, &project(), "again", 10 + at)
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        notifications(&fixture.connection, Some(&project()), 1000)
            .unwrap()
            .len() as i64,
        NOTIFICATIONS_PER_PROJECT_MAX
    );
}

fn wakes(connection: &Connection) -> Vec<String> {
    let mut statement = connection
        .prepare("SELECT text FROM team_messages WHERE project=?1 AND author='engine' ORDER BY id")
        .unwrap();
    statement
        .query_map([PROJECT], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn answer_all(connection: &Connection) {
    connection
        .execute(
            "UPDATE team_messages SET state='answered' WHERE state IN ('pending','sent')",
            [],
        )
        .unwrap();
}

fn event_states(connection: &Connection) -> Vec<(EventState, Option<i64>)> {
    events(connection, &project(), 100)
        .unwrap()
        .into_iter()
        .map(|event| (event.state, event.delivered_ms))
        .collect()
}

#[test]
fn queued_events_wake_a_free_master_in_the_order_they_happened() {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let ada = fixture.agent(PROJECT, "sub", "codex", "working");
    let bo = fixture.agent(PROJECT, "sub", "claude-code", "working");
    let add = |agent: i64, task: Option<i64>, kind: EventKind, text: &str, at: i64| {
        record(connection, &project(), agent, task, kind, text, at)
            .unwrap()
            .unwrap()
    };
    add(
        ada,
        Some(3),
        EventKind::Question,
        "Proceed? | 1. Yes | 2. No",
        10,
    );
    add(bo, Some(4), EventKind::Finished, "Built the page.", 11);
    add(
        ada,
        None,
        EventKind::ChangeSetReady,
        "Edits wait in Changes.",
        12,
    );
    assert!(deliver_by_message(connection, &project(), true, 100).unwrap());
    assert_eq!(
        wakes(connection),
        [format!(
            "[team] Sub-agent {ada} asks: Proceed? | 1. Yes | 2. No [task 3]\n[team] Sub-agent {bo} finished: Built the page. [task 4]\n[team] Sub-agent {ada} has changes ready: Edits wait in Changes."
        )]
    );
    assert_eq!(
        event_states(connection),
        [(EventState::Delivered, Some(100)); 3]
    );
    add(bo, Some(5), EventKind::Failed, "Tests failed.", 101);
    assert!(
        !deliver_by_message(connection, &project(), true, 102).unwrap(),
        "nothing is typed while the master still has a message waiting"
    );
    answer_all(connection);
    assert!(deliver_by_message(connection, &project(), true, 103).unwrap());
    assert_eq!(
        wakes(connection).last().unwrap(),
        &format!("[team] Sub-agent {bo} failed: Tests failed. [task 5]")
    );
    answer_all(connection);
    assert!(!deliver_by_message(connection, &project(), true, 104).unwrap());
    let many: Vec<i64> = (0..EVENTS_PER_WAKE_MAX + 3)
        .map(|at| add(ada, None, EventKind::Finished, "x", 200 + at as i64))
        .collect();
    assert!(deliver_by_message(connection, &project(), true, 300).unwrap());
    assert_eq!(
        wakes(connection).last().unwrap().lines().count(),
        EVENTS_PER_WAKE_MAX
    );
    answer_all(connection);
    assert!(deliver_by_message(connection, &project(), true, 301).unwrap());
    assert_eq!(wakes(connection).last().unwrap().lines().count(), 3);
    assert!(
        events(connection, &project(), 100)
            .unwrap()
            .iter()
            .filter(|event| many.contains(&event.id))
            .all(|event| event.state == EventState::Delivered)
    );
}

#[test]
fn with_no_live_master_only_a_question_starts_one() {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let ada = fixture.agent(PROJECT, "sub", "codex", "working");
    record(
        connection,
        &project(),
        ada,
        None,
        EventKind::Finished,
        "Done.",
        1,
    )
    .unwrap();
    assert!(!deliver_by_message(connection, &project(), false, 2).unwrap());
    assert_eq!(event_states(connection), [(EventState::Queued, None)]);
    record(
        connection,
        &project(),
        ada,
        None,
        EventKind::Question,
        "Proceed?",
        3,
    )
    .unwrap();
    assert!(deliver_by_message(connection, &project(), false, 4).unwrap());
    assert_eq!(
        wakes(connection),
        [format!(
            "[team] Sub-agent {ada} finished: Done.\n[team] Sub-agent {ada} asks: Proceed?"
        )]
    );
}

#[test]
fn with_finished_muted_a_result_ending_in_a_question_starts_no_master() {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let ada = fixture.agent(PROJECT, "sub", "codex", "working");
    set_rule(
        connection,
        &RuleTarget::Project {
            project_id: project(),
        },
        Some(&WakeRule {
            finished: false,
            ..WakeRule::default()
        }),
    )
    .unwrap()
    .unwrap();
    let task = fixture.task(
        ada,
        "owner",
        "done",
        Some("Built the page.\nShould I also add tests?"),
    );
    assert!(task_ended(connection, task, 1).unwrap().is_some());
    assert!(questions(connection, &project()).unwrap().is_empty());
    assert!(!deliver_by_message(connection, &project(), false, 2).unwrap());
    assert!(!deliver_by_message(connection, &project(), true, 3).unwrap());
    assert!(wakes(connection).is_empty(), "no master turn was started");
    assert_eq!(event_states(connection), [(EventState::Muted, None)]);
}

#[test]
fn a_question_muted_by_the_rule_starts_no_master_for_the_other_events() {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let ada = fixture.agent(PROJECT, "sub", "codex", "working");
    set_rule(
        connection,
        &RuleTarget::Project {
            project_id: project(),
        },
        Some(&WakeRule {
            question: false,
            ..WakeRule::default()
        }),
    )
    .unwrap()
    .unwrap();
    for (kind, text) in [
        (EventKind::Finished, "Done."),
        (EventKind::Question, "Proceed?"),
    ] {
        record(connection, &project(), ada, None, kind, text, 1)
            .unwrap()
            .unwrap();
    }
    assert!(!deliver_by_message(connection, &project(), false, 2).unwrap());
    assert!(wakes(connection).is_empty());
    assert!(deliver_by_message(connection, &project(), true, 3).unwrap());
    assert_eq!(
        wakes(connection),
        [format!("[team] Sub-agent {ada} finished: Done.")]
    );
}

#[test]
fn wakes_that_start_a_master_that_is_not_running_have_their_own_smaller_cap() {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let ada = fixture.agent(PROJECT, "sub", "codex", "working");
    let ask = |n: u32, at: i64| {
        record(
            connection,
            &project(),
            ada,
            None,
            EventKind::Question,
            &format!("Question {n}?"),
            at,
        )
        .unwrap()
        .unwrap()
    };
    let start = 1_000;
    for n in 0..COLD_WAKES_PER_HOUR_MAX {
        let at = start + i64::from(n) * 10;
        ask(n, at);
        assert!(deliver_by_message(connection, &project(), false, at + 1).unwrap());
        answer_all(connection);
    }
    ask(99, start + 100);
    for at in [start + 101, start + 102] {
        assert!(!deliver_by_message(connection, &project(), false, at).unwrap());
    }
    let told = notifications(connection, Some(&project()), 10).unwrap();
    assert_eq!(told.len(), 1, "the owner hears of the cold cap once");
    assert_eq!(told[0].kind, NotificationKind::WakePaused);
    assert!(told[0].text.contains(&format!(
        "after {COLD_WAKES_PER_HOUR_MAX} starts in the last hour"
    )));
    assert!(
        !status(connection, &project()).unwrap().paused,
        "a running master still wakes"
    );
    assert!(deliver_by_message(connection, &project(), true, start + 103).unwrap());
    assert!(
        wakes(connection)
            .last()
            .unwrap()
            .ends_with("asks: Question 99?")
    );
    answer_all(connection);
    ask(100, start + 200);
    assert!(!deliver_by_message(connection, &project(), false, start + 201).unwrap());
    assert_eq!(
        notifications(connection, Some(&project()), 10)
            .unwrap()
            .len(),
        2,
        "a new cold pause after the running master's wake is told again"
    );
    assert!(
        !deliver_by_message(connection, &project(), false, start + 1 + HOUR_MS).unwrap(),
        "the first cold start is still inside the hour"
    );
    assert!(deliver_by_message(connection, &project(), false, start + 2 + HOUR_MS).unwrap());
    assert!(
        wakes(connection)
            .last()
            .unwrap()
            .ends_with("asks: Question 100?")
    );
}

#[test]
fn a_wake_table_made_before_cold_starts_gains_the_column_and_counts_them() {
    let root = tempfile::tempdir().unwrap();
    let connection = Connection::open(root.path().join("engine.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE team_event_wakes (id INTEGER PRIMARY KEY AUTOINCREMENT, project TEXT NOT NULL, at_ms INTEGER NOT NULL);
             INSERT INTO team_event_wakes(project,at_ms) VALUES ('team-events',1);",
        )
        .unwrap();
    super::super::initialize(&connection).unwrap();
    super::super::initialize(&connection).unwrap();
    let ada: i64 = {
        connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,'sub','codex',0,'working',1,1)",
                [PROJECT],
            )
            .unwrap();
        connection.last_insert_rowid()
    };
    record(
        &connection,
        &project(),
        ada,
        None,
        EventKind::Question,
        "Proceed?",
        2,
    )
    .unwrap()
    .unwrap();
    assert!(deliver_by_message(&connection, &project(), false, 3).unwrap());
    let rows: Vec<(i64, bool)> = connection
        .prepare("SELECT at_ms,cold FROM team_event_wakes ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, [(1, false), (3, true)]);
}

#[test]
fn a_kind_turned_off_is_muted_and_the_hourly_cap_pauses_auto_wake_and_tells_the_owner_once() {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let ada = fixture.agent(PROJECT, "sub", "codex", "working");
    let rule = WakeRule {
        finished: false,
        hourly_cap: 2,
        ..WakeRule::default()
    };
    set_rule(
        connection,
        &RuleTarget::Project {
            project_id: project(),
        },
        Some(&rule),
    )
    .unwrap()
    .unwrap();
    record(
        connection,
        &project(),
        ada,
        None,
        EventKind::Finished,
        "Done.",
        1,
    )
    .unwrap();
    assert!(!deliver_by_message(connection, &project(), true, 2).unwrap());
    assert_eq!(event_states(connection), [(EventState::Muted, None)]);
    let start = 1_000;
    for at in [start, start + 10] {
        record(
            connection,
            &project(),
            ada,
            None,
            EventKind::Question,
            "Go?",
            at,
        )
        .unwrap();
        assert!(deliver_by_message(connection, &project(), true, at + 1).unwrap());
        answer_all(connection);
    }
    record(
        connection,
        &project(),
        ada,
        None,
        EventKind::Question,
        "Third?",
        start + 20,
    )
    .unwrap();
    for at in [start + 21, start + 22] {
        assert!(!deliver_by_message(connection, &project(), true, at).unwrap());
    }
    let told = notifications(connection, Some(&project()), 10).unwrap();
    assert_eq!(told.len(), 1, "the owner hears of one pause once");
    assert_eq!(told[0].kind, NotificationKind::WakePaused);
    assert!(
        told[0]
            .text
            .contains("project team-events paused after 2 wakes")
    );
    assert!(status(connection, &project()).unwrap().paused);
    assert!(
        !deliver_by_message(connection, &project(), true, start + HOUR_MS).unwrap(),
        "the first wake is still inside the hour"
    );
    assert!(deliver_by_message(connection, &project(), true, start + 2 + HOUR_MS).unwrap());
    assert!(wakes(connection).last().unwrap().ends_with("asks: Third?"));
    assert!(!status(connection, &project()).unwrap().paused);
    answer_all(connection);
    let never = WakeRule {
        hourly_cap: 0,
        ..WakeRule::default()
    };
    set_rule(connection, &RuleTarget::App, Some(&never))
        .unwrap()
        .unwrap();
    set_rule(
        connection,
        &RuleTarget::Project {
            project_id: project(),
        },
        None,
    )
    .unwrap()
    .unwrap();
    record(
        connection,
        &project(),
        ada,
        None,
        EventKind::Question,
        "Again?",
        9 * HOUR_MS,
    )
    .unwrap();
    assert!(!deliver_by_message(connection, &project(), true, 9 * HOUR_MS + 1).unwrap());
    assert_eq!(
        notifications(connection, Some(&project()), 10)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn wake_rules_resolve_from_the_agent_up_to_the_shipped_default() {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let ada = fixture.agent(PROJECT, "sub", "codex", "working");
    let bo = fixture.agent(PROJECT, "sub", "codex", "working");
    let foreign = fixture.agent(OTHER, "sub", "codex", "working");
    let shipped = WakeRule::default();
    assert_eq!(
        (
            shipped.question,
            shipped.finished,
            shipped.failed,
            shipped.change_set_ready,
            shipped.progress,
            shipped.hourly_cap
        ),
        (true, true, true, true, false, 30)
    );
    assert_eq!(
        rule(connection, None, None).unwrap(),
        (shipped, RuleLevel::Shipped)
    );
    let app = WakeRule {
        progress: true,
        ..shipped
    };
    let project_rule = WakeRule {
        hourly_cap: 5,
        ..shipped
    };
    let agent_rule = WakeRule {
        question: false,
        hourly_cap: 1,
        ..shipped
    };
    set_rule(connection, &RuleTarget::App, Some(&app))
        .unwrap()
        .unwrap();
    assert_eq!(
        rule(connection, Some(&project()), Some(ada)).unwrap(),
        (app, RuleLevel::App)
    );
    let here = RuleTarget::Project {
        project_id: project(),
    };
    set_rule(connection, &here, Some(&project_rule))
        .unwrap()
        .unwrap();
    let mine = RuleTarget::Agent {
        project_id: project(),
        agent: ada,
    };
    set_rule(connection, &mine, Some(&agent_rule))
        .unwrap()
        .unwrap();
    assert_eq!(
        rule(connection, Some(&project()), Some(ada)).unwrap(),
        (agent_rule, RuleLevel::Agent)
    );
    assert_eq!(
        rule(connection, Some(&project()), Some(bo)).unwrap(),
        (project_rule, RuleLevel::Project)
    );
    assert_eq!(rule(connection, None, None).unwrap(), (app, RuleLevel::App));
    let too_high = WakeRule {
        hourly_cap: HOURLY_CAP_MAX + 1,
        ..shipped
    };
    assert_eq!(
        set_rule(connection, &here, Some(&too_high)).unwrap(),
        Err("team_wake_cap_invalid")
    );
    for target in [
        RuleTarget::Agent {
            project_id: project(),
            agent: foreign,
        },
        RuleTarget::Agent {
            project_id: project(),
            agent: 0,
        },
    ] {
        assert_eq!(
            set_rule(connection, &target, Some(&shipped)).unwrap(),
            Err("team_agent_not_found")
        );
    }
    let add = |agent: i64, kind: EventKind, at: i64| {
        record(connection, &project(), agent, None, kind, "Done.", at)
            .unwrap()
            .unwrap()
    };
    add(ada, EventKind::Finished, 10);
    assert!(deliver_by_message(connection, &project(), true, 11).unwrap());
    answer_all(connection);
    add(ada, EventKind::Finished, 12);
    add(bo, EventKind::Finished, 13);
    assert!(deliver_by_message(connection, &project(), true, 14).unwrap());
    assert_eq!(
        wakes(connection).last().unwrap(),
        &format!("[team] Sub-agent {bo} finished: Done."),
        "an agent at its own cap waits while the others still wake the master"
    );
    let told = notifications(connection, Some(&project()), 10).unwrap();
    assert_eq!(told.len(), 1);
    assert!(
        told[0]
            .text
            .contains(&format!("Sub-agent {ada} paused after 1 wakes"))
    );
    answer_all(connection);
    add(ada, EventKind::Question, 15);
    assert!(deliver_by_message(connection, &project(), true, 16).is_ok());
    assert!(
        events(connection, &project(), 10)
            .unwrap()
            .iter()
            .any(|event| event.kind == EventKind::Question && event.state == EventState::Muted)
    );
    set_rule(connection, &mine, None).unwrap().unwrap();
    assert_eq!(
        rule(connection, Some(&project()), Some(ada)).unwrap(),
        (project_rule, RuleLevel::Project)
    );
}

#[test]
fn queued_questions_count_per_agent_until_they_reach_the_master_and_show_on_the_graph_node() {
    let fixture = Fixture::new();
    let connection = &fixture.connection;
    let ada = fixture.agent(PROJECT, "sub", "codex", "working");
    let bo = fixture.agent(PROJECT, "sub", "claude-code", "working");
    let cy = fixture.agent(PROJECT, "sub", "codex", "working");
    for (agent, kind, text, at) in [
        (ada, EventKind::Question, "Proceed?", 10),
        (bo, EventKind::Finished, "Done.", 11),
        (ada, EventKind::Question, "Which file?", 12),
        (bo, EventKind::Question, "Proceed?", 13),
    ] {
        record(connection, &project(), agent, None, kind, text, at)
            .unwrap()
            .unwrap();
    }
    let counted = view(connection, &project()).unwrap().questions;
    assert_eq!(
        counted,
        [
            AgentQuestions {
                agent: ada,
                count: 1
            },
            AgentQuestions {
                agent: bo,
                count: 1
            }
        ]
    );
    assert!(
        events(connection, &project(), 10)
            .unwrap()
            .iter()
            .any(|event| event.agent == ada && event.text == "Which file?")
    );
    let agents: Vec<serde_json::Value> = [ada, bo, cy]
        .into_iter()
        .map(|id| {
            serde_json::json!({
                "id": id, "role": "sub", "label": format!("Sub-agent {id}"), "harness": "codex", "tools": true, "account": null, "worktree": null,
                "status": "working", "status_label": "Working", "task": null, "turns": 0, "problem": null,
                "process_group": null, "created_ms": 0, "heartbeat_ms": 0, "stopped_ms": null,
            })
        })
        .collect();
    let node = |questions: Vec<AgentQuestions>, agent: i64| {
        let mut status: crate::team::TeamStatus = serde_json::from_value(serde_json::json!({
            "project_id": PROJECT, "keep_warm": false, "paused": false, "min_agents": 0, "max_agents": 3,
            "harnesses": [], "tasks": [], "messages": [], "plan": [],
            "agents": agents.clone(),
            "events": serde_json::to_value(EventsView::default()).unwrap(),
        }))
        .unwrap();
        status.events.questions = questions;
        crate::team::graph(&status)
            .nodes
            .into_iter()
            .find(|node| node.id == format!("team-{agent}"))
            .unwrap()
            .questions
    };
    assert_eq!(node(counted.clone(), ada), Some(1));
    assert_eq!(node(counted.clone(), bo), Some(1));
    assert_eq!(node(counted, cy), Some(0));
    assert!(deliver_by_message(connection, &project(), true, 100).unwrap());
    let delivered = view(connection, &project()).unwrap().questions;
    assert!(delivered.is_empty(), "{delivered:?}");
    assert_eq!(node(delivered, ada), Some(0));
}
