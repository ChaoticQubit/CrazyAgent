use super::*;
use crate::app_plugins::{Listing, Preview, store};
use crate::engine::owner::AccountSecret;
use crate::protocol::encode_frame;
use crate::semantic::{SearchChoice, SearchView};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = crate::protocol::FRAME_BYTES_MAX;
const KEY_BYTES_MAX: usize = 8192;
const NAME_BYTES_MAX: usize = 64;
pub(super) const APP_PLUGINS_TIMEOUT: Duration = Duration::from_secs(30);

/// Every code an App plugins or Search request can answer an owner client with: the
/// plugin and search refusals, this protocol's own, and the owner connection's.
pub const APP_PLUGINS_CODES: &[&str] = &[
    "app_plugin_account_has_no_key",
    "app_plugin_account_not_found",
    "app_plugin_bundled",
    "app_plugin_changed",
    "app_plugin_folder_unavailable",
    "app_plugin_folder_unsafe",
    "app_plugin_key_invalid",
    "app_plugin_key_store_unavailable",
    "app_plugin_name_taken",
    "app_plugin_needs_review",
    "app_plugin_not_found",
    "app_plugin_outcome_unknown",
    "app_plugin_refused",
    "app_plugin_request_bounds",
    "app_plugin_request_invalid",
    "app_plugin_secret_not_declared",
    "app_plugins_unavailable",
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
    "search_choice_unavailable",
    "search_confirm_required",
    "search_model_downloading",
    "search_model_store_unavailable",
];

/// One App plugins or Search request. Reads: `list`, `preview` (a pasted manifest's trust
/// screen), `review` (the trust screen of a file in the owner's folder) and `search`.
/// Changes: add, trust, turn on or off, remove, bind a secret to an account or to a key
/// sent in the frame after the request, unbind, save the Search choice, and start, cancel
/// or delete the local model download.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    List,
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
    SetEnabled {
        name: String,
        enabled: bool,
    },
    Remove {
        name: String,
    },
    BindAccount {
        name: String,
        secret: String,
        account: String,
    },
    SetKey {
        name: String,
        secret: String,
    },
    Unbind {
        name: String,
        secret: String,
    },
    Search,
    SetSearch {
        choice: SearchChoice,
        confirm_host: Option<String>,
        version: u64,
    },
    DownloadModel,
    CancelDownload,
    DeleteModel,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    app_plugins: Action,
    secret_present: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Plugins { listing: Box<Listing> },
    Preview { preview: Box<Preview> },
    Search { search: Box<SearchView> },
    Stale { current: u64 },
    Rejected { code: String },
}

/// How a Search choice save ended: saved, with the page after it, or refused because the
/// choice changed since the owner's screen read it, with the current version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchSaved {
    Saved(Box<SearchView>),
    Stale(u64),
}

fn unknown() -> EngineError {
    failure("app_plugin_outcome_unknown")
}

impl OwnerClient {
    /// Reads every app plugin: bundled ones, then the owner's files. Output: the listing.
    pub async fn app_plugins(&self) -> Result<Listing, EngineError> {
        self.listing(Action::List, None).await
    }

    /// Reads the trust screen of a pasted or picked manifest without saving anything.
    /// Input: the manifest text. Output: ready with hosts, secrets and SHA-256, or refused
    /// with the line or field and a plain sentence.
    pub async fn app_plugin_preview(&self, yaml: String) -> Result<Preview, EngineError> {
        self.preview(Action::Preview { yaml }).await
    }

    /// Reads the trust screen of a plugin file in the owner's folder, for one that needs
    /// review. Input: the plugin name. Output: ready or refused, as for a preview.
    pub async fn app_plugin_review(&self, name: String) -> Result<Preview, EngineError> {
        self.preview(Action::Review { name }).await
    }

    /// Adds a manifest the owner confirmed on its trust screen, saved as a private file in
    /// the owner's folder and turned on. Inputs: the text, the SHA-256 the trust screen
    /// showed, and whether it replaces a user plugin of that name. Output: the listing.
    pub async fn app_plugin_add(
        &self,
        yaml: String,
        sha256: String,
        replace: bool,
    ) -> Result<Listing, EngineError> {
        self.listing(
            Action::Add {
                yaml,
                sha256,
                replace,
            },
            None,
        )
        .await
    }

    /// Trusts a file in the owner's folder after its trust screen. Inputs: the plugin name
    /// and the SHA-256 the screen showed. Output: the listing.
    pub async fn app_plugin_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<Listing, EngineError> {
        self.listing(Action::Trust { name, sha256 }, None).await
    }

    /// Turns one plugin on or off. Inputs: the name and the switch. Output: the listing.
    pub async fn app_plugin_set_enabled(
        &self,
        name: String,
        enabled: bool,
    ) -> Result<Listing, EngineError> {
        self.listing(Action::SetEnabled { name, enabled }, None)
            .await
    }

