use super::*;

#[test]
fn a_run_attempt_outside_the_job_attempt_bound_is_refused() {
    let root = engine_root();
    let runner = ResearchRunner::new(
        &root.database,
        &root.state,
        Path::new("/usr/bin/true"),
        ResearchLimits::default(),
    )
    .unwrap();
    let jobs = ResearchJobs::open(&root.database).unwrap();
    let id = jobs
        .enqueue("alpha", "market_research", "codex", false)
        .unwrap();
    let mut job = jobs.claim("alpha", "attempts-row", 1).unwrap().unwrap();
    assert_eq!(job.id, id);
    for (attempts, named) in [
        (1, Some(format!("job-{id}-1"))),
        (10, Some(format!("job-{id}-10"))),
        (0, None),
        (11, None),
    ] {
        job.attempts = attempts;
        match runner.free_run(&root.base, &job) {
            Ok((name, _)) => assert_eq!(Some(name), named, "attempt {attempts}"),
            Err(RunError::Invalid(_)) => assert_eq!(named, None, "attempt {attempts}"),
            Err(other) => panic!("attempt {attempts}: {other:?}"),
        }
    }
}
