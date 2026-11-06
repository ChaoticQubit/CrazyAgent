use super::*;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::team::agent_messages::{
    AgentMessage, AgentMessages, Cursor, EntryKind, MessageSettings, PAGE_MAX,
    RESULT_PREVIEW_BYTES_MAX, ResultsShown, Speaker,
};
use crate::team::events::{self, EventKind};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{AgentId, Host};
use rusqlite::params;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

const PROJECT: &str = "setup-pooled";
const DELIVERY_WAIT: Duration = Duration::from_secs(30);
const SHORT_RESULT: &str = "Footer fixed.";

/// What one timeline entry shows the owner: who it is from, what it is and its text.
type Shown = (Speaker, EntryKind, Option<String>);

/// What a finished entry shows: its task, its result and whether more is stored.
type Finished = (Option<i64>, Option<String>, bool);

/// Where the sub-agent's pane writes the lines typed into it: a file the test reads, or
/// nowhere for the tests that only count what the timeline keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Typed {
    Recorded,
    Discarded,
}

/// A private tmux server running one sub-agent's pane, as the engine's terminal host runs
/// it, killed when the test ends.
struct Pane {
    folder: tempfile::TempDir,
    tmux: PathBuf,
    host: Arc<dyn Host>,
    typed: PathBuf,
}

