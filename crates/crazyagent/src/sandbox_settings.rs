use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::Outcome;
use crate::sandbox::{LinuxSockets, LinuxSupervisor};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[cfg(any(unix, test))]
const TABLE: &str = "CREATE TABLE IF NOT EXISTS sandbox_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    linux_sockets TEXT NOT NULL CHECK(linux_sockets IN ('tcp_only', 'refuse', 'tcp_ports'))
)";

/// The owner's app-wide sandbox settings: how a limited-network agent's sockets are held
/// on Linux ("Linux limited-network sockets") and whether Linux agents run under the call
/// supervisor ("Linux call supervisor"). Before the first save every value is its default;
/// a client that sends no supervisor choice gets the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSettings {
    pub linux_sockets: LinuxSockets,
    #[serde(default)]
    pub linux_supervisor: LinuxSupervisor,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// Creates the settings table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
#[cfg(any(unix, test))]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// Saves the owner's sandbox settings over the stored ones. Inputs: the open transaction
/// and the settings. Output: `Applied`, or the database error.
pub(crate) fn save(
    connection: &Connection,
    settings: &SandboxSettings,
) -> Result<Outcome, EngineError> {
    let serde_json::Value::String(sockets) =
        serde_json::to_value(settings.linux_sockets).map_err(error)?
    else {
        return Err(error("sandbox_settings_invalid"));
    };
    connection
        .execute(
            "INSERT INTO sandbox_settings(singleton,linux_sockets) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET linux_sockets=excluded.linux_sockets",
            [&sockets],
        )
        .map_err(error)?;
    Ok(Outcome::Applied)
}

/// Loads the owner's sandbox settings. Input: the engine database, writable or read-only.
/// Output: the stored settings, the defaults before the first save, or
/// `sandbox_settings_invalid` for a stored value this engine does not know, so a launch
/// that needs the setting fails closed.
pub fn load(connection: &Connection) -> Result<SandboxSettings, EngineError> {
    let sockets: Option<String> = connection
        .query_row(
            "SELECT linux_sockets FROM sandbox_settings WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(sockets) = sockets else {
        return Ok(SandboxSettings::default());
    };
    let linux_sockets = serde_json::from_value(serde_json::Value::String(sockets))
        .map_err(|_| error("sandbox_settings_invalid"))?;
    Ok(SandboxSettings {
        linux_sockets,
        linux_supervisor: LinuxSupervisor::default(),
    })
}

/// The owner read of the sandbox settings. Input: the open transaction. Output: the
/// settings as owner data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::SandboxSettings {
        settings: load(connection)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for row in ["'open','on'", "'tcp_only','maybe'"] {
            connection
                .execute_batch(&format!(
                    "DROP TABLE sandbox_settings; CREATE TABLE sandbox_settings (singleton INTEGER PRIMARY KEY, linux_sockets TEXT NOT NULL, linux_supervisor TEXT NOT NULL); INSERT INTO sandbox_settings VALUES(1,{row});"
                ))
                .unwrap();
            assert_eq!(load(&connection).unwrap_err().0, "sandbox_settings_invalid");
        }
    }

    #[test]
    fn each_call_supervisor_option_is_saved_and_read_back_beside_the_sockets_choice() {
        let folder = tempfile::tempdir().unwrap();
        let connection = Connection::open(folder.path().join("engine.db")).unwrap();
        initialize(&connection).unwrap();
        assert_eq!(load(&connection).unwrap().linux_supervisor, LinuxSupervisor::On);
        for linux_supervisor in [LinuxSupervisor::Off, LinuxSupervisor::On] {
            let settings = SandboxSettings {
                linux_sockets: LinuxSockets::TcpPorts,
                linux_supervisor,
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
        };
        assert_eq!(load(&connection).unwrap(), kept);
        let off = SandboxSettings {
            linux_supervisor: LinuxSupervisor::Off,
            ..kept
        };
        save(&connection, &off).unwrap();
        assert_eq!(load(&connection).unwrap(), off);
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
            r#"{"linux_sockets":"tcp_only","linux_supervisor":"off"}"#
        );
    }
}
