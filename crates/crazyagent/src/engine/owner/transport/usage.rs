use super::*;
use crate::protocol::{CredentialRef, ProjectId, encode_frame};
use crate::usage::metrics::Metrics;
use crate::usage::money::MoneySettings;
use crate::usage::report::UsageReport;
use crate::usage::{self, WindowKind};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024;
pub(super) const USAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// Every code a Usage request can answer an owner client with: the usage store's
/// refusals, this protocol's own, and the owner connection's it travels on.
pub const USAGE_CODES: &[&str] = &[
    "usage_limits_invalid",
    "usage_outcome_unknown",
    "usage_reply_bounds",
    "usage_request_bounds",
    "usage_request_invalid",
    "usage_unavailable",
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
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Report {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
        since_ms: Option<i64>,
        offset_minutes: i32,
    },
    Metrics {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
        since_ms: i64,
    },
    Refresh,
    SetReset {
        account: CredentialRef,
        window: WindowKind,
        at_ms: Option<i64>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    usage: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Report { report: Box<UsageReport> },
    Metrics { metrics: Box<Metrics> },
    Done,
    Rejected { code: String },
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::Report { .. } => Some("usage.report"),
        Action::Metrics { .. } => Some("usage.metrics"),
        _ => None,
    }
}

fn unknown() -> EngineError {
    failure("usage_outcome_unknown")
}

impl OwnerClient {
    /// Reads the usage page's numbers for one project, None for every project, over the
    /// range starting at `since_ms`, None for all time, with days cut at the viewer's local
    /// midnight. Inputs: the project, the range start and the viewer's offset east of UTC in
    /// minutes. Output: the report.
    pub async fn usage_report(
        &self,
        project: Option<ProjectId>,
        since_ms: Option<i64>,
        offset_minutes: i32,
    ) -> Result<UsageReport, EngineError> {
        let action = Action::Report {
            project,
            since_ms,
            offset_minutes,
        };
        match self.usage_exchange(action).await? {
            Reply::Report { report } => Ok(*report),
            _ => Err(unknown()),
        }
    }

    /// Reads the Metrics page's task counts, tokens chart and run, account and harness rows
    /// for one project, None for every project, over the range starting at `since_ms`.
    /// Inputs: the project and the range start. Output: the metrics.
    pub async fn usage_metrics(
        &self,
        project: Option<ProjectId>,
        since_ms: i64,
    ) -> Result<Metrics, EngineError> {
        match self
            .usage_exchange(Action::Metrics { project, since_ms })
            .await?
        {
            Reply::Metrics { metrics } => Ok(*metrics),
            _ => Err(unknown()),
        }
    }

