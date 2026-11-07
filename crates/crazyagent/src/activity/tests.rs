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
    assert_eq!(list(&db, &project(), 3).unwrap(), listed[..3].to_vec());
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
    let live = accepted_text(&format!("Sub-agent {worker}"), "Change cs-b");
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
    assert_eq!(listed[1].text, accepted_text(&worker_label, "Change cs-a"));
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
            kind: ActivityKind::Phase,
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
            kind: ActivityKind::Phase,
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
