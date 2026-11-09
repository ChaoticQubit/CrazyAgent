use super::*;
use std::path::PathBuf;
use std::process::Command;

const PROJECT: &str = "team-mates";

const CLOSED: &str = "Stopped when CrazyAgent closed";

/// One test's engine database with a running native lead and the lead's private home.
struct Engine {
    _directory: tempfile::TempDir,
    connection: Connection,
    project: ProjectId,
    lead: i64,
    home: PathBuf,
}

impl Engine {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("engine.db")).unwrap();
        crate::team::initialize(&connection).unwrap();
        crate::project_setup::initialize(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms,hookless)
                 VALUES (?1,'sub','claude-code',1,'local-a','working',1,1,1)",
                [PROJECT],
            )
            .unwrap();
        let lead = connection.last_insert_rowid();
        let home = directory.path().join("home");
        std::fs::create_dir(&home).unwrap();
        Self {
            _directory: directory,
            connection,
            project: ProjectId::new(PROJECT).unwrap(),
            lead,
            home,
        }
    }

    fn team(&self, folder: &str, members: &str) -> PathBuf {
        let team = self.home.join(folder);
        std::fs::create_dir_all(&team).unwrap();
        let config = team.join("config.json");
        std::fs::write(&config, format!(r#"{{"members":[{members}]}}"#)).unwrap();
        config
    }

    fn register(&self, live: &[&str], since: i64, at: i64) {
        register(
            &self.connection,
            &self.project,
            self.lead,
            &self.home,
            live,
            since,
            at,
        )
        .unwrap();
    }

    fn stopped_teammate(&self, pane: &str, reason: Option<&str>, stopped: i64) -> i64 {
        self.connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,connection,status,error,created_ms,heartbeat_ms,stopped_ms,parent,pane)
                 VALUES (?1,'teammate','claude-code',1,'local-a','stopped',?2,1,1,?3,?4,?5)",
                params![PROJECT, reason, stopped, self.lead, pane],
            )
            .unwrap();
        self.connection.last_insert_rowid()
    }

    fn rows(&self) -> Vec<Row> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id,parent,pane,harness,tools,connection,hookless,status,error,stopped_ms,heartbeat_ms
                 FROM team_agents WHERE role='teammate' ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok(Row {
                    id: row.get(0)?,
                    parent: row.get(1)?,
                    pane: row.get(2)?,
                    harness: row.get(3)?,
                    tools: row.get(4)?,
                    connection: row.get(5)?,
                    hookless: row.get(6)?,
                    status: row.get(7)?,
                    error: row.get(8)?,
                    stopped_ms: row.get(9)?,
                    heartbeat_ms: row.get(10)?,
                })
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    id: i64,
    parent: Option<i64>,
    pane: Option<String>,
    harness: String,
    tools: bool,
    connection: Option<String>,
    hookless: bool,
    status: String,
    error: Option<String>,
    stopped_ms: Option<i64>,
    heartbeat_ms: i64,
}

fn member(name: &str, pane: &str) -> Member {
    Member {
        name: name.into(),
        pane: pane.into(),
    }
}

