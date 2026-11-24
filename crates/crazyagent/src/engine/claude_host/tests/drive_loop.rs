use super::*;

#[tokio::test]
async fn a_quiet_claude_turn_wakes_the_drive_loop_a_few_times_a_second() {
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let mut claude = turn_until_marked(&mut fixture, WAKE_IDLE_HARNESS, "quiet").await;
    let passes = passes_in_a_second(&claude, Duration::from_millis(300)).await;
    assert!(passes <= 10, "{passes} drive passes in a quiet second");
    claude.cancel().await.unwrap();
}
