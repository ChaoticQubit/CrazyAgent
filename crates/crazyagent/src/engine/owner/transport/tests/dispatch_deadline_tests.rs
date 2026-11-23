use super::*;

#[test]
fn dispatch_deadline_reserves_closeout_and_keeps_control_ceiling() {
    let now = tokio::time::Instant::now();
    for (remaining, expected) in [
        (Duration::from_millis(10), Duration::from_millis(5)),
        (Duration::from_millis(1000), Duration::from_millis(500)),
        (Duration::from_secs(2), Duration::from_secs(1)),
        (Duration::from_secs(3), Duration::from_secs(1)),
        (Duration::from_secs(30), Duration::from_secs(1)),
    ] {
        let outer = now + remaining;
        let deadline = dispatch_ipc_deadline(now, outer).unwrap();
        assert_eq!(deadline, now + expected);
        assert!(deadline < outer);
        assert!(outer - deadline >= remaining / 2);
    }
}

#[test]
fn dispatch_deadline_skips_expired_and_indivisible_budget() {
    let now = tokio::time::Instant::now();
    assert_eq!(dispatch_ipc_deadline(now, now), None);
    assert_eq!(
        dispatch_ipc_deadline(now, now - Duration::from_nanos(1)),
        None
    );
    assert_eq!(
        dispatch_ipc_deadline(now, now + Duration::from_nanos(1)),
        None
    );
    assert_eq!(
        dispatch_ipc_deadline(now, now + Duration::from_nanos(2)),
        Some(now + Duration::from_nanos(1))
    );
    assert_eq!(
        dispatch_ipc_deadline(now, now + Duration::from_nanos(3)),
        Some(now + Duration::from_nanos(1))
    );
}
