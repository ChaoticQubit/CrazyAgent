use super::{ENV_VALUE_MIN, OUTPUT_MAX, holds, redact};

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

#[test]
fn overlapping_values_are_redacted_longest_first() {
    let output = b"token=ghp_abc123def and ghp_abc alone";
    assert_eq!(
        redact(output, &[b"ghp_abc", b"ghp_abc123def"], &[]),
        b"token=[REDACTED] and [REDACTED] alone".to_vec()
    );
    assert_eq!(
        redact(output, &[b"ghp_abc123def", b"ghp_abc"], &[]),
        b"token=[REDACTED] and [REDACTED] alone".to_vec()
    );
    assert_eq!(
        redact(b"aaaa", &[b"aa"], &[]),
        b"[REDACTED][REDACTED]".to_vec()
    );
}

#[test]
fn a_value_at_the_buffers_end_is_redacted() {
    assert_eq!(
        redact(b"TOKEN=s3cret", &[b"s3cret"], &[]),
        b"TOKEN=[REDACTED]".to_vec()
    );
    assert_eq!(redact(b"s3cret", &[b"s3cret"], &[]), b"[REDACTED]".to_vec());
    assert_eq!(
        redact(b"at the end: install-key-01", &[], &[b"install-key-01"]),
        b"at the end: [REDACTED]".to_vec()
    );
    assert_eq!(redact(b"s3cre", &[b"s3cret"], &[]), b"s3cre".to_vec());
}

#[test]
fn an_empty_value_is_ignored() {
    let output = b"nothing secret here";
    assert_eq!(redact(output, &[b""], &[b""]), output.to_vec());
    assert_eq!(redact(output, &[b"", b"secret"], &[]), b"nothing [REDACTED] here".to_vec());
    assert_eq!(redact(b"", &[b"secret"], &[b"longer-env"]), Vec::<u8>::new());
}

#[test]
fn a_seven_byte_env_value_is_kept() {
    let value = b"1234567";
    assert_eq!(value.len() + 1, ENV_VALUE_MIN);
    let output = b"pid 1234567 started";
    assert_eq!(redact(output, &[], &[value]), output.to_vec());
    assert_eq!(redact(output, &[value], &[]), b"pid [REDACTED] started".to_vec());
    assert_eq!(
        redact(b"pid 12345678 started", &[], &[b"12345678"]),
        b"pid [REDACTED] started".to_vec()
    );
}

#[test]
fn output_at_its_bound_is_redacted() {
    let mut output = vec![b'x'; OUTPUT_MAX - 6];
    output.extend_from_slice(b"s3cret");
    let redacted = redact(&output, &[b"s3cret"], &[]);
    assert!(redacted.ends_with(b"x[REDACTED]"));
    assert_eq!(redacted.len(), OUTPUT_MAX - 6 + 10);
}