#[test]
fn registers_only_live_listed_panes() {
    let engine = Engine::new();
    engine.team(
        "teams/session-1",
        r#"{"name":"team-lead","tmuxPaneId":"%1"},{"name":"Planner","tmuxPaneId":"%3"},{"name":"tester","tmuxPaneId":"%4"}"#,
    );

    engine.register(&["%3", "%9"], 1, 5);
    let rows = engine.rows();
    assert_eq!(
        rows.len(),
        1,
        "only a live pane the lead's team lists gets a row: {rows:?}"
    );
    let mate = rows[0].clone();
    assert_eq!(mate.parent, Some(engine.lead));
    assert_eq!(mate.pane.as_deref(), Some("%3"));
    assert_eq!(
        (
            mate.harness.as_str(),
            mate.tools,
            mate.connection.as_deref(),
            mate.hookless
        ),
        ("claude-code", true, Some("local-a"), true),
        "a teammate runs on its lead's harness, tools, account and hook mode"
    );
    assert_eq!((mate.status.as_str(), mate.heartbeat_ms), ("working", 5));
    let name = crate::team::names::name(&engine.connection, mate.id)
        .unwrap()
        .expect("a registered teammate is named");
    assert_eq!(name.handle.as_deref(), Some("planner"));
    assert!(!name.persona.is_empty());
    assert_eq!(
        working(&engine.connection, &engine.project, engine.lead).unwrap(),
        vec![(mate.id, "%3".to_owned())]
    );

    engine.register(&["%3"], 1, 6);
    assert_eq!(
        engine.rows().len(),
        1,
        "a second pass adds no second row for the same pane"
    );

    engine.register(&[], 1, 7);
    let rows = engine.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].id, rows[0].status.as_str(), rows[0].stopped_ms),
        (mate.id, "stopped", Some(7)),
        "a teammate whose pane is no longer live stops"
    );
    assert!(
        working(&engine.connection, &engine.project, engine.lead)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_lead_that_is_not_a_running_top_level_sub_agent_registers_nothing() {
    let engine = Engine::new();
    engine.team("teams/t", r#"{"name":"mate","tmuxPaneId":"%3"}"#);
    let other = ProjectId::new("another-project").unwrap();
    register(
        &engine.connection,
        &other,
        engine.lead,
        &engine.home,
        &["%3"],
        1,
        5,
    )
    .unwrap();
    engine
        .connection
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,parent)
             VALUES (?1,'sub','claude-code',1,'working',1,1,?2)",
            params![PROJECT, engine.lead],
        )
        .unwrap();
    let nested = engine.connection.last_insert_rowid();
    register(
        &engine.connection,
        &engine.project,
        nested,
        &engine.home,
        &["%3"],
        1,
        5,
    )
    .unwrap();
    assert!(
        engine.rows().is_empty(),
        "neither another project's id nor a nested sub-agent leads teammates"
    );
}

