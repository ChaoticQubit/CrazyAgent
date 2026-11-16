#![cfg(target_os = "macos")]

use crazyagent_mac_power::lease::{Full, HOLDERS_MAX, Holder, Lease, Leases};

const NOW_MS: u64 = 1_000_000;

fn holder(pid: i32) -> Holder {
    Holder {
        euid: 501,
        pid,
        pid_version: 7,
    }
}

#[test]
fn a_lease_runs_its_seconds_and_a_renewal_starts_it_again_from_now() {
    let mut lease = Lease::new(NOW_MS, 120);
    assert!(!lease.expired(NOW_MS + 119_999));
    assert!(lease.expired(NOW_MS + 120_000));
    assert_eq!(lease.left_s(NOW_MS + 30_000), 90);
    lease.extend(NOW_MS + 100_000, 120);
    assert!(!lease.expired(NOW_MS + 219_999));
    assert!(lease.expired(NOW_MS + 220_000));
    assert_eq!(lease.left_s(NOW_MS + 300_000), 0);
}

#[test]
fn a_release_from_another_caller_leaves_the_first_lease_running() {
    let mut leases = Leases::default();
    assert_eq!(leases.hold(holder(10), NOW_MS, 120), Ok(()));
    assert!(!leases.release(&holder(11)));
    let reused_pid = Holder {
        pid_version: 8,
        ..holder(10)
    };
    assert!(!leases.release(&reused_pid));
    assert_eq!(leases.len(), 1);
    assert_eq!(leases.left_s(NOW_MS), 120);
    assert!(leases.release(&holder(10)));
    assert!(leases.is_empty());
}

#[test]
fn a_fifth_holder_is_refused() {
    let mut leases = Leases::default();
    for pid in 0..HOLDERS_MAX as i32 {
        assert_eq!(leases.hold(holder(pid), NOW_MS, 60), Ok(()));
    }
    assert_eq!(leases.hold(holder(99), NOW_MS, 60), Err(Full));
    assert_eq!(leases.len(), HOLDERS_MAX);
    assert_eq!(leases.hold(holder(0), NOW_MS + 10_000, 120), Ok(()));
    assert_eq!(leases.len(), HOLDERS_MAX);
}

#[test]
fn expiry_ends_only_the_leases_whose_time_ran_out() {
    let mut leases = Leases::default();
    assert_eq!(leases.hold(holder(1), NOW_MS, 30), Ok(()));
    assert_eq!(leases.hold(holder(2), NOW_MS, 300), Ok(()));
    assert_eq!(leases.next_expiry_ms(), Some(NOW_MS + 30_000));
    assert_eq!(leases.expire(NOW_MS + 29_999), 0);
    assert_eq!(leases.expire(NOW_MS + 30_000), 1);
    assert_eq!(leases.len(), 1);
    assert_eq!(leases.left_s(NOW_MS + 30_000), 270);
    assert_eq!(leases.expire(NOW_MS + 300_000), 1);
    assert!(leases.is_empty());
    assert_eq!(leases.next_expiry_ms(), None);
}
