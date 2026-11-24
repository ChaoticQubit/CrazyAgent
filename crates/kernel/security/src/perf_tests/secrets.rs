use crate::error::StoreError;
use crate::perf_kit::{Row, SAMPLES, time};
use crate::secret::SecretString;
use crate::secrets::{Handles, OwnerEntry, Scopes, Secrets};
use crate::store::{SecretRef, Store, reference};
use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::Mutex;

const FAST: usize = 1000;
const PARSE: usize = 100;
const ONE: usize = 1;
const ONE_SHOT: usize = SAMPLES / 10 + SAMPLES;
const NAMESPACE: &str = "dev.crazyagent.e0123456789ab";
const PASTE: &[u8] = b"ghp_owner-paste-0123456789";
const HOST: &str = "api.github.com";

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "security",
        name,
        batch,
    }
}

/// The in-memory store the secrets rows write to: items by reference, no disk and no OS
/// keyring, answering stored items as the real stores do.
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

fn scopes() -> Scopes {
    Scopes::new(&[("github", "github"), ("notes", "notes")]).unwrap()
}

fn paste() -> SecretString {
    SecretString::new(PASTE.to_vec()).unwrap()
}

fn owner() -> OwnerEntry {
    OwnerEntry {
        plugin: "github",
        hosts: vec![HOST.to_owned()],
        value: paste(),
    }
}

/// Times the handle table and the secrets capability on an in-memory store. Repeatable
/// calls run in batches; a call that adds a handle or consumes a pasted entry runs once per
/// sample on its own table, under the per-plugin bound, with its inputs made before timing.
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn secrets_calls() {
    let store = MemoryStore::default();
    let table = scopes();
    let hosts = vec![HOST.to_owned()];
    let shared = Mutex::new(Handles::new());
    let secrets = Secrets::for_request(&store, NAMESPACE, &table, &shared, Some(owner()));
    let put = secrets.put_from_owner("github", "token").unwrap();
    let held = secrets.hold("github", paste(), &hosts).unwrap();
    assert_eq!(secrets.find("github", "token").unwrap(), Some(put));
    assert_eq!(
        secrets
            .value("github", held, HOST)
            .unwrap()
            .expose()
            .as_bytes(),
        PASTE
    );
    assert_eq!(shared.lock().unwrap().total(), 2);
    time(&row("secrets.Scopes.new", FAST), SAMPLES, || {
        black_box(scopes());
    });
    time(&row("secrets.Handles.new", FAST), SAMPLES, || {
        black_box(Handles::new());
    });
    let references: Vec<SecretRef> = (0..ONE_SHOT)
        .map(|at| reference(&format!("{NAMESPACE}.github"), &format!("minted-{at}")).unwrap())
        .collect();
    let mut fresh = references.into_iter();
    let mut minted = Handles::new();
    time(&row("secrets.Handles.mint", ONE), SAMPLES, || {
        black_box(minted.mint("github", fresh.next().unwrap()).unwrap());
    });
    assert_eq!(minted.total(), ONE_SHOT);
    let mut holding = Handles::new();
    time(&row("secrets.Handles.hold", ONE), SAMPLES, || {
        black_box(holding.hold("github", paste()).unwrap());
    });
    assert_eq!(holding.total(), ONE_SHOT);
    time(&row("secrets.Handles.free", FAST), SAMPLES, || {
        black_box(shared.lock().unwrap().free(black_box("github")));
    });
    time(&row("secrets.Handles.total", FAST), SAMPLES, || {
        black_box(shared.lock().unwrap().total());
    });
    time(&row("secrets.Handles.resolve", FAST), SAMPLES, || {
        black_box(
            shared
                .lock()
                .unwrap()
                .resolve("github", black_box(put))
                .is_ok(),
        );
    });
    time(&row("secrets.Handles.bind", FAST), SAMPLES, || {
        shared
            .lock()
            .unwrap()
            .bind("github", black_box(put), &hosts)
            .unwrap();
    });
    time(&row("secrets.Secrets.for_request", FAST), SAMPLES, || {
        black_box(Secrets::for_request(
            &store, NAMESPACE, &table, &shared, None,
        ));
    });
    let names: Vec<String> = (0..ONE_SHOT).map(|at| format!("token-{at}")).collect();
    let owner_handles = Mutex::new(Handles::new());
    let owned: Vec<Secrets<'_>> = (0..ONE_SHOT)
        .map(|_| Secrets::for_request(&store, NAMESPACE, &table, &owner_handles, Some(owner())))
        .collect();
    let mut next = owned.iter().zip(&names);
    time(&row("secrets.Secrets.put_from_owner", ONE), SAMPLES, || {
        let (secrets, name) = next.next().unwrap();
        black_box(secrets.put_from_owner("github", name).unwrap());
    });
    assert!(next.next().is_none());
    time(&row("secrets.Secrets.check_name", FAST), SAMPLES, || {
        black_box(secrets.check_name("github", black_box("token")).is_ok());
    });
    let value = paste();
    time(&row("secrets.Secrets.keep", PARSE), SAMPLES, || {
        black_box(secrets.keep("github", "kept", &value, &hosts).unwrap());
    });
    let hold_handles = Mutex::new(Handles::new());
    let holder = Secrets::for_request(&store, NAMESPACE, &table, &hold_handles, None);
    let mut held_ids = Vec::with_capacity(ONE_SHOT);
    time(&row("secrets.Secrets.hold", ONE), SAMPLES, || {
        held_ids.push(holder.hold("github", paste(), &hosts).unwrap());
    });
    assert_eq!(hold_handles.lock().unwrap().total(), ONE_SHOT);
    let mut next = held_ids.iter();
    time(&row("secrets.Secrets.release", ONE), SAMPLES, || {
        holder.release("github", black_box(*next.next().unwrap()));
    });
    assert!(next.next().is_none() && hold_handles.lock().unwrap().total() == 0);
    time(&row("secrets.Secrets.value", FAST), SAMPLES, || {
        black_box(secrets.value("github", black_box(held), HOST).unwrap());
    });
    time(&row("secrets.Secrets.free_handles", FAST), SAMPLES, || {
        black_box(secrets.free_handles(black_box("github")));
    });
    time(&row("secrets.Secrets.find", FAST), SAMPLES, || {
        black_box(secrets.find("github", black_box("token")).unwrap());
    });
    let forget_handles = Mutex::new(Handles::new());
    let forgetter = Secrets::for_request(&store, NAMESPACE, &table, &forget_handles, None);
    let kept: Vec<u64> = names
        .iter()
        .map(|name| forgetter.keep("github", name, &value, &hosts).unwrap().0)
        .collect();
    let mut next = kept.iter();
    time(&row("secrets.Secrets.forget", ONE), SAMPLES, || {
        forgetter.forget("github", *next.next().unwrap()).unwrap();
    });
    assert!(next.next().is_none() && forget_handles.lock().unwrap().total() == 0);
}
