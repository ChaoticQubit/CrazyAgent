use super::sign_in_gap;
use crate::engine::owner::native_auth::NativeAuthPhase;

#[test]
fn a_finished_sign_in_without_an_account_reads_as_not_signed_in() {
    for phase in [
        NativeAuthPhase::UnauthenticatedObserved,
        NativeAuthPhase::Failed,
        NativeAuthPhase::Cancelled,
        NativeAuthPhase::Unavailable,
    ] {
        assert_eq!(
            sign_in_gap(phase),
            Some("master_authentication_unknown"),
            "{phase:?}"
        );
    }
    for phase in [
        NativeAuthPhase::Starting,
        NativeAuthPhase::AwaitingOwner,
        NativeAuthPhase::CheckingAccount,
        NativeAuthPhase::Cancelling,
        NativeAuthPhase::Uncertain,
    ] {
        assert_eq!(
            sign_in_gap(phase),
            Some("master_authentication_stale"),
            "{phase:?}"
        );
    }
    assert_eq!(sign_in_gap(NativeAuthPhase::AuthenticatedObserved), None);
}
