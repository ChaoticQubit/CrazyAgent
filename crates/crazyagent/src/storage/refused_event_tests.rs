use super::*;
use crate::engine::owner::{OwnerMutation, OwnerOperation, OwnerRequest, OwnerResult, Started};
use crate::protocol::{Cursor, PROTOCOL_VERSION, RequestId};
use crate::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crate::sdk_slice::{self, BUNDLED, EngineRefusals, KIND_CALL_REFUSED};
use crate::state_log::Aggregate;
use crate::state_log::fence;
use crate::state_log::replay::replay;
use bus_gateway::{Grants, Refusals};
use crazyagent_plugin_agent_quit as agent_quit;
use rusqlite::Connection;
use sdk::settings::SAVE_QUIT;
use sdk::{Refusal, Tier};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// The one `CallRefused` body a refused agent-quit save logs: ids only, no settings value.
const BODY: &[u8] =
    br#"{"plugin":"agent-quit","capability":"settings.save_quit","needs":"settings.write"}"#;

/// An engine store opened as the engine opens it, with its owner started and its host
/// running in this process.
struct Store {
    _root: tempfile::TempDir,
    path: PathBuf,
    config: EffectiveConfig,
    db: Connection,
    _lock: EngineLock,
    started: Started,
}

/// Opens a fresh [`Store`]. Output: the store.
fn store() -> Store {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("db");
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (mut db, lock) = open_database(&path, false).unwrap();
    let started = owner_start(&mut db).unwrap();
    fence::host_started(&started.host);
    Store {
        _root: root,
        path,
        config,
        db,
        _lock: lock,
        started,
    }
}

/// The quit settings the rows save: stop, never, 12 hours.
fn stop_never_12() -> QuitSettings {
    QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::Never,
        hours: 12,
    }
}

/// The owner's save of [`stop_never_12`] at the store's revision. Inputs: the store and the
/// request id. Output: the request.
fn quit_request(db: &mut Connection, id: &str) -> OwnerRequest {
    let revision = owner_revision(&db.transaction().unwrap()).unwrap();
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::SetQuitSettings {
                settings: stop_never_12(),
            },
        },
    }
}

