use super::*;

#[test]
fn with_nudges_off_no_reminder_is_sent() {
    let team = Team::new();
    let (edits, notes) = nudged(&team, Nudges::Off);
    assert_eq!(edits, ["", ""], "no hook reminder on an edit");
    assert!(notes.is_empty(), "no engine reminder: {notes:?}");
}
