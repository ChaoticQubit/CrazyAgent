use super::*;
use crate::checks::{self, Trigger, queue};
use crate::config::ModelAccess;
use crate::ide::workspace::tests::{Agent, Case};
use crate::landing::{AgentKind, ChangeAgent, ChangeSetId, TurnEnd, TurnOutcome, Workspace};
use crate::questions::{Answer, Ask, Asker, EngineSource};
use crate::team::TeamAction;
use crate::tracker::Host;
use rusqlite::{Transaction, TransactionBehavior};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use trackwright_core::criteria;
use trackwright_core::model::{Actor as CoreActor, CheckKind, ProjectKey, ShortId};
use trackwright_core::ops;
use trackwright_core::requests::Request;
use trackwright_core::tests_model::{CaseKind, CaseStatus};
use trackwright_core::tests_store::{self, NewCase};

const PASS: &str = "exit 0\n";
const FAIL: &str = "exit 1\n";

/// One real engine root and project repository from the IDE workspace case, attached to
/// the tracker as `CA`, with an approved profile whose one check `unit` runs `sh gate.sh`
/// in the change set's tree, one ready test case named after that check, so a ticket
/// whose criterion it proves closes on the first passing run, and an owner home for
/// Accept.
struct Score {
    case: Case,
    home: PathBuf,
    test_case: String,
    turns: std::cell::Cell<u32>,
}

