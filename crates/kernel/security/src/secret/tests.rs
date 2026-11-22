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
