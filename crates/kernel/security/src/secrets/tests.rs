use std::collections::BTreeMap;
use std::sync::Mutex;

use super::*;
use crate::error::StoreError;
use crate::exposure::holds;
use crate::secret::SecretString;
use crate::store::{SecretRef, Store, reference};

/// The engine namespace the rows use: 28 bytes, as `dev.crazyagent.e<12 hex>`.
const NAMESPACE: &str = "dev.crazyagent.e0123456789ab";

/// The token bytes the owner pastes in the rows.
const PASTE: &[u8] = b"ghp_owner-paste-0123456789";

/// The in-memory store double: items by reference, every call counted, and an optional
/// failure every call answers with.
#[derive(Default)]
struct FakeStore {
    items: Mutex<BTreeMap<SecretRef, Vec<u8>>>,
    calls: Mutex<u32>,
    failing: Option<&'static str>,
}

impl FakeStore {
    /// A store whose every call fails with one code. Input: the code. Output: the store.
    fn failing(code: &'static str) -> Self {
        Self {
            failing: Some(code),
            ..Self::default()
        }
    }

    /// Counts one call and answers the failure, if any. Output: nothing, or the failure.
    fn called(&self) -> Result<(), StoreError> {
        *self.calls.lock().unwrap() += 1;
        self.failing
            .map_or(Ok(()), |code| Err(StoreError::code(code)))
    }

    /// The stored bytes of one item. Inputs: service and account. Output: the bytes.
    fn held(&self, service: &str, account: &str) -> Option<Vec<u8>> {
        let found = reference(service, account).unwrap();
        self.items.lock().unwrap().get(&found).cloned()
    }
}

impl Store for FakeStore {
    fn put(
        &self,
        service: &str,
        account: &str,
        secret: &SecretString,
    ) -> Result<SecretRef, StoreError> {
        self.called()?;
        let found = reference(service, account)?;
        self.items
            .lock()
            .unwrap()
            .insert(found.clone(), secret.expose().as_bytes().to_vec());
        Ok(found)
    }

    fn get(&self, found: &SecretRef) -> Result<SecretString, StoreError> {
        self.called()?;
        let bytes = self.items.lock().unwrap().get(found).cloned();
        bytes.map_or_else(
            || Err(StoreError::code("credential_missing")),
            SecretString::new,
        )
    }

    fn delete(&self, found: &SecretRef) -> Result<(), StoreError> {
        self.called()?;
        self.items.lock().unwrap().remove(found);
        Ok(())
    }

    fn git_credential_helper(
        &self,
        _found: &SecretRef,
        _username: &str,
    ) -> Result<String, StoreError> {
        unreachable!("the secrets module never builds a git helper")
    }
}

/// The scopes of the rows: `github` on leaf `github`, `notes` on leaf `notes`.
fn scopes() -> Scopes {
    Scopes::new(&[("github", "github"), ("notes", "notes")]).expect("the scopes build")
}

/// The owner's paste. Output: the secret.
fn paste() -> SecretString {
    SecretString::new(PASTE.to_vec()).unwrap()
}

/// Puts the paste under one name for one plugin. Inputs: the store, the scopes, the
/// handles, the plugin and the name. Output: the handle id.
fn put(
    store: &FakeStore,
    scopes: &Scopes,
    handles: &Mutex<Handles>,
    caller: &'static str,
    name: &str,
) -> u64 {
    Secrets::for_request(store, NAMESPACE, scopes, handles, Some(owner(caller)))
        .put_from_owner(caller, name)
        .expect("the put is admitted")
}


/// The owner's paste for one plugin, approved for `api.github.com`. Input: the plugin.
/// Output: the entry.
fn owner(plugin: &'static str) -> OwnerEntry {
    OwnerEntry {
        plugin,
        hosts: vec!["api.github.com".to_owned()],
        value: paste(),
    }
}

#[test]
fn a_handle_minted_for_one_plugin_is_refused_for_another() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let id = put(&store, &scopes, &handles, "github", "token");
    let held = handles.lock().unwrap();
    assert!(held.resolve("github", id).is_ok());
    assert_eq!(
        held.resolve("notes", id),
        Err(StoreError::code("secret_handle_refused"))
    );
}

#[test]
fn a_forged_handle_id_is_refused() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let id = put(&store, &scopes, &handles, "github", "token");
    let held = handles.lock().unwrap();
    for forged in [id + 1, id.wrapping_sub(1), 0, u64::MAX] {
        assert_eq!(
            held.resolve("github", forged),
            Err(StoreError::code("secret_handle_refused")),
            "{forged}"
        );
    }
}

