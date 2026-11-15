use super::*;
use crate::app_plugins::{PluginKind, Preview, Problem, store};
use crate::connectors::{ConnectorSettings, ConnectorsView, Who};
use crate::engine::owner::AccountSecret;
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = crate::protocol::FRAME_BYTES_MAX;
const NAME_BYTES_MAX: usize = 64;
const PROJECT_BYTES_MAX: usize = 128;
pub(super) const CONNECTORS_TIMEOUT: Duration = Duration::from_secs(30);

/// Every code a Connectors request can answer an owner client with: the plugin store's
/// refusals a connector can meet, this protocol's own, and the owner connection's.
pub const CONNECTORS_CODES: &[&str] = &[
    "app_plugin_bundled",
    "app_plugin_changed",
    "app_plugin_connector_oauth_not_yet",
    "app_plugin_folder_unavailable",
    "app_plugin_folder_unsafe",
    "app_plugin_key_invalid",
    "app_plugin_key_store_unavailable",
    "app_plugin_name_taken",
    "app_plugin_needs_key",
    "app_plugin_needs_review",
    "app_plugin_not_found",
    "app_plugin_refused",
    "app_plugin_secret_not_declared",
    "app_plugins_unavailable",
    "connector_outcome_unknown",
    "connector_project_unknown",
    "connector_request_bounds",
    "connector_request_invalid",
    "connectors_unavailable",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_entropy_unavailable",
    "owner_grant_stale_or_revoked",
    "owner_request_failed",
    "owner_root_replaced_or_unsafe",
    "owner_root_unavailable",
    "owner_socket_replaced",
    "owner_socket_unsafe",
    "peer_owner_mismatch",
    "questions_unavailable",
];

/// One Connectors request. Reads: `view` (the page, with one project's switches),
/// `preview` (a pasted manifest's trust screen) and `review` (the trust screen of a listed
/// connector). Changes: add, trust, remove, save a key sent in the frame after the
/// request, clear a key, turn a connector on or off for a project, and save the settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    View {
        project: Option<String>,
    },
    Preview {
        yaml: String,
    },
    Review {
        name: String,
    },
    Add {
        yaml: String,
        sha256: String,
        replace: bool,
    },
    Trust {
        name: String,
        sha256: String,
    },
    Remove {
        name: String,
    },
    SetKey {
        name: String,
        secret: String,
    },
    ClearKey {
        name: String,
        secret: String,
    },
    Grant {
        project: String,
        connector: String,
        enabled: bool,
        who: Who,
    },
    Set {
        settings: ConnectorSettings,
        version: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    connectors: Action,
    secret_present: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Connectors { view: Box<ConnectorsView> },
    Preview { preview: Box<Preview> },
    Stale { current: u64 },
    Rejected { code: String },
}

/// How a connector settings save ended: saved, with the page after it, or refused because
/// the settings changed since the owner's screen read them, with the current version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorsSaved {
    Saved(Box<ConnectorsView>),
    Stale(u64),
}

fn unknown() -> EngineError {
    failure("connector_outcome_unknown")
}

impl OwnerClient {
    /// Reads the Connectors page. Input: the project whose switches to include, if any.
    /// Output: every connector, the settings and that project's switches.
    pub async fn connectors(&self, project: Option<String>) -> Result<ConnectorsView, EngineError> {
        self.connectors_view(Action::View { project }, None).await
    }

    /// Reads the trust screen of a pasted or picked connector manifest without saving
    /// anything. Input: the manifest text. Output: ready with every host, header, scope and
    /// read tool it declares, or refused with the line or field.
    pub async fn connector_preview(&self, yaml: String) -> Result<Preview, EngineError> {
        self.connector_trust_screen(Action::Preview { yaml }).await
    }

    /// Reads the trust screen of a listed connector, for one that needs review. Input:
    /// the connector's name. Output: ready or refused, as for a preview.
    pub async fn connector_review(&self, name: String) -> Result<Preview, EngineError> {
        self.connector_trust_screen(Action::Review { name }).await
    }

    /// Adds a connector manifest the owner confirmed on its trust screen. It reaches no
    /// project until it is turned on for one. Inputs: the text, the SHA-256 the trust
    /// screen showed and whether it replaces the owner's connector of that name. Output:
    /// the page.
    pub async fn connector_add(
        &self,
        yaml: String,
        sha256: String,
        replace: bool,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_view(
            Action::Add {
                yaml,
                sha256,
                replace,
            },
            None,
        )
        .await
    }

