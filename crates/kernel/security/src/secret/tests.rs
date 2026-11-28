use super::{SECRET_BYTES_MAX, SecretString, TOKEN_BYTES_MAX, wipe};
use crate::error::StoreError;

fn refused(bytes: Vec<u8>) -> StoreError {
    SecretString::new(bytes).expect_err("the secret must be refused")
}

#[test]
fn secret_string_refuses_empty_long_space_control_and_non_ascii() {
    let invalid = StoreError::code("credential_secret_invalid");
    assert_eq!(refused(Vec::new()), invalid);
    assert_eq!(refused(vec![b'a'; SECRET_BYTES_MAX + 1]), invalid);
    assert_eq!(refused(b"two words".to_vec()), invalid);
    assert_eq!(refused(b"tab\there".to_vec()), invalid);
    assert_eq!(refused(b"line\n".to_vec()), invalid);
    assert_eq!(refused(vec![0x7f]), invalid);
    assert_eq!(refused("caf\u{e9}".as_bytes().to_vec()), invalid);
    let longest = SecretString::new(vec![b'~'; SECRET_BYTES_MAX]).unwrap();
    assert_eq!(longest.expose().len(), SECRET_BYTES_MAX);
    assert_eq!(SecretString::new(b"!a~".to_vec()).unwrap().expose(), "!a~");
}

#[test]
fn bearer_allows_16_kib_and_refuses_one_more() {
    assert_eq!(TOKEN_BYTES_MAX, 16_384);
    let token = SecretString::bearer(vec![b'x'; TOKEN_BYTES_MAX]).unwrap();
    assert_eq!(token.expose().len(), TOKEN_BYTES_MAX);
    assert_eq!(
        SecretString::bearer(vec![b'x'; TOKEN_BYTES_MAX + 1]).unwrap_err(),
        StoreError::code("credential_secret_invalid")
    );
    assert!(SecretString::new(vec![b'x'; SECRET_BYTES_MAX + 1]).is_err());
}

#[test]
fn debug_of_a_secret_never_prints_it() {
    let secret = SecretString::new(b"hunter2-value".to_vec()).unwrap();
    let shown = format!("{secret:?} {secret:#?}");
    assert!(!shown.contains("hunter2"));
    assert_eq!(format!("{secret:?}"), "SecretString([REDACTED])");
    assert_eq!(
        StoreError::code("credential_secret_invalid").to_string(),
        "credential_secret_invalid"
    );
}

#[test]
fn wipe_zeroes_bytes_and_wider_units() {
    let mut bytes = *b"secret-bytes";
    wipe(&mut bytes);
    assert_eq!(bytes, [0_u8; 12]);
    let mut units = [0x0041_u16, 0x0042, 0xffff];
    wipe(&mut units);
    assert_eq!(units, [0_u16; 3]);
    let mut wide = [u32::MAX; 4];
    wipe(&mut wide);
    assert_eq!(wide, [0_u32; 4]);
}

#[test]
fn sized_writes_exactly_its_measured_length() {
    let written = super::sized(&[
        super::Piece::Plain("a="),
        super::Piece::Escaped("x\"y\\z"),
        super::Piece::Plain("&"),
        super::Piece::Form("a b&c"),
        super::Piece::Plain(":"),
        super::Piece::Hex(&[0x00, 0xab, 0xff]),
    ])
    .unwrap();
    assert_eq!(written, b"a=x\\\"y\\\\z&a+b%26c:00abff".to_vec());
}

#[test]
fn sized_refuses_a_control_byte_in_an_escaped_piece() {
    for bad in ["line\n", "tab\t", "del\u{7f}"] {
        assert!(super::sized(&[super::Piece::Plain("ok"), super::Piece::Escaped(bad)]).is_none());
    }
}

/// A value whose JSON changes length between two serializations: a counter written as
/// `first` on the first pass and `then` after it, next to a secret.
struct Changing {
    first: u32,
    then: u32,
    passes: std::cell::Cell<u32>,
}

impl serde::Serialize for Changing {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let pass = self.passes.get();
        self.passes.set(pass + 1);
        let counter = if pass == 0 { self.first } else { self.then };
        (counter, "secret-bytes").serialize(serializer)
    }
}

#[test]
fn sized_json_refuses_a_value_that_grows_between_passes() {
    let value = Changing {
        first: 9,
        then: 10,
        passes: std::cell::Cell::new(0),
    };
    assert!(super::sized_json(&value).is_err());
    assert_eq!(value.passes.get(), 2);
}

#[test]
fn sized_json_refuses_a_value_that_shrinks_between_passes() {
    let value = Changing {
        first: 10,
        then: 9,
        passes: std::cell::Cell::new(0),
    };
    assert!(super::sized_json(&value).is_err());
    assert_eq!(value.passes.get(), 2);
}

#[test]
fn capped_writer_refuses_a_longer_value_without_growing_or_passing_its_limit() {
    let limit = serde_json::to_vec(&(9, "secret-bytes")).unwrap().len();
    let mut buffer = Vec::with_capacity(limit);
    let capacity = buffer.capacity();
    let refused = serde_json::to_writer(
        &mut super::Capped {
            buffer: &mut buffer,
            limit,
        },
        &(10, "secret-bytes"),
    );
    assert!(refused.is_err());
    assert_eq!(buffer.capacity(), capacity);
    assert!(buffer.len() < limit);
}

#[test]
fn capped_write_all_refuses_a_chunk_past_its_limit_before_pushing_any_byte_of_it() {
    use std::io::Write;
    let mut buffer = Vec::with_capacity(8);
    let capacity = buffer.capacity();
    let mut capped = super::Capped {
        buffer: &mut buffer,
        limit: 8,
    };
    capped.write_all(b"abc").unwrap();
    assert!(capped.write_all(b"secret").is_err());
    capped.write_all(b"defgh").unwrap();
    assert!(capped.write_all(b"i").is_err());
    assert_eq!(buffer.capacity(), capacity);
    assert_eq!(buffer, b"abcdefgh".to_vec());
}

#[test]
fn capped_writer_takes_a_value_of_its_limit_without_growing() {
    let limit = serde_json::to_vec(&(9, "secret-bytes")).unwrap().len();
    let mut buffer = Vec::with_capacity(limit);
    let capacity = buffer.capacity();
    serde_json::to_writer(
        &mut super::Capped {
            buffer: &mut buffer,
            limit,
        },
        &(9, "secret-bytes"),
    )
    .unwrap();
    assert_eq!(buffer.capacity(), capacity);
    assert_eq!(buffer, br#"[9,"secret-bytes"]"#.to_vec());
}
