use super::*;
use crate::engine::bootstrap::EngineService;
use crate::owner_profile::OwnerProfile;
use crate::tui::seam::{ShellEvent, ShellRequest};
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

struct Live {
    root: tempfile::TempDir,
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
            root,
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

    fn sqlite(&self, sql: &str) {
        let output = crate::spawn_gate::output(
            std::process::Command::new("/usr/bin/sqlite3")
                .arg(self.root.path().join("engine.db"))
                .arg(sql)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
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

fn profile(name: &str) -> OwnerProfile {
    OwnerProfile {
        name: name.into(),
        role: String::new(),
        bio: String::new(),
        preferred_stack: Vec::new(),
        working_style: Default::default(),
    }
}

fn saved(events: &[OwnerEvent]) -> Result<(), String> {
    events
        .iter()
        .find_map(|event| match event {
            OwnerEvent::Shell(event) => match event.as_ref() {
                ShellEvent::ProfileSaved(result) => Some(result.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("a profile save answers")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_left_unconfirmed_is_settled_by_the_next_job_and_unblocks_changes() {
    let mut live = Live::start().await;
    let save = |name: &str| ShellRequest::SaveProfile {
        profile: profile(name),
        generation: 1,
    };
    live.sqlite(
        "CREATE TRIGGER ar01_abort BEFORE INSERT ON owner_operations \
         BEGIN SELECT RAISE(ABORT, 'ar01_abort'); END;",
    );
    live.connection.shell(save("Unconfirmed")).unwrap();
    assert_eq!(
        saved(&live.answer().await),
        Err("profile_outcome_unknown".into())
    );
    live.sqlite("DROP TRIGGER ar01_abort;");

    live.connection.shell(save("Ada")).unwrap();
    let mut events = live.answer().await;
    assert_eq!(
        saved(&events),
        Ok(()),
        "the unconfirmed change no longer blocks the next one"
    );
    let settled = events.remove(0);
    assert!(
        matches!(settled, OwnerEvent::Settled(Ok(()))),
        "the engine never recorded it, so it was sent once more and saved first"
    );
    let mut shell = crate::tui::shell::Shell::new(crate::tui::ctx::Ctx::new(
        crate::tui::theme::Depth::NoColor,
        crate::tui::theme::Colors::Dark,
        crate::tui::theme::Appearance::Dark,
        &crate::tui::glyphs::UNICODE,
        String::new(),
    ));
    shell.arrive(crate::tui::run::owner_arrival(settled));
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some("Your earlier change is saved.")
    );
    live.connection.shell(ShellRequest::Profile).unwrap();
    let read = live.answer().await;
    assert!(
        matches!(read.as_slice(), [OwnerEvent::Shell(event)]
            if matches!(event.as_ref(), ShellEvent::Profile(Ok(Some(profile))) if profile.name == "Ada")),
        "the engine keeps the newer change"
    );
}
