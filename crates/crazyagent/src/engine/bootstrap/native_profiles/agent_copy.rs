use super::renewal::{Keeper, since_epoch};
use super::{NativeProfileUse, failure};
use crate::engine::{EngineError, off_worker};
use base64::Engine;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde_json::{Map, Value, json};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

pub(super) const CODEX_RECORD: &str = "auth.json";
pub(super) const CLAUDE_RECORD: &str = ".credentials.json";
const RECORD_BYTES_MAX: u64 = 65_536;
const USABLE_MIN: Duration = Duration::from_secs(60);

/// How far ahead a turn with no time bound has its copy checked when it starts: the day a
/// team agent's copy is checked for. After that its keeper renews the copy, however long
/// the turn runs.
const OPEN_TURN_AHEAD: Duration = Duration::from_secs(24 * 60 * 60);
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

fn renewal_failed() -> EngineError {
    failure("native_profile_renewal_failed")
}

fn revoked() -> EngineError {
    failure("native_profile_sign_in_revoked")
}

fn unsafe_home() -> EngineError {
    failure("native_profile_agent_home_unsafe")
}

fn wide_scope() -> EngineError {
    failure("native_profile_scope_too_wide")
}

/// The Claude OAuth scope prefix an agent's copy may never carry. A Console sign-in holds
/// `org:create_api_key`, which turns a copy into a durable API key, and the `org:` family
/// grants organisation-wide access; a subscription sign-in holds only `user:` scopes
/// (inference, profile, sessions, mcp_servers, file_upload), which stay turn-bounded.
const AGENT_CLAUDE_FORBIDDEN_SCOPE_PREFIX: &str = "org:";

/// The Claude OAuth scope an agent's copy must carry: without inference the copy cannot run
/// the harness at all, so a record missing it, or with no scopes, is not a usable sign-in.
const AGENT_CLAUDE_REQUIRED_SCOPE: &str = "user:inference";

fn codex_api_key() -> EngineError {
    failure("native_profile_codex_api_key")
}

fn codex_unreadable() -> EngineError {
    failure("native_profile_codex_token_unreadable")
}

fn codex_expired() -> EngineError {
    failure("native_profile_codex_sign_in_expired")
}

/// The harness whose credential record a managed account keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Harness {
    Codex,
    Claude,
}

impl Harness {
    /// The file name an agent's copy has in its home. Output: the name.
    fn copy_name(self) -> &'static str {
        match self {
            Self::Codex => CODEX_RECORD,
            Self::Claude => CLAUDE_RECORD,
        }
    }
}

/// An agent's copy of the account's credential and the moment its access token lapses.
pub(super) struct Record {
    pub(super) copy: Vec<u8>,
    pub(super) expires: Duration,
}

/// An account credential copied into one agent's own home for one turn, with the keeper
/// that renews it when the turn may outlast it. Dropping it stops the keeper and then
/// removes the copy, so nothing outlives the turn that needed it.
pub(crate) struct AgentCredential {
    home: File,
    name: &'static str,
    keeper: Option<Keeper>,
}

impl AgentCredential {
    /// Leaves the copy in the agent's home for an agent whose processes may still run, as
    /// when its sandbox could not be confirmed empty: stops the keeper, so no renewed token
    /// reaches the copy, and keeps the copy and the home's descriptor for the engine's life.
    /// Takes the credential. Returns nothing.
    pub(crate) fn retain(mut self) {
        self.stop_renewing();
        std::mem::forget(self);
    }

    /// Stops the keeper of a copy that processes of an agent may still read after its stop,
    /// so no renewed token reaches the copy; the copy stays until the credential is dropped.
    /// Takes the credential. Returns nothing.
    pub(crate) fn stop_renewing(&mut self) {
        drop(self.keeper.take());
        assert!(self.keeper.is_none());
    }
}

impl Drop for AgentCredential {
    /// Stops the keeper, then removes the copy beneath the agent home's descriptor.
    /// Input: the credential being dropped. Output: none; a copy already gone is fine.
    fn drop(&mut self) {
        drop(self.keeper.take());
        let _ = rustix::fs::unlinkat(&self.home, self.name, AtFlags::empty());
    }
}

/// Reads the account's credential record and builds the agent's copy from it.
/// Inputs: the harness, the managed home's descriptor and path. Output: the record.
pub(super) fn account_record(
    harness: Harness,
    home: &File,
    path: &Path,
) -> Result<Record, EngineError> {
    match harness {
        Harness::Codex => codex_copy(&read_record(home, CODEX_RECORD)?),
        Harness::Claude => claude_copy(&claude_source(home, path)?),
    }
}

#[cfg(target_os = "macos")]
fn claude_source(home: &File, path: &Path) -> Result<Vec<u8>, EngineError> {
    keychain_record(home, path)
}

