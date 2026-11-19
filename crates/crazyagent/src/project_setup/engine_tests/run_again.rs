use super::*;

/// Whether a retry ran the job again: applied where this engine runs research, else refused
/// with the code every client shows, after which a row has nothing left to check there.
/// Input: the retry's response. Output: the answer.
fn ran_again(response: OwnerResponse) -> bool {
    if retryable(ResearchStatus::Failed) {
        assert_eq!(result(response), OwnerResult::Applied);
        return true;
    }
    assert_eq!(result(response), rejected(RESEARCH_UNAVAILABLE_HERE));
    false
}

#[tokio::test]
async fn owner_stops_and_retries_research_and_reads_its_plain_log() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![
                ResearchKind::MarketResearch,
                ResearchKind::CompetitorResearch,
            ]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let jobs = owner.research_jobs(project.clone()).await.unwrap();
    let (first, second) = (jobs[0].id, jobs[1].id);

    let cancelled = owner.cancel_research(project.clone(), first, 2).await;
    assert_eq!(result(cancelled.unwrap()), OwnerResult::Applied);
    let stopped = owner.research_jobs(project.clone()).await.unwrap();
    assert_eq!(
        stopped
            .iter()
            .map(|job| (job.id, job.retryable))
            .collect::<Vec<_>>(),
        vec![
            (first, retryable(ResearchStatus::Cancelled)),
            (second, false)
        ]
    );
    let again = owner.cancel_research(project.clone(), first, 3).await;
    assert_eq!(result(again.unwrap()), rejected("research_job_finished"));
    let waiting = owner.retry_research(project.clone(), second, 4).await;
    assert_eq!(
        result(waiting.unwrap()),
        rejected("research_job_not_retryable")
    );
    let unknown = owner.cancel_research(project.clone(), 9999, 5).await;
    assert_eq!(result(unknown.unwrap()), rejected("research_job_missing"));
    let retried = owner.retry_research(project.clone(), first, 6).await;
    if !ran_again(retried.unwrap()) {
        return;
    }
    let job = owner
        .research_jobs(project.clone())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (job.id, job.status, job.attempts, job.cancel_requested),
        (first, ResearchStatus::Queued, 0, false)
    );

    let store =
        crate::research::jobs::ResearchJobs::open(&engine.root.path().join("engine.db")).unwrap();
    let claimed = store
        .claim("setup-pooled", "runner-test", 4)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, first);
    let stopping = owner.cancel_research(project.clone(), first, 7).await;
    assert_eq!(result(stopping.unwrap()), OwnerResult::Applied);
    let job = owner
        .research_jobs(project.clone())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (job.status, job.cancel_requested),
        (ResearchStatus::Running, true)
    );
    assert!(
        store.heartbeat(&claimed, 64).unwrap(),
        "the runner's heartbeat sees the stop and kills the agent's process group"
    );
    store.cancelled(&claimed).unwrap();
    assert_eq!(
        owner.research_jobs(project.clone()).await.unwrap()[0].status,
        ResearchStatus::Cancelled
    );

    assert!(
        owner
            .research_log(project.clone(), second)
            .await
            .unwrap()
            .is_empty()
    );
    let agents = engine.root.path().join("research/agents");
    let older = agents.join(format!("job-{first}-1"));
    let newest = agents.join(format!("job-{first}-1-1"));
    for run in [&older, &newest] {
        std::fs::create_dir_all(run).unwrap();
    }
    std::fs::write(older.join("stderr.log"), "old run\n").unwrap();
    std::fs::write(
        newest.join("events.jsonl"),
        concat!(
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Found three competitors.\"}}\n",
            "{\"type\":\"error\",\"message\":\"Rate limited, waiting\"}\n",
        ),
    )
    .unwrap();
    std::fs::write(
        newest.join("stderr.log"),
        "warning:\tslow network\nAuthorization: Bearer abc123\nusing key sk-proj-ABCDEFGHIJKLMNOPQRSTUVWX now\n",
    )
    .unwrap();
    let lines = owner.research_log(project.clone(), first).await.unwrap();
    assert_eq!(
        lines,
        [
            "Found three competitors.",
            "Rate limited, waiting",
            "warning: slow network",
            "[hidden: this line may contain a secret]",
            "using key [hidden] now",
        ]
    );

    let busy = agents.join(format!("job-{second}-2"));
    std::fs::create_dir_all(&busy).unwrap();
    let steps: String = (0..500).map(|n| format!("step {n}\n")).collect();
    std::fs::write(busy.join("stderr.log"), steps).unwrap();
    let lines = owner.research_log(project.clone(), second).await.unwrap();
    assert_eq!(lines.len(), RESEARCH_LOG_LINES_MAX);
    assert_eq!(
        (
            lines[0].as_str(),
            lines[RESEARCH_LOG_LINES_MAX - 1].as_str()
        ),
        ("step 300", "step 499")
    );
    let wide = agents.join(format!("job-{second}-3"));
    std::fs::create_dir_all(&wide).unwrap();
    let mut output = format!("{}\n", "x".repeat(RESEARCH_LOG_LINE_CHARS_MAX)).repeat(100);
    output.push_str(&"y".repeat(500));
    std::fs::write(wide.join("output.md"), output).unwrap();
    let lines = owner.research_log(project.clone(), second).await.unwrap();
    assert_eq!(lines.len(), 61, "the newest lines that fit one owner frame");
    assert!(lines.iter().map(String::len).sum::<usize>() <= RESEARCH_LOG_BYTES_MAX);
    assert_eq!(
        lines.last().unwrap(),
        &format!("{}...", "y".repeat(RESEARCH_LOG_LINE_CHARS_MAX))
    );
    assert_eq!(
        owner
            .research_log(project.clone(), 9999)
            .await
            .unwrap_err()
            .0,
        "research_job_missing"
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn a_retried_job_whose_research_was_turned_off_is_not_cancelled_by_the_next_save() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let planning = ResearchKind::RoadmapPlanning;
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![ResearchKind::MarketResearch]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let planned = owner.plan_roadmap(project.clone(), 2).await.unwrap();
    assert_eq!(result(planned), OwnerResult::Applied);
    let saved = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(saved.revision, 2);
    assert!(saved.research.contains(&planning));

    let mut off = saved.editable();
    off.research = vec![ResearchKind::MarketResearch];
    let response = owner
        .update_project_setup(project.clone(), 2, None, off, 3)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let before_retry = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(before_retry.revision, 3);
    assert!(!before_retry.research.contains(&planning));
    let job = job_of(&mut owner, &project, planning).await;
    assert_eq!(job.status, ResearchStatus::Cancelled);

    let retried = owner.retry_research(project.clone(), job.id, 4).await;
    if !ran_again(retried.unwrap()) {
        return;
    }
    let after_retry = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        after_retry.revision, 4,
        "turning the kind back on raises the revision"
    );
    assert_eq!(
        after_retry.research,
        [ResearchKind::MarketResearch, planning],
        "the retried kind is on again"
    );
    let queued = job_of(&mut owner, &project, planning).await;
    assert_eq!(
        (queued.id, queued.status, queued.cancel_requested),
        (job.id, ResearchStatus::Queued, false)
    );

    let mut drafted_before = before_retry.editable();
    drafted_before.about = "Plans meals for two.".into();
    let stale = owner
        .update_project_setup(project.clone(), 3, None, drafted_before, 5)
        .await
        .unwrap();
    assert_eq!(
        result(stale),
        rejected("stale_revision"),
        "an edit drafted before the retry cannot turn the kind off unseen"
    );
    let mut fresh = after_retry.editable();
    fresh.about = "Plans meals for two.".into();
    let response = owner
        .update_project_setup(project.clone(), 4, None, fresh, 6)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let kept = job_of(&mut owner, &project, planning).await;
    assert_eq!(
        (kept.id, kept.status, kept.cancel_requested),
        (job.id, ResearchStatus::Queued, false),
        "the next save keeps the retried job"
    );

    let market = job_of(&mut owner, &project, ResearchKind::MarketResearch).await;
    let stopped = owner.cancel_research(project.clone(), market.id, 7).await;
    assert_eq!(result(stopped.unwrap()), OwnerResult::Applied);
    let retried = owner.retry_research(project.clone(), market.id, 8).await;
    if !ran_again(retried.unwrap()) {
        return;
    }
    assert_eq!(
        owner
            .project_setup(project.clone())
            .await
            .unwrap()
            .unwrap()
            .revision,
        5,
        "a kind still on keeps the revision"
    );
    drop(owner);
    engine.stop().await;
}

