use super::*;

#[test]
fn only_a_views_focus_and_mouse_reports_are_not_pressed_keys() {
    assert!(!pressed(b""));
    assert!(!pressed(b"\x1b[I"));
    assert!(!pressed(b"\x1b[O\x1b[I"));
    assert!(!pressed(b"\x1b[<65;10;5M\x1b[<0;3;4m"));
    assert!(!pressed(b"\x1b[M`!!"));
    assert!(pressed(b"\x1b[A"), "an arrow key is the owner's");
    assert!(pressed(b"\x1bOB"));
    assert!(pressed(b"\x1b[Ix"), "a key beside a report counts");
    assert!(pressed(b"\x1b[<65;10;5"), "a cut report is not one");
    assert!(pressed(b"\x1b[M`"), "a cut X10 report is not one");
    assert!(
        pressed(b"\x1b[Mab\r"),
        "an X10 report holds no control byte, so Enter after ESC [ M a b counts"
    );
    assert!(pressed(b"\x1b[M\r\r\r"));
    assert!(pressed(b"\r"));
    assert!(pressed(b"\x1b"));
}
