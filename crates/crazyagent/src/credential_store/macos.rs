use super::{SECRET_BYTES_MAX, SecretString, Store, StoreError, checked_name, parts, reference};
use crate::protocol::CredentialRef;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SECURITY: &str = "/usr/bin/security";
const STEP_TIME: Duration = Duration::from_secs(10);
const OUTPUT_BYTES_MAX: usize = 4096;
const ITEM_MISSING: i32 = 44;
const NAMESPACE_ROOT: &str = "dev.crazyagent.";

/// The macOS Keychain as a credential store. Every item lives in the owner's default
/// keychain under services that start with this store's namespace, and every call names
/// that keychain file, so lookups never depend on `HOME`. Items are written through
/// `security -i` on stdin as hex, so a secret never appears in any process argument.
pub struct Keychain {
    namespace: String,
    keychain: PathBuf,
}

struct Ran {
    code: Option<i32>,
    stdout: Vec<u8>,
}

impl Keychain {
    /// Opens the store for one namespace under `dev.crazyagent.`. Input: the namespace,
    /// such as `dev.crazyagent.e1a2b3`. Output: the store bound to the owner's default
    /// keychain file, or `credential_namespace_refused` / `credential_store_unavailable`.
    pub fn new(namespace: &str) -> Result<Self, StoreError> {
        let namespace = checked_name(namespace)?;
        if !namespace.starts_with(NAMESPACE_ROOT) || namespace.len() == NAMESPACE_ROOT.len() {
            return Err(StoreError::code("credential_namespace_refused"));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or_else(unavailable)?;
        let ran = run(
            &["default-keychain".as_ref(), "-d".as_ref(), "user".as_ref()],
            None,
            Some(&home),
        )?;
        if ran.code != Some(0) {
            return Err(unavailable());
        }
        let text = String::from_utf8(ran.stdout).map_err(|_| unavailable())?;
        let keychain = PathBuf::from(text.trim().trim_matches('"'));
        let shown = keychain.to_str().ok_or_else(unavailable)?;
        if !keychain.is_absolute()
            || shown.contains(['\'', '"', '\\', '\n', '\r'])
            || !keychain.is_file()
        {
            return Err(unavailable());
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            keychain,
        })
    }

    fn owned<'a>(&self, service: &'a str) -> Result<&'a str, StoreError> {
        let service = checked_name(service)?;
        let inside = service
            .strip_prefix(&self.namespace)
            .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1);
        if !inside {
            return Err(StoreError::code("credential_namespace_refused"));
        }
        Ok(service)
    }

    fn owned_parts<'a>(
        &self,
        reference: &'a CredentialRef,
    ) -> Result<(&'a str, &'a str), StoreError> {
        let (service, account) = parts(reference)?;
        Ok((self.owned(service)?, account))
    }

    fn keychain_text(&self) -> &str {
        self.keychain
            .to_str()
            .expect("keychain path was checked as text")
    }
}

impl Store for Keychain {
    fn put(
        &self,
        service: &str,
        account: &str,
        secret: &SecretString,
    ) -> Result<CredentialRef, StoreError> {
        let service = self.owned(service)?;
        let account = checked_name(account)?;
        let found = reference(service, account)?;
        let mut line = format!(
            "add-generic-password -U -s \"{service}\" -a \"{account}\" -X {} \"{}\"\n",
            hex(secret.expose().as_bytes()),
            self.keychain_text()
        )
        .into_bytes();
        let ran = run(&["-i".as_ref()], Some(&line), None);
        line.fill(0);
        if ran?.code != Some(0) {
            return Err(unavailable());
        }
        Ok(found)
    }

    fn get(&self, reference: &CredentialRef) -> Result<SecretString, StoreError> {
        let (service, account) = self.owned_parts(reference)?;
        let mut ran = run(
            &[
                "find-generic-password".as_ref(),
                "-s".as_ref(),
                service.as_ref(),
                "-a".as_ref(),
                account.as_ref(),
                "-w".as_ref(),
                self.keychain.as_os_str(),
            ],
            None,
            None,
        )?;
        let found = match ran.code {
            Some(0) => {
                if ran.stdout.last() == Some(&b'\n') {
                    ran.stdout.pop();
                }
                SecretString::new(std::mem::take(&mut ran.stdout))
            }
            Some(ITEM_MISSING) => Err(StoreError::code("credential_missing")),
            _ => Err(unavailable()),
        };
        ran.stdout.fill(0);
        found
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), StoreError> {
        let (service, account) = self.owned_parts(reference)?;
        let ran = run(
            &[
                "delete-generic-password".as_ref(),
                "-s".as_ref(),
                service.as_ref(),
                "-a".as_ref(),
                account.as_ref(),
                self.keychain.as_os_str(),
            ],
            None,
            None,
        )?;
        match ran.code {
            Some(0 | ITEM_MISSING) => Ok(()),
            _ => Err(unavailable()),
        }
    }

    fn git_credential_helper(
        &self,
        reference: &CredentialRef,
        username: &str,
    ) -> Result<String, StoreError> {
        let (service, account) = self.owned_parts(reference)?;
        if username.is_empty()
            || username.len() > 64
            || !username
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        {
            return Err(StoreError::code("credential_name_invalid"));
        }
        Ok(format!(
            "!f() {{ test \"$1\" = get || exit 0; t=$({SECURITY} find-generic-password -s '{service}' -a '{account}' -w '{}') || exit 1; printf 'username={username}\\npassword=%s\\n' \"$t\"; }}; f",
            self.keychain_text()
        ))
    }
}

fn unavailable() -> StoreError {
    StoreError::code("credential_store_unavailable")
}

fn hex(bytes: &[u8]) -> String {
    assert!(bytes.len() <= SECRET_BYTES_MAX);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn run(
    args: &[&std::ffi::OsStr],
    input: Option<&[u8]>,
    home: Option<&Path>,
) -> Result<Ran, StoreError> {
    let mut command = Command::new(SECURITY);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = home {
        command.env("HOME", home);
    }
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(|_| unavailable())?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().ok_or_else(unavailable)?;
        let written = stdin.write_all(input);
        drop(stdin);
        if written.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable());
        }
    }
    let stdout = child.stdout.take().ok_or_else(unavailable)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout
            .take(OUTPUT_BYTES_MAX as u64 + 1)
            .read_to_end(&mut bytes);
        (read, bytes)
    });
    let deadline = Instant::now() + STEP_TIME;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| unavailable())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(unavailable());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let (read, mut stdout) = reader.join().map_err(|_| unavailable())?;
    if read.is_err() || stdout.len() > OUTPUT_BYTES_MAX {
        stdout.fill(0);
        return Err(unavailable());
    }
    Ok(Ran {
        code: status.code(),
        stdout,
    })
}

#[cfg(test)]
#[path = "macos_tests.rs"]
mod tests;
