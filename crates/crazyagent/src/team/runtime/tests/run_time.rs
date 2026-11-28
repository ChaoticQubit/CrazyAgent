use super::*;

/// 7 days, the default before the owner picks (F01).
#[cfg(target_os = "macos")]
#[test]
fn an_unset_run_time_stops_the_live_master_after_seven_days_without_the_owner() {
    run_time_row(None);
}

/// 1 day (F01).
#[cfg(target_os = "macos")]
#[test]
fn a_one_day_run_time_stops_the_live_master_after_a_day_without_the_owner() {
    run_time_row(Some(86_400));
}

/// 3 days (F01).
#[cfg(target_os = "macos")]
#[test]
fn a_three_day_run_time_stops_the_live_master_after_three_days_without_the_owner() {
    run_time_row(Some(259_200));
}

/// 7 days picked by the owner (F01).
#[cfg(target_os = "macos")]
#[test]
fn a_seven_day_run_time_stops_the_live_master_after_seven_days_without_the_owner() {
    run_time_row(Some(604_800));
}

/// 14 days (F01).
#[cfg(target_os = "macos")]
#[test]
fn a_fourteen_day_run_time_stops_the_live_master_after_fourteen_days_without_the_owner() {
    run_time_row(Some(1_209_600));
}

/// 30 days (F01).
#[cfg(target_os = "macos")]
#[test]
fn a_thirty_day_run_time_stops_the_live_master_after_thirty_days_without_the_owner() {
    run_time_row(Some(2_592_000));
}
