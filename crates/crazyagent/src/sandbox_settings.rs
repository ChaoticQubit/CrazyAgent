use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::Outcome;
use crate::sandbox::{LinuxSockets, LinuxStrict, LinuxSupervisor};
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[cfg(any(unix, test))]
const TABLE: &str = "CREATE TABLE IF NOT EXISTS sandbox_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    linux_sockets TEXT NOT NULL CHECK(linux_sockets IN ('tcp_only', 'refuse', 'tcp_ports')),
    linux_supervisor TEXT NOT NULL DEFAULT 'on' CHECK(linux_supervisor IN ('on', 'off')),
    linux_strict TEXT NOT NULL DEFAULT 'off' CHECK(linux_strict IN ('off', 'on'))
)";
#[cfg(any(unix, test))]
const SUPERVISOR_COLUMN: &str = "ALTER TABLE sandbox_settings ADD COLUMN linux_supervisor TEXT NOT NULL DEFAULT 'on' CHECK(linux_supervisor IN ('on', 'off'))";
#[cfg(any(unix, test))]
const STRICT_COLUMN: &str = "ALTER TABLE sandbox_settings ADD COLUMN linux_strict TEXT NOT NULL DEFAULT 'off' CHECK(linux_strict IN ('off', 'on'))";

/// The owner's app-wide sandbox settings: how a limited-network agent's sockets are held
/// on Linux ("Linux limited-network sockets"), whether Linux agents run under the call
/// supervisor ("Linux call supervisor") and whether a Linux launch without the full sandbox
/// is refused (DEC-185). Before the first save every value is its default; a client that
/// sends no supervisor or strict choice gets the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSettings {
    pub linux_sockets: LinuxSockets,
    #[serde(default)]
    pub linux_supervisor: LinuxSupervisor,
    #[serde(default)]
    pub linux_strict: LinuxStrict,
}

/// The owner's sandbox settings with the version of the read that showed them. A save
/// names that version, so a save from a window that read older settings is refused
/// `stale_revision` instead of turning a newer choice back (rv-sec2 F1). Fields: the
/// settings and the version, 0 before the first save.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSandboxSettings {
    pub settings: SandboxSettings,
    pub version: u64,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// The stored word of one setting value, as its serde name. Input: the value. Output: the
/// word, or `sandbox_settings_invalid` for a value that is not one word.
fn word(value: impl Serialize) -> Result<String, EngineError> {
    match serde_json::to_value(value).map_err(error)? {
        serde_json::Value::String(word) => Ok(word),
        _ => Err(error("sandbox_settings_invalid")),
    }
}

/// The setting value a stored word names. Input: the word. Output: the value, or
/// `sandbox_settings_invalid` for a word this engine does not know.
fn value<T: DeserializeOwned>(word: String) -> Result<T, EngineError> {
    serde_json::from_value(serde_json::Value::String(word))
        .map_err(|_| error("sandbox_settings_invalid"))
}

/// Creates the settings table when it is missing and adds the call supervisor and strict
/// columns to a table made before them, holding `on` and `off` for a row already saved, so
/// the owner's other choices stay. Input: the engine database. Output: Ok, or the database
/// error.
#[cfg(any(unix, test))]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)?;
    for (name, column) in [
        ("linux_supervisor", SUPERVISOR_COLUMN),
        ("linux_strict", STRICT_COLUMN),
    ] {
        let columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('sandbox_settings') WHERE name=?1",
                [name],
                |row| row.get(0),
            )
            .map_err(error)?;
        assert!((0..=1).contains(&columns));
        if columns == 0 {
            connection.execute_batch(column).map_err(error)?;
        }
    }
    Ok(())
}

