use super::*;
use crate::cleanup_settings::{CleanupMode, CleanupSettings};
use crate::sandbox_settings::SandboxSettings;
use crate::settings_log::{self, Event, ProjectSaved};

/// The two per-project settings tables a removal clears.
const PROJECT_TABLES: [&str; 2] = ["agent_instruction_settings", "roadmap_status_settings"];

/// Both per-project settings tables of a store, each ordered by all its columns. Input: the
/// store. Output: the rows.
fn project_rows(db: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    PROJECT_TABLES
        .iter()
        .map(|table| rows(db, table).unwrap())
        .collect()
}

/// v2.5 L10 Q4: a removal tombstones the removed project's instruction and roadmap settings
/// events, keeping the other project's, and the settings replay brings back no row of the
/// removed project, even from a snapshot run taken before the removal that still holds them,
/// since replay applies the removal's delete at its place in the log.
#[test]
fn a_removal_tombstones_its_project_s_settings_and_their_replay_brings_none_back() {
    let mut s = store();
    settings_log::initialize(&s.db).unwrap();
    let engine = || fence::Fence::Engine(s.started.host.clone());
    let tx = s.db.transaction().unwrap();
    let mut appender = crate::state_log::Appender::own(&tx, engine()).unwrap();
    for project in [REMOVED, OTHER] {
        let project = ProjectId::new(project).unwrap();
        let instruction = Event::Instruction(ProjectSaved {
            project: project.clone(),
            settings: crate::instruction_files::Settings::default(),
        });
        settings_log::record(&mut appender, 1, &instruction).unwrap();
        let roadmap = Event::Roadmap(ProjectSaved {
            project,
            settings: crate::roadmap_status::Refresh::Off,
        });
        settings_log::record(&mut appender, 1, &roadmap).unwrap();
    }
    drop(appender);
    tx.commit().unwrap();
    let tx = s.db.transaction().unwrap();
    let mut appender = crate::state_log::Appender::own(&tx, engine()).unwrap();
    settings_log::snapshot(&mut appender, 2).unwrap();
    drop(appender);
    tx.commit().unwrap();

    let request = remove_request(&mut s.db);
    let admission = &s.started.owners[0].admission;
    let applied = owner_apply(&mut s.db, &s.config, &s.path, admission, &request).unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);

    let events = logged(&s.db);
    let saved: Vec<&Vec<u8>> = events
        .iter()
        .filter(|(_, _, kind, _, _)| {
            kind == settings_log::KIND_INSTRUCTION || kind == settings_log::KIND_ROADMAP
        })
        .map(|(_, _, _, _, body)| body)
        .collect();
    assert_eq!(saved.len(), 4);
    assert_eq!(saved[0].as_slice(), crate::state_log::TOMBSTONE);
    assert_eq!(saved[1].as_slice(), crate::state_log::TOMBSTONE);
    assert!(String::from_utf8_lossy(saved[2]).contains("\"other\""));
    assert!(String::from_utf8_lossy(saved[3]).contains("\"other\""));

    let live = project_rows(&s.db);
    assert!(
        live.iter().all(|table| table.len() == 1),
        "only the other project's rows stand: {live:?}"
    );
    let tx = s.db.transaction().unwrap();
    settings_log::rebuild(&tx).unwrap();
    assert_eq!(project_rows(&tx), live);
}

/// v2.5 L10 owner command row on the owner's own path: an applied settings save logs its
/// one event at the revision the request committed, whose `events` row is the request's
/// own; a sandbox save from a stale read is refused and logs nothing.
#[test]
fn an_owner_settings_save_logs_its_event_at_the_request_s_revision_and_a_stale_one_none() {
    let mut s = store();
    settings_log::initialize(&s.db).unwrap();
    let admission = s.started.owners[0].admission.clone();
    let settings = CleanupSettings {
        mode: CleanupMode::Manual,
        days: 3,
    };
    let cleanup = request(
        &mut s.db,
        "cleanup",
        OwnerMutation::SetCleanupSettings { settings },
    );
    let applied = owner_apply(&mut s.db, &s.config, &s.path, &admission, &cleanup).unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);
    let next = i64::try_from(applied.provenance.revision_after.value()).unwrap();
    let events = logged(&s.db);
    let (_, aggregate, kind, revision, _) = events.last().unwrap();
    assert_eq!(
        (aggregate.as_str(), kind.as_str(), *revision),
        ("settings", settings_log::KIND_CLEANUP, next)
    );
    let published: i64 =
        s.db.query_row(
            "SELECT COUNT(*) FROM events WHERE cursor=?1",
            [next],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(published, 1, "the request's own events row");

    let fresh = SandboxSettings {
        linux_sockets: crate::sandbox::LinuxSockets::TcpPorts,
        ..SandboxSettings::default()
    };
    let stale = request(
        &mut s.db,
        "sandbox-stale",
        OwnerMutation::SetSandboxSettings {
            settings: fresh,
            version: 9,
        },
    );
    let refused = owner_apply(&mut s.db, &s.config, &s.path, &admission, &stale).unwrap();
    assert_eq!(
        refused.result,
        OwnerResult::Rejected {
            code: crate::storage::versioned::STALE_CODE.into()
        }
    );
    assert_eq!(
        logged(&s.db).len(),
        events.len(),
        "a refused save logs none"
    );

    let sandbox = request(
        &mut s.db,
        "sandbox",
        OwnerMutation::SetSandboxSettings {
            settings: fresh,
            version: 0,
        },
    );
    let applied = owner_apply(&mut s.db, &s.config, &s.path, &admission, &sandbox).unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);
    let (_, _, kind, _, _) = logged(&s.db).pop().unwrap();
    assert_eq!(kind, settings_log::KIND_SANDBOX);
    let tx = s.db.transaction().unwrap();
    let live = rows(&tx, "sandbox_settings").unwrap();
    settings_log::rebuild(&tx).unwrap();
    assert_eq!(rows(&tx, "sandbox_settings").unwrap(), live);
}
