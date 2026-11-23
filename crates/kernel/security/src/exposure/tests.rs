use super::{ENV_VALUE_MIN, OUTPUT_MAX, holds, redact, redact_edges};

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
    assert_eq!(redact(b"aaaa", &[b"aa"], &[]), b"[REDACTED]".to_vec());
    assert_eq!(
        redact(b"x abcdef y", &[b"abc", b"bcdef"], &[]),
        b"x [REDACTED] y".to_vec()
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
    assert_eq!(
        redact(output, &[b"", b"secret"], &[]),
        b"nothing [REDACTED] here".to_vec()
    );
    assert_eq!(
        redact(b"", &[b"secret"], &[b"longer-env"]),
        Vec::<u8>::new()
    );
}

#[test]
fn a_seven_byte_env_value_is_kept() {
    let value = b"1234567";
    assert_eq!(value.len() + 1, ENV_VALUE_MIN);
    let output = b"pid 1234567 started";
    assert_eq!(redact(output, &[], &[value]), output.to_vec());
    assert_eq!(
        redact(output, &[value], &[]),
        b"pid [REDACTED] started".to_vec()
    );
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

#[test]
fn only_a_cut_edge_redacts_the_part_of_a_value_it_left() {
    let output = b"value-1 mid TOKEN-abc";
    let env: &[&[u8]] = &[b"secret-value-1", b"TOKEN-abcdef"];
    assert_eq!(
        redact_edges(output, &[], env, false, false),
        output.to_vec()
    );
    assert_eq!(
        redact_edges(output, &[], env, true, true),
        b"[REDACTED] mid [REDACTED]".to_vec()
    );
    assert_eq!(redact(output, &[], env), output.to_vec());
}

#[test]
fn an_edge_part_and_a_whole_value_it_touches_become_one_marker() {
    let env: &[&[u8]] = &[b"/opt/tools/bin", b"/bin/python3"];
    assert_eq!(
        redact_edges(b"ols/bin/python3 ok", &[], env, true, false),
        b"[REDACTED] ok".to_vec()
    );
}

#[test]
fn a_short_environment_value_is_not_redacted_at_an_edge() {
    let short = "v".repeat(ENV_VALUE_MIN - 1);
    let output = String::from("vv end v");
    assert_eq!(
        redact_edges(output.as_bytes(), &[], &[short.as_bytes()], true, true),
        output.into_bytes()
    );
}

#[test]
fn an_output_inside_a_longer_value_cut_at_both_edges_is_covered_whole() {
    let env: &[&[u8]] = &[b"AAAAsecret-partBBBB"];
    let output = b"secret-part";
    assert_eq!(
        redact_edges(output, &[], env, true, true),
        b"[REDACTED]".to_vec()
    );
    assert_eq!(redact_edges(output, &[], env, true, false), output.to_vec());
}

#[test]
fn a_short_secret_part_is_marked_at_an_edge() {
    let secrets: &[&[u8]] = &[b"pw1234"];
    assert_eq!(
        redact_edges(b"1234 ok", secrets, &[], true, false),
        b"[REDACTED] ok".to_vec()
    );
}
