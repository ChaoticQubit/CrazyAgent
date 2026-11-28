use super::global_write::{AFTER_RENAME, SETTING_STAGED};
use super::*;
use crate::config::edit_when_turned_off;
use kernel_plugin_host::usage::TurnOff;
use std::cell::RefCell;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::rc::Rc;

const GLOBAL: &str = "# owner notes\ntimeout_ms = 1000 # keep\n";

struct Fixture {
    sources: SettingsSources,
    global: PathBuf,
    root: tempfile::TempDir,
}

/// A step's result with the step named in its error. Inputs: the result and the step.
/// Output: the value, or the named error.
fn named<T, E: std::fmt::Display>(
    result: Result<T, E>,
    step: impl std::fmt::Display,
) -> Result<T, String> {
    result.map_err(|error| format!("{step}: {error}"))
}

/// Makes a private 0700 folder. Input: the path. Output: none, or the step that failed.
fn private_directory(path: &Path) -> Result<(), String> {
    named(
        std::fs::create_dir(path),
        format!("make {}", path.display()),
    )?;
    named(
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)),
        format!("make {} private", path.display()),
    )
}

/// Writes a private 0600 file. Inputs: the path and the bytes. Output: none, or the step
/// that failed.
fn private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    named(
        std::fs::write(path, bytes),
        format!("write {}", path.display()),
    )?;
    named(
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)),
        format!("make {} private", path.display()),
    )
}

impl Fixture {
    /// A private root holding a global settings file with the owner's lines and an empty
    /// project file, both opened as the engine opens them. Input: none. Output: the fixture,
    /// or the step that failed.
    fn new() -> Result<Self, String> {
        let base = named(std::fs::canonicalize("/tmp"), "canonicalize /tmp")?;
        let root = named(
            tempfile::Builder::new()
                .prefix("ca-settings-when-off-")
                .tempdir_in(&base),
            format!("make a folder in {}", base.display()),
        )?;
        named(
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
            "make the fixture root private",
        )?;
        let global_parent = root.path().join("global");
        let project_parent = root.path().join("project");
        private_directory(&global_parent)?;
        private_directory(&project_parent)?;
        let global = global_parent.join("settings.toml");
        private_file(&global, GLOBAL.as_bytes())?;
        private_file(&project_parent.join("settings.toml"), b"")?;
        let sources = named(
            SettingsSources::open(&global, &project_parent.join("settings.toml")),
            "open the settings sources",
        )?;
        Ok(Self {
            sources,
            global,
            root,
        })
    }

    fn write(&self, when_off: TurnOff) -> Result<EffectiveConfig, EngineError> {
        self.sources
            .write_global(&move |global: &str, project: &str| {
                edit_when_turned_off(global, project, when_off)
            })
            .map_err(|refused| refused.refused)
    }

    fn staged_left(&self) -> bool {
        std::fs::symlink_metadata(self.global.with_file_name(SETTING_STAGED)).is_ok()
    }
}

/// The choice reaches the global file by a rename in its own folder: the new file is
/// private, the owner's other lines stay, nothing staged is left, and the config handed back
/// is the one the files now resolve to.
#[test]
fn the_turn_off_choice_reaches_the_file_by_rename_and_the_config_follows() -> Result<(), String> {
    let fixture = Fixture::new()?;
    let before = std::fs::metadata(&fixture.global).unwrap().ino();
    let next = fixture.write(TurnOff::StopRuns).unwrap();
    let bytes = std::fs::read_to_string(&fixture.global).unwrap();
    assert!(bytes.starts_with(GLOBAL), "{bytes}");
    assert!(bytes.contains("when_turned_off = \"stop_runs\""), "{bytes}");
    let after = std::fs::metadata(&fixture.global).unwrap();
    assert_ne!(after.ino(), before);
    assert_eq!(after.mode() & 0o777, 0o600);
    assert!(!fixture.staged_left());
    assert_eq!(next.settings.plugins.when_turned_off, TurnOff::StopRuns);
    assert_eq!(next.id, fixture.sources.config().unwrap().id);
    assert!(fixture.root.path().exists());
    Ok(())
}

