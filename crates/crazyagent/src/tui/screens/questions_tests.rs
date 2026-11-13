use super::new_project_frames::{check_frames, compose_pending};
use super::new_project_view::Look;
use super::project::{ProjectScreen, Tab};
use super::project_frames::clocks;
use super::project_view::project_screen;
use super::{Place, ProjectTab};
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse};
use crate::project_setup::{Platform, ProjectSetup};
use crate::protocol::{ProjectId, ProjectSpec};
use crate::questions::{Answer, Ask, Asker, QuestionKind, QuestionStatus, Settlement};
use crate::tui::client::OwnerEvent;
use crate::tui::client::questions::{QuestionsEvent, QuestionsQuery, QuestionsReply};
use crate::tui::ctx::Load;
use crate::tui::keys::Key;
use crate::tui::render_tests::{check, ready, render, shell_at};
use crate::tui::seam::{Arrival, ProjectCard};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::os::unix::fs::PermissionsExt;

const NOW: i64 = 1_793_000_000_000;

/// A running engine with one set-up project, an owner controller attached to it and the
/// engine root, so a test asks questions the way an agent's ask tool logs them.
struct Engine {
    root: tempfile::TempDir,
    _home: tempfile::TempDir,
    _stop: tokio::sync::watch::Sender<bool>,
    _service: EngineService,
    controller: OwnerController,
    project: ProjectId,
}

impl Engine {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-questions-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let config = crate::tui::test_home::engine_config();
        let service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        let ordinary = Client::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap();
        let (_, mut controller) =
            OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
                .await
                .unwrap();
        let home = crate::tui::test_home::projects_home(&mut controller, 1).await;
        let project = ProjectId::new("plant-shop").unwrap();
        let created = controller
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Plant Shop".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "An online shop for house plants.".into(),
                    tech_stack: vec![],
                    platforms: vec![Platform::Website],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                2,
            )
            .await
            .unwrap();
        assert!(matches!(
            created,
            OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
        ));
        Self {
            root,
            _home: home,
            _stop: stop,
            _service: service,
            controller,
            project,
        }
    }

    /// Logs one question from the project's master, as its ask tool does. Inputs: the
    /// question, its options and the recommended one. Output: the question id.
    fn ask(&self, question: &str, options: &[&str], recommended: &str) -> u64 {
        self.ask_of(QuestionKind::Single, question, options, Some(recommended))
    }

    /// Logs one question of a kind from the project's master. Inputs: the kind, the
    /// question, its options and the recommended answer. Output: the question id.
    fn ask_of(
        &self,
        kind: QuestionKind,
        question: &str,
        options: &[&str],
        recommended: Option<&str>,
    ) -> u64 {
        let db = crate::questions::open(self.root.path()).unwrap();
        crate::questions::ask(
            &db,
            &self.project,
            &Asker::Master(1),
            &Ask {
                question: question.into(),
                kind,
                options: options.iter().map(|option| (*option).to_owned()).collect(),
                recommended: recommended.map(str::to_owned),
                context: Some("The shop keeps orders and stock.".into()),
            },
        )
        .unwrap()
    }

    /// Runs one request of the Questions tab through the owner controller, as the TUI's
    /// owner worker does, and hands the reply back to the screen. Input: the screen.
    /// Output: the query sent, or none when nothing was due.
    async fn pump(&mut self, screen: &mut ProjectScreen, now_ms: i64) -> Option<QuestionsQuery> {
        let query = screen.questions.next_query(now_ms)?;
        let result = match query.clone() {
            QuestionsQuery::List { project } => self
                .controller
                .questions_list(project, None)
                .await
                .map(QuestionsReply::Page),
            QuestionsQuery::Answer {
                id,
                version,
                answer,
                ..
            } => self
                .controller
                .questions_answer(id, version, answer)
                .await
                .map(QuestionsReply::Settled),
            QuestionsQuery::Dismiss { id, version, .. } => self
                .controller
                .questions_dismiss(id, version)
                .await
                .map(QuestionsReply::Settled),
            QuestionsQuery::Counts => self
                .controller
                .questions_counts()
                .await
                .map(QuestionsReply::Counts),
        };
        screen.questions.apply(&QuestionsEvent {
            query: query.clone(),
            result: result.map_err(|error| error.0),
        });
        Some(query)
    }

    /// Reads one question back from the engine. Input: its id. Output: its status, answer
    /// and version.
    async fn status(&self, id: u64) -> (QuestionStatus, Option<String>, u64) {
        let page = self
            .controller
            .questions_list(self.project.clone(), None)
            .await
            .unwrap();
        let question = page.items.into_iter().find(|item| item.id == id).unwrap();
        (question.status, question.answer, question.version)
    }
}

