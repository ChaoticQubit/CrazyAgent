use super::{
    failed_turn_code, instruction_file_text, rcli_failure_observation, require_rcli_connection,
    timed_out, turn_instruction_text,
};
use crate::agent_home::Given;
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::engine::EngineError;
use crate::master_session::MasterReason;
use crate::protocol::CredentialRef;
use crate::rcli_host::RcliProviderFailure;
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

/// PA-5 (DEC-141): an RCLI master's instruction file holds its startup instruction and then
/// the owner's app and project instructions, as a Claude master's prompt file does. Past the
/// RCLI host's file limit only the owner's part is cut, at a character boundary, and it ends
/// with the cut note; the startup instruction always stays whole.
#[test]
fn an_rcli_instruction_file_holds_the_startup_instruction_and_the_owner_text() {
    let startup = "## Project\n\nThe brief.\n";
    let owner = Given {
        text: "## App instructions\n\nBe brief.\n".into(),
        files: Vec::new(),
        section_bytes: 0,
    };
    assert_eq!(
        instruction_file_text(startup, owner.clone()),
        format!("{startup}\n{}", owner.text)
    );
    assert_eq!(instruction_file_text(startup, Given::default()), startup);
    let largest = "s".repeat(crate::launch_context::MASTER_INSTRUCTIONS_BYTES_MAX);
    let section = "f".repeat(crate::instruction_files::SECTION_BYTES_MAX);
    let long = Given {
        text: format!(
            "{}{section}",
            "\u{e9}".repeat(crate::agent_home::GIVEN_BYTES_MAX / 2)
        ),
        files: Vec::new(),
        section_bytes: section.len(),
    };
    let cut = instruction_file_text(&largest, long);
    assert!(cut.len() <= crate::rcli_host::INSTRUCTIONS_BYTES_MAX);
    assert!(cut.starts_with(&format!("{largest}\n\u{e9}")));
    assert!(cut.contains("[CrazyAgent cut the app and project instructions here"));
    assert!(
        cut.ends_with(&section),
        "the instruction files section stays whole"
    );
}

/// DEC-141 PA-18 (PALOW-L1): an RCLI master turn after the owner changed the instructions is
/// given the new text and not the old, after the startup instruction, since every turn reads
/// the owner's text again.
#[test]
fn an_rcli_master_turn_after_an_instruction_change_is_given_the_new_text() {
    let data = tempfile::tempdir().unwrap();
    let project = crate::agent_home::AgentProject {
        data: data.path().canonicalize().unwrap(),
        id: crate::protocol::ProjectId::new("rcli").unwrap(),
        folder: None,
        mode: crate::instruction_files::Mode::Both,
        repeated: crate::agent_home::Repeated::LeaveOut,
    };
    let startup = "## Project\n\nThe brief.\n";

    crate::agent_home::save_app(&project.data, "OWNER-ALPHA-41\n").unwrap();
    let first = turn_instruction_text(startup, &project).unwrap();
    crate::agent_home::save_app(&project.data, "OWNER-BETA-42\n").unwrap();
    let second = turn_instruction_text(startup, &project).unwrap();

    assert!(
        first.starts_with(startup) && first.contains("OWNER-ALPHA-41"),
        "{first}"
    );
    assert!(second.starts_with(startup), "{second}");
    assert!(
        second.contains("OWNER-BETA-42") && !second.contains("OWNER-ALPHA-41"),
        "the next turn keeps the old text: {second}"
    );
}

/// PA-5 (DEC-141): the bundled RCLI host takes the instruction file size the engine writes
/// for every master, so a startup instruction and owner text that fit a Claude master fit
/// an RCLI master the same way.
#[test]
fn the_rcli_host_takes_the_instruction_file_size_every_master_gets() {
    assert_eq!(
        crate::rcli_host::INSTRUCTIONS_BYTES_MAX as u64,
        crate::engine::claude_host::INSTRUCTIONS_BYTES_MAX
    );
    let host = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/rcli/host.runtime.ts"),
    )
    .unwrap();
    let declared: Vec<usize> = host
        .lines()
        .filter_map(|line| line.strip_prefix("const INSTRUCTIONS_BYTES_MAX = "))
        .map(|value| {
            value
                .trim_end_matches(';')
                .replace('_', "")
                .parse()
                .unwrap()
        })
        .collect();
    assert_eq!(declared, [crate::rcli_host::INSTRUCTIONS_BYTES_MAX]);
}

