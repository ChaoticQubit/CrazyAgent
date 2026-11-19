use super::stamps::{ShowStamps, now_ms};

#[test]
fn a_hidden_view_turned_shown_is_stamped_and_a_place_while_shown_keeps_the_stamps() {
    let mut stamps = ShowStamps::default();
    stamps.shown(true, 1000.0);
    stamps.presented(1012.0);
    stamps.shown(false, 1500.0);
    let kept = ShowStamps {
        uncovered_ms: Some(1000.0),
        presented_ms: Some(1012.0),
    };
    assert_eq!(stamps, kept);
}

#[test]
fn only_the_first_frame_after_the_uncover_is_stamped() {
    let mut stamps = ShowStamps::default();
    stamps.shown(true, 1000.0);
    stamps.shown(false, 1004.0);
    assert_eq!(stamps.presented_ms, None);
    stamps.presented(1012.0);
    stamps.presented(1030.0);
    assert_eq!(stamps.presented_ms, Some(1012.0));
    assert_eq!(stamps.uncovered_ms, Some(1000.0));
}

#[test]
fn a_hide_clears_both_stamps_and_the_next_show_stamps_anew() {
    let mut stamps = ShowStamps::default();
    stamps.shown(true, 1000.0);
    stamps.presented(1012.0);
    stamps.hidden();
    assert_eq!(stamps, ShowStamps::default());
    stamps.shown(true, 2000.0);
    assert_eq!(stamps.uncovered_ms, Some(2000.0));
    assert_eq!(stamps.presented_ms, None);
    stamps.presented(2009.0);
    assert_eq!(stamps.presented_ms, Some(2009.0));
}

#[test]
#[should_panic(expected = "only a shown view presents")]
fn a_frame_before_any_uncover_is_a_host_bug() {
    ShowStamps::default().presented(1000.0);
}

#[test]
fn the_host_clock_counts_ms_since_1970() {
    let now = now_ms();
    assert!(now > 1.7e12);
    assert!(now_ms() >= now);
}