#[cfg(not(target_os = "macos"))]
fn claude_source(home: &File, _path: &Path) -> Result<Vec<u8>, EngineError> {
    read_record(home, CLAUDE_RECORD)
}

impl NativeProfileUse {
    /// Gives an agent's own home a copy of this account's credential that cannot refresh it,
    /// so the agent signs in without ever writing to, or reading from, the managed home.
    /// Codex gets `auth.json` with an empty refresh token and no API key; Claude Code gets
    /// `.credentials.json` holding only its OAuth record, with no refresh token. When the
    /// access token lapses within the turn, the harness first renews the account in the
    /// managed home, and a keeper renews it again and replaces the copy while the turn runs.
    /// Inputs: the agent's private home, the turn's time bound and the harness executable.
    /// Output: the copy, removed when dropped; `native_profile_renewal_failed`, a transient
    /// refusal, when the token still lapses within a minute after a renewal that ended
    /// without a fresh token (no network, a timeout); `native_profile_sign_in_revoked` when
    /// Claude Code ended the sign-in after its provider refused a refresh;
    /// `native_profile_codex_api_key` or `native_profile_codex_token_unreadable` for a
    /// Codex record without usable ChatGPT tokens; `native_profile_credential_unavailable`,
    /// `native_profile_renewal_busy` or `native_profile_agent_home_unsafe`.
    pub(crate) fn hand_to_agent(
        &self,
        agent_home: &Path,
        turn: Duration,
        executable: &Path,
    ) -> Result<AgentCredential, EngineError> {
        self.hand_to_turn(agent_home, Some(turn), executable)
    }

    /// Does what `hand_to_agent` does for a turn that may have no time bound. A bounded turn
    /// gets a keeper only when its copy lapses within it. A turn with no bound always gets
    /// one, whatever time the token has left (OB-5 r2): the keeper acts only near the
    /// expiry. Inputs: the agent's private home, the turn's time bound or none, and the
    /// harness executable. Output: as `hand_to_agent`.
    pub(crate) fn hand_to_turn(
        &self,
        agent_home: &Path,
        turn: Option<Duration>,
        executable: &Path,
    ) -> Result<AgentCredential, EngineError> {
        self.revalidate_retained()?;
        if self.configured.is_some() {
            return Err(unavailable());
        }
        let home = open_agent_home(agent_home)?;
        let harness = match crate::harnesses::spec(self.binding.connection.harness.as_str())
            .map(|spec| spec.id)
        {
            Some("codex") => Harness::Codex,
            Some("claude-code") => Harness::Claude,
            _ => return Err(unavailable()),
        };
        let path = self.profile_path()?;
        let mut record = account_record(harness, &self.home.file, &path)?;
        let now = since_epoch()?;
        let ahead = turn.unwrap_or(OPEN_TURN_AHEAD);
        assert!(!ahead.is_zero());
        let turn_end = now.checked_add(ahead).ok_or_else(expiring)?;
        let renewal = if record.expires <= turn_end.max(now + USABLE_MIN) {
            let renewal = self.renewal(harness, executable)?;
            record = off_worker(|| renewal.renew(record.expires))?;
            if record.expires <= since_epoch()? + USABLE_MIN {
                return Err(renewal_failed());
            }
            Some(renewal)
        } else {
            None
        };
        let name = harness.copy_name();
        place(&home, name, &record.copy)?;
        let mut credential = AgentCredential {
            home,
            name,
            keeper: None,
        };
        if turn.is_none() || (renewal.is_some() && record.expires <= turn_end) {
            let renewal = match renewal {
                Some(renewal) => renewal,
                None => self.renewal(harness, executable)?,
            };
            let agent = credential.home.try_clone().map_err(|_| unavailable())?;
            credential.keeper = Some(Keeper::start(renewal, agent, name, record.expires)?);
        }
        Ok(credential)
    }

