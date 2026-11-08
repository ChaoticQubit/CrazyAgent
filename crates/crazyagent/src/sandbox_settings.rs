use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::Outcome;
use crate::sandbox::LinuxSockets;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
const TABLE: &str = "CREATE TABLE IF NOT EXISTS sandbox_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    linux_sockets TEXT NOT NULL CHECK(linux_sockets IN ('tcp_only', 'refuse', 'tcp_ports'))
)";

/// The owner's app-wide sandbox settings: how a limited-network agent's sockets are held
/// on Linux ("Linux limited-network sockets"). Before the first save every value is its
/// default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSettings {
    pub linux_sockets: LinuxSockets,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// Creates the settings table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
#[cfg(unix)]
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
    Ok(SandboxSettings { linux_sockets })
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
            let settings = SandboxSettings { linux_sockets };
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
        connection
            .execute_batch(
                "DROP TABLE sandbox_settings; CREATE TABLE sandbox_settings (singleton INTEGER PRIMARY KEY, linux_sockets TEXT NOT NULL); INSERT INTO sandbox_settings VALUES(1,'open');",
            )
            .unwrap();
        assert_eq!(load(&connection).unwrap_err().0, "sandbox_settings_invalid");
    }
}