impl Score {
    fn new() -> Self {
        let case = Case::new();
        let checks_folder = case.root.join("checks");
        std::fs::create_dir(&checks_folder).unwrap();
        std::fs::set_permissions(&checks_folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = case.folder.parent().unwrap().join("owner");
        std::fs::create_dir(&home).unwrap();
        let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
        crate::tracker::attach(&tx, &case.project, &key(), "Alpha", now_ms()).unwrap();
        let test_case = tests_store::create_case(
            &tx,
            &owner(),
            &key(),
            &NewCase {
                title: "The unit check passes".into(),
                kind: CaseKind::Unit,
                stable_name: Some("unit".into()),
                steps: Vec::new(),
                protected: false,
                status: CaseStatus::Ready,
            },
            now_ms(),
        )
        .unwrap();
        tx.commit().unwrap();
        let body = json!({
            "version": 1,
            "checks": [{
                "id": "unit",
                "kind": "unit",
                "toolchain": null,
                "argv": ["sh", "gate.sh"],
                "cwd": ".",
                "format": "exit_code",
                "report": null,
                "timeout_s": 600,
                "network": "off",
                "when": ["on_request"],
                "required": true,
                "protected": false
            }],
            "holdouts": [],
            "protected_paths": []
        });
        let proposal = checks::propose(
            &case.db,
            &case.project,
            &serde_json::to_vec(&body).unwrap(),
            checks::Actor::Owner,
        )
        .unwrap();
        checks::approve(&case.db, &case.project, &proposal.id, checks::Actor::Owner).unwrap();
        Self {
            case,
            home,
            test_case: String::from(test_case.short_id),
            turns: std::cell::Cell::new(0),
        }
    }

    fn agent(&mut self) -> Agent {
        self.case.agent("sub", "working")
    }

    fn landing(&self) -> Connection {
        crate::landing::open(&self.case.root).unwrap()
    }

    /// Writes `gate.sh` in the agent's worktree and records the turn end, which commits it
    /// into the worktree's open change set. Inputs: the agent and the script. Output: the
    /// change set.
    fn turn(&self, agent: &Agent, script: &str) -> ChangeSetId {
        std::fs::write(agent.worktree.join("gate.sh"), script).unwrap();
        self.turns.set(self.turns.get() + 1);
        let workspace = Workspace {
            project: self.case.project.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: agent.id.to_string(),
                label: format!("Sub-agent {}", agent.id),
            },
            repository: self.case.folder.clone(),
            worktree: agent.worktree.clone(),
            git_dir: agent.git_dir.clone(),
        };
        let end = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: format!("turn-{}", self.turns.get()),
            prompt_line: "Make the gate pass".into(),
        };
        crate::landing::record_turn_end(&self.landing(), &workspace, &end)
            .unwrap()
            .expect("the turn left an open change set")
    }

    /// Runs the profile's checks on a change set as one batch, as the owner asks. Input:
    /// the change set. Output: how many jobs ran.
    fn gate(&self, id: &ChangeSetId) -> usize {
        let db = self.landing();
        let batch = queue::submit(&db, id, Trigger::OnRequest, checks::Actor::Owner, &[]).unwrap();
        let mut ran = 0;
        while let Some(job) = queue::run_next(&db, &self.case.root).unwrap() {
            assert_eq!(job.batch, batch);
            ran += 1;
        }
        assert!(ran > 0);
        ran
    }

    /// Creates an owner ticket in `CA` whose one criterion the `unit` test case proves,
    /// closed by evidence, the default. Input: the title. Output: the ticket.
    fn ticket(&self, title: &str) -> ShortId {
        let tx = Transaction::new_unchecked(&self.case.db, TransactionBehavior::Immediate).unwrap();
        let created = dispatch(&tx, "create", json!({"project": "CA", "title": title}));
        let ticket = ShortId::parse(created["data"]["id"].as_str().unwrap()).unwrap();
        let criterion = criteria::add_criterion(
            &tx,
            &Host,
            &owner(),
            &ticket,
            "The unit check passed",
            now_ms(),
        )
        .unwrap();
        criteria::add_check(
            &tx,
            &Host,
            &owner(),
            &ticket,
            criterion.position,
            CheckKind::Test,
            &self.test_case,
            now_ms(),
        )
        .unwrap();
        tx.commit().unwrap();
        ticket
    }

    fn link(&self, ticket: &ShortId, id: &ChangeSetId) {
        let tx = Transaction::new_unchecked(&self.case.db, TransactionBehavior::Immediate).unwrap();
        let args = json!({
            "id": String::from(ticket.clone()),
            "kind": "change_set",
            "target": id.as_str()
        });
        dispatch(&tx, "link", args);
        tx.commit().unwrap();
    }

    fn status(&self, ticket: &ShortId) -> String {
        let tx = self.case.db.unchecked_transaction().unwrap();
        let shown = dispatch(&tx, "show", json!({"id": String::from(ticket.clone())}));
        shown["data"]["ticket"]["status"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn head(&self, id: &ChangeSetId) -> String {
        crate::landing::change_set(&self.case.db, id)
            .unwrap()
            .unwrap()
            .1
            .head
    }

    fn accept(&self, id: &ChangeSetId) {
        crate::landing::accept(
            &self.landing(),
            &self.home,
            id,
            &self.head(id),
            None,
            crate::activity::AcceptPlace::Changes,
        )
        .unwrap();
    }

    fn discard(&self, id: &ChangeSetId) {
        let now = u64::try_from(now_ms()).unwrap();
        crate::landing::discard(&self.landing(), id, &self.head(id), now).unwrap();
    }

    fn report(&self) -> Scorecard {
        report(&self.case.db, now_ms(), 0).unwrap()
    }

    fn settings(&self, settings: Settings) {
        let version = stored(&self.case.db).unwrap().version;
        set(&self.case.db, &settings, version).unwrap().unwrap();
    }

    /// One done task: a fresh agent's change set whose first batch passes, which closes a
    /// fresh ticket linked to it, then accepted by the owner. Output: the ticket.
    fn clean_task(&mut self, title: &str) -> ShortId {
        let agent = self.agent();
        let id = self.turn(&agent, PASS);
        let ticket = self.ticket(title);
        self.link(&ticket, &id);
        self.gate(&id);
        assert_eq!(self.status(&ticket), "done");
        self.accept(&id);
        ticket
    }

    fn message(&self, text: &str) {
        let tx = self.case.db.unchecked_transaction().unwrap();
        let action = TeamAction::Message {
            project_id: self.case.project.clone(),
            text: text.into(),
        };
        let outcome = crate::team::execute(&tx, &ModelAccess::default(), &action).unwrap();
        assert_eq!(outcome, crate::protocol::Outcome::Applied);
        tx.commit().unwrap();
    }

    fn today(&self) -> String {
        day_label(now_ms(), 0)
    }
}

fn key() -> ProjectKey {
    ProjectKey::new("CA").unwrap()
}

fn owner() -> CoreActor {
    CoreActor::new("owner").unwrap()
}

fn dispatch(tx: &Transaction<'_>, op: &str, args: Value) -> Value {
    let request = Request::parse(op, &serde_json::to_vec(&args).unwrap()).unwrap();
    let reply: Value =
        serde_json::from_str(ops::dispatch(tx, &Host, &owner(), request).json()).unwrap();
    assert_eq!(reply["ok"], true, "{op}: {reply}");
    reply
}

fn ask(text: &str) -> Ask {
    Ask {
        question: text.into(),
        kind: crate::questions::QuestionKind::Single,
        options: vec!["Yes".into(), "No".into()],
        recommended: None,
        context: None,
    }
}

fn day_offset(day: &str, days: i64) -> String {
    let number = day_number(day).unwrap();
    day_label((number + days) * DAY_MS, 0)
}

#[test]
fn a_task_with_a_discarded_change_set_is_not_clean() {
    let mut score = Score::new();
    let first = score.agent();
    let discarded = score.turn(&first, PASS);
    let ticket = score.ticket("Make the gate pass");
    score.link(&ticket, &discarded);
    score.discard(&discarded);
    let second = score.agent();
    let accepted = score.turn(&second, PASS);
    score.link(&ticket, &accepted);
    score.gate(&accepted);
    assert_eq!(score.status(&ticket), "done");
    score.accept(&accepted);

    let card = score.report();
    assert_eq!(card.real, 1);
    assert_eq!(card.clean, 0);
    assert_eq!(card.tasks.len(), 1);
    let task = &card.tasks[0];
    assert_eq!(task.ticket, String::from(ticket));
    assert!(!task.clean);
    assert_eq!(task.flaws, vec![Flaw::Discarded]);
    let mut sets = task.change_sets.clone();
    sets.sort();
    let mut expected = vec![discarded.as_str().to_owned(), accepted.as_str().to_owned()];
    expected.sort();
    assert_eq!(sets, expected);
    assert_eq!(card.coordination.measured.interventions, 1);
}

#[test]
fn an_accepted_task_whose_first_batch_passed_with_no_owner_edit_is_clean() {
    let mut score = Score::new();
    let ticket = score.clean_task("Make the gate pass");
    let card = score.report();
    assert_eq!(card.real, 1);
    assert_eq!(card.clean, 1);
    assert_eq!(card.tasks[0].ticket, String::from(ticket));
    assert!(card.tasks[0].clean);
    assert!(card.tasks[0].flaws.is_empty());
    assert_eq!(card.coordination.measured.acts(), 0);
}

#[test]
fn a_failed_first_batch_keeps_a_task_unclean_after_a_later_batch_passed() {
    let mut score = Score::new();
    let agent = score.agent();
    let id = score.turn(&agent, FAIL);
    let ticket = score.ticket("Make the gate pass");
    score.link(&ticket, &id);
    score.gate(&id);
    assert_ne!(score.status(&ticket), "done");
    assert_eq!(score.turn(&agent, PASS), id);
    score.gate(&id);
    assert_eq!(score.status(&ticket), "done");
    score.accept(&id);

    let card = score.report();
    assert_eq!(card.real, 1);
    assert_eq!(card.clean, 0);
    assert_eq!(card.tasks[0].flaws, vec![Flaw::FirstBatchNotPassed]);
}

#[test]
fn an_owner_edit_keeps_a_task_unclean_and_counts_as_an_intervention() {
    let mut score = Score::new();
    let agent = score.agent();
    let now = u64::try_from(now_ms()).unwrap();
    crate::ide::bridge::owner_edit::edit(
        &score.case.root,
        None,
        &score.case.project,
        &crate::terminal::AgentId::team(agent.id).unwrap(),
        "gate.sh",
        now,
    )
    .unwrap();
    let id = score.turn(&agent, PASS);
    let ticket = score.ticket("Make the gate pass");
    score.link(&ticket, &id);
    score.gate(&id);
    score.accept(&id);

    let card = score.report();
    assert_eq!(card.real, 1);
    assert_eq!(card.clean, 0);
    assert_eq!(card.tasks[0].flaws, vec![Flaw::OwnerEdited]);
    assert_eq!(card.coordination.measured.interventions, 1);
    assert_eq!(card.acts.len(), 1);
    assert_eq!(card.acts[0].kind, Kind::OwnerEdit);
    assert_eq!(card.acts[0].subject, "gate.sh");
}

#[test]
fn a_done_ticket_whose_change_set_was_never_accepted_is_no_real_task() {
    let mut score = Score::new();
    let agent = score.agent();
    let id = score.turn(&agent, PASS);
    let ticket = score.ticket("Make the gate pass");
    score.link(&ticket, &id);
    score.gate(&id);
    assert_eq!(score.status(&ticket), "done");

    let card = score.report();
    assert_eq!(card.real, 0);
    assert!(card.tasks.is_empty());
    assert!(!card.met.tasks);
}

#[test]
fn one_accepted_change_set_backs_one_real_task_however_many_tickets_link_it() {
    let mut score = Score::new();
    let agent = score.agent();
    let shared = score.turn(&agent, PASS);
    let other = score.agent();
    let own = score.turn(&other, PASS);
    let first = score.ticket("Make the gate pass");
    let copies: Vec<ShortId> = (0..3)
        .map(|copy| score.ticket(&format!("Make the gate pass, copy {copy}")))
        .collect();
    score.link(&first, &shared);
    for copy in &copies {
        score.link(copy, &shared);
    }
    score.link(&copies[0], &own);
    score.gate(&shared);
    score.gate(&own);
    assert_eq!(score.status(&first), "done");
    for copy in &copies {
        assert_eq!(score.status(copy), "done");
    }
    score.accept(&shared);
    score.accept(&own);

    let card = score.report();
    assert_eq!(card.real, 2, "the shared set backs its first ticket only");
    assert_eq!(card.clean, 2);
    let mut listed: Vec<String> = card.tasks.iter().map(|task| task.ticket.clone()).collect();
    listed.sort();
    let mut expected = vec![String::from(first), String::from(copies[0].clone())];
    expected.sort();
    assert_eq!(listed, expected);
    assert_eq!(card.coordination.measured.tasks, 2);
}

#[test]
fn owner_acts_stand_when_the_ledger_refuses_their_marks() {
    let mut score = Score::new();
    score.case.db.execute_batch(SCHEMA).unwrap();
    score
        .case
        .db
        .execute_batch(
            "CREATE TRIGGER scorecard_marks_refused BEFORE INSERT ON scorecard_marks
             BEGIN SELECT RAISE(ABORT, 'scorecard_marks refused'); END;",
        )
        .unwrap();
    score.message("Please make the gate pass");
    let master = questions::ask(
        &score.case.db,
        &score.case.project,
        &Asker::Master(41),
        &ask("May I rewrite the gate?"),
    )
    .unwrap();
    let settled = questions::answer(
        &score.case.db,
        master,
        0,
        &Answer {
            picks: vec![0],
            comment: None,
        },
    )
    .unwrap();
    assert_eq!(settled, questions::Settlement::Settled);
    let stopped = score.agent();
    let tx = score.case.db.unchecked_transaction().unwrap();
    let action = TeamAction::StopAgent {
        project_id: score.case.project.clone(),
        agent: stopped.id,
    };
    let outcome = crate::team::execute(&tx, &ModelAccess::default(), &action).unwrap();
    assert_eq!(outcome, crate::protocol::Outcome::Applied);
    tx.commit().unwrap();

    assert!(crate::team::owner_stopped(&score.case.db, stopped.id).unwrap());
    let pending = questions::pending(&score.case.db, &score.case.project).unwrap();
    assert!(pending.is_empty(), "the answer stands: {pending:?}");
    let card = score.report();
    assert!(card.acts.is_empty(), "{:?}", card.acts);
    assert_eq!(card.coordination.measured.acts(), 0);
}

#[test]
fn owner_messages_answers_and_stops_count_per_task_against_the_baseline() {
    let mut score = Score::new();
    score.settings(Settings {
        window: Window::FromDay(day_offset(&score.today(), -5)),
        baseline: Baseline::FirstDays,
        daily_question: DailyQuestion::Off,
    });
    score.message("Please make the gate pass");
    let master = questions::ask(
        &score.case.db,
        &score.case.project,
        &Asker::Master(41),
        &ask("May I rewrite the gate?"),
    )
    .unwrap();
    questions::answer(
        &score.case.db,
        master,
        0,
        &Answer {
            picks: vec![0],
            comment: None,
        },
    )
    .unwrap();
    let stopped = score.agent();
    let tx = score.case.db.unchecked_transaction().unwrap();
    let action = TeamAction::StopAgent {
        project_id: score.case.project.clone(),
        agent: stopped.id,
    };
    let outcome = crate::team::execute(&tx, &ModelAccess::default(), &action).unwrap();
    assert_eq!(outcome, crate::protocol::Outcome::Applied);
    tx.commit().unwrap();
    score.clean_task("Make the gate pass");

    let card = score.report();
    let measured = &card.coordination.measured;
    assert_eq!(measured.messages, 1);
    assert_eq!(measured.answers, 1);
    assert_eq!(measured.interventions, 1);
    assert_eq!(measured.tasks, 1);
    assert_eq!(measured.acts(), 3);
    assert_eq!(card.coordination.baseline.acts(), 0);
    assert_eq!(card.coordination.baseline.tasks, 0);
    assert!(!card.met.coordination);
    let kinds: Vec<Kind> = card.acts.iter().map(|act| act.kind).collect();
    assert!(kinds.contains(&Kind::OwnerMessage), "{kinds:?}");
    assert!(kinds.contains(&Kind::Answer), "{kinds:?}");
    assert!(kinds.contains(&Kind::OwnerStop), "{kinds:?}");
}

#[test]
fn owner_stops_interrupts_and_restarts_from_the_agents_page_count_as_interventions() {
    let score = Score::new();
    for kind in [Kind::OwnerStop, Kind::OwnerInterrupt, Kind::OwnerRestart] {
        intervened(&score.case.root, &score.case.project, kind, "agent-7");
    }
    let card = score.report();
    let kinds: Vec<Kind> = card.acts.iter().map(|act| act.kind).collect();
    assert_eq!(
        kinds.len(),
        3,
        "every act on a running agent is listed: {kinds:?}"
    );
    for kind in [Kind::OwnerStop, Kind::OwnerInterrupt, Kind::OwnerRestart] {
        assert!(kinds.contains(&kind), "{kind:?} in {kinds:?}");
    }
    assert!(card.acts.iter().all(|act| act.subject == "agent-7"));
    let counted =
        card.coordination.baseline.interventions + card.coordination.measured.interventions;
    assert_eq!(counted, 3);
    assert_eq!(
        card.coordination.baseline.messages + card.coordination.measured.messages,
        0
    );
}

#[test]
fn an_agents_page_stop_of_a_hosted_agent_counts_once_after_the_runtime_stops_it() {
    let mut score = Score::new();
    let hosted = score.agent();
    let page_id = crate::terminal::AgentId::team(hosted.id).unwrap();
    intervened(
        &score.case.root,
        &score.case.project,
        Kind::OwnerStop,
        page_id.as_str(),
    );
    crate::team::owner_stop(&score.case.db, &score.case.project, hosted.id, now_ms()).unwrap();
    assert!(crate::team::owner_stopped(&score.case.db, hosted.id).unwrap());

    let card = score.report();
    let stops: Vec<&Act> = card
        .acts
        .iter()
        .filter(|act| act.kind == Kind::OwnerStop)
        .collect();
    assert_eq!(stops.len(), 1, "{:?}", card.acts);
    assert_eq!(stops[0].subject, page_id.as_str());
    let counted =
        card.coordination.baseline.interventions + card.coordination.measured.interventions;
    assert_eq!(counted, 1);
}

#[test]
fn a_master_cannot_settle_the_daily_question_from_chat() {
    let score = Score::new();
    score.settings(Settings {
        window: Window::Rolling,
        baseline: Baseline::FirstDays,
        daily_question: DailyQuestion::On,
    });
    let projects = [score.case.project.clone()];
    let id = ask_daily(&score.case.db, &projects, now_ms())
        .unwrap()
        .unwrap();
    let chat = questions::record_from_chat(&score.case.db, &score.case.project, 41, id, 0, 1);
    assert_eq!(chat.unwrap_err().0, "questions_not_found");
    let session = crate::protocol::SessionId::new("session-1").unwrap();
    let from_session =
        questions::record_from_session(&score.case.db, &score.case.project, &session, id, 0, "Yes");
    assert_eq!(from_session.unwrap_err().0, "questions_not_found");

    let pending = questions::pending(&score.case.db, &score.case.project).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, id);
    let card = score.report();
    assert_eq!(card.answered_days, 0);
}

