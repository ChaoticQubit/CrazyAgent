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

/// A settings file with a knowledge setting and a plugin setting opens the store, and the
/// store's owner reads list every setting and return one of each kind, instead of
/// stopping the store thread on its listed-settings bound.
#[tokio::test]
async fn the_store_reads_back_knowledge_and_plugin_settings() {
    use crate::engine::owner::{OwnerData, OwnerRead};
    use crate::storage::{Job, Storage};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::Builder::new()
        .prefix("ca-plugin-settings-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    for (path, text) in [
        (
            &global,
            format!(
                "version = {SETTINGS_VERSION}\n[knowledge]\nrecords_max = 256\n[plugins]\nessential_unlocked = true\n"
            ),
        ),
        (&project, format!("version = {SETTINGS_VERSION}\n")),
    ] {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let (storage, config) =
        Storage::open_with_settings_sources(&root.path().join("engine.db"), &global, &project)
            .await
            .unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStart(send))
        .unwrap_or_else(|_| panic!("storage queue unavailable"));
    let admission = receive.await.unwrap().unwrap().owners[0].admission.clone();
    let read = async |query: OwnerRead| {
        let (send, receive) = tokio::sync::oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerRead(admission.clone(), query, send))
            .unwrap_or_else(|_| panic!("storage queue unavailable"));
        receive.await.unwrap().unwrap().data
    };
    let OwnerData::Settings { names } = read(OwnerRead::Settings).await else {
        panic!("settings names required")
    };
    assert_eq!(
        names,
        config.descriptors.keys().cloned().collect::<Vec<_>>()
    );
    assert!(names.iter().any(|name| name == "knowledge.records_max"));
    assert!(names.iter().any(|name| name == "plugins.when_turned_off"));
    for (name, value) in [
        ("knowledge.records_max", serde_json::json!(256)),
        ("plugins.essential_unlocked", serde_json::json!(true)),
    ] {
        let OwnerData::Setting { descriptor } =
            read(OwnerRead::Setting { name: name.into() }).await
        else {
            panic!("one setting required")
        };
        assert_eq!(descriptor.value, value);
        assert_eq!(descriptor.origin, Origin::Global);
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    storage
        .sender
        .try_send(Job::Close(send))
        .unwrap_or_else(|_| panic!("storage queue unavailable"));
    receive.await.unwrap().unwrap();
}