/// A global file replaced by a link after the engine opened it is refused with the settings
/// code for a file that is not private, and neither the link nor its target changes.
#[test]
fn a_linked_settings_file_is_refused_and_left_alone() -> Result<(), String> {
    let fixture = Fixture::new()?;
    let target = fixture.root.path().join("elsewhere.toml");
    private_file(&target, GLOBAL.as_bytes())?;
    std::fs::remove_file(&fixture.global).unwrap();
    symlink(&target, &fixture.global).unwrap();
    let refused = fixture.write(TurnOff::StopRuns).unwrap_err();
    assert_eq!(refused.0, "settings_metadata_invalid");
    assert!(
        std::fs::symlink_metadata(&fixture.global)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), GLOBAL);
    assert!(!fixture.staged_left());
    Ok(())
}

/// A global file others may read, or one with a second name, is refused and its bytes stay.
#[test]
fn a_shared_or_hard_linked_settings_file_is_refused() -> Result<(), String> {
    let fixture = Fixture::new()?;
    std::fs::set_permissions(&fixture.global, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        fixture.write(TurnOff::StopRuns).unwrap_err().0,
        "settings_metadata_invalid"
    );
    assert_eq!(std::fs::read_to_string(&fixture.global).unwrap(), GLOBAL);
    std::fs::set_permissions(&fixture.global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let second = fixture.root.path().join("second-name.toml");
    std::fs::hard_link(&fixture.global, &second).unwrap();
    assert_eq!(
        fixture.write(TurnOff::StopRuns).unwrap_err().0,
        "settings_metadata_invalid"
    );
    assert_eq!(std::fs::read_to_string(&fixture.global).unwrap(), GLOBAL);
    assert!(!fixture.staged_left());
    Ok(())
}

/// A global file that stopped parsing after the engine opened it is refused and never
/// rewritten: same bytes, same file.
#[test]
fn an_unparsable_settings_file_is_refused_and_never_rewritten() -> Result<(), String> {
    let fixture = Fixture::new()?;
    let broken = "timeout_ms = \n[";
    private_file(&fixture.global, broken.as_bytes())?;
    let before = std::fs::metadata(&fixture.global).unwrap().ino();
    assert_eq!(
        fixture.write(TurnOff::StopRuns).unwrap_err().0,
        "invalid_configuration_format"
    );
    assert_eq!(std::fs::read_to_string(&fixture.global).unwrap(), broken);
    assert_eq!(std::fs::metadata(&fixture.global).unwrap().ino(), before);
    assert!(!fixture.staged_left());
    Ok(())
}

/// A staged file a crash left behind is replaced, not trusted, by the next write.
#[test]
fn a_staged_file_a_crash_left_is_replaced_by_the_next_write() -> Result<(), String> {
    let fixture = Fixture::new()?;
    private_file(
        &fixture.global.with_file_name(SETTING_STAGED),
        b"timeout_ms = 5\n",
    )?;
    fixture.write(TurnOff::StopRuns).unwrap();
    let bytes = std::fs::read_to_string(&fixture.global).unwrap();
    assert!(bytes.starts_with(GLOBAL), "{bytes}");
    assert!(!fixture.staged_left());
    Ok(())
}

/// Every refusal of the settings write reaches the owner as a listed App plugins code: an
/// unsafe file, an unreadable one, and anything else as unavailable.
#[test]
fn setting_refusals_map_to_listed_app_plugin_codes() {
    for (raw, mapped) in [
        ("settings_metadata_invalid", "app_plugin_setting_unsafe"),
        ("settings_source_replaced", "app_plugin_setting_unsafe"),
        ("settings_parent_replaced", "app_plugin_setting_unsafe"),
        (
            "invalid_configuration_format",
            "app_plugin_setting_unreadable",
        ),
        ("settings_file_bounds", "app_plugin_setting_unreadable"),
        ("settings_source_conflict", "app_plugin_setting_unavailable"),
        (
            "settings_publication_pending",
            "app_plugin_setting_unavailable",
        ),
    ] {
        assert_eq!(setting_refusal(error(raw)).0, mapped, "{raw}");
        assert!(
            crate::engine::owner::APP_PLUGINS_CODES.contains(&mapped),
            "{mapped}"
        );
    }
}

/// A turn-off choice storage cannot record leaves the global file as it was, so the files
/// still resolve to the config the worker holds.
#[test]
fn a_turn_off_choice_storage_cannot_record_leaves_the_file_as_it_was() -> Result<(), String> {
    let fixture = Fixture::new()?;
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE settings_publication (singleton INTEGER PRIMARY KEY, body BLOB)",
    )
    .unwrap();
    let mut config = named(
        fixture.sources.config(),
        "read the config the files resolve to",
    )?;
    let held = config.id.clone();
    let command = crate::app_plugins::Command::SetWhenOff {
        when_off: TurnOff::StopRuns,
    };
    let saved = app_plugins(&db, &mut config, (false, Some(&fixture.sources), command));
    assert!(saved.is_err(), "storage could not record the new config");
    assert_eq!(config.id, held);
    assert_eq!(std::fs::read_to_string(&fixture.global).unwrap(), GLOBAL);
    assert!(verified_source_hash(&config, Some(&fixture.sources)).is_ok());
    assert!(!fixture.staged_left());
    Ok(())
}

