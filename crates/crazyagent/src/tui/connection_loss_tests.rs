use super::*;
use crate::engine::bootstrap::EngineService;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

struct Live {
    _root: tempfile::TempDir,
    service: EngineService,
    connection: Connection,
    _stop: tokio::sync::watch::Sender<bool>,
}

impl Live {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-loss-")
            .tempdir_in("/private/tmp")
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
            _root: root,
            service,
            connection: Connection::connect_paired(client).unwrap(),
            _stop: stop,
        };
        assert_eq!(kinds(&[live.next().await]), ["ready"]);
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

    async fn answer(&mut self) -> Vec<OwnerEvent> {
        assert!(self.connection.busy());
        let mut events = Vec::new();
        while self.connection.busy() {
            events.push(self.next().await);
        }
        events
    }

    async fn stop_engine(&mut self) {
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

fn kinds(events: &[OwnerEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            OwnerEvent::Access { ready: true, .. } => "ready",
            OwnerEvent::Access { ready: false, .. } => "lost",
            OwnerEvent::Read { .. } => "evidence",
            OwnerEvent::Setup(_) => "setup",
            OwnerEvent::TeamGraph { .. } => "team",
            _ => "other",
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_that_drops_the_connection_tells_the_shell_once() {
    let mut live = Live::start().await;
    let missing = ProjectId::new("missing").unwrap();
    live.connection.team_graph(missing.clone()).unwrap();
    assert_eq!(
        kinds(&live.answer().await),
        ["team"],
        "an answered read keeps the connection"
    );

    live.stop_engine().await;
    live.connection
        .read_evidence(missing.clone(), 1, true)
        .unwrap();
    assert_eq!(
        kinds(&live.answer().await),
        ["lost", "evidence"],
        "an evidence read that loses the engine tells the shell to reconnect"
    );
    live.connection.team_graph(missing).unwrap();
    assert_eq!(
        kinds(&live.answer().await),
        ["team"],
        "the shell is told once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_setup_read_on_a_stopped_engine_starts_a_reconnect() {
    let mut live = Live::start().await;
    let missing = ProjectId::new("missing").unwrap();
    live.stop_engine().await;
    live.connection
        .project_setup(SetupQuery::Setup {
            project: missing.clone(),
        })
        .unwrap();
    assert_eq!(kinds(&live.answer().await), ["lost", "setup"]);
    live.connection.team_graph(missing).unwrap();
    assert_eq!(kinds(&live.answer().await), ["team"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_team_read_on_a_stopped_engine_starts_a_reconnect() {
    let mut live = Live::start().await;
    live.stop_engine().await;
    live.connection
        .team_graph(ProjectId::new("missing").unwrap())
        .unwrap();
    assert_eq!(kinds(&live.answer().await), ["lost", "team"]);
}
