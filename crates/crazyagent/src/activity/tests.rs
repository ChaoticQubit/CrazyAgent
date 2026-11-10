use super::*;

const PROJECT: &str = "activity-units";
const OTHER: &str = "activity-other";

/// An engine database with every table the engine makes at start and the change set
/// store, in a temporary folder, holding two projects. Returns the folder and the
/// connection.
fn db() -> (tempfile::TempDir, Connection) {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap();
    let connection = crate::landing::open(&root).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    initialize(&connection).unwrap();
    for project in [PROJECT, OTHER] {
        let row = crate::protocol::StoredProject {
            project_id: ProjectId::new(project).unwrap(),
            spec: crate::protocol::ProjectSpec {
                name: project.into(),
                repositories: vec![],
            },
        };
        connection
            .execute(
                "INSERT INTO projects VALUES (?1,?2)",
                params![project, serde_json::to_string(&row).unwrap()],
            )
            .unwrap();
    }
    (folder, connection)
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

/// Adds a team agent of a role to the project. Returns its id.
fn agent(db: &Connection, role: &str) -> i64 {
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?1,?2,'codex',1,'working',1,1)",
        params![PROJECT, role],
    )
    .unwrap();
    db.last_insert_rowid()
}

/// Adds a unit of work an agent owns in a state. Returns its id.
fn work(db: &Connection, owner: i64, title: &str, state: &str, updated: i64) -> i64 {
    db.execute(
        "INSERT INTO coord_work(project,title,owner_agent,state,version,created,updated) VALUES (?1,?2,?3,?4,1,?5,?5)",
        params![PROJECT, title, owner, state, updated],
    )
    .unwrap();
    db.last_insert_rowid()
}

#[test]
fn every_kind_records_and_lists_newest_first_with_its_goal() {
    let (_folder, db) = db();
    let worker = agent(&db, "sub");
    work(&db, worker, "Old finished work", "complete", 50);
    work(&db, worker, "Ship the login page", "active", 100);
    let actor = agent_actor(&db, worker).unwrap();
    assert_eq!(actor, format!("Sub-agent {worker}"));
    for (at, kind) in ActivityKind::ALL.into_iter().enumerate() {
        let text = format!("event\n  {}", kind.name());
        let item = NewActivity {
            kind,
            actor: &actor,
            agent: Some(worker),
            goal: (at % 2 == 1).then_some("Its own goal"),
            text: &text,
            change_set: (kind == ActivityKind::Accepted).then_some("cs-1"),
        };
        record(&db, &project(), &item, 1_000 + at as i64).unwrap();
    }
    let other = NewActivity {
        kind: ActivityKind::Phase,
        actor: "Master",
        agent: None,
        goal: None,
        text: "elsewhere",
        change_set: None,
    };
    record(&db, &ProjectId::new(OTHER).unwrap(), &other, 5_000).unwrap();

    let listed = list(&db, &project(), LIST_MAX).unwrap();

    assert_eq!(listed.len(), ActivityKind::ALL.len());
    for (newest, item) in listed.iter().enumerate() {
        let at = ActivityKind::ALL.len() - 1 - newest;
        assert_eq!(item.kind, ActivityKind::ALL[at]);
        assert_eq!(item.at_ms, 1_000 + at as i64);
        assert_eq!(item.actor, actor);
        assert_eq!(item.text, format!("event {}", item.kind.name()));
        let goal = if at % 2 == 1 {
            "Its own goal"
        } else {
            "Ship the login page"
        };
        assert_eq!(item.goal.as_deref(), Some(goal));
        assert_eq!(
            item.change_set.as_deref(),
            (item.kind == ActivityKind::Accepted).then_some("cs-1")
        );
        assert!(!item.undone);
    }
    let (paced, others): (Vec<ActivityItem>, Vec<ActivityItem>) = listed
        .iter()
        .cloned()
        .partition(|item| LOGGED_KINDS.contains(&item.kind) || WORK_KINDS.contains(&item.kind));
    assert_eq!(
        list(&db, &project(), 3).unwrap(),
        [paced[0].clone(), others[0].clone(), others[1].clone()],
        "a limit of 3 gives the agents' rows one place (T11-R2-L1)"
    );
    assert_eq!(
        list(&db, &project(), 0).unwrap_err().0,
        "activity_limit_invalid"
    );
    assert_eq!(
        list(&db, &project(), LIST_MAX + 1).unwrap_err().0,
        "activity_limit_invalid"
    );
}