    /// Removes one of the owner's plugins: its file, trust record, secret bindings, keys
    /// and vectors. Input: the name. Output: the listing, or `app_plugin_bundled`.
    pub async fn app_plugin_remove(&self, name: String) -> Result<Listing, EngineError> {
        self.listing(Action::Remove { name }, None).await
    }

    /// Binds one declared secret to a saved API-key account's key. Inputs: plugin name,
    /// secret name and the account's credential reference. Output: the listing.
    pub async fn app_plugin_bind_account(
        &self,
        name: String,
        secret: String,
        account: String,
    ) -> Result<Listing, EngineError> {
        self.listing(
            Action::BindAccount {
                name,
                secret,
                account,
            },
            None,
        )
        .await
    }

    /// Saves a key the owner entered for one declared secret, sent only as a separate frame
    /// and kept in a private file; replaces the plugin's earlier key for that secret.
    /// Inputs: plugin name, secret name and the key. Output: the listing.
    pub async fn app_plugin_set_key(
        &self,
        name: String,
        secret: String,
        key: AccountSecret,
    ) -> Result<Listing, EngineError> {
        self.listing(Action::SetKey { name, secret }, Some(key))
            .await
    }

    /// Clears one secret's binding, deleting the plugin's own key when it had one.
    /// Inputs: plugin name and secret name. Output: the listing.
    pub async fn app_plugin_unbind(
        &self,
        name: String,
        secret: String,
    ) -> Result<Listing, EngineError> {
        self.listing(Action::Unbind { name, secret }, None).await
    }

    /// Reads the Search settings page. Output: choice, options, model and progress.
    pub async fn search_settings(&self) -> Result<SearchView, EngineError> {
        self.search(Action::Search).await
    }

    /// Saves what search ranks with. Choosing an HTTP plugin needs `confirm_host` equal to
    /// the host its confirm named. Inputs: the choice, the confirmed host and the version the
    /// screen read. Output: saved with the page, or stale with the current version.
    pub async fn set_search(
        &self,
        choice: SearchChoice,
        confirm_host: Option<String>,
        version: u64,
    ) -> Result<SearchSaved, EngineError> {
        match self
            .app_plugins_exchange(
                Action::SetSearch {
                    choice,
                    confirm_host,
                    version,
                },
                None,
            )
            .await?
        {
            Reply::Search { search } => Ok(SearchSaved::Saved(search)),
            Reply::Stale { current } => Ok(SearchSaved::Stale(current)),
            _ => Err(unknown()),
        }
    }

    /// Starts the local model's one-time download after the owner confirmed its size.
    /// Output: the Search page, with the model downloading.
    pub async fn search_model_download(&self) -> Result<SearchView, EngineError> {
        self.search(Action::DownloadModel).await
    }

    /// Cancels the local model download and deletes the partial file. Output: the page.
    pub async fn search_model_cancel(&self) -> Result<SearchView, EngineError> {
        self.search(Action::CancelDownload).await
    }

    /// Deletes the downloaded local model; search falls back to full text until it is
    /// downloaded again. Output: the page.
    pub async fn search_model_delete(&self) -> Result<SearchView, EngineError> {
        self.search(Action::DeleteModel).await
    }

    async fn listing(
        &self,
        action: Action,
        key: Option<AccountSecret>,
    ) -> Result<Listing, EngineError> {
        match self.app_plugins_exchange(action, key).await? {
            Reply::Plugins { listing } => Ok(*listing),
            _ => Err(unknown()),
        }
    }

    async fn preview(&self, action: Action) -> Result<Preview, EngineError> {
        match self.app_plugins_exchange(action, None).await? {
            Reply::Preview { preview } => Ok(*preview),
            _ => Err(unknown()),
        }
    }

    async fn search(&self, action: Action) -> Result<SearchView, EngineError> {
        match self.app_plugins_exchange(action, None).await? {
            Reply::Search { search } => Ok(*search),
            _ => Err(unknown()),
        }
    }

