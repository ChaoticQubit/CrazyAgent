use super::*;

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crate::storage::plugins::initialize(&connection).unwrap();
    crate::storage::setup::initialize(&connection).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    crate::onboarding::initialize(&connection).unwrap();
    connection
}

fn waiting_to_map(database: &Connection, project: &str, created_ms: i64) {
    database
        .execute(
            "INSERT INTO onboarding(project,repo,pool,graph_state,mapping_state,drafted,created_ms,updated_ms)
             VALUES (?1,?2,'main','done','waiting',?3,?4,?4)",
            params![
                project,
                format!("/repos/{project}"),
                serde_json::to_string(&Drafted::new(
                    project.to_owned(),
                    &CredentialRef::new("main").unwrap()
                ))
                .unwrap(),
                created_ms
            ],
        )
        .unwrap();
}

fn claimed(database: &Connection) -> Option<String> {
    claim(database, "claim-test").unwrap().map(|claimed| {
        assert_eq!(claimed.kind, Kind::Mapping);
        claimed.project.as_str().to_owned()
    })
}

fn pause(database: &Connection, project: &str, until_ms: i64) {
    let claimed = claim(database, "claim-test").unwrap().unwrap();
    assert_eq!(claimed.project.as_str(), project);
    finish(
        database,
        "claim-test",
        &claimed,
        &Step::Waiting {
            until_ms: Some(until_ms),
        },
        None,
    )
    .unwrap();
}

#[test]
fn a_paused_mapping_lets_newer_projects_go_first_until_its_pause_ends() {
    let database = database();
    waiting_to_map(&database, "older", 1);
    waiting_to_map(&database, "newer", 2);
    let later = now_ms() + 5 * 60 * 60 * 1000;

    pause(&database, "older", later);

    assert_eq!(claimed(&database).as_deref(), Some("newer"));
    assert_eq!(
        claimed(&database),
        None,
        "the paused project is not claimed again"
    );
    database
        .execute(
            "UPDATE onboarding SET mapping_reason=?1 WHERE project='older'",
            [(now_ms() - 1).to_string()],
        )
        .unwrap();
    assert_eq!(claimed(&database).as_deref(), Some("older"));
    let runs: i64 = database
        .query_row(
            "SELECT runs FROM onboarding WHERE project='older'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(runs, 2, "a pause costs no extra run");
}

#[test]
fn a_mapping_paused_with_no_end_in_sight_asks_for_its_account_again_after_a_while() {
    let database = database();
    waiting_to_map(&database, "shop", 1);
    pause(&database, "shop", i64::MAX);
    assert_eq!(claimed(&database), None);
    database
        .execute(
            "UPDATE onboarding SET updated_ms=?1 WHERE project='shop'",
            [now_ms() - PAUSE_RECHECK_MS],
        )
        .unwrap();
    assert_eq!(claimed(&database).as_deref(), Some("shop"));
}

#[cfg(unix)]
#[test]
fn a_waiting_onboarding_step_halts_a_refresh_build() {
    let database = database();
    let running = || false;
    assert!(!gives_way(&database, &running)());
    assert!(
        gives_way(&database, &|| true)(),
        "the engine's stop halts it"
    );

    waiting_to_map(&database, "paused", 1);
    pause(&database, "paused", now_ms() + 60 * 60 * 1000);
    assert!(
        !gives_way(&database, &running)(),
        "a step paused on its account lets the build run"
    );

    waiting_to_map(&database, "fresh", 2);
    let stop = gives_way(&database, &running);
    assert!(stop(), "a waiting step halts the build");
    assert!(stop(), "and the stop stays");
    assert_eq!(claimed(&database).as_deref(), Some("fresh"));
    assert!(
        !gives_way(&database, &running)(),
        "a claimed step halts no later build"
    );
}

#[cfg(unix)]
#[test]
fn a_mapping_rechecking_its_account_pause_never_halts_a_refresh_build() {
    let database = database();
    let running = || false;
    waiting_to_map(&database, "rechecking", 1);
    pause(&database, "rechecking", i64::MAX);
    database
        .execute(
            "UPDATE onboarding SET updated_ms=?1 WHERE project='rechecking'",
            [now_ms() - PAUSE_RECHECK_MS],
        )
        .unwrap();
    assert!(
        !gives_way(&database, &running)(),
        "a pause recheck waits one build"
    );
    assert_eq!(
        claimed(&database).as_deref(),
        Some("rechecking"),
        "the claim still rechecks it"
    );

    waiting_to_map(&database, "ended", 2);
    pause(&database, "ended", now_ms() + 60 * 60 * 1000);
    assert!(!gives_way(&database, &running)());
    database
        .execute(
            "UPDATE onboarding SET mapping_reason=?1 WHERE project='ended'",
            [(now_ms() - 1).to_string()],
        )
        .unwrap();
    assert!(
        gives_way(&database, &running)(),
        "a pause that ended halts the build"
    );
}

#[cfg(unix)]
#[test]
fn a_project_with_no_onboarding_row_gets_its_main_graph_refreshed() {
    let database = database();
    database
        .execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)")
        .unwrap();
    let projects: [(&str, &[&str]); 5] = [
        ("alone", &["/repos/alone", "/repos/second"]),
        ("bare", &[]),
        ("building", &["/repos/building"]),
        ("onboarded", &["/repos/declared"]),
        ("taken", &["/repos/taken"]),
    ];
    for (id, repositories) in projects {
        let body = serde_json::json!({
            "project_id": id,
            "spec": {"name": id, "repositories": repositories}
        });
        database
            .execute(
                "INSERT INTO projects(id,body) VALUES (?1,?2)",
                params![id, body.to_string()],
            )
            .unwrap();
    }
    for (project, created_ms) in [("onboarded", 1), ("building", 2), ("taken", 3), ("gone", 4)] {
        waiting_to_map(&database, project, created_ms);
    }
    database
        .execute(
            "UPDATE onboarding SET graph_state='waiting' WHERE project='building'",
            [],
        )
        .unwrap();
    database
        .execute(
            "UPDATE onboarding SET claimer='claim-test' WHERE project='taken'",
            [],
        )
        .unwrap();

    assert_eq!(
        refreshable(&database).unwrap(),
        [
            ("alone".to_owned(), PathBuf::from("/repos/alone")),
            ("onboarded".to_owned(), PathBuf::from("/repos/onboarded")),
        ]
    );
}