#[test]
fn texts_are_cut_to_their_bounds_and_a_list_fits_one_frame() {
    let (_folder, db) = db();
    let long = "é".repeat(TEXT_BYTES_MAX);
    let goal = "g".repeat(GOAL_BYTES_MAX + 40);
    for at in 0..200 {
        let item = NewActivity {
            kind: ActivityKind::Test,
            actor: "Master",
            agent: None,
            goal: Some(&goal),
            text: &long,
            change_set: None,
        };
        record(&db, &project(), &item, at).unwrap();
    }
    let empty = NewActivity {
        kind: ActivityKind::Test,
        actor: "Master",
        agent: None,
        goal: None,
        text: " \n ",
        change_set: None,
    };
    assert_eq!(
        record(&db, &project(), &empty, 1).unwrap_err().0,
        "activity_item_invalid"
    );

    let listed = list(&db, &project(), LIST_MAX).unwrap();

    assert!(!listed.is_empty() && listed.len() < 200, "{}", listed.len());
    assert_eq!(listed[0].at_ms, 199);
    assert!(listed.iter().all(|item| {
        item.text.len() <= TEXT_BYTES_MAX
            && item.text.chars().all(|character| character == 'é')
            && item
                .goal
                .as_ref()
                .is_some_and(|goal| goal.len() == GOAL_BYTES_MAX)
    }));
    let bytes: usize = listed
        .iter()
        .map(|item| serde_json::to_vec(item).unwrap().len())
        .sum();
    assert!(bytes <= LISTED_BYTES_MAX, "{bytes}");
}

#[test]
fn backfill_brings_in_accepts_audits_and_research_once() {
    let (_folder, db) = db();
    let worker = agent(&db, "sub");
    let lead = agent(&db, "master");
    for (id, state, updated) in [
        ("cs-a", "accepted", 2_000),
        ("cs-b", "accepted", 3_000),
        ("cs-c", "pending", 4_000),
    ] {
        db.execute(
            "INSERT INTO landing_change_sets(id,project,agent_kind,agent_id,label,repository,worktree,git_dir,base,head,state,last_outcome,turn,prompt_line,files,added,removed,conflicts,created_ms,updated_ms)
             VALUES (?1,?2,'sub',?3,?4,'/r',?5,'/g','b','h',?6,'succeeded','t',?7,1,1,0,'[]',1,?8)",
            params![
                id,
                PROJECT,
                worker.to_string(),
                format!("Sub-agent {worker}"),
                format!("/w/{id}"),
                state,
                format!("Change {id}"),
                updated
            ],
        )
        .unwrap();
    }
    let unit = work(&db, worker, "Ship the login page", "complete", 900);
    for (action, by_kind, by_agent, at) in [
        ("start", "agent", Some(worker), 500),
        ("assign", "agent", Some(lead), 600),
        ("finish_complete", "agent", Some(worker), 900),
        ("finish_closed", "owner", None, 950),
    ] {
        db.execute(
            "INSERT INTO coord_audit(project,work,by_kind,by_agent,action,from_version,to_version,at) VALUES (?1,?2,?3,?4,?5,1,2,?6)",
            params![PROJECT, unit, by_kind, by_agent, action, at],
        )
        .unwrap();
    }
    for (kind, status, started, finished) in [
        ("brief", "done", Some(100), Some(200)),
        ("market", "failed", Some(150), Some(250)),
        ("deep", "queued", None, None),
    ] {
        db.execute(
            "INSERT INTO research_jobs(project,kind,harness,tools,status,created_ms,started_ms,finished_ms) VALUES (?1,?2,'codex',1,?3,1,?4,?5)",
            params![PROJECT, kind, status, started, finished],
        )
        .unwrap();
    }
    let live = accepted_text(
        OWNER_ACTOR,
        &crate::landing::change_set_words(None, &format!("Sub-agent {worker}"), "Change cs-b", 1),
    );
    record(
        &db,
        &project(),
        &NewActivity {
            kind: ActivityKind::Accepted,
            actor: "Owner",
            agent: Some(worker),
            goal: None,
            text: &live,
            change_set: Some("cs-b"),
        },
        3_000,
    )
    .unwrap();

    let added = backfill(&db, &project()).unwrap();
    let again = backfill(&db, &project()).unwrap();

    assert_eq!((added, again), (8, 0));
    let listed = list(&db, &project(), LIST_MAX).unwrap();
    let seen: Vec<(ActivityKind, i64, &str)> = listed
        .iter()
        .map(|item| (item.kind, item.at_ms, item.actor.as_str()))
        .collect();
    let worker_label = format!("Sub-agent {worker}");
    assert_eq!(
        seen,
        vec![
            (ActivityKind::Accepted, 3_000, "Owner"),
            (ActivityKind::Accepted, 2_000, "Owner"),
            (ActivityKind::WorkClosed, 950, "Owner"),
            (ActivityKind::WorkComplete, 900, worker_label.as_str()),
            (ActivityKind::WorkStarted, 500, worker_label.as_str()),
            (ActivityKind::ResearchStopped, 250, "Research"),
            (ActivityKind::ResearchSaved, 200, "Research"),
            (ActivityKind::ResearchStarted, 150, "Research"),
            (ActivityKind::ResearchStarted, 100, "Research"),
        ]
    );
    assert_eq!(listed[1].change_set.as_deref(), Some("cs-a"));
    assert_eq!(
        listed[1].text,
        accepted_text(
            OWNER_ACTOR,
            &crate::landing::change_set_words(None, &worker_label, "Change cs-a", 1)
        )
    );
    assert_eq!(listed[3].goal.as_deref(), Some("Ship the login page"));
    assert_eq!(listed[6].text, "Research saved: brief");
    assert_eq!(
        list(&db, &ProjectId::new(OTHER).unwrap(), LIST_MAX).unwrap(),
        Vec::new()
    );
}

