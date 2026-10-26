use crate::config::ModelConnection;
use crate::engine::EngineError;
use crate::engine::owner::{AccountSecret, HostStamp, NativeProfileBinding, PreparedNativeProfile};
use crate::protocol::{CredentialRef, RequestId};
use sha2::{Digest, Sha256};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfiguredRcliProfileMetadata {
    pub(crate) profile_ref: CredentialRef,
    pub(crate) custody_id: String,
    pub(crate) profile_config_sha256: String,
    pub(crate) profile_generation: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeProfileUsePurpose {
    Authentication,
    #[allow(
        dead_code,
        reason = "Refresh requires the separately qualified native host"
    )]
    Refresh,
    Logout,
    #[allow(
        dead_code,
        reason = "Execution requires the separately qualified native host"
    )]
    Execution,
}

/// Native profile custody, which cannot exist without Unix directory-relative file custody.
/// No value is ever made: `capture` reports `native_profile_custody_unavailable`, so every
/// method below is unreachable by construction.
pub(crate) enum NativeProfileCustody {}

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
    pub(crate) fn profile_path(&self) -> Result<PathBuf, EngineError> {
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

impl NativeProfileUse {
    /// Matches the Unix handoff of a credential copy to an agent's own home. No value is
    /// ever made here. Inputs: the agent home and turn bound, unused. Output: none.
    pub(crate) fn hand_to_agent(
        &self,
        _agent_home: &Path,
        _turn: Duration,
    ) -> Result<AgentCredential, EngineError> {
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

impl NativeProfileCustody {
    /// Refuses native profile custody where Unix owner, inode and directory-relative checks
    /// do not exist. Takes the database path and metadata, both unused.
    /// Returns `native_profile_custody_unavailable`.
    pub(crate) fn capture(_path: &Path, _opened: &Metadata) -> Result<Self, EngineError> {
        Err(EngineError("native_profile_custody_unavailable".into()))
    }

    pub(crate) fn hold_released_profile(
        &self,
        _host: &HostStamp,
        _key: &str,
    ) -> Result<Option<NativeProfileRelease>, EngineError> {
        match *self {}
    }

    pub(crate) fn generated_configured_home(&self, _profile_ref: &CredentialRef) -> PathBuf {
        match *self {}
    }

    pub(crate) fn acquire_use(
        &mut self,
        _host: &HostStamp,
        _binding: &NativeProfileBinding,
        _operation_id: &RequestId,
        _generation: u64,
        _purpose: NativeProfileUsePurpose,
    ) -> Result<NativeProfileUse, EngineError> {
        match *self {}
    }

    pub(crate) fn prepare(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
    ) -> Result<(String, String), EngineError> {
        match *self {}
    }

    pub(crate) fn register_configured_rcli_profile(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
        _target_profile_ref: &CredentialRef,
        _configured_home: &Path,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        match *self {}
    }

    pub(crate) fn configured_rcli_profile(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
        _target_profile_ref: &CredentialRef,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        match *self {}
    }

    pub(crate) fn acquire_configured_rcli_use(
        &mut self,
        _host: &HostStamp,
        _binding: &NativeProfileBinding,
        _target_profile_ref: &CredentialRef,
        _operation_id: &RequestId,
        _generation: u64,
    ) -> Result<NativeProfileUse, EngineError> {
        match *self {}
    }

    pub(crate) fn prepare_configured_rcli_profile(
        &mut self,
        _host: &HostStamp,
        _connection: &ModelConnection,
        _profile_ref: &CredentialRef,
        _secret: Option<&AccountSecret>,
    ) -> Result<ConfiguredRcliProfileMetadata, EngineError> {
        match *self {}
    }

    /// Builds the owner receipt for a prepared profile, exactly as on Unix.
    /// Takes the binding, the requested and prepared references and the profile key.
    /// Returns the receipt; a mismatched reference or key trips an assertion.
    pub(crate) fn receipt(
        binding: NativeProfileBinding,
        requested: &CredentialRef,
        profile: &CredentialRef,
        key: &str,
    ) -> PreparedNativeProfile {
        assert_eq!(&binding.connection.id, requested);
        assert_eq!(
            format!("{:x}", Sha256::digest(profile.as_str().as_bytes())),
            key
        );
        PreparedNativeProfile::from_binding(binding)
    }
}
