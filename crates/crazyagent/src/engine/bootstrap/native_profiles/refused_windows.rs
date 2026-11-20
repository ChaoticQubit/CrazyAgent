use super::{NativeProfileCustody, NativeProfileUsePurpose, digest, failure};
use crate::config::ModelConnection;
use crate::engine::EngineError;
use crate::engine::owner::{AccountSecret, HostStamp, NativeProfileBinding};
use crate::protocol::{CredentialRef, RequestId};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfiguredRcliProfileMetadata {
    pub(crate) profile_ref: CredentialRef,
    pub(crate) custody_id: String,
    pub(crate) profile_config_sha256: String,
    pub(crate) profile_generation: u64,
}

/// Matches the Unix check for a generated SDK home missing its key file. No generated home
/// exists without native profile custody. Inputs: the engine root and the credential
/// reference, unused. Output: false.
pub(crate) fn generated_home_needs_key(
    _root: &Path,
    _profile_ref: &CredentialRef,
) -> Result<bool, EngineError> {
    Ok(false)
}

/// A held native profile lock for settling a stale sign-in, which cannot exist without
/// native profile custody. No value is ever made.
pub(crate) enum NativeProfileRelease {}

impl Drop for NativeProfileRelease {
    /// Matches the Unix release, which unlocks the profile when dropped, so the sign-in
    /// settlement that ends it at a chosen point means the same on every target.
    /// No value is ever made, so this never runs.
    fn drop(&mut self) {
        match *self {}
    }
}

/// A held native profile lease, which cannot exist without native profile custody.
/// No value is ever made, so every method below is unreachable by construction.
pub(crate) enum NativeProfileUse {}

impl NativeProfileUse {
    pub(crate) fn account(&self) -> &crate::protocol::CredentialRef {
        match *self {}
    }

    pub(crate) fn profile_path(&self) -> Result<PathBuf, EngineError> {
        match *self {}
    }

    pub(crate) fn engine_root(&self) -> &Path {
        match *self {}
    }

    pub(crate) fn revalidate_retained(&self) -> Result<(), EngineError> {
        match *self {}
    }

    pub(crate) fn revalidate(
        &self,
        _host: &HostStamp,
        _binding: &NativeProfileBinding,
        _operation_id: &RequestId,
        _generation: u64,
        _purpose: NativeProfileUsePurpose,
    ) -> Result<(), EngineError> {
        match *self {}
    }
}

/// Matches the Unix credential copy; no lease exists here, so no copy is ever made.
pub(crate) enum AgentCredential {}

impl AgentCredential {
    /// Matches the Unix copy kept for an agent whose cleanup is uncertain.
    /// No value is ever made, so this never runs.
    pub(crate) fn retain(self) {
        match self {}
    }
}

impl Drop for AgentCredential {
    /// Matches the Unix copy, which leaves the agent's home when dropped, so the callers
    /// that end a copy at a chosen point mean the same on every target.
    /// No value is ever made, so this never runs.
    fn drop(&mut self) {
        match *self {}
    }
}

impl NativeProfileUse {
    /// Matches the Unix handoff of a credential copy to an agent's own home. No value is
    /// ever made here. Inputs: the agent home and turn bound, unused. Output: none.
    pub(crate) fn hand_to_agent(
        &self,
        _agent_home: &Path,
        _turn: Duration,
        _executable: &Path,
    ) -> Result<AgentCredential, EngineError> {
        match *self {}
    }
}

impl NativeProfileUse {
    /// Matches the Unix handoff of a credential copy to a master turn, whose time bound may be
    /// none. No value is ever made here. Inputs: the agent home and the turn bound, unused.
    /// Output: none.
    pub(crate) fn hand_to_turn(
        &self,
        _agent_home: &Path,
        _turn: Option<Duration>,
        _executable: &Path,
    ) -> Result<AgentCredential, EngineError> {
        match *self {}
    }
}

impl NativeProfileUse {
    /// Matches the Unix read of an account's access token for one engine usage poll. No
    /// value is ever made here. Input: the harness executable, unused. Output: none.
    pub(crate) fn usage_credential(
        &self,
        _executable: &Path,
    ) -> Result<crate::usage::poll::PollCredential, EngineError> {
        match *self {}
    }
}

impl Drop for NativeProfileUse {
    /// Matches the Unix lease, which releases its profile locks when dropped, so the
    /// callers that end a lease at a chosen point mean the same on every target.
    /// No value is ever made, so this never runs.
    fn drop(&mut self) {
        match *self {}
    }
}

/// The custody parts no Windows slice has ported yet (DESIGN-0515 D5): each refuses with
/// `native_profile_custody_unavailable`, so no profile, configured home or lease is made.
impl NativeProfileCustody {
    pub(crate) fn hold_released_profile(
        &self,
        _host: &HostStamp,
        _key: &str,
    ) -> Result<Option<NativeProfileRelease>, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    /// The generated SDK home a configured account would use, the same path as on Unix.
    /// Input: the profile reference. Output: the path.
    pub(crate) fn generated_configured_home(&self, profile_ref: &CredentialRef) -> PathBuf {
        let home = self
            .anchor
            .path
            .join(crate::engine::bootstrap::CONFIGURED_SDK_HOMES_FOLDER)
            .join(digest(profile_ref.as_str().as_bytes()));
        assert!(home.starts_with(&self.anchor.path));
        home
    }

    pub(crate) fn acquire_use(
        &mut self,
        _host: &HostStamp,
        _binding: &NativeProfileBinding,
        _operation_id: &RequestId,
        _generation: u64,
        _purpose: NativeProfileUsePurpose,
    ) -> Result<NativeProfileUse, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    /// Matches the Unix use with a chosen renewal wait, refused until a Windows slice ports
    /// the sign-in lease. Inputs: the use request and the wait, unused. Output: the refusal.
    pub(crate) fn acquire_use_within(
        &mut self,
        _host: &HostStamp,
        _binding: &NativeProfileBinding,
        _operation_id: &RequestId,
        _generation: u64,
        _purpose: NativeProfileUsePurpose,
        _renewal_wait: Option<Duration>,
    ) -> Result<NativeProfileUse, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    pub(crate) fn prepare(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
    ) -> Result<(String, String), EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    pub(crate) fn register_configured_rcli_profile(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
        _target_profile_ref: &CredentialRef,
        _configured_home: &Path,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    pub(crate) fn register_undone_configured_rcli_profile(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
        _target_profile_ref: &CredentialRef,
        _configured_home: &Path,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    pub(crate) fn configured_rcli_home(
        &self,
        _host: &HostStamp,
        _target_profile_ref: &CredentialRef,
    ) -> Result<PathBuf, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    pub(crate) fn configured_rcli_profile(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
        _target_profile_ref: &CredentialRef,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    pub(crate) fn acquire_configured_rcli_use(
        &mut self,
        _host: &HostStamp,
        _binding: &NativeProfileBinding,
        _target_profile_ref: &CredentialRef,
        _operation_id: &RequestId,
        _generation: u64,
    ) -> Result<NativeProfileUse, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    pub(crate) fn prepare_configured_rcli_profile(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
        _profile_ref: &CredentialRef,
        _secret: Option<&AccountSecret>,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }

    pub(crate) fn repair_legacy_configured_home(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
        _profile_ref: &CredentialRef,
    ) -> Result<(), EngineError> {
        Err(failure("native_profile_custody_unavailable"))
    }
}
