use super::{
    NAME_BYTES_MAX, REFERENCE_BYTES_MAX, SecretRef, Store, checked_name, parts, reference,
};
use crate::error::StoreError;
use crate::secret::SecretString;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn checked_name_refuses_each_bad_shape() {
    let invalid = StoreError::code("credential_name_invalid");
    let long = "a".repeat(NAME_BYTES_MAX + 1);
    for bad in [
        "",
        ".a",
        "-a",
        "a..b",
        "A",
        "a b",
        "a:b",
        "a/b",
        "caf\u{e9}",
        &long,
    ] {
        assert_eq!(checked_name(bad).unwrap_err(), invalid, "{bad:?}");
    }
    let longest = "a".repeat(NAME_BYTES_MAX);
    for good in ["a", "a.b-c_d", "0", &longest] {
        assert_eq!(checked_name(good).unwrap(), good);
    }
}

#[test]
fn reference_and_parts_round_trip() {
    let found = reference("dev.crazyagent.test.github", "token").unwrap();
    assert_eq!(found.as_str(), "dev.crazyagent.test.github:token");
    assert_eq!(
        parts(&found).unwrap(),
        ("dev.crazyagent.test.github", "token")
    );
    let longest = "a".repeat(NAME_BYTES_MAX);
    let widest = reference(&longest, &longest).unwrap();
    assert_eq!(widest.as_str().len(), REFERENCE_BYTES_MAX);
    assert_eq!(SecretRef::try_from(widest.as_str()).unwrap(), widest);
}

#[test]
fn parts_refuses_a_second_colon() {
    let invalid = StoreError::code("credential_reference_invalid");
    for bad in ["a:b:c", "a", ":b", "a:", "A:b", "a:b c"] {
        assert_eq!(SecretRef::try_from(bad).unwrap_err(), invalid, "{bad:?}");
    }
}

#[test]
fn reference_refuses_the_shapes_a_credential_ref_refuses() {
    let invalid = StoreError::code("credential_name_invalid");
    for (service, account) in [
        ("svc", "a."),
        ("svc", "a_."),
        ("con.v1", "token"),
        ("aux.x", "a"),
        ("lpt9.x", "a"),
        ("nul.github", "token"),
    ] {
        assert_eq!(
            reference(service, account).unwrap_err(),
            invalid,
            "{service:?} {account:?}"
        );
    }
    assert_eq!(reference("svc", "aux").unwrap().as_str(), "svc:aux");
    assert_eq!(reference("aux", "a").unwrap().as_str(), "aux:a");
    assert_eq!(reference("svc.", "a").unwrap().as_str(), "svc.:a");
    assert_eq!(reference("svc", "aux.x").unwrap().as_str(), "svc:aux.x");
    assert_eq!(reference("console", "a").unwrap().as_str(), "console:a");
    if cfg!(windows) {
        assert_eq!(reference("c", "token").unwrap_err(), invalid);
    } else {
        assert_eq!(reference("c", "token").unwrap().as_str(), "c:token");
    }
    assert_eq!(reference("cd", "token").unwrap().as_str(), "cd:token");
}

struct Counting(AtomicUsize);

impl Store for Counting {
    fn put(
        &self,
        service: &str,
        account: &str,
        _secret: &SecretString,
    ) -> Result<SecretRef, StoreError> {
        let found = reference(service, account)?;
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(found)
    }

    fn get(&self, _reference: &SecretRef) -> Result<SecretString, StoreError> {
        Err(StoreError::code("credential_missing"))
    }

    fn delete(&self, _reference: &SecretRef) -> Result<(), StoreError> {
        Ok(())
    }

    fn git_credential_helper(
        &self,
        _reference: &SecretRef,
        _username: &str,
    ) -> Result<String, StoreError> {
        Err(StoreError::code("credential_missing"))
    }
}

#[test]
fn a_refused_name_writes_nothing() {
    let store = Counting(AtomicUsize::new(0));
    let secret = SecretString::new(b"s3cret".to_vec()).unwrap();
    for (service, account) in [("svc", "a."), ("aux.v1", "token"), ("svc", "A")] {
        assert_eq!(
            store.put(service, account, &secret).unwrap_err(),
            StoreError::code("credential_name_invalid")
        );
    }
    assert_eq!(store.0.load(Ordering::SeqCst), 0);
    store.put("svc", "token", &secret).unwrap();
    assert_eq!(store.0.load(Ordering::SeqCst), 1);
}
