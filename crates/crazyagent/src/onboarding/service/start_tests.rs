use super::*;
use crate::harnesses::launch::LaunchError;

const PRIVATE: &str = "/Users/someone/secret-project token=sk-private";

#[test]
fn every_start_failure_has_a_plain_cause_code_without_its_text() {
    let io = || std::io::Error::new(std::io::ErrorKind::PermissionDenied, PRIVATE);
    let cases = [
        (RunError::Invalid(RUN_NAME_USED.into()), "run_name_used"),
        (RunError::Invalid(PRIVATE.into()), "run_refused"),
        (RunError::Jobs(JobError::Invalid("bad job")), "job_refused"),
        (RunError::Jobs(JobError::NotHeld), "job_not_held"),
        (
            RunError::Jobs(JobError::Storage(rusqlite::Error::InvalidQuery)),
            "job_storage",
        ),
        (
            RunError::Sandbox(SandboxError::Invalid(PRIVATE.into())),
            "sandbox_refused",
        ),
        (
            RunError::Sandbox(SandboxError::Unavailable(PRIVATE.into())),
            "sandbox_unavailable",
        ),
        (
            RunError::Sandbox(SandboxError::Io(io())),
            "sandbox_io_permission_denied",
        ),
        (
            RunError::Context(LaunchError(PRIVATE.into())),
            "context_refused",
        ),
        (RunError::Account(PRIVATE.into()), "account_unavailable"),
        (RunError::Busy, "account_busy"),
        (
            RunError::Uncertain("warden_unconfirmed"),
            "cleanup_uncertain",
        ),
        (RunError::Io(io()), "io_permission_denied"),
    ];
    for (error, code) in cases {
        assert_eq!(start_cause(&error), code, "{error:?}");
    }
}

#[test]
fn a_step_that_cannot_start_keeps_its_sentence_and_logs_one_plain_line() {
    let error = RunError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, PRIVATE));
    assert_eq!(
        not_started(Kind::Graph, &error),
        Step::failed("Graph build could not start (io_entity_not_found)")
    );
    assert_eq!(
        not_started(Kind::Mapping, &RunError::Invalid(RUN_NAME_USED.into())),
        Step::failed("Mapping agent could not start (run_name_used)")
    );
    assert_eq!(
        start_diagnostic(Kind::Graph, "io_entity_not_found"),
        "onboarding_graph_not_started cause=io_entity_not_found"
    );
    assert_eq!(
        start_diagnostic(Kind::Mapping, "run_name_used"),
        "onboarding_mapping_not_started cause=run_name_used"
    );
}
