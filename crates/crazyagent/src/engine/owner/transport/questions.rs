use super::*;
use crate::protocol::encode_frame;
use crate::questions::{self, Question, Refusal};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 16 * 1024;
pub(super) const QUESTIONS_TIMEOUT: Duration = Duration::from_secs(30);

const _: () = assert!(4 * questions::ANSWER_BYTES_MAX + 1024 <= REQUEST_BYTES_MAX);

/// Every code a Questions request can answer an owner client with: the questions log's
/// refusals, this protocol's own, and the owner connection's it travels on.
pub const QUESTIONS_CODES: &[&str] = &[
    "changes_unavailable",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_grant_stale_or_revoked",
    "owner_request_failed",
    "owner_root_replaced_or_unsafe",
    "owner_root_unavailable",
    "owner_socket_replaced",
    "owner_socket_unsafe",
    "peer_owner_mismatch",
    "question_delivery_full",
    "question_in_terminal",
    "question_invalid",
    "question_missing",
    "question_not_terminal",
    "question_project_unknown",
    "questions_outcome_unknown",
    "questions_page_unavailable",
    "questions_reply_bounds",
    "questions_request_bounds",
    "questions_request_invalid",
    "questions_unavailable",
    "versioned_item_invalid",
    "versioned_unavailable",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    List {
        project: ProjectId,
        pending_only: bool,
        before: Option<i64>,
    },
    Answer {
        project: ProjectId,
        id: i64,
        version: u64,
        answer: String,
    },
    Dismiss {
        project: ProjectId,
        id: i64,
        version: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    questions: Action,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Listed { questions: Vec<Question> },
    Answered,
    Dismissed,
    Stale { question: Box<Question> },
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("questions_outcome_unknown")
}

impl OwnerClient {
    /// Lists one project's owner questions newest first, at most one frame of them: only the
    /// pending ones, or all, older than `before` when given. Inputs: the project, the filter
    /// and the paging id. Output: the questions with the version each answer names.
    pub async fn questions_list(
        &self,
        project: &ProjectId,
        pending_only: bool,
        before: Option<i64>,
    ) -> Result<Vec<Question>, EngineError> {
        let action = Action::List {
            project: project.clone(),
            pending_only,
            before,
        };
        match self.questions_exchange(action).await? {
            Reply::Listed { questions } => Ok(questions),
            _ => Err(unknown()),
        }
    }

    /// Answers one owner question, naming the version the owner read; the engine delivers the
    /// answer to the asker once. Inputs: the project, the question id, the version and the
    /// answer. Output: Ok(Ok) once recorded, or Ok(Err) with the question as it now stands
    /// when it was settled or changed since it was read.
    pub async fn question_answer(
        &self,
        project: &ProjectId,
        id: i64,
        version: u64,
        answer: &str,
    ) -> Result<Result<(), Box<Question>>, EngineError> {
        let action = Action::Answer {
            project: project.clone(),
            id,
            version,
            answer: answer.to_owned(),
        };
        match self.questions_exchange(action).await? {
            Reply::Answered => Ok(Ok(())),
            Reply::Stale { question } => Ok(Err(question)),
            _ => Err(unknown()),
        }
    }

    /// Dismisses one owner question the owner will not answer, naming the version the owner
    /// read. Inputs: the project, the question id and the version. Output: as
    /// `question_answer`.
    pub async fn question_dismiss(
        &self,
        project: &ProjectId,
        id: i64,
        version: u64,
    ) -> Result<Result<(), Box<Question>>, EngineError> {
        let action = Action::Dismiss {
            project: project.clone(),
            id,
            version,
        };
        match self.questions_exchange(action).await? {
            Reply::Dismissed => Ok(Ok(())),
            Reply::Stale { question } => Ok(Err(question)),
            _ => Err(unknown()),
        }
    }

    async fn questions_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(QUESTIONS_TIMEOUT), async {
            let frame = encode_frame(&Envelope { questions: action })
                .map_err(|_| failure("questions_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("questions_unavailable"))?;
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
                .map_err(|_| failure("questions_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("questions_unavailable"))?;
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

pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let reply = match handle(engine, authority, packet, admission, bytes).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "questions_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    stream.write_all(&frame).await.map_err(io_error)
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Reply, EngineError> {
    let action = parse(bytes)?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    tokio::task::spawn_blocking(move || run(&root, action))
        .await
        .map_err(|_| failure("questions_unavailable"))?
}

/// Reads one Questions request from an owner frame body, at most REQUEST_BYTES_MAX bytes.
/// Input: the body. Output: the action, or `questions_request_bounds` or
/// `questions_request_invalid`.
fn parse(bytes: &[u8]) -> Result<Action, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("questions_request_bounds"));
    }
    let Envelope { questions } =
        serde_json::from_slice(bytes).map_err(|_| failure("questions_request_invalid"))?;
    Ok(questions)
}

/// Runs one authenticated Questions action against the engine database in the engine root.
/// Inputs: the engine root and the action. Output: the reply, or the log's refusal.
fn run(root: &Path, action: Action) -> Result<Reply, EngineError> {
    let db = questions::open(root)?;
    let now = crate::team::now_ms();
    let settled = |outcome: Result<(), Refusal>, done: Reply| match outcome {
        Ok(()) => Ok(done),
        Err(Refusal::Stale(question)) => Ok(Reply::Stale { question }),
        Err(Refusal::Code(code)) => Err(failure(code)),
    };
    match action {
        Action::List {
            project,
            pending_only,
            before,
        } => Ok(Reply::Listed {
            questions: questions::list(&db, &project, pending_only, before)?,
        }),
        Action::Answer {
            project,
            id,
            version,
            answer,
        } => settled(
            questions::answer(&db, &project, id, version, &answer, now)?,
            Reply::Answered,
        ),
        Action::Dismiss {
            project,
            id,
            version,
        } => settled(
            questions::dismiss(&db, &project, id, version, now)?,
            Reply::Dismissed,
        ),
    }
}

#[cfg(test)]
#[path = "questions_tests.rs"]
mod tests;
