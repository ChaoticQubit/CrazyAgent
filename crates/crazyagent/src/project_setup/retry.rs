use super::*;

/// The code a retry gets where this engine runs no research ([`retryable`]).
pub const RESEARCH_UNAVAILABLE_HERE: &str = "research_unavailable_here";

/// Whether a job in this status can run again on this engine's system: a failed or stopped
/// job, where the engine runs research at all ([`crate::research::runner::confined_unavailable`]),
/// so no client offers a Retry that could only fail again. Input: the status. Output: the
/// answer.
pub fn retryable(status: ResearchStatus) -> bool {
    matches!(status, ResearchStatus::Failed | ResearchStatus::Cancelled)
        && crate::research::runner::confined_unavailable(std::env::consts::OS).is_none()
}

/// Queues a failed or cancelled research job again with a fresh attempt budget. A job whose
/// kind was turned off turns it back on, so the next setup edit does not cancel it again.
/// Takes the transaction, configuration, project and job id. Returns Applied,
/// `research_job_missing`, `project_setup_missing`, `setup_unconfirmed` for a kind turned off in
/// an unconfirmed setup review, `research_job_not_retryable` for a job that is not stopped, or
/// [`RESEARCH_UNAVAILABLE_HERE`] where this engine runs no research.
pub(super) fn retry_research(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project_id: &ProjectId,
    id: i64,
) -> Result<Outcome, EngineError> {
    let Some(job) = project_job(tx, project_id, id)? else {
        return Ok(rejected("research_job_missing"));
    };
    if !matches!(
        job.status,
        ResearchStatus::Failed | ResearchStatus::Cancelled
    ) {
        return Ok(rejected("research_job_not_retryable"));
    }
    if !retryable(job.status) {
        return Ok(rejected(RESEARCH_UNAVAILABLE_HERE));
    }
    let Some(mut stored) = load_setup(tx, project_id)? else {
        return Ok(rejected("project_setup_missing"));
    };
    if !stored.research.contains(&job.kind) && unconfirmed(tx, project_id, &stored)? {
        return Ok(rejected("setup_unconfirmed"));
    }
    let status = effective(ResearchStatus::Queued, project_has_pool(config, project_id));
    let changed = tx
        .execute(
            &format!(
                "UPDATE research_jobs SET status=?2, attempts=0, claimer=NULL, error=NULL,
                cancel_requested=0, output_bytes=0, started_ms=NULL, heartbeat_ms=NULL,
                finished_ms=NULL, {RUN_AGAIN} WHERE id=?1 AND status IN ('failed','cancelled')"
            ),
            params![id, status.stored()],
        )
        .map_err(error)?;
    if changed != 1 {
        return Ok(rejected("research_job_not_retryable"));
    }
    turn_on(tx, &mut stored, job.kind)?;
    Ok(Outcome::Applied)
}
