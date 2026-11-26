use super::*;

/// W1 (leader 01:25, DEC-194): Windows offers no Update in any update mode, so the key bar
/// shows no `u`, and a press says CrazyAgent can't update it on this computer.
#[test]
fn windows_offers_no_update_in_any_mode() {
    assert_eq!(
        update_blocked(UpdateMode::CheckAndUpdate),
        Some(UNSUPPORTED_HERE)
    );
    assert_eq!(
        update_blocked(UpdateMode::CheckOnly),
        Some("harness_update_check_only")
    );
    assert_eq!(update_blocked(UpdateMode::Off), Some("harness_update_off"));
    assert_eq!(
        update_copy(UNSUPPORTED_HERE),
        "CrazyAgent can't update it on this computer."
    );
}

#[test]
fn the_subtitle_says_to_install_and_update_harnesses_outside_crazyagent() {
    assert_eq!(
        SUBTITLE,
        "Agents run inside these tools. CrazyAgent finds them on this computer. Install and update them outside CrazyAgent."
    );
    assert!(!SUBTITLE.contains("Homebrew"));
}