/// RV-DS241-159: Run again starts a job's rate-limited ends in a row again. A job that ended
/// failed on its third limit hands a limited account back and waits for the pool's next one
/// after the owner runs it again, as a new job does, instead of failing on its first limit.
#[tokio::test]
async fn a_job_run_again_after_the_rate_limit_cap_takes_a_limit_as_a_new_job_does() {
    use crate::research::jobs::ResearchStatus as QueueStatus;
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![ResearchKind::MarketResearch]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let market = job_of(&mut owner, &project, ResearchKind::MarketResearch)
        .await
        .id;
    let store =
        crate::research::jobs::ResearchJobs::open(&engine.root.path().join("engine.db")).unwrap();
    let account = CredentialRef::new("a-codex").unwrap();
    let limited = || {
        let held = store
            .claim("setup-pooled", "runner-test", 4)
            .unwrap()
            .unwrap();
        assert_eq!(held.id, market);
        store.rate_limited(&held, &account).unwrap().0
    };
    assert_eq!(
        [limited(), limited(), limited()],
        [
            QueueStatus::Queued,
            QueueStatus::Queued,
            QueueStatus::Failed
        ]
    );

    let retried = owner.retry_research(project.clone(), market, 2).await;
    if !ran_again(retried.unwrap()) {
        return;
    }
    assert_eq!(
        limited(),
        QueueStatus::Queued,
        "a job run again waits for the pool's next account on its first limit"
    );
    drop(owner);
    engine.stop().await;
}

