use super::{rcli_failure_observation, require_rcli_connection, timed_out};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::engine::EngineError;
use crate::master_session::MasterReason;
use crate::protocol::CredentialRef;
use crate::storage::MasterRuntimeObservation;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn connection() -> ModelConnection {
    ModelConnection {
        id: reference("configured-rcli"),
        provider: reference("configured-provider"),
        harness: reference("rcli"),
        protocol: reference("openai-chat"),
        account_ref: reference("selected-account"),
        endpoint: ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into(),
        },
        model: ModelReference {
            id: "configured-model".into(),
            author: reference("configured-author"),
        },
        auth: ModelAuthentication::None {},
    }
}

fn reason(code: &str) -> MasterReason {
    match rcli_failure_observation(&EngineError(code.into())) {
        MasterRuntimeObservation::Failed { reason, .. } => reason,
        _ => panic!("an ordinary failure changed its runtime observation category"),
    }
}

#[test]
fn configured_rcli_connections_preserve_exact_selected_identity() {
    for protocol in ["openai-chat", "openai-responses", "anthropic-messages"] {
        for auth in [
            ModelAuthentication::None {},
            ModelAuthentication::ApiKey {
                credential_ref: reference("selected-credential"),
            },
        ] {
            let mut configured = connection();
            configured.protocol = reference(protocol);
            configured.auth = auth;
            let before = configured.clone();
            require_rcli_connection(&configured).unwrap();
            assert_eq!(configured, before);
        }
    }
}

#[test]
fn rcli_subscription_bridges_remain_unsupported() {
    for auth in [
        ModelAuthentication::Oauth {
            credential_ref: reference("subscription-credential"),
        },
        ModelAuthentication::NativeManaged {
            profile_ref: reference("subscription-profile"),
        },
    ] {
        let mut configured = connection();
        configured.auth = auth;
        assert_eq!(
            require_rcli_connection(&configured).unwrap_err().0,
            "master_runtime_backend_unsupported"
        );
    }
}

#[test]
fn rcli_driver_requires_the_existing_http_harness_contract() {
    let mut configured = connection();
    configured.harness = reference("codex");
    assert_eq!(
        require_rcli_connection(&configured).unwrap_err().0,
        "master_runtime_backend_unsupported"
    );
    configured = connection();
    configured.protocol = reference("native");
    assert_eq!(
        require_rcli_connection(&configured).unwrap_err().0,
        "master_runtime_backend_unsupported"
    );
    configured = connection();
    configured.endpoint = ModelEndpoint::Native {};
    assert_eq!(
        require_rcli_connection(&configured).unwrap_err().0,
        "master_runtime_backend_unsupported"
    );
}

#[test]
fn rcli_cleanup_uncertainty_never_becomes_a_confirmed_terminal_failure() {
    for code in [
        "rcli_master_cleanup_incomplete",
        "rcli_master_output_limit;rcli_master_cleanup_incomplete",
        "rcli_master_input_required;rcli_master_cleanup_incomplete",
        "master_runtime_cleanup_uncertain:rcli_master_wait_failed",
        "rcli_master_wait_failed",
    ] {
        assert!(
            matches!(
                rcli_failure_observation(&EngineError(code.into())),
                MasterRuntimeObservation::Disconnected
            ),
            "{code}"
        );
    }
}