#[test]
fn the_rolling_window_holds_the_last_fourteen_days_and_moves_on() {
    let mut score = Score::new();
    assert_eq!(
        stored(&score.case.db).unwrap().settings.window,
        Window::Rolling
    );
    score.clean_task("Make the gate pass");
    let now = now_ms();
    let card = report(&score.case.db, now, 0).unwrap();
    assert_eq!(card.real, 1);
    assert_eq!(
        card.window.end_ms,
        (day_number(&score.today()).unwrap() + 1) * DAY_MS
    );
    assert_eq!(
        card.window.start_ms,
        (day_number(&score.today()).unwrap() - 13) * DAY_MS
    );
    let later = report(&score.case.db, now + 14 * DAY_MS, 0).unwrap();
    assert_eq!(later.real, 0);
}

#[test]
fn a_window_from_a_start_day_holds_fourteen_days_from_that_day() {
    let mut score = Score::new();
    score.clean_task("Make the gate pass");
    let today = score.today();
    for (start, real) in [(0, 1), (-13, 1), (-14, 0), (1, 0)] {
        score.settings(Settings {
            window: Window::FromDay(day_offset(&today, start)),
            baseline: Baseline::FirstDays,
            daily_question: DailyQuestion::Off,
        });
        let card = score.report();
        assert_eq!(card.real, real, "start {start}");
        let begins = (day_number(&today).unwrap() + start) * DAY_MS;
        assert_eq!(card.window.start_ms, begins, "start {start}");
        assert!(card.window.end_ms <= begins + WINDOW_DAYS * DAY_MS);
    }
}

