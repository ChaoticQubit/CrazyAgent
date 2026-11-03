use super::*;
use crate::harness_settings::{
    self, Catalog, CatalogPage, Page, STRING_BYTES_MAX, Scope, Setting, Written, catalog,
    installed, store,
};
use crate::protocol::encode_frame;
use crate::storage::versioned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const REQUEST_BYTES_MAX: usize = 8 * 1024;
pub(super) const HARNESS_SETTINGS_TIMEOUT: Duration = Duration::from_secs(30);

const _: () = assert!(6 * STRING_BYTES_MAX + 1024 <= REQUEST_BYTES_MAX);
const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);

/// Every code a harness settings request can answer an owner client with: the store's
/// refusals, this protocol's own, and the owner connection's it travels on.
pub const HARNESS_SETTINGS_CODES: &[&str] = &[
    "changes_unavailable",
    "harness_settings_catalog_invalid",
    "harness_settings_key_app_only",
    "harness_settings_key_invalid",
    "harness_settings_key_locked",
    "harness_settings_key_not_qualified",
    "harness_settings_no_catalog",
    "harness_settings_outcome_unknown",
    "harness_settings_page_invalid",
    "harness_settings_project_unknown",
    "harness_settings_reply_bounds",
    "harness_settings_request_bounds",
    "harness_settings_request_invalid",
    "harness_settings_unavailable",
    "harness_settings_value_invalid",
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
    "versioned_item_invalid",
    "versioned_unavailable",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Catalog {
        harness: String,
        after: Option<u32>,
    },
    Values {
        harness: String,
        project: Option<ProjectId>,
        after: Option<u32>,
    },
    Set {
        harness: String,
        project: Option<ProjectId>,
        key: String,
        value: Value,
        version: u64,
    },
    Reset {
        harness: String,
        project: Option<ProjectId>,
        key: String,
        version: u64,
    },
}