#[test]
fn a_removed_project_loses_its_events_and_backfills_again() {
    let (_folder, db) = db();
    let item = NewActivity {
        kind: ActivityKind::Phase,
        actor: "Master",
        agent: None,
        goal: None,
        text: "building",
        change_set: None,
    };
    record(&db, &project(), &item, 10).unwrap();
    backfill(&db, &project()).unwrap();

    forget_project(&db, &project()).unwrap();

    assert_eq!(list(&db, &project(), LIST_MAX).unwrap(), Vec::new());
    let marked: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM project_activity_backfill WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(marked, 0);
}

#[test]
fn a_new_project_records_once_that_the_owner_created_it() {
    let (_folder, mut db) = db();
    let id = ProjectId::new("activity-new").unwrap();
    let spec = crate::protocol::ProjectSpec {
        name: "Harbor Notes".into(),
        repositories: vec![],
    };
    let tx = db.transaction().unwrap();
    assert_eq!(
        crate::storage::create_project(&tx, &id, &spec).unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
    let created = |db: &Connection| -> Vec<(ActivityKind, String, Option<String>, String)> {
        list(db, &id, 50)
            .unwrap()
            .into_iter()
            .map(|item| (item.kind, item.actor, item.goal, item.text))
            .collect()
    };
    let once = vec![(
        ActivityKind::Created,
        OWNER_ACTOR.to_owned(),
        None,
        "You created Harbor Notes".to_owned(),
    )];
    assert_eq!(created(&db), once);
    let tx = db.transaction().unwrap();
    assert_ne!(
        crate::storage::create_project(&tx, &id, &spec).unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
    initialize(&db).unwrap();
    assert_eq!(
        created(&db),
        once,
        "a refused create and a restart add nothing"
    );
}

#[test]
fn a_project_made_before_events_gets_one_created_event_from_its_setup() {
    let (_folder, db) = db();
    crate::project_setup::initialize(&db).unwrap();
    let setup = serde_json::json!({"name": "Harbor Notes", "created_ms": 1_000});
    db.execute(
        "INSERT INTO project_setups VALUES (?1,?2)",
        params![PROJECT, setup.to_string()],
    )
    .unwrap();
    initialize(&db).unwrap();
    initialize(&db).unwrap();
    assert!(
        !backfill_created(&db, &project()).unwrap(),
        "it has one already"
    );
    let created = |project: &ProjectId| -> Vec<(i64, String, String)> {
        list(&db, project, 50)
            .unwrap()
            .into_iter()
            .filter(|item| item.kind == ActivityKind::Created)
            .map(|item| (item.at_ms, item.actor, item.text))
            .collect()
    };
    assert_eq!(
        created(&project()),
        vec![(
            1_000,
            OWNER_ACTOR.to_owned(),
            "You created Harbor Notes".to_owned()
        )]
    );
    let other = ProjectId::new(OTHER).unwrap();
    assert!(!backfill_created(&db, &other).unwrap());
    assert_eq!(
        created(&other),
        vec![],
        "a project with no saved setup has no creation time"
    );
}

#[test]
fn a_read_says_when_older_events_were_left_out_by_the_limit_or_the_byte_bound() {
    let (_folder, db) = db();
    let line = "x".repeat(TEXT_BYTES_MAX);
    for at_ms in 0..3 {
        let item = NewActivity {
            kind: ActivityKind::TaskStarted,
            actor: "Master",
            agent: None,
            goal: None,
            text: &line,
            change_set: None,
        };
        record(&db, &project(), &item, at_ms).unwrap();
    }
    let all = listed(&db, &project(), 3).unwrap();
    assert_eq!((all.items.len(), all.more), (3, false));
    let newest = listed(&db, &project(), 2).unwrap();
    assert_eq!(
        newest
            .items
            .iter()
            .map(|item| item.at_ms)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert!(newest.more, "the limit left one out");
    for at_ms in 3..200 {
        let item = NewActivity {
            kind: ActivityKind::TaskStarted,
            actor: "Master",
            agent: None,
            goal: None,
            text: &line,
            change_set: None,
        };
        record(&db, &project(), &item, at_ms).unwrap();
    }
    let cut = listed(&db, &project(), LIST_MAX).unwrap();
    assert!(cut.items.len() < 200, "{}", cut.items.len());
    assert!(cut.more, "the byte bound left the rest out");
    assert_eq!(cut.items, list(&db, &project(), LIST_MAX).unwrap());
}

/// Every writer's text, goal and actor go through the change set subject's scrub (F1): a
/// character a reader cannot see is shown as an escape and an absolute local path is cut
/// to its last component, so neither reaches a row as written.
#[test]
fn record_shows_hidden_characters_and_cuts_local_paths_in_text_goal_and_actor() {
    let (_folder, db) = db();
    record(
        &db,
        &project(),
        &NewActivity {
            kind: ActivityKind::Phase,
            actor: "Ada\u{200b}",
            agent: None,
            goal: Some("Ship /Users/ada/private/goal.md\u{202e}"),
            text: "Read /Users/ada/.ssh/id_rsa and ~/notes/plan.md\u{2066} twice",
            change_set: None,
        },
        1_000,
    )
    .unwrap();
    let item = &list(&db, &project(), 1).unwrap()[0];
    for shown in [
        item.actor.as_str(),
        item.goal.as_deref().unwrap_or_default(),
        item.text.as_str(),
    ] {
        assert!(!crate::memory::hidden_text(shown), "{shown:?}");
        assert!(
            !shown.contains("/Users/") && !shown.contains("~/"),
            "{shown:?}"
        );
    }
    assert_eq!(item.text, "Read id_rsa and plan.md twice");
    assert_eq!(item.actor, "Ada\\u200b");
    assert_eq!(item.goal.as_deref(), Some("Ship goal.md"));
}

/// A team agent is named the way the team list names it (F2): its kept persona and handle,
/// the persona alone before its first job, its role number only without a kept name. A
/// backfill after the agent's name changed adds no second row for an event it recorded.
#[test]
fn an_agent_is_named_by_its_kept_persona_and_a_renamed_agent_is_not_backfilled_twice() {
    let (_folder, db) = db();
    let ada = agent(&db, "sub");
    let bo = agent(&db, "teammate");
    let plain = agent(&db, "sub");
    let lead = agent(&db, "master");
    db.execute(
        "INSERT INTO team_names(agent,project,persona,handle) VALUES (?1,?2,'Ada','login-fix'),(?3,?2,'Bo',NULL)",
        params![ada, PROJECT, bo],
    )
    .unwrap();
    assert_eq!(agent_actor(&db, ada).unwrap(), "Ada (@login-fix)");
    assert_eq!(agent_actor(&db, bo).unwrap(), "Bo");
    assert_eq!(
        agent_actor(&db, plain).unwrap(),
        format!("Sub-agent {plain}")
    );
    assert_eq!(agent_actor(&db, lead).unwrap(), "Master");
    let unit = work(&db, ada, "Ship the login page", "active", 500);
    db.execute(
        "INSERT INTO coord_audit(project,work,by_kind,by_agent,action,from_version,to_version,at) VALUES (?1,?2,'agent',?3,'start',1,2,500)",
        params![PROJECT, unit, ada],
    )
    .unwrap();
    note_audit(&db, &project(), unit, ("agent", Some(ada)), "start", 500);
    db.execute("UPDATE team_names SET persona='Cy' WHERE agent=?1", [ada])
        .unwrap();
    db.execute("DELETE FROM project_activity_backfill", [])
        .unwrap();
    assert_eq!(backfill(&db, &project()).unwrap(), 0);
    let started: Vec<String> = list(&db, &project(), LIST_MAX)
        .unwrap()
        .into_iter()
        .filter(|item| item.kind == ActivityKind::WorkStarted)
        .map(|item| item.actor)
        .collect();
    assert_eq!(started, vec!["Ada (@login-fix)".to_owned()]);
}

/// A research run the queue took back for another attempt is backfilled as started only,
/// never as stopped; a cancelled run still stops (F3).
#[test]
fn backfill_records_a_requeued_run_as_started_only() {
    let (_folder, db) = db();
    for (kind, status, started, finished) in [
        ("retry", "queued", 300, 350),
        ("gone", "cancelled", 400, 450),
    ] {
        db.execute(
            "INSERT INTO research_jobs(project,kind,harness,tools,status,created_ms,started_ms,finished_ms) VALUES (?1,?2,'codex',1,?3,1,?4,?5)",
            params![PROJECT, kind, status, started, finished],
        )
        .unwrap();
    }
    db.execute("DELETE FROM project_activity_backfill", [])
        .unwrap();
    backfill(&db, &project()).unwrap();
    let seen: Vec<(ActivityKind, i64)> = list(&db, &project(), LIST_MAX)
        .unwrap()
        .into_iter()
        .map(|item| (item.kind, item.at_ms))
        .collect();
    assert_eq!(
        seen,
        vec![
            (ActivityKind::ResearchStopped, 450),
            (ActivityKind::ResearchStarted, 400),
            (ActivityKind::ResearchStarted, 300),
        ]
    );
}

/// The store keeps a project's `KEPT_PER_PROJECT_MAX` events split in two (F4, T11 r1 M1):
/// Created with the newest 8,999 events of the engine and the owner, and the newest
/// `AGENT_LOGGED_KEPT_MAX` phases and test runs agents logged. Past each share its oldest go
/// first, and Created, which never churns, stays.
#[test]
fn the_store_keeps_its_bound_with_the_created_event_and_drops_the_oldest_others() {
    let (_folder, db) = db();
    note_created(&db, &project(), "Harbor Notes", 0);
    let tx = db.unchecked_transaction().unwrap();
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO project_activity(project,at_ms,kind,actor,text) VALUES (?1,?2,'task_started','Master',?3)",
            )
            .unwrap();
        for at in 1..KEPT_PER_PROJECT_MAX {
            insert
                .execute(params![PROJECT, at, format!("Event {at}")])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    for at in KEPT_PER_PROJECT_MAX..KEPT_PER_PROJECT_MAX + 5 {
        let text = format!("Event {at}");
        let item = NewActivity {
            kind: ActivityKind::TaskStarted,
            actor: MASTER_ACTOR,
            agent: None,
            goal: None,
            text: &text,
            change_set: None,
        };
        record(&db, &project(), &item, at).unwrap();
    }
    let count = |filter: &str| -> i64 {
        db.query_row(
            &format!("SELECT COUNT(*) FROM project_activity WHERE project=?1 AND {filter}"),
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap()
    };
    let others = "kind NOT IN ('created','phase','test')";
    let first_kept = AGENT_LOGGED_KEPT_MAX + 6;
    assert_eq!(count("kind='created'"), 1);
    assert_eq!(
        count(others),
        KEPT_PER_PROJECT_MAX - AGENT_LOGGED_KEPT_MAX - 1
    );
    assert_eq!(count(&format!("{others} AND at_ms < {first_kept}")), 0);
    assert_eq!(count(&format!("{others} AND at_ms = {first_kept}")), 1);
    db.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2)
         INSERT INTO project_activity(project,at_ms,kind,actor,text)
         SELECT ?1, 20000 + i, 'phase', 'Master', 'Phase ' || i FROM n",
        params![PROJECT, AGENT_LOGGED_KEPT_MAX + 2],
    )
    .unwrap();
    let phase = NewActivity {
        kind: ActivityKind::Phase,
        actor: MASTER_ACTOR,
        agent: None,
        goal: None,
        text: "The newest phase",
        change_set: None,
    };
    record(&db, &project(), &phase, 30_000).unwrap();
    assert_eq!(count("kind IN ('phase','test')"), AGENT_LOGGED_KEPT_MAX);
    assert_eq!(count("kind='phase' AND at_ms <= 20003"), 0);
    assert_eq!(
        count(others),
        KEPT_PER_PROJECT_MAX - AGENT_LOGGED_KEPT_MAX - 1,
        "agents' rows push out none of the others"
    );
    assert_eq!(count("1"), KEPT_PER_PROJECT_MAX);
}

/// A lead's merge and a merge into main are the only record the Merges count has, so they
/// keep a share of their own: however many other events arrive none of them goes, and past
/// that share only the oldest merges do.
#[test]
fn merges_keep_their_own_share_and_other_events_never_push_them_out() {
    let (_folder, db) = db();
    let filler = |kind: &str, first: i64, rows: i64| {
        db.execute(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?4)
             INSERT INTO project_activity(project,at_ms,kind,actor,text)
             SELECT ?1, ?3 + i, ?2, 'Master', 'Event ' || i FROM n",
            params![PROJECT, kind, first, rows],
        )
        .unwrap();
    };
    let event = |kind, text: &'static str| NewActivity {
        kind,
        actor: MASTER_ACTOR,
        agent: None,
        goal: None,
        text,
        change_set: None,
    };
    let count = |filter: &str| -> i64 {
        db.query_row(
            &format!("SELECT COUNT(*) FROM project_activity WHERE project=?1 AND {filter}"),
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap()
    };
    let merges = "kind IN ('lead_merged','merged_into_main')";
    filler("lead_merged", 1, 2);
    filler("merged_into_main", 3, 1);
    filler("task_started", 100, KEPT_PER_PROJECT_MAX);
    let newest = KEPT_PER_PROJECT_MAX + 1_000;
    record(
        &db,
        &project(),
        &event(ActivityKind::TaskStarted, "One more event"),
        newest,
    )
    .unwrap();
    assert_eq!(count(merges), 3, "other events pushed out no merge");
    assert_eq!(
        count("kind='task_started'"),
        KEPT_PER_PROJECT_MAX - AGENT_LOGGED_KEPT_MAX
    );
    filler("lead_merged", newest + 1, MERGES_KEPT_MAX);
    record(
        &db,
        &project(),
        &event(ActivityKind::MergedIntoMain, "The newest merge"),
        newest + 60_000,
    )
    .unwrap();
    assert_eq!(count(merges), MERGES_KEPT_MAX);
    assert_eq!(count(&format!("{merges} AND at_ms <= 3")), 0);
    assert_eq!(
        count("kind='task_started'"),
        KEPT_PER_PROJECT_MAX - AGENT_LOGGED_KEPT_MAX,
        "merges push out none of the others"
    );
}

/// Phases and test runs agents log keep their own newest 1,000 (T11 r1 M1): a looping
/// agent's 10,001 phases never push out the owner's Accept, which the store keeps.
#[test]
fn agent_logged_rows_keep_their_own_share_and_never_push_out_other_events() {
    let (_folder, db) = db();
    let accepted = NewActivity {
        kind: ActivityKind::Accepted,
        actor: OWNER_ACTOR,
        agent: None,
        goal: None,
        text: "Accepted the changes of Ada (@api): Add the login form",
        change_set: None,
    };
    record(&db, &project(), &accepted, 1).unwrap();
    db.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10000)
         INSERT INTO project_activity(project,at_ms,kind,actor,text)
         SELECT ?1, 1 + i, 'phase', 'Ada (@api)', 'Phase ' || i FROM n",
        [PROJECT],
    )
    .unwrap();
    let phase = NewActivity {
        kind: ActivityKind::Phase,
        actor: "Ada (@api)",
        agent: None,
        goal: Some("Build the service"),
        text: "One more phase",
        change_set: None,
    };
    record(&db, &project(), &phase, 20_000).unwrap();
    let count = |kind: &str| -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM project_activity WHERE project=?1 AND kind=?2",
            [PROJECT, kind],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        count("accepted"),
        1,
        "an agent's phases never push out the owner's Accept"
    );
    assert_eq!(
        count("phase"),
        1_000,
        "agents' rows keep their newest 1,000"
    );
    let listed: Vec<String> = list(&db, &project(), LIST_MAX)
        .unwrap()
        .into_iter()
        .map(|item| item.text)
        .collect();
    assert_eq!(listed.first().map(String::as_str), Some("One more phase"));
}