#[test]
fn the_first_days_baseline_is_the_windows_first_three_days() {
    let mut score = Score::new();
    let today = score.today();
    score.settings(Settings {
        window: Window::FromDay(today.clone()),
        baseline: Baseline::FirstDays,
        daily_question: DailyQuestion::Off,
    });
    score.message("Please make the gate pass");
    score.clean_task("Make the gate pass");
    let card = score.report();
    let start = day_number(&today).unwrap() * DAY_MS;
    assert_eq!(card.baseline.start_ms, start);
    assert_eq!(card.baseline.end_ms, start + BASELINE_DAYS * DAY_MS);
    assert_eq!(card.measured.start_ms, start + BASELINE_DAYS * DAY_MS);
    assert_eq!(card.coordination.baseline.messages, 1);
    assert_eq!(card.coordination.baseline.tasks, 1);
    assert_eq!(card.coordination.measured.acts(), 0);
}

#[test]
fn the_previous_window_baseline_is_the_fourteen_days_before_the_window() {
    let mut score = Score::new();
    let today = score.today();
    score.settings(Settings {
        window: Window::FromDay(today.clone()),
        baseline: Baseline::PreviousWindow,
        daily_question: DailyQuestion::Off,
    });
    score.message("Please make the gate pass");
    score.clean_task("Make the gate pass");
    let card = score.report();
    let start = day_number(&today).unwrap() * DAY_MS;
    assert_eq!(card.baseline.start_ms, start - WINDOW_DAYS * DAY_MS);
    assert_eq!(card.baseline.end_ms, start);
    assert_eq!(card.measured.start_ms, start);
    assert_eq!(card.coordination.measured.messages, 1);
    assert_eq!(card.coordination.measured.tasks, 1);
    assert_eq!(card.coordination.baseline.acts(), 0);
}

