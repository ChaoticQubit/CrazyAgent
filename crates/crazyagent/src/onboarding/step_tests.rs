use super::*;
use crate::setup::pack::GRAPH_ENTRY;

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crate::storage::plugins::initialize(&connection).unwrap();
    crate::storage::setup::initialize(&connection).unwrap();
    initialize(&connection).unwrap();
    connection
}

fn graphify(tx: &Transaction<'_>, enabled: bool) {
    tx.execute(
        "INSERT INTO plugin_items(id, kind, scope, name, body) VALUES('skill-1', 'skill', 'global', ?1, ?2)
         ON CONFLICT(kind, scope, name) DO UPDATE SET body=excluded.body",
        [GRAPH_ENTRY, &format!("{{\"enabled\":{enabled}}}")],
    )
    .unwrap();
}

fn stored(tx: &Transaction<'_>, project: &ProjectId) -> Step {
    let (state, reason): (String, Option<String>) = tx
        .query_row(
            "SELECT graph_state, graph_reason FROM onboarding WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    Step::from_columns(&state, reason).unwrap()
}

fn onboarded(tx: &Transaction<'_>, graph: &Step) -> ProjectId {
    let (state, reason) = graph.columns();
    tx.execute(
        "INSERT INTO onboarding(project,repo,pool,graph_state,graph_reason,mapping_state,drafted,created_ms,updated_ms)
         VALUES ('shop','/repos/shop','main',?1,?2,'done','{}',1,1)",
        params![state, reason],
    )
    .unwrap();
    ProjectId::new("shop").unwrap()
}

#[test]
fn every_step_reads_back_from_its_stored_columns() {
    for step in [
        Step::Waiting,
        Step::Running,
        Step::Done,
        Step::Off,
        Step::NotInstalled,
        Step::failed("graph build failed"),
    ] {
        let (state, reason) = step.columns();
        assert_eq!(
            Step::from_columns(state, reason.map(str::to_owned)).unwrap(),
            step
        );
    }
    assert_eq!(
        Step::NotInstalled.columns(),
        ("off", Some("graphify_not_installed")),
        "a missing skill is stored as off, so every query that counts off as settled still does"
    );
    assert_eq!(
        Step::from_columns("off", Some("something_else".into()))
            .unwrap_err()
            .0,
        "onboarding_step_invalid"
    );
}

#[test]
fn a_missing_graphify_skill_reaches_clients_as_not_installed() {
    assert_eq!(
        serde_json::to_string(&Step::NotInstalled).unwrap(),
        r#"{"kind":"not_installed"}"#
    );
    assert_eq!(
        serde_json::from_str::<Step>(r#"{"kind":"not_installed"}"#).unwrap(),
        Step::NotInstalled
    );
    assert_eq!(serde_json::to_string(&Step::Off).unwrap(), r#"{"kind":"off"}"#);
}

#[test]
fn the_graph_step_starts_from_whether_graphify_is_installed_and_on() {
    let mut connection = database();
    let tx = connection.transaction().unwrap();
    assert_eq!(graph_step(&tx).unwrap(), Step::NotInstalled);
    graphify(&tx, false);
    assert_eq!(graph_step(&tx).unwrap(), Step::Off);
    graphify(&tx, true);
    assert_eq!(graph_step(&tx).unwrap(), Step::Waiting);
}

#[test]
fn a_restart_stores_why_the_graph_is_not_built() {
    let mut connection = database();
    let tx = connection.transaction().unwrap();
    let project = onboarded(&tx, &Step::Off);
    let pool = CredentialRef::new("main").unwrap();
    restart(&tx, &project, &pool, Step::NotInstalled).unwrap();
    assert_eq!(stored(&tx, &project), Step::NotInstalled);
    restart(&tx, &project, &pool, Step::Off).unwrap();
    assert_eq!(stored(&tx, &project), Step::Off);
    restart(&tx, &project, &pool, Step::NotInstalled).unwrap();
    restart(&tx, &project, &pool, Step::Waiting).unwrap();
    assert_eq!(stored(&tx, &project), Step::Waiting);
    restart(&tx, &project, &pool, Step::NotInstalled).unwrap();
    assert_eq!(
        stored(&tx, &project),
        Step::Waiting,
        "a waiting graph keeps waiting"
    );
}
