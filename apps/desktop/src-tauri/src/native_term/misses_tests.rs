use super::misses::{Next, REFUSALS_MAX, Refusals, SKIP_REASONS, SKIPS_MAX, Skips, skip_reason};

#[test]
fn a_skipped_draw_asks_for_another_pass_until_the_bound_logs_once_there_and_then_waits() {
    let mut skips = Skips::default();
    for _ in 1..SKIPS_MAX {
        assert_eq!(skips.skipped("no_surface"), Next::Again);
    }
    assert_eq!(skips.skipped("no_surface"), Next::Bound);
    assert_eq!(skips.skipped("no_surface"), Next::Wait);
    assert_eq!(skips.skipped("no_surface"), Next::Wait);
    assert_eq!(skips.first(), Some("no_surface"));
}

#[test]
fn a_drawn_frame_or_a_hide_starts_the_count_again_and_the_first_reason_is_kept_until_then() {
    let mut skips = Skips::default();
    assert_eq!(skips.first(), None);
    assert_eq!(skips.skipped("frame_timeout"), Next::Again);
    assert_eq!(skips.skipped("no_screen"), Next::Again);
    assert_eq!(skips.first(), Some("frame_timeout"));
    skips.reset();
    assert_eq!(skips, Skips::default());
    assert_eq!(skips.skipped("no_screen"), Next::Again);
    assert_eq!(skips.first(), Some("no_screen"));
}

#[test]
#[should_panic(expected = "a skip names its reason")]
fn a_skip_without_a_reason_is_a_bug() {
    Skips::default().skipped("");
}

#[test]
fn a_draw_error_keeps_its_fixed_reason_and_any_other_text_is_frame_refused() {
    for code in SKIP_REASONS {
        assert_eq!(skip_reason(code), code);
    }
    assert_eq!(skip_reason("Surface timed out"), "frame_refused");
    assert_eq!(skip_reason(""), "frame_refused");
}

#[test]
fn a_refused_cover_is_told_once_per_view_per_hide() {
    let mut refusals = Refusals::default();
    assert!(refusals.refused(1));
    assert!(!refusals.refused(1));
    assert!(refusals.refused(2));
    refusals.cleared(1);
    assert!(refusals.refused(1));
    assert!(!refusals.refused(2));
    refusals.cleared(9);
    assert!(!refusals.refused(2), "clearing a view never told leaves the others");
}

#[test]
fn the_refusals_held_are_bounded_and_the_oldest_view_goes_first() {
    let mut refusals = Refusals::default();
    let last = REFUSALS_MAX as u64 + 1;
    for page in 1..=last {
        assert!(refusals.refused(page));
    }
    assert!(refusals.refused(1), "the oldest went to make room");
    assert!(!refusals.refused(last));
}

#[test]
#[should_panic(expected = "a view id is never 0")]
fn a_refusal_for_view_0_is_a_bug() {
    Refusals::default().refused(0);
}