/// Work verbs keep their own newest 2,000 rows beside the other shares (T11-R2-F2): an
/// agent looping start_work and finish_work never pushes out the owner's Accept or the
/// project's Created, and past its share only the oldest work rows go.
#[test]
fn work_rows_keep_their_own_share_and_never_push_out_other_events() {
    let (_folder, db) = db();
    note_created(&db, &project(), "Harbor Notes", 0);
    let accepted = NewActivity {
        kind: ActivityKind::Accepted,
        actor: OWNER_ACTOR,
        agent: None,
        goal: None,
        text: "Accepted the changes of Ada (@api): Add the login form",
        change_set: None,
    };
    record(&db, &project(), &accepted, 1).unwrap();
    db.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10000)
         INSERT INTO project_activity(project,at_ms,kind,actor,goal,text)
         SELECT ?1, 1 + i, CASE i % 2 WHEN 0 THEN 'work_started' ELSE 'work_complete' END,
                'Ada (@api)', 'Loop', 'Work ' || i FROM n",
        [PROJECT],
    )
    .unwrap();
    let start = NewActivity {
        kind: ActivityKind::WorkStarted,
        actor: "Ada (@api)",
        agent: None,
        goal: Some("Loop"),
        text: "Started the work",
        change_set: None,
    };
    record(&db, &project(), &start, 20_000).unwrap();
    let count = |filter: &str| -> i64 {
        db.query_row(
            &format!("SELECT COUNT(*) FROM project_activity WHERE project=?1 AND {filter}"),
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap()
    };
    let work = "kind IN ('work_started','work_review_ready','work_complete','work_closed')";
    assert_eq!(
        count("kind='accepted'"),
        1,
        "an agent's work verbs never push out the owner's Accept"
    );
    assert_eq!(count("kind='created'"), 1);
    assert_eq!(count(work), 2_000, "work rows keep their newest 2,000");
    assert_eq!(count(&format!("{work} AND at_ms <= 8002")), 0);
    assert_eq!(count(&format!("{work} AND at_ms = 8003")), 1);
}