fn screen(engine: &Engine) -> ProjectScreen {
    let mut screen = ProjectScreen::new(engine.project.clone(), "Plant Shop".into(), None);
    screen.open(Tab::Questions);
    screen
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn typed(screen: &mut ProjectScreen, text: &str) {
    for character in text.chars() {
        assert!(screen.questions.key(key(KeyCode::Char(character))));
    }
}

fn shown(screen: &mut ProjectScreen, width: u16, height: u16) -> String {
    shown_pending(screen, width, height, 0)
}

/// Draws the project screen as the shell shows it with a pending questions badge. Takes
/// the screen, the terminal size and the pending count. Returns the frame text.
fn shown_pending(screen: &mut ProjectScreen, width: u16, height: u16, pending: u64) -> String {
    let look = Look::plain();
    screen.width = width;
    let frame = project_screen(screen, width, height, &look);
    clocks(&compose_pending(&frame, width, height, "", &look, pending))
}

#[tokio::test]
async fn questions_page_lists_pending_first_with_the_recommendation() {
    let mut engine = Engine::start().await;
    let newsletter = engine.ask(
        "Should the shop send a weekly newsletter?",
        &["Yes", "No"],
        "No",
    );
    engine.ask(
        "Which hosting region should the shop use?",
        &["Europe", "North America"],
        "Europe",
    );
    assert_eq!(
        engine
            .controller
            .questions_dismiss(newsletter, 0)
            .await
            .unwrap(),
        Settlement::Settled
    );
    let mut screen = screen(&engine);
    assert_eq!(
        engine.pump(&mut screen, NOW).await,
        Some(QuestionsQuery::List {
            project: engine.project.clone()
        })
    );
    assert_eq!(engine.pump(&mut screen, NOW + 1).await, None, "one read");
    let pending: u64 = engine
        .controller
        .questions_counts()
        .await
        .unwrap()
        .iter()
        .map(|(_, count)| u64::from(*count))
        .sum();
    assert_eq!(pending, 1);

    let narrow = shown_pending(&mut screen, 80, 24, pending);
    let lines: Vec<&str> = narrow.lines().collect();
    assert!(
        lines[0].contains("Home  /  Plant Shop  /  Questions"),
        "{narrow}"
    );
    let first = lines
        .iter()
        .position(|line| line.contains("Which hosting region"))
        .unwrap();
    let second = lines
        .iter()
        .position(|line| line.contains("Should the shop send a weekly newsl…"))
        .unwrap();
    assert!(first < second, "pending first:\n{narrow}");
    assert!(lines[first].contains("Waiting") && lines[second].contains("Dismissed"));
    assert!(narrow.contains("1. Europe (recommended)"), "{narrow}");
    assert!(
        lines.last().unwrap().starts_with(" a Answer  d Dismiss")
            && lines.last().unwrap().ends_with("1 question  ? Help"),
        "the badge shows on the Questions page too:\n{narrow}"
    );
    let wide = shown_pending(&mut screen, 120, 36, pending);
    check_frames(&[
        ("questions-80".to_owned(), narrow),
        ("questions-120".to_owned(), wide),
    ]);
}

#[tokio::test]
async fn answer_sends_the_version_read() {
    let mut engine = Engine::start().await;
    let region = engine.ask(
        "Which hosting region should the shop use?",
        &["Europe", "North America"],
        "Europe",
    );
    let mut screen = screen(&engine);
    engine.pump(&mut screen, NOW).await.unwrap();

    assert!(screen.questions.key(key(KeyCode::Char('a'))));
    assert!(screen.questions.editing());
    typed(&mut screen, "x");
    assert!(
        !shown(&mut screen, 80, 24).contains("[ x"),
        "letters type only once the comment line is open"
    );
    assert!(screen.questions.key(key(KeyCode::Char('c'))));
    typed(&mut screen, "Europe, close to the owner.");
    assert_eq!(
        engine
            .controller
            .questions_answer(
                region,
                0,
                Answer {
                    picks: vec![1],
                    comment: None
                }
            )
            .await
            .unwrap(),
        Settlement::Settled,
        "answered elsewhere after the page was read"
    );
    assert!(screen.questions.key(key(KeyCode::Enter)));
    assert!(!screen.questions.editing());
    let sent = engine.pump(&mut screen, NOW + 1).await.unwrap();
    assert_eq!(
        sent,
        QuestionsQuery::Answer {
            project: engine.project.clone(),
            id: region,
            version: 0,
            answer: Answer {
                picks: Vec::new(),
                comment: Some("Europe, close to the owner.".into())
            },
        }
    );
    assert_eq!(
        engine.status(region).await,
        (QuestionStatus::Answered, Some("North America".into()), 1),
        "the stale version changed nothing"
    );
    assert!(
        shown(&mut screen, 80, 24).contains("answered or changed since you read it"),
        "the owner is told"
    );

    let payment = engine.ask(
        "Which payment provider should checkout use?",
        &["Stripe", "Adyen"],
        "Stripe",
    );
    assert!(matches!(
        engine.pump(&mut screen, NOW + 2).await,
        Some(QuestionsQuery::List { .. })
    ));
    assert!(shown(&mut screen, 80, 24).contains("Which payment provider"));
    assert!(
        screen.questions.key(key(KeyCode::Up)),
        "the answered one stays chosen"
    );
    assert!(screen.questions.key(key(KeyCode::Char('a'))));
    assert!(screen.questions.key(key(KeyCode::Enter)));
    assert!(
        shown(&mut screen, 80, 24).contains("Pick an option or add a comment."),
        "Enter with nothing picked sends nothing"
    );
    assert!(screen.questions.editing());
    assert!(screen.questions.key(key(KeyCode::Char(' '))));
    assert!(screen.questions.key(key(KeyCode::Enter)));
    assert_eq!(
        engine.pump(&mut screen, NOW + 3).await.unwrap(),
        QuestionsQuery::Answer {
            project: engine.project.clone(),
            id: payment,
            version: 0,
            answer: Answer {
                picks: vec![0],
                comment: None
            },
        },
        "the box starts on the recommended option, and Space picks it"
    );
    assert_eq!(
        engine.status(payment).await,
        (QuestionStatus::Answered, Some("Stripe".into()), 1)
    );
    assert!(shown(&mut screen, 80, 24).contains("Answer sent. The asker gets it once."));
}

#[tokio::test]
async fn footer_badge_counts_pending_questions() {
    let engine = Engine::start().await;
    engine.ask(
        "Which hosting region should the shop use?",
        &["Europe", "North America"],
        "Europe",
    );
    engine.ask(
        "Should the shop send a weekly newsletter?",
        &["Yes", "No"],
        "No",
    );
    let mut ctx = ready(80, 24);
    ctx.store.projects = Load::Ready(vec![ProjectCard {
        id: engine.project.clone(),
        name: "Plant Shop".into(),
        about: "An online shop for house plants.".into(),
        platforms: vec![Platform::Website],
        pool: None,
        pool_id: None,
        research: Default::default(),
        created_ms: Some(NOW),
        activity: Default::default(),
    }]);
    let mut shell = shell_at(ctx, Place::Home);
    let mut asked = Vec::new();
    while let Some(request) = shell.ctx.next_request() {
        asked.push(format!("{request:?}"));
    }
    assert!(asked.contains(&"Questions".to_owned()), "{asked:?}");
    let before = render(&mut shell, 80, 24);
    assert!(
        !before.last().unwrap().contains("question"),
        "no badge before a read"
    );

    let counts = |counts| {
        Arrival::Owner(Box::new(OwnerEvent::Questions(Box::new(QuestionsEvent {
            query: QuestionsQuery::Counts,
            result: Ok(QuestionsReply::Counts(counts)),
        }))))
    };
    let read = engine.controller.questions_counts().await.unwrap();
    assert_eq!(read, [(engine.project.clone(), 2)]);
    shell.arrive(counts(read));
    let home = render(&mut shell, 80, 24);
    assert!(
        home.last().unwrap().ends_with("2 questions  ? Help "),
        "{home:?}"
    );
    assert!(
        shell.ctx.message().is_none(),
        "the first read only fills the badge"
    );
    check("questions-badge-80", &home, &[]);

    engine.ask(
        "Which payment provider should checkout use?",
        &["Stripe", "Adyen"],
        "Stripe",
    );
    shell.arrive(counts(engine.controller.questions_counts().await.unwrap()));
    let arrived = render(&mut shell, 80, 24);
    assert!(arrived.last().unwrap().ends_with("3 questions  ? Help "));
    assert!(
        arrived[22].contains("New question in Plant Shop."),
        "{arrived:?}"
    );
    check("questions-new-80", &arrived, &[]);

    shell.key(Key::Enter);
    assert!(
        matches!(
            shell.place(),
            Some(Place::Project {
                tab: ProjectTab::Overview,
                ..
            })
        ),
        "{:?}",
        shell.place()
    );
    let project = render(&mut shell, 80, 24);
    assert!(
        project.last().unwrap().ends_with("3 questions  ? Help "),
        "the badge shows on every screen: {project:?}"
    );
}

#[tokio::test]
async fn a_long_question_is_marked_cut_reads_whole_on_v_and_keeps_its_options_in_view() {
    let mut engine = Engine::start().await;
    for topic in ["hosting region", "payment provider", "newsletter tool"] {
        engine.ask(
            &format!("Which {topic} should the shop use?"),
            &["Yes", "No"],
            "Yes",
        );
    }
    let mut question: String = (1..=60).map(|word| format!("word{word:02} ")).collect();
    question.push_str("QUESTION-END?");
    let mut context: String = (1..=120).map(|word| format!("fact{word:03} ")).collect();
    context.push_str("CONTEXT-END");
    let db = crate::questions::open(engine.root.path()).unwrap();
    crate::questions::ask(
        &db,
        &engine.project,
        &Asker::Master(2),
        &Ask {
            question,
            kind: crate::questions::QuestionKind::Single,
            options: vec![
                "Keep it".into(),
                "Change it".into(),
                "Ask again later".into(),
            ],
            recommended: Some("Keep it".into()),
            context: Some(context),
        },
    )
    .unwrap();
    let mut screen = screen(&engine);
    engine.pump(&mut screen, NOW).await.unwrap();
    for _ in 0..3 {
        assert!(screen.questions.key(key(KeyCode::Down)));
    }
    let page = shown(&mut screen, 80, 24);
    assert!(
        page.contains("... v reads all of it"),
        "a cut text says so:\n{page}"
    );
    assert!(
        !page.contains("QUESTION-END") && !page.contains("CONTEXT-END"),
        "{page}"
    );
    assert!(page.contains("v Read all"), "{page}");

    assert!(screen.questions.key(key(KeyCode::Char('a'))));
    let answering = shown(&mut screen, 80, 24);
    for option in [
        "( ) 1. Keep it (recommended)",
        "( ) 2. Change it",
        "( ) 3. Ask again later",
        "( ) Add a comment",
        "Space picks an option",
    ] {
        assert!(
            answering.contains(option),
            "{option} stays in view while answering:\n{answering}"
        );
    }
    assert!(screen.questions.key(key(KeyCode::Char('c'))));
    let commenting = shown(&mut screen, 80, 24);
    for part in ["3. Ask again later", "(o) Add a comment", "Comment"] {
        assert!(
            commenting.contains(part),
            "{part} stays in view while commenting:\n{commenting}"
        );
    }
    assert!(screen.questions.key(key(KeyCode::Esc)));
    assert!(!screen.questions.editing());

    assert!(screen.questions.key(key(KeyCode::Char('v'))));
    let top = shown(&mut screen, 80, 24);
    assert!(
        top.contains("word01") && top.contains("QUESTION-END?"),
        "the whole question:\n{top}"
    );
    assert!(top.contains("Lines 1-"), "{top}");
    for _ in 0..20 {
        assert!(screen.questions.key(key(KeyCode::PageDown)));
    }
    let end = shown(&mut screen, 80, 24);
    assert!(
        end.contains("CONTEXT-END"),
        "the end of the context:\n{end}"
    );
    assert!(screen.questions.key(key(KeyCode::Up)));
    let back = shown(&mut screen, 80, 24);
    assert_ne!(back, end, "one line up from the end moves at once");
    assert!(screen.questions.key(key(KeyCode::Esc)));
    assert!(shown(&mut screen, 80, 24).contains("... v reads all of it"));
}

#[tokio::test]
async fn space_picks_two_options_of_a_multi_question_and_enter_sends_them_with_the_comment() {
    let mut engine = Engine::start().await;
    let platforms = engine.ask_of(
        QuestionKind::Multi,
        "Which platforms ship in V1?",
        &["Web", "Desktop", "Mobile"],
        Some("Web"),
    );
    let mut screen = screen(&engine);
    engine.pump(&mut screen, NOW).await.unwrap();
    for code in [
        KeyCode::Char('a'),
        KeyCode::Char(' '),
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Char(' '),
        KeyCode::Char('c'),
    ] {
        assert!(screen.questions.key(key(code)));
    }
    typed(&mut screen, "later");
    let drawn = shown(&mut screen, 80, 24);
    for part in [
        "[x] 1. Web (recommended)",
        "[ ] 2. Desktop",
        "[x] 3. Mobile",
        "[x] Add a comment",
        "[ later",
    ] {
        assert!(drawn.contains(part), "{part}:\n{drawn}");
    }
    assert!(screen.questions.key(key(KeyCode::Enter)));
    assert_eq!(
        engine.pump(&mut screen, NOW + 1).await.unwrap(),
        QuestionsQuery::Answer {
            project: engine.project.clone(),
            id: platforms,
            version: 0,
            answer: Answer {
                picks: vec![0, 2],
                comment: Some("later".into()),
            },
        }
    );
    assert_eq!(
        engine.status(platforms).await,
        (
            QuestionStatus::Answered,
            Some("Web; Mobile\nComment: later".into()),
            1
        )
    );
}

#[tokio::test]
async fn space_replaces_the_pick_of_a_single_question_and_a_comment_can_stand_alone() {
    let mut engine = Engine::start().await;
    let region = engine.ask(
        "Which hosting region should the shop use?",
        &["Europe", "North America"],
        "Europe",
    );
    let mut screen = screen(&engine);
    engine.pump(&mut screen, NOW).await.unwrap();
    for code in [
        KeyCode::Char('a'),
        KeyCode::Char(' '),
        KeyCode::Down,
        KeyCode::Char(' '),
    ] {
        assert!(screen.questions.key(key(code)));
    }
    let drawn = shown(&mut screen, 80, 24);
    assert!(
        drawn.contains("( ) 1. Europe (recommended)") && drawn.contains("(o) 2. North America"),
        "one pick at a time:\n{drawn}"
    );
    assert!(screen.questions.key(key(KeyCode::Char(' '))));
    assert!(screen.questions.key(key(KeyCode::Down)));
    assert!(screen.questions.key(key(KeyCode::Char(' '))));
    typed(&mut screen, "Neither, a CDN first.");
    assert!(screen.questions.key(key(KeyCode::Enter)));
    assert_eq!(
        engine.pump(&mut screen, NOW + 1).await.unwrap(),
        QuestionsQuery::Answer {
            project: engine.project.clone(),
            id: region,
            version: 0,
            answer: Answer {
                picks: Vec::new(),
                comment: Some("Neither, a CDN first.".into()),
            },
        }
    );
    assert_eq!(
        engine.status(region).await,
        (
            QuestionStatus::Answered,
            Some("Neither, a CDN first.".into()),
            1
        )
    );
}

#[tokio::test]
async fn a_text_question_opens_its_answer_line_at_once() {
    let mut engine = Engine::start().await;
    let name = engine.ask_of(
        QuestionKind::Text,
        "What should the shop be called?",
        &[],
        Some("Fern and Co"),
    );
    let mut screen = screen(&engine);
    engine.pump(&mut screen, NOW).await.unwrap();
    assert!(screen.questions.key(key(KeyCode::Char('a'))));
    let drawn = shown(&mut screen, 80, 24);
    assert!(
        drawn.contains("Your answer") && !drawn.contains("Add a comment"),
        "{drawn}"
    );
    assert!(screen.questions.key(key(KeyCode::Enter)));
    assert!(screen.questions.editing(), "nothing typed sends nothing");
    typed(&mut screen, "Leaf c and co");
    assert!(screen.questions.key(key(KeyCode::Enter)));
    assert_eq!(
        engine.pump(&mut screen, NOW + 1).await.unwrap(),
        QuestionsQuery::Answer {
            project: engine.project.clone(),
            id: name,
            version: 0,
            answer: Answer {
                picks: Vec::new(),
                comment: Some("Leaf c and co".into()),
            },
        }
    );
}

/// One pending text question from a team sub-agent, as the engine's page names it: its kept
/// name, if any, and whether it is urgent and its asker paused. Inputs: the id, the agent,
/// the name and both marks. Output: the question.
fn team_question(
    id: u64,
    agent: i64,
    name: Option<&str>,
    urgent: bool,
    paused: bool,
) -> crate::questions::Question {
    crate::questions::Question {
        id,
        project: ProjectId::new("plant-shop").unwrap(),
        asker: crate::questions::Asker::Agent(agent).name(),
        question: format!("Question {id} from agent {agent}?"),
        kind: QuestionKind::Text,
        options: Vec::new(),
        recommended: None,
        context: None,
        status: QuestionStatus::Pending,
        version: 0,
        answer: None,
        answered_by: None,
        created_ms: u64::try_from(NOW).unwrap(),
        settled_ms: None,
        closed_by: None,
        asker_name: name.map(str::to_owned),
        urgent,
        paused,
    }
}

/// V241-QUESTION-UX P1, P3 (S4-R1, DEC-141): a team question names its asker by the name the
/// engine sends, falls back to the agent's number only when no name came, and says
/// "Urgent" and "The agent is paused until you answer." only while each holds, in the
/// desktop card's words.
#[test]
fn a_team_question_names_its_asker_and_says_urgent_and_paused_only_while_they_hold() {
    assert_eq!(crate::questions::URGENT_WORD, "Urgent");
    assert_eq!(
        crate::questions::PAUSED_NOTE,
        "The agent is paused until you answer."
    );
    let project = ProjectId::new("plant-shop").unwrap();
    let mut screen = ProjectScreen::new(project.clone(), "Plant Shop".into(), None);
    screen.open(Tab::Questions);
    let query = screen.questions.next_query(NOW).unwrap();
    assert_eq!(query, QuestionsQuery::List { project });
    let page = crate::questions::Page {
        items: vec![
            team_question(1, 7, Some("Ada (@menu-prices)"), true, true),
            team_question(2, 8, None, true, false),
            team_question(3, 9, Some("Lin"), false, false),
        ],
        next: None,
    };
    screen.questions.apply(&QuestionsEvent {
        query,
        result: Ok(QuestionsReply::Page(page)),
    });

    let first = shown(&mut screen, 120, 34);
    assert!(
        first
            .lines()
            .any(|line| line.contains("From") && line.contains("Ada (@menu-prices)")),
        "{first}"
    );
    assert!(first.contains("Urgent"), "{first}");
    assert!(
        first.contains("The agent is paused until you answer."),
        "{first}"
    );

    assert!(screen.questions.key(key(KeyCode::Down)));
    let second = shown(&mut screen, 120, 34);
    assert!(
        second
            .lines()
            .any(|line| line.contains("From") && line.contains("agent 8")),
        "no name came, so its number: {second}"
    );
    assert!(second.contains("Urgent"), "{second}");
    assert!(
        !second.contains("The agent is paused until you answer."),
        "the pause no longer holds: {second}"
    );

    assert!(screen.questions.key(key(KeyCode::Down)));
    let third = shown(&mut screen, 120, 34);
    assert!(
        third
            .lines()
            .any(|line| line.contains("From") && line.contains("Lin")),
        "{third}"
    );
    assert!(
        !third.contains("Urgent") && !third.contains("paused until you answer"),
        "{third}"
    );
}
