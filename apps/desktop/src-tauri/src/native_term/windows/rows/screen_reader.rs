use super::*;

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_screen_reader_reads_lines_the_selection_bounds_and_events() {
    let (parent, webview) = stand_ins();
    let failed = draw::counts().1;
    let (view, _temp) = shown_for_readers(parent, webview);
    read_in_full(&view);
    drop(view);
    no_failure_since(failed, "no surface setup, swap-chain wait or draw failed");
}
