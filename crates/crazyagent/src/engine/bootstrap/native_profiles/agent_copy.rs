use super::{NativeProfileUse, failure};
use crate::engine::EngineError;
use base64::Engine;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde_json::{Map, Value, json};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CODEX_RECORD: &str = "auth.json";
const CLAUDE_RECORD: &str = ".credentials.json";
const RECORD_BYTES_MAX: u64 = 65_536;
#[cfg(target_os = "macos")]
const SECURITY: &str = "/usr/bin/security";
#[cfg(target_os = "macos")]
const SECURITY_TIME: Duration = Duration::from_secs(10);

fn unavailable() -> EngineError {
    failure("native_profile_credential_unavailable")
}

fn expiring() -> EngineError {
    failure("native_profile_credential_expiring")
}

fn unsafe_home() -> EngineError {
    failure("native_profile_agent_home_unsafe")
}

/// An account credential copied into one agent's own home for one turn. Dropping it removes
/// the copy from that home, so nothing outlives the turn that needed it.
pub(crate) struct AgentCredential {
    home: File,
    name: &'static str,
}

impl Drop for AgentCredential {
    /// Removes the copy beneath the agent home's descriptor when the turn ends.
    /// Input: the credential being dropped. Output: none; a copy already gone is fine.
    fn drop(&mut self) {
        let _ = rustix::fs::unlinkat(&self.home, self.name, AtFlags::empty());
    }
}

impl NativeProfileUse {
    /// Gives an agent's own home a copy of this account's credential that cannot refresh it,
    /// so the agent signs in without ever writing to, or reading from, the managed home.
    /// Codex gets `auth.json` with an empty refresh token and no API key; Claude Code gets
    /// `.credentials.json` holding only its OAuth record, with no refresh token.
    /// Inputs: the agent's private home and the turn's time bound. Output: the copy, removed
    /// when dropped; `native_profile_credential_expiring` when the access token lapses within
    /// the turn; `native_profile_credential_unavailable` or `native_profile_agent_home_unsafe`.
    pub(crate) fn hand_to_agent(
        &self,
        agent_home: &Path,
        turn: Duration,
    ) -> Result<AgentCredential, EngineError> {
        self.revalidate_retained()?;
        if self.configured.is_some() {
            return Err(unavailable());
        }
        let home = open_agent_home(agent_home)?;
        let until = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| unavailable())?
            .checked_add(turn)
            .ok_or_else(expiring)?;
        let harness =
            crate::harnesses::spec(self.binding.connection.harness.as_str()).map(|spec| spec.id);
        let (name, copy) = match harness {
            Some("codex") => (
                CODEX_RECORD,
                codex_copy(&read_record(&self.home.file, CODEX_RECORD)?, until)?,
            ),
            Some("claude-code") => (CLAUDE_RECORD, claude_copy(&self.claude_record()?, until)?),
            _ => return Err(unavailable()),
        };
        place(&home, name, &copy)?;
        Ok(AgentCredential { home, name })
    }

    #[cfg(target_os = "macos")]
    fn claude_record(&self) -> Result<Vec<u8>, EngineError> {
        let path = self.profile_path()?;
        keychain_record(&self.home.file, &path)
    }

    #[cfg(not(target_os = "macos"))]
    fn claude_record(&self) -> Result<Vec<u8>, EngineError> {
        read_record(&self.home.file, CLAUDE_RECORD)
    }
}

/// Opens an agent's home without following a link and checks it is this user's private
/// folder. Input: the home path. Output: its descriptor, or `native_profile_agent_home_unsafe`.
fn open_agent_home(path: &Path) -> Result<File, EngineError> {
    let home = File::from(
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| unsafe_home())?,
    );
    let stat = rustix::fs::fstat(&home).map_err(|_| unsafe_home())?;
    if stat.st_uid != rustix::process::geteuid().as_raw() || stat.st_mode & 0o077 != 0 {
        return Err(unsafe_home());
    }
    Ok(home)
}

/// Reads one credential file beneath a folder's descriptor without following a link: it
/// must be this user's regular file with one link and at most 64 KiB.
/// Inputs: the folder and the file name. Output: the bytes, or the unavailable refusal.
fn read_record(folder: &File, name: &str) -> Result<Vec<u8>, EngineError> {
    let file = File::from(
        rustix::fs::openat(
            folder,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| unavailable())?,
    );
    let stat = rustix::fs::fstat(&file).map_err(|_| unavailable())?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_nlink != 1
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_size < 0
        || stat.st_size as u64 > RECORD_BYTES_MAX
    {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    file.take(RECORD_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() as u64 > RECORD_BYTES_MAX {
        return Err(unavailable());
    }
    Ok(bytes)
}

/// Writes the copy under a fresh private name in the agent home and renames it into place,
/// so a link an agent left at that name is replaced, never followed.
/// Inputs: the agent home, the file name and the bytes. Output: done, or unavailable.
fn place(home: &File, name: &str, bytes: &[u8]) -> Result<(), EngineError> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| unavailable())?;
    let temporary = format!(".{name}.{:016x}", u64::from_le_bytes(nonce));
    let mut file = File::from(
        rustix::fs::openat(
            home,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| unavailable())?,
    );
    let written = rustix::fs::fchmod(&file, Mode::from_raw_mode(0o600))
        .map_err(|_| unavailable())
        .and_then(|()| file.write_all(bytes).map_err(|_| unavailable()))
        .and_then(|()| {
            rustix::fs::renameat(home, temporary.as_str(), home, name).map_err(|_| unavailable())
        });
    if written.is_err() {
        let _ = rustix::fs::unlinkat(home, temporary.as_str(), AtFlags::empty());
    }
    written
}

