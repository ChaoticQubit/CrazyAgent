#[cfg(unix)]
use super::integrity::{self, Reported};
use super::runner::CheckExecution;
use super::{AdmittedCheck, CheckKind, NetworkMode, ReportFormat};
use crate::sandbox::toolchains::Toolchain;
use std::collections::HashSet;
use trackwright_core::results::{self, ResultFormat, ResultRow};
use trackwright_core::runs::{self, NewResult, ResultOutcome, StopReason, ToolchainFamily};
use trackwright_core::tests_model::{CaseKind, RESULTS_PER_RUN_MAX};

/// The reason recorded for a JUnit or Allure check that left no report file.
pub const REPORT_MISSING: &str = "report_missing";
/// The reason recorded for a report the parser refused.
pub const REPORT_REFUSED: &str = "report_refused";
/// The reason recorded for report files that name one result twice between them.
pub const RESULTS_REPEATED: &str = "results_repeated";
/// The reason recorded for report files holding more results than one run keeps.
pub const RESULTS_TOO_MANY: &str = "results_too_many";

/// How one execution ends as a trackwright run: stopped by a cancel or its timeout, never
/// started (with the runner's code), its report unreadable (with why), a build that failed
/// before any test ran, or completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    Stopped(StopReason),
    NotStarted(String),
    ReportUnreadable(String),
    BuildFailed,
    Completed,
}

/// One execution as the recorder writes it: the results to record (none unless the run
/// completed), whether the parser saw a build failure, and the ending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub results: Vec<NewResult>,
    pub build_failed: bool,
    pub ending: Ending,
}

#[cfg(unix)]
impl Bound {
    /// What the check reported, as `integrity::assess` reads it. Output: the names with
    /// their outcomes, the build failure and the exit code.
    pub fn reported<'a>(&'a self, execution: &CheckExecution) -> Reported<'a> {
        let results = self
            .results
            .iter()
            .map(|result| (result.name.as_str(), outcome(result.outcome)))
            .collect();
        Reported {
            results,
            build_failed: self.build_failed,
            exit: execution.exit,
        }
    }
}

/// Binds one execution to the run it records: a cancel or timeout stops the run, a runner
/// refusal means it never started, a report problem or a refused report makes it
/// unreadable, a build failure with no result is a failed build, and otherwise the results
/// are the parsed report (libtest reads standard error then standard output; an exit-code
/// check records one result named after the check). A holdout run keeps no result message
/// and no artifact, so nothing its files printed is stored and a result linked to an
/// unprotected case shows nothing more than its outcome. Inputs: the admitted check and the
/// execution. Output: the binding.
pub fn bind(check: &AdmittedCheck, execution: &CheckExecution) -> Bound {
    let stopped = |ending| Bound {
        results: Vec::new(),
        build_failed: false,
        ending,
    };
    if execution.cancelled {
        return stopped(Ending::Stopped(StopReason::Cancelled));
    }
    if execution.timed_out {
        return stopped(Ending::Stopped(StopReason::TimedOut));
    }
    if let Some(code) = &execution.unavailable {
        return stopped(Ending::NotStarted(code.clone()));
    }
    if let Some(problem) = execution.report_problem {
        return stopped(Ending::ReportUnreadable(problem.into()));
    }
    let parsed = match parsed(check, execution) {
        Ok(parsed) => parsed,
        Err(reason) => return stopped(Ending::ReportUnreadable(reason.into())),
    };
    let (rows, build_failed) = parsed;
    if build_failed && rows.is_empty() {
        return Bound {
            results: Vec::new(),
            build_failed,
            ending: Ending::BuildFailed,
        };
    }
    let holdout = check.holdout.is_some();
    let results: Vec<NewResult> = rows
        .into_iter()
        .map(|row| NewResult {
            name: row.name,
            outcome: row.outcome,
            duration_ms: row
                .duration_ms
                .map(|ms| u32::try_from(ms).unwrap_or(u32::MAX)),
            message: if holdout { None } else { row.message },
            artifact: None,
        })
        .collect();
    assert!(results.len() <= RESULTS_PER_RUN_MAX);
    Bound {
        results,
        build_failed,
        ending: Ending::Completed,
    }
}

