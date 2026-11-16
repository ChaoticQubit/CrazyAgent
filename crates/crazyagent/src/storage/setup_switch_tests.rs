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
        hooks: Vec::new(),
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

/// WIN-PACK-SEED: a pack server entry installs as its approved, switched-on server on every
/// system, Windows included, from a spec with this system's full command path.
#[test]
fn a_pack_server_entry_installs_its_approved_server_on_every_system() {
    let root = tempfile::tempdir().unwrap();
    let mut connection = database();
    let command = if cfg!(windows) {
        r"C:\Program Files\nodejs\npx.cmd"
    } else {
        "/usr/local/bin/npx"
    };
    let spec = ServerSpec::Stdio {
        command: command.into(),
        args: vec!["-y".into(), "context-mode@1.0.169".into()],
    };
    let install = Command::Install {
        id: "context-mode".into(),
        source: "https://github.com/mksglu/context-mode@589d8214d56740a28b5f7bf63167743d586b0b40/"
            .into(),
        skills: Vec::new(),
        server: Some(("context-mode".into(), spec.clone())),
    };
    execute(&mut connection, root.path(), install).unwrap();
    let tx = connection.transaction().unwrap();
    let record = load_pack(&tx, "context-mode").unwrap().unwrap();
    assert!(record.enabled && record.skills.is_empty());
    let server = record.server.unwrap();
    let body: String = tx
        .query_row(
            "SELECT body FROM plugin_items WHERE id=?1 AND kind='server'",
            [&server],
            |row| row.get(0),
        )
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["spec"], serde_json::to_value(&spec).unwrap());
    assert_eq!(body["approved"], spec.digest());
    assert_eq!(entry_switch(&tx, "context-mode").unwrap(), Some(true));
}