#[test]
fn the_daily_question_is_off_by_default_and_asks_nothing() {
    let score = Score::new();
    assert_eq!(
        stored(&score.case.db).unwrap().settings.daily_question,
        DailyQuestion::Off
    );
    let asked = ask_daily(
        &score.case.db,
        std::slice::from_ref(&score.case.project),
        now_ms(),
    )
    .unwrap();
    assert_eq!(asked, None);
    let pending = questions::pending(&score.case.db, &score.case.project).unwrap();
    assert!(pending.is_empty());
}

#[test]
fn the_daily_question_on_asks_once_a_day_and_a_yes_counts_the_day() {
    let score = Score::new();
    score.settings(Settings {
        window: Window::Rolling,
        baseline: Baseline::FirstDays,
        daily_question: DailyQuestion::On,
    });
    let projects = [score.case.project.clone()];
    let now = now_ms();
    let id = ask_daily(&score.case.db, &projects, now).unwrap().unwrap();
    assert_eq!(
        ask_daily(&score.case.db, &projects, now + 60_000).unwrap(),
        None
    );
    let pending = questions::pending(&score.case.db, &score.case.project).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, id);
    assert_eq!(pending[0].asker, EngineSource::Scorecard.name().unwrap());
    questions::answer(
        &score.case.db,
        id,
        0,
        &Answer {
            picks: vec![0],
            comment: None,
        },
    )
    .unwrap();

    let card = score.report();
    assert_eq!(card.chosen_days, 1);
    assert_eq!(card.answered_days, 1);
    assert_eq!(
        card.days,
        vec![DayAnswer {
            day: score.today(),
            yes: true
        }]
    );
    assert_eq!(card.coordination.measured.answers, 0);
    let told: i64 = score
        .case
        .db
        .query_row("SELECT COUNT(*) FROM team_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(told, 0, "nobody is told the owner's daily answer");
    let tomorrow = ask_daily(&score.case.db, &projects, now + DAY_MS)
        .unwrap()
        .unwrap();
    assert_ne!(tomorrow, id);
    questions::answer(
        &score.case.db,
        tomorrow,
        0,
        &Answer {
            picks: vec![1],
            comment: None,
        },
    )
    .unwrap();
    let card = report(&score.case.db, now_ms(), 0).unwrap();
    assert_eq!(card.answered_days, 1, "both answers fell on today");
    assert_eq!(card.chosen_days, 1, "a day with a yes is chosen");
}