#[test]
fn put_from_owner_moves_the_paste_and_returns_only_a_handle() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let secrets = Secrets::for_request(
        &store,
        NAMESPACE,
        &scopes,
        &handles,
        Some(owner("github")),
    );
    let id = secrets.put_from_owner("github", "token").unwrap();
    let service = format!("{NAMESPACE}.github");
    assert_eq!(store.held(&service, "token").as_deref(), Some(PASTE));
    assert!(!holds(PASTE, &id.to_le_bytes()));
    assert!(!holds(PASTE, format!("{id:?}").as_bytes()));
    assert_eq!(
        secrets.put_from_owner("github", "other"),
        Err(StoreError::code("secret_owner_entry_missing"))
    );
    assert_eq!(store.held(&service, "other"), None);
}

#[test]
fn put_from_owner_without_an_entry_is_refused() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    assert_eq!(
        secrets.put_from_owner("github", "token"),
        Err(StoreError::code("secret_owner_entry_missing"))
    );
    assert_eq!(*store.calls.lock().unwrap(), 0);
}

#[test]
fn a_refused_name_keeps_the_owner_entry() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let secrets = Secrets::for_request(
        &store,
        NAMESPACE,
        &scopes,
        &handles,
        Some(owner("github")),
    );
    for refused in ["", "Token", "a b", ".token", &"t".repeat(64)] {
        assert_eq!(
            secrets.put_from_owner("github", refused),
            Err(StoreError::code("credential_name_invalid")),
            "{refused:?}"
        );
    }
    assert_eq!(
        secrets.put_from_owner("nobody", "token"),
        Err(StoreError::code("secret_scope_missing"))
    );
    assert_eq!(*store.calls.lock().unwrap(), 0);
    assert!(secrets.put_from_owner("github", "token").is_ok());
}

#[test]
fn a_full_handle_table_refuses_the_put_before_the_store_write() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    for index in 0..HANDLES_MAX {
        let found = reference(&format!("{NAMESPACE}.notes"), &format!("n{index}")).unwrap();
        handles.lock().unwrap().mint("notes", found).unwrap();
    }
    let secrets = Secrets::for_request(
        &store,
        NAMESPACE,
        &scopes,
        &handles,
        Some(owner("github")),
    );
    assert_eq!(
        secrets.put_from_owner("github", "token"),
        Err(StoreError::code("secret_handles_full"))
    );
    assert_eq!(*store.calls.lock().unwrap(), 0);
    assert!(secrets.owner_entry.lock().unwrap().is_some());
}

#[test]
fn a_failed_store_write_drops_the_new_handle() {
    let (store, scopes, handles) = (
        FakeStore::failing("credential_store_unavailable"),
        scopes(),
        Mutex::new(Handles::new()),
    );
    let secrets = Secrets::for_request(
        &store,
        NAMESPACE,
        &scopes,
        &handles,
        Some(owner("github")),
    );
    assert_eq!(
        secrets.put_from_owner("github", "token"),
        Err(StoreError::code("credential_store_unavailable"))
    );
    assert!(handles.lock().unwrap().total() == 0);
}

#[test]
fn the_github_scope_names_todays_service_and_account() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let id = put(&store, &scopes, &handles, "github", "token");
    let held = handles.lock().unwrap();
    let found = held.resolve("github", id).unwrap();
    assert_eq!(found.as_str(), "dev.crazyagent.e0123456789ab.github:token");
    assert_eq!(
        store
            .held("dev.crazyagent.e0123456789ab.github", "token")
            .as_deref(),
        Some(PASTE)
    );
}

#[test]
fn a_plugin_with_no_scope_is_refused() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    assert_eq!(
        secrets.find("nobody", "token"),
        Err(StoreError::code("secret_scope_missing"))
    );
    assert_eq!(
        secrets.forget("nobody", 1),
        Err(StoreError::code("secret_scope_missing"))
    );
    assert_eq!(*store.calls.lock().unwrap(), 0);
}

#[test]
fn find_of_a_missing_name_is_none() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    assert_eq!(secrets.find("github", "token"), Ok(None));
    assert!(handles.lock().unwrap().total() == 0);
}

#[test]
fn a_failing_store_is_not_a_missing_name() {
    let (store, scopes, handles) = (
        FakeStore::failing("credential_store_unavailable"),
        scopes(),
        Mutex::new(Handles::new()),
    );
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    assert_eq!(
        secrets.find("github", "token"),
        Err(StoreError::code("credential_store_unavailable"))
    );
}

