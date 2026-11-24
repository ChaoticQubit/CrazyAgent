use super::*;

/// A settings file holding one plugin setting, either one, reads back through a
/// reload: both plugin settings are listed, the unset one at its default, and the
/// config validates instead of stopping the reader.
#[test]
fn a_settings_file_with_one_plugin_setting_reads_back() {
    use kernel_plugin_host::usage::TurnOff;
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    let mut state = ConfigState {
        current: resolve_config(&global, &project).unwrap(),
    };
    std::fs::write(
        &global,
        format!("version = {SETTINGS_VERSION}\n[plugins]\nwhen_turned_off = \"stop_runs\"\n"),
    )
    .unwrap();
    state.reload(&global, &project).unwrap();
    let config = &state.current;
    assert_eq!(config.settings.plugins.when_turned_off, TurnOff::StopRuns);
    assert!(!config.settings.plugins.essential_unlocked);
    assert_eq!(
        config.descriptors["plugins.when_turned_off"].origin,
        Origin::Global
    );
    let unlocked = &config.descriptors["plugins.essential_unlocked"];
    assert_eq!(unlocked.value, serde_json::json!(false));
    assert_eq!(unlocked.origin, Origin::Default);
    config.validate().unwrap();
    std::fs::write(
        &global,
        format!("version = {SETTINGS_VERSION}\n[plugins]\nessential_unlocked = true\n"),
    )
    .unwrap();
    state.reload(&global, &project).unwrap();
    let config = &state.current;
    assert!(config.settings.plugins.essential_unlocked);
    assert_eq!(
        config.descriptors["plugins.when_turned_off"].origin,
        Origin::Default
    );
    assert_eq!(
        config.descriptors["plugins.essential_unlocked"].origin,
        Origin::Global
    );
    config.validate().unwrap();
}