/// Saves the owner's sandbox settings over the stored ones. Inputs: the open transaction
/// and the settings. Output: `Applied`, or the database error.
pub(crate) fn save(
    connection: &Connection,
    settings: &SandboxSettings,
) -> Result<Outcome, EngineError> {
    let words = [
        word(settings.linux_sockets)?,
        word(settings.linux_supervisor)?,
        word(settings.linux_strict)?,
    ];
    let changed = connection
        .execute(
            "INSERT INTO sandbox_settings(singleton,linux_sockets,linux_supervisor,linux_strict) VALUES(1,?1,?2,?3) ON CONFLICT(singleton) DO UPDATE SET linux_sockets=excluded.linux_sockets, linux_supervisor=excluded.linux_supervisor, linux_strict=excluded.linux_strict",
            words,
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(Outcome::Applied)
}

/// Saves the owner's sandbox settings from a read of `version`, so a save from a screen
/// that read older settings never replaces newer ones, as team settings do (V2SYNC-2).
/// Inputs: the owner transaction, the settings and the version the screen showed. Output:
/// `Applied`, the `stale_revision` rejection with the stored row unchanged, or the error.
pub(crate) fn save_from(
    tx: &Transaction<'_>,
    settings: &SandboxSettings,
    version: u64,
) -> Result<Outcome, EngineError> {
    let _ = version;
    save(tx, settings)
}

/// Loads the owner's sandbox settings. Input: the engine database, writable or read-only.
/// Output: the stored settings, the defaults before the first save, or
/// `sandbox_settings_invalid` for a stored value this engine does not know, so a launch
/// that needs the setting fails closed.
pub fn load(connection: &Connection) -> Result<SandboxSettings, EngineError> {
    let words: Option<(String, String, String)> = connection
        .query_row(
            "SELECT linux_sockets, linux_supervisor, linux_strict FROM sandbox_settings WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((sockets, supervisor, strict)) = words else {
        return Ok(SandboxSettings::default());
    };
    Ok(SandboxSettings {
        linux_sockets: value(sockets)?,
        linux_supervisor: value(supervisor)?,
        linux_strict: value(strict)?,
    })
}

/// Reads the owner's sandbox settings from the engine database in the engine root without
/// writing to it, for a launch outside the engine's own transaction: a master turn and the
/// two checks. Input: the engine root. Output: the settings, `sandbox_settings_unavailable`
/// when the database cannot be opened, or [`load`]'s refusal, so the launch fails closed.
#[cfg(any(target_os = "linux", all(test, unix)))]
pub(crate) fn stored(engine: &std::path::Path) -> Result<SandboxSettings, EngineError> {
    let unavailable = |_: rusqlite::Error| error("sandbox_settings_unavailable");
    let connection = Connection::open_with_flags(
        engine.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(unavailable)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(unavailable)?;
    load(&connection)
}

/// The owner read of the sandbox settings. Input: the open transaction. Output: the
/// settings and the version a save from this read names, as owner data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::SandboxSettings {
        settings: load(connection)?,
        version: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(connection: &Connection) -> Option<(SandboxSettings, u64)> {
        match read(connection) {
            Ok(OwnerData::SandboxSettings { settings, version }) => Some((settings, version)),
            _ => None,
        }
    }

    #[test]
    fn a_save_from_a_stale_read_is_refused_and_the_stored_strict_choice_stays() {
        let folder = tempfile::tempdir().unwrap();
        let mut connection = Connection::open(folder.path().join("engine.db")).unwrap();
        initialize(&connection).unwrap();
        assert_eq!(shown(&connection), Some((SandboxSettings::default(), 0)));
        let on = SandboxSettings {
            linux_strict: LinuxStrict::On,
            ..SandboxSettings::default()
        };
        let tx = connection.transaction().unwrap();
        assert_eq!(save_from(&tx, &on, 0).unwrap(), Outcome::Applied);
        tx.commit().unwrap();
        assert_eq!(shown(&connection), Some((on, 1)));
        let tx = connection.transaction().unwrap();
        let stale = save_from(&tx, &SandboxSettings::default(), 0).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            stale,
            Outcome::Rejected {
                code: "stale_revision".into()
            }
        );
        assert_eq!(shown(&connection), Some((on, 1)));
        assert_eq!(load(&connection).unwrap().linux_strict, LinuxStrict::On);
    }

    #[test]
    fn each_option_is_saved_and_read_back_and_a_fresh_engine_has_the_default() {
        let folder = tempfile::tempdir().unwrap();
        let connection = Connection::open(folder.path().join("engine.db")).unwrap();
        initialize(&connection).unwrap();
        assert_eq!(load(&connection).unwrap(), SandboxSettings::default());
        assert_eq!(
            SandboxSettings::default().linux_sockets,
            LinuxSockets::TcpOnly
        );
        for linux_sockets in [
            LinuxSockets::Refuse,
            LinuxSockets::TcpPorts,
            LinuxSockets::TcpOnly,
        ] {
            let settings = SandboxSettings {
                linux_sockets,
                ..SandboxSettings::default()
            };
            assert_eq!(save(&connection, &settings).unwrap(), Outcome::Applied);
            assert_eq!(load(&connection).unwrap(), settings);
            let reader = Connection::open_with_flags(
                folder.path().join("engine.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            assert_eq!(load(&reader).unwrap(), settings);
        }
        let rows: i64 = connection
            .query_row("SELECT count(*) FROM sandbox_settings", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn a_stored_value_this_engine_does_not_know_fails_closed() {
        let folder = tempfile::tempdir().unwrap();
        let connection = Connection::open(folder.path().join("engine.db")).unwrap();
        initialize(&connection).unwrap();
        assert!(
            connection
                .execute(
                    "INSERT INTO sandbox_settings(singleton,linux_sockets) VALUES(1,'udp')",
                    [],
                )
                .is_err(),
            "the table refuses a value outside the three options"
        );
        for row in [
            "'open','on','off'",
            "'tcp_only','maybe','off'",
            "'tcp_only','on','maybe'",
        ] {
            connection
                .execute_batch(&format!(
                    "DROP TABLE sandbox_settings; CREATE TABLE sandbox_settings (singleton INTEGER PRIMARY KEY, linux_sockets TEXT NOT NULL, linux_supervisor TEXT NOT NULL, linux_strict TEXT NOT NULL); INSERT INTO sandbox_settings VALUES(1,{row});"
                ))
                .unwrap();
            let read = load(&connection).map_err(|error| error.0);
            assert_eq!(read, Err("sandbox_settings_invalid".into()), "{row}");
        }
    }

    #[test]
    fn each_call_supervisor_option_is_saved_and_read_back_beside_the_sockets_choice() {
        let folder = tempfile::tempdir().unwrap();
        let connection = Connection::open(folder.path().join("engine.db")).unwrap();
        initialize(&connection).unwrap();
        assert_eq!(
            load(&connection).unwrap().linux_supervisor,
            LinuxSupervisor::On
        );
        for linux_supervisor in [LinuxSupervisor::Off, LinuxSupervisor::On] {
            let settings = SandboxSettings {
                linux_sockets: LinuxSockets::TcpPorts,
                linux_supervisor,
                linux_strict: LinuxStrict::Off,
            };
            assert_eq!(save(&connection, &settings).unwrap(), Outcome::Applied);
            assert_eq!(load(&connection).unwrap(), settings);
        }
        assert!(
            connection
                .execute(
                    "UPDATE sandbox_settings SET linux_supervisor='maybe' WHERE singleton=1",
                    [],
                )
                .is_err(),
            "the table refuses a value outside the two options"
        );
    }

    #[test]
    fn an_engine_database_from_before_the_supervisor_setting_reads_on_and_keeps_its_sockets() {
        let folder = tempfile::tempdir().unwrap();
        let connection = Connection::open(folder.path().join("engine.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sandbox_settings (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), linux_sockets TEXT NOT NULL CHECK(linux_sockets IN ('tcp_only', 'refuse', 'tcp_ports'))); INSERT INTO sandbox_settings VALUES(1,'tcp_ports');",
            )
            .unwrap();
        initialize(&connection).unwrap();
        initialize(&connection).unwrap();
        let columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('sandbox_settings') WHERE name='linux_supervisor'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 1);
        let kept = SandboxSettings {
            linux_sockets: LinuxSockets::TcpPorts,
            linux_supervisor: LinuxSupervisor::On,
            linux_strict: LinuxStrict::Off,
        };
        assert_eq!(load(&connection).unwrap(), kept);
        let off = SandboxSettings {
            linux_supervisor: LinuxSupervisor::Off,
            ..kept
        };
        save(&connection, &off).unwrap();
        assert_eq!(load(&connection).unwrap(), off);
    }

    #[cfg(unix)]
    #[test]
    fn a_launch_reads_the_stored_choices_from_the_engine_root_and_fails_closed_without_them() {
        let folder = tempfile::tempdir().unwrap();
        let engine = folder.path().canonicalize().unwrap();
        assert_eq!(
            stored(&engine).unwrap_err().0,
            "sandbox_settings_unavailable"
        );
        let connection = Connection::open(engine.join("engine.db")).unwrap();
        initialize(&connection).unwrap();
        let off = SandboxSettings {
            linux_sockets: LinuxSockets::TcpPorts,
            linux_supervisor: LinuxSupervisor::Off,
            linux_strict: LinuxStrict::Off,
        };
        save(&connection, &off).unwrap();
        assert_eq!(stored(&engine).unwrap(), off);
        drop(connection);
        let held = std::fs::read(engine.join("engine.db")).unwrap();
        assert_eq!(stored(&engine).unwrap(), off);
        assert_eq!(
            std::fs::read(engine.join("engine.db")).unwrap(),
            held,
            "read only"
        );
    }

    #[test]
    fn a_client_that_sends_no_supervisor_choice_gets_on_and_each_choice_has_one_word() {
        let old: SandboxSettings = serde_json::from_str(r#"{"linux_sockets":"refuse"}"#).unwrap();
        assert_eq!(old.linux_sockets, LinuxSockets::Refuse);
        assert_eq!(old.linux_supervisor, LinuxSupervisor::On);
        let off = SandboxSettings {
            linux_supervisor: LinuxSupervisor::Off,
            ..SandboxSettings::default()
        };
        assert_eq!(
            serde_json::to_string(&off).unwrap(),
            r#"{"linux_sockets":"tcp_only","linux_supervisor":"off","linux_strict":"off"}"#
        );
    }

    #[test]
    fn each_strict_option_is_saved_and_read_back_and_a_fresh_engine_has_off() {
        let folder = tempfile::tempdir().unwrap();
        let connection = Connection::open(folder.path().join("engine.db")).unwrap();
        initialize(&connection).unwrap();
        assert_eq!(load(&connection).unwrap().linux_strict, LinuxStrict::Off);
        for linux_strict in [LinuxStrict::On, LinuxStrict::Off, LinuxStrict::On] {
            let settings = SandboxSettings {
                linux_sockets: LinuxSockets::TcpPorts,
                linux_supervisor: LinuxSupervisor::Off,
                linux_strict,
            };
            assert_eq!(save(&connection, &settings).unwrap(), Outcome::Applied);
            assert_eq!(load(&connection).unwrap(), settings);
        }
        assert!(
            connection
                .execute(
                    "UPDATE sandbox_settings SET linux_strict='maybe' WHERE singleton=1",
                    [],
                )
                .is_err(),
            "the table refuses a value outside the two options"
        );
    }

    #[test]
    fn an_engine_database_from_before_the_strict_setting_reads_off_and_keeps_the_others() {
        let folder = tempfile::tempdir().unwrap();
        let connection = Connection::open(folder.path().join("engine.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sandbox_settings (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), linux_sockets TEXT NOT NULL CHECK(linux_sockets IN ('tcp_only', 'refuse', 'tcp_ports')), linux_supervisor TEXT NOT NULL DEFAULT 'on' CHECK(linux_supervisor IN ('on', 'off'))); INSERT INTO sandbox_settings VALUES(1,'refuse','off');",
            )
            .unwrap();
        initialize(&connection).unwrap();
        initialize(&connection).unwrap();
        let columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('sandbox_settings') WHERE name='linux_strict'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 1, "the strict column is added once");
        let kept = SandboxSettings {
            linux_sockets: LinuxSockets::Refuse,
            linux_supervisor: LinuxSupervisor::Off,
            linux_strict: LinuxStrict::Off,
        };
        assert_eq!(load(&connection).unwrap(), kept);
    }

    #[test]
    fn a_client_that_sends_no_strict_choice_gets_off_and_each_choice_has_one_word() {
        let old: SandboxSettings =
            serde_json::from_str(r#"{"linux_sockets":"tcp_only","linux_supervisor":"on"}"#)
                .unwrap();
        assert_eq!(old.linux_strict, LinuxStrict::Off);
        for (strict, word) in [(LinuxStrict::Off, "\"off\""), (LinuxStrict::On, "\"on\"")] {
            assert_eq!(serde_json::to_string(&strict).unwrap(), word);
            assert_eq!(serde_json::from_str::<LinuxStrict>(word).unwrap(), strict);
        }
    }
}