#[test]
fn a_stale_settings_write_is_refused_and_changes_nothing() {
    let score = Score::new();
    let first = stored(&score.case.db).unwrap();
    assert_eq!(first.version, 0);
    assert_eq!(first.settings, Settings::default());
    let on = Settings {
        window: Window::Rolling,
        baseline: Baseline::PreviousWindow,
        daily_question: DailyQuestion::On,
    };
    assert_eq!(set(&score.case.db, &on, 0).unwrap().unwrap(), 1);
    let stale = set(&score.case.db, &Settings::default(), 0)
        .unwrap()
        .unwrap_err();
    assert_eq!(stale.current, 1);
    let now = stored(&score.case.db).unwrap();
    assert_eq!(now.settings, on);
    assert_eq!(now.version, 1);
}

#[test]
fn a_start_day_that_is_no_calendar_day_is_refused() {
    let score = Score::new();
    for day in ["2026-02-30", "2026-13-01", "26-11-03", "2026-11-3", ""] {
        let settings = Settings {
            window: Window::FromDay(day.into()),
            baseline: Baseline::FirstDays,
            daily_question: DailyQuestion::Off,
        };
        let refused = set(&score.case.db, &settings, 0).unwrap_err();
        assert_eq!(refused.0, "scorecard_settings_invalid", "{day}");
    }
    assert_eq!(stored(&score.case.db).unwrap().version, 0);
}

