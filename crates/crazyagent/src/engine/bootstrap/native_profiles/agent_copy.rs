use super::renewal::{Keeper, since_epoch};
use super::{NativeProfileUse, failure};
use crate::engine::{EngineError, off_worker};
use base64::Engine;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde_json::{Map, Value, json};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

pub(super) const CODEX_RECORD: &str = "auth.json";
pub(super) const CLAUDE_RECORD: &str = ".credentials.json";
const RECORD_BYTES_MAX: u64 = 65_536;
const USABLE_MIN: Duration = Duration::from_secs(60);

/// Held across every engine rename into a copy's name and every removal of a copy, so a
/// removal that finds its own file at the name removes that file and never one an engine
/// placement renamed there in between.
static PLACING: Mutex<()> = Mutex::new(());

/// The device and inode of the file a credential last placed at its name, shared with its
/// keeper, which records each renewed copy it places there.
pub(super) type Placed = Arc<Mutex<(u64, u64)>>;

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

#[cfg(target_os = "macos")]
fn keychain_unavailable() -> EngineError {
    failure("native_profile_agent_keychain_unavailable")
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
/// removes the copy it placed and any record of it in the agent's own keychain, so nothing
/// outlives the turn that needed it, and a newer copy placed at the same name for a later
/// start of the same agent stays.
pub(crate) struct AgentCredential {
    home: File,
    name: &'static str,
    placed: Placed,
    keychain: Option<AgentKeychain>,
    keeper: Option<Keeper>,
}

/// The keychain a Claude Code agent's own home holds, the default and only one there, and
/// the item Claude Code would file the agent's copy under in it (DEC-146). Each handoff
/// starts the agent on a newly created keychain under a new name, and the engine deletes
/// that item from exactly that keychain whenever the keeper replaces the copy or the copy
/// is removed, so the harness reads the copy the engine wrote, never a stale one, and an
/// earlier copy never reaches a later handoff's keychain. Codex keeps its sign-in only in
/// its file.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub(super) struct AgentKeychain {
    home: PathBuf,
    keychain: PathBuf,
    service: String,
}

/// No agent keychain exists off macOS, where Claude Code keeps its sign-in in its file.
#[cfg(not(target_os = "macos"))]
#[derive(Clone)]
pub(super) enum AgentKeychain {}

#[cfg(target_os = "macos")]
impl AgentKeychain {
    /// Gives a Claude Code agent's home a newly created, empty-password, unlocked keychain
    /// that never locks itself as that home's default and only search-list keychain, and
    /// only then deletes any an earlier launch left, so the harness, even one still running
    /// there, stores its sign-in there instead of asking macOS to create a login keychain.
    /// A native lead's teammates' home, when there is one beside it, gets the same keychain
    /// as its default, since teammates file the lead's sign-in under the lead's item name.
    /// The home must lie below the engine's own folder and outside the account profiles,
    /// see [`crate::engine::keychain::fresh_profile_keychain`]. Every call names the
    /// keychain or runs with `HOME` set to an agent home, so the owner's keychain settings
    /// are untouched. Inputs: the engine folder and the agent home. Output: the keychain,
    /// or `native_profile_agent_keychain_unavailable`.
    fn prepare(engine: &Path, home: &Path) -> Result<Self, EngineError> {
        let text = home
            .to_str()
            .filter(|text| text.is_ascii())
            .ok_or_else(keychain_unavailable)?;
        if std::fs::canonicalize(engine.join(super::ROOT))
            .map_or(true, |profiles| home.starts_with(profiles))
        {
            return Err(keychain_unavailable());
        }
        let mates = crate::launch_context::mates_home(home);
        let sharers: Vec<&Path> =
            if std::fs::symlink_metadata(&mates).is_ok_and(|metadata| metadata.is_dir()) {
                vec![mates.as_path()]
            } else {
                Vec::new()
            };
        let keychain = crate::engine::keychain::fresh_profile_keychain(engine, home, &sharers)
            .map_err(|_| keychain_unavailable())?;
        assert_eq!(
            keychain.parent(),
            Some(home.join("Library/Keychains").as_path())
        );
        Ok(Self {
            home: home.to_path_buf(),
            keychain,
            service: claude_service(text),
        })
    }

    /// Deletes the item Claude Code filed the agent's copy under from the keychain this
    /// handoff created, naming that keychain and following no link to it; one a later
    /// handoff already replaced is fine. Input: the keychain. Output: done, or
    /// `native_profile_agent_keychain_unavailable`.
    pub(super) fn forget(&self) -> Result<(), EngineError> {
        crate::engine::keychain::forget_record(&self.home, &self.keychain, &self.service)
            .map_err(|_| keychain_unavailable())
    }
}

#[cfg(not(target_os = "macos"))]
impl AgentKeychain {
    /// Never runs: no agent keychain exists off macOS. Input: the keychain. Output: done.
    pub(super) fn forget(&self) -> Result<(), EngineError> {
        match *self {}
    }
}

