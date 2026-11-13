use super::*;
use crate::projects_home::icloud::{self, DOWNLOAD_TIME_MAX, Progress};
use crate::projects_home::job::JobView;
use crate::projects_home::{AddMode, AddOverride};
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 8192;
const ACTION_TIME: Duration = Duration::from_secs(10);
pub(super) const ICLOUD_TIMEOUT: Duration = Duration::from_secs(DOWNLOAD_TIME_MAX.as_secs() + 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Download {
        path: String,
    },
    Progress,
    Cancel,
    Start {
        request_id: RequestId,
        path: String,
        mode: AddMode,
        #[serde(default)]
        overrides: Vec<AddOverride>,
    },
    Status,
    Stop {
        request_id: RequestId,
    },
    Dismiss {
        request_id: RequestId,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    icloud: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Downloaded { files: u64 },
    Progress { progress: Option<Progress> },
    Cancelled { running: bool },
    Job { job: Option<JobView> },
    Stopped { running: bool },
    Dismissed { forgotten: bool },
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("icloud_outcome_unknown")
}

impl OwnerClient {
    /// Downloads every file of a folder that is only in iCloud and waits until all are on
    /// this Mac; the folder is never moved or changed. Input: the absolute folder.
    /// Output: how many placeholders were downloaded, or the refusal code, such as
    /// `home_icloud_not_downloaded` after a cancel, a stall or the time limit.
    pub async fn icloud_download(&self, path: String) -> Result<u64, EngineError> {
        match self.icloud_exchange(Action::Download { path }).await? {
            Reply::Downloaded { files } => Ok(files),
            _ => Err(unknown()),
        }
    }

    /// Reads how far the running iCloud download has come. Output: the counts, or None
    /// when no download runs.
    pub async fn icloud_progress(&self) -> Result<Option<Progress>, EngineError> {
        match self.icloud_exchange(Action::Progress).await? {
            Reply::Progress { progress } => Ok(progress),
            _ => Err(unknown()),
        }
    }

    /// Stops the running iCloud download at its next check. Output: whether one was
    /// running; its own request then answers `home_icloud_not_downloaded`.
    pub async fn icloud_cancel(&self) -> Result<bool, EngineError> {
        match self.icloud_exchange(Action::Cancel).await? {
            Reply::Cancelled { running } => Ok(running),
            _ => Err(unknown()),
        }
    }

    /// Starts an intake job that adds a folder apart from this client: it downloads what
    /// is only in iCloud, then moves or copies the folder into the projects home. A start
    /// for the folder of the running job, or with its id, attaches to it. Inputs: the add's
    /// request id, which names the job, the folder, the mode and the overrides. Output: the
    /// job's view, or `intake_busy` while a job for another folder runs.
    pub async fn intake_start(
        &self,
        request_id: RequestId,
        path: String,
        mode: AddMode,
        overrides: Vec<AddOverride>,
    ) -> Result<JobView, EngineError> {
        let action = Action::Start {
            request_id,
            path,
            mode,
            overrides,
        };
        match self.icloud_exchange(action).await? {
            Reply::Job { job: Some(job) } => Ok(job),
            _ => Err(unknown()),
        }
    }

    /// Reads the intake job, running or last ended. Output: its view, or None.
    pub async fn intake_status(&self) -> Result<Option<JobView>, EngineError> {
        match self.icloud_exchange(Action::Status).await? {
            Reply::Job { job } => Ok(job),
            _ => Err(unknown()),
        }
    }

    /// Asks the running intake job to stop between files. Input: its id. Output: whether it
    /// was running; its view then reads cancelled, or done when its folder had landed.
    pub async fn intake_stop(&self, request_id: RequestId) -> Result<bool, EngineError> {
        match self.icloud_exchange(Action::Stop { request_id }).await? {
            Reply::Stopped { running } => Ok(running),
            _ => Err(unknown()),
        }
    }

    /// Forgets an ended intake job. Input: its id. Output: whether it was forgotten.
    pub async fn intake_dismiss(&self, request_id: RequestId) -> Result<bool, EngineError> {
        match self.icloud_exchange(Action::Dismiss { request_id }).await? {
            Reply::Dismissed { forgotten } => Ok(forgotten),
            _ => Err(unknown()),
        }
    }

    async fn icloud_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        let time = match action {
            Action::Download { .. } => ICLOUD_TIMEOUT,
            _ => ACTION_TIME,
        };
        tokio::time::timeout(self.timeout.max(time), async {
            let frame = encode_frame(&Envelope { icloud: action })
                .map_err(|_| failure("icloud_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("icloud_owner_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("icloud_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("icloud_owner_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..]).map_err(|_| unknown())? {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Whether a request body is a download, which waits for iCloud and so is served past the
/// owner exchange; progress, cancel and the intake job's actions are answered at once.
/// Input: the body. Output: the answer, false for a body that does not parse.
pub(super) fn deferred(bytes: &[u8]) -> bool {
    matches!(
        serde_json::from_slice::<Envelope>(bytes),
        Ok(Envelope {
            icloud: Action::Download { .. }
        })
    )
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

/// Serves one admitted iCloud request and writes its reply. A download holds the engine's
/// one download slot while it runs; when this request ends for any reason, its timeout, the
/// client hanging up or the engine stopping included, the slot is freed and the download
/// cancelled. Inputs: the stream, the engine, the server authority, the handshake packet,
/// its admission and the request body. Output: Ok once the reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let mut extra = [0; 1];
    let outcome = tokio::select! {
        outcome = handle(engine, authority, packet, admission, bytes) => outcome,
        _ = stream.read(&mut extra) => return Err(failure("icloud_client_gone")),
    };
    let reply = match outcome {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "icloud_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    tokio::time::timeout(engine.timeout, stream.write_all(&frame))
        .await
        .map_err(|_| unknown())?
        .map_err(io_error)
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Reply, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("icloud_request_bounds"));
    }
    let Envelope { icloud: action } =
        serde_json::from_slice(bytes).map_err(|_| failure("icloud_request_invalid"))?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    match action {
        Action::Download { path } => {
            let claim = authority.icloud.claim()?;
            let download = claim.download();
            let files = tokio::task::spawn_blocking(move || icloud::download(&path, &download))
                .await
                .map_err(|_| failure("home_icloud_not_downloaded"))??;
            drop(claim);
            Ok(Reply::Downloaded { files })
        }
        Action::Progress => Ok(Reply::Progress {
            progress: authority.icloud.progress(),
        }),
        Action::Cancel => Ok(Reply::Cancelled {
            running: authority.icloud.cancel(),
        }),
        Action::Start {
            request_id,
            path,
            mode,
            overrides,
        } => {
            let asked = intake::Asked {
                request_id,
                path,
                mode,
                overrides,
            };
            intake::Intake::start(engine, authority, packet, admission, asked)
                .map(|job| Reply::Job { job: Some(job) })
        }
        Action::Status => Ok(Reply::Job {
            job: authority.intake.status(),
        }),
        Action::Stop { request_id } => Ok(Reply::Stopped {
            running: authority.intake.stop(&request_id),
        }),
        Action::Dismiss { request_id } => Ok(Reply::Dismissed {
            forgotten: authority.intake.dismiss(&request_id),
        }),
    }
}