fn parsed(
    check: &AdmittedCheck,
    execution: &CheckExecution,
) -> Result<(Vec<ResultRow>, bool), &'static str> {
    let format = match check.spec.format {
        ReportFormat::ExitCode => {
            let outcome = if execution.exit == Some(0) {
                ResultOutcome::Passed
            } else {
                ResultOutcome::Failed
            };
            let row = ResultRow {
                name: check.spec.id.clone(),
                outcome,
                duration_ms: Some(execution.duration_ms),
                message: None,
            };
            return Ok((vec![row], false));
        }
        ReportFormat::Libtest => {
            let mut text = execution.stderr_tail.clone();
            text.extend_from_slice(&execution.stdout_tail);
            let parsed =
                results::parse(ResultFormat::Libtest, &text).map_err(|_| REPORT_REFUSED)?;
            return Ok((parsed.rows, parsed.build_failed));
        }
        ReportFormat::Junit => ResultFormat::Junit,
        ReportFormat::Allure => ResultFormat::Allure,
    };
    if execution.report.is_empty() {
        return Err(REPORT_MISSING);
    }
    let mut rows = Vec::new();
    let mut names = HashSet::new();
    let mut build_failed = false;
    for file in &execution.report {
        let parsed = results::parse(format, &file.bytes).map_err(|_| REPORT_REFUSED)?;
        build_failed |= parsed.build_failed;
        for row in parsed.rows {
            if !names.insert(row.name.clone()) {
                return Err(RESULTS_REPEATED);
            }
            rows.push(row);
            if rows.len() > RESULTS_PER_RUN_MAX {
                return Err(RESULTS_TOO_MANY);
            }
        }
    }
    Ok((rows, build_failed))
}

#[cfg(unix)]
fn outcome(outcome: ResultOutcome) -> integrity::ResultOutcome {
    match outcome {
        ResultOutcome::Passed => integrity::ResultOutcome::Passed,
        ResultOutcome::Failed => integrity::ResultOutcome::Failed,
        ResultOutcome::Skipped => integrity::ResultOutcome::Skipped,
        ResultOutcome::Errored => integrity::ResultOutcome::Errored,
        ResultOutcome::NotRun => integrity::ResultOutcome::NotRun,
    }
}

/// The test case kind a check's runs are recorded under. Input: the check kind. Output:
/// the trackwright kind of the same name.
pub fn case_kind(kind: CheckKind) -> CaseKind {
    match kind {
        CheckKind::Unit => CaseKind::Unit,
        CheckKind::Integration => CaseKind::Integration,
        CheckKind::E2e => CaseKind::E2e,
        CheckKind::Smoke => CaseKind::Smoke,
        CheckKind::Scenario => CaseKind::Scenario,
    }
}

/// The toolchain family a check's runs record. Input: the check's toolchain. Output: the
/// trackwright family, `none` for a check that names none.
pub fn family(toolchain: Option<Toolchain>) -> ToolchainFamily {
    match toolchain {
        None => ToolchainFamily::None,
        Some(Toolchain::Rust) => ToolchainFamily::Rust,
        Some(Toolchain::Node) => ToolchainFamily::Node,
        Some(Toolchain::Python) => ToolchainFamily::Python,
    }
}

/// The network a run records. Input: the network the check ran under. Output: the
/// trackwright mode of the same name.
pub fn network(mode: NetworkMode) -> runs::NetworkMode {
    match mode {
        NetworkMode::Off => runs::NetworkMode::Off,
        NetworkMode::Localhost => runs::NetworkMode::Localhost,
        NetworkMode::Remotes => runs::NetworkMode::Remotes,
    }
}
