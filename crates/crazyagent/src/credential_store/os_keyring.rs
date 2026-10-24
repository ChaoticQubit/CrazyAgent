use super::{SecretString, Store, StoreError, checked_name, parts, platform_store, reference};
use crate::protocol::CredentialRef;
use keyring_core::{CredentialStore, Entry};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

pub const GIT_HELPER_COMMAND: &str = "git-credential";
const NAMESPACE_ROOT: &str = "dev.crazyagent.";
const GIT_REQUEST_BYTES_MAX: u64 = 64 * 1024;
const USERNAME_BYTES_MAX: usize = 64;

pub(super) type Connect = fn() -> keyring_core::Result<Arc<CredentialStore>>;
pub(super) type EntryFor = fn(&CredentialStore, &str, &str) -> keyring_core::Result<Entry>;

/// The Linux Secret Service or Windows Credential Manager as a credential store, through
/// the keyring-core store crates. Only services under this store's `dev.crazyagent.`
/// namespace are read, written or deleted. Its git helper runs the engine executable's
/// `git-credential` command, which reads the item itself, so no secret leaves the store
/// through an argument, an environment variable or a git config file.
pub struct OsKeyring {
    namespace: String,
    executable: PathBuf,
    store: Arc<CredentialStore>,
    entry_for: EntryFor,
    step_time: Option<Duration>,
}

impl OsKeyring {
    /// Checks the namespace and connects to the platform store under the step limit.
    /// Inputs: namespace, helper executable, the platform's connect and entry builders,
    /// and the step limit (None runs calls on the caller's thread). Output: the store, or
    /// `credential_namespace_refused` / `credential_store_unavailable`.
    pub(super) fn new(
        namespace: &str,
        executable: &Path,
        connect: Connect,
        entry_for: EntryFor,
        step_time: Option<Duration>,
    ) -> Result<Self, StoreError> {
        let namespace = checked_namespace(namespace)?;
        let store = bounded(step_time, move || connect().map_err(|_| unavailable()))?;
        Ok(Self {
            namespace: namespace.to_owned(),
            executable: executable.to_path_buf(),
            store,
            entry_for,
            step_time,
        })
    }

    fn entry(&self, service: &str, account: &str) -> Result<Entry, StoreError> {
        (self.entry_for)(&*self.store, service, account).map_err(failure)
    }

    fn owned_entry(&self, reference: &CredentialRef) -> Result<Entry, StoreError> {
        let (service, account) = parts(reference)?;
        self.entry(owned_service(&self.namespace, service)?, account)
    }
}

impl Store for OsKeyring {
    fn put(
        &self,
        service: &str,
        account: &str,
        secret: &SecretString,
    ) -> Result<CredentialRef, StoreError> {
        let service = owned_service(&self.namespace, service)?;
        let account = checked_name(account)?;
        let found = reference(service, account)?;
        let entry = self.entry(service, account)?;
        let copy = SecretString::new(secret.expose().as_bytes().to_vec())?;
        bounded(self.step_time, move || {
            entry.set_secret(copy.expose().as_bytes()).map_err(failure)
        })?;
        Ok(found)
    }

    fn get(&self, reference: &CredentialRef) -> Result<SecretString, StoreError> {
        let entry = self.owned_entry(reference)?;
        bounded(self.step_time, move || {
            SecretString::new(entry.get_secret().map_err(failure)?)
        })
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), StoreError> {
        let entry = self.owned_entry(reference)?;
        bounded(self.step_time, move || match entry.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(error) => Err(failure(error)),
        })
    }

    fn git_credential_helper(
        &self,
        reference: &CredentialRef,
        username: &str,
    ) -> Result<String, StoreError> {
        let (service, _) = parts(reference)?;
        owned_service(&self.namespace, service)?;
        let username = checked_username(username)?;
        let shown = self
            .executable
            .to_str()
            .filter(|text| {
                self.executable.is_absolute() && !text.chars().any(|c| c == '\'' || c.is_control())
            })
            .ok_or_else(unavailable)?;
        let executable = if cfg!(target_os = "windows") {
            shown.replace('\\', "/")
        } else {
            shown.to_owned()
        };
        Ok(format!(
            "!'{executable}' {GIT_HELPER_COMMAND} '{}' '{}' '{username}'",
            self.namespace,
            reference.as_str()
        ))
    }
}

/// Answers one git credential helper call made through `crazyagent git-credential`.
/// Inputs: namespace, reference and user name from the helper value, git's operation,
/// git's request and the answer stream. Output: `username` and `password` lines for `get`;
/// nothing for `store` or `erase`, so git never replaces or deletes the engine's item.
pub fn answer_git_credential(
    namespace: &str,
    reference: &str,
    username: &str,
    operation: &str,
    input: impl Read,
    mut output: impl Write,
) -> Result<(), StoreError> {
    let username = checked_username(username)?;
    let reference = CredentialRef::new(reference)
        .map_err(|_| StoreError::code("credential_reference_invalid"))?;
    parts(&reference)?;
    let mut request = Vec::new();
    let read = input.take(GIT_REQUEST_BYTES_MAX).read_to_end(&mut request);
    request.fill(0);
    read.map_err(|_| StoreError::code("credential_helper_io_failed"))?;
    if operation != "get" {
        return Ok(());
    }
    let secret = platform_store(namespace)?.get(&reference)?;
    let mut answer = format!("username={username}\npassword={}\n", secret.expose()).into_bytes();
    let written = output.write_all(&answer).and_then(|()| output.flush());
    answer.fill(0);
    written.map_err(|_| StoreError::code("credential_helper_io_failed"))
}

fn checked_namespace(namespace: &str) -> Result<&str, StoreError> {
    let namespace = checked_name(namespace)?;
    if !namespace.starts_with(NAMESPACE_ROOT) || namespace.len() == NAMESPACE_ROOT.len() {
        return Err(StoreError::code("credential_namespace_refused"));
    }
    Ok(namespace)
}

fn owned_service<'a>(namespace: &str, service: &'a str) -> Result<&'a str, StoreError> {
    let service = checked_name(service)?;
    let inside = service
        .strip_prefix(namespace)
        .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1);
    if !inside {
        return Err(StoreError::code("credential_namespace_refused"));
    }
    Ok(service)
}

fn checked_username(username: &str) -> Result<&str, StoreError> {
    if username.is_empty()
        || username.len() > USERNAME_BYTES_MAX
        || !username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return Err(StoreError::code("credential_name_invalid"));
    }
    Ok(username)
}

fn bounded<T: Send + 'static>(
    step_time: Option<Duration>,
    work: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    let Some(step_time) = step_time else {
        return work();
    };
    let (sender, receiver) = sync_channel(1);
    std::thread::Builder::new()
        .name("crazyagent-credential-store".to_owned())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .map_err(|_| unavailable())?;
    receiver
        .recv_timeout(step_time)
        .map_err(|_| unavailable())?
}

fn failure(error: keyring_core::Error) -> StoreError {
    match error {
        keyring_core::Error::NoEntry => StoreError::code("credential_missing"),
        keyring_core::Error::Ambiguous(_) => StoreError::code("credential_ambiguous"),
        _ => unavailable(),
    }
}

fn unavailable() -> StoreError {
    StoreError::code("credential_store_unavailable")
}
