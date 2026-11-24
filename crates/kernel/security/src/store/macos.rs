use super::{SecretRef, Store, checked_name, parts, reference};
use crate::error::StoreError;
use crate::secret::Piece::{Hex, Plain};
use crate::secret::sized;
use crate::secret::{STORED_BYTES_MAX, SecretString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SECURITY: &str = "/usr/bin/security";
const STEP_TIME: Duration = Duration::from_secs(10);
const OUTPUT_BYTES_MAX: usize = 4096;
const ITEM_MISSING: i32 = 44;
const NAMESPACE_ROOT: &str = "dev.crazyagent.";
/// The most bytes of the keychain file path a store names.
const KEYCHAIN_PATH_BYTES_MAX: usize = 512;
/// The longest line `security -i` takes on stdin.
const SECURITY_LINE_MAX: usize = 4_096;
/// The longest `add-generic-password` line `put` writes: the fixed words, a service and
/// account of 63 bytes each, the item as hex, and the keychain path.
const PUT_LINE_MAX: usize =
    28 + 63 + 6 + 63 + 5 + 2 * STORED_BYTES_MAX + 2 + KEYCHAIN_PATH_BYTES_MAX + 2;

const _: () = assert!(PUT_LINE_MAX <= SECURITY_LINE_MAX);

/// The macOS Keychain as a credential store. Every item lives in one keychain file, the
/// owner's default unless the store was opened with `at`, under services that start with
/// this store's namespace, and every call names that file, so lookups never depend on
/// `HOME`. Items are written through `security -i` on stdin as hex, so a secret never
/// appears in any process argument.
pub struct Keychain {
    namespace: String,
    keychain: PathBuf,
}

struct Ran {
    code: Option<i32>,
    stdout: Vec<u8>,
}

impl Keychain {
    /// Opens the store for one namespace in the default keychain of `home`, read with
    /// `security default-keychain -d user` and only `HOME` set to it. Inputs: the namespace,
    /// such as `dev.crazyagent.e1a2b3`, and an absolute home. Output: the store, or
    /// `credential_namespace_refused` / `credential_store_unavailable`.
    pub fn in_home(namespace: &str, home: &Path) -> Result<Self, StoreError> {
        let namespace = owned_namespace(namespace)?;
        if !home.is_absolute() {
            return Err(unavailable());
        }
        let ran = run(
            &["default-keychain".as_ref(), "-d".as_ref(), "user".as_ref()],
            None,
            Some(home),
        )?;
        if ran.code != Some(0) {
            return Err(unavailable());
        }
        let text = String::from_utf8(ran.stdout).map_err(|_| unavailable())?;
        Self::at(namespace, Path::new(text.trim().trim_matches('"')))
    }

    /// Opens the store for one namespace in one named keychain file, such as a test's own
    /// temporary keychain. Inputs: the namespace and the absolute keychain file. Output: the
    /// store bound to that file, or `credential_namespace_refused` or
    /// `credential_store_unavailable`.
    pub fn at(namespace: &str, keychain: &Path) -> Result<Self, StoreError> {
        let namespace = owned_namespace(namespace)?;
        let shown = keychain.to_str().ok_or_else(unavailable)?;
        if !keychain.is_absolute()
            || shown.len() > KEYCHAIN_PATH_BYTES_MAX
            || shown.contains(['\'', '"', '\\', '\n', '\r'])
            || !keychain.is_file()
        {
            return Err(unavailable());
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            keychain: keychain.to_path_buf(),
        })
    }

    /// The keychain file this store reads and writes. Output: the absolute path.
    pub fn path(&self) -> &Path {
        &self.keychain
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

    fn owned_parts<'a>(&self, reference: &'a SecretRef) -> Result<(&'a str, &'a str), StoreError> {
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
    ) -> Result<SecretRef, StoreError> {
        if secret.expose().len() > STORED_BYTES_MAX {
            return Err(StoreError::code("credential_secret_invalid"));
        }
        let service = self.owned(service)?;
        let account = checked_name(account)?;
        let found = reference(service, account)?;
        let mut line = sized(&[
            Plain("add-generic-password -U -s \""),
            Plain(service),
            Plain("\" -a \""),
            Plain(account),
            Plain("\" -X "),
            Hex(secret.expose().as_bytes()),
            Plain(" \""),
            Plain(self.keychain_text()),
            Plain("\"\n"),
        ])
        .ok_or_else(|| StoreError::code("credential_secret_invalid"))?;
        assert!(line.len() <= PUT_LINE_MAX);
        let ran = run(&["-i".as_ref()], Some(&line), None);
        crate::secret::wipe(&mut line);
        if ran?.code != Some(0) {
            return Err(unavailable());
        }
        Ok(found)
    }

    fn get(&self, reference: &SecretRef) -> Result<SecretString, StoreError> {
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
                SecretString::stored(std::mem::take(&mut ran.stdout))
            }
            Some(ITEM_MISSING) => Err(StoreError::code("credential_missing")),
            _ => Err(unavailable()),
        };
        crate::secret::wipe(&mut ran.stdout);
        found
    }

    fn delete(&self, reference: &SecretRef) -> Result<(), StoreError> {
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
        reference: &SecretRef,
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

/// Checks one engine namespace: a store name under `dev.crazyagent.` with a leaf after it.
/// Input: the namespace. Output: it, or `credential_name_invalid` /
/// `credential_namespace_refused`.
pub fn owned_namespace(namespace: &str) -> Result<&str, StoreError> {
    let namespace = checked_name(namespace)?;
    if !namespace.starts_with(NAMESPACE_ROOT) || namespace.len() == NAMESPACE_ROOT.len() {
        return Err(StoreError::code("credential_namespace_refused"));
    }
    Ok(namespace)
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
    let mut child = kernel_processes::spawn_gate::spawn(&mut command).map_err(|_| unavailable())?;
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
    let reader =
        std::thread::spawn(move || crate::read_limited::read_limited(stdout, OUTPUT_BYTES_MAX));
    let deadline = Instant::now() + STEP_TIME;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| unavailable())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            if let Ok((_, mut bytes)) = reader.join() {
                crate::secret::wipe(&mut bytes);
            }
            return Err(unavailable());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let (read, mut stdout) = reader.join().map_err(|_| unavailable())?;
    if read.is_err() || stdout.len() > OUTPUT_BYTES_MAX {
        crate::secret::wipe(&mut stdout);
        return Err(unavailable());
    }
    Ok(Ran {
        code: status.code(),
        stdout,
    })
}
