use crate::engine::{Client, ClientError};
use crate::protocol::{
    ClientId, Command, Cursor, PROTOCOL_VERSION, RequestId, WireReply, WireRequest,
};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;

struct Pending {
    request: WireRequest,
    failed: bool,
}

pub(super) struct Connection {
    sender: Option<SyncSender<WireRequest>>,
    receiver: Option<Receiver<Result<WireReply, ClientError>>>,
    worker: Option<JoinHandle<()>>,
    pending: Option<Pending>,
    client_id: ClientId,
    sequence: u64,
}

impl Connection {
    pub fn connect(client: Client) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let (sender, requests) = sync_channel::<WireRequest>(1);
        let (replies, receiver) = sync_channel(1);
        let client_id = ClientId::new(format!(
            "tui-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        ))
        .map_err(|e| e.to_string())?;
        let worker = std::thread::Builder::new()
            .name("tui-client".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    if replies
                        .send(runtime.block_on(client.request(request)))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            sender: Some(sender),
            receiver: Some(receiver),
            worker: Some(worker),
            pending: None,
            client_id,
            sequence: 0,
        })
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn uncertain(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| pending.failed)
    }

    pub fn mutation_pending(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.request.command, Command::CreateProject { .. }))
    }

    pub fn start(&mut self, command: Command, revision: Cursor) -> Result<(), String> {
        if self.busy() {
            return Err("A request is pending; Ctrl-R retries an uncertain project creation with its original identity".into());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Client request counter exhausted")?;
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: self.client_id.clone(),
            request_id: RequestId::new(format!("request-{}", self.sequence))
                .map_err(|e| e.to_string())?,
            expected_revision: revision,
            command,
        };
        request.validate().map_err(|e| e.to_string())?;
        self.sender
            .as_ref()
            .ok_or("Client stopped")?
            .try_send(request.clone())
            .map_err(|e| e.to_string())?;
        self.pending = Some(Pending {
            request,
            failed: false,
        });
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Option<Result<WireReply, ClientError>>, String> {
        let result = match self.receiver.as_ref().ok_or("Client stopped")?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Ok(None),
            Err(TryRecvError::Disconnected) => return Err("Client worker stopped".into()),
        };
        let pending = self
            .pending
            .as_mut()
            .expect("client reply requires one pending request");
        assert!(!pending.failed);
        if result.is_err() && matches!(pending.request.command, Command::CreateProject { .. }) {
            pending.failed = true;
        } else {
            self.pending = None;
        }
        Ok(Some(result))
    }

    pub fn retry(&mut self) -> Result<(), String> {
        let pending = self
            .pending
            .as_mut()
            .ok_or("No uncertain project creation to retry")?;
        if !pending.failed {
            return Err("Request is still running".into());
        }
        self.sender
            .as_ref()
            .ok_or("Client stopped")?
            .try_send(pending.request.clone())
            .map_err(|e| e.to_string())?;
        pending.failed = false;
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.sender.take();
        self.receiver.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, Server};
    use crate::protocol::{Outcome, ProjectId, ProjectSpec};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    async fn response(connection: &mut Connection) -> Result<WireReply, ClientError> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = connection.poll().unwrap() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "client response exceeded three seconds"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn unavailable_endpoint_retry_preserves_exact_identity_and_commits_once() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.path().join("engine.sock");
        let mut connection =
            Connection::connect(Client::new(socket.clone(), Duration::from_secs(1)).unwrap())
                .unwrap();
        connection
            .start(
                Command::CreateProject {
                    project_id: ProjectId::new("retry-project").unwrap(),
                    spec: ProjectSpec {
                        name: "Real retry".into(),
                        repositories: Vec::new(),
                    },
                },
                Cursor::new(0),
            )
            .unwrap();
        let original = connection.pending.as_ref().unwrap().request.clone();
        assert!(response(&mut connection).await.is_err());
        assert!(connection.uncertain());
        assert!(connection.start(Command::Snapshot, Cursor::new(0)).is_err());
        assert_eq!(connection.pending.as_ref().unwrap().request, original);
        let config = crate::config::resolve_config(
            &root.path().join("global"),
            &root.path().join("project"),
        )
        .unwrap();
        let engine = Arc::new(
            Engine::open(&root.path().join("engine.db"), config.clone())
                .await
                .unwrap(),
        );
        let server = Server::bind(&socket, engine.clone(), &config).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let service = tokio::spawn(server.run(stopped));
        connection.retry().unwrap();
        assert_eq!(connection.pending.as_ref().unwrap().request, original);
        let applied = response(&mut connection).await.unwrap();
        assert_eq!(applied.outcome, Outcome::Applied);
        assert!(!connection.busy());
        let observer = Client::new(socket, Duration::from_secs(1)).unwrap();
        assert_eq!(observer.request(original.clone()).await.unwrap(), applied);
        let reply = observer
            .request(WireRequest {
                request_id: RequestId::new("observer").unwrap(),
                command: Command::Snapshot,
                ..original
            })
            .await
            .unwrap();
        let Outcome::Snapshot { projects, .. } = reply.outcome else {
            panic!("snapshot required")
        };
        assert_eq!(projects.len(), 1);
        assert_eq!(reply.revision.value(), 1);
        drop(connection);
        stop.send(()).unwrap();
        service.await.unwrap().unwrap();
        engine.close().await.unwrap();
    }
}