/// F25: a project that started empty and has no onboarding row is refreshable once its
/// folder is named, and every commit that lands on its main branch makes the published
/// main graph due again, so the idle loop rebuilds it at its next check (DEC-123).
#[cfg(unix)]
#[test]
fn a_landing_on_main_makes_the_published_main_graph_due_again() {
    use std::process::{Command, Stdio};
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (repo, home) = (root.join("fresh"), root.join("home"));
    for folder in [&repo, &home] {
        std::fs::create_dir(folder).unwrap();
    }
    let git = |args: &[&str]| {
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("LC_ALL", "C")
                .arg("-C")
                .arg(&repo)
                .args([
                    "-c",
                    "user.name=Owner",
                    "-c",
                    "user.email=owner@example.invalid",
                ])
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "start"]);
    std::fs::create_dir(repo.join(CRAZYAGENT_DIRECTORY)).unwrap();
    std::fs::create_dir(repo.join("graphify-out")).unwrap();
    std::fs::write(
        repo.join("graphify-out").join("graph.json"),
        "{\"nodes\":1}",
    )
    .unwrap();

    let database = database();
    database
        .execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)")
        .unwrap();
    let body = serde_json::json!({
        "project_id": "fresh",
        "spec": {"name": "fresh", "repositories": [repo.to_str().unwrap()]}
    });
    database
        .execute(
            "INSERT INTO projects(id,body) VALUES ('fresh',?1)",
            [body.to_string()],
        )
        .unwrap();
    assert_eq!(
        refreshable(&database).unwrap(),
        [("fresh".to_owned(), repo.clone())]
    );

    let hidden = main_graph::hidden_digest(&[]);
    let first = main_tip(&database, "fresh", &repo).unwrap();
    assert!(main_graph::due(None, &first, &hidden));
    let built = main_graph::Build {
        tip: first.clone(),
        built_ms: 1_000,
        hidden: hidden.clone(),
    };
    assert!(main_graph::publish(&repo, &built).unwrap());
    assert!(!main_graph::due(
        main_graph::latest(&repo).as_ref(),
        &first,
        &hidden
    ));

    git(&["commit", "-q", "--allow-empty", "-m", "landed change set"]);
    let landed = main_tip(&database, "fresh", &repo).unwrap();
    assert_ne!(landed, first);
    assert!(
        main_graph::due(main_graph::latest(&repo).as_ref(), &landed, &hidden),
        "a landing makes the published graph due again"
    );
}

