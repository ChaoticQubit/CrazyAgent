use super::*;
use crate::harnesses::update::UNSUPPORTED_HERE;

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

/// On Windows the subtitle offers install with npm and update with the harness's own
/// updater, the macOS and Linux sentence without Homebrew.
#[test]
fn the_subtitle_offers_install_with_npm_and_update_on_windows() {
    assert_eq!(
        SUBTITLE,
        "Agents run inside these tools. CrazyAgent finds them on this computer. It can install one with npm and update one with that harness's own updater, each only when you choose it."
    );
    assert!(!SUBTITLE.contains("Homebrew"));
}
