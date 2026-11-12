use super::*;
use crate::landing::{
    AcceptOutcome, AgentKind, ChangeAgent, ChangeSetId, ChangeState, TurnEnd, TurnOutcome,
    Workspace,
};
use crate::questions::{Ask, Asker};
use crate::tracker::Host;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use trackwright_core::criteria;
use trackwright_core::events;
use trackwright_core::model::{CheckKind, CriterionState, TicketStatus};
use trackwright_core::ops;
use trackwright_core::requests::Request;
use trackwright_core::store;

const PROJECT: &str = "ticket-hooks";
const KEY: &str = "TH";
const AGENT: i64 = 7;
const IDLE: i64 = 8;
const NOW: i64 = 1_800_000_000_000;

/// One owner repository with two sub-agents' worktrees, each on its own branch, and an
/// engine root whose `engine.db` holds every authority schema, the change set store and
/// the tracker's tables. The project is attached as `TH` with the owner's ticket `TH-1`,
/// and `task` is queued and linked to it the way a master's `spawn` with that ticket is.
struct Case {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    engine: PathBuf,
    db: Connection,
    task: i64,
}

/// Runs git as the owner would in a test: an isolated home, no system config, in `dir`.
/// Inputs: the owner home, the folder and the arguments. Output: standard output.
fn git_in(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn ticket() -> ShortId {
    ShortId::parse("TH-1").unwrap()
}

fn owner() -> Actor {
    Actor::new("owner").unwrap()
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, home, engine) = (root.join("repo"), root.join("home"), root.join("engine"));
        for folder in [&repo, &home, &engine] {
            std::fs::create_dir(folder).unwrap();
        }
        git_in(&home, &repo, &["init", "-q", "--template=", "-b", "main"]);
        git_in(&home, &repo, &["config", "user.name", "Repo Owner"]);
        git_in(
            &home,
            &repo,
            &["config", "user.email", "owner@example.invalid"],
        );
        std::fs::write(repo.join("README"), b"one\n").unwrap();
        git_in(&home, &repo, &["add", "README"]);
        git_in(&home, &repo, &["commit", "-q", "-m", "Start"]);
        let db = crate::landing::open(&engine).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::ide::workspace::tests::add_project(&db, PROJECT, &repo);
        let mut case = Self {
            _tmp: tmp,
            home,
            repo,
            engine,
            db,
            task: 0,
        };
        case.agent(AGENT);
        case.agent(IDLE);
        let tx = case.tx();
        crate::tracker::attach(
            &tx,
            &project(),
            &ProjectKey::new(KEY).unwrap(),
            "Hooks",
            NOW,
        )
        .unwrap();
        let create = br#"{"project": "TH", "title": "Land the notes"}"#;
        let reply = ops::dispatch(
            &tx,
            &Host,
            &owner(),
            Request::parse("create", create).unwrap(),
        );
        assert!(reply.is_ok(), "{}", reply.json());
        tx.commit().unwrap();
        case.task = case.delegated("Write the notes", Some("TH-1"));
        case
    }

    fn tx(&self) -> Transaction<'_> {
        Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate).unwrap()
    }

    fn worktree(&self, id: i64) -> PathBuf {
        self.repo.parent().unwrap().join(format!("agent-{id}"))
    }

    fn git_dir(&self, id: i64) -> PathBuf {
        self.repo.join(".git/worktrees").join(format!("agent-{id}"))
    }

    /// Adds one sub-agent the way the runtime records it: a real worktree on its own
    /// branch `crazyagent/agent-<id>` with its start recorded, and its team rows.
    /// Input: the id.
    fn agent(&self, id: i64) {
        let worktree = self.worktree(id);
        let branch = format!("crazyagent/agent-{id}");
        let at = worktree.to_str().unwrap();
        git_in(
            &self.home,
            &self.repo,
            &["worktree", "add", "-q", "-b", &branch, at, "HEAD"],
        );
        crate::landing::record_worktree_start(&self.db, &self.repo, &worktree, &self.git_dir(id))
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_agents(id,project,role,harness,tools,connection,status,
                 created_ms,heartbeat_ms) VALUES (?1,?2,'sub','codex',1,'local-a','working',?3,?3)",
                params![id, PROJECT, NOW],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    PROJECT,
                    self.repo.to_str().unwrap(),
                    at,
                    self.git_dir(id).to_str().unwrap()
                ],
            )
            .unwrap();
    }

    /// Queues a task the way a master's `spawn` admits one and, with a ticket, links it in
    /// the same transaction through the delegate link. Inputs: the prompt and the ticket.
    /// Output: the task.
    fn delegated(&self, prompt: &str, ticket: Option<&str>) -> i64 {
        let tx = self.tx();
        tx.execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms)
             VALUES (?1,'master','codex',1,?2,'queued',?3)",
            params![PROJECT, prompt, NOW],
        )
        .unwrap();
        let task = tx.last_insert_rowid();
        if let Some(ticket) = ticket {
            crate::team::tool::tracker::link_task(&tx, &project(), ticket, task, NOW)
                .unwrap()
                .unwrap();
        }
        tx.commit().unwrap();
        task
    }

    /// Starts a queued task on an agent as the runtime's take does: the task is marked
    /// running and its ticket links are added in one engine transaction. Inputs: the task
    /// and the agent.
    fn take(&self, task: i64, agent: i64) {
        let tx = self.tx();
        let started = tx
            .execute(
                "UPDATE team_tasks SET status='running', agent=?2, attempts=attempts+1,
                 started_ms=?3, error=NULL WHERE id=?1 AND status='queued'",
                params![task, agent, NOW],
            )
            .unwrap();
        assert_eq!(started, 1);
        task_taken(&tx, &project(), task, agent, NOW).unwrap();
        tx.commit().unwrap();
    }

    /// Ends one turn of agent 7's task in its worktree as the runtime does, after writing
    /// the README. Input: the README text. Output: what `record_turn_end` returned.
    fn turn(&self, text: &str) -> Result<Option<ChangeSetId>, EngineError> {
        std::fs::write(self.worktree(AGENT).join("README"), text).unwrap();
        let workspace = Workspace {
            project: project(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: AGENT.to_string(),
                label: format!("Agent {AGENT}"),
            },
            repository: self.repo.clone(),
            worktree: self.worktree(AGENT),
            git_dir: self.git_dir(AGENT),
        };
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: format!("task-{}", self.task),
            prompt_line: "Write the notes".into(),
        };
        crate::landing::record_turn_end(&self.db, &workspace, &turn)
    }

    fn state(&self, id: &ChangeSetId) -> (ChangeState, String) {
        let view = crate::landing::list(&self.db, &project())
            .unwrap()
            .into_iter()
            .find(|view| &view.id == id)
            .unwrap();
        (view.state, view.head)
    }

    fn accept(&self, id: &ChangeSetId) -> Result<AcceptOutcome, EngineError> {
        let (_, head) = self.state(id);
        crate::landing::accept(
            &self.db,
            &self.home,
            id,
            &head,
            None,
            crate::activity::AcceptPlace::Changes,
        )
    }

    /// Adds an owner criterion to `TH-1`, with a `change` check on a change set when one is
    /// given. Inputs: the criterion text and the change set. Output: its position.
    fn criterion(&self, text: &str, change: Option<&ChangeSetId>) -> u32 {
        let tx = self.tx();
        let added = criteria::add_criterion(&tx, &Host, &owner(), &ticket(), text, NOW).unwrap();
        if let Some(change) = change {
            self.check(&tx, added.position, change);
        }
        tx.commit().unwrap();
        added.position
    }

    fn check(&self, tx: &Transaction<'_>, position: u32, change: &ChangeSetId) {
        let kind = CheckKind::Change;
        criteria::add_check(
            tx,
            &Host,
            &owner(),
            &ticket(),
            position,
            kind,
            change.as_str(),
            NOW,
        )
        .unwrap();
    }

    /// Reads `TH-1`'s links of one kind. Input: the kind. Output: each target and the actor
    /// that linked it, in the order they were linked.
    fn links(&self, kind: &str) -> Vec<(String, String)> {
        let mut statement = self
            .db
            .prepare(
                "SELECT l.target, l.created_by FROM tk_links l JOIN tk_tickets t ON t.id = l.ticket
                 WHERE t.number = 1 AND l.kind = ?1 ORDER BY l.created_ms, l.target",
            )
            .unwrap();
        statement
            .query_map([kind], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn ticket_now(&self) -> (TicketStatus, Option<String>, Vec<CriterionState>) {
        let tx = self.tx();
        let read = store::ticket(&tx, &ticket()).unwrap();
        let states = criteria::criteria(&tx, &ticket())
            .unwrap()
            .into_iter()
            .map(|criterion| criterion.state)
            .collect();
        let closed_by = read.closed_by.map(|actor| actor.as_str().to_owned());
        (read.status, closed_by, states)
    }

    fn count(&self, sql: &str) -> i64 {
        self.db.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

fn hooked(target: impl ToString) -> (String, String) {
    (target.to_string(), HOOK_ACTOR.to_owned())
}

#[test]
fn accepting_the_last_change_set_closes_the_ticket_in_the_landing_transaction() {
    let case = Case::new();
    case.take(case.task, AGENT);
    let first = case.turn("one\ntwo\n").unwrap().unwrap();
    assert_eq!(case.links("change_set"), vec![hooked(first.as_str())]);
    case.criterion("The notes have their second line", Some(&first));
    let last = case.criterion("The notes have their third line", None);

    let AcceptOutcome::Landed { .. } = case.accept(&first).unwrap() else {
        panic!("the first change set conflicted");
    };
    let (status, closed_by, states) = case.ticket_now();
    assert!(!status.is_closed(), "{status:?}");
    assert_eq!(closed_by, None);
    assert_eq!(states, vec![CriterionState::Met, CriterionState::Open]);

    let second = case.turn("one\ntwo\nthree\n").unwrap().unwrap();
    assert_ne!(second, first);
    assert_eq!(
        case.links("change_set"),
        vec![hooked(first.as_str()), hooked(second.as_str())]
    );
    let tx = case.tx();
    case.check(&tx, last, &second);
    tx.commit().unwrap();
    let AcceptOutcome::Landed { .. } = case.accept(&second).unwrap() else {
        panic!("the last change set conflicted");
    };
    assert_eq!(case.state(&second).0, ChangeState::Accepted);
    let (status, closed_by, states) = case.ticket_now();
    assert_eq!(status, TicketStatus::Done);
    assert_eq!(closed_by.as_deref(), Some("host:evidence"));
    assert_eq!(states, vec![CriterionState::Met, CriterionState::Met]);
}

#[test]
fn a_rolled_back_landing_write_leaves_no_link_and_no_close() {
    let case = Case::new();
    case.take(case.task, AGENT);
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_link BEFORE INSERT ON tk_links
             BEGIN SELECT RAISE(ABORT, 'the test refuses the link'); END;",
        )
        .unwrap();
    assert!(case.turn("one\ntwo\n").is_err());
    assert_eq!(case.count("SELECT COUNT(*) FROM landing_change_sets"), 0);
    assert_eq!(case.links("change_set"), vec![]);
    case.db.execute_batch("DROP TRIGGER refuse_link").unwrap();

    let change = case.turn("one\ntwo\nthree\n").unwrap().unwrap();
    assert_eq!(case.links("change_set"), vec![hooked(change.as_str())]);
    case.criterion("The notes have three lines", Some(&change));
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_close BEFORE UPDATE ON tk_tickets
             WHEN NEW.status = 'done'
             BEGIN SELECT RAISE(ABORT, 'the test refuses the close'); END;",
        )
        .unwrap();
    assert!(case.accept(&change).is_err());
    assert_ne!(case.state(&change).0, ChangeState::Accepted);
    let (status, closed_by, states) = case.ticket_now();
    assert!(!status.is_closed(), "{status:?}");
    assert_eq!(closed_by, None);
    assert_eq!(states, vec![CriterionState::Open]);
    assert_eq!(
        case.count("SELECT COUNT(*) FROM tk_events WHERE actor = 'host:evidence'"),
        0
    );
}

#[test]
fn a_delegated_ticket_task_links_agent_worktree_and_branch_once() {
    let case = Case::new();
    assert_eq!(
        case.links("task"),
        vec![(
            case.task.to_string(),
            crate::team::tool::tracker::DELEGATE_ACTOR.to_owned()
        )]
    );
    case.take(case.task, AGENT);
    let worktree = case.worktree(AGENT);
    assert_eq!(case.links("agent"), vec![hooked(AGENT)]);
    assert_eq!(
        case.links("worktree"),
        vec![hooked(worktree.to_str().unwrap())]
    );
    assert_eq!(case.links("branch"), vec![hooked("crazyagent/agent-7")]);
    let linked = format!(
        "SELECT COUNT(*) FROM tk_events WHERE op = '{}'",
        events::LINK_ADD
    );
    assert_eq!(case.count(&linked), 4);

    let Case {
        _tmp,
        home,
        repo,
        engine,
        db,
        task,
    } = case;
    db.execute(
        "UPDATE team_tasks SET status='queued', agent=NULL WHERE id=?1",
        [task],
    )
    .unwrap();
    drop(db);
    let restarted = Case {
        db: crate::landing::open(&engine).unwrap(),
        _tmp,
        home,
        repo,
        engine,
        task,
    };
    restarted.take(restarted.task, AGENT);
    assert_eq!(restarted.links("agent"), vec![hooked(AGENT)]);
    assert_eq!(restarted.links("worktree").len(), 1);
    assert_eq!(restarted.links("branch").len(), 1);
    assert_eq!(restarted.count(&linked), 4, "a replayed take adds no link");

    let plain = restarted.delegated("Tidy the folder", None);
    restarted.take(plain, IDLE);
    assert_eq!(restarted.links("agent"), vec![hooked(AGENT)]);
    assert_eq!(restarted.count(&linked), 4);
}

#[test]
fn a_memory_saved_on_the_task_links_once() {
    let mut case = Case::new();
    let remember = |case: &mut Case, agent: Option<i64>, text: &str| {
        let label = agent.map_or("A caller with no team id".to_owned(), |id| {
            format!("Agent {id}")
        });
        crate::memory::library::agent_remember(
            &mut case.db,
            &project(),
            (agent, &label),
            false,
            text,
        )
        .unwrap()
    };
    let saved = |case: &Case, text: &str| -> String {
        case.db
            .query_row(
                "SELECT id FROM memory_library WHERE text=?1",
                [text],
                |row| row.get(0),
            )
            .unwrap()
    };
    remember(&mut case, Some(AGENT), "Before the task: keep spans.").unwrap();
    case.take(case.task, AGENT);
    remember(&mut case, Some(AGENT), "The notes use plain text.").unwrap();
    let memory = saved(&case, "The notes use plain text.");
    assert_eq!(case.links("memory"), vec![hooked(&memory)]);
    assert_eq!(
        remember(&mut case, Some(AGENT), "The notes use plain text."),
        Err("memory_duplicate".to_owned())
    );
    remember(&mut case, Some(IDLE), "Agent 8 has no ticket task.").unwrap();
    remember(&mut case, None, "A note with no team agent.").unwrap();
    assert_eq!(case.links("memory"), vec![hooked(&memory)]);

    case.db
        .execute(
            "UPDATE team_tasks SET status='succeeded' WHERE id=?1",
            [case.task],
        )
        .unwrap();
    remember(&mut case, Some(AGENT), "After the task: no ticket.").unwrap();
    assert_eq!(case.links("memory"), vec![hooked(&memory)]);
}

#[test]
fn a_question_asked_on_the_task_links_once() {
    let case = Case::new();
    let ask = |asker: &Asker, question: &str| {
        let asked = Ask {
            question: question.into(),
            kind: crate::questions::QuestionKind::Single,
            options: vec!["Markdown".into(), "Plain text".into()],
            recommended: None,
            context: None,
        };
        crate::questions::ask(&case.db, &project(), asker, &asked).unwrap()
    };
    ask(&Asker::Agent(AGENT), "Before the task: which folder?");
    case.take(case.task, AGENT);
    let asked = ask(&Asker::Agent(AGENT), "Which format do the notes use?");
    assert_eq!(case.links("question"), vec![hooked(asked)]);
    ask(&Asker::Agent(IDLE), "Agent 8 asks with no ticket task.");
    ask(&Asker::Master(41), "A master never runs a task.");
    assert_eq!(case.links("question"), vec![hooked(asked)]);
}