/// Builds the agent's Codex `auth.json` from the account's: the ChatGPT tokens with the
/// refresh token emptied, no API key and nothing else. Inputs: the account's record and the
/// moment the turn must end by. Output: the copy, or the expiring or unavailable refusal.
fn codex_copy(record: &[u8], until: Duration) -> Result<Vec<u8>, EngineError> {
    let record: Value = serde_json::from_slice(record).map_err(|_| unavailable())?;
    let tokens = record
        .get("tokens")
        .and_then(Value::as_object)
        .ok_or_else(unavailable)?;
    let text = |key: &str| {
        tokens
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(unavailable)
    };
    let access = text("access_token")?;
    let identity = text("id_token")?;
    if Duration::from_secs(token_expiry(access)?) <= until {
        return Err(expiring());
    }
    let copy = json!({
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": identity,
            "access_token": access,
            "refresh_token": "",
            "account_id": tokens.get("account_id").cloned().unwrap_or(Value::Null)
        },
        "last_refresh": record.get("last_refresh").cloned().unwrap_or(Value::Null)
    });
    let bytes = serde_json::to_vec(&copy).map_err(|_| unavailable())?;
    assert!(!bytes.is_empty());
    Ok(bytes)
}

/// Reads the `exp` claim of a ChatGPT access token without checking its signature, which
/// the engine cannot and need not do for a token Codex itself wrote. Input: the token.
/// Output: the expiry in seconds since the epoch, or unavailable.
fn token_expiry(token: &str) -> Result<u64, EngineError> {
    let claims = token.split('.').nth(1).ok_or_else(unavailable)?;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .map_err(|_| unavailable())?;
    serde_json::from_slice::<Value>(&claims)
        .ok()
        .and_then(|claims| claims.get("exp").and_then(Value::as_u64))
        .ok_or_else(unavailable)
}

/// Builds the agent's Claude Code `.credentials.json`: the account's OAuth record alone,
/// with no refresh token and none of the other records the account keeps. Inputs: the
/// account's credential record and the moment the turn must end by. Output: the copy, or
/// the expiring or unavailable refusal.
fn claude_copy(record: &[u8], until: Duration) -> Result<Vec<u8>, EngineError> {
    let record: Value = serde_json::from_slice(record).map_err(|_| unavailable())?;
    let mut oauth: Map<String, Value> = record
        .get("claudeAiOauth")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(unavailable)?;
    if !oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.is_empty())
    {
        return Err(unavailable());
    }
    let expires = oauth
        .get("expiresAt")
        .and_then(Value::as_u64)
        .ok_or_else(unavailable)?;
    if Duration::from_millis(expires) <= until {
        return Err(expiring());
    }
    oauth.insert("refreshToken".to_owned(), Value::Null);
    let bytes =
        serde_json::to_vec(&json!({ "claudeAiOauth": oauth })).map_err(|_| unavailable())?;
    assert!(!bytes.is_empty());
    Ok(bytes)
}

/// Reads the account's Claude Code credential from the managed home's own keychain, where
/// the sign-in custodian's Claude Code stored it under the item name Claude Code derives
/// from that home. The secret comes back only through a bounded pipe, never an argument.
/// Inputs: the managed home's descriptor and path. Output: the record, or unavailable.
#[cfg(target_os = "macos")]
fn keychain_record(home: &File, path: &Path) -> Result<Vec<u8>, EngineError> {
    use std::process::{Command, Stdio};
    let text = path
        .to_str()
        .filter(|text| text.is_ascii())
        .ok_or_else(unavailable)?;
    let library = open_folder(home, "Library")?;
    let keychains = open_folder(&library, "Keychains")?;
    let stat = rustix::fs::statat(&keychains, "login.keychain-db", AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unavailable())?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_nlink != 1
        || stat.st_uid != rustix::process::geteuid().as_raw()
    {
        return Err(unavailable());
    }
    let service = format!(
        "Claude Code-credentials-{}",
        &super::digest(text.as_bytes())[..8]
    );
    let keychain = path.join("Library/Keychains/login.keychain-db");
    let mut child = crate::spawn_gate::spawn(
        Command::new(SECURITY)
            .args(["find-generic-password", "-s", service.as_str(), "-w"])
            .arg(&keychain)
            .env_clear()
            .env("HOME", path)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|_| unavailable())?;
    let deadline = std::time::Instant::now() + SECURITY_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unavailable());
            }
        }
    };
    let mut bytes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(RECORD_BYTES_MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
    }
    if !status.success() || bytes.len() as u64 > RECORD_BYTES_MAX {
        return Err(unavailable());
    }
    while bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    Ok(bytes)
}

/// Opens one folder beneath a descriptor without following a link, as this user's.
/// Inputs: the parent and the name. Output: the folder, or unavailable.
#[cfg(target_os = "macos")]
fn open_folder(parent: &File, name: &str) -> Result<File, EngineError> {
    let folder = File::from(
        rustix::fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| unavailable())?,
    );
    let stat = rustix::fs::fstat(&folder).map_err(|_| unavailable())?;
    if stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(unavailable());
    }
    Ok(folder)
}
