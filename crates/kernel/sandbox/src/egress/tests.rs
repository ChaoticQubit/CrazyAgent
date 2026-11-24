use super::*;
use kernel_security::error::StoreError;
use kernel_security::net::{CaptureRules, Egress, Method, Request, Requests};
use kernel_security::secret::SecretString;
use kernel_security::secrets::{Handles, Scopes, Secrets};
use kernel_security::store::{SecretRef, Store};
use std::sync::Mutex;

/// The host table of the allowlist rows.
const HOSTS: &[(&str, &[&str])] = &[
    (
        "github",
        &["github.com", "api.github.com", "ghe.example:8443"],
    ),
    ("local", &["127.0.0.1:8080", "[::1]:9000"]),
];

#[test]
fn a_host_outside_the_allowlist_is_refused() {
    let egress = Curl::with_hosts(HOSTS);
    let refused = Err(StoreError::code("net_host_refused"));
    assert_eq!(egress.allows("github", "https", "github.com", 443), Ok(()));
    assert_eq!(
        egress.allows("github", "https", "ghe.example", 8443),
        Ok(())
    );
    assert_eq!(egress.allows("local", "http", "127.0.0.1", 8080), Ok(()));
    assert_eq!(egress.allows("local", "http", "[::1]", 9000), Ok(()));
    assert_eq!(egress.allows("local", "http", "::1", 9000), refused);
    assert_eq!(
        egress.allows("github", "https", "evil.example", 443),
        refused
    );
    assert_eq!(
        egress.allows("github", "https", "github.com", 8443),
        refused
    );
    assert_eq!(
        egress.allows("github", "https", "ghe.example", 443),
        refused
    );
    assert_eq!(egress.allows("local", "https", "github.com", 443), refused);
    assert_eq!(egress.allows("notes", "https", "github.com", 443), refused);
    assert_eq!(egress.allows("local", "http", "127.0.0.1", 9090), refused);
}

/// A store the reply-header row never opens: the request it makes carries no secret.
struct Unopened;

impl Store for Unopened {
    fn put(&self, _: &str, _: &str, _: &SecretString) -> Result<SecretRef, StoreError> {
        unreachable!("a request with no secret needs no store call")
    }

    fn get(&self, _: &SecretRef) -> Result<SecretString, StoreError> {
        unreachable!("a request with no secret needs no store call")
    }

    fn delete(&self, _: &SecretRef) -> Result<(), StoreError> {
        unreachable!("a request with no secret needs no store call")
    }

    fn git_credential_helper(&self, _: &SecretRef, _: &str) -> Result<String, StoreError> {
        unreachable!("a request with no secret needs no store call")
    }
}

#[test]
fn a_reply_header_other_than_the_scopes_is_refused_before_curl() {
    let scopes = Scopes::new(&[("github", "github")]).unwrap();
    let handles = Mutex::new(Handles::new());
    let secrets = Secrets::for_request(
        &Unopened,
        "dev.crazyagent.e0123456789ab",
        &scopes,
        &handles,
        None,
    );
    let rules = CaptureRules::new(&[]).unwrap();
    let egress = Curl::with_hosts(&[("github", &["127.0.0.1:9"])]);
    let requests = Requests::new(&secrets, &egress, &rules);
    let refused = requests.request(
        "github",
        Request {
            method: Method::Get,
            url: "http://127.0.0.1:9/v1".to_owned(),
            headers: Vec::new(),
            bearer: None,
            form: Vec::new(),
            secret_form: Vec::new(),
            json: None,
            reply_headers: vec!["X-OAuth-Scopes".to_owned(), "set-cookie".to_owned()],
            capture: Vec::new(),
        },
    );
    assert_eq!(
        refused,
        Err(StoreError::code("net_reply_header_unsupported"))
    );
}
