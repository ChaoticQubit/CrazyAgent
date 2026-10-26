use super::*;

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    plugins::initialize(&connection).unwrap();
    initialize(&connection).unwrap();
    connection
}

fn skill(tx: &Transaction<'_>, name: &str, enabled: bool) {
    tx.execute(
        "INSERT INTO plugin_items(id, kind, scope, name, body) VALUES(?1, 'skill', 'global', ?2, ?3)
         ON CONFLICT(kind, scope, name) DO UPDATE SET body=excluded.body",
        [&format!("skill-{name}"), name, &format!("{{\"enabled\":{enabled}}}")],
    )
    .unwrap();
}

#[test]
fn the_graph_entry_is_not_installed_until_the_plugin_store_or_a_pack_row_has_it() {
    let mut connection = database();
    let tx = connection.transaction().unwrap();
    assert_eq!(entry_switch(&tx, GRAPH_ENTRY).unwrap(), None);
    skill(&tx, "other", true);
    assert_eq!(entry_switch(&tx, GRAPH_ENTRY).unwrap(), None);
    skill(&tx, GRAPH_ENTRY, false);
    assert_eq!(entry_switch(&tx, GRAPH_ENTRY).unwrap(), Some(false));
    skill(&tx, GRAPH_ENTRY, true);
    assert_eq!(entry_switch(&tx, GRAPH_ENTRY).unwrap(), Some(true));
    assert_eq!(entry_switch(&tx, "not-in-the-pack").unwrap(), None);
}

#[test]
fn a_pack_row_decides_the_switch_over_the_plugin_store() {
    let mut connection = database();
    let tx = connection.transaction().unwrap();
    skill(&tx, GRAPH_ENTRY, true);
    let record = PackRecord {
        id: GRAPH_ENTRY.into(),
        source: PRESENT.into(),
        skills: vec![format!("skill-{GRAPH_ENTRY}")],
        server: None,
        enabled: false,
    };
    save_pack(&tx, &record).unwrap();
    assert_eq!(entry_switch(&tx, GRAPH_ENTRY).unwrap(), Some(false));
    save_pack(
        &tx,
        &PackRecord {
            enabled: true,
            ..record
        },
    )
    .unwrap();
    assert_eq!(entry_switch(&tx, GRAPH_ENTRY).unwrap(), Some(true));
}

#[test]
fn a_damaged_plugin_store_body_is_refused_not_read_as_off() {
    let mut connection = database();
    let tx = connection.transaction().unwrap();
    tx.execute(
        "INSERT INTO plugin_items(id, kind, scope, name, body) VALUES('skill-1', 'skill', 'global', ?1, '{}')",
        [GRAPH_ENTRY],
    )
    .unwrap();
    assert_eq!(
        entry_switch(&tx, GRAPH_ENTRY).unwrap_err().0,
        "setup_record_invalid"
    );
}