/// A team that logs fast cannot crowd the engine's and the owner's events out of a listing
/// (T11-R2-L1): agents' phases, test runs and work verbs take at most a quarter of it, so
/// the owner's Accept under 1,000 newer agent rows still lists, at every limit, and the
/// read says older rows were left out.
#[test]
fn agent_rows_take_at_most_a_quarter_of_a_listing() {
    let (_folder, db) = db();
    note_created(&db, &project(), "Harbor Notes", 0);
    let accepted = NewActivity {
        kind: ActivityKind::Accepted,
        actor: OWNER_ACTOR,
        agent: None,
        goal: None,
        text: "Accepted the changes of Ada (@api): Add the login form",
        change_set: None,
    };
    record(&db, &project(), &accepted, 1).unwrap();
    db.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000)
         INSERT INTO project_activity(project,at_ms,kind,actor,text)
         SELECT ?1, 1 + i, CASE i % 3 WHEN 0 THEN 'phase' WHEN 1 THEN 'test' ELSE 'work_started' END,
                'Ada (@api)', 'Row ' || i FROM n",
        [PROJECT],
    )
    .unwrap();
    for limit in [LIST_MAX, 200, 4] {
        let read = listed(&db, &project(), limit).unwrap();
        let kinds: Vec<ActivityKind> = read.items.iter().map(|item| item.kind).collect();
        let agent_rows = kinds
            .iter()
            .filter(|kind| {
                matches!(
                    kind,
                    ActivityKind::Phase
                        | ActivityKind::Test
                        | ActivityKind::WorkStarted
                        | ActivityKind::WorkReviewReady
                        | ActivityKind::WorkComplete
                        | ActivityKind::WorkClosed
                )
            })
            .count();
        assert!(
            agent_rows <= limit.div_ceil(4),
            "limit {limit}: {agent_rows}"
        );
        assert!(
            agent_rows > 0,
            "limit {limit}: the newest agent rows still list"
        );
        assert!(kinds.contains(&ActivityKind::Accepted), "limit {limit}");
        assert!(kinds.contains(&ActivityKind::Created), "limit {limit}");
        assert!(read.more, "limit {limit}: older agent rows were left out");
    }
}

