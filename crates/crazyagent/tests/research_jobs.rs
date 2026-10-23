use crazyagent::research::jobs::{ResearchJobs, ResearchStatus};
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};

fn store() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("research.sqlite3");
    ResearchJobs::open(&path).unwrap();
    (temp, path)
}

#[test]
fn two_claimers_never_claim_the_same_job() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let mut queued = BTreeSet::new();
    for index in 0..12 {
        queued.insert(
            jobs.enqueue("project-a", &format!("kind-{index}"), "codex", true)
                .unwrap(),
        );
    }
    let claimers = 6;
    let barrier = Arc::new(Barrier::new(claimers));
    let handles: Vec<_> = (0..claimers)
        .map(|claimer| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let jobs = ResearchJobs::open(&path).unwrap();
                barrier.wait();
                let mut claimed = Vec::new();
                while let Some(job) = jobs
                    .claim("project-a", &format!("claimer-{claimer}"), 64)
                    .unwrap()
                {
                    assert_eq!(job.status, ResearchStatus::Running);
                    assert_eq!(job.attempts, 1);
                    claimed.push(job.id);
                }
                claimed
            })
        })
        .collect();
    let mut all = Vec::new();
    for handle in handles {
        all.extend(handle.join().unwrap());
    }
    let unique: BTreeSet<_> = all.iter().copied().collect();
    assert_eq!(all.len(), 12);
    assert_eq!(unique, queued);
}

#[test]
fn claim_respects_the_per_project_running_limit_and_project_scope() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    for kind in ["market", "competitors", "design"] {
        jobs.enqueue("project-a", kind, "codex", true).unwrap();
    }
    let other = jobs.enqueue("project-b", "market", "codex", true).unwrap();
    let first = jobs.claim("project-a", "runner", 2).unwrap().unwrap();
    let second = jobs.claim("project-a", "runner", 2).unwrap().unwrap();
    assert_ne!(first.id, second.id);
    assert!(jobs.claim("project-a", "runner", 2).unwrap().is_none());
    let claimed_b = jobs.claim("project-b", "runner", 2).unwrap().unwrap();
    assert_eq!(claimed_b.id, other);
    jobs.finish(first.id, "runner", "context/research/market.md")
        .unwrap();
    let third = jobs.claim("project-a", "runner", 2).unwrap().unwrap();
    assert_eq!(third.kind, "design");
    let listed = jobs.list("project-a").unwrap();
    assert_eq!(listed.len(), 3);
    let done = listed.iter().find(|job| job.id == first.id).unwrap();
    assert_eq!(done.status, ResearchStatus::Done);
    assert_eq!(done.status.label(), "Done");
    assert_eq!(
        done.report_path.as_deref(),
        Some("context/research/market.md")
    );
    assert!(done.finished_ms.unwrap() >= done.started_ms.unwrap());
}

#[test]
fn failures_retry_until_the_attempt_limit_then_stay_failed() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let id = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    for attempt in 1..=3 {
        let job = jobs.claim("project-a", "runner", 1).unwrap().unwrap();
        assert_eq!(job.id, id);
        assert_eq!(job.attempts, attempt);
        let status = jobs.fail(id, "runner", "agent exited early", 3).unwrap();
        if attempt < 3 {
            assert_eq!(status, ResearchStatus::Queued);
        } else {
            assert_eq!(status, ResearchStatus::Failed);
        }
    }
    assert!(jobs.claim("project-a", "runner", 1).unwrap().is_none());
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status.label(), "Failed");
    assert_eq!(job.error.as_deref(), Some("agent exited early"));
}

#[test]
fn a_claimer_cannot_finish_a_job_it_does_not_hold() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let id = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    jobs.claim("project-a", "runner-a", 1).unwrap().unwrap();
    assert!(
        jobs.finish(id, "runner-b", "context/research/market.md")
            .is_err()
    );
    assert!(jobs.fail(id, "runner-b", "x", 3).is_err());
    assert_eq!(
        jobs.get(id).unwrap().unwrap().status,
        ResearchStatus::Running
    );
}