impl Pane {
    /// Starts the pane of a team agent on a new private tmux server. Takes the agent and
    /// where typed lines go. Returns the pane.
    fn start(agent: i64, typed: Typed) -> Self {
        let folder = tempfile::Builder::new()
            .prefix("ca-msgs-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = folder.path().canonicalize().unwrap();
        let tmux = installed(&base).expect("tmux must be installed on this machine");
        let host: Arc<dyn Host> = Arc::new(Tmux::open(
            Some(tmux.clone()),
            &base.join(crate::terminal::FOLDER),
        ));
        let recorded = base.join("typed.txt");
        let target = match typed {
            Typed::Recorded => recorded.clone(),
            Typed::Discarded => PathBuf::from("/dev/null"),
        };
        let command = InteractiveCommand {
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "exec cat > \"$0\"".to_owned(),
                target.to_str().unwrap().to_owned(),
            ],
            cwd: base,
            env: Vec::new(),
            transcript: Transcript::Unavailable,
        };
        host.spawn(&AgentId::team(agent).unwrap(), &command)
            .unwrap();
        Self {
            folder,
            tmux,
            host,
            typed: recorded,
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let socket = self
            .folder
            .path()
            .canonicalize()
            .unwrap()
            .join(crate::terminal::FOLDER)
            .join(SOCKET);
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

/// One real engine with a set-up project whose Messages settings the owner saved through
/// the engine, and one live sub-agent of it, with a real pane when the test types into it.
struct Timeline {
    engine: Engine,
    connection: rusqlite::Connection,
    agent: i64,
    pane: Option<Pane>,
}

impl Timeline {
    /// Starts the engine, sets the project up and saves the settings through the owner
    /// connection, then adds the live sub-agent and its pane. Takes the settings and the
    /// pane, none for a test that types nothing. Returns the timeline.
    async fn start(settings: MessageSettings, pane: Option<Typed>) -> Self {
        let engine = Engine::start().await;
        let mut owner = engine.owner().await;
        let response = owner
            .setup_project(project(), spec("Meal planner"), pooled_setup(vec![]), 1)
            .await
            .unwrap();
        assert_eq!(result(response), OwnerResult::Applied);
        let saved = owner
            .team_action(
                crate::team::TeamAction::MessageSettings {
                    project_id: project(),
                    settings,
                },
                2,
            )
            .await
            .unwrap();
        assert_eq!(result(saved), OwnerResult::Applied);
        drop(owner);
        let connection = rusqlite::Connection::open(engine.root.path().join("engine.db")).unwrap();
        connection.busy_timeout(ENGINE_WAIT).unwrap();
        connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,'sub','codex',0,'working',1,1)",
                [PROJECT],
            )
            .unwrap();
        let agent = connection.last_insert_rowid();
        let pane = pane.map(|typed| Pane::start(agent, typed));
        Self {
            engine,
            connection,
            agent,
            pane,
        }
    }

    /// Sends a master's `message` the way the live master's tool does, through the team
    /// event handlers the engine's team runtime serves it with. Takes the text.
    fn message(&self, text: &str) {
        self.said(events::HANDLERS.send_to_agent, text);
    }

    /// Sends a master's `answer` the way the live master's tool does. Takes the text.
    fn answer(&self, text: &str) {
        self.said(events::HANDLERS.answer, text);
    }

    fn said(&self, handler: crate::team::master::SendToAgent, text: &str) {
        let pane = self.pane.as_ref().expect("a test that types has a pane");
        let told = handler(
            &self.connection,
            pane.host.as_ref(),
            &project(),
            &format!("codex-{}", self.agent),
            text,
        );
        assert!(told.is_ok(), "{told:?}");
    }

    /// Waits until the agent's terminal took every line. Takes the lines.
    fn reached(&self, lines: &[&str]) {
        let pane = self.pane.as_ref().expect("a test that types has a pane");
        let deadline = Instant::now() + DELIVERY_WAIT;
        loop {
            let typed = std::fs::read_to_string(&pane.typed).unwrap_or_default();
            if lines
                .iter()
                .all(|line| typed.lines().any(|got| got == *line))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the pane never took {lines:?}: {typed:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Adds a task the agent works on. Takes who gave it, the prompt and the time. Returns
    /// the task id.
    fn task(&self, source: &str, prompt: &str, at: i64) -> i64 {
        self.connection
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms,started_ms) VALUES (?1,?2,'codex',0,?3,'running',?4,?5,?5)",
                params![PROJECT, source, prompt, self.agent, at],
            )
            .unwrap();
        self.connection.last_insert_rowid()
    }

    /// Ends a task with its result and records its end as the team runtime does. Takes the
    /// task, the result and the time.
    fn finish(&self, task: i64, result: &str, at: i64) {
        self.connection
            .execute(
                "UPDATE team_tasks SET status='done', result=?2, finished_ms=?3 WHERE id=?1",
                params![task, result, at],
            )
            .unwrap();
        assert!(
            events::task_ended(&self.connection, task, at)
                .unwrap()
                .is_some()
        );
    }

    /// Records one of the agent's events as the screen watch does. Takes the kind, the text
    /// and the time.
    fn event(&self, kind: EventKind, text: &str, at: i64) {
        assert!(
            events::record(
                &self.connection,
                &project(),
                self.agent,
                None,
                kind,
                text,
                at
            )
            .unwrap()
            .is_some()
        );
    }

    /// Records a prompt the owner typed into the agent's terminal, as the owner terminal
    /// connection does. Takes the text and the time.
    fn prompt(&self, text: &str, at: i64) {
        assert!(
            crate::team::direct::record(&self.connection, self.agent, text, at)
                .unwrap()
                .is_some()
        );
    }

    /// Reads one page of the agent's timeline through a fresh owner connection. Takes the
    /// cursor and the page size. Returns the page.
    async fn page(&self, before: Option<Cursor>, limit: Option<u16>) -> AgentMessages {
        self.engine
            .owner()
            .await
            .team_agent_messages(project(), self.agent, before, limit)
            .await
            .unwrap()
    }

    /// Reads the agent's whole timeline, newest first, page after page of the largest size.
    /// Returns every entry.
    async fn every_entry(&self) -> Vec<AgentMessage> {
        let mut owner = self.engine.owner().await;
        let mut all = Vec::new();
        let mut before = None;
        loop {
            let page = owner
                .team_agent_messages(project(), self.agent, before, Some(PAGE_MAX))
                .await
                .unwrap();
            let last = page.entries.last().map(AgentMessage::cursor);
            all.extend(page.entries);
            assert!(all.len() <= 10_000, "the timeline never ends");
            match (page.older, last) {
                (true, Some(cursor)) => before = Some(cursor),
                _ => return all,
            }
        }
    }

    async fn stop(self) {
        drop(self.pane);
        drop(self.connection);
        self.engine.stop().await;
    }
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn shown(page: &AgentMessages) -> Vec<Shown> {
    page.entries
        .iter()
        .map(|entry| (entry.from, entry.kind, entry.text.clone()))
        .collect()
}

fn entry(from: Speaker, kind: EntryKind, text: &str) -> Shown {
    (from, kind, Some(text.to_owned()))
}

fn texts(page: &AgentMessages) -> Vec<Option<String>> {
    page.entries
        .iter()
        .map(|entry| entry.text.clone())
        .collect()
}

fn notes(name: &str, numbers: impl DoubleEndedIterator<Item = usize>) -> Vec<Option<String>> {
    numbers.rev().map(|n| Some(format!("{name} {n}"))).collect()
}

fn earlier() -> i64 {
    crate::team::now_ms() - 60_000
}

/// Keep the master's messages, On: what the master typed into the agent with `message` and
/// `answer` reaches its terminal and shows in its timeline as the master's, newest first,
/// with the text it sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_master_messages_kept_the_timeline_shows_what_the_master_said() {
    let timeline = Timeline::start(
        MessageSettings {
            keep_master_messages: true,
            ..MessageSettings::default()
        },
        Some(Typed::Recorded),
    )
    .await;
    timeline.message("Check the login page first");
    timeline.answer("Use option 2");
    timeline.reached(&["Check the login page first", "Use option 2"]);
    let page = timeline.page(None, None).await;
    assert_eq!(
        shown(&page),
        vec![
            entry(Speaker::Master, EntryKind::Answer, "Use option 2"),
            entry(
                Speaker::Master,
                EntryKind::Message,
                "Check the login page first"
            ),
        ]
    );
    assert!(!page.older);
    timeline.stop().await;
}

/// Keep the master's messages, Off: the agent still gets what the master typed, the timeline
/// shows that the master sent a message and an answer without their text, and neither text is
/// in the reply or anywhere in the engine database.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_master_messages_not_kept_the_timeline_shows_only_that_the_master_said_something() {
    let message = "Rotate the staging key 4F7Q";
    let answer = "Option 3 then wait 9K2M";
    let timeline = Timeline::start(
        MessageSettings {
            keep_master_messages: false,
            ..MessageSettings::default()
        },
        Some(Typed::Recorded),
    )
    .await;
    timeline.message(message);
    timeline.answer(answer);
    timeline.reached(&[message, answer]);
    let page = timeline.page(None, None).await;
    assert_eq!(
        shown(&page),
        vec![
            (Speaker::Master, EntryKind::Answer, None),
            (Speaker::Master, EntryKind::Message, None),
        ],
        "the timeline keeps only the fact of each"
    );
    let reply = serde_json::to_string(&page).unwrap();
    for said in [message, answer] {
        assert!(!reply.contains(said), "{reply}");
        for file in std::fs::read_dir(timeline.engine.root.path()).unwrap() {
            let file = file.unwrap();
            if !file.file_name().to_string_lossy().starts_with("engine.db") {
                continue;
            }
            let bytes = std::fs::read(file.path()).unwrap();
            assert!(
                !bytes
                    .windows(said.len())
                    .any(|window| window == said.as_bytes()),
                "{said:?} is stored in {}",
                file.path().display()
            );
        }
    }
    timeline.stop().await;
}

/// Messages kept per project at one choice: one past that many messages typed into the agent,
/// its timeline holds exactly the newest that many, and the oldest made room.
async fn keeps_the_newest(kept: u32) {
    let timeline = Timeline::start(
        MessageSettings {
            kept_per_project: kept,
            ..MessageSettings::default()
        },
        Some(Typed::Discarded),
    )
    .await;
    let count = usize::try_from(kept).unwrap();
    for note in 0..=count {
        timeline.message(&format!("note {note}"));
    }
    let all = timeline.every_entry().await;
    assert!(
        all.iter()
            .all(|said| (said.from, said.kind) == (Speaker::Master, EntryKind::Message)),
        "only the master's messages are in this timeline"
    );
    let kept_texts: Vec<Option<String>> = all.into_iter().map(|said| said.text).collect();
    assert_eq!(
        kept_texts.len(),
        count,
        "the project keeps its newest {kept}"
    );
    assert_eq!(kept_texts, notes("note", 1..=count), "the oldest made room");
    timeline.stop().await;
}

/// Messages kept per project, 500.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_keeping_500_messages_shows_the_newest_500() {
    keeps_the_newest(500).await;
}

