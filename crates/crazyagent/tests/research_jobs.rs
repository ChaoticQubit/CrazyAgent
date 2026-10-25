#![cfg(unix)]

use crazyagent::research::jobs::{ResearchJob, ResearchJobs, ResearchStatus};
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
    jobs.finish(&first, "context/research/market.md").unwrap();
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
        let status = jobs.fail(&job, "agent exited early", 3).unwrap();
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
    let held = jobs.claim("project-a", "runner-a", 1).unwrap().unwrap();
    let other = ResearchJob {
        claimer: Some("runner-b".into()),
        ..held.clone()
    };
    let earlier = ResearchJob {
        attempts: held.attempts - 1,
        ..held.clone()
    };
    for stranger in [&other, &earlier] {
        assert!(jobs.finish(stranger, "context/research/market.md").is_err());
        assert!(jobs.fail(stranger, "x", 3).is_err());
    }
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
    let held = jobs.claim("project-a", "runner", 1).unwrap().unwrap();
    assert_eq!(held.id, running);
    assert!(jobs.request_cancel(queued).unwrap());
    assert_eq!(
        jobs.get(queued).unwrap().unwrap().status,
        ResearchStatus::Cancelled
    );
    assert!(!jobs.heartbeat(&held, 10).unwrap());
    assert!(jobs.request_cancel(running).unwrap());
    assert!(jobs.heartbeat(&held, 20).unwrap());
    jobs.cancelled(&held).unwrap();
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
    assert_eq!(jobs.needs_pool(&job).unwrap(), ResearchStatus::NeedsPool);
    let waiting = jobs.get(id).unwrap().unwrap();
    assert_eq!(waiting.status, ResearchStatus::NeedsPool);
    assert_eq!(waiting.status.label(), "Needs a pool");
    assert_eq!(waiting.attempts, 0);
    assert!(jobs.claim("project-a", "runner", 1).unwrap().is_none());
    assert_eq!(jobs.resume_waiting("project-a", "codex").unwrap(), 1);
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
    let dead = jobs.claim("project-a", "dead-runner", 1).unwrap().unwrap();
    assert_eq!(jobs.recover_stale(60_000).unwrap(), 0);
    assert_eq!(jobs.recover_stale(0).unwrap(), 1);
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status, ResearchStatus::Queued);
    assert!(jobs.finish(&dead, "x").is_err());
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
        jobs.work().unwrap(),
        [
            ("project-a".to_owned(), "codex".to_owned()),
            ("project-b".to_owned(), "codex".to_owned())
        ]
    );
    assert_eq!(jobs.park_waiting("project-a", "codex", None).unwrap(), 1);
    assert_eq!(jobs.park_waiting("project-a", "codex", None).unwrap(), 0);
    assert_eq!(
        jobs.park_waiting("project-a", "codex", Some("No usable account"))
            .unwrap(),
        1
    );
    let parked = jobs.get(id).unwrap().unwrap();
    assert_eq!(parked.status, ResearchStatus::NeedsPool);
    assert_eq!(parked.error.as_deref(), Some("No usable account"));
    assert_eq!(
        jobs.work().unwrap(),
        [
            ("project-a".to_owned(), "codex".to_owned()),
            ("project-b".to_owned(), "codex".to_owned())
        ]
    );
    assert_eq!(jobs.resume_waiting("project-a", "codex").unwrap(), 1);
    let resumed = jobs.get(id).unwrap().unwrap();
    assert_eq!(resumed.status, ResearchStatus::Queued);
    assert_eq!(resumed.error, None);
}

#[test]
fn an_interrupted_job_returns_to_the_queue_without_spending_an_attempt() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let id = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    let held = jobs.claim("project-a", "runner", 1).unwrap().unwrap();
    let other = ResearchJob {
        claimer: Some("other".into()),
        ..held.clone()
    };
    assert!(jobs.release(&other).is_err());
    jobs.release(&held).unwrap();
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
    assert!(jobs.work().unwrap().is_empty());
}

#[test]
fn the_queue_refuses_a_database_reached_through_a_symlink() {
    let (temp, path) = store();
    let link = temp.path().join("linked.sqlite3");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(ResearchJobs::open(&link).is_err());
    assert!(ResearchJobs::open(&path).is_ok());
}

