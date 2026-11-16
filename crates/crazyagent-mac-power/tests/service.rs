#![cfg(target_os = "macos")]

use crazyagent_mac_power::service::Status;

#[test]
fn the_service_status_is_read_from_apples_raw_values() {
    assert_eq!(Status::from_raw(0), Some(Status::NotRegistered));
    assert_eq!(Status::from_raw(1), Some(Status::Enabled));
    assert_eq!(Status::from_raw(2), Some(Status::RequiresApproval));
    assert_eq!(Status::from_raw(3), Some(Status::NotFound));
    assert_eq!(Status::from_raw(4), None);
    assert_eq!(Status::from_raw(-1), None);
}
