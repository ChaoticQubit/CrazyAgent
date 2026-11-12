use super::*;
use crate::protocol::{CredentialRef, ProjectId, encode_frame};
use crate::usage::extras::LimitExtras;
use crate::usage::metrics::Metrics;
use crate::usage::money::MoneySettings;
use crate::usage::report::UsageReport;
use crate::usage::spend::SpendSettings;
use crate::usage::{self, WindowKind};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024;
pub(super) const USAGE_TIMEOUT: Duration = Duration::from_secs(30);
/// The most accounts one extras reply holds, the same bound as the report's limits, so the
/// reply always fits one frame.
pub const EXTRAS_ACCOUNTS_MAX: usize = crate::usage::report::LIMITS_MAX;

/// Every code a Usage request can answer an owner client with: the usage store's
/// refusals, this protocol's own, and the owner connection's it travels on.
pub const USAGE_CODES: &[&str] = &[
    "usage_limits_invalid",
    "usage_outcome_unknown",
    "usage_recount_not_ready",
    "usage_reply_bounds",
    "usage_request_bounds",
    "usage_request_invalid",
    "usage_spend_off",
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
    Extras {
        #[serde(default)]
        detail: bool,
    },
    SetReset {
        account: CredentialRef,
        window: WindowKind,
        at_ms: Option<i64>,
    },
    SetMoney {
        settings: MoneySettings,
    },
    DecideRecount {
        use_new: bool,
    },
    Spend {
        account: CredentialRef,
    },
    SetSpend {
        settings: SpendSettings,
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
    Extras { accounts: Vec<AccountExtras> },
    Done,
    Rejected { code: String },
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::Report { .. } => Some("usage.report"),
        Action::Metrics { .. } => Some("usage.metrics"),
        Action::Extras { .. } => Some("usage.extras"),
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

    /// Reads every saved account's free resets, credits and spend as the usage store holds
    /// them (DEC-154). Pools passes `detail` on each reread so the usage service reads the
    /// detail rows while it is open; every other caller passes false and never makes the
    /// engine ask a provider. Input: whether Pools is open. Output: one entry per account.
    pub async fn usage_extras(&self, detail: bool) -> Result<Vec<AccountExtras>, EngineError> {
        match self.usage_exchange(Action::Extras { detail }).await? {
            Reply::Extras { accounts } => Ok(accounts),
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
    pub async fn usage_set_money(&self, settings: MoneySettings) -> Result<(), EngineError> {
        match self.usage_exchange(Action::SetMoney { settings }).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Applies the owner's choice of the recount, once. Input: whether to use the new
    /// numbers. Output: done, or `usage_recount_not_ready`.
    pub async fn usage_decide_recount(&self, use_new: bool) -> Result<(), EngineError> {
        match self
            .usage_exchange(Action::DecideRecount { use_new })
            .await?
        {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Asks the engine to spend one free reset on an account (DEC-154 slice 3). The usage
    /// thread sends it with a key the engine made and stored; its outcome appears in the
    /// report's spend view. Input: the account. Output: done once queued, or
    /// `usage_spend_off` when the owner turned spending off.
    pub async fn usage_spend(&self, account: CredentialRef) -> Result<(), EngineError> {
        match self.usage_exchange(Action::Spend { account }).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Stores the owner's spend settings: Spend confirmation and When every pool account is
    /// limited. Input: the settings. Output: done.
    pub async fn usage_set_spend(&self, settings: SpendSettings) -> Result<(), EngineError> {
        match self.usage_exchange(Action::SetSpend { settings }).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
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
    Extras(Vec<AccountExtras>, Option<PathBuf>),
    Change(PathBuf, Change),
}

/// One account's limits contract on the wire: the account id the app gave it and what its
/// adapter reports. Never a provider's reset, grant, account or org id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountExtras {
    pub account: CredentialRef,
    pub extras: LimitExtras,
}

/// A change a usage request makes to the usage store.
enum Change {
    Refresh,
    SetReset(CredentialRef, WindowKind, Option<i64>),
    SetMoney(MoneySettings),
    DecideRecount(bool),
    Spend(CredentialRef),
    SetSpend(SpendSettings),
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
/// barrier, each from one read transaction, so a usage scan that commits meanwhile never
/// leaves the total and the rows of one reply at two different moments.
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
                    let mut db = usage::open(&root)?;
                    let snapshot = db.transaction().map_err(|_| failure("usage_unavailable"))?;
                    usage::report::report(
                        &snapshot,
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
                let mut db = usage::open(&root)?;
                let snapshot = db.transaction().map_err(|_| failure("usage_unavailable"))?;
                usage::metrics::read(&snapshot, project.as_ref(), since_ms, crate::team::now_ms())
            })
            .await
            .map_err(|_| failure("usage_unavailable"))??,
        )),
        Action::Extras { detail } => {
            let connections = saved_connections(engine, admission).await?;
            let read = root.clone();
            let accounts = tokio::task::spawn_blocking(move || {
                let mut db = usage::open(&read)?;
                let snapshot = db.transaction().map_err(|_| failure("usage_unavailable"))?;
                let now = crate::team::now_ms();
                connections
                    .iter()
                    .map(|connection| {
                        let stored = usage::extras::extras_of(&snapshot, &connection.id, now)?;
                        Ok(AccountExtras {
                            account: connection.id.clone(),
                            extras: usage::extras::limit_extras(connection, &stored),
                        })
                    })
                    .collect::<Result<Vec<_>, EngineError>>()
            })
            .await
            .map_err(|_| failure("usage_unavailable"))??;
            assert!(accounts.len() <= EXTRAS_ACCOUNTS_MAX);
            Prepared::Extras(accounts, detail.then_some(root))
        }
        Action::Refresh => Prepared::Change(root, Change::Refresh),
        Action::SetReset {
            account,
            window,
            at_ms,
        } => Prepared::Change(root, Change::SetReset(account, window, at_ms)),
        Action::SetMoney { settings } => Prepared::Change(root, Change::SetMoney(settings)),
        Action::DecideRecount { use_new } => Prepared::Change(root, Change::DecideRecount(use_new)),
        Action::Spend { account } => Prepared::Change(root, Change::Spend(account)),
        Action::SetSpend { settings } => Prepared::Change(root, Change::SetSpend(settings)),
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

/// Every saved account's connection, in the order Settings > Accounts lists them, at most
/// `EXTRAS_ACCOUNTS_MAX`, so the extras read covers key, URL and local accounts as well as
/// sign-ins. Inputs: the engine and the owner admission. Output: the connections, or
/// storage's refusal.
async fn saved_connections(
    engine: &Engine,
    admission: &Admission,
) -> Result<Vec<crate::config::ModelConnection>, EngineError> {
    let mut found = Vec::new();
    let mut after = None;
    loop {
        let wanted = (EXTRAS_ACCOUNTS_MAX - found.len()).min(crate::accounts::ACCOUNTS_PAGE_MAX);
        let page = engine
            .native_auth_job(|reply| {
                Job::Accounts(
                    admission.clone(),
                    crate::storage::accounts::Command::List(after.clone(), wanted),
                    reply,
                )
            })
            .await?;
        let crate::storage::accounts::ResultValue::Page(items, next) = page else {
            return Err(unknown());
        };
        assert!(items.len() <= wanted);
        found.extend(items.into_iter().map(|item| item.connection));
        if next.is_none() || found.len() >= EXTRAS_ACCOUNTS_MAX {
            return Ok(found);
        }
        after = next;
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
        Prepared::Extras(accounts, None) => return Ok(Reply::Extras { accounts }),
        Prepared::Extras(accounts, Some(root)) => {
            tokio::task::spawn_blocking(move || {
                usage::extras::request_detail(&usage::open(&root)?, crate::team::now_ms())
            })
            .await
            .map_err(|_| failure("usage_unavailable"))??;
            return Ok(Reply::Extras { accounts });
        }
        Prepared::Change(root, change) => (root, change),
    };
    tokio::task::spawn_blocking(move || {
        let db = usage::open(&root)?;
        match change {
            Change::Refresh => usage::service::request_refresh(&db, crate::team::now_ms()),
            Change::SetReset(account, window, at_ms) => {
                usage::set_reset_override(&db, &account, window, at_ms)
            }
            Change::SetMoney(settings) => usage::money::write(&db, settings),
            Change::DecideRecount(use_new) => {
                usage::recount::decide(&db, use_new, crate::team::now_ms())
            }
            Change::Spend(account) => usage::spend::request(&db, &account, crate::team::now_ms()),
            Change::SetSpend(settings) => usage::spend::write(&db, settings),
        }
    })
    .await
    .map_err(|_| failure("usage_unavailable"))??;
    Ok(Reply::Done)
}

#[cfg(test)]
mod extras_request_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_extras_request_reads_detail_off_by_default_and_refuses_an_unknown_field() {
        let unknown = json!({ "usage": { "kind": "extras", "detail": false, "account": "x" } });
        assert!(serde_json::from_value::<Envelope>(unknown).is_err());
        let plain = json!({ "usage": { "kind": "extras" } });
        let Envelope { usage: action } = serde_json::from_value(plain).unwrap();
        assert!(matches!(action, Action::Extras { detail: false }));
        assert_eq!(read_kind(&action), Some("usage.extras"));
        let pools = json!({ "usage": { "kind": "extras", "detail": true } });
        let Envelope { usage: action } = serde_json::from_value(pools).unwrap();
        assert!(matches!(action, Action::Extras { detail: true }));
    }

    #[test]
    fn a_full_extras_reply_of_the_longest_entries_fits_one_frame() {
        use crate::usage::extras::{Adapter, CREDITS_BYTES_MAX, ResetKind, Resets, Spend};
        let harness = "h".repeat(64);
        let entry = |index: usize| AccountExtras {
            account: CredentialRef::new(format!("{index:03}{}", "a".repeat(125))).unwrap(),
            extras: LimitExtras {
                harness: harness.clone(),
                adapter: Adapter::OpenRouter,
                may_break: true,
                resets: Some(Resets {
                    count: crate::usage::extras::RESETS_MAX,
                    kinds: vec![
                        ResetKind::FiveHour,
                        ResetKind::Weekly,
                        ResetKind::Model,
                        ResetKind::AllLimits,
                    ],
                    next_expiry_ms: Some(i64::MAX),
                }),
                credits: Some("c".repeat(CREDITS_BYTES_MAX)),
                spend: Spend::Elsewhere {
                    place: "p".repeat(253),
                },
                not_reported: format!("not reported by {harness}"),
                resets_checked_ms: Some(i64::MAX),
                credits_checked_ms: Some(i64::MAX),
                resets_gained_ms: Some(i64::MAX),
            },
        };
        let accounts = (0..EXTRAS_ACCOUNTS_MAX).map(entry).collect::<Vec<_>>();
        let frame = encode_frame(&Reply::Extras { accounts }).unwrap();
        assert!(
            frame.len() <= crate::protocol::FRAME_BYTES_MAX,
            "{}",
            frame.len()
        );
    }
}