    /// Reads this account's OAuth access token for one usage poll by the engine itself,
    /// first renewing it through the harness when it lapses within a minute, as an agent
    /// handoff does. The token is read from the same refresh-free copy an agent gets and
    /// never leaves the returned value. Input: the harness executable. Output: the token,
    /// the ChatGPT account id Codex sends with it and the plan Claude Code recorded; for a
    /// token that still lapses after its renewal, `native_profile_codex_sign_in_expired`
    /// for Codex and `native_profile_renewal_failed` for Claude Code; the Codex record's own
    /// refusals, or the unavailable refusal.
    pub(crate) fn usage_credential(
        &self,
        executable: &Path,
    ) -> Result<crate::usage::poll::PollCredential, EngineError> {
        self.revalidate_retained()?;
        if self.configured.is_some() {
            return Err(unavailable());
        }
        let harness = match crate::harnesses::spec(self.binding.connection.harness.as_str())
            .map(|spec| spec.id)
        {
            Some("codex") => Harness::Codex,
            Some("claude-code") => Harness::Claude,
            _ => return Err(unavailable()),
        };
        let path = self.profile_path()?;
        let mut record = account_record(harness, &self.home.file, &path)?;
        if record.expires <= since_epoch()? + USABLE_MIN {
            let renewal = self.renewal(harness, executable)?;
            record = off_worker(|| renewal.renew(record.expires))?;
            if record.expires <= since_epoch()? + USABLE_MIN {
                record.copy.fill(0);
                return Err(match harness {
                    Harness::Codex => codex_expired(),
                    Harness::Claude => renewal_failed(),
                });
            }
        }
        let credential = poll_credential(harness, &record.copy);
        record.copy.fill(0);
        credential
    }

    /// When this Claude Code account's access token lapses, read from the record exactly as
    /// the agent handoff reads it, for the sign-in check to renew a lapsing token first.
    /// Input: none. Output: the expiry since the epoch; `native_profile_sign_in_revoked`
    /// for a sign-in Claude Code ended, or the unavailable refusal.
    pub(crate) fn claude_expiry(&self) -> Result<Duration, EngineError> {
        account_record(Harness::Claude, &self.home.file, &self.profile_path()?)
            .map(|record| record.expires)
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
pub(super) fn place(home: &File, name: &str, bytes: &[u8]) -> Result<(), EngineError> {
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
/// refresh token emptied, no API key, and the id token's signature dropped so the agent
/// reads its claims but cannot exchange it for a durable API key. Input: the account's
/// record. Output: the copy with its access token's expiry; `native_profile_codex_api_key`
/// for an API-key sign-in without ChatGPT tokens, `native_profile_codex_token_unreadable`
/// for a malformed record or token, or the unavailable refusal.
fn codex_copy(record: &[u8]) -> Result<Record, EngineError> {
    let record: Value = serde_json::from_slice(record).map_err(|_| codex_unreadable())?;
    let tokens = record
        .get("tokens")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            let keyed = record
                .get("OPENAI_API_KEY")
                .and_then(Value::as_str)
                .is_some_and(|key| !key.is_empty());
            if keyed {
                codex_api_key()
            } else {
                codex_unreadable()
            }
        })?;
    let text = |key: &str| {
        tokens
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(codex_unreadable)
    };
    let access = text("access_token")?;
    let identity = claims_only(text("id_token")?)?;
    let expires = Duration::from_secs(token_expiry(access)?);
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
    let copy = serde_json::to_vec(&copy).map_err(|_| unavailable())?;
    assert!(!copy.is_empty());
    Ok(Record { copy, expires })
}

/// A fixed, non-verifying signature put in place of an id token's real one. Any JWT parser
/// that expects three non-empty parts accepts it, while it cannot pass the provider's
/// signature check, so the token stays unexchangeable for a durable credential.
const AGENT_ID_TOKEN_SIGNATURE: &str = "unexchangeable";

/// Keeps a signed JWT's claims but replaces its signature, so an agent reads the account
/// claims its harness needs while the token can no longer be exchanged for a durable
/// credential. A compact JWS is three non-empty dot-separated parts; this returns the
/// header and payload unchanged with [`AGENT_ID_TOKEN_SIGNATURE`] in place of the real
/// signature, so a parser that rejects an empty part still loads it. Input: the account's
/// id token. Output: the claims-only token, or `native_profile_codex_token_unreadable`
/// when the input is not a three-part JWT.
fn claims_only(token: &str) -> Result<String, EngineError> {
    let mut parts = token.split('.');
    let header = parts.next().filter(|part| !part.is_empty());
    let payload = parts.next().filter(|part| !part.is_empty());
    let signature = parts.next().filter(|part| !part.is_empty());
    let (header, payload) = match (header, payload, signature, parts.next()) {
        (Some(header), Some(payload), Some(_), None) => (header, payload),
        _ => return Err(codex_unreadable()),
    };
    let stripped = format!("{header}.{payload}.{AGENT_ID_TOKEN_SIGNATURE}");
    assert!(stripped.matches('.').count() == 2 && !stripped.ends_with('.'));
    Ok(stripped)
}

/// Takes what a usage poll sends from an agent's copy of the account's credential: the
/// access token, the ChatGPT account id for Codex, and the plan for Claude Code.
/// Inputs: the harness and the copy. Output: the poll credential;
/// `native_profile_codex_token_unreadable` for a Codex copy without an access token, or
/// unavailable.
fn poll_credential(
    harness: Harness,
    copy: &[u8],
) -> Result<crate::usage::poll::PollCredential, EngineError> {
    let record: Value = serde_json::from_slice(copy).map_err(|_| unavailable())?;
    let (token, account_id, plan) = match harness {
        Harness::Codex => {
            let tokens = &record["tokens"];
            (
                tokens["access_token"].as_str(),
                tokens["account_id"].as_str(),
                None,
            )
        }
        Harness::Claude => {
            let oauth = &record["claudeAiOauth"];
            (
                oauth["accessToken"].as_str(),
                None,
                oauth["subscriptionType"].as_str(),
            )
        }
    };
    let token = token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| match harness {
            Harness::Codex => codex_unreadable(),
            Harness::Claude => unavailable(),
        })?;
    Ok(crate::usage::poll::PollCredential {
        token: crate::credentials::SecretString::bearer(token.as_bytes().to_vec())
            .map_err(|_| unavailable())?,
        account_id: account_id.map(str::to_owned),
        plan: plan.map(str::to_owned),
    })
}

