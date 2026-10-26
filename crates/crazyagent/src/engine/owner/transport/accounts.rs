use super::*;
use crate::accounts::{AccountDraft, AccountStatus, AccountView, Field, ProviderEntry, probe};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::engine::owner::AccountSecret;
use crate::engine::owner::native_auth::NativeAuthPhase;
use crate::protocol::{CredentialRef, encode_frame};
use crate::storage::accounts::{Command, Listed, ResultValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::fd::OwnedFd;

#[cfg(unix)]
const KEYS: &str = "account-keys";
const SECRET_MAX: usize = 8192;
const LIST_PROBE: Duration = Duration::from_millis(400);
const CHECK_PROBE: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Catalog,
    List {
        after: Option<CredentialRef>,
        limit: u16,
    },
    Add {
        draft: AccountDraft,
    },
    Rename {
        id: CredentialRef,
        name: String,
    },
    Remove {
        id: CredentialRef,
    },
    ReplaceKey {
        id: CredentialRef,
    },
    Check {
        id: CredentialRef,
    },
    LocalModels {
        url: String,
    },
}

impl Action {
    /// The longest the engine spends on an action once it has read it: one owner wait
    /// for the account records, plus the network probes the action makes.
    /// Input: the owner wait. Output: the action's budget.
    fn time(&self, owner: Duration) -> Duration {
        match self {
            Self::Check { .. } | Self::LocalModels { .. } => owner + CHECK_PROBE,
            Self::List { .. } | Self::Add { .. } | Self::Rename { .. } => owner + LIST_PROBE,
            Self::ReplaceKey { .. } => owner * 2 + LIST_PROBE,
            Self::Catalog | Self::Remove { .. } => owner,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    accounts: Action,
    secret_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPage {
    pub accounts: Vec<AccountView>,
    pub next: Option<CredentialRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountCheck {
    pub account: AccountView,
    pub models: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Catalog { providers: Vec<ProviderEntry> },
    Page { page: AccountPage },
    Account { account: AccountView },
    Checked { check: AccountCheck },
    Removed,
    Models { models: Vec<String> },
    Rejected { code: String },
}

impl OwnerClient {
    /// Reads the provider catalog the add-account menu renders.
    /// Output: providers in display order with their plain sign-in methods.
    pub async fn account_catalog(&self) -> Result<Vec<ProviderEntry>, EngineError> {
        match self.accounts_exchange(Action::Catalog, None).await? {
            Reply::Catalog { providers } => Ok(providers),
            _ => Err(unknown()),
        }
    }

    /// Reads one page of accounts with plain statuses. Inputs: the previous page's `next`
    /// (or none) and a page size of 1 to 200. Output: accounts and the next cursor.
    pub async fn list_accounts(
        &self,
        after: Option<CredentialRef>,
        limit: u16,
    ) -> Result<AccountPage, EngineError> {
        match self
            .accounts_exchange(Action::List { after, limit }, None)
            .await?
        {
            Reply::Page { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Adds one account from a catalog choice. A key, when given, travels only as a separate
    /// socket frame and the engine keeps it in private custody. Output: the new account.
    pub async fn add_account(
        &self,
        draft: AccountDraft,
        key: Option<AccountSecret>,
    ) -> Result<AccountView, EngineError> {
        match self.accounts_exchange(Action::Add { draft }, key).await? {
            Reply::Account { account } => Ok(account),
            _ => Err(unknown()),
        }
    }

    /// Renames one account. Inputs: account id and the new plain name. Output: the account.
    pub async fn rename_account(
        &self,
        id: CredentialRef,
        name: String,
    ) -> Result<AccountView, EngineError> {
        match self
            .accounts_exchange(Action::Rename { id, name }, None)
            .await?
        {
            Reply::Account { account } => Ok(account),
            _ => Err(unknown()),
        }
    }

    /// Removes one account that is in no pool, and its saved key. Input: the account id.
    pub async fn remove_account(&self, id: CredentialRef) -> Result<(), EngineError> {
        match self.accounts_exchange(Action::Remove { id }, None).await? {
            Reply::Removed => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Replaces the saved key of one account that uses a key, in place: the account keeps
    /// its id, name and pools, and the old key file is swapped out atomically. The key
    /// travels only as a separate socket frame. Inputs: account id and new key. Output: it.
    pub async fn replace_account_key(
        &self,
        id: CredentialRef,
        key: AccountSecret,
    ) -> Result<AccountView, EngineError> {
        match self
            .accounts_exchange(Action::ReplaceKey { id }, Some(key))
            .await?
        {
            Reply::Account { account } => Ok(account),
            _ => Err(unknown()),
        }
    }

    /// Checks one account now: its address answers and, over plain HTTP, accepts the saved
    /// key on `/models`. Input: the account id. Output: fresh status and any listed models.
    pub async fn check_account(&self, id: CredentialRef) -> Result<AccountCheck, EngineError> {
        match self.accounts_exchange(Action::Check { id }, None).await? {
            Reply::Checked { check } => Ok(check),
            _ => Err(unknown()),
        }
    }

    /// Lists the models pulled on a local model server such as Ollama.
    /// Input: a loopback http address. Output: sorted model names.
    pub async fn local_models(&self, url: String) -> Result<Vec<String>, EngineError> {
        match self
            .accounts_exchange(Action::LocalModels { url }, None)
            .await?
        {
            Reply::Models { models } => Ok(models),
            _ => Err(unknown()),
        }
    }

    async fn accounts_exchange(
        &self,
        action: Action,
        key: Option<AccountSecret>,
    ) -> Result<Reply, EngineError> {
        let time = action.time(self.timeout) + self.timeout;
        tokio::time::timeout(time, async {
            let envelope = Envelope {
                accounts: action,
                secret_present: key.is_some(),
            };
            let frame = encode_frame(&envelope).map_err(|_| failure("accounts_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("accounts_owner_unavailable"))?;
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
                .map_err(|_| failure("accounts_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("accounts_owner_unavailable"))?;
            if let Some(key) = key {
                let mut bytes = key.into_bytes();
                let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
                framed.extend_from_slice(&bytes);
                bytes.fill(0);
                let sent = stream.write_all(&framed).await;
                framed.fill(0);
                sent.map_err(|_| failure("accounts_owner_unavailable"))?;
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

/// Serves one accounts owner request after the owner exchange deadline: the action runs
/// under its own budget, which covers its network probes, and the reply write under the
/// owner wait. Inputs: the stream, engine, authority, owner packet, admission and request
/// bytes. Output: the reply written, a rejection code included.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| failure("accounts_request_invalid"))?;
    let key = if envelope.secret_present {
        Some(receive_key(stream).await?)
    } else {
        None
    };
    let time = envelope.accounts.time(engine.timeout);
    let outcome = tokio::time::timeout(
        time,
        handle(engine, authority, packet, admission, envelope.accounts, key),
    )
    .await
    .unwrap_or_else(|_| Err(unknown()));
    let reply = match outcome {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected {
            code: public_code(error),
        },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "accounts_reply_bounds".into(),
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
    action: Action,
    key: Option<AccountSecret>,
) -> Result<Reply, EngineError> {
    authenticate(engine, authority, packet, admission).await?;
    if key.is_some() && !matches!(action, Action::Add { .. } | Action::ReplaceKey { .. }) {
        return Err(failure("accounts_key_unexpected"));
    }
    let root = &authority.root;
    Ok(match action {
        Action::Catalog => Reply::Catalog {
            providers: crate::accounts::catalog(),
        },
        Action::List { after, limit } => {
            let ResultValue::Page(items, next) =
                actor(engine, admission, Command::List(after, limit.into())).await?
            else {
                return Err(unknown());
            };
            let mut accounts = views(root, items).await?;
            let mut next = next;
            while encode_frame(&Reply::Page {
                page: AccountPage {
                    accounts: accounts.clone(),
                    next: next.clone(),
                },
            })
            .is_err()
            {
                accounts
                    .pop()
                    .ok_or_else(|| failure("accounts_reply_bounds"))?;
                next = accounts.last().map(|account| account.id.clone());
            }
            Reply::Page {
                page: AccountPage { accounts, next },
            }
        }
        Action::Add { draft } => {
            let nonce = crate::engine::owner::random_id()?;
            let (connection, details) = draft.build(key.is_some(), &nonce)?;
            let credential = match &connection.auth {
                ModelAuthentication::ApiKey { credential_ref } => Some(credential_ref.clone()),
                _ => None,
            };
            if let (Some(credential), Some(key)) = (&credential, key) {
                store_key(root, credential, key)?;
            }
            let keys = authority.clone();
            let added = engine
                .settled_job(
                    |reply| {
                        Job::Accounts(
                            admission.clone(),
                            Command::Add(Box::new(connection), details),
                            reply,
                        )
                    },
                    move |result| {
                        if let (Err(_), Some(credential)) = (result, &credential) {
                            let _ = remove_key(&keys.root, credential);
                        }
                        Ok(())
                    },
                )
                .await?;
            let ResultValue::Account(added) = added else {
                return Err(unknown());
            };
            Reply::Account {
                account: views(root, vec![*added]).await?.remove(0),
            }
        }
        Action::Rename { id, name } => {
            let ResultValue::Account(listed) =
                actor(engine, admission, Command::Rename(id, name)).await?
            else {
                return Err(unknown());
            };
            Reply::Account {
                account: views(root, vec![*listed]).await?.remove(0),
            }
        }
        Action::Remove { id } => {
            let ResultValue::Removed(connection) =
                actor(engine, admission, Command::Remove(id)).await?
            else {
                return Err(unknown());
            };
            if let ModelAuthentication::ApiKey { credential_ref } = &connection.auth {
                remove_key(root, credential_ref)?;
            }
            Reply::Removed
        }
        Action::ReplaceKey { id } => {
            let key = key.ok_or_else(|| failure("account_key_required"))?;
            let listed = match actor(engine, admission, Command::Get(id.clone())).await {
                Ok(ResultValue::Account(listed)) => listed,
                refused => {
                    key.into_bytes().fill(0);
                    return Err(refused.err().unwrap_or_else(unknown));
                }
            };
            let ModelAuthentication::ApiKey { credential_ref } = &listed.connection.auth else {
                key.into_bytes().fill(0);
                return Err(failure("account_key_not_allowed"));
            };
            replace_key(root, credential_ref, key)?;
            let listed = match actor(engine, admission, Command::KeyReplaced(id)).await {
                Ok(ResultValue::Account(current)) => current,
                gone => {
                    remove_key(root, credential_ref)?;
                    return Err(gone.err().unwrap_or_else(unknown));
                }
            };
            Reply::Account {
                account: views(root, vec![*listed]).await?.remove(0),
            }
        }
        Action::Check { id } => {
            let ResultValue::Account(listed) = actor(engine, admission, Command::Get(id)).await?
            else {
                return Err(unknown());
            };
            let (account, models) = check(root, *listed).await?;
            Reply::Checked {
                check: AccountCheck { account, models },
            }
        }
        Action::LocalModels { url } => {
            let url = crate::accounts::local_url(&url)?;
            Reply::Models {
                models: probe::local_models(&url, CHECK_PROBE).await?,
            }
        }
    })
}

async fn actor(
    engine: &Engine,
    admission: &Admission,
    command: Command,
) -> Result<ResultValue, EngineError> {
    engine
        .native_auth_job(|reply| Job::Accounts(admission.clone(), command, reply))
        .await
}

async fn authenticate(
    engine: &Engine,
    authority: &Authority,
    packet: &[u8; 37],
    admission: &Admission,
) -> Result<(), EngineError> {
    let _barrier = engine.owner_egress.write().await;
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    Ok(())
}

async fn receive_key(stream: &mut UnixStream) -> Result<AccountSecret, EngineError> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let invalid = || failure("accounts_key_frame_invalid");
        let size = stream.read_u32().await.map_err(|_| invalid())? as usize;
        if size == 0 || size > SECRET_MAX {
            return Err(invalid());
        }
        let mut bytes = vec![0; size];
        if stream.read_exact(&mut bytes).await.is_err() {
            bytes.fill(0);
            return Err(invalid());
        }
        AccountSecret::new(bytes).map_err(|_| failure("account_key_invalid"))
    })
    .await
    .map_err(|_| failure("accounts_key_frame_invalid"))?
}

fn public_code(error: EngineError) -> String {
    if error.0.len() <= 96
        && error
            .0
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        error.0
    } else {
        "accounts_outcome_unknown".into()
    }
}

fn unknown() -> EngineError {
    failure("accounts_outcome_unknown")
}

#[cfg(unix)]
fn key_name(credential: &CredentialRef) -> Result<&str, EngineError> {
    let name = credential.as_str();
    if name.starts_with('.') {
        return Err(failure("account_key_reference_invalid"));
    }
    Ok(name)
}

#[cfg(unix)]
fn keys_directory(root: &Root, create: bool) -> Result<Option<File>, EngineError> {
    root.verify()?;
    let anchor = Root::open(
        root.path
            .parent()
            .ok_or_else(|| failure("account_key_store_unavailable"))?,
    )?;
    if create {
        match rustix::fs::mkdirat(&anchor.directory, KEYS, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Err(failure("account_key_store_unavailable")),
        }
    }
    let directory = match rustix::fs::openat(
        &anchor.directory,
        KEYS,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => File::from(fd),
        Err(rustix::io::Errno::NOENT) if !create => return Ok(None),
        Err(_) => return Err(failure("account_key_store_unavailable")),
    };
    let metadata = directory
        .metadata()
        .map_err(|_| failure("account_key_store_unavailable"))?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(failure("account_key_store_unsafe"));
    }
    Ok(Some(directory))
}

#[cfg(unix)]
fn store_key(
    root: &Root,
    credential: &CredentialRef,
    key: AccountSecret,
) -> Result<(), EngineError> {
    let directory =
        keys_directory(root, true)?.ok_or_else(|| failure("account_key_store_unavailable"))?;
    write_key(&directory, key_name(credential)?, key)?;
    directory
        .sync_all()
        .map_err(|_| failure("account_key_store_unavailable"))?;
    Ok(())
}

/// Writes a key into a new owner-only file in the key directory, removing the file again
/// when the write fails. Inputs: the key directory, a file name that must not exist yet,
/// and the key, which is zeroed after the write. Output: the file is synced to disk.
#[cfg(unix)]
fn write_key(directory: &File, name: &str, key: AccountSecret) -> Result<(), EngineError> {
    let fd: OwnedFd = rustix::fs::openat(
        directory,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| failure("account_key_store_unavailable"))?;
    let mut file = File::from(fd);
    let mut bytes = key.into_bytes();
    let written = std::io::Write::write_all(&mut file, &bytes).and_then(|()| file.sync_all());
    bytes.fill(0);
    if written.is_err() {
        let _ = rustix::fs::unlinkat(directory, name, rustix::fs::AtFlags::empty());
        return Err(failure("account_key_store_unavailable"));
    }
    Ok(())
}

/// Swaps the saved key of one credential for a new one: the new key is written to a
/// staged file, then renamed over the old file, so the old key is unlinked and the file
/// never holds a partial key. Inputs: owner root, credential reference and the new key.
/// Output: the key directory synced; on failure the old key stays and nothing is staged.
#[cfg(unix)]
fn replace_key(
    root: &Root,
    credential: &CredentialRef,
    key: AccountSecret,
) -> Result<(), EngineError> {
    let directory =
        keys_directory(root, true)?.ok_or_else(|| failure("account_key_store_unavailable"))?;
    let name = key_name(credential)?;
    let staged = format!(".replace-{}", crate::engine::owner::random_id()?);
    write_key(&directory, &staged, key)?;
    if rustix::fs::renameat(&directory, staged.as_str(), &directory, name).is_err() {
        let _ = rustix::fs::unlinkat(&directory, staged.as_str(), rustix::fs::AtFlags::empty());
        return Err(failure("account_key_store_unavailable"));
    }
    directory
        .sync_all()
        .map_err(|_| failure("account_key_store_unavailable"))?;
    Ok(())
}

/// Refuses to save an account key where the Unix owner-only key directory does not exist.
/// Takes the owner root and credential, unused, and the key, which is zeroed before it drops.
/// Returns `account_key_store_unavailable`.
#[cfg(not(unix))]
fn store_key(
    _root: &Root,
    _credential: &CredentialRef,
    key: AccountSecret,
) -> Result<(), EngineError> {
    let mut bytes = key.into_bytes();
    bytes.fill(0);
    Err(failure("account_key_store_unavailable"))
}

#[cfg(unix)]
fn open_key(root: &Root, credential: &CredentialRef) -> Result<Option<File>, EngineError> {
    let Some(directory) = keys_directory(root, false)? else {
        return Ok(None);
    };
    let file = match rustix::fs::openat(
        &directory,
        key_name(credential)?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => File::from(fd),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(failure("account_key_store_unavailable")),
    };
    let metadata = file
        .metadata()
        .map_err(|_| failure("account_key_store_unavailable"))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.len() as usize > SECRET_MAX
    {
        return Err(failure("account_key_store_unsafe"));
    }
    Ok(Some(file))
}

/// Refuses to replace an account key where the Unix owner-only key directory does not exist.
/// Takes the owner root and credential, unused, and the key, which is zeroed before it drops.
/// Returns `account_key_store_unavailable`.
#[cfg(not(unix))]
fn replace_key(
    _root: &Root,
    _credential: &CredentialRef,
    key: AccountSecret,
) -> Result<(), EngineError> {
    key.into_bytes().fill(0);
    Err(failure("account_key_store_unavailable"))
}

/// Refuses to open an account key where the Unix owner-only key directory does not exist.
/// Takes the owner root and credential, both unused. Returns `account_key_store_unavailable`.
#[cfg(not(unix))]
fn open_key(_root: &Root, _credential: &CredentialRef) -> Result<Option<File>, EngineError> {
    Err(failure("account_key_store_unavailable"))
}

fn read_key(root: &Root, credential: &CredentialRef) -> Result<Option<Vec<u8>>, EngineError> {
    let Some(mut file) = open_key(root, credential)? else {
        return Ok(None);
    };
    let mut bytes = Vec::with_capacity(SECRET_MAX);
    if (&mut file)
        .take(SECRET_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > SECRET_MAX
    {
        bytes.fill(0);
        return Err(failure("account_key_store_unavailable"));
    }
    Ok(Some(bytes))
}

/// Reads the key saved for one account so the engine can hand it to a harness writer over
/// a pipe. Input: owner root and the account's credential reference. Output: the key, or
/// none when the account has no saved key.
pub(super) fn saved_key(
    root: &Root,
    credential: &CredentialRef,
) -> Result<Option<AccountSecret>, EngineError> {
    let Some(bytes) = read_key(root, credential)? else {
        return Ok(None);
    };
    AccountSecret::new(bytes)
        .map(Some)
        .map_err(|_| failure("account_key_store_unsafe"))
}

#[cfg(unix)]
fn remove_key(root: &Root, credential: &CredentialRef) -> Result<(), EngineError> {
    let Some(directory) = keys_directory(root, false)? else {
        return Ok(());
    };
    match rustix::fs::unlinkat(
        &directory,
        key_name(credential)?,
        rustix::fs::AtFlags::empty(),
    ) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(_) => Err(failure("account_key_store_unavailable")),
    }
}

/// Refuses to remove an account key where the Unix owner-only key directory does not exist.
/// Takes the owner root and credential, both unused. Returns `account_key_store_unavailable`.
#[cfg(not(unix))]
fn remove_key(_root: &Root, _credential: &CredentialRef) -> Result<(), EngineError> {
    Err(failure("account_key_store_unavailable"))
}

fn http_url(connection: &ModelConnection) -> Option<url::Url> {
    match &connection.endpoint {
        ModelEndpoint::Http { url } => url::Url::parse(url).ok(),
        ModelEndpoint::Native {} => None,
    }
}

fn entered_url(listed: &Listed) -> bool {
    crate::accounts::catalog()
        .into_iter()
        .find(|entry| entry.provider == listed.details.provider)
        .and_then(|entry| {
            entry.methods.into_iter().find(|method| {
                method.method
                    == crate::accounts::method_of(&listed.connection, listed.details.provider)
            })
        })
        .is_some_and(|method| method.url != Field::Hidden)
}

fn base_status(root: &Root, listed: &Listed) -> Result<AccountStatus, EngineError> {
    Ok(match &listed.connection.auth {
        ModelAuthentication::NativeManaged { .. } | ModelAuthentication::Oauth { .. } => {
            match listed.sign_in {
                Some(NativeAuthPhase::AuthenticatedObserved) if listed.launchable => {
                    AccountStatus::SignedIn
                }
                Some(
                    NativeAuthPhase::Starting
                    | NativeAuthPhase::AwaitingOwner
                    | NativeAuthPhase::CheckingAccount
                    | NativeAuthPhase::Cancelling
                    | NativeAuthPhase::Uncertain,
                ) => AccountStatus::SigningIn,
                _ => AccountStatus::NotSignedIn,
            }
        }
        ModelAuthentication::ApiKey { credential_ref } => {
            if !listed.rejected && open_key(root, credential_ref)?.is_some() {
                AccountStatus::KeySaved
            } else {
                AccountStatus::NotSignedIn
            }
        }
        ModelAuthentication::None {} if listed.rejected => AccountStatus::NotSignedIn,
        ModelAuthentication::None {} => AccountStatus::SignedIn,
    })
}

fn view(listed: &Listed, status: AccountStatus) -> AccountView {
    let provider = listed.details.provider;
    let method = crate::accounts::method_of(&listed.connection, provider);
    let label = crate::accounts::catalog()
        .into_iter()
        .find(|entry| entry.provider == provider)
        .and_then(|entry| {
            entry
                .methods
                .into_iter()
                .find(|entry| entry.method == method)
        })
        .map(|entry| entry.label)
        .unwrap_or_default();
    let native = matches!(listed.connection.endpoint, ModelEndpoint::Native {});
    AccountView {
        id: listed.connection.id.clone(),
        name: listed.details.name.clone(),
        provider,
        provider_name: crate::accounts::provider_name(provider).into(),
        method,
        method_label: label,
        url: match &listed.connection.endpoint {
            ModelEndpoint::Http { url } => Some(url.clone()),
            ModelEndpoint::Native {} => None,
        },
        model: (!native).then(|| listed.connection.model.id.clone()),
        status,
        status_label: if status == AccountStatus::NotSignedIn && listed.rejected {
            "Sign-in rejected"
        } else {
            status.label()
        }
        .into(),
    }
}

async fn views(root: &Root, items: Vec<Listed>) -> Result<Vec<AccountView>, EngineError> {
    let mut probes: BTreeMap<String, Option<bool>> = BTreeMap::new();
    let mut bases = Vec::with_capacity(items.len());
    for listed in &items {
        let base = base_status(root, listed)?;
        if base != AccountStatus::NotSignedIn
            && entered_url(listed)
            && let Some(url) = http_url(&listed.connection)
        {
            probes
                .entry(url.origin().ascii_serialization())
                .or_insert(None);
        }
        bases.push(base);
    }
    let mut tasks = tokio::task::JoinSet::new();
    for origin in probes.keys().cloned() {
        tasks.spawn(async move {
            let reached = match url::Url::parse(&origin) {
                Ok(url) => probe::reachable(&url, LIST_PROBE).await,
                Err(_) => false,
            };
            (origin, reached)
        });
    }
    while let Some(result) = tasks.join_next().await {
        let (origin, reached) = result.map_err(|_| unknown())?;
        probes.insert(origin, Some(reached));
    }
    let views = items
        .iter()
        .zip(bases)
        .map(|(listed, base)| {
            let reached = http_url(&listed.connection).and_then(|url| {
                probes
                    .get(&url.origin().ascii_serialization())
                    .copied()
                    .flatten()
            });
            let status = if reached == Some(false) {
                AccountStatus::Unreachable
            } else {
                base
            };
            view(listed, status)
        })
        .collect::<Vec<_>>();
    assert_eq!(views.len(), items.len());
    Ok(views)
}

async fn check(root: &Root, listed: Listed) -> Result<(AccountView, Vec<String>), EngineError> {
    let base = base_status(root, &listed)?;
    let Some(url) = http_url(&listed.connection) else {
        return Ok((view(&listed, base), Vec::new()));
    };
    crate::accounts::account_url(url.as_str())?;
    let key = match &listed.connection.auth {
        ModelAuthentication::ApiKey { credential_ref } => read_key(root, credential_ref)?,
        _ => None,
    };
    if url.scheme() != "http" {
        let status = if probe::reachable(&url, CHECK_PROBE).await {
            base
        } else {
            AccountStatus::Unreachable
        };
        return Ok((view(&listed, status), Vec::new()));
    }
    let checked = probe::check(&url, key.as_deref(), CHECK_PROBE).await;
    let mut key = key;
    if let Some(bytes) = key.as_mut() {
        bytes.fill(0);
    }
    Ok(match checked {
        Err(_) => (view(&listed, AccountStatus::Unreachable), Vec::new()),
        Ok((401 | 403, _)) => (view(&listed, AccountStatus::NotSignedIn), Vec::new()),
        Ok((_, models)) => (view(&listed, base), models),
    })
}