/// A turn-off choice storage cannot record is refused with a listed code before the global
/// file is touched, so no put-back write can fail and leave the files and the worker apart.
#[test]
fn a_turn_off_choice_storage_cannot_record_never_touches_the_file() -> Result<(), String> {
    let fixture = Fixture::new()?;
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE settings_publication (singleton INTEGER PRIMARY KEY, body BLOB)",
    )
    .unwrap();
    let mut config = named(
        fixture.sources.config(),
        "read the config the files resolve to",
    )?;
    let before = std::fs::metadata(&fixture.global).unwrap().ino();
    let command = crate::app_plugins::Command::SetWhenOff {
        when_off: TurnOff::StopRuns,
    };
    let refused = app_plugins(&db, &mut config, (false, Some(&fixture.sources), command))
        .unwrap_err()
        .0;
    assert!(
        crate::engine::owner::APP_PLUGINS_CODES.contains(&refused.as_str()),
        "{refused}"
    );
    assert_eq!(std::fs::metadata(&fixture.global).unwrap().ino(), before);
    assert_eq!(std::fs::read_to_string(&fixture.global).unwrap(), GLOBAL);
    assert!(verified_source_hash(&config, Some(&fixture.sources)).is_ok());
    Ok(())
}

/// Runs `step` once, right after the next settings rename on this thread.
fn after_rename(step: impl FnOnce() -> Result<(), EngineError> + 'static) {
    AFTER_RENAME.with(|hook| *hook.borrow_mut() = Some(Box::new(step)));
}

/// Runs `write` once, right after the next settings rename on this thread, and keeps its
/// file error for the row, since a hook error would read as a failed folder sync. Input:
/// the write. Output: the slot the row reads after the call.
fn after_rename_write(
    write: impl FnOnce() -> std::io::Result<()> + 'static,
) -> Rc<RefCell<Option<String>>> {
    let failed = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&failed);
    after_rename(move || {
        if let Err(error) = write() {
            *slot.borrow_mut() = Some(format!("the write after the rename failed: {error}"));
        }
        Ok(())
    });
    failed
}

/// A database with the two tables a turn-off choice reads and writes. Output: the
/// database, or the step that failed.
fn recording_db() -> Result<Connection, String> {
    let db = named(Connection::open_in_memory(), "open the database")?;
    named(
        db.execute_batch(
            "CREATE TABLE settings_publication (singleton INTEGER PRIMARY KEY, body BLOB);
             CREATE TABLE configs (id TEXT PRIMARY KEY, body TEXT NOT NULL);",
        ),
        "make the two tables",
    )?;
    Ok(db)
}

/// Whether `id` has a stored config row. Inputs: the database and the id. Output: the
/// answer, or the read that failed.
fn stored(db: &Connection, id: &str) -> Result<bool, String> {
    let rows = named(
        db.query_row("SELECT COUNT(*) FROM configs WHERE id = ?1", [id], |row| {
            row.get::<_, i64>(0)
        }),
        format!("count the stored configs {id}"),
    )?;
    Ok(rows == 1)
}