#[test]
fn find_returns_a_handle_and_never_the_value() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let put_id = put(&store, &scopes, &handles, "github", "token");
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    let reply = secrets.find("github", "token");
    let Ok(Some(found)) = reply else {
        panic!("find answers a handle, got {reply:?}");
    };
    assert_eq!(found, put_id);
    assert!(!holds(PASTE, &found.to_le_bytes()));
    assert!(!holds(PASTE, format!("{reply:?}").as_bytes()));
}

#[test]
fn find_twice_returns_the_same_handle() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    put(&store, &scopes, &handles, "github", "token");
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    let first = secrets.find("github", "token").unwrap();
    for _ in 0..HANDLES_MAX + 1 {
        assert_eq!(secrets.find("github", "token").unwrap(), first);
    }
    assert_eq!(handles.lock().unwrap().total(), 1);
}

#[test]
fn forget_of_another_plugins_handle_is_refused() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let id = put(&store, &scopes, &handles, "github", "token");
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    assert_eq!(
        secrets.forget("notes", id),
        Err(StoreError::code("secret_handle_refused"))
    );
    assert_eq!(
        store
            .held(&format!("{NAMESPACE}.github"), "token")
            .as_deref(),
        Some(PASTE)
    );
    assert!(handles.lock().unwrap().resolve("github", id).is_ok());
}

#[test]
fn forget_deletes_and_the_handle_is_refused_after() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let id = put(&store, &scopes, &handles, "github", "token");
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    assert_eq!(secrets.forget("github", id), Ok(()));
    assert_eq!(store.held(&format!("{NAMESPACE}.github"), "token"), None);
    assert_eq!(
        secrets.forget("github", id),
        Err(StoreError::code("secret_handle_refused"))
    );
    assert_eq!(secrets.find("github", "token"), Ok(None));
}

#[test]
fn handles_stop_at_their_bound() {
    let full = Err(StoreError::code("secret_handles_full"));
    let mut handles = Handles::new();
    let plugins: Vec<&'static str> = (0..PLUGINS_MAX)
        .map(|index| &*Box::leak(format!("p{index}").into_boxed_str()))
        .collect();
    for plugin in &plugins {
        let mut minted = Vec::with_capacity(PLUGIN_HANDLES_MAX);
        for index in 0..PLUGIN_HANDLES_MAX {
            let found = reference("svc.github", &format!("n{index}")).unwrap();
            minted.push(handles.mint(plugin, found).expect("under the share"));
        }
        let over = reference("svc.github", "over").unwrap();
        assert_eq!(handles.mint(plugin, over), full);
        let again = reference("svc.github", "n0").unwrap();
        assert_eq!(handles.mint(plugin, again), Ok(minted[0]));
        assert_eq!(handles.free(plugin), 0);
    }
    assert_eq!(handles.total(), HANDLES_MAX);
    let late = reference("svc.github", "n0").unwrap();
    assert_eq!(handles.mint("late", late), full);
    assert_eq!(handles.free("late"), 0);
}

#[test]
fn one_plugin_cannot_take_the_handles_another_needs() {
    let mut handles = Handles::new();
    for index in 0..PLUGIN_HANDLES_MAX {
        let found = reference("svc.github", &format!("n{index}")).unwrap();
        handles.mint("github", found).expect("under the share");
    }
    let over = reference("svc.github", "over").unwrap();
    assert_eq!(
        handles.mint("github", over),
        Err(StoreError::code("secret_handles_full"))
    );
    assert_eq!(
        handles.hold("github", SecretString::new(b"held".to_vec()).unwrap()),
        Err(StoreError::code("secret_handles_full"))
    );
    assert_eq!(handles.free("notes"), PLUGIN_HANDLES_MAX);
    let notes = reference("svc.notes", "n0").unwrap();
    let id = handles
        .mint("notes", notes.clone())
        .expect("notes keeps its own share");
    assert_eq!(handles.resolve("notes", id), Ok(&notes));
    assert!(
        handles
            .hold("notes", SecretString::new(b"held".to_vec()).unwrap())
            .is_ok()
    );
}

