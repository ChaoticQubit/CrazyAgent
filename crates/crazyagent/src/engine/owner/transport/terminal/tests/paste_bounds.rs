use super::*;

/// Slice C: a paste chunk that is empty or over `INPUT_BYTES_MAX`, chunks past
/// `PASTE_BYTES_MAX` or a paste that is not UTF-8 are refused whole, so the view ends; a
/// paste of exactly `PASTE_BYTES_MAX` is taken.
#[test]
fn a_paste_out_of_bounds_is_refused_whole() {
    let full = [b'a'; INPUT_BYTES_MAX];
    assert_eq!(Pasting::default().add(&[], true), Err(()));
    assert_eq!(
        Pasting::default().add(&[b'a'; INPUT_BYTES_MAX + 1], true),
        Err(())
    );
    assert_eq!(
        Pasting::default().add(&[0xc3], true),
        Err(()),
        "half a UTF-8 character"
    );
    let mut over = Pasting::default();
    for _ in 0..PASTE_BYTES_MAX / INPUT_BYTES_MAX {
        assert_eq!(over.add(&full, false), Ok(None));
    }
    assert_eq!(
        over.add(b"a", true),
        Err(()),
        "one byte past PASTE_BYTES_MAX"
    );
    let mut whole = Pasting::default();
    for _ in 1..PASTE_BYTES_MAX / INPUT_BYTES_MAX {
        assert_eq!(whole.add(&full, false), Ok(None));
    }
    assert_eq!(
        whole
            .add(&full, true)
            .map(|keys| keys.map(|keys| keys.len())),
        Ok(Some(PASTE_BYTES_MAX))
    );
    assert!(!whole.open());
}