/// The keychain the agent's harness keeps its sign-in in, given before its copy is written:
/// one for Claude Code on macOS and none for Codex, which runs with its file store. Inputs:
/// the harness, the agent home and the engine folder agent homes lie below. Output: the
/// keychain or none, or the refusal.
fn agent_keychain(
    harness: Harness,
    home: &Path,
    engine: &Path,
) -> Result<Option<AgentKeychain>, EngineError> {
    match harness {
        #[cfg(target_os = "macos")]
        Harness::Claude => AgentKeychain::prepare(engine, home).map(Some),
        #[cfg(not(target_os = "macos"))]
        Harness::Claude => {
            let _ = (home, engine);
            Ok(None)
        }
        Harness::Codex => Ok(None),
    }
}

impl AgentCredential {
    /// Leaves the copy in the agent's home for an agent whose processes may still run, as
    /// when its sandbox could not be confirmed empty: stops the keeper, so no renewed token
    /// reaches the copy, and keeps the copy and the home's descriptor for the engine's life.
    /// Takes the credential. Returns nothing.
    pub(crate) fn retain(mut self) {
        drop(self.keeper.take());
        assert!(self.keeper.is_none());
        std::mem::forget(self);
    }
}

impl Drop for AgentCredential {
    /// Stops the keeper, after which the placed identity no longer changes, then removes
    /// the copy beneath the agent home's descriptor only while its name still holds that
    /// file, and its record in the agent's keychain, which this handoff alone created.
    /// Input: the credential being dropped. Output: none; a copy or record already gone, or
    /// a copy replaced, is left alone.
    fn drop(&mut self) {
        drop(self.keeper.take());
        let placed = *self.placed.lock().unwrap_or_else(PoisonError::into_inner);
        remove_placed(&self.home, self.name, placed);
        if let Some(keychain) = &self.keychain {
            let _ = keychain.forget();
        }
    }
}

/// Removes the file at `name` in the agent home only while that name still refers to the
/// file with the placed device and inode, under the lock every engine placement takes.
/// Inputs: the agent home, the copy's name and the placed identity. Output: none; a name
/// that is gone or holds another file is left alone.
#[allow(clippy::unnecessary_cast)]
fn remove_placed(home: &File, name: &str, placed: (u64, u64)) {
    let _placing = PLACING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Ok(named) = rustix::fs::statat(home, name, AtFlags::SYMLINK_NOFOLLOW)
        && (named.st_dev as u64, named.st_ino) == placed
    {
        let _ = rustix::fs::unlinkat(home, name, AtFlags::empty());
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

/// The keychain item name Claude Code files a sign-in under for the secure storage folder
/// it is given, which the engine sets to the harness's own home. Input: the folder's ASCII
/// path. Output: the service name.
#[cfg(target_os = "macos")]
fn claude_service(folder: &str) -> String {
    assert!(folder.is_ascii());
    format!(
        "Claude Code-credentials-{}",
        &super::digest(folder.as_bytes())[..8]
    )
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
    /// On macOS a Claude Code agent's home first gets its own newly created keychain, from
    /// which any record of the copy is deleted whenever the keeper replaces the copy or the
    /// copy is removed (DEC-146).
    /// Inputs: the agent's private home, the turn's time bound and the harness executable.
    /// Output: the copy, removed when dropped; `native_profile_renewal_failed`, a transient
    /// refusal, when the token still lapses within a minute after a renewal that ended
    /// without a fresh token (no network, a timeout); `native_profile_sign_in_revoked` when
    /// Claude Code ended the sign-in after its provider refused a refresh;
    /// `native_profile_codex_api_key` or `native_profile_codex_token_unreadable` for a
    /// Codex record without usable ChatGPT tokens; `native_profile_credential_unavailable`,
    /// `native_profile_renewal_busy`, `native_profile_agent_home_unsafe` or
    /// `native_profile_agent_keychain_unavailable`.
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
        let keychain = agent_keychain(harness, agent_home, &self.custody.anchor.path)?;
        let placed = Arc::new(Mutex::new(place(&home, name, &record.copy)?));
        let mut credential = AgentCredential {
            home,
            name,
            placed: Arc::clone(&placed),
            keychain,
            keeper: None,
        };
        assert_eq!(
            credential.keychain.is_some(),
            cfg!(target_os = "macos") && harness == Harness::Claude
        );
        if turn.is_none() || (renewal.is_some() && record.expires <= turn_end) {
            let renewal = match renewal {
                Some(renewal) => renewal,
                None => self.renewal(harness, executable)?,
            };
            let agent = credential.home.try_clone().map_err(|_| unavailable())?;
            credential.keeper = Some(Keeper::start(
                renewal,
                agent,
                name,
                placed,
                credential.keychain.clone(),
                record.expires,
            )?);
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
/// Inputs: the agent home, the file name and the bytes. Output: the placed file's device
/// and inode, or unavailable.
#[allow(clippy::unnecessary_cast)]
pub(super) fn place(home: &File, name: &str, bytes: &[u8]) -> Result<(u64, u64), EngineError> {
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
        .and_then(|()| rustix::fs::fstat(&file).map_err(|_| unavailable()))
        .and_then(|stat| {
            let _placing = PLACING.lock().unwrap_or_else(PoisonError::into_inner);
            rustix::fs::renameat(home, temporary.as_str(), home, name)
                .map(|()| (stat.st_dev as u64, stat.st_ino))
                .map_err(|_| unavailable())
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
    let service = claude_service(text);
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