#[test]
fn members_skips_lead_links_and_bad_panes() {
    let engine = Engine::new();
    engine.team(
        "teams/a",
        &[
            r#"{"name":"team-lead","tmuxPaneId":"%1"}"#,
            r#"{"name":"good","tmuxPaneId":"%2"}"#,
            r#"{"name":"lettered","tmuxPaneId":"%x"}"#,
            r#"{"name":"bare","tmuxPaneId":"2"}"#,
            r#"{"name":"","tmuxPaneId":"%5"}"#,
            r#"{"name":"bell\u0007","tmuxPaneId":"%6"}"#,
            &format!(r#"{{"name":"{}","tmuxPaneId":"%10"}}"#, "n".repeat(65)),
            r#"{"name":"again","tmuxPaneId":"%2"}"#,
            r#"{"name":"no-pane"}"#,
            r#"{"name":"numbered","tmuxPaneId":7}"#,
        ]
        .join(","),
    );
    let outside = engine.home.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(
        outside.join("config.json"),
        r#"{"members":[{"name":"linked","tmuxPaneId":"%7"}]}"#,
    )
    .unwrap();
    std::fs::create_dir_all(engine.home.join("teams/b")).unwrap();
    std::os::unix::fs::symlink(
        outside.join("config.json"),
        engine.home.join("teams/b/config.json"),
    )
    .unwrap();
    std::fs::create_dir_all(engine.home.join("teams/c")).unwrap();
    let fifo = engine.home.join("teams/c/config.json");
    let made = crate::spawn_gate::status(Command::new("/usr/bin/mkfifo").arg(&fifo)).unwrap();
    assert!(made.success());
    let huge = engine.team(
        "teams/d",
        &format!(
            r#"{{"name":"huge","tmuxPaneId":"%9","pad":"{}"}}"#,
            "p".repeat(256 * 1024)
        ),
    );
    assert!(std::fs::metadata(&huge).unwrap().len() > 256 * 1024);
    engine.team("teams/e", "not json");
    engine.team(".claude/teams/f", r#"{"name":"deep","tmuxPaneId":"%8"}"#);

    let (sender, receiver) = std::sync::mpsc::channel();
    let home = engine.home.clone();
    std::thread::spawn(move || sender.send(members(&home)).unwrap());
    let found = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("reading the team configs waited on a FIFO");
    assert_eq!(
        found,
        vec![member("good", "%2"), member("deep", "%8")],
        "the lead, invalid names and panes, a repeated pane, a link, a FIFO, an oversized \
         file and broken JSON are all left out"
    );
}

#[test]
fn members_stop_at_their_bound() {
    let engine = Engine::new();
    let listed: Vec<String> = (1..=MEMBERS_MAX + 6)
        .map(|n| format!(r#"{{"name":"mate-{n}","tmuxPaneId":"%{n}"}}"#))
        .collect();
    engine.team("teams/big", &listed.join(","));
    let found = members(&engine.home);
    assert_eq!(found.len(), MEMBERS_MAX);
    assert_eq!(found[0], member("mate-1", "%1"));
}

#[test]
fn owner_stopped_row_stays_stopped_while_pane_lives() {
    for reason in [
        super::super::OWNER_STOPPED_TEAMMATE,
        super::super::MASTER_STOPPED_TEAMMATE,
        super::super::LEAD_STOPPED_TEAMMATE,
        super::super::LEAD_ENDED_TEAMMATE,
        super::super::OVER_TEAMMATE_CAP,
    ] {
        let engine = Engine::new();
        engine.team("teams/t", r#"{"name":"mate","tmuxPaneId":"%3"}"#);
        let mate = engine.stopped_teammate("%3", Some(reason), 10);
        engine.register(&["%3"], 5, 20);
        let rows = engine.rows();
        assert_eq!(
            rows.len(),
            1,
            "{reason}: no second row while the stop is pending"
        );
        assert_eq!(
            (
                rows[0].id,
                rows[0].status.as_str(),
                rows[0].error.as_deref(),
                rows[0].stopped_ms
            ),
            (mate, "stopped", Some(reason), Some(10)),
            "{reason}: a stopped row is never marked working while its pane still ends"
        );
    }
}

#[test]
fn engine_closed_row_returns() {
    for reason in [Some(CLOSED), None] {
        let engine = Engine::new();
        engine.team("teams/t", r#"{"name":"mate","tmuxPaneId":"%3"}"#);
        let mate = engine.stopped_teammate("%3", reason, 10);
        engine.register(&["%3"], 5, 20);
        let rows = engine.rows();
        assert_eq!(rows.len(), 1, "{reason:?}");
        assert_eq!(
            (
                rows[0].id,
                rows[0].status.as_str(),
                rows[0].error.as_deref(),
                rows[0].stopped_ms,
                rows[0].heartbeat_ms
            ),
            (mate, "working", None, None, 20),
            "{reason:?}: a row of this lead run whose pane still lives works again"
        );
    }
}

#[test]
fn earlier_run_row_does_not_block() {
    let engine = Engine::new();
    engine.team("teams/t", r#"{"name":"mate","tmuxPaneId":"%3"}"#);
    let old = engine.stopped_teammate("%3", Some(super::super::OWNER_STOPPED_TEAMMATE), 3);
    engine.register(&["%3"], 5, 20);
    let rows = engine.rows();
    assert_eq!(rows.len(), 2, "a new lead run gets a new row: {rows:?}");
    assert_eq!(
        (rows[0].id, rows[0].status.as_str()),
        (old, "stopped"),
        "the earlier run's row stays as it ended"
    );
    assert_eq!(
        (rows[1].pane.as_deref(), rows[1].status.as_str()),
        (Some("%3"), "working")
    );
}

#[test]
fn a_teammate_whose_lead_stopped_ends_with_the_lead_reason() {
    let engine = Engine::new();
    engine.team(
        "teams/t",
        r#"{"name":"mate","tmuxPaneId":"%3"},{"name":"other","tmuxPaneId":"%4"}"#,
    );
    engine.register(&["%3", "%4"], 1, 5);
    end_orphans(&engine.connection, 6).unwrap();
    assert!(
        engine.rows().iter().all(|row| row.status == "working"),
        "a running lead keeps its teammates"
    );
    engine
        .connection
        .execute(
            "UPDATE team_agents SET status='stopped', stopped_ms=7 WHERE id=?1",
            [engine.lead],
        )
        .unwrap();
    end_orphans(&engine.connection, 8).unwrap();
    for row in engine.rows() {
        assert_eq!(
            (row.status.as_str(), row.error.as_deref(), row.stopped_ms),
            ("stopped", Some(super::super::LEAD_ENDED_TEAMMATE), Some(8))
        );
    }
}
