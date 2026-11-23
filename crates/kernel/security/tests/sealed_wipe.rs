use kernel_security::error::StoreError;
use kernel_security::net::{CaptureRules, Egress, Method, RawReply, Request, Requests, Sealed};
use kernel_security::secret::SecretString;
use kernel_security::secrets::{Handles, OwnerEntry, Scopes, Secrets};
use kernel_security::store::{SecretRef, Store, reference};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

const TOKEN: &[u8] = b"ghp_sealed-wipe-fixture-7781";
const UNSEEN: u8 = 0;
const WIPED: u8 = 1;
const KEPT: u8 = 2;

static WATCHED: AtomicUsize = AtomicUsize::new(0);
static FREED: AtomicU8 = AtomicU8::new(UNSEEN);

/// The system allocator, which also records whether the one watched block held only zeros
/// when it was freed.
struct FreeWatch;

unsafe impl GlobalAlloc for FreeWatch {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        let address = block as usize;
        if address != 0
            && WATCHED
                .compare_exchange(address, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            let bytes = unsafe { std::slice::from_raw_parts(block, layout.size()) };
            let seen = if bytes.iter().all(|byte| *byte == 0) {
                WIPED
            } else {
                KEPT
            };
            FREED.store(seen, Ordering::SeqCst);
        }
        unsafe { System.dealloc(block, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: FreeWatch = FreeWatch;

/// An in-memory store holding what the owner put.
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
        unreachable!("the row never builds a git helper")
    }
}

/// Egress that watches the sealed config's block, drops the request and records what the
/// allocator saw when the block was freed.
struct DroppingEgress {
    held_token: Mutex<bool>,
}

impl Egress for DroppingEgress {
    fn allows(&self, _: &'static str, _: &str, _: &str, _: u16) -> Result<(), StoreError> {
        Ok(())
    }

    fn resolve(&self, _: &str, _: u16) -> Result<Vec<std::net::IpAddr>, StoreError> {
        Ok(vec!["140.82.112.4".parse().unwrap()])
    }

    fn send(&self, sealed: Sealed) -> Result<RawReply, StoreError> {
        let config = sealed.config();
        *self.held_token.lock().unwrap() =
            config.windows(TOKEN.len()).any(|window| window == TOKEN);
        WATCHED.store(config.as_ptr() as usize, Ordering::SeqCst);
        drop(sealed);
        Ok(RawReply {
            status: 200,
            headers: Vec::new(),
            body: b"{}".to_vec(),
        })
    }
}

#[test]
fn the_sealed_config_is_wiped_on_drop() {
    let store = MemoryStore::default();
    let scopes = Scopes::new(&[("github", "github")]).unwrap();
    let handles = Mutex::new(Handles::new());
    let paste = SecretString::new(TOKEN.to_vec()).unwrap();
    let secrets = Secrets::for_request(
        &store,
        "dev.crazyagent.e0123456789ab",
        &scopes,
        &handles,
        Some(OwnerEntry {
            plugin: "github",
            hosts: vec!["api.github.com".to_owned()],
            value: paste,
        }),
    );
    let token = secrets.put_from_owner("github", "token").unwrap();
    let egress = DroppingEgress {
        held_token: Mutex::new(false),
    };
    let rules = CaptureRules::new(&[]).unwrap();
    let requests = Requests::new(&secrets, &egress, &rules);
    let request = Request {
        method: Method::Get,
        url: "https://api.github.com/user".to_owned(),
        headers: Vec::new(),
        bearer: Some(token),
        form: Vec::new(),
        secret_form: Vec::new(),
        json: None,
        reply_headers: Vec::new(),
        capture: Vec::new(),
    };
    requests.request("github", request).unwrap();
    assert!(*egress.held_token.lock().unwrap());
    assert_eq!(FREED.load(Ordering::SeqCst), WIPED);
}
