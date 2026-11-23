use super::*;
use kernel_plugin_host::usage::TurnOff;

/// Setting the turn-off choice changes only that value: every other byte, comment, key and
/// the comment on the value's own line stay as the owner wrote them.
#[test]
fn the_turn_off_choice_is_written_and_every_other_byte_stays() {
    let global = "# my settings\ntimeout_ms = 1000 # keep this\n\n[plugins]\n# how plugins stop\nwhen_turned_off = \"refuse_new_calls\" # owner note\nessential_unlocked = false\n";
    let (bytes, config) = edit_when_turned_off(global, "", TurnOff::StopRuns).unwrap();
    assert_eq!(
        bytes,
        "# my settings\ntimeout_ms = 1000 # keep this\n\n[plugins]\n# how plugins stop\nwhen_turned_off = \"stop_runs\" # owner note\nessential_unlocked = false\n"
    );
    assert_eq!(config.settings.plugins.when_turned_off, TurnOff::StopRuns);
    assert_eq!(config.id, resolve_config_bytes(&bytes, "").unwrap().id);
}

/// A file with no plugins table gains one as its own table at the end, and what was there
/// stays byte for byte.
#[test]
fn a_file_without_a_plugins_table_gains_one_and_keeps_the_rest() {
    let global = "timeout_ms = 1000\n# the end\n";
    let (bytes, config) = edit_when_turned_off(global, "", TurnOff::StopRuns).unwrap();
    assert!(bytes.starts_with(global), "{bytes}");
    assert!(
        bytes.contains("[plugins]\nwhen_turned_off = \"stop_runs\"\n"),
        "{bytes}"
    );
    assert!(!bytes.contains("plugins = {"), "{bytes}");
    assert_eq!(config.settings.plugins.when_turned_off, TurnOff::StopRuns);
    let (empty, _) = edit_when_turned_off("", "", TurnOff::RefuseNewCalls).unwrap();
    assert_eq!(empty, "[plugins]\nwhen_turned_off = \"refuse_new_calls\"\n");
}

/// The choice is written to the global file, but a project file that sets it still wins,
/// and the resolved setting says it comes from the project.
#[test]
fn a_project_file_that_sets_the_choice_still_wins() {
    let project = "[plugins]\nwhen_turned_off = \"refuse_new_calls\"\n";
    let (bytes, config) = edit_when_turned_off("", project, TurnOff::StopRuns).unwrap();
    assert!(bytes.contains("when_turned_off = \"stop_runs\""), "{bytes}");
    assert_eq!(config.settings.plugins.when_turned_off, TurnOff::RefuseNewCalls);
    assert_eq!(
        config.descriptors["plugins.when_turned_off"].origin,
        Origin::Project
    );
    let (_, global_only) = edit_when_turned_off("", "", TurnOff::StopRuns).unwrap();
    assert_ne!(
        global_only.descriptors["plugins.when_turned_off"].origin,
        Origin::Project
    );
}

/// A global file that does not parse, or parses to settings the engine refuses, is refused
/// and no new text is made from it.
#[test]
fn an_unparsable_or_refused_file_is_refused() {
    for global in [
        "timeout_ms = \n[",
        "[plugins]\nunknown_key = 1\n",
        "plugins = 3\n",
        "[plugins]\nwhen_turned_off = [1]\n",
    ] {
        let refused = edit_when_turned_off(global, "", TurnOff::StopRuns).unwrap_err();
        assert!(!refused.0.is_empty(), "{global}");
    }
    let refused = edit_when_turned_off("timeout_ms = \n[", "", TurnOff::StopRuns).unwrap_err();
    assert_eq!(refused.0, "invalid_configuration_format");
}