/// A task that could not finish reads as failed, never with the done check of a finished
/// one (TEAM-17 note): it is stored under a kind of its own beside task_finished.
#[test]
fn a_task_that_could_not_finish_has_its_own_kind() {
    let (_folder, db) = db();
    let worker = agent(&db, "sub");
    note_task_end(&db, &project(), worker, None, true, 10);
    note_task_end(&db, &project(), worker, None, false, 20);
    let seen: Vec<(&str, String)> = list(&db, &project(), LIST_MAX)
        .unwrap()
        .into_iter()
        .map(|item| (item.kind.name(), item.text))
        .collect();
    let actor = agent_actor(&db, worker).unwrap();
    assert_eq!(
        seen,
        [
            ("task_finished", format!("{actor} finished a task")),
            ("task_failed", format!("{actor} could not finish a task")),
        ]
    );
}

/// Rows stored before a task that could not finish and a research run parked for a pool
/// had kinds of their own move to them at engine start, matched on the exact words their
/// writers used, while a finished task whose title holds those words stays finished and a
/// stopped run stays stopped (TEAM-17 note, T10-R2-L1).
#[test]
fn old_rows_move_to_the_failed_and_waiting_kinds_at_start() {
    let (_folder, db) = db();
    let rows = [
        (
            "task_finished",
            "Ada (@api)",
            "Ada (@api) could not finish \u{201c}Login\u{201d}",
        ),
        (
            "task_finished",
            "Ada (@api)",
            "Ada (@api) finished \u{201c}Why it could not finish the build\u{201d}",
        ),
        (
            "research_stopped",
            RESEARCH_ACTOR,
            "Research waiting for a pool: Design research",
        ),
        (
            "research_stopped",
            RESEARCH_ACTOR,
            "Research stopped: Market research",
        ),
    ];
    for (at, (kind, actor, text)) in (1_i64..).zip(rows) {
        db.execute(
            "INSERT INTO project_activity(project,at_ms,kind,actor,text) VALUES (?1,?2,?3,?4,?5)",
            params![PROJECT, at, kind, actor, text],
        )
        .unwrap();
    }
    initialize(&db).unwrap();
    let kinds: Vec<String> = db
        .prepare("SELECT kind FROM project_activity WHERE project=?1 ORDER BY at_ms")
        .unwrap()
        .query_map([PROJECT], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        kinds,
        [
            "task_failed",
            "task_finished",
            "research_waiting",
            "research_stopped"
        ]
    );
}