#[test]
fn a_plugin_learns_nothing_from_the_gap_between_its_ids() {
    let mut handles = Handles::new();
    let first = handles
        .mint("github", reference("svc.github", "a").unwrap())
        .unwrap();
    for index in 0..10 {
        handles
            .mint(
                "notes",
                reference("svc.notes", &format!("n{index}")).unwrap(),
            )
            .unwrap();
        handles
            .hold("notes", SecretString::new(b"held".to_vec()).unwrap())
            .unwrap();
    }
    let second = handles
        .mint("github", reference("svc.github", "b").unwrap())
        .unwrap();
    assert_eq!(second, first + 1);
    let mut alone = Handles::new();
    let first_alone = alone
        .mint("github", reference("svc.github", "a").unwrap())
        .unwrap();
    let second_alone = alone
        .mint("github", reference("svc.github", "b").unwrap())
        .unwrap();
    assert_eq!((first, second), (first_alone, second_alone));
}

#[test]
fn a_second_plugin_in_the_same_request_gets_secret_owner_entry_missing() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let secrets = Secrets::for_request(
        &store,
        NAMESPACE,
        &scopes,
        &handles,
        Some(owner("github")),
    );
    assert_eq!(
        secrets.put_from_owner("notes", "token"),
        Err(StoreError::code("secret_owner_entry_missing"))
    );
    assert_eq!(*store.calls.lock().unwrap(), 0);
    let id = secrets
        .put_from_owner("github", "token")
        .expect("the addressed plugin takes the paste");
    let service = format!("{NAMESPACE}.github");
    assert_eq!(store.held(&service, "token").as_deref(), Some(PASTE));
    assert!(secrets.value("github", id, "api.github.com").is_ok());
    assert_eq!(
        secrets.put_from_owner("github", "other"),
        Err(StoreError::code("secret_owner_entry_missing"))
    );
}

#[test]
fn scopes_refuse_a_duplicate_leaf() {
    assert_eq!(
        Scopes::new(&[("github", "shared"), ("notes", "shared")]).err(),
        Some(StoreError::code("secret_scope_duplicate"))
    );
}

#[test]
fn scopes_refuse_a_duplicate_plugin() {
    assert_eq!(
        Scopes::new(&[("github", "github"), ("github", "notes")]).err(),
        Some(StoreError::code("secret_scope_duplicate"))
    );
}

#[test]
fn a_scope_leaf_outside_its_shape_is_refused() {
    let longest = "l".repeat(SCOPE_LEAF_BYTES_MAX);
    assert!(Scopes::new(&[("github", Box::leak(longest.into_boxed_str()))]).is_ok());
    let over: &'static str = Box::leak("l".repeat(SCOPE_LEAF_BYTES_MAX + 1).into_boxed_str());
    for leaf in [over, "", "Git", "a.b", ".x"] {
        assert_eq!(
            Scopes::new(&[("github", leaf)]).err(),
            Some(StoreError::code("secret_scope_invalid")),
            "{leaf:?}"
        );
    }
}

#[test]
fn forget_of_a_held_value_drops_it_without_a_store_call() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    let id = secrets.hold("github", paste(), &["api.github.com".to_owned()]).unwrap();
    assert_eq!(
        secrets.value("github", id, "api.github.com").unwrap().expose().as_bytes(),
        PASTE
    );
    secrets.forget("github", id).unwrap();
    assert_eq!(*store.calls.lock().unwrap(), 0);
    assert_eq!(
        secrets.value("github", id, "api.github.com").map(|_| ()),
        Err(StoreError::code("secret_handle_refused"))
    );
}

#[test]
fn a_held_value_is_refused_for_another_plugin_and_never_resolves_to_a_reference() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let secrets = Secrets::for_request(&store, NAMESPACE, &scopes, &handles, None);
    let id = secrets.hold("github", paste(), &["api.github.com".to_owned()]).unwrap();
    assert_eq!(
        secrets.value("notes", id, "api.github.com").map(|_| ()),
        Err(StoreError::code("secret_handle_refused"))
    );
    assert_eq!(
        secrets.forget("notes", id),
        Err(StoreError::code("secret_handle_refused"))
    );
    assert!(handles.lock().unwrap().resolve("github", id).is_err());
    assert_eq!(*store.calls.lock().unwrap(), 0);
    assert!(secrets.value("github", id, "api.github.com").is_ok());
}

#[test]
fn a_dropped_id_is_never_given_again() {
    let mut handles = Handles::new();
    let first = handles
        .hold("github", SecretString::new(b"held".to_vec()).unwrap())
        .unwrap();
    handles.drop_id("github", first);
    assert_eq!(handles.total(), 0);
    assert_eq!(handles.free("github"), PLUGIN_HANDLES_MAX);
    let second = handles
        .hold("github", SecretString::new(b"held".to_vec()).unwrap())
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        handles.resolve("github", first),
        Err(StoreError::code("secret_handle_refused"))
    );
}