/// PA-17 (DEC-141): every master stops for the same reason when the same thing happens,
/// whatever harness runs it. Each row is one stop the app shows and the code each harness
/// adapter ends the turn with, in the order claude-code, codex, rcli; None marks a stop the
/// harness cannot reach, and the row says why.
#[test]
fn every_master_harness_stops_for_the_same_reason() {
    let native =
        |code: &str| super::super::master_runtime::reason_for_error(&EngineError(code.into()));
    let same = |code| [Some(code); 3];
    let rows: [(&str, MasterReason, [Option<&str>; 3]); 19] = [
        (
            "the harness did not start in the start bound",
            MasterReason::RuntimeUnavailable,
            [
                Some(crate::engine::claude_host::start_end_code(false)),
                Some("codex_host_startup_timeout"),
                Some("rcli_master_startup_deadline"),
            ],
        ),
        (
            "the run clock ended before the harness started",
            MasterReason::TimeLimit,
            [
                Some(crate::engine::claude_host::start_end_code(true)),
                Some("codex_host_deadline"),
                Some("rcli_master_turn_deadline"),
            ],
        ),
        (
            "the run clock ended",
            MasterReason::TimeLimit,
            [
                Some("claude_host_deadline"),
                Some("codex_host_deadline"),
                Some("rcli_master_turn_deadline"),
            ],
        ),
        (
            "the owner's time limit ended it",
            MasterReason::TimeLimit,
            same("master_time_limit_10"),
        ),
        (
            "its output was too large",
            MasterReason::OutputLimit,
            [
                Some("claude_host_output_limit"),
                Some("codex_host_output_limit"),
                Some("rcli_master_output_limit"),
            ],
        ),
        (
            "it gave no reply",
            MasterReason::NoReply,
            [
                Some("master_empty_reply"),
                Some("master_empty_reply"),
                Some("rcli_master_empty_reply"),
            ],
        ),
        (
            "its capture is incomplete",
            MasterReason::CaptureIncomplete,
            [
                Some("master_capture_incomplete"),
                Some("codex_host_capture_incomplete"),
                Some("rcli_master_capture_incomplete"),
            ],
        ),
        (
            "it wanted the owner's approval or answer mid-turn, which no master waits for; Claude Code runs with bypassPermissions or no tools and never asks",
            MasterReason::ApprovalRequired,
            [
                None,
                Some("codex_host_approval_required"),
                Some("rcli_master_input_required"),
            ],
        ),
        (
            "the provider rate-limited the account",
            MasterReason::RateLimited,
            same("gateway_rate_limited"),
        ),
        (
            "the account's quota or credit ran out",
            MasterReason::QuotaExhausted,
            same("gateway_quota_exhausted"),
        ),
        (
            "the provider rejected the key or sign-in",
            MasterReason::InvalidAuthentication,
            same("gateway_invalid_authentication"),
        ),
        (
            "the account is cooling down",
            MasterReason::CooldownActive,
            same("gateway_cooldown"),
        ),
        (
            "the instructions do not fit",
            MasterReason::InstructionsTooLarge,
            same("master_instructions_too_large"),
        ),
        (
            "a Git setting in the repository runs commands",
            MasterReason::RepositoryConfigRefused,
            same("repository_config_refused:core.fsmonitor"),
        ),
        (
            "the project has no repository",
            MasterReason::RepositoryMissing,
            same("master_repository_missing"),
        ),
        (
            "the account profile is busy",
            MasterReason::ProfileBusy,
            same("native_profile_in_use"),
        ),
        (
            "the master's settings scope changed",
            MasterReason::ScopeRevoked,
            same("master_runtime_scope_revoked"),
        ),
        (
            "the master runtime host is unavailable",
            MasterReason::RuntimeUnavailable,
            same("master_runtime_host_unavailable"),
        ),
        (
            "the account has no configured profile yet",
            MasterReason::RuntimeUnavailable,
            same("rcli_profile_configuration_required"),
        ),
    ];
    for (stop, expected, codes) in rows {
        for (harness, code) in ["claude-code", "codex", "rcli"].into_iter().zip(codes) {
            let Some(code) = code else {
                continue;
            };
            let got = match harness {
                "rcli" => reason(code),
                _ => native(code),
            };
            assert_eq!(got, expected, "{stop}: {harness} ended with {code}");
        }
    }
}

/// PA-6 (DEC-091): a failed rcli turn that names a provider failure stops with the reason a
/// native master's provider failure gives, so the pool pauses that account and moves on;
/// a failed turn that names none stays the rcli execution failure.
#[test]
fn an_rcli_provider_failure_stops_the_turn_like_a_native_one() {
    let native =
        |code: &str| super::super::master_runtime::reason_for_error(&EngineError(code.into()));
    for (failure, expected, cause) in [
        (
            RcliProviderFailure::RateLimited,
            MasterReason::RateLimited,
            "gateway_rate_limited",
        ),
        (
            RcliProviderFailure::QuotaExhausted,
            MasterReason::QuotaExhausted,
            "gateway_quota_exhausted",
        ),
        (
            RcliProviderFailure::InvalidAuthentication,
            MasterReason::InvalidAuthentication,
            "gateway_invalid_authentication",
        ),
    ] {
        let code = failed_turn_code(Some(failure));
        assert_eq!(code, cause);
        assert_eq!(reason(code), expected);
        assert_eq!(native(code), expected);
    }
    assert_eq!(failed_turn_code(None), "rcli_master_execution_failed");
    assert_eq!(
        reason(failed_turn_code(None)),
        MasterReason::RuntimeUnavailable
    );
}
