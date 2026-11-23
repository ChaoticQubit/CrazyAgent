use super::*;
use crate::config::edit_when_turned_off;
use kernel_plugin_host::usage::TurnOff;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

const GLOBAL: &str = "# owner notes\ntimeout_ms = 1000 # keep\n";

struct Fixture {
    sources: SettingsSources,
    global: PathBuf,
    root: tempfile::TempDir,
}

fn private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-settings-when-off-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global_parent = root.path().join("global");
        let project_parent = root.path().join("project");
        private_directory(&global_parent);
        private_directory(&project_parent);
        let global = global_parent.join("settings.toml");
        private_file(&global, GLOBAL.as_bytes());
        private_file(&project_parent.join("settings.toml"), b"");
        let sources =
            SettingsSources::open(&global, &project_parent.join("settings.toml")).unwrap();
        Self {
            sources,
            global,
            root,
        }
    }

    fn write(&self, when_off: TurnOff) -> Result<EffectiveConfig, EngineError> {
        self.sources
            .write_global(&move |global: &str, project: &str| {
                edit_when_turned_off(global, project, when_off)
            })
    }

    fn staged_left(&self) -> bool {
        std::fs::symlink_metadata(self.global.with_file_name(SETTING_STAGED)).is_ok()
    }
}

/// The choice reaches the global file by a rename in its own folder: the new file is
/// private, the owner's other lines stay, nothing staged is left, and the config handed back
/// is the one the files now resolve to.
#[test]
fn the_turn_off_choice_reaches_the_file_by_rename_and_the_config_follows() {
    let fixture = Fixture::new();
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
}

/// A global file replaced by a link after the engine opened it is refused with the settings
/// code for a file that is not private, and neither the link nor its target changes.
#[test]
fn a_linked_settings_file_is_refused_and_left_alone() {
    let fixture = Fixture::new();
    let target = fixture.root.path().join("elsewhere.toml");
    private_file(&target, GLOBAL.as_bytes());
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
}

/// A global file others may read, or one with a second name, is refused and its bytes stay.
#[test]
fn a_shared_or_hard_linked_settings_file_is_refused() {
    let fixture = Fixture::new();
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
}

/// A global file that stopped parsing after the engine opened it is refused and never
/// rewritten: same bytes, same file.
#[test]
fn an_unparsable_settings_file_is_refused_and_never_rewritten() {
    let fixture = Fixture::new();
    let broken = "timeout_ms = \n[";
    private_file(&fixture.global, broken.as_bytes());
    let before = std::fs::metadata(&fixture.global).unwrap().ino();
    assert_eq!(
        fixture.write(TurnOff::StopRuns).unwrap_err().0,
        "invalid_configuration_format"
    );
    assert_eq!(std::fs::read_to_string(&fixture.global).unwrap(), broken);
    assert_eq!(std::fs::metadata(&fixture.global).unwrap().ino(), before);
    assert!(!fixture.staged_left());
}

/// A staged file a crash left behind is replaced, not trusted, by the next write.
#[test]
fn a_staged_file_a_crash_left_is_replaced_by_the_next_write() {
    let fixture = Fixture::new();
    private_file(
        &fixture.global.with_file_name(SETTING_STAGED),
        b"timeout_ms = 5\n",
    );
    fixture.write(TurnOff::StopRuns).unwrap();
    let bytes = std::fs::read_to_string(&fixture.global).unwrap();
    assert!(bytes.starts_with(GLOBAL), "{bytes}");
    assert!(!fixture.staged_left());
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
fn a_turn_off_choice_storage_cannot_record_leaves_the_file_as_it_was() {
    let fixture = Fixture::new();
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE settings_publication (singleton INTEGER PRIMARY KEY, body BLOB)",
    )
    .unwrap();
    let mut config = fixture.sources.config().unwrap();
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
}
