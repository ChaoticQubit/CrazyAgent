use super::*;
use crate::app_plugins::{Command, Done};
use crate::engine::owner::AccountSecret;
use crate::protocol::encode_frame;
use kernel_plugin_host::usage::{ChangeNotice, TurnOff};
use kernel_plugin_host::views::{Listing, Preview};
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = crate::protocol::FRAME_BYTES_MAX;
const KEY_BYTES_MAX: usize = 8192;
const NAME_BYTES_MAX: usize = 64;
const ACCOUNT_BYTES_MAX: usize = 128;
const FOLDER_BYTES_MAX: usize = 4096;
const ACTION_TIME: Duration = Duration::from_secs(30);

/// Every code an App plugins request can answer an owner client with: the plugin store's
/// refusals, this protocol's own, and the owner connection's.
pub const APP_PLUGINS_CODES: &[&str] = &[
    "app_plugin_account_has_no_key",
    "app_plugin_account_not_found",
    "app_plugin_bundled",
    "app_plugin_changed",
    "app_plugin_essential",
    "app_plugin_folder_unavailable",
    "app_plugin_folder_unsafe",
    "app_plugin_key_invalid",
    "app_plugin_key_store_unavailable",
    "app_plugin_name_taken",
    "app_plugin_needs_review",
    "app_plugin_not_found",
    "app_plugin_outcome_unknown",
    "app_plugin_past_cap",
    "app_plugin_port_handed_out",
    "app_plugin_refused",
    "app_plugin_request_bounds",
    "app_plugin_request_invalid",
    "app_plugin_secret_not_declared",
    "app_plugin_setting_unavailable",
    "app_plugin_setting_unreadable",
    "app_plugin_setting_unsafe",
    "app_plugins_readonly",
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
];

/// One App plugins request. Reads: `list`, `preview` (the trust screen of a folder the
/// owner picked), `review` (the trust screen of a plugin already in the plugins folder) and
/// `users` (the runs holding a plugin, for the turn-off confirm). Changes: add a picked
/// folder, trust, turn on or off (off with a one-time choice of what happens to runs
/// holding it), remove, bind a secret to an account or to a key sent in the frame after
/// the request, and unbind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    List,
    Preview {
        folder: PathBuf,
    },
    Review {
        name: String,
    },
    Add {
        folder: PathBuf,
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
        #[serde(default)]
        when_off: Option<TurnOff>,
    },
    Users {
        name: String,
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
    SetWhenOff {
        when_off: TurnOff,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    app_plugins: Action,
    secret_present: bool,
}

/// The plain words the owner reads for one permission a plugin may hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionWords {
    pub name: String,
    pub words: String,
}

/// One holder of a plugin as the turn-off confirm names it: the run and its label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginUser {
    pub run_id: String,
    pub label: String,
}

/// The App plugins page: every plugin, the plain words for each permission, and the change
/// notices not yet shown, one per trusted plugin whose plugin.toml changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppPluginsPage {
    pub listing: Listing,
    pub words: Vec<PermissionWords>,
    pub notices: Vec<PluginChanged>,
    pub when_turned_off: WhenTurnedOff,
}

/// The "When a plugin is turned off" setting in force, and whether a project settings file
/// sets it, which wins over the choice saved here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhenTurnedOff {
    pub value: TurnOff,
    pub project_override: bool,
}

/// One change notice: the plugin and the SHA-256 it was trusted at and holds now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginChanged {
    pub plugin: String,
    pub trusted_sha256: String,
    pub current_sha256: String,
}

impl From<ChangeNotice> for PluginChanged {
    fn from(notice: ChangeNotice) -> Self {
        Self {
            plugin: notice.plugin,
            trusted_sha256: notice.trusted_sha256,
            current_sha256: notice.current_sha256,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Plugins { page: Box<AppPluginsPage> },
    Preview { preview: Box<Preview> },
    Users { users: Vec<PluginUser> },
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("app_plugin_outcome_unknown")
}

impl OwnerClient {
    /// Reads the App plugins page: bundled plugins, then the owner's, with the permission
    /// words and the change notices not yet shown. Output: the page.
    pub async fn app_plugins(&self) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_page(Action::List, None).await
    }

    /// Reads the trust screen of a folder the owner picked, saving nothing; the engine reads
    /// and hashes the folder itself. Input: the folder. Output: ready, or refused with the
    /// line or field and a plain sentence.
    pub async fn app_plugin_preview(&self, folder: PathBuf) -> Result<Preview, EngineError> {
        self.app_plugin_preview_of(Action::Preview { folder }).await
    }

    /// Reads the trust screen of a plugin already in the plugins folder. Input: the name.
    /// Output: ready or refused, as for a preview.
    pub async fn app_plugin_review(&self, name: String) -> Result<Preview, EngineError> {
        self.app_plugin_preview_of(Action::Review { name }).await
    }

    /// Copies a folder the owner approved on its trust screen into the plugins folder,
    /// trusted and on. Inputs: the folder, the SHA-256 the screen showed and whether it
    /// replaces a plugin of that name. Output: the page.
    pub async fn app_plugin_add(
        &self,
        folder: PathBuf,
        sha256: String,
        replace: bool,
    ) -> Result<AppPluginsPage, EngineError> {
        let action = Action::Add {
            folder,
            sha256,
            replace,
        };
        self.app_plugins_page(action, None).await
    }

    /// Trusts a plugin in the plugins folder after its trust screen. Inputs: the name and
    /// the SHA-256 the screen showed. Output: the page.
    pub async fn app_plugin_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_page(Action::Trust { name, sha256 }, None)
            .await
    }