    /// Asks the engine to poll every signed-in account's limits now.
    pub async fn usage_refresh(&self) -> Result<(), EngineError> {
        match self.usage_exchange(Action::Refresh).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Sets or clears the owner's manual reset time of one account's window.
    /// Inputs: the account, the window and the time, None to clear it.
    pub async fn usage_set_reset(
        &self,
        account: CredentialRef,
        window: WindowKind,
        at_ms: Option<i64>,
    ) -> Result<(), EngineError> {
        let action = Action::SetReset {
            account,
            window,
            at_ms,
        };
        match self.usage_exchange(action).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Stores the owner's main number and price source. Input: the settings. Output: done.
    pub async fn usage_set_money(&self, _settings: MoneySettings) -> Result<(), EngineError> {
        Ok(())
    }

    async fn usage_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(USAGE_TIMEOUT), async {
            let read = read_kind(&action);
            let frame = encode_frame(&Envelope { usage: action })
                .map_err(|_| failure("usage_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("usage_unavailable"))?;
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
                .map_err(|_| failure("usage_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("usage_unavailable"))?;
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

/// A usage request read and checked once: a report or metrics already built, or a change
/// still to make under the owner barrier, with the engine folder whose store it changes.
enum Prepared {
    Report(Box<UsageReport>),
    Metrics(Box<Metrics>),
    Change(PathBuf, Change),
}

/// A change a usage request makes to the usage store.
enum Change {
    Refresh,
    SetReset(CredentialRef, WindowKind, Option<i64>),
}

/// Serves one usage request. The report is built first; then, under the owner barrier, the
/// grant is checked again, any change is made and the reply is written, so a revocation
/// that returned saw every change and reply admitted before it and lets none through after.
/// Inputs: the connection, engine, authority, packet, admission and request body.
/// Output: done once the reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let prepared = prepare(engine, authority, packet, admission, bytes).await;
    let _barrier = engine.owner_egress.write().await;
    let reply = match finish(authority, packet, admission, prepared).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "usage_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    write_reply(stream, &frame).await
}

/// Reads and checks a usage request, and builds a report or metrics outside the owner
/// barrier.
/// Inputs: the engine, authority, packet, admission and request body.
/// Output: the prepared request, or the refusal.
async fn prepare(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Prepared, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("usage_request_bounds"));
    }
    let Envelope { usage: action } =
        serde_json::from_slice(bytes).map_err(|_| failure("usage_request_invalid"))?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    Ok(match action {
        Action::Report {
            project,
            since_ms,
            offset_minutes,
        } => {
            let held = held_accounts(engine, admission).await?;
            Prepared::Report(Box::new(
                tokio::task::spawn_blocking(move || {
                    let db = usage::open(&root)?;
                    usage::report::report(
                        &db,
                        project.as_ref(),
                        since_ms,
                        offset_minutes,
                        crate::team::now_ms(),
                        &held,
                    )
                })
                .await
                .map_err(|_| failure("usage_unavailable"))??,
            ))
        }
        Action::Metrics { project, since_ms } => Prepared::Metrics(Box::new(
            tokio::task::spawn_blocking(move || {
                let db = usage::open(&root)?;
                usage::metrics::read(&db, project.as_ref(), since_ms, crate::team::now_ms())
            })
            .await
            .map_err(|_| failure("usage_unavailable"))??,
        )),
        Action::Refresh => Prepared::Change(root, Change::Refresh),
        Action::SetReset {
            account,
            window,
            at_ms,
        } => Prepared::Change(root, Change::SetReset(account, window, at_ms)),
    })
}

/// The accounts the live config holds, which storage alone knows, so the report shows
/// limits only for accounts that exist now. Inputs: the engine and the owner admission.
/// Output: the account ids, or storage's refusal.
async fn held_accounts(
    engine: &Engine,
    admission: &Admission,
) -> Result<Vec<CredentialRef>, EngineError> {
    match engine
        .native_auth_job(|reply| {
            Job::Accounts(
                admission.clone(),
                crate::storage::accounts::Command::Held,
                reply,
            )
        })
        .await?
    {
        crate::storage::accounts::ResultValue::Held(held) => Ok(held),
        _ => Err(unknown()),
    }
}

/// Answers a prepared request under the owner barrier its caller holds: checks the grant
/// again, then makes any change. Inputs: the authority, packet, admission and the prepared
/// request. Output: the reply, or the refusal.
async fn finish(
    authority: &Authority,
    packet: &[u8; 37],
    admission: &Admission,
    prepared: Result<Prepared, EngineError>,
) -> Result<Reply, EngineError> {
    let prepared = prepared?;
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    let (root, change) = match prepared {
        Prepared::Report(report) => return Ok(Reply::Report { report }),
        Prepared::Metrics(metrics) => return Ok(Reply::Metrics { metrics }),
        Prepared::Change(root, change) => (root, change),
    };
    tokio::task::spawn_blocking(move || {
        let db = usage::open(&root)?;
        match change {
            Change::Refresh => usage::service::request_refresh(&db, crate::team::now_ms()),
            Change::SetReset(account, window, at_ms) => {
                usage::set_reset_override(&db, &account, window, at_ms)
            }
        }
    })
    .await
    .map_err(|_| failure("usage_unavailable"))??;
    Ok(Reply::Done)
}
