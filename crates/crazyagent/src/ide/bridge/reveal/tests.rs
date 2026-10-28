use super::*;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClient, OwnerClientKind};
use crate::ide::bridge::{IdeReply, MAGIC, issue, launch_env};
use crate::platform::unix_socket::UnixStream;
use crate::protocol::FRAME_BYTES_MAX;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BRIEF: &str = "context/brief.md";

/// A real engine service with its owner server, a TUI and a desktop owner client, and
/// projects `alpha` and `beta` whose folders hold a context brief.
struct Desk {
    _root: tempfile::TempDir,
    service: EngineService,
    _shutdown: tokio::sync::watch::Sender<bool>,
    tui: Arc<OwnerClient>,
    desktop: OwnerClient,
    folders: Vec<PathBuf>,
}

impl Desk {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-reveal-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, "version = 1\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (shutdown, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let engine_root = service.owner_runtime().unwrap().ide().unwrap().root;
        let db = rusqlite::Connection::open(engine_root.join("engine.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let mut folders = Vec::new();
        for project in ["alpha", "beta"] {
            let folder = root.path().join("projects").join(project);
            std::fs::create_dir_all(folder.join("context")).unwrap();
            std::fs::write(folder.join(BRIEF), "# Brief\n").unwrap();
            crate::ide::workspace::tests::add_project(&db, project, &folder);
            folders.push(folder);
        }
        let discover = |kind| OwnerClient::discover(root.path(), kind, Duration::from_secs(20));
        let tui = discover(OwnerClientKind::Tui).await.unwrap().0;
        let desktop = discover(OwnerClientKind::Desktop).await.unwrap().0;
        Self {
            _root: root,
            service,
            _shutdown: shutdown,
            tui: Arc::new(tui),
            desktop,
            folders,
        }
    }

    fn runtime(&self) -> &OwnerRuntimeHandle {
        self.service.owner_runtime().unwrap()
    }

    /// Issues a project's IDE capability the way an IDE server launch does and swaps its
    /// one-time file token for a session, as the built-in extension does before any other
    /// call. Input: the project. Output: the owner socket and the session's handshake packet.
    async fn capability(&self, project: &str) -> (PathBuf, [u8; 37]) {
        let path = issue(self.runtime(), &ProjectId::new(project).unwrap()).unwrap();
        let env = launch_env(self.runtime(), &path).unwrap();
        let socket = PathBuf::from(&env[1].1);
        let session = crate::ide::bridge::tests::session(&socket, &path).await;
        (
            socket,
            crate::ide::bridge::tests::packet(MAGIC, 0, &session),
        )
    }

    /// Asks the engine, as the TUI does, to show a target in a project's IDE.
    fn reveal(
        &self,
        project: &str,
        target: RevealTarget,
    ) -> tokio::task::JoinHandle<Result<RequestId, EngineError>> {
        let tui = self.tui.clone();
        let project = ProjectId::new(project).unwrap();
        tokio::spawn(async move { tui.ide_reveal(project, target).await })
    }

    async fn stop(mut self) {
        drop(self.desktop);
        drop(self.tui);
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

fn brief() -> RevealTarget {
    RevealTarget::Project { path: BRIEF.into() }
}

/// Sends one IDE request frame for a project and call after the handshake.
async fn send(
    socket: &Path,
    packet: [u8; 37],
    project: &str,
    call: serde_json::Value,
) -> UnixStream {
    let mut stream = crate::spawn_gate::connect_unix(socket).await.unwrap();
    stream.write_all(&packet).await.unwrap();
    let body = serde_json::to_vec(&json!({ "project": project, "call": call })).unwrap();
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend(body);
    stream.write_all(&frame).await.unwrap();
    stream
}

/// Reads one reply frame, or none once the engine closed the connection.
async fn read_reply(stream: &mut UnixStream) -> Option<IdeReply> {
    let length = stream.read_u32().await.ok()? as usize;
    assert!(length > 0 && length <= FRAME_BYTES_MAX);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.ok()?;
    Some(serde_json::from_slice(&body).unwrap())
}

/// Opens a project's event subscription, as the extension does, and waits for its `ok`.
async fn subscribe(socket: &Path, packet: [u8; 37], project: &str) -> UnixStream {
    let events = json!({ "name": "ide.events", "args": {} });
    let mut stream = send(socket, packet, project, events).await;
    assert_eq!(
        read_reply(&mut stream).await,
        Some(IdeReply::Ok {
            data: serde_json::Value::Null
        })
    );
    stream
}

/// Reads the next reveal event from a subscription. Output: its request and path.
async fn next_reveal(events: &mut UnixStream) -> (RequestId, String) {
    let pushed = tokio::time::timeout(Duration::from_secs(10), read_reply(events))
        .await
        .unwrap();
    match pushed {
        Some(IdeReply::Event {
            event: IdeEvent::Reveal { request, path },
        }) => (request, path),
        other => panic!("reveal event expected, got {other:?}"),
    }
}

/// Acknowledges a reveal from a project's capability, as the extension does once shown.
async fn ack(
    socket: &Path,
    packet: [u8; 37],
    project: &str,
    request: &RequestId,
) -> Option<IdeReply> {
    let call = json!({ "name": "ide.reveal_ack", "args": { "request": request } });
    let mut stream = send(socket, packet, project, call).await;
    read_reply(&mut stream).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tui_open_in_ide_sends_one_reveal_to_the_running_ide() {
    let desk = Desk::start().await;
    let (socket, alpha) = desk.capability("alpha").await;
    let (_, beta) = desk.capability("beta").await;
    let mut events = subscribe(&socket, alpha, "alpha").await;
    let mut other = subscribe(&socket, beta, "beta").await;

    let asked = desk.reveal("alpha", brief());
    let (request, path) = next_reveal(&mut events).await;
    assert_eq!(path, desk.folders[0].join(BRIEF).to_str().unwrap());

    assert_eq!(
        desk.desktop.ide_focus().await.unwrap(),
        Some(RevealFocus {
            project: ProjectId::new("alpha").unwrap(),
            request: request.clone(),
        })
    );
    assert_eq!(desk.desktop.ide_focus().await.unwrap(), None);
    assert_eq!(
        desk.tui.ide_focus().await.unwrap_err().0,
        "ide_focus_desktop_only"
    );

    assert_eq!(
        ack(&socket, beta, "beta", &request).await,
        Some(IdeReply::Rejected {
            code: "ide_reveal_unknown".into()
        }),
        "another project's IDE cannot answer this reveal"
    );
    assert!(!asked.is_finished());
    assert_eq!(
        ack(&socket, alpha, "alpha", &request).await,
        Some(IdeReply::Ok {
            data: serde_json::Value::Null
        })
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), asked)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        request
    );
    assert_eq!(
        ack(&socket, alpha, "alpha", &request).await,
        Some(IdeReply::Rejected {
            code: "ide_reveal_unknown".into()
        }),
        "a reveal is answered once"
    );
    for stream in [&mut events, &mut other] {
        assert!(
            tokio::time::timeout(Duration::from_millis(300), read_reply(stream))
                .await
                .is_err(),
            "one reveal reaches only its own project's IDE, once"
        );
    }
    desk.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_in_ide_without_a_running_desktop_reports_it() {
    let desk = Desk::start().await;
    let refused = |result: Result<Result<RequestId, EngineError>, tokio::task::JoinError>| {
        result.unwrap().unwrap_err().0
    };
    assert_eq!(
        refused(desk.reveal("alpha", brief()).await),
        "ide_not_running"
    );

    let (socket, alpha) = desk.capability("alpha").await;
    assert_eq!(
        refused(desk.reveal("alpha", brief()).await),
        "ide_not_running",
        "an IDE server whose extension is not listening is not a running IDE"
    );
    let events = subscribe(&socket, alpha, "alpha").await;
    drop(events);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        refused(desk.reveal("alpha", brief()).await),
        "ide_not_running",
        "an IDE that closed its subscription is no longer running"
    );
    assert_eq!(desk.desktop.ide_focus().await.unwrap(), None);
    desk.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reveal_names_only_paths_inside_the_workspace_folders() {
    let desk = Desk::start().await;
    let (socket, alpha) = desk.capability("alpha").await;
    let mut events = subscribe(&socket, alpha, "alpha").await;
    let outside = desk.folders[1].join("context");
    std::os::unix::fs::symlink(&outside, desk.folders[0].join("context").join("linked")).unwrap();
    let missing_set = ChangeSetId::new("0".repeat(32)).unwrap();
    for (target, code) in [
        (
            RevealTarget::Project {
                path: "../beta/context/brief.md".into(),
            },
            "ide_reveal_invalid",
        ),
        (
            RevealTarget::Project {
                path: "/etc/hosts".into(),
            },
            "ide_reveal_invalid",
        ),
        (
            RevealTarget::Project {
                path: "context/absent.md".into(),
            },
            "ide_reveal_missing",
        ),
        (
            RevealTarget::Project {
                path: "context/linked/brief.md".into(),
            },
            "ide_reveal_outside",
        ),
        (
            RevealTarget::ChangeSet {
                id: missing_set,
                path: None,
            },
            "changes_not_found",
        ),
    ] {
        assert_eq!(
            desk.reveal("alpha", target.clone())
                .await
                .unwrap()
                .unwrap_err()
                .0,
            code,
            "{target:?}"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(300), read_reply(&mut events))
            .await
            .is_err(),
        "a refused target sends no reveal"
    );
    assert_eq!(desk.desktop.ide_focus().await.unwrap(), None);
    desk.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reveal_the_ide_never_acknowledges_ends_unanswered() {
    let desk = Desk::start().await;
    let (socket, alpha) = desk.capability("alpha").await;
    let mut events = subscribe(&socket, alpha, "alpha").await;
    let started = std::time::Instant::now();
    let asked = desk.reveal("alpha", brief());
    let (request, _) = next_reveal(&mut events).await;
    assert_eq!(asked.await.unwrap().unwrap_err().0, "ide_reveal_unanswered");
    assert!(started.elapsed() >= REVEAL_WAIT);
    assert_eq!(
        ack(&socket, alpha, "alpha", &request).await,
        Some(IdeReply::Rejected {
            code: "ide_reveal_unknown".into()
        }),
        "a late acknowledgement finds nothing waiting"
    );
    desk.stop().await;
}

#[test]
fn a_change_set_reveal_opens_its_worktree_or_a_file_in_it() {
    let mut case = crate::ide::workspace::tests::Case::new();
    let agent = case.agent("sub", "working");
    case.change(&agent);
    let id: String = case
        .db
        .query_row("SELECT id FROM landing_change_sets", [], |row| row.get(0))
        .unwrap();
    let id = ChangeSetId::new(id).unwrap();
    let target = |path: Option<&str>| RevealTarget::ChangeSet {
        id: id.clone(),
        path: path.map(str::to_owned),
    };
    let worktree = agent.worktree.to_str().unwrap().to_owned();
    assert_eq!(
        resolve(&case.root, &case.project, &target(None)).unwrap(),
        worktree
    );
    assert_eq!(
        resolve(&case.root, &case.project, &target(Some("notes.txt"))).unwrap(),
        format!("{worktree}/notes.txt")
    );
    assert_eq!(
        resolve(&case.root, &case.project, &target(Some("../README")))
            .unwrap_err()
            .0,
        "ide_reveal_invalid"
    );

    case.db
        .execute("UPDATE landing_change_sets SET state='accepted'", [])
        .unwrap();
    case.status(&agent, "stopped");
    assert_eq!(
        resolve(&case.root, &case.project, &target(None))
            .unwrap_err()
            .0,
        "ide_reveal_not_listed",
        "a worktree the IDE workspace no longer lists is never revealed"
    );
}