impl Action {
    fn harness(&self) -> &str {
        match self {
            Self::Catalog { harness, .. }
            | Self::Values { harness, .. }
            | Self::Set { harness, .. }
            | Self::Reset { harness, .. } => harness,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    harness_settings: Action,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Catalog { page: CatalogPage },
    Values { page: Page },
    Saved { setting: Setting },
    Stale { setting: Setting },
    Refused { code: String, setting: Setting },
    Rejected { code: String },
}

impl From<Written> for Reply {
    fn from(written: Written) -> Self {
        match written {
            Written::Saved { setting } => Self::Saved { setting },
            Written::Stale { setting } => Self::Stale { setting },
            Written::Refused { code, setting } => Self::Refused { code, setting },
        }
    }
}

fn unknown() -> EngineError {
    failure("harness_settings_outcome_unknown")
}

fn scope(project: Option<ProjectId>) -> Scope {
    project.map_or(Scope::App, Scope::Project)
}

impl OwnerClient {
    /// Reads one page of a harness's settings catalog with the installed version and
    /// whether it is qualified. Inputs: the harness id and the `next` of the page before,
    /// none for the first. Output: the page, or a `HARNESS_SETTINGS_CODES` refusal.
    pub async fn harness_settings_catalog(
        &self,
        harness: &str,
        after: Option<u32>,
    ) -> Result<CatalogPage, EngineError> {
        let action = Action::Catalog {
            harness: harness.into(),
            after,
        };
        match self.harness_settings_exchange(action).await? {
            Reply::Catalog { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Reads one page of a harness's settings app-wide or for one project, each row with
    /// the version a set or reset names. Inputs: the harness, the project, none for the
    /// app, and the `next` of the page before. Output: the page, or a refusal.
    pub async fn harness_settings_values(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        after: Option<u32>,
    ) -> Result<Page, EngineError> {
        let action = Action::Values {
            harness: harness.into(),
            project,
            after,
        };
        match self.harness_settings_exchange(action).await? {
            Reply::Values { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Sets one harness setting app-wide or as a project override, naming the version the
    /// owner read; the harness uses it from its next start. Inputs: the harness, the
    /// project, the key, the value and the version. Output: Saved, Stale or Refused with
    /// the key's row, or a refusal.
    pub async fn harness_settings_set(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        key: &str,
        value: Value,
        version: u64,
    ) -> Result<Written, EngineError> {
        let action = Action::Set {
            harness: harness.into(),
            project,
            key: key.into(),
            value,
            version,
        };
        self.harness_settings_written(action).await
    }

    /// Resets one harness setting at one scope, naming the version the owner read.
    /// Inputs: as `harness_settings_set`, without a value. Output: as `harness_settings_set`.
    pub async fn harness_settings_reset(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        key: &str,
        version: u64,
    ) -> Result<Written, EngineError> {
        let action = Action::Reset {
            harness: harness.into(),
            project,
            key: key.into(),
            version,
        };
        self.harness_settings_written(action).await
    }

    async fn harness_settings_written(&self, action: Action) -> Result<Written, EngineError> {
        match self.harness_settings_exchange(action).await? {
            Reply::Saved { setting } => Ok(Written::Saved { setting }),
            Reply::Stale { setting } => Ok(Written::Stale { setting }),
            Reply::Refused { code, setting } => Ok(Written::Refused { code, setting }),
            _ => Err(unknown()),
        }
    }

    async fn harness_settings_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(HARNESS_SETTINGS_TIMEOUT), async {
            let frame = encode_frame(&Envelope {
                harness_settings: action,
            })
            .map_err(|_| failure("harness_settings_request_bounds"))?;
            if frame.len() > REQUEST_BYTES_MAX + 4 {
                return Err(failure("harness_settings_request_bounds"));
            }
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("harness_settings_unavailable"))?;
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
                .map_err(|_| failure("harness_settings_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("harness_settings_unavailable"))?;
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
            code: "harness_settings_reply_bounds".into(),
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
    let catalog = catalog::builtin(action.harness())?
        .ok_or_else(|| failure("harness_settings_no_catalog"))?;
    let executable = registered(engine, authority, &catalog.harness);
    let root = super::changes::engine_root(authority)?;
    tokio::task::spawn_blocking(move || {
        let version =
            executable.and_then(|path| installed::version_cached(&path, installed::LIMIT));
        run(&root, &catalog, version.as_deref(), action)
    })
    .await
    .map_err(|_| failure("harness_settings_unavailable"))?
}

/// Finds the engine-verified executable whose version decides qualification: the
/// registered Claude Code or Codex executable. Inputs: the engine, its authority and the
/// harness. Output: the path, or none for another harness or when none is registered.
fn registered(engine: &Arc<Engine>, authority: &Arc<Authority>, harness: &str) -> Option<PathBuf> {
    let runtime = OwnerRuntimeHandle {
        engine: engine.clone(),
        authority: authority.clone(),
    };
    let executable = match harness {
        "claude-code" => runtime.registered_claude_executable(),
        "codex" => runtime.registered_codex_executable(),
        _ => return None,
    }
    .ok()?;
    executable.verified_path().ok().map(Path::to_path_buf)
}

/// Reads one harness settings request from an owner frame body, at most
/// REQUEST_BYTES_MAX bytes. Input: the body. Output: the action, or
/// `harness_settings_request_bounds` or `harness_settings_request_invalid`.
fn parse(bytes: &[u8]) -> Result<Action, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("harness_settings_request_bounds"));
    }
    let Envelope { harness_settings } =
        serde_json::from_slice(bytes).map_err(|_| failure("harness_settings_request_invalid"))?;
    Ok(harness_settings)
}

/// Runs one authenticated harness settings action against the engine database in the
/// engine root. Inputs: the engine root, the harness's catalog, its installed version,
/// none when unknown, and the action. Output: the reply, or the store's refusal.
fn run(
    root: &Path,
    catalog: &Catalog,
    installed: Option<&str>,
    action: Action,
) -> Result<Reply, EngineError> {
    assert_eq!(action.harness(), catalog.harness);
    match action {
        Action::Catalog { after, .. } => Ok(Reply::Catalog {
            page: harness_settings::catalog_page(catalog, installed, after)?,
        }),
        Action::Values { project, after, .. } => Ok(Reply::Values {
            page: store::values(
                &versioned::open(root)?,
                catalog,
                installed,
                &scope(project),
                after,
            )?,
        }),
        Action::Set {
            project,
            key,
            value,
            version,
            ..
        } => Ok(store::set(
            &versioned::open(root)?,
            catalog,
            installed,
            &scope(project),
            &key,
            &value,
            version,
        )?
        .into()),
        Action::Reset {
            project,
            key,
            version,
            ..
        } => Ok(store::reset(
            &versioned::open(root)?,
            catalog,
            installed,
            &scope(project),
            &key,
            version,
        )?
        .into()),
    }
}

#[cfg(all(test, unix))]
#[path = "harness_settings_tests.rs"]
mod tests;