/// Reads the `exp` claim of a ChatGPT access token without checking its signature, which
/// the engine cannot and need not do for a token Codex itself wrote. Input: the token.
/// Output: the expiry in seconds since the epoch, or `native_profile_codex_token_unreadable`.
fn token_expiry(token: &str) -> Result<u64, EngineError> {
    let claims = token.split('.').nth(1).ok_or_else(codex_unreadable)?;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .map_err(|_| codex_unreadable())?;
    serde_json::from_slice::<Value>(&claims)
        .ok()
        .and_then(|claims| claims.get("exp").and_then(Value::as_u64))
        .ok_or_else(codex_unreadable)
}

/// Builds the agent's Claude Code `.credentials.json`: only the fields Claude Code reads
/// from the account's OAuth record, with no refresh token and none of the other records
/// the account keeps. Claude Code empties both tokens when its provider refuses a refresh.
/// Each scope is trimmed, case-folded to lowercase and must be a string; the record is refused when any is in the
/// [`AGENT_CLAUDE_FORBIDDEN_SCOPE_PREFIX`] family or when none is
/// [`AGENT_CLAUDE_REQUIRED_SCOPE`] (an empty or inference-less list is not a usable sign-in),
/// so an agent never receives a Console credential that can create API keys or grant
/// organisation-wide access, and a subscription's `user:` scopes are accepted and kept.
/// Input: the account's record. Output: the copy with its access token's expiry,
/// `native_profile_sign_in_revoked` for emptied tokens, `native_profile_scope_too_wide`
/// for an org, empty, non-string or inference-less scope set, or the unavailable refusal.
fn claude_copy(record: &[u8]) -> Result<Record, EngineError> {
    let record: Value = serde_json::from_slice(record).map_err(|_| unavailable())?;
    let oauth: &Map<String, Value> = record
        .get("claudeAiOauth")
        .and_then(Value::as_object)
        .ok_or_else(unavailable)?;
    let token = |name: &str| oauth.get(name).and_then(Value::as_str);
    if token("accessToken") == Some("") && token("refreshToken") == Some("") {
        return Err(revoked());
    }
    if !token("accessToken").is_some_and(|token| !token.is_empty()) {
        return Err(unavailable());
    }
    let scopes = oauth
        .get("scopes")
        .and_then(Value::as_array)
        .ok_or_else(unavailable)?;
    let mut has_required = false;
    for scope in scopes {
        let scope = scope
            .as_str()
            .ok_or_else(wide_scope)?
            .trim()
            .to_ascii_lowercase();
        if scope.starts_with(AGENT_CLAUDE_FORBIDDEN_SCOPE_PREFIX) {
            return Err(wide_scope());
        }
        if scope == AGENT_CLAUDE_REQUIRED_SCOPE {
            has_required = true;
        }
    }
    if !has_required {
        return Err(wide_scope());
    }
    let expires_ms = oauth
        .get("expiresAt")
        .and_then(Value::as_u64)
        .ok_or_else(unavailable)?;
    let mut kept = Map::new();
    kept.insert(
        "accessToken".to_owned(),
        oauth.get("accessToken").cloned().unwrap_or(Value::Null),
    );
    kept.insert("refreshToken".to_owned(), Value::Null);
    kept.insert("expiresAt".to_owned(), Value::from(expires_ms));
    kept.insert("scopes".to_owned(), Value::Array(scopes.clone()));
    if let Some(subscription) = oauth.get("subscriptionType") {
        kept.insert("subscriptionType".to_owned(), subscription.clone());
    }
    let copy = serde_json::to_vec(&json!({ "claudeAiOauth": kept })).map_err(|_| unavailable())?;
    assert!(!copy.is_empty());
    Ok(Record {
        copy,
        expires: Duration::from_millis(expires_ms),
    })
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
