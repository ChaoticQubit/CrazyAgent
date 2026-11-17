use super::*;
use crate::projects_home::job::{JobKind, JobSubject, JobView};
use crate::projects_home::{AddMode, AddOverride};
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

/// The most bytes one iCloud or job request may hold: one owner frame, as for any owner
/// change, since a New Project job carries every answer the wizard gives (DEC-177 addendum 2).
const REQUEST_BYTES_MAX: usize = crate::protocol::FRAME_BYTES_MAX;
const ACTION_TIME: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)]
enum Action {
    Start {
        request_id: RequestId,
        subject: JobSubject,
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
    Job { job: Option<JobView> },
    Jobs { jobs: Vec<JobView> },
    Stopped { running: bool },
    Dismissed { forgotten: bool },
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("icloud_outcome_unknown")
}

impl OwnerClient {
    /// Starts the engine's one job apart from this client: an add of a folder or a move of a
    /// project into the projects home. It downloads what is only in iCloud, then moves or
    /// copies the folder. A start for the subject of the running job, or with its id,
    /// attaches to it. Inputs: the change's request id, which names the job, the subject, the
    /// mode and the overrides. Output: the job's view, or `job_busy` while another job runs.
    pub async fn job_start(
        &self,
        request_id: RequestId,
        subject: JobSubject,
        mode: AddMode,
        overrides: Vec<AddOverride>,
    ) -> Result<JobView, EngineError> {
        let action = Action::Start {
            request_id,
            subject,
            mode,
            overrides,
        };
        match self.icloud_exchange(action).await? {
            Reply::Job { job: Some(job) } => Ok(job),
            _ => Err(unknown()),
        }
    }

    /// Reads every job: running, waiting for its turn, or ended and kept for the clients.
    /// Output: their views in start order.
    pub async fn job_status(&self) -> Result<Vec<JobView>, EngineError> {
        match self.icloud_exchange(Action::Status).await? {
            Reply::Jobs { jobs } => Ok(jobs),
            _ => Err(unknown()),
        }
    }

    /// Asks the running job to stop between files. Input: its id. Output: whether it was
    /// running; its view then reads cancelled, or done when its folder had landed; or
    /// `delete_cannot_stop` for a delete already moving its folders to the Trash.
    pub async fn job_stop(&self, request_id: RequestId) -> Result<bool, EngineError> {
        match self.icloud_exchange(Action::Stop { request_id }).await? {
            Reply::Stopped { running } => Ok(running),
            _ => Err(unknown()),
        }
    }

    /// Forgets an ended job; for a delete cut short this keeps the project, as its banner's
    /// Dismiss does. Input: its id. Output: whether it was forgotten.
    pub async fn job_dismiss(&self, request_id: RequestId) -> Result<bool, EngineError> {
        match self.icloud_exchange(Action::Dismiss { request_id }).await? {
            Reply::Dismissed { forgotten } => Ok(forgotten),
            _ => Err(unknown()),
        }
    }

    async fn icloud_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(ACTION_TIME), async {
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

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

/// Serves one admitted intake job request and writes its reply, answered at once; the job
/// downloads, moves or copies apart from the request. Inputs: the stream, the engine, the
/// server authority, the handshake packet, its admission and the request body. Output: Ok
/// once the reply is written.
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
        Action::Start {
            request_id,
            subject,
            mode,
            overrides,
        } => {
            let asked = intake::Asked {
                request_id,
                subject,
                mode,
                overrides,
            };
            intake::Intake::start(engine, authority, packet, admission, asked)
                .await
                .map(|job| Reply::Job { job: Some(job) })
        }
        Action::Status => Ok(Reply::Jobs {
            jobs: authority.intake.status(),
        }),
        Action::Stop { request_id } => authority
            .intake
            .stop(&request_id)
            .map(|running| Reply::Stopped { running }),
        Action::Dismiss { request_id } => {
            let forgotten = authority.intake.dismiss(&request_id);
            if forgotten == Some(JobKind::Delete) {
                let id = request_id.clone();
                engine
                    .owner_job(|reply| Job::HomeDeleteKeep(id, reply))
                    .await?;
            }
            if forgotten.is_some() {
                engine
                    .owner_job(|reply| Job::HomeJobForget(request_id, reply))
                    .await?;
            }
            Ok(Reply::Dismissed {
                forgotten: forgotten.is_some(),
            })
        }
    }
}
