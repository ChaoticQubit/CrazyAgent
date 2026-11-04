use super::*;
use crate::protocol::encode_frame;
use crate::scorecard::{self, OFFSET_MINUTES_MAX, Scorecard, Settings};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 4 * 1024;
pub(super) const SCORECARD_TIMEOUT: Duration = Duration::from_secs(30);

const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);

/// Every code a Scorecard request can answer an owner client with: the scorecard's
/// refusals, this protocol's own, and the owner connection's it travels on.
pub const SCORECARD_CODES: &[&str] = &[
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
    "questions_unavailable",
    "scorecard_outcome_unknown",
    "scorecard_reply_bounds",
    "scorecard_request_bounds",
    "scorecard_request_invalid",
    "scorecard_settings_invalid",
    "scorecard_unavailable",
    "versioned_item_invalid",
    "versioned_unavailable",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Report { offset_minutes: i32 },
    Set { settings: Settings, version: u64 },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    scorecard: Action,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Report { scorecard: Box<Scorecard> },
    Saved { version: u64 },
    Stale { current: u64 },
    Rejected { code: String },
}

/// How a settings save ended: saved at a new version, or refused because the settings
/// changed since the owner's screen read them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saved {
    Saved(u64),
    Stale(u64),
}

fn unknown() -> EngineError {
    failure("scorecard_outcome_unknown")
}

impl OwnerClient {
    /// Reads the go/no-go scorecard in the reader's local days. Input: the reader's UTC
    /// offset in minutes. Output: the scorecard.
    pub async fn scorecard_report(&self, offset_minutes: i32) -> Result<Scorecard, EngineError> {
        match self
            .scorecard_exchange(Action::Report { offset_minutes })
            .await?
        {
            Reply::Report { scorecard } => Ok(*scorecard),
            _ => Err(unknown()),
        }
    }

    /// Saves the scorecard settings, naming the version the owner read. Inputs: the
    /// settings and the version. Output: the new version, or the current one when stale.
    pub async fn scorecard_set(
        &self,
        settings: Settings,
        version: u64,
    ) -> Result<Saved, EngineError> {
        match self
            .scorecard_exchange(Action::Set { settings, version })
            .await?
        {
            Reply::Saved { version } => Ok(Saved::Saved(version)),
            Reply::Stale { current } => Ok(Saved::Stale(current)),
            _ => Err(unknown()),
        }
    }

    async fn scorecard_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(SCORECARD_TIMEOUT), async {
            let frame = encode_frame(&Envelope { scorecard: action })
                .map_err(|_| failure("scorecard_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("scorecard_unavailable"))?;
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
                .map_err(|_| failure("scorecard_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("scorecard_unavailable"))?;
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
            code: "scorecard_reply_bounds".into(),
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
        .map_err(|_| failure("scorecard_unavailable"))?
}

/// Reads one Scorecard request from an owner frame body, at most REQUEST_BYTES_MAX bytes,
/// with a UTC offset at most `OFFSET_MINUTES_MAX` minutes either way. Input: the body.
/// Output: the action, or `scorecard_request_bounds` or `scorecard_request_invalid`.
fn parse(bytes: &[u8]) -> Result<Action, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("scorecard_request_bounds"));
    }
    let Envelope { scorecard } =
        serde_json::from_slice(bytes).map_err(|_| failure("scorecard_request_invalid"))?;
    if let Action::Report { offset_minutes } = scorecard
        && !(-OFFSET_MINUTES_MAX..=OFFSET_MINUTES_MAX).contains(&offset_minutes)
    {
        return Err(failure("scorecard_request_invalid"));
    }
    Ok(scorecard)
}

/// Runs one authenticated Scorecard action against the engine database in the engine root.
/// Inputs: the engine root and the action. Output: the reply, or the scorecard's refusal.
fn run(root: &Path, action: Action) -> Result<Reply, EngineError> {
    let db = crate::questions::open(root)?;
    match action {
        Action::Report { offset_minutes } => {
            assert!((-OFFSET_MINUTES_MAX..=OFFSET_MINUTES_MAX).contains(&offset_minutes));
            let scorecard = scorecard::report(&db, scorecard::now_ms(), offset_minutes)?;
            Ok(Reply::Report {
                scorecard: Box::new(scorecard),
            })
        }
        Action::Set { settings, version } => Ok(match scorecard::set(&db, &settings, version)? {
            Ok(version) => Reply::Saved { version },
            Err(stale) => Reply::Stale {
                current: stale.current,
            },
        }),
    }
}

#[cfg(test)]
#[path = "scorecard_tests.rs"]
mod tests;
