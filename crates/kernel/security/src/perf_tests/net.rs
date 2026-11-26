use crate::error::StoreError;
use crate::net::capture::{CaptureRules, fields, take};
use crate::net::request::{Capture, Egress, Method, RawReply, Request, Requests, Sealed};
use crate::perf_kit::{Row, SAMPLES, time};
use crate::secret::SecretString;
use crate::secrets::{Handles, OwnerEntry, Scopes, Secrets};
use crate::store::{SecretRef, Store, reference};
use std::collections::BTreeMap;
use std::hint::black_box;
use std::net::IpAddr;
use std::sync::Mutex;

const FAST: usize = 1000;
const PARSE: usize = 100;
const ONE: usize = 1;
const ONE_SHOT: usize = SAMPLES / 10 + SAMPLES;
const NAMESPACE: &str = "dev.crazyagent.e0123456789ab";
const PASTE: &[u8] = b"ghp_owner-paste-0123456789";
const TOKEN_REPLY: &[u8] =
    br#"{"access_token":"gho_captured-access-token-0123456789","token_type":"bearer","scope":"repo"}"#;
const USER_REPLY: &[u8] = br#"{"login":"perf","id":1,"type":"User"}"#;

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "security",
        name,
        batch,
    }
}

/// The in-memory store the net rows write captured values to.
#[derive(Default)]
struct MemoryStore {
    items: Mutex<BTreeMap<SecretRef, Vec<u8>>>,
}

impl Store for MemoryStore {
    fn put(
        &self,
        service: &str,
        account: &str,
        secret: &SecretString,
    ) -> Result<SecretRef, StoreError> {
        let found = reference(service, account)?;
        self.items
            .lock()
            .unwrap()
            .insert(found.clone(), secret.expose().as_bytes().to_vec());
        Ok(found)
    }

    fn get(&self, found: &SecretRef) -> Result<SecretString, StoreError> {
        let bytes = self.items.lock().unwrap().get(found).cloned();
        bytes.map_or_else(
            || Err(StoreError::code("credential_missing")),
            SecretString::stored,
        )
    }

    fn delete(&self, found: &SecretRef) -> Result<(), StoreError> {
        self.items.lock().unwrap().remove(found);
        Ok(())
    }

    fn git_credential_helper(
        &self,
        _found: &SecretRef,
        _username: &str,
    ) -> Result<String, StoreError> {
        Err(StoreError::code("credential_store_unavailable"))
    }
}

/// The egress the net rows send through: every host allowed, every name resolved to one
/// public address, and one fixed reply, with no socket opened.
struct FixedEgress;

impl Egress for FixedEgress {
    fn allows(
        &self,
        _caller: &'static str,
        _scheme: &str,
        _host: &str,
        _port: u16,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<IpAddr>, StoreError> {
        Ok(vec![IpAddr::from([140, 82, 112, 4])])
    }

    fn send(&self, sealed: Sealed) -> Result<RawReply, StoreError> {
        black_box((sealed.host(), sealed.config(), sealed.reply_headers()));
        Ok(RawReply {
            status: 200,
            headers: vec![("X-OAuth-Scopes".to_owned(), "repo".to_owned())],
            body: USER_REPLY.to_vec(),
        })
    }
}

fn rules() -> CaptureRules {
    CaptureRules::new(&[("github", "github.com", &["access_token", "device_code"])]).unwrap()
}

fn capture(field: &str) -> Capture {
    Capture {
        field: field.to_owned(),
        keep_as: None,
    }
}

/// Times the capture rules and one plugin request on an in-memory store and a fixed
/// egress. `Sealed`'s accessors get no row of their own: only `Requests::request` builds a
/// `Sealed`, and the egress reads all three inside that row. Prints one PERF line per call
/// and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn net_calls() {
    let store = MemoryStore::default();
    let scopes = Scopes::new(&[("github", "github")]).unwrap();
    let handles = Mutex::new(Handles::new());
    let secrets = Secrets::for_request(
        &store,
        NAMESPACE,
        &scopes,
        &handles,
        Some(OwnerEntry {
            plugin: "github",
            hosts: vec!["api.github.com".to_owned()],
            value: SecretString::new(PASTE.to_vec()).unwrap(),
        }),
    );
    let token = secrets.put_from_owner("github", "token").unwrap();
    let sends = rules()
        .with_sends(&[("github", "access_token", &["api.github.com"], None)])
        .unwrap();
    let asked = [capture("access_token")];
    let egress = FixedEgress;
    let requests = Requests::new(&secrets, &egress, &sends);
    let request = Request {
        method: Method::Get,
        url: "https://api.github.com/user".to_owned(),
        headers: Vec::new(),
        bearer: Some(token),
        form: Vec::new(),
        secret_form: Vec::new(),
        json: None,
        reply_headers: vec!["X-OAuth-Scopes".to_owned()],
        capture: Vec::new(),
    };
    assert_eq!(
        requests.request("github", request.clone()).unwrap().status,
        200
    );
    assert_eq!(
        fields(&sends, "github", "github.com", &asked)
            .unwrap()
            .len(),
        2
    );
    time(&row("net.capture.CaptureRules.new", FAST), SAMPLES, || {
        black_box(rules());
    });
    let mut fresh: Vec<CaptureRules> = (0..ONE_SHOT).map(|_| rules()).collect();
    time(
        &row("net.capture.CaptureRules.with_sends", ONE),
        SAMPLES,
        || {
            let unsent = fresh.pop().unwrap();
            black_box(
                unsent
                    .with_sends(&[("github", "access_token", &["api.github.com"], None)])
                    .unwrap(),
            );
        },
    );
    assert!(fresh.is_empty());
    time(
        &row("net.capture.CaptureRules.send_hosts", FAST),
        SAMPLES,
        || {
            black_box(sends.send_hosts("github", black_box("access_token"), "github.com"));
        },
    );
    time(
        &row("net.capture.CaptureRules.fields", FAST),
        SAMPLES,
        || {
            black_box(sends.fields("github", black_box("github.com")));
        },
    );
    time(&row("net.capture.fields", FAST), SAMPLES, || {
        black_box(fields(&sends, "github", black_box("github.com"), &asked).unwrap());
    });
    let take_handles = Mutex::new(Handles::new());
    let taker = Secrets::for_request(&store, NAMESPACE, &scopes, &take_handles, None);
    time(&row("net.capture.take", ONE), SAMPLES, || {
        black_box(
            take(
                &taker,
                "github",
                (&sends, "github.com"),
                &asked,
                TOKEN_REPLY.to_vec(),
            )
            .unwrap(),
        );
    });
    assert_eq!(take_handles.lock().unwrap().total(), ONE_SHOT);
    time(&row("net.request.Requests.new", FAST), SAMPLES, || {
        black_box(Requests::new(&secrets, &egress, &sends));
    });
    time(&row("net.request.Requests.request", PARSE), SAMPLES, || {
        black_box(
            requests
                .request("github", black_box(request.clone()))
                .unwrap(),
        );
    });
}
