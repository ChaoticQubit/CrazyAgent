use super::*;
use crate::cleanup_settings::{CleanupMode, CleanupSettings};
use crate::harnesses::update::{
    CheckWhen, InstallFromApp, OnlineLookup, UpdateMode, UpdateSettings,
};
use crate::instruction_files::{AppSettings, Mode, Settings};
use crate::master_frame_limit::{FrameSize, MasterFrameSettings};
use crate::master_step_limit::{MasterStepSettings, StepLimit};
use crate::master_time_limit::{MasterTimeSettings, TimeLimit};
use crate::protocol::{Command, Outcome, ProjectId};
use crate::provider_retry::ProviderRetry;
use crate::quit_settings::{LeftRunning, OnQuit, QuitSettings};
use crate::roadmap_status::Refresh;
use crate::sandbox::LinuxSockets;
use crate::sandbox_settings::SandboxSettings;
use crate::state_log::fence::{engine, seed};
use crate::state_log::replay::{Logged, redact, rows};
use crate::state_log::{Aggregate, Appender, SNAPSHOT_CHUNK};
use crate::storage::opening::Opening;
use rusqlite::Connection;
use rusqlite::types::Value;

/// The `state` row at revision 0 and the `events` table a store holds before any write.
const STATE: &str =
    "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
    CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
    INSERT INTO state VALUES (1,0);";

/// A store with the `state` row at revision 0, the owner tables holding the test host
/// started, and every settings table. Output: the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(STATE).unwrap();
    seed(&db);
    initialize(&db).unwrap();
    db
}

/// A store no engine host runs: the `state` and `events` tables and every settings table.
/// Output: the connection.
fn unhosted() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(STATE).unwrap();
    initialize(&db).unwrap();
    db
}

/// Every settings table in snapshot order, each ordered by all its columns. Input: the
/// store. Output: the tables' rows.
fn tables(db: &Connection) -> Vec<Vec<Vec<Value>>> {
    TABLES
        .iter()
        .map(|(table, _)| rows(db, table).unwrap())
        .collect()
}

/// The refusal code of a result, none when it succeeded. Input: the result. Output: the code.
fn code<T>(result: Result<T, EngineError>) -> Option<String> {
    result.err().map(|error| error.0)
}

/// A project id. Input: the id text. Output: the id.
fn project(id: &str) -> ProjectId {
    ProjectId::new(id).unwrap()
}

/// One save of every settings kind, each away from its default so a replay that dropped it
/// would show. Output: the events, in [`TABLES`] order.
fn every() -> Vec<Event> {
    vec![
        Event::Sandbox(Saved {
            settings: SandboxSettings {
                linux_sockets: LinuxSockets::TcpPorts,
                ..SandboxSettings::default()
            },
        }),
        Event::Cleanup(Saved {
            settings: CleanupSettings {
                mode: CleanupMode::OnceLanded,
                days: 7,
            },
        }),
        Event::Quit(Saved {
            settings: QuitSettings {
                on_quit: OnQuit::Stop,
                left_running: LeftRunning::Never,
                hours: 12,
            },
        }),
        Event::Retry(Saved {
            settings: ProviderRetry::StopAtOnce,
        }),
        Event::HarnessUpdate(Saved {
            settings: UpdateSettings {
                mode: UpdateMode::CheckOnly,
                when: CheckWhen::OnOpen,
                online: OnlineLookup::Off,
                install: InstallFromApp::Off,
            },
        }),
        Event::InstructionApp(Saved {
            settings: AppSettings {
                edit_with_ai: false,
            },
        }),
        Event::Instruction(ProjectSaved {
            project: project("p1"),
            settings: Settings {
                mode: Mode::ProjectOnly,
                transcripts: false,
                ..Settings::default()
            },
        }),
        Event::Roadmap(ProjectSaved {
            project: project("p1"),
            settings: Refresh::RefreshOnly,
        }),
        Event::Steps(Saved {
            settings: MasterStepSettings {
                limit: StepLimit::Steps100,
            },
        }),
        Event::Time(Saved {
            settings: MasterTimeSettings {
                limit: TimeLimit::Minutes30,
            },
        }),
        Event::Frames(Saved {
            settings: MasterFrameSettings {
                limit: FrameSize::Mib1,
            },
        }),
    ]
}