/// Messages kept per project, 2000.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_keeping_2000_messages_shows_the_newest_2000() {
    keeps_the_newest(2000).await;
}

/// Messages kept per project, 5000.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_keeping_5000_messages_shows_the_newest_5000() {
    keeps_the_newest(5000).await;
}

/// The agent's timeline with progress events shown or hidden: a question and a progress
/// event recorded as the screen watch records them. Takes the choice. Returns what shows.
async fn progress_with(progress_shown: bool) -> Vec<Shown> {
    let timeline = Timeline::start(
        MessageSettings {
            progress_shown,
            ..MessageSettings::default()
        },
        None,
    )
    .await;
    let at = earlier();
    timeline.event(EventKind::Question, "Which theme should the page use?", at);
    timeline.event(EventKind::Progress, "Halfway through the page", at + 1);
    let page = timeline.page(None, None).await;
    timeline.stop().await;
    shown(&page)
}

/// Progress events, Shown: the agent's progress shows beside its question.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_progress_shown_the_timeline_shows_progress_events() {
    assert_eq!(
        progress_with(true).await,
        vec![
            entry(
                Speaker::Engine,
                EntryKind::Progress,
                "Halfway through the page"
            ),
            entry(
                Speaker::Agent,
                EntryKind::Question,
                "Which theme should the page use?"
            ),
        ]
    );
}

