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
fn run_names_differ_between_onboardings_of_one_project_id_and_stay_in_bound() {
    let database = database();
    waiting_to_map(&database, "shop", 1_761_400_000_000);
    let first = claim(&database, "claim-test").unwrap().unwrap();
    assert_eq!(run_name(&first), "onboard-m-shop-1761400000000-1");
    database.execute("DELETE FROM onboarding", []).unwrap();
    waiting_to_map(&database, "shop", 1_761_500_000_000);
    let again = claim(&database, "claim-test").unwrap().unwrap();
    assert_eq!(run_name(&again), "onboard-m-shop-1761500000000-1");

    let long = "p".repeat(64);
    let mut widest = again.clone();
    widest.project.clone_from(&long);
    widest.created_ms = i64::MIN;
    widest.runs = i64::MIN;
    let name = run_name(&widest);
    assert_eq!(name.len(), RUN_NAME_BYTES_MAX);
    assert!(name.ends_with(&format!("-{}-{}", i64::MIN, i64::MIN)));
    widest.created_ms = 1_761_500_000_000;
    widest.runs = 1;
    assert_eq!(
        run_name(&widest),
        format!("onboard-m-{long}-1761500000000-1")
    );
}