/// Records events in one appending transaction of the engine host. Inputs: the store and
/// the events. Output: each event's seq.
fn write(db: &mut Connection, events: &[Event]) -> Vec<u64> {
    let tx = db.transaction().unwrap();
    let mut appender = Appender::own(&tx, engine()).unwrap();
    let seqs = events
        .iter()
        .map(|event| record(&mut appender, 5, event).unwrap())
        .collect();
    drop(appender);
    tx.commit().unwrap();
    seqs
}

/// The `state` revision, the logged events (0 with no log) and the `events` rows. Input: the
/// store. Output: the three counts.
fn moved(db: &Connection) -> (i64, i64, i64) {
    let revision = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let logged: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='state_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let count = |sql: &str| db.query_row(sql, [], |row| row.get(0)).unwrap();
    let events = if logged {
        count("SELECT COUNT(*) FROM state_log")
    } else {
        0
    };
    (revision, events, count("SELECT COUNT(*) FROM events"))
}

/// The kinds of the logged events in seq order, with the revision each carries. Input: the
/// store. Output: the pairs.
fn kinds(db: &Connection) -> Vec<(i64, String)> {
    db.prepare("SELECT revision, kind FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Asserts that replay of the log rebuilds every settings table as it stands. Input: the
/// store. Output: none.
fn replays_to_live(db: &mut Connection) {
    let live = tables(db);
    let tx = db.transaction().unwrap();
    rebuild(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(tables(db), live, "replay equals the live tables");
}

/// v2.5 L10 replay row: one save of every settings kind and a later save of one leave the
/// tables as replay of their events rebuilds them, and a skipped (redacted) later save shows
/// the earlier one in the rebuilt row.
#[test]
fn replay_rebuilds_every_settings_table_as_the_writes_left_them() {
    let mut db = store();
    write(&mut db, &every());
    let later = Event::Cleanup(Saved {
        settings: CleanupSettings {
            mode: CleanupMode::Manual,
            days: 30,
        },
    });
    let seqs = write(&mut db, &[later]);
    let live = tables(&db);
    assert_eq!(live.len(), TABLES.len());
    assert!(live.iter().all(|rows| rows.len() == 1), "{live:?}");

    let tx = db.transaction().unwrap();
    assert_eq!(rebuild(&tx).unwrap(), 12, "the last seq read");
    tx.commit().unwrap();
    assert_eq!(tables(&db), live, "replay equals the live tables");

    let tx = db.transaction().unwrap();
    redact(&tx, seqs[0], &engine()).unwrap();
    rebuild(&tx).unwrap();
    tx.commit().unwrap();
    let days: i64 = db
        .query_row("SELECT days FROM agent_cleanup_settings", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(days, 7, "a skipped save shows the earlier one");
}

/// v2.5 L10: a snapshot run of the tables as they stand, in more than one chunk, and the
/// events after it replay to the same rows, integer columns included; rebuilding twice
/// gives the same rows.
#[test]
fn a_snapshot_run_and_the_events_after_it_replay_to_the_same_rows() {
    let mut db = store();
    for event in every() {
        apply(&db, &event).unwrap();
    }
    for index in 0..6_000 {
        let roadmap = Event::Roadmap(ProjectSaved {
            project: project(&format!("p{index}")),
            settings: Refresh::Off,
        });
        apply(&db, &roadmap).unwrap();
    }
    let tx = db.transaction().unwrap();
    let mut appender = Appender::own(&tx, engine()).unwrap();
    let done = snapshot(&mut appender, 9).unwrap();
    let retry = Event::Retry(Saved {
        settings: ProviderRetry::HarnessDefault,
    });
    record(&mut appender, 9, &retry).unwrap();
    let instruction = Event::Instruction(ProjectSaved {
        project: project("p2"),
        settings: Settings::default(),
    });
    record(&mut appender, 9, &instruction).unwrap();
    drop(appender);
    tx.commit().unwrap();
    crate::state_log::verify_snapshot(&db, done).unwrap();
    let chunks: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM state_log WHERE kind=?1",
            [SNAPSHOT_CHUNK],
            |row| row.get(0),
        )
        .unwrap();
    let tables_count = i64::try_from(TABLES.len()).unwrap();
    assert!(
        chunks > tables_count,
        "{chunks} chunks: 6000 roadmap rows outgrow one body"
    );
    let live = tables(&db);
    assert_eq!(live[7].len(), 6_000);
    assert!(matches!(live[1][0][2], Value::Integer(7)), "{:?}", live[1]);
    for _ in 0..2 {
        let tx = db.transaction().unwrap();
        rebuild(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(tables(&db), live);
    }
}

/// v2.5 L10 and L1-N1: replay reads only the settings kinds at schema 1 with exactly their
/// fields, a project as an id and each setting as one of its known words or bounded
/// numbers; anything else, free text in any field included, is refused.
#[test]
fn replay_refuses_a_settings_event_it_does_not_know() {
    let logged = |aggregate: Aggregate, kind: &str, schema: u16, body: &str| Logged {
        seq: 1,
        revision: 1,
        aggregate,
        kind: kind.into(),
        schema,
        at_ms: 1,
        body: body.as_bytes().to_vec(),
    };
    let Event::Instruction(event) = every().swap_remove(6) else {
        unreachable!("every holds the instruction save seventh")
    };
    let good = serde_json::to_string(&event).unwrap();
    assert!(matches!(
        decode(&logged(Aggregate::Settings, KIND_INSTRUCTION, 1, &good)),
        Ok(Event::Instruction(_))
    ));
    let secret = "the owner's secret prompt with sk-ant-api03-key";
    let swapped = |from: &str, to: &str| {
        assert_eq!(good.matches(from).count(), 1, "{from}");
        good.replacen(from, to, 1)
    };
    for (aggregate, kind, schema, body) in [
        (Aggregate::Worktrees, KIND_INSTRUCTION, 1, good.clone()),
        (Aggregate::Settings, "InstructionDropped", 1, good.clone()),
        (Aggregate::Settings, KIND_INSTRUCTION, 2, good.clone()),
        (
            Aggregate::Settings,
            KIND_INSTRUCTION,
            1,
            format!("{},\"x\":1}}", &good[..good.len() - 1]),
        ),
        (Aggregate::Settings, KIND_ROADMAP, 1, good.clone()),
        (
            Aggregate::Settings,
            KIND_INSTRUCTION,
            1,
            swapped("\"project\":\"p1\"", &format!("\"project\":\"{secret}\"")),
        ),
        (
            Aggregate::Settings,
            KIND_INSTRUCTION,
            1,
            swapped("\"project_only\"", &format!("\"{secret}\"")),
        ),
        (
            Aggregate::Settings,
            KIND_INSTRUCTION,
            1,
            format!("{{\"project\":\"p1\",\"settings\":\"{secret}\"}}"),
        ),
        (
            Aggregate::Settings,
            KIND_RETRY,
            1,
            format!("{{\"settings\":\"{secret}\"}}"),
        ),
        (
            Aggregate::Settings,
            KIND_RETRY,
            1,
            "{\"settings\":\"retry_forever\"}".to_owned(),
        ),
    ] {
        assert_eq!(
            code(decode(&logged(aggregate, kind, schema, &body))),
            Some("settings_log_event_invalid".into()),
            "{kind} {schema} {body}"
        );
    }
}

/// v2.5 L10 owner command row: each owner settings save logs exactly one event of its kind
/// at the request's signed next revision, moving `state` there; a save its checks refuse
/// logs none and leaves `state` where it was.
#[test]
fn each_owner_settings_command_logs_one_event_at_the_signed_next_and_a_refused_one_none() {
    let mut db = store();
    let commands = [
        (
            Command::OwnerAgentCleanup {
                settings: CleanupSettings {
                    mode: CleanupMode::Manual,
                    days: 3,
                },
            },
            KIND_CLEANUP,
        ),
        (
            Command::OwnerAgentQuit {
                settings: QuitSettings::default(),
            },
            KIND_QUIT,
        ),
        (
            Command::OwnerProviderRetry {
                choice: ProviderRetry::StopAtOnce,
            },
            KIND_RETRY,
        ),
        (
            Command::OwnerMasterSteps {
                settings: MasterStepSettings {
                    limit: StepLimit::Steps50,
                },
            },
            KIND_STEPS,
        ),
        (
            Command::OwnerMasterTime {
                settings: MasterTimeSettings {
                    limit: TimeLimit::Minutes10,
                },
            },
            KIND_TIME,
        ),
        (
            Command::OwnerMasterFrames {
                settings: MasterFrameSettings {
                    limit: FrameSize::Kib256,
                },
            },
            KIND_FRAMES,
        ),
    ];
    for (command, kind) in &commands {
        let tx = db.transaction().unwrap();
        let before = moved(&tx).0;
        let revision = u64::try_from(before).unwrap();
        assert_eq!(owner(&tx, command, revision).unwrap(), Outcome::Applied);
        assert_eq!(moved(&tx).0, before + 1, "{kind}");
        assert_eq!(
            kinds(&tx).last(),
            Some(&(before + 1, (*kind).to_owned())),
            "{kind}"
        );
        tx.commit().unwrap();
    }
    assert_eq!(kinds(&db).len(), commands.len());
    replays_to_live(&mut db);

    let tx = db.transaction().unwrap();
    let before = moved(&tx);
    let revision = u64::try_from(before.0).unwrap();
    let quit = Command::OwnerAgentQuit {
        settings: QuitSettings {
            hours: 0,
            ..QuitSettings::default()
        },
    };
    assert_eq!(
        owner(&tx, &quit, revision).unwrap(),
        Outcome::Rejected {
            code: "quit_settings_invalid".into()
        }
    );
    let cleanup = Command::OwnerAgentCleanup {
        settings: CleanupSettings {
            mode: CleanupMode::AfterDays,
            days: 0,
        },
    };
    assert_eq!(
        code(owner(&tx, &cleanup, revision)),
        Some("cleanup_settings_invalid".into())
    );
    let other = Command::OwnerLearningMark {
        step: crate::learning::MarkStep::Terminal,
        on: true,
    };
    assert_eq!(
        code(owner(&tx, &other, revision)),
        Some("settings_log_command".into())
    );
    assert_eq!(moved(&tx), before, "a refused save moves and logs nothing");
}

/// v2.5 L10 engine writer row: the instruction, app instruction, roadmap refresh and harness
/// update writers each log one event and move `state` and its `events` row once; turning
/// the roadmap refresh off still settles the project's status row, a live effect replay does
/// not repeat.
#[test]
fn each_engine_settings_writer_logs_once_and_moves_the_revision_once() {
    let mut db = store();
    let p1 = project("p1");
    db.execute(
        "INSERT INTO roadmap_status(project,stale,running_ms) VALUES ('p1',1,5)",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    let settings = Settings {
        mode: Mode::AppOnly,
        ..Settings::default()
    };
    instruction(&tx, &p1, settings).unwrap();
    instruction_app(
        &tx,
        AppSettings {
            edit_with_ai: false,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    roadmap_refresh(&db, &p1, Refresh::Off).unwrap();
    let update = UpdateSettings {
        mode: UpdateMode::Off,
        ..UpdateSettings::default()
    };
    harness_update(&db, update).unwrap();
    assert_eq!(
        moved(&db),
        (4, 4, 4),
        "one revision, event and events row each"
    );
    let logged: Vec<String> = kinds(&db).into_iter().map(|(_, kind)| kind).collect();
    assert_eq!(
        logged,
        [
            KIND_INSTRUCTION,
            KIND_INSTRUCTION_APP,
            KIND_ROADMAP,
            KIND_HARNESS_UPDATE
        ]
    );
    let settled: (i64, Option<i64>) = db
        .query_row(
            "SELECT stale, running_ms FROM roadmap_status WHERE project='p1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(settled, (0, None), "off settles the status row");
    replays_to_live(&mut db);
}

/// v2.5 L10 and L6 Q9: before an owner runtime starts the store, every engine settings
/// writer is refused with its own code and changes nothing: no row, no revision, no event.
#[test]
fn an_engine_settings_writer_is_refused_before_an_owner_runtime_starts() {
    let mut db = unhosted();
    let p1 = project("p1");
    let tx = db.transaction().unwrap();
    assert_eq!(
        code(record_own(&tx, &every()[3])),
        Some("owner_runtime_stale".into())
    );
    assert_eq!(
        code(instruction(&tx, &p1, Settings::default())),
        Some("instruction_settings_unavailable".into())
    );
    assert_eq!(
        code(instruction_app(&tx, AppSettings::default())),
        Some("instruction_settings_save_unavailable".into())
    );
    drop(tx);
    assert_eq!(
        code(roadmap_refresh(&db, &p1, Refresh::Off)),
        Some("roadmap_status_unavailable".into())
    );
    assert_eq!(
        code(harness_update(&db, UpdateSettings::default())),
        Some("harness_update_settings_unreadable".into())
    );
    assert_eq!(moved(&db), (0, 0, 0));
    assert!(tables(&db).iter().all(Vec::is_empty));
}

/// v2.5 L10 upgrade row: settings written before the log reach replay through one snapshot
/// run at one revision, through the open's proof, and a settings table no open ever made is
/// made by the step instead of skipping the run.
#[test]
fn the_first_run_puts_settings_written_before_the_log_on_it_and_makes_a_missing_table() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(STATE).unwrap();
    crate::cleanup_settings::initialize(&db).unwrap();
    apply(&db, &every()[1]).unwrap();
    let tx = db.transaction().unwrap();
    assert!(first_run(&tx, Opening::for_test(3)).unwrap());
    tx.commit().unwrap();
    let events = i64::try_from(TABLES.len()).unwrap() + 1;
    assert_eq!(
        moved(&db),
        (1, events, 1),
        "one revision: a chunk per table and the closing event"
    );
    let live = tables(&db);
    assert_eq!(
        live.iter().map(Vec::len).sum::<usize>(),
        1,
        "the run changes no row"
    );
    replays_to_live(&mut db);
}

/// v2.5 L10 upgrade row: a store with no settings row or no `state` table gets no run, so
/// the step moves no revision.
#[test]
fn the_first_run_writes_nothing_without_settings_rows_or_a_state_table() {
    let mut db = unhosted();
    let tx = db.transaction().unwrap();
    assert!(!first_run(&tx, Opening::for_test(3)).unwrap());
    tx.commit().unwrap();
    assert_eq!(moved(&db), (0, 0, 0));

    let mut stateless = Connection::open_in_memory().unwrap();
    initialize(&stateless).unwrap();
    apply(&stateless, &every()[1]).unwrap();
    let tx = stateless.transaction().unwrap();
    assert!(
        !first_run(&tx, Opening::for_test(3)).unwrap(),
        "no state table"
    );
}