    /// Turns one plugin on or off. Off follows the owner's setting for runs holding it,
    /// unless `when_off` picks another choice for this one call. Inputs: the name, the
    /// switch and the one-time choice. Output: the page.
    pub async fn app_plugin_set_enabled(
        &self,
        name: String,
        enabled: bool,
        when_off: Option<TurnOff>,
    ) -> Result<AppPluginsPage, EngineError> {
        let action = Action::SetEnabled {
            name,
            enabled,
            when_off,
        };
        self.app_plugins_page(action, None).await
    }

    /// Reads the runs holding one plugin, for the turn-off confirm. Input: the name.
    /// Output: the holders.
    pub async fn app_plugin_users(&self, name: String) -> Result<Vec<PluginUser>, EngineError> {
        match self
            .app_plugins_exchange(Action::Users { name }, None)
            .await?
        {
            Reply::Users { users } => Ok(users),
            _ => Err(unknown()),
        }
    }

    /// Removes one of the owner's plugins with its trust, bindings and the keys entered for
    /// it; account keys stay. Input: the name. Output: the page, or `app_plugin_bundled`.
    pub async fn app_plugin_remove(&self, name: String) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_page(Action::Remove { name }, None).await
    }

    /// Binds one declared secret to a saved API-key account's key. Inputs: the plugin, the
    /// secret and the account reference. Output: the page.
    pub async fn app_plugin_bind_account(
        &self,
        name: String,
        secret: String,
        account: String,
    ) -> Result<AppPluginsPage, EngineError> {
        let action = Action::BindAccount {
            name,
            secret,
            account,
        };
        self.app_plugins_page(action, None).await
    }

    /// Saves a key the owner entered for one declared secret, sent only in the frame after
    /// the request. Inputs: the plugin, the secret and the key. Output: the page.
    pub async fn app_plugin_set_key(
        &self,
        name: String,
        secret: String,
        key: AccountSecret,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_page(Action::SetKey { name, secret }, Some(key))
            .await
    }

    /// Clears one secret's binding, deleting the key entered for it. Inputs: the plugin
    /// and the secret. Output: the page.
    pub async fn app_plugin_unbind(
        &self,
        name: String,
        secret: String,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_page(Action::Unbind { name, secret }, None)
            .await
    }

    /// Saves what happens to running work when a plugin is turned off, in the global
    /// settings file. Input: the choice. Output: the page, with the choice now in force.
    pub async fn app_plugin_set_when_off(
        &self,
        when_off: TurnOff,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_page(Action::SetWhenOff { when_off }, None)
            .await
    }

    async fn app_plugins_page(
        &self,
        action: Action,
        key: Option<AccountSecret>,
    ) -> Result<AppPluginsPage, EngineError> {
        match self.app_plugins_exchange(action, key).await? {
            Reply::Plugins { page } => Ok(*page),
            _ => Err(unknown()),
        }
    }

    async fn app_plugin_preview_of(&self, action: Action) -> Result<Preview, EngineError> {
        match self.app_plugins_exchange(action, None).await? {
            Reply::Preview { preview } => Ok(*preview),
            _ => Err(unknown()),
        }
    }

    async fn app_plugins_exchange(
        &self,
        action: Action,
        key: Option<AccountSecret>,
    ) -> Result<Reply, EngineError> {
        assert_eq!(matches!(action, Action::SetKey { .. }), key.is_some());
        tokio::time::timeout(self.timeout.max(ACTION_TIME), async {
            let frame = encode_frame(&Envelope {
                app_plugins: action,
                secret_present: key.is_some(),
            })
            .map_err(|_| failure("app_plugin_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::ipc::owner_connect(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("app_plugins_unavailable"))?;
            verify_server(&stream)?;
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
                kernel_security::secret::wipe(&mut bytes);
                let sent = stream.write_all(&framed).await;
                kernel_security::secret::wipe(&mut framed);
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

/// Answers one admitted App plugins request: reads the key frame a `set_key` announces,
/// runs the request in the storage worker and writes one reply frame. Inputs: the stream,
/// the engine, the authority, the handshake, the admission and the request body. Output:
/// Ok once the reply is written.
pub(super) async fn serve(
    stream: &mut Stream,
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
    tokio::time::timeout(engine.timeout, stream.write_all(&frame))
        .await
        .map_err(|_| unknown())?
        .map_err(io_error)
}

async fn handle(
    stream: &mut Stream,
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
    let command = command(action, key.map(AccountSecret::into_bytes));
    let done = engine
        .owner_job(|reply| Job::AppPlugins(command, reply))
        .await?;
    Ok(match done {
        Done::Page {
            listing,
            notices,
            when_turned_off,
            project_override,
        } => Reply::Plugins {
            page: Box::new(AppPluginsPage {
                listing,
                words: crate::app_plugins::words::WORDS
                    .iter()
                    .map(|(name, words)| PermissionWords {
                        name: (*name).into(),
                        words: (*words).into(),
                    })
                    .collect(),
                notices: notices.into_iter().map(PluginChanged::from).collect(),
                when_turned_off: WhenTurnedOff {
                    value: when_turned_off,
                    project_override,
                },
            }),
        },
        Done::Preview(preview) => Reply::Preview {
            preview: Box::new(preview),
        },
        Done::Users(users) => Reply::Users {
            users: users
                .into_iter()
                .map(|holder| PluginUser {
                    run_id: holder.run_id,
                    label: holder.label,
                })
                .collect(),
        },
    })
}

/// The storage command for one parsed action. Inputs: the action and the key bytes that
/// came with a `set_key`. Output: the command.
fn command(action: Action, key: Option<Vec<u8>>) -> Command {
    match action {
        Action::List => Command::List,
        Action::Preview { folder } => Command::Preview { folder },
        Action::Review { name } => Command::Review { name },
        Action::Add {
            folder,
            sha256,
            replace,
        } => Command::Add {
            folder,
            sha256,
            replace,
        },
        Action::Trust { name, sha256 } => Command::Trust { name, sha256 },
        Action::SetEnabled {
            name,
            enabled,
            when_off,
        } => Command::SetEnabled {
            name,
            enabled,
            when_off,
        },
        Action::Users { name } => Command::Users { name },
        Action::Remove { name } => Command::Remove { name },
        Action::BindAccount {
            name,
            secret,
            account,
        } => Command::BindAccount {
            name,
            secret,
            account,
        },
        Action::SetKey { name, secret } => Command::SetKey {
            name,
            secret,
            key: key.expect("parse admits set_key only with its key frame"),
        },
        Action::Unbind { name, secret } => Command::Unbind { name, secret },
        Action::SetWhenOff { when_off } => Command::SetWhenOff { when_off },
    }
}

/// Reads one App plugins request from an owner frame body: the action and whether a key
/// frame follows, which only `set_key` may announce and must announce. Names, the account
/// and the folder are bounded here; the store checks them fully, and a folder must be an
/// absolute path. Input: the body. Output: the action and the flag, or
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
    let names: Vec<&str> = match &app_plugins {
        Action::Review { name }
        | Action::Trust { name, .. }
        | Action::SetEnabled { name, .. }
        | Action::Users { name }
        | Action::Remove { name } => vec![name],
        Action::BindAccount { name, secret, .. }
        | Action::SetKey { name, secret }
        | Action::Unbind { name, secret } => vec![name, secret],
        Action::List | Action::Preview { .. } | Action::Add { .. } | Action::SetWhenOff { .. } => {
            Vec::new()
        }
    };
    let folder_invalid =
        |folder: &PathBuf| !folder.is_absolute() || folder.as_os_str().len() > FOLDER_BYTES_MAX;
    let invalid = names
        .iter()
        .any(|name| name.is_empty() || name.len() > NAME_BYTES_MAX)
        || match &app_plugins {
            Action::BindAccount { account, .. } => {
                account.is_empty() || account.len() > ACCOUNT_BYTES_MAX
            }
            Action::Preview { folder } | Action::Add { folder, .. } => folder_invalid(folder),
            _ => false,
        };
    if invalid {
        return Err(failure("app_plugin_request_invalid"));
    }
    Ok((app_plugins, secret_present))
}

async fn receive_key(stream: &mut Stream) -> Result<AccountSecret, EngineError> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let invalid = || failure("app_plugin_key_invalid");
        let size = stream.read_u32().await.map_err(|_| invalid())? as usize;
        if size == 0 || size > KEY_BYTES_MAX {
            return Err(invalid());
        }
        let mut bytes = vec![0; size];
        if stream.read_exact(&mut bytes).await.is_err() {
            kernel_security::secret::wipe(&mut bytes);
            return Err(invalid());
        }
        AccountSecret::new(bytes).map_err(|_| invalid())
    })
    .await
    .map_err(|_| failure("app_plugin_key_invalid"))?
}

#[cfg(test)]
#[path = "app_plugins_tests.rs"]
mod tests;
