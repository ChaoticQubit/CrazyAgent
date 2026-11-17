use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::owner_profile::OwnerProfile;
use crate::protocol::Topic;
use crate::tui::client::changes::ChangesQuery;
use crate::tui::client::intake::{IntakeJob, IntakeQuery};
use crate::tui::client::questions::QuestionsQuery;
use crate::tui::client::settings::SettingsJob;
use crate::tui::client::{Connection, OwnerEvent};
use crate::tui::seam::ShellRequest;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

const QUIET: Duration = Duration::from_secs(6);
const SETTLE_MAX: Duration = Duration::from_secs(30);
const NAMED_WITHIN: Duration = Duration::from_secs(2);

struct Live {
    connection: Connection,
    _service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    _root: tempfile::TempDir,
}

impl Live {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-watch-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let config = crate::tui::test_home::engine_config();
        let service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        let client = Client::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap();
        let mut live = Self {
            connection: Connection::connect_paired(client).unwrap(),
            _service: service,
            _stop: stop,
            _root: root,
        };
        assert!(
            matches!(live.next().await, OwnerEvent::Access { ready: true, .. }),
            "the paired worker attaches as the owner"
        );
        live
    }

    async fn next(&mut self) -> OwnerEvent {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(event) = self.connection.poll_owner().unwrap() {
                return event;
            }
            assert!(Instant::now() < deadline, "the owner worker never answered");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn answer(&mut self) {
        assert!(self.connection.busy());
        while self.connection.busy() {
            self.next().await;
        }
    }

    async fn settle(&mut self) -> Vec<Topic> {
        let deadline = Instant::now() + SETTLE_MAX;
        let mut topics = Vec::new();
        let mut last = Instant::now();
        loop {
            let delivered = self.connection.changed().unwrap();
            if !delivered.is_empty() {
                topics.extend(delivered);
                last = Instant::now();
            }
            if last.elapsed() >= QUIET {
                return topics;
            }
            assert!(
                Instant::now() < deadline,
                "the watch never went quiet after its start: {topics:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn delivered_within(&mut self, within: Duration) -> Vec<Topic> {
        let deadline = Instant::now() + within;
        let mut topics = Vec::new();
        while Instant::now() < deadline {
            topics.extend(self.connection.changed().unwrap());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        topics
    }
}

fn profile(name: &str) -> OwnerProfile {
    OwnerProfile {
        name: name.into(),
        role: String::new(),
        bio: String::new(),
        preferred_stack: Vec::new(),
        working_style: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_engine_delivers_nothing_and_an_owner_save_delivers_its_topic_at_once() {
    let mut live = Live::start().await;
    live.settle().await;
    assert_eq!(
        live.delivered_within(QUIET).await,
        Vec::<Topic>::new(),
        "while nothing commits the watch delivers nothing, so the TUI asks for no refresh"
    );

    live.connection
        .shell(ShellRequest::SaveProfile {
            profile: profile("Ada"),
            generation: 1,
        })
        .unwrap();
    live.answer().await;
    let topics = live.delivered_within(NAMED_WITHIN).await;
    assert!(
        topics.contains(&Topic::Settings),
        "the owner's profile save reaches the TUI as its named topic at once: {topics:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reads_a_refresh_and_home_ask_commit_nothing() {
    let mut live = Live::start().await;
    live.settle().await;

    live.connection
        .changes(ChangesQuery::Waiting { project: None })
        .unwrap();
    live.answer().await;
    live.connection
        .intake(IntakeJob::Query(IntakeQuery::JobStatus))
        .unwrap();
    live.answer().await;
    live.connection.questions(QuestionsQuery::Counts).unwrap();
    live.answer().await;
    live.connection
        .settings(SettingsJob::Extras { detail: false })
        .unwrap();
    live.answer().await;
    for request in [
        ShellRequest::Profile,
        ShellRequest::Projects,
        ShellRequest::Pools,
    ] {
        live.connection.shell(request).unwrap();
        live.answer().await;
    }
    for query in [IntakeQuery::Found, IntakeQuery::Home] {
        live.connection.intake(IntakeJob::Query(query)).unwrap();
        live.answer().await;
    }
    assert_eq!(
        live.delivered_within(QUIET).await,
        Vec::<Topic>::new(),
        "a read that committed would wake the watch and refresh again, with no end"
    );
}