#[test]
fn jobs_queued_with_the_engine_report_path_keep_it_until_done() {
    let (_temp, path) = store();
    let database = rusqlite::Connection::open(&path).unwrap();
    let id: i64 = database
        .query_row(
            "INSERT INTO research_jobs(project,kind,harness,tools,status,report_path,created_ms)
            VALUES ('project-a','tech_stack_research','codex',0,'queued','context/research/tech_stack_research.md',5)
            RETURNING id",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let jobs = ResearchJobs::open(&path).unwrap();
    let job = jobs.claim("project-a", "runner-a", 3).unwrap().unwrap();
    assert_eq!(job.id, id);
    assert!(!job.tools);
    assert_eq!(
        job.report_path.as_deref(),
        Some("context/research/tech_stack_research.md")
    );
    assert_eq!(job.status, ResearchStatus::Running);
}

#[test]
fn work_claims_and_parking_are_scoped_to_one_harness() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let codex = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    let claude = jobs.enqueue("project-a", "design", "claude", true).unwrap();
    assert_eq!(
        jobs.work().unwrap(),
        [
            ("project-a".to_owned(), "claude".to_owned()),
            ("project-a".to_owned(), "codex".to_owned())
        ]
    );
    assert_eq!(
        jobs.park_waiting("project-a", "codex", Some("No usable account"))
            .unwrap(),
        1
    );
    assert_eq!(
        jobs.get(claude).unwrap().unwrap().status,
        ResearchStatus::Queued
    );
    assert!(
        jobs.claim_harness("project-a", "codex", "runner", 3)
            .unwrap()
            .is_none()
    );
    let held = jobs
        .claim_harness("project-a", "claude", "runner", 3)
        .unwrap()
        .unwrap();
    assert_eq!(held.id, claude);
    assert!(
        jobs.claim_harness("project-a", "claude", "runner", 3)
            .unwrap()
            .is_none()
    );
    assert_eq!(jobs.resume_waiting("project-a", "claude").unwrap(), 0);
    assert_eq!(jobs.resume_waiting("project-a", "codex").unwrap(), 1);
    assert_eq!(
        jobs.claim_harness("project-a", "codex", "runner", 3)
            .unwrap()
            .unwrap()
            .id,
        codex
    );
    let third = jobs.enqueue("project-a", "tech", "codex", true).unwrap();
    assert!(
        jobs.claim_harness("project-a", "codex", "runner", 2)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        jobs.claim_harness("project-a", "codex", "runner", 3)
            .unwrap()
            .unwrap()
            .id,
        third
    );
}

#[test]
fn a_job_cancelled_while_its_run_prepares_stays_cancelled_when_the_run_fails_or_parks() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let failing = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    let parked = jobs.enqueue("project-a", "design", "codex", true).unwrap();
    let first = jobs.claim("project-a", "runner", 2).unwrap().unwrap();
    let second = jobs.claim("project-a", "runner", 2).unwrap().unwrap();
    assert_eq!((first.id, second.id), (failing, parked));
    assert!(jobs.request_cancel(failing).unwrap());
    assert!(jobs.request_cancel(parked).unwrap());
    let status = jobs
        .fail(&first, "Could not prepare the research sandbox", 3)
        .unwrap();
    assert_eq!(status, ResearchStatus::Cancelled);
    assert_eq!(jobs.needs_pool(&second).unwrap(), ResearchStatus::Cancelled);
    for id in [failing, parked] {
        let job = jobs.get(id).unwrap().unwrap();
        assert_eq!(job.status, ResearchStatus::Cancelled, "job {id}");
        assert!(job.claimer.is_none());
        assert!(job.finished_ms.is_some());
        assert!(!jobs.request_cancel(id).unwrap());
    }
    assert!(jobs.claim("project-a", "runner", 2).unwrap().is_none());
    assert!(jobs.work().unwrap().is_empty());
}

#[test]
fn a_run_whose_claim_stale_recovery_took_back_cannot_act_after_the_runner_claims_it_again() {
    let (_temp, path) = store();
    let jobs = ResearchJobs::open(&path).unwrap();
    let id = jobs.enqueue("project-a", "market", "codex", true).unwrap();
    let stale = jobs.claim("project-a", "runner", 1).unwrap().unwrap();
    assert_eq!(jobs.recover_stale(0).unwrap(), 1);
    let live = jobs.claim("project-a", "runner", 1).unwrap().unwrap();
    assert_eq!((stale.id, live.id), (id, id));
    assert_eq!(stale.claimer, live.claimer);
    assert!(jobs.heartbeat(&stale, 1).is_err());
    assert!(jobs.finish(&stale, "context/research/market.md").is_err());
    assert!(jobs.fail(&stale, "x", 3).is_err());
    assert!(jobs.needs_pool(&stale).is_err());
    assert!(jobs.release(&stale).is_err());
    assert!(jobs.cancelled(&stale).is_err());
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status, ResearchStatus::Running);
    assert_eq!(job.attempts, live.attempts);
    assert!(!jobs.heartbeat(&live, 2).unwrap());
    jobs.finish(&live, "context/research/market.md").unwrap();
    assert_eq!(jobs.get(id).unwrap().unwrap().status, ResearchStatus::Done);
}