#[test]
fn every_option_of_every_setting_round_trips_through_the_store() {
    let score = Score::new();
    let mut version = 0;
    for window in [Window::Rolling, Window::FromDay("2026-11-03".into())] {
        for baseline in Baseline::ALL {
            for daily_question in DailyQuestion::ALL {
                let settings = Settings {
                    window: window.clone(),
                    baseline,
                    daily_question,
                };
                version = set(&score.case.db, &settings, version).unwrap().unwrap();
                assert_eq!(stored(&score.case.db).unwrap().settings, settings);
            }
        }
    }
    assert_eq!(version, 8);
}

#[test]
fn days_count_in_the_readers_offset() {
    assert_eq!(day_label(0, 0), "1970-01-01");
    assert_eq!(day_label(0, -60), "1969-12-31");
    assert_eq!(day_number("2024-02-29"), Some(19_782));
    assert_eq!(day_label(19_782 * DAY_MS, 0), "2024-02-29");
    assert_eq!(day_number("2023-02-29"), None);
}

#[test]
fn the_ledger_keeps_at_most_marks_kept_max_rows() {
    let score = Score::new();
    let tx = Transaction::new_unchecked(&score.case.db, TransactionBehavior::Immediate).unwrap();
    for at in 0..=MARKS_KEPT_MAX {
        mark(&tx, &score.case.project, Kind::OwnerMessage, "1", at).unwrap();
    }
    tx.commit().unwrap();
    let (kept, oldest): (i64, i64) = score
        .case
        .db
        .query_row(
            "SELECT COUNT(*), MIN(at_ms) FROM scorecard_marks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(kept, MARKS_KEPT_MAX);
    assert_eq!(oldest, 1);
}