#[test]
fn run_names_differ_between_onboardings_of_one_project_id_and_stay_in_bound() {
    let database = database();
    waiting_to_map(&database, "shop", 1_761_400_000_000);
    let first = claim(&database, "claim-test").unwrap().unwrap();
    assert_eq!(run_name(&first, None), "onboard-m-shop-1761400000000-1");
    assert_eq!(
        run_name(&first, Some(2)),
        "onboard-m-shop-1761400000000-1-t2"
    );
    database.execute("DELETE FROM onboarding", []).unwrap();
    waiting_to_map(&database, "shop", 1_761_500_000_000);
    let again = claim(&database, "claim-test").unwrap().unwrap();
    assert_eq!(run_name(&again, None), "onboard-m-shop-1761500000000-1");

    let long = "p".repeat(64);
    let mut widest = again.clone();
    widest.project.clone_from(&long);
    widest.created_ms = i64::MIN;
    widest.runs = i64::MIN;
    let name = run_name(&widest, None);
    assert_eq!(name.len(), RUN_NAME_BYTES_MAX);
    assert!(name.ends_with(&format!("-{}-{}", i64::MIN, i64::MIN)));
    let tried = run_name(&widest, Some(MAPPING_TRIES));
    assert_eq!(tried.len(), RUN_NAME_BYTES_MAX);
    assert!(tried.ends_with(&format!("-{}-{}-t3", i64::MIN, i64::MIN)));
    widest.created_ms = 1_761_500_000_000;
    widest.runs = 1;
    assert_eq!(
        run_name(&widest, None),
        format!("onboard-m-{long}-1761500000000-1")
    );
}

const TABLE_BEFORE_TRIES: &str = "CREATE TABLE onboarding (
    project TEXT PRIMARY KEY,
    repo TEXT NOT NULL UNIQUE,
    pool TEXT NOT NULL,
    graph_state TEXT NOT NULL,
    graph_reason TEXT,
    mapping_state TEXT NOT NULL,
    mapping_reason TEXT,
    drafted TEXT NOT NULL,
    runs INTEGER NOT NULL DEFAULT 0,
    claimer TEXT,
    heartbeat_ms INTEGER,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
);";

#[test]
fn a_running_mapping_shows_its_try_and_why_the_earlier_failed_and_an_older_table_gains_the_columns()
{
    let mut database = Connection::open_in_memory().unwrap();
    crate::storage::plugins::initialize(&database).unwrap();
    crate::storage::setup::initialize(&database).unwrap();
    crate::project_setup::initialize(&database).unwrap();
    database.execute_batch(TABLE_BEFORE_TRIES).unwrap();
    crate::onboarding::initialize(&database).unwrap();
    waiting_to_map(&database, "shop", 1_761_400_000_000);
    let shown = |database: &mut Connection| {
        let tx = database.transaction().unwrap();
        let view =
            crate::onboarding::status(&tx, &crate::protocol::ProjectId::new("shop").unwrap());
        assert!(view.is_ok(), "{view:?}");
        view.unwrap().unwrap().mapping_try
    };
    let claimed = claim(&database, "claim-test");
    assert!(matches!(claimed, Ok(Some(_))), "{claimed:?}");
    let claimed = claimed.unwrap().unwrap();
    assert_eq!(shown(&mut database), None);
    let before = now_ms();
    assert_eq!(
        record_try(&database, "claim-test", &claimed, 1, None),
        Ok(())
    );
    assert_eq!(
        record_try(
            &database,
            "claim-test",
            &claimed,
            2,
            Some("Mapping agent ran out of time")
        ),
        Ok(())
    );
    let running = shown(&mut database);
    assert!(running.is_some(), "the running try is not shown");
    let running = running.unwrap();
    assert!((before..=now_ms()).contains(&running.started_ms));
    assert_eq!(
        running,
        crate::onboarding::MappingTry {
            number: 2,
            tries: MAPPING_TRIES,
            started_ms: running.started_ms,
            earlier: Some("Mapping agent ran out of time".into()),
        }
    );
    assert_eq!(
        record_try(&database, "another-worker", &claimed, 3, Some("other")),
        Ok(())
    );
    assert_eq!(shown(&mut database).unwrap().number, 2);
    finish(
        &database,
        "claim-test",
        &claimed,
        &Step::failed("Mapping agent failed 2 tries: Mapping agent ran out of time"),
        None,
    )
    .unwrap();
    assert_eq!(shown(&mut database), None);
    database
        .execute(
            "UPDATE onboarding SET mapping_state='waiting',mapping_reason=NULL",
            [],
        )
        .unwrap();
    claim(&database, "claim-test").unwrap().unwrap();
    assert_eq!(shown(&mut database), None);
}

#[test]
fn a_step_whose_every_try_failed_names_its_tries_and_their_reasons() {
    let timed_out = "Mapping agent ran out of time".to_owned();
    let stopped = "Agent stopped: The model closed-model does not exist".to_owned();
    assert_eq!(tries_failed(std::slice::from_ref(&timed_out)), timed_out);
    assert_eq!(
        tries_failed(&[timed_out.clone(), timed_out.clone(), timed_out.clone()]),
        "Mapping agent failed 3 tries: Mapping agent ran out of time"
    );
    assert_eq!(
        tries_failed(&[timed_out.clone(), stopped, timed_out]),
        "Mapping agent failed 3 tries: try 1: Mapping agent ran out of time; try 2: Agent stopped: The model closed-model does not exist; try 3: Mapping agent ran out of time"
    );
}

