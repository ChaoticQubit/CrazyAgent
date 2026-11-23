use bus_gateway::Switches;
use kernel_plugin_host::registry::Switch;
use kernel_plugin_host::usage::{TurnOff, Usage};
use rusqlite::Connection;

use super::{OffSet, engine, initialize};

/// The agent-quit plugin's id, from the generated grant table.
const QUIT: &str = kernel_plugin_host::generated::permissions::BUNDLED_GRANTS[0].plugin;

/// Turning a code plugin off with the essential lock lifted or not. Inputs: whether it is
/// lifted and the usage table. Output: the switch.
fn off(unlocked: bool, usage: &Usage) -> Switch<'_> {
    Switch::Off {
        usage,
        when_off: TurnOff::RefuseNewCalls,
        essential_unlocked: unlocked,
    }
}

/// A database with the plugin store's tables. Output: the connection.
fn plugin_store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    db
}

#[test]
fn an_unloaded_off_set_refuses_every_code_plugin() {
    let set = OffSet::new();
    assert!(!set.is_on(QUIT));
    assert!(!set.is_on("any-plugin"));
    set.load(&plugin_store());
    assert!(set.is_on(QUIT));
    set.load(&Connection::open_in_memory().unwrap());
    assert!(!set.is_on(QUIT), "a load that fails leaves the set closed");
}

#[test]
fn the_off_set_changes_only_after_the_switch_commits() {
    let temp = tempfile::tempdir().unwrap();
    let db = plugin_store();
    let usage = Usage::default();
    let set = OffSet::new();
    set.load(&db);
    let code = |result: Result<(), crate::engine::EngineError>| result.err().map(|error| error.0);
    assert_eq!(
        code(set.switch(&db, &engine(temp.path()), QUIT, off(false, &usage))),
        Some("app_plugin_essential".to_owned())
    );
    assert!(set.is_on(QUIT), "a refused switch leaves the set");
    db.execute_batch("BEGIN").unwrap();
    assert_eq!(
        code(set.switch(&db, &engine(temp.path()), QUIT, off(true, &usage))),
        Some("app_plugins_unavailable".to_owned())
    );
    assert!(set.is_on(QUIT), "a switch inside an open transaction is refused");
    db.execute_batch("ROLLBACK").unwrap();
    set.switch(&db, &engine(temp.path()), QUIT, off(true, &usage))
        .unwrap();
    assert!(!set.is_on(QUIT));
    let fresh = OffSet::new();
    fresh.load(&db);
    assert!(!fresh.is_on(QUIT), "the committed off loads at the next start");
    set.switch(&db, &engine(temp.path()), QUIT, Switch::On)
        .unwrap();
    assert!(set.is_on(QUIT));
}