/// Saves the turn-off choice through the worker's App plugins command.
fn set_when_off(
    fixture: &Fixture,
    db: &Connection,
    config: &mut EffectiveConfig,
) -> Result<crate::app_plugins::Done, EngineError> {
    let command = crate::app_plugins::Command::SetWhenOff {
        when_off: TurnOff::StopRuns,
    };
    app_plugins(db, config, (false, Some(&fixture.sources), command))
}

/// A folder sync that fails after the rename is refused as unavailable, and the worker
/// takes the config the renamed file resolves to, stored, so the two never part.
#[test]
fn a_turn_off_choice_whose_folder_sync_fails_after_the_rename_leaves_the_worker_on_the_file()
-> Result<(), String> {
    let fixture = Fixture::new()?;
    let db = recording_db()?;
    let mut config = named(
        fixture.sources.config(),
        "read the config the files resolve to",
    )?;
    let next = named(
        edit_when_turned_off(GLOBAL, "", TurnOff::StopRuns),
        "edit the global text",
    )?
    .1;
    after_rename(|| Err(error("settings_sync_failed")));
    let refused = set_when_off(&fixture, &db, &mut config).unwrap_err();
    assert_eq!(refused.0, "app_plugin_setting_unavailable");
    let on_file = fixture.sources.config().unwrap();
    assert_eq!(on_file.id, next.id);
    assert_eq!(config.id, on_file.id);
    assert!(stored(&db, config.id.as_str())?);
    Ok(())
}

/// Another valid text that lands after the rename is followed: the worker takes the config
/// that text resolves to, not the one it staged, and that config is stored.
#[test]
fn a_turn_off_choice_the_file_changes_under_after_the_rename_follows_the_file() -> Result<(), String>
{
    let fixture = Fixture::new()?;
    let db = recording_db()?;
    let mut config = named(
        fixture.sources.config(),
        "read the config the files resolve to",
    )?;
    let next = named(
        edit_when_turned_off(GLOBAL, "", TurnOff::StopRuns),
        "edit the global text",
    )?
    .1;
    let global = fixture.global.clone();
    let failed = after_rename_write(move || std::fs::write(&global, "timeout_ms = 2000\n"));
    let refused = set_when_off(&fixture, &db, &mut config).unwrap_err();
    if let Some(error) = failed.take() {
        return Err(error);
    }
    assert_eq!(refused.0, "app_plugin_setting_unavailable");
    let on_file = fixture.sources.config().unwrap();
    assert_ne!(on_file.id, next.id);
    assert_eq!(config.id, on_file.id);
    assert!(stored(&db, on_file.id.as_str())?);
    Ok(())
}

/// A text that stops resolving after the rename leaves the worker on its config, and the
/// next turn-off choice is refused with the file left exactly as it is.
#[test]
fn a_turn_off_choice_whose_file_stops_resolving_after_the_rename_keeps_the_worker_and_refuses_the_next_write()
-> Result<(), String> {
    let fixture = Fixture::new()?;
    let db = recording_db()?;
    let mut config = named(
        fixture.sources.config(),
        "read the config the files resolve to",
    )?;
    let held = config.id.clone();
    let broken = "timeout_ms = \n[";
    let global = fixture.global.clone();
    let failed = after_rename_write(move || std::fs::write(&global, broken));
    let refused = set_when_off(&fixture, &db, &mut config).unwrap_err();
    if let Some(error) = failed.take() {
        return Err(error);
    }
    assert_eq!(refused.0, "app_plugin_setting_unavailable");
    assert_eq!(config.id, held);
    let before = std::fs::metadata(&fixture.global).unwrap().ino();
    let again = set_when_off(&fixture, &db, &mut config).unwrap_err();
    assert_eq!(again.0, "app_plugin_setting_unreadable");
    assert_eq!(config.id, held);
    assert_eq!(std::fs::read_to_string(&fixture.global).unwrap(), broken);
    assert_eq!(std::fs::metadata(&fixture.global).unwrap().ino(), before);
    assert!(!fixture.staged_left());
    Ok(())
}
