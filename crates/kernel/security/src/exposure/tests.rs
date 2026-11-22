use super::holds;

#[test]
fn a_value_inside_the_text_is_held() {
    assert!(holds(b"s3cret", b"--token=s3cret --verbose"));
}

#[test]
fn a_value_at_the_text_end_is_held() {
    assert!(holds(b"s3cret", b"TOKEN=s3cret"));
    assert!(holds(b"s3cret", b"s3cret"));
}

#[test]
fn an_empty_value_is_never_held() {
    assert!(!holds(b"", b"any text at all"));
    assert!(!holds(b"", b""));
}

#[test]
fn a_value_longer_than_the_text_is_not_held() {
    assert!(!holds(b"s3cret-longer", b"s3cret"));
    assert!(!holds(b"x", b""));
}

#[test]
fn a_one_byte_difference_is_not_held() {
    assert!(!holds(b"s3cret", b"--token=s3creT"));
    assert!(!holds(b"s3cret", b"--token=3cret"));
}