/// Progress events, Hidden: the progress event leaves the timeline and the question stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_progress_hidden_the_timeline_shows_no_progress_events() {
    assert_eq!(
        progress_with(false).await,
        vec![entry(
            Speaker::Agent,
            EntryKind::Question,
            "Which theme should the page use?"
        )]
    );
}

/// A result longer than the preview: a first line, then lines up to about 5 KiB.
fn long_result() -> String {
    let mut text = String::from("Page built.\n");
    for line in 0..80 {
        text.push_str(&format!(
            "Line {line}: the header, footer and menu now match the design.\n"
        ));
    }
    let text = text.trim_end().to_owned();
    assert!(text.len() > RESULT_PREVIEW_BYTES_MAX * 2);
    text
}

/// The finished entries of an agent with one long and one short result, shown as the
/// choice says. Takes the choice. Returns each finished entry's task, result and whether
/// more is stored, with the long and short task ids.
async fn results_with(results: ResultsShown) -> (Vec<Finished>, i64, i64) {
    let timeline = Timeline::start(
        MessageSettings {
            results,
            ..MessageSettings::default()
        },
        None,
    )
    .await;
    let at = earlier();
    let long = timeline.task("master", "Build the page", at);
    timeline.finish(long, &long_result(), at + 1);
    let short = timeline.task("master", "Fix the footer", at + 2);
    timeline.finish(short, SHORT_RESULT, at + 3);
    let page = timeline.page(None, None).await;
    timeline.stop().await;
    let finished = page
        .entries
        .iter()
        .filter(|said| said.kind == EntryKind::Finished)
        .map(|said| (said.task, said.result.clone(), said.result_more))
        .collect();
    (finished, long, short)
}

/// Task results, first line then expand: a long result shows as a preview of at most
/// `RESULT_PREVIEW_BYTES_MAX` bytes that opens on its first line and says more is stored; a
/// short one shows whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_results_on_their_first_line_a_long_result_shows_as_a_preview_that_says_more() {
    let (finished, long, short) = results_with(ResultsShown::FirstLine).await;
    assert_eq!(finished.len(), 2, "{finished:?}");
    assert_eq!(
        finished[0],
        (Some(short), Some(SHORT_RESULT.to_owned()), false)
    );
    let (task, preview, more) = finished[1].clone();
    assert_eq!(task, Some(long));
    let preview = preview.unwrap_or_default();
    assert!(more, "the preview says the stored result is longer");
    assert!(
        preview.len() <= RESULT_PREVIEW_BYTES_MAX,
        "{}",
        preview.len()
    );
    assert!(preview.starts_with("Page built.\n"), "{preview:?}");
    assert!(long_result().starts_with(&preview), "{preview:?}");
}

/// Task results, full text: the long result shows whole and nothing more is stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_full_results_a_long_result_shows_whole() {
    let (finished, long, short) = results_with(ResultsShown::Full).await;
    assert_eq!(
        finished,
        vec![
            (Some(short), Some(SHORT_RESULT.to_owned()), false),
            (Some(long), Some(long_result()), false),
        ]
    );
}

/// The agent's timeline with the owner's own tasks and prompts shown or hidden: an owner
/// task, a master task, a prompt the owner typed into the agent and a master message.
/// Takes the choice. Returns what shows.
async fn owner_items_with(owner_items_shown: bool) -> Vec<Shown> {
    let timeline = Timeline::start(
        MessageSettings {
            owner_items_shown,
            ..MessageSettings::default()
        },
        Some(Typed::Recorded),
    )
    .await;
    let at = earlier();
    timeline.task("owner", "Fix the footer", at);
    timeline.task("master", "Build the page", at + 1);
    timeline.prompt("Use the blue theme", at + 2);
    timeline.message("Start with the header");
    timeline.reached(&["Start with the header"]);
    let page = timeline.page(None, None).await;
    timeline.stop().await;
    shown(&page)
}