#[test]
fn an_unreadable_row_fails_plainly_instead_of_being_claimed_again_and_again() {
    let database = database();
    waiting_to_map(&database, "broken", 1);
    waiting_to_map(&database, "shop", 2);
    database
        .execute(
            "UPDATE onboarding SET drafted='{\"from\":\"another engine\"}' WHERE project='broken'",
            [],
        )
        .unwrap();

    assert_eq!(
        claim(&database, "claim-test").unwrap().map(|c| c.project),
        None
    );
    let row: (String, Option<String>, Option<String>, i64) = database
        .query_row(
            "SELECT mapping_state,mapping_reason,claimer,runs FROM onboarding WHERE project='broken'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(row, ("failed".into(), Some(ROW_UNREADABLE.into()), None, 0));
    assert_eq!(claimed(&database).as_deref(), Some("shop"));
    assert_eq!(recover_stale(&database).unwrap(), 0);
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftedOn1814 {
    name: String,
    draft: crate::project_setup::ProjectSetup,
    facts: FactsOn1814,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FactsOn1814 {
    languages: Vec<String>,
    build_commands: Vec<String>,
    test_commands: Vec<String>,
    entry_points: Vec<String>,
}

#[test]
fn a_row_with_documents_found_still_reads_on_18_14_and_the_view_keeps_them() {
    let mut database = database();
    waiting_to_map(&database, "mapped", 1);
    let claimed = claim(&database, "claim-test").unwrap().unwrap();
    let mut drafted = claimed.drafted.clone();
    drafted.facts.languages = vec!["Rust".into()];
    drafted.facts.research = vec![crate::onboarding::FoundResearch {
        kind: crate::project_setup::ResearchKind::CompetitorResearch,
        files: vec!["research/landscape.md".into()],
    }];
    drafted.facts.documents = vec!["docs/plan.md".into()];
    assert_eq!(
        finish(
            &database,
            "claim-test",
            &claimed,
            &Step::Done,
            Some(&drafted)
        ),
        Ok(())
    );

    let row = database
        .query_row(
            "SELECT graph_state,graph_reason,mapping_state,mapping_reason,drafted,repo
             FROM onboarding WHERE project=?1",
            ["mapped"],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!((row.2.as_str(), row.5.as_str()), ("done", "/repos/mapped"));
    let old = serde_json::from_str::<DraftedOn1814>(&row.4);
    assert!(old.is_ok(), "18.14 cannot decode the drafted row: {old:?}");
    let old = old.unwrap();
    assert_eq!(
        (old.name.as_str(), &old.draft),
        (drafted.name.as_str(), &drafted.draft)
    );
    assert_eq!(
        [
            old.facts.languages,
            old.facts.build_commands,
            old.facts.test_commands,
            old.facts.entry_points
        ],
        [vec!["Rust".to_owned()], vec![], vec![], vec![]]
    );

    let shown = |database: &mut Connection| {
        let tx = database.transaction().unwrap();
        let view =
            crate::onboarding::status(&tx, &crate::protocol::ProjectId::new("mapped").unwrap());
        assert!(view.is_ok(), "{view:?}");
        let facts = view.unwrap().unwrap().facts;
        (facts.research, facts.documents)
    };
    let found = (
        drafted.facts.research.clone(),
        drafted.facts.documents.clone(),
    );
    assert_eq!(shown(&mut database), found);

    database
        .execute(
            "UPDATE onboarding SET mapping_state='waiting' WHERE project='mapped'",
            [],
        )
        .unwrap();
    let again = claim(&database, "claim-test").unwrap().unwrap();
    assert!(again.drafted.facts.research.is_empty());
    assert_eq!(
        finish(
            &database,
            "claim-test",
            &again,
            &Step::Done,
            Some(&again.drafted)
        ),
        Ok(())
    );
    assert_eq!(
        shown(&mut database),
        found,
        "a later draft naming nothing keeps the earlier map"
    );
}

/// While a project move is pending for a project's folder, its waiting step is not claimed;
/// it is once the move is finished or forgotten.
#[cfg(unix)]
#[test]
fn a_step_of_a_project_whose_folder_is_being_moved_is_not_claimed() {
    let database = database();
    crate::projects_home::initialize(&database).unwrap();
    waiting_to_map(&database, "alpha", 1);
    database
        .execute(
            "INSERT INTO projects_home_project_move VALUES (1,'request-1',1,'alpha','/repos/alpha','/home/alpha','move')",
            [],
        )
        .unwrap();
    assert_eq!(claimed(&database), None);
    database
        .execute("DELETE FROM projects_home_project_move", [])
        .unwrap();
    assert_eq!(claimed(&database).as_deref(), Some("alpha"));
}