    async fn app_plugins_exchange(
        &self,
        action: Action,
        key: Option<AccountSecret>,
    ) -> Result<Reply, EngineError> {
        assert_eq!(
            matches!(action, Action::SetKey { .. }),
            key.is_some(),
            "a key travels with a set_key request only"
        );
        tokio::time::timeout(self.timeout.max(APP_PLUGINS_TIMEOUT), async {
            let frame = encode_frame(&Envelope {
                app_plugins: action,
                secret_present: key.is_some(),
            })
            .map_err(|_| failure("app_plugin_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("app_plugins_unavailable"))?;
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
                .map_err(|_| failure("app_plugins_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("app_plugins_unavailable"))?;
            if let Some(key) = key {
                let mut bytes = key.into_bytes();
                let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
                framed.extend_from_slice(&bytes);
                bytes.fill(0);
                let sent = stream.write_all(&framed).await;
                framed.fill(0);
                sent.map_err(|_| failure("app_plugins_unavailable"))?;
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

/// Answers one admitted App plugins request on its stream: reads the key frame that a
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
            code: "app_plugin_request_bounds".into(),
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
        Some(receive_key(stream).await?)
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
        .map_err(|_| failure("app_plugins_unavailable"))?
}

/// Reads one App plugins request from an owner frame body: the action and whether a key
/// frame follows, which only `set_key` may announce and must announce. Names are bounded
/// here; the store checks them fully. Input: the body. Output: the action and the flag, or
/// `app_plugin_request_bounds` or `app_plugin_request_invalid`.
fn parse(bytes: &[u8]) -> Result<(Action, bool), EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("app_plugin_request_bounds"));
    }
    let Envelope {
        app_plugins,
        secret_present,
    } = serde_json::from_slice(bytes).map_err(|_| failure("app_plugin_request_invalid"))?;
    if matches!(app_plugins, Action::SetKey { .. }) != secret_present {
        return Err(failure("app_plugin_request_invalid"));
    }
    let names: &[&str] = match &app_plugins {
        Action::Review { name }
        | Action::Trust { name, .. }
        | Action::SetEnabled { name, .. }
        | Action::Remove { name } => &[name.as_str()],
        Action::BindAccount {
            name,
            secret,
            account,
        } => &[name.as_str(), secret.as_str(), account.as_str()],
        Action::SetKey { name, secret } | Action::Unbind { name, secret } => {
            &[name.as_str(), secret.as_str()]
        }
        _ => &[],
    };
    if names
        .iter()
        .any(|name| name.is_empty() || name.len() > NAME_BYTES_MAX)
    {
        return Err(failure("app_plugin_request_invalid"));
    }
    Ok((app_plugins, secret_present))
}

async fn receive_key(stream: &mut UnixStream) -> Result<AccountSecret, EngineError> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let invalid = || failure("app_plugin_key_invalid");
        let size = stream.read_u32().await.map_err(|_| invalid())? as usize;
        if size == 0 || size > KEY_BYTES_MAX {
            return Err(invalid());
        }
        let mut bytes = vec![0; size];
        if stream.read_exact(&mut bytes).await.is_err() {
            bytes.fill(0);
            return Err(invalid());
        }
        AccountSecret::new(bytes).map_err(|_| invalid())
    })
    .await
    .map_err(|_| failure("app_plugin_key_invalid"))?
}

/// Runs one authenticated App plugins or Search request against the engine database and
/// folders in the engine root. Inputs: the engine root, the action and the key that came
/// with a `set_key`. Output: the reply, or the store's refusal.
fn run(root: &Path, action: Action, key: Option<AccountSecret>) -> Result<Reply, EngineError> {
    let db = crate::questions::open(root)?;
    let now = crate::semantic::now_ms();
    let listing = |db: &rusqlite::Connection| -> Result<Reply, EngineError> {
        Ok(Reply::Plugins {
            listing: Box::new(store::list(db, root)?),
        })
    };
    let search = |db: &rusqlite::Connection| -> Result<Reply, EngineError> {
        Ok(Reply::Search {
            search: Box::new(crate::semantic::settings::view(db, root)?),
        })
    };
    match action {
        Action::List => listing(&db),
        Action::Preview { yaml } => Ok(Reply::Preview {
            preview: Box::new(store::preview(root, &yaml)?),
        }),
        Action::Review { name } => Ok(Reply::Preview {
            preview: Box::new(store::review(root, &name)?),
        }),
        Action::Add {
            yaml,
            sha256,
            replace,
        } => {
            store::add(&db, root, &yaml, &sha256, replace, now)?;
            listing(&db)
        }
        Action::Trust { name, sha256 } => {
            store::trust_file(&db, root, &name, &sha256, now)?;
            listing(&db)
        }
        Action::SetEnabled { name, enabled } => {
            store::set_enabled(&db, root, &name, enabled)?;
            listing(&db)
        }
        Action::Remove { name } => {
            store::remove(&db, root, &name)?;
            listing(&db)
        }
        Action::BindAccount {
            name,
            secret,
            account,
        } => {
            store::bind_account(&db, root, &name, &secret, &account)?;
            listing(&db)
        }
        Action::SetKey { name, secret } => {
            let key = key.expect("parse admits set_key only with its key frame");
            store::set_key(&db, root, &name, &secret, key.into_bytes())?;
            listing(&db)
        }
        Action::Unbind { name, secret } => {
            store::unbind(&db, root, &name, &secret)?;
            listing(&db)
        }
        Action::Search => search(&db),
        Action::SetSearch {
            choice,
            confirm_host,
            version,
        } => match crate::semantic::settings::set(
            &db,
            root,
            &choice,
            confirm_host.as_deref(),
            version,
        )? {
            Ok(view) => Ok(Reply::Search {
                search: Box::new(view),
            }),
            Err(current) => Ok(Reply::Stale { current }),
        },
        Action::DownloadModel => {
            crate::semantic::local::start_download(root)?;
            search(&db)
        }
        Action::CancelDownload => {
            crate::semantic::local::cancel_download(root);
            search(&db)
        }
        Action::DeleteModel => {
            crate::semantic::local::delete(root)?;
            search(&db)
        }
    }
}

#[cfg(test)]
#[path = "app_plugins_tests.rs"]
mod tests;
