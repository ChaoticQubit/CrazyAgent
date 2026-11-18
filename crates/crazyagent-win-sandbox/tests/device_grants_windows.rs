#![cfg(windows)]

use crazyagent_win_sandbox::dacl::DEVICE_GRANTS;
use crazyagent_win_sandbox::{device_grant_held, missing_device_grant};

#[test]
#[ignore = "a stock Windows image: the administrator step grants both devices (test farm)"]
fn a_stock_machine_reads_both_device_lists_and_holds_neither_grant() {
    for grant in &DEVICE_GRANTS {
        assert_eq!(device_grant_held(grant), Ok(false), "{grant:?}");
    }
    assert_eq!(missing_device_grant(), Some(&DEVICE_GRANTS[0]));
}