/// Every `plugins` event's kind, schema and body, in seq order, none when the store has no
/// log yet (the first append creates it). Input: the store. Output: the rows.
fn plugin_events(db: &Connection) -> Vec<(String, i64, Vec<u8>)> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !exists {
        return Vec::new();
    }
    db.prepare("SELECT kind, schema, body FROM state_log WHERE aggregate='plugins' ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The store's `state` revision. Input: the store. Output: the revision.
fn revision(db: &Connection) -> i64 {
    db.query_row("SELECT revision FROM state WHERE id=1", [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// The agent-quit settings rows as text, in key order. Input: the store. Output: the rows.
fn quit_rows(db: &Connection) -> Vec<String> {
    db.prepare("SELECT * FROM agent_quit_settings")
        .unwrap()
        .query_map([], |row| {
            let columns = row.as_ref().column_count();
            let cells: Vec<String> = (0..columns)
                .map(|index| format!("{:?}", row.get_ref(index).unwrap()))
                .collect();
            Ok(cells.join("|"))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Queues one first refusal of the agent-quit save in a sink, as the gateway reports it.
/// Input: the sink. Output: none.
fn refuse(sink: &EngineRefusals) {
    let refusal = Refusal::NotGranted {
        capability: SAVE_QUIT.name,
        needs: SAVE_QUIT.needs,
    };
    sink.refused(BUNDLED[0].0, &SAVE_QUIT, &refusal);
}

#[test]
fn a_first_refusal_is_logged_once_per_run() {
    let mut s = store();
    let grants = Grants::new(&[(BUNDLED[0].0, Tier::T2, &[])]).unwrap();
    let sink = EngineRefusals::new();
    for _ in 0..2 {
        let tx = s.db.transaction().unwrap();
        let at = owner_revision(&tx).unwrap();
        let result =
            sdk_slice::save_quit_with(agent_quit::save, &grants, &sink, &tx, at, &stop_never_12());
        assert_eq!(
            result.err().map(|error| error.0).as_deref(),
            Some("plugin_call_refused")
        );
        drop(tx);
        sdk_slice::drain_refusals(&mut s.db, &s.started.host, &sink).unwrap();
    }
    assert_eq!(sink.count(BUNDLED[0].0), 2);
    assert_eq!(
        plugin_events(&s.db),
        vec![(KIND_CALL_REFUSED.to_owned(), 1, BODY.to_vec())]
    );
    assert_eq!(
        sdk_slice::drain_refusals(&mut s.db, &s.started.host, &sink).unwrap(),
        0
    );
}

#[test]
fn a_refusal_event_survives_the_refused_requests_rollback() {
    let mut s = store();
    let sink = EngineRefusals::new();
    refuse(&sink);
    let before = revision(&s.db);
    let request = quit_request(&mut s.db, "refused-quit");
    let admission = &s.started.owners[0].admission;
    let done = Ok(Vec::new());
    let result = owner_apply_draining(
        &mut s.db,
        &s.config,
        &s.path,
        admission,
        &request,
        Some(Effect::Delete(&done)),
        &sink,
    );
    assert_eq!(
        result.err().map(|error| error.0).as_deref(),
        Some("delete_record_invalid"),
        "the request fails inside its transaction and rolls back"
    );
    assert!(quit_rows(&s.db).is_empty(), "the request saved nothing");
    assert_eq!(
        plugin_events(&s.db),
        vec![(KIND_CALL_REFUSED.to_owned(), 1, BODY.to_vec())]
    );
    assert_eq!(revision(&s.db), before + 1, "only the event's own revision");
    assert_eq!(
        sdk_slice::drain_refusals(&mut s.db, &s.started.host, &sink).unwrap(),
        0
    );
}

#[test]
fn no_projection_replays_the_plugins_aggregate() {
    let mut s = store();
    let request = quit_request(&mut s.db, "saved-quit");
    let admission = &s.started.owners[0].admission;
    let applied =
        owner_apply_with(&mut s.db, &s.config, &s.path, admission, &request, None).unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);
    let saved = quit_rows(&s.db);
    assert_eq!(saved.len(), 1);
    let sink = EngineRefusals::new();
    refuse(&sink);
    assert_eq!(
        sdk_slice::drain_refusals(&mut s.db, &s.started.host, &sink).unwrap(),
        1
    );
    let tx = s.db.transaction().unwrap();
    crate::settings_log::rebuild(&tx).unwrap();
    assert_eq!(quit_rows(&tx), saved, "the settings rebuild is unchanged");
    drop(tx);
    let mut plugins = Vec::new();
    replay(&s.db, Aggregate::Plugins, 0, |logged| {
        plugins.push(logged.kind.clone());
        Ok(())
    })
    .unwrap();
    assert_eq!(plugins, vec![KIND_CALL_REFUSED.to_owned()]);
    for aggregate in [
        Aggregate::Worktrees,
        Aggregate::Ide,
        Aggregate::Memory,
        Aggregate::Projects,
        Aggregate::Landing,
        Aggregate::Settings,
        Aggregate::Sessions,
        Aggregate::Intake,
        Aggregate::Engine,
        Aggregate::Accounts,
        Aggregate::Team,
    ] {
        replay(&s.db, aggregate, 0, |logged| {
            assert_ne!(logged.kind, KIND_CALL_REFUSED, "{aggregate:?}");
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn a_refusal_queued_before_a_committed_request_is_logged() {
    let mut s = store();
    let sink = EngineRefusals::new();
    refuse(&sink);
    let request = quit_request(&mut s.db, "committed-quit");
    let admission = &s.started.owners[0].admission;
    let applied = owner_apply_draining(
        &mut s.db, &s.config, &s.path, admission, &request, None, &sink,
    )
    .unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);
    assert_eq!(quit_rows(&s.db).len(), 1, "the request committed its save");
    assert_eq!(
        plugin_events(&s.db),
        vec![(KIND_CALL_REFUSED.to_owned(), 1, BODY.to_vec())]
    );
    assert_eq!(
        sdk_slice::drain_refusals(&mut s.db, &s.started.host, &sink).unwrap(),
        0
    );
}