#[test]
fn cancel_stops_queued_jobs_now_and_flags_running_jobs() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let running = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    let queued = jobs.enqueue("project-a", "design", "codex", true).unwrap();
    jobs.claim("project-a", "runner", 1).unwrap().unwrap();
    assert!(jobs.request_cancel(queued).unwrap());
    assert_eq!(
        jobs.get(queued).unwrap().unwrap().status,
        ResearchStatus::Cancelled
    );
    assert!(!jobs.heartbeat(running, "runner", 10).unwrap());
    assert!(jobs.request_cancel(running).unwrap());
    assert!(jobs.heartbeat(running, "runner", 20).unwrap());
    jobs.cancelled(running, "runner").unwrap();
    let job = jobs.get(running).unwrap().unwrap();
    assert_eq!(job.status, ResearchStatus::Cancelled);
    assert_eq!(job.output_bytes, 20);
    assert!(!jobs.request_cancel(running).unwrap());
}

#[test]
fn jobs_without_a_pool_wait_and_resume_when_one_is_assigned() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let id = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    let job = jobs.claim("project-a", "runner", 1).unwrap().unwrap();
    jobs.needs_pool(job.id, "runner").unwrap();
    let waiting = jobs.get(id).unwrap().unwrap();
    assert_eq!(waiting.status, ResearchStatus::NeedsPool);
    assert_eq!(waiting.status.label(), "Needs a pool");
    assert_eq!(waiting.attempts, 0);
    assert!(jobs.claim("project-a", "runner", 1).unwrap().is_none());
    assert_eq!(jobs.resume_waiting("project-a").unwrap(), 1);
    assert_eq!(
        jobs.claim("project-a", "runner", 1).unwrap().unwrap().id,
        id
    );
}

#[test]
fn stale_running_jobs_from_a_dead_runner_are_requeued() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let id = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    jobs.claim("project-a", "dead-runner", 1).unwrap().unwrap();
    assert_eq!(jobs.recover_stale(60_000).unwrap(), 0);
    assert_eq!(jobs.recover_stale(0).unwrap(), 1);
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status, ResearchStatus::Queued);
    assert!(jobs.finish(id, "dead-runner", "x").is_err());
}

#[test]
fn enqueue_bounds_its_inputs() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    assert!(jobs.enqueue("", "market", "codex", true).is_err());
    assert!(
        jobs.enqueue("project-a", "../escape", "codex", true)
            .is_err()
    );
    assert!(jobs.enqueue("project-a", "Market", "codex", true).is_err());
    assert!(
        jobs.enqueue("project-a", &"k".repeat(65), "codex", true)
            .is_err()
    );
    assert!(jobs.enqueue("project-a", "market", "", true).is_err());
}

#[test]
fn parked_jobs_carry_a_plain_reason_and_resume_clears_it() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let id = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    jobs.enqueue("project-b", "market", "codex", true).unwrap();
    assert_eq!(
        jobs.projects_with_work().unwrap(),
        ["project-a", "project-b"]
    );
    assert_eq!(jobs.park_waiting("project-a", None).unwrap(), 1);
    assert_eq!(jobs.park_waiting("project-a", None).unwrap(), 0);
    assert_eq!(
        jobs.park_waiting("project-a", Some("No usable account"))
            .unwrap(),
        1
    );
    let parked = jobs.get(id).unwrap().unwrap();
    assert_eq!(parked.status, ResearchStatus::NeedsPool);
    assert_eq!(parked.error.as_deref(), Some("No usable account"));
    assert_eq!(
        jobs.projects_with_work().unwrap(),
        ["project-a", "project-b"]
    );
    assert_eq!(jobs.resume_waiting("project-a").unwrap(), 1);
    let resumed = jobs.get(id).unwrap().unwrap();
    assert_eq!(resumed.status, ResearchStatus::Queued);
    assert_eq!(resumed.error, None);
}

#[test]
fn an_interrupted_job_returns_to_the_queue_without_spending_an_attempt() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let id = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    jobs.claim("project-a", "runner", 1).unwrap().unwrap();
    assert!(jobs.release(id, "other").is_err());
    jobs.release(id, "runner").unwrap();
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status, ResearchStatus::Queued);
    assert_eq!(job.attempts, 0);
    assert_eq!(
        jobs.claim("project-a", "runner", 1)
            .unwrap()
            .unwrap()
            .attempts,
        1
    );
    assert!(jobs.projects_with_work().unwrap().is_empty());
}