#[test]
fn rcli_failures_keep_capture_budget_and_approval_reasons() {
    for (code, expected) in [
        ("rcli_master_output_limit", MasterReason::OutputLimit),
        (
            "rcli_master_capture_incomplete",
            MasterReason::CaptureIncomplete,
        ),
        ("rcli_master_empty_reply", MasterReason::NoReply),
        ("rcli_master_message_order", MasterReason::CaptureIncomplete),
        ("rcli_master_turn_scope", MasterReason::CaptureIncomplete),
        (
            "rcli_master_truncated_event",
            MasterReason::CaptureIncomplete,
        ),
        ("rcli_master_input_required", MasterReason::ApprovalRequired),
        ("rcli_master_turn_deadline", MasterReason::TimeLimit),
        ("master_time_limit_10", MasterReason::TimeLimit),
        ("master_time_limit_30", MasterReason::TimeLimit),
        ("master_time_limit_60", MasterReason::TimeLimit),
        (
            "master_runtime_budget_exhausted",
            MasterReason::BudgetExhausted,
        ),
        (
            "rcli_master_startup_deadline",
            MasterReason::RuntimeUnavailable,
        ),
        ("rcli_master_spawn_failed", MasterReason::RuntimeUnavailable),
        (
            "rcli_master_artifact_changed",
            MasterReason::RuntimeUnavailable,
        ),
        (
            "rcli_master_profile_changed",
            MasterReason::ConnectionChanged,
        ),
        (
            "rcli_master_profile_mismatch",
            MasterReason::ConnectionChanged,
        ),
        (
            "native_profile_binding_conflict",
            MasterReason::ConnectionChanged,
        ),
        (
            "native_profile_identity_changed",
            MasterReason::ConnectionChanged,
        ),
        ("owner_runtime_stale", MasterReason::ScopeRevoked),
        ("master_runtime_scope_revoked", MasterReason::ScopeRevoked),
        (
            "master_runtime_backend_unsupported",
            MasterReason::UnsupportedCapability,
        ),
    ] {
        assert_eq!(reason(code), expected, "{code}");
    }
}

/// An rcli master turn its run clock ended names the owner's master time limit, as a Claude
/// or Codex one does (OB-5d): the stored cause is `master_time_limit_<minutes>` with the
/// reason time_limit. With no limit the code is kept, and no other code is renamed.
#[test]
fn an_rcli_turn_its_clock_ended_names_the_time_limit_it_ran_under() {
    use crate::master_time_limit::TimeLimit;

    let clock = || EngineError("rcli_master_turn_deadline".into());
    for (limit, minutes) in [
        (TimeLimit::Minutes10, 10),
        (TimeLimit::Minutes30, 30),
        (TimeLimit::Minutes60, 60),
    ] {
        let named = timed_out(clock(), limit);
        assert_eq!(named.0, format!("master_time_limit_{minutes}"));
        match rcli_failure_observation(&named) {
            MasterRuntimeObservation::Failed { reason, cause } => {
                assert_eq!(reason, MasterReason::TimeLimit);
                assert_eq!(cause, named.0);
            }
            _ => panic!("a turn its clock ended is a failed turn"),
        }
    }
    let kept = timed_out(clock(), TimeLimit::NoLimit);
    assert_eq!(kept.0, "rcli_master_turn_deadline");
    assert_eq!(reason(&kept.0), MasterReason::TimeLimit);
    for code in [
        "master_runtime_budget_exhausted",
        "rcli_master_startup_deadline",
        "rcli_master_execution_failed",
    ] {
        let other = timed_out(EngineError(code.into()), TimeLimit::Minutes10);
        assert_eq!(other.0, code);
        assert_ne!(reason(code), MasterReason::TimeLimit, "{code}");
    }
    assert_eq!(reason("master_time_limit_600"), MasterReason::Unknown);
}

#[test]
fn real_exit_categories_never_qualify_a_provider_reply_or_authentication() {
    for code in [
        "rcli_master_process_exited",
        "rcli_master_control_rejected",
        "rcli_master_execution_failed",
        "rcli_master_closed",
    ] {
        assert_eq!(reason(code), MasterReason::RuntimeUnavailable, "{code}");
    }
    assert_eq!(reason("unrecognized_failure"), MasterReason::Unknown);
}

#[test]
fn retirement_uncertainty_takes_precedence_over_budget_and_scope_reasons() {
    for code in [
        "master_runtime_budget_exhausted;rcli_master_cleanup_incomplete",
        "owner_runtime_stale;rcli_master_cleanup_incomplete",
        "rcli_master_process_exited;rcli_master_cleanup_incomplete",
    ] {
        assert!(matches!(
            rcli_failure_observation(&EngineError(code.into())),
            MasterRuntimeObservation::Disconnected
        ));
    }
}
