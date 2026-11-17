use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::owner_profile::OwnerProfile;
use crate::protocol::Topic;
use crate::tui::client::app_setup::AppSetupJob;
use crate::tui::client::changes::ChangesQuery;
use crate::tui::client::intake::{IntakeJob, IntakeQuery};
use crate::tui::client::learning::LearningQuery;
use crate::tui::client::questions::QuestionsQuery;
use crate::tui::client::settings::SettingsJob;
use crate::tui::client::{Connection, OwnerEvent};
use crate::tui::seam::ShellRequest;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

const QUIET: Duration = Duration::from_secs(6);
const SETTLE_MAX: Duration = Duration::from_secs(30);
const NAMED_WITHIN: Duration = Duration::from_secs(2);
const OWNER_WAIT: Duration = Duration::from_secs(20);

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
            matches!(live.next(), OwnerEvent::Access { ready: true, .. }),
            "the paired worker attaches as the owner"
        );
        live
    }

    fn next(&mut self) -> OwnerEvent {
        let receiver = self
            .connection
            .owner_receiver
            .as_ref()
            .expect("the owner worker runs");
        let reply = tokio::task::block_in_place(|| receiver.recv_timeout(OWNER_WAIT))
            .expect("the owner worker answers");
        if reply.finished {
            assert!(self.connection.owner_busy);
            self.connection.owner_busy = false;
        }
        reply.event
    }

    fn answer(&mut self) {
        assert!(self.connection.busy());
        while self.connection.busy() {
            self.next();
        }
    }

    fn delivery(&mut self, within: Duration) -> Option<Vec<Topic>> {
        let first =
            tokio::task::block_in_place(|| self.connection.watch.deliveries.recv_timeout(within));
        match first {
            Ok(mut topics) => {
                topics.extend(self.connection.changed().unwrap());
                Some(topics)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("the watch thread stopped"),
        }
    }

    fn settle(&mut self) -> Vec<Topic> {
        let deadline = Instant::now() + SETTLE_MAX;
        let mut topics = Vec::new();
        while let Some(delivered) = self.delivery(QUIET) {
            topics.extend(delivered);
            assert!(
                Instant::now() < deadline,
                "the watch never went quiet after its start: {topics:?}"
            );
        }
        topics
    }

    fn delivered_within(&mut self, within: Duration) -> Vec<Topic> {
        let deadline = Instant::now() + within;
        let mut topics = Vec::new();
        while let Some(delivered) =
            self.delivery(deadline.saturating_duration_since(Instant::now()))
        {
            topics.extend(delivered);
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
    live.settle();
    assert_eq!(
        live.delivered_within(QUIET),
        Vec::<Topic>::new(),
        "while nothing commits the watch delivers nothing, so the TUI asks for no refresh"
    );

    live.connection
        .shell(ShellRequest::SaveProfile {
            profile: profile("Ada"),
            generation: 1,
        })
        .unwrap();
    live.answer();
    let topics = live.delivered_within(NAMED_WITHIN);
    assert!(
        topics.contains(&Topic::Settings),
        "the owner's profile save reaches the TUI as its named topic at once: {topics:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reads_a_refresh_and_home_ask_commit_nothing() {
    let mut live = Live::start().await;
    live.settle();

    live.connection
        .changes(ChangesQuery::Waiting { project: None })
        .unwrap();
    live.answer();
    live.connection
        .intake(IntakeJob::Query(IntakeQuery::JobStatus))
        .unwrap();
    live.answer();
    live.connection.questions(QuestionsQuery::Counts).unwrap();
    live.answer();
    live.connection.learning(LearningQuery::Progress).unwrap();
    live.answer();
    for job in [
        SettingsJob::Extras { detail: false },
        SettingsJob::AllAccounts,
        SettingsJob::Harnesses,
        SettingsJob::AppSetup(AppSetupJob::Status),
    ] {
        live.connection.settings(job).unwrap();
        live.answer();
    }
    for request in [
        ShellRequest::Profile,
        ShellRequest::Projects,
        ShellRequest::Pools,
    ] {
        live.connection.shell(request).unwrap();
        live.answer();
    }
    for query in [IntakeQuery::Found, IntakeQuery::Home] {
        live.connection.intake(IntakeJob::Query(query)).unwrap();
        live.answer();
    }
    assert_eq!(
        live.delivered_within(QUIET),
        Vec::<Topic>::new(),
        "a read that committed would wake the watch and refresh again, with no end"
    );
}