/// AL2: an Accept brought in from before the engine recorded events reads as the owner's,
/// the set named by its number when it has one and by its agent and prompt while not.
#[test]
fn backfilled_accepts_read_as_yours_and_name_a_numbered_set_by_its_number() {
    let (_folder, db) = db();
    let worker = agent(&db, "sub");
    for (id, updated) in [("cs-a", 2_000), ("cs-b", 3_000)] {
        db.execute(
            "INSERT INTO landing_change_sets(id,project,agent_kind,agent_id,label,repository,worktree,git_dir,base,head,state,last_outcome,turn,prompt_line,files,added,removed,conflicts,created_ms,updated_ms)
             VALUES (?1,?2,'sub',?3,?4,'/r',?5,'/g','b','h','accepted','succeeded','t',?6,2,1,0,'[]',1,?7)",
            params![
                id,
                PROJECT,
                worker.to_string(),
                format!("Sub-agent {worker}"),
                format!("/w/{id}"),
                format!("Change {id}"),
                updated
            ],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO landing_change_set_numbers(change_set,project,number) VALUES ('cs-a',?1,3)",
        [PROJECT],
    )
    .unwrap();
    backfill(&db, &project()).unwrap();
    let texts: Vec<String> = list(&db, &project(), LIST_MAX)
        .unwrap()
        .into_iter()
        .filter(|item| item.kind == ActivityKind::Accepted)
        .map(|item| item.text)
        .collect();
    assert_eq!(
        texts,
        vec![
            format!("You accepted the changes of Sub-agent {worker}: Change cs-b"),
            format!("You accepted change set #3 (Sub-agent {worker}: Change cs-a, 2 files)"),
        ]
    );
}
