use super::NativeAuthPhase;

/// Names why the latest sign-in cannot authorize a master turn.
/// Input: that sign-in's phase. Output: None when authenticated, the
/// not-signed-in code when it finished without an account, else the stale code.
pub(super) fn sign_in_gap(phase: NativeAuthPhase) -> Option<&'static str> {
    match phase {
        NativeAuthPhase::AuthenticatedObserved => None,
        NativeAuthPhase::UnauthenticatedObserved
        | NativeAuthPhase::Failed
        | NativeAuthPhase::Cancelled
        | NativeAuthPhase::Unavailable => Some("master_authentication_unknown"),
        NativeAuthPhase::Starting
        | NativeAuthPhase::AwaitingOwner
        | NativeAuthPhase::CheckingAccount
        | NativeAuthPhase::Cancelling
        | NativeAuthPhase::Uncertain => Some("master_authentication_stale"),
    }
}
