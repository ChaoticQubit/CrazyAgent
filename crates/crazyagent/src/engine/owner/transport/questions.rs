use super::*;
use crate::protocol::encode_frame;
use crate::questions::{self, Answer, Page, Settlement};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 16 * 1024;
pub(super) const QUESTIONS_TIMEOUT: Duration = Duration::from_secs(30);

const _: () = assert!(2 * questions::ANSWER_BYTES_MAX + 1024 <= REQUEST_BYTES_MAX);
const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(
    questions::COUNTED_PROJECTS_MAX as usize * 160 + 64 <= crate::protocol::FRAME_BYTES_MAX
);

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
    "questions_answer_invalid",
    "questions_ask_invalid",
    "questions_asker_full",
    "questions_delivery_full",
    "questions_full",
    "questions_in_terminal",
    "questions_not_found",
    "questions_not_terminal",
    "questions_notes_invalid",
    "questions_option_invalid",
    "questions_outcome_unknown",
    "questions_owner_turn_invalid",
    "questions_page_unavailable",
    "questions_project_missing",
    "questions_reply_bounds",
    "questions_request_bounds",
    "questions_request_invalid",
    "questions_text_invalid",
    "questions_unavailable",
    "versioned_item_invalid",
    "versioned_unavailable",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    List {
        project: ProjectId,
        after: Option<u64>,
    },
    Answer {
        id: u64,
        version: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        picks: Vec<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment: Option<String>,
    },
    Dismiss {
        id: u64,
        version: u64,
    },
    Counts {},
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    questions: Action,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Page { page: Page },
    Settled { settlement: Settlement },
    Counts { counts: Vec<(ProjectId, u32)> },
    Rejected { code: String },
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::List { .. } => Some("questions.list"),
        Action::Counts { .. } => Some("questions.counts"),
        _ => None,
    }
}

fn unknown() -> EngineError {
    failure("questions_outcome_unknown")
}

impl OwnerClient {
    /// Reads one page of a project's owner questions, the pending ones first, with the
    /// version each answer or dismissal names. Inputs: the project and the `next` of the
    /// page before, none for the first. Output: the page.
    pub async fn questions_list(
        &self,
        project: ProjectId,
        after: Option<u64>,
    ) -> Result<Page, EngineError> {
        match self
            .questions_exchange(Action::List { project, after })
            .await?
        {
            Reply::Page { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Answers one owner question with picked options, a comment or both, naming the version
    /// the owner read; the engine tells the asker once. Inputs: the question id, the version
    /// and the answer. Output: Settled once recorded, or Stale when it was settled or changed
    /// since it was read.
    pub async fn questions_answer(
        &self,
        id: u64,
        version: u64,
        answer: Answer,
    ) -> Result<Settlement, EngineError> {
        let action = Action::Answer {
            id,
            version,
            picks: answer.picks,
            comment: answer.comment,
        };
        match self.questions_exchange(action).await? {
            Reply::Settled { settlement } => Ok(settlement),
            _ => Err(unknown()),
        }
    }

    /// Dismisses one owner question the owner will not answer, naming the version the owner
    /// read. Inputs: the question id and the version. Output: as `questions_answer`.
    pub async fn questions_dismiss(
        &self,
        id: u64,
        version: u64,
    ) -> Result<Settlement, EngineError> {
        match self
            .questions_exchange(Action::Dismiss { id, version })
            .await?
        {
            Reply::Settled { settlement } => Ok(settlement),
            _ => Err(unknown()),
        }
    }

    /// Counts the questions waiting for the owner in each project that has any. Output: the
    /// projects and their counts.
    pub async fn questions_counts(&self) -> Result<Vec<(ProjectId, u32)>, EngineError> {
        match self.questions_exchange(Action::Counts {}).await? {
            Reply::Counts { counts } => Ok(counts),
            _ => Err(unknown()),
        }
    }

    async fn questions_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(QUESTIONS_TIMEOUT), async {
            let read = read_kind(&action);
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
            match serde_json::from_slice(&frame[4..])
                .map_err(|error| self.skewed_reply(&error, read, unknown))?
            {
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
/// An answer names option positions, a comment or both, never neither; the log checks them
/// against the question. Inputs: the engine root and the action. Output: the reply, or the
/// log's refusal.
fn run(root: &Path, action: Action) -> Result<Reply, EngineError> {
    let db = questions::open(root)?;
    let settlement = |settlement| Ok(Reply::Settled { settlement });
    match action {
        Action::List { project, after } => Ok(Reply::Page {
            page: questions::page(&db, &project, after)?,
        }),
        Action::Answer {
            id,
            version,
            picks,
            comment,
        } => {
            if picks.is_empty() && comment.is_none() {
                return Err(failure("questions_answer_invalid"));
            }
            let answer = Answer { picks, comment };
            settlement(questions::answer(&db, id, version, &answer)?)
        }
        Action::Dismiss { id, version } => settlement(questions::dismiss(&db, id, version)?),
        Action::Counts {} => Ok(Reply::Counts {
            counts: questions::count_pending(&db)?,
        }),
    }
}

#[cfg(test)]
#[path = "questions_tests.rs"]
mod tests;
