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
    assert_eq!(reference("c", "token").unwrap_err(), invalid);
    assert_eq!(reference("a", "x").unwrap_err(), invalid);
    assert_eq!(reference("ab", "x").unwrap().as_str(), "ab:x");
    assert_eq!(
        SecretRef::try_from("a:x").unwrap_err(),
        StoreError::code("credential_reference_invalid")
    );
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

#[test]
fn try_from_refuses_exactly_what_reference_refuses() {
    let long = "a".repeat(NAME_BYTES_MAX + 1);
    let longest = "b".repeat(NAME_BYTES_MAX);
    let cases: [(&str, &str); 27] = [
        ("dev.crazyagent.test.github", "token"),
        ("svc", "aux"),
        ("aux", "a"),
        ("svc.", "a"),
        ("svc", "aux.x"),
        ("console", "a"),
        ("ab", "x"),
        ("cd", "token"),
        (&longest, &longest),
        ("svc", "a."),
        ("svc", "a_."),
        ("con.v1", "token"),
        ("aux.x", "a"),
        ("lpt9.x", "a"),
        ("nul.github", "token"),
        ("com1.x", "a"),
        ("c", "token"),
        ("a", "x"),
        ("Svc", "a"),
        ("svc", "A"),
        (&long, "a"),
        ("svc", &long),
        ("", "a"),
        ("svc", ""),
        ("x", "y:z"),
        ("svc", "y:z"),
        ("svc", "a b"),
    ];
    let mut accepted = 0;
    for (service, account) in cases {
        let joined = format!("{service}:{account}");
        let made = reference(service, account);
        let read = SecretRef::try_from(joined.as_str());
        assert_eq!(made.is_ok(), read.is_ok(), "{service:?} {account:?}");
        if let (Ok(made), Ok(read)) = (made, read) {
            assert_eq!(made, read);
            assert_eq!(read.as_str(), joined);
            accepted += 1;
        } else {
            assert_eq!(
                SecretRef::try_from(joined.as_str()).unwrap_err(),
                StoreError::code("credential_reference_invalid")
            );
        }
    }
    assert_eq!(accepted, 9);
    assert_eq!(
        SecretRef::try_from("svcx").unwrap_err(),
        StoreError::code("credential_reference_invalid")
    );
}