/// Owner's tasks and prompts, Shown: the owner's task and prompt show as the owner's beside
/// the master's task and message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_owner_items_shown_the_timeline_shows_the_owner_s_tasks_and_prompts() {
    assert_eq!(
        owner_items_with(true).await,
        vec![
            entry(Speaker::Master, EntryKind::Message, "Start with the header"),
            entry(Speaker::Owner, EntryKind::Prompt, "Use the blue theme"),
            entry(Speaker::Master, EntryKind::Task, "Build the page"),
            entry(Speaker::Owner, EntryKind::Task, "Fix the footer"),
        ]
    );
}

/// Owner's tasks and prompts, Hidden: only the master's task and message stay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_owner_items_hidden_the_timeline_shows_only_the_master_s_words() {
    assert_eq!(
        owner_items_with(false).await,
        vec![
            entry(Speaker::Master, EntryKind::Message, "Start with the header"),
            entry(Speaker::Master, EntryKind::Task, "Build the page"),
        ]
    );
}

/// Page size at one choice: with seven entries more than a page, a read without a size
/// shows that many, newest first, says older entries remain, and the next page from the
/// last entry's cursor shows the seven oldest and no more.
async fn pages_of(size: u16) {
    let timeline = Timeline::start(
        MessageSettings {
            page_size: size,
            ..MessageSettings::default()
        },
        Some(Typed::Discarded),
    )
    .await;
    let total = usize::from(size) + 7;
    for step in 0..total {
        timeline.message(&format!("step {step}"));
    }
    let first = timeline.page(None, None).await;
    assert_eq!(texts(&first), notes("step", 7..total));
    assert!(first.older, "older entries remain after one page");
    assert_eq!(first.settings.page_size, size);
    let next = timeline
        .page(first.entries.last().map(AgentMessage::cursor), None)
        .await;
    assert_eq!(texts(&next), notes("step", 0..7));
    assert!(!next.older);
    timeline.stop().await;
}

/// Page size, 25.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_size_of_25_shows_25_entries_a_page() {
    pages_of(25).await;
}

/// Page size, 50.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_size_of_50_shows_50_entries_a_page() {
    pages_of(50).await;
}

/// Page size, 100.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_size_of_100_shows_100_entries_a_page() {
    pages_of(100).await;
}

/// Every setting the owner saves is what both owner reads give back: before a save the
/// timeline and the team status read the proposed defaults; after one save of a non-default
/// choice for all six, both read exactly it; a choice outside the lists is refused and
/// changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_six_settings_the_owner_saves_are_what_the_timeline_and_team_status_read() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let response = owner
        .setup_project(project(), spec("Meal planner"), pooled_setup(vec![]), 1)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let connection = rusqlite::Connection::open(engine.root.path().join("engine.db")).unwrap();
    connection.busy_timeout(ENGINE_WAIT).unwrap();
    connection
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,'sub','codex',0,'working',1,1)",
            [PROJECT],
        )
        .unwrap();
    let agent = connection.last_insert_rowid();
    drop(connection);
    let proposed = MessageSettings {
        keep_master_messages: true,
        kept_per_project: 2000,
        progress_shown: true,
        results: ResultsShown::FirstLine,
        owner_items_shown: true,
        page_size: 50,
    };
    let chosen = MessageSettings {
        keep_master_messages: false,
        kept_per_project: 5000,
        progress_shown: false,
        results: ResultsShown::Full,
        owner_items_shown: false,
        page_size: 100,
    };
    for (generation, saved) in [(None, proposed), (Some(2), chosen)] {
        if let Some(generation) = generation {
            let response = owner
                .team_action(
                    crate::team::TeamAction::MessageSettings {
                        project_id: project(),
                        settings: saved,
                    },
                    generation,
                )
                .await
                .unwrap();
            assert_eq!(result(response), OwnerResult::Applied);
        }
        let page = owner
            .team_agent_messages(project(), agent, None, None)
            .await
            .unwrap();
        assert_eq!(page.settings, saved);
        assert_eq!(
            owner.team_status(project()).await.unwrap().message_settings,
            saved
        );
    }
    let refused = owner
        .team_action(
            crate::team::TeamAction::MessageSettings {
                project_id: project(),
                settings: MessageSettings {
                    page_size: 75,
                    ..chosen
                },
            },
            3,
        )
        .await
        .map(|_| ())
        .map_err(|error| error.0);
    assert_eq!(refused, Err("team_message_settings_invalid".to_owned()));
    assert_eq!(
        owner.team_status(project()).await.unwrap().message_settings,
        chosen
    );
    drop(owner);
    engine.stop().await;
}
