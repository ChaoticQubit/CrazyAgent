use super::*;

/// Windows offers Update in Check and update mode as macOS and Linux do, so the key bar
/// shows `u`; check only and off still say why not, and the not-here sentence stays for a
/// system the engine runs no updater on.
#[test]
fn windows_offers_update_as_macos_and_linux_do() {
    assert_eq!(update_blocked(UpdateMode::CheckAndUpdate), None);
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