    /// Trusts a connector file in the owner's folder after its trust screen. Inputs: the
    /// name and the SHA-256 the screen showed. Output: the page.
    pub async fn connector_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_view(Action::Trust { name, sha256 }, None)
            .await
    }

    /// Removes one of the owner's connectors: its file, trust record, keys and its switch
    /// in every project. Input: the name. Output: the page, or `app_plugin_bundled`.
    pub async fn connector_remove(&self, name: String) -> Result<ConnectorsView, EngineError> {
        self.connectors_view(Action::Remove { name }, None).await
    }

    /// Saves a key the owner entered for one of a connector's secrets, sent only as a
    /// separate frame and kept in a private file. Inputs: connector, secret name and the
    /// key. Output: the page, which never holds the key.
    pub async fn connector_set_key(
        &self,
        name: String,
        secret: String,
        key: AccountSecret,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_view(Action::SetKey { name, secret }, Some(key))
            .await
    }

    /// Deletes the key saved for one of a connector's secrets. Inputs: connector and
    /// secret name. Output: the page.
    pub async fn connector_clear_key(
        &self,
        name: String,
        secret: String,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_view(Action::ClearKey { name, secret }, None)
            .await
    }

    /// Turns a connector on or off for one project and says who it reaches. Inputs: the
    /// project, the connector, the switch and who. Output: the page with that project's
    /// switches.
    pub async fn connector_grant(
        &self,
        project: String,
        connector: String,
        enabled: bool,
        who: Who,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_view(
            Action::Grant {
                project,
                connector,
                enabled,
                who,
            },
            None,
        )
        .await
    }

    /// Saves the connector settings. Inputs: the settings and the version the screen
    /// read. Output: saved with the page, or stale with the current version.
    pub async fn set_connector_settings(
        &self,
        settings: ConnectorSettings,
        version: u64,
    ) -> Result<ConnectorsSaved, EngineError> {
        match self
            .connectors_exchange(Action::Set { settings, version }, None)
            .await?
        {
            Reply::Connectors { view } => Ok(ConnectorsSaved::Saved(view)),
            Reply::Stale { current } => Ok(ConnectorsSaved::Stale(current)),
            _ => Err(unknown()),
        }
    }

    async fn connectors_view(
        &self,
        action: Action,
        key: Option<AccountSecret>,
    ) -> Result<ConnectorsView, EngineError> {
        match self.connectors_exchange(action, key).await? {
            Reply::Connectors { view } => Ok(*view),
            _ => Err(unknown()),
        }
    }

    async fn connector_trust_screen(&self, action: Action) -> Result<Preview, EngineError> {
        match self.connectors_exchange(action, None).await? {
            Reply::Preview { preview } => Ok(*preview),
            _ => Err(unknown()),
        }
    }

    async fn connectors_exchange(
        &self,
        action: Action,
        key: Option<AccountSecret>,
    ) -> Result<Reply, EngineError> {
        assert_eq!(
            matches!(action, Action::SetKey { .. }),
            key.is_some(),
            "a key travels with a set_key request only"
        );
        tokio::time::timeout(self.timeout.max(CONNECTORS_TIMEOUT), async {
            let frame = encode_frame(&Envelope {
                connectors: action,
                secret_present: key.is_some(),
            })
            .map_err(|_| failure("connector_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("connectors_unavailable"))?;
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
                .map_err(|_| failure("connectors_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("connectors_unavailable"))?;
            if let Some(key) = key {
                let mut bytes = key.into_bytes();
                let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
                framed.extend_from_slice(&bytes);
                bytes.fill(0);
                let sent = stream.write_all(&framed).await;
                framed.fill(0);
                sent.map_err(|_| failure("connectors_unavailable"))?;
            }
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

/// Answers one admitted Connectors request on its stream: reads the key frame that a
/// `set_key` request announces, runs the request and writes one reply frame.
/// Inputs: the stream, the engine, the authority, the handshake, the admission and the
/// request body. Output: Ok once the reply is written.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let reply = match handle(stream, engine, authority, packet, admission, bytes).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "connector_request_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    stream.write_all(&frame).await.map_err(io_error)
}

async fn handle(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Reply, EngineError> {
    let (action, key_present) = parse(bytes)?;
    let key = if key_present {
        Some(super::app_plugins::receive_key(stream).await?)
    } else {
        None
    };
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    tokio::task::spawn_blocking(move || run(&root, action, key))
        .await
        .map_err(|_| failure("connectors_unavailable"))?
}

/// Reads one Connectors request from an owner frame body: the action and whether a key
/// frame follows, which only `set_key` may announce and must announce. Names and project
/// ids are bounded here; the store checks them fully. Input: the body. Output: the action
/// and the flag, or `connector_request_bounds` or `connector_request_invalid`.
fn parse(bytes: &[u8]) -> Result<(Action, bool), EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("connector_request_bounds"));
    }
    let Envelope {
        connectors,
        secret_present,
    } = serde_json::from_slice(bytes).map_err(|_| failure("connector_request_invalid"))?;
    if matches!(connectors, Action::SetKey { .. }) != secret_present {
        return Err(failure("connector_request_invalid"));
    }
    let names: &[&str] = match &connectors {
        Action::Review { name } | Action::Trust { name, .. } | Action::Remove { name } => {
            &[name.as_str()]
        }
        Action::SetKey { name, secret } | Action::ClearKey { name, secret } => {
            &[name.as_str(), secret.as_str()]
        }
        Action::Grant { connector, .. } => &[connector.as_str()],
        Action::View { .. } | Action::Preview { .. } | Action::Add { .. } | Action::Set { .. } => {
            &[]
        }
    };
    let project = match &connectors {
        Action::View { project } => project.as_deref(),
        Action::Grant { project, .. } => Some(project.as_str()),
        _ => None,
    };
    if names
        .iter()
        .any(|name| name.is_empty() || name.len() > NAME_BYTES_MAX)
        || project.is_some_and(|project| project.is_empty() || project.len() > PROJECT_BYTES_MAX)
    {
        return Err(failure("connector_request_invalid"));
    }
    Ok((connectors, secret_present))
}

/// The Connectors page's view of a trust screen: a plugin of another kind is refused
/// there, since it is added on the App plugins page. Input: the store's preview. Output:
/// the same preview, or the refusal naming `kind`.
fn on_page(preview: Preview) -> Preview {
    match preview {
        Preview::Ready { trust } if trust.kind != PluginKind::Connector => Preview::Refused {
            problem: Problem::new("app_plugin_not_connector", None, Some("kind")),
        },
        other => other,
    }
}

/// Refuses a name that is not a connector's, so this page never changes an embedding
/// plugin. Inputs: the engine root and the name. Output: Ok, or `app_plugin_not_found`.
fn connector(root: &Path, name: &str) -> Result<(), EngineError> {
    match store::kind_of(root, name)? {
        Some(PluginKind::Connector) => Ok(()),
        _ => Err(failure("app_plugin_not_found")),
    }
}

/// Runs one authenticated Connectors request against the engine database and folders in
/// the engine root. Every by-name change acts on connectors only. Inputs: the engine
/// root, the action and the key that came with a `set_key`. Output: the reply, or the
/// refusal.
fn run(root: &Path, action: Action, key: Option<AccountSecret>) -> Result<Reply, EngineError> {
    let db = crate::questions::open(root)?;
    let now = crate::semantic::now_ms();
    let page = |db: &rusqlite::Connection, project: Option<&str>| -> Result<Reply, EngineError> {
        Ok(Reply::Connectors {
            view: Box::new(crate::connectors::view(db, root, project)?),
        })
    };
    match action {
        Action::View { project } => page(&db, project.as_deref()),
        Action::Preview { yaml } => Ok(Reply::Preview {
            preview: Box::new(on_page(store::preview(root, &yaml)?)),
        }),
        Action::Review { name } => {
            connector(root, &name)?;
            Ok(Reply::Preview {
                preview: Box::new(store::review(root, &name)?),
            })
        }
        Action::Add {
            yaml,
            sha256,
            replace,
        } => {
            if !matches!(
                crate::app_plugins::manifest::parse(&yaml),
                Ok(manifest) if manifest.kind == PluginKind::Connector
            ) {
                return Err(failure("app_plugin_refused"));
            }
            store::add(&db, root, &yaml, &sha256, replace, now)?;
            page(&db, None)
        }
        Action::Trust { name, sha256 } => {
            connector(root, &name)?;
            store::trust_file(&db, root, &name, &sha256, now)?;
            page(&db, None)
        }
        Action::Remove { name } => {
            connector(root, &name)?;
            store::remove(&db, root, &name)?;
            page(&db, None)
        }
        Action::SetKey { name, secret } => {
            connector(root, &name)?;
            let key = key.expect("parse admits set_key only with its key frame");
            store::set_key(&db, root, &name, &secret, key.into_bytes())?;
            page(&db, None)
        }
        Action::ClearKey { name, secret } => {
            connector(root, &name)?;
            store::unbind(&db, root, &name, &secret)?;
            page(&db, None)
        }
        Action::Grant {
            project,
            connector: name,
            enabled,
            who,
        } => {
            crate::connectors::grants::set(&db, root, &project, &name, enabled, who, now)?;
            page(&db, Some(project.as_str()))
        }
        Action::Set { settings, version } => {
            match crate::connectors::settings::save(&db, &settings, version)? {
                Ok(_) => page(&db, None),
                Err(current) => Ok(Reply::Stale { current }),
            }
        }
    }
}

#[cfg(test)]
#[path = "connectors_tests.rs"]
mod tests;