/// RV-DS241-159: Plan roadmap now starts the planning job's rate-limited ends in a row again.
/// A plan that was saved after two limits, and then one the cap ended failed, takes a limit
/// when it is planned again, as a new job does, instead of failing on the first one.
#[tokio::test]
async fn a_roadmap_planned_again_takes_a_rate_limit_as_a_new_job_does() {
    use crate::research::jobs::ResearchStatus as QueueStatus;
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![ResearchKind::MarketResearch]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let market = job_of(&mut owner, &project, ResearchKind::MarketResearch)
        .await
        .id;
    let stopped = owner.cancel_research(project.clone(), market, 2).await;
    assert_eq!(result(stopped.unwrap()), OwnerResult::Applied);
    let planned = owner.plan_roadmap(project.clone(), 3).await.unwrap();
    assert_eq!(result(planned), OwnerResult::Applied);
    let planning = job_of(&mut owner, &project, ResearchKind::RoadmapPlanning)
        .await
        .id;
    let store =
        crate::research::jobs::ResearchJobs::open(&engine.root.path().join("engine.db")).unwrap();
    let account = CredentialRef::new("a-codex").unwrap();
    let claim = || {
        let held = store
            .claim("setup-pooled", "runner-test", 4)
            .unwrap()
            .unwrap();
        assert_eq!(held.id, planning);
        held
    };
    for _ in 0..2 {
        assert_eq!(
            store.rate_limited(&claim(), &account).unwrap().0,
            QueueStatus::Queued
        );
    }
    store.finish(&claim(), "context/roadmap.md").unwrap();

    let planned = owner.plan_roadmap(project.clone(), 4).await.unwrap();
    assert_eq!(result(planned), OwnerResult::Applied);
    assert_eq!(
        store.rate_limited(&claim(), &account).unwrap().0,
        QueueStatus::Queued,
        "a roadmap planned again waits for the pool's next account on its first limit"
    );

    let ends = [
        store.rate_limited(&claim(), &account).unwrap().0,
        store.rate_limited(&claim(), &account).unwrap().0,
    ];
    assert_eq!(ends, [QueueStatus::Queued, QueueStatus::Failed]);
    let planned = owner.plan_roadmap(project.clone(), 5).await.unwrap();
    assert_eq!(result(planned), OwnerResult::Applied);
    assert_eq!(
        store.rate_limited(&claim(), &account).unwrap().0,
        QueueStatus::Queued,
        "a roadmap planned again after the cap failed it takes a limit as a new job does"
    );
    drop(owner);
    engine.stop().await;
}

/// RV-DS241-159: Run again ends the wait a refused launch left and forgets the refusals in a
/// row. A job stopped while it waits out a refusal is claimed at once after the owner runs it
/// again, and its next refusal counts as the first.
#[tokio::test]
async fn a_job_run_again_does_not_wait_out_the_refusal_it_was_stopped_in() {
    use crate::research::jobs::ResearchStatus as QueueStatus;
    const WAITING: &str = "Waiting for this account to be ready";
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(
            project.clone(),
            spec("Meal planner"),
            pooled_setup(vec![ResearchKind::MarketResearch]),
            1,
        )
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let market = job_of(&mut owner, &project, ResearchKind::MarketResearch)
        .await
        .id;
    let store =
        crate::research::jobs::ResearchJobs::open(&engine.root.path().join("engine.db")).unwrap();
    let held = store
        .claim("setup-pooled", "runner-test", 4)
        .unwrap()
        .unwrap();
    assert_eq!(held.id, market);
    assert_eq!(store.refused(&held, WAITING).unwrap(), QueueStatus::Queued);
    assert!(
        store
            .claim("setup-pooled", "runner-test", 4)
            .unwrap()
            .is_none(),
        "the refused job waits out its backoff"
    );

    let stopped = owner.cancel_research(project.clone(), market, 2).await;
    assert_eq!(result(stopped.unwrap()), OwnerResult::Applied);
    let retried = owner.retry_research(project.clone(), market, 3).await;
    if !ran_again(retried.unwrap()) {
        return;
    }
    let claimed = store.claim("setup-pooled", "runner-test", 4).unwrap();
    assert_eq!(
        claimed.as_ref().map(|job| job.id),
        Some(market),
        "a job run again is claimed at once"
    );
    let held = claimed.unwrap();
    assert_eq!(store.refused(&held, WAITING).unwrap(), QueueStatus::Queued);
    assert_eq!(
        engine.rows("SELECT refused FROM research_jobs WHERE kind='market_research'"),
        1,
        "the next refusal counts as the first"
    );
    drop(owner);
    engine.stop().await;
}
