use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::error::StoreError;
use crate::secret::SecretString;
use crate::store::{NAME_BYTES_MAX, SecretRef, Store, checked_name, reference};

/// The most handles one engine holds at once, over every plugin.
pub const HANDLES_MAX: usize = 4_096;
/// The most bytes of one plugin's scope leaf, so `<namespace>.<leaf>` stays a store name.
pub const SCOPE_LEAF_BYTES_MAX: usize = 34;
/// The most bytes of an engine namespace: `dev.crazyagent.e<12 hex>` is exactly this.
pub const NAMESPACE_BYTES_MAX: usize = 28;

const _: () = assert!(NAMESPACE_BYTES_MAX + 1 + SCOPE_LEAF_BYTES_MAX <= NAME_BYTES_MAX);

/// Which store service each plugin's secrets live under: `<namespace>.<leaf>`, so the
/// bundled GitHub connector's leaf `github` finds today's item byte-equal.
pub struct Scopes {
    by_plugin: BTreeMap<&'static str, &'static str>,
}

impl Scopes {
    /// Builds the scope table. Input: each plugin with its leaf. Output: the table, or
    /// `secret_scope_duplicate` for a plugin or leaf named twice, or `secret_scope_invalid`
    /// for a leaf that is not a store name of at most `SCOPE_LEAF_BYTES_MAX` bytes.
    pub fn new(entries: &[(&'static str, &'static str)]) -> Result<Self, StoreError> {
        let mut by_plugin = BTreeMap::new();
        for (plugin, leaf) in entries {
            if leaf.len() > SCOPE_LEAF_BYTES_MAX
                || leaf.contains('.')
                || checked_name(leaf).is_err()
            {
                return Err(StoreError::code("secret_scope_invalid"));
            }
            if by_plugin.values().any(|taken| taken == leaf)
                || by_plugin.insert(*plugin, *leaf).is_some()
            {
                return Err(StoreError::code("secret_scope_duplicate"));
            }
        }
        assert!(by_plugin.len() == entries.len());
        Ok(Self { by_plugin })
    }

    /// The store service of one plugin's secrets. Inputs: the namespace and the plugin.
    /// Output: `<namespace>.<leaf>`, or `secret_scope_missing` for a plugin with no scope.
    fn service(&self, namespace: &str, plugin: &str) -> Result<String, StoreError> {
        let leaf = self
            .by_plugin
            .get(plugin)
            .ok_or_else(|| StoreError::code("secret_scope_missing"))?;
        Ok(format!("{namespace}.{leaf}"))
    }
}

/// The handles minted so far: each id names one reference for the one plugin it was
/// minted for.
pub struct Handles {
    next: u64,
    by_id: BTreeMap<u64, (&'static str, SecretRef)>,
}

impl Default for Handles {
    fn default() -> Self {
        Self::new()
    }
}

impl Handles {
    /// An empty table whose first id is 1. Output: the table.
    pub fn new() -> Self {
        Self {
            next: 1,
            by_id: BTreeMap::new(),
        }
    }

    /// Mints the handle of one reference for one plugin, or returns the one it already
    /// holds for that reference. Inputs: the plugin and the reference. Output: the id, or
    /// `secret_handles_full` when `HANDLES_MAX` handles are held.
    pub fn mint(&mut self, caller: &'static str, reference: SecretRef) -> Result<u64, StoreError> {
        let held = self
            .by_id
            .iter()
            .find(|(_, (plugin, found))| *plugin == caller && *found == reference);
        if let Some((id, _)) = held {
            return Ok(*id);
        }
        if self.by_id.len() >= HANDLES_MAX {
            return Err(StoreError::code("secret_handles_full"));
        }
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .expect("handle ids never run out of u64");
        assert!(self.by_id.insert(id, (caller, reference)).is_none());
        assert!(self.by_id.len() <= HANDLES_MAX);
        Ok(id)
    }

    /// The handle one plugin holds for a reference, minting it when there is none yet.
    /// Inputs: the plugin and the reference. Output: the id and whether it is new, or
    /// `secret_handles_full`.
    fn reserve(
        &mut self,
        caller: &'static str,
        reference: &SecretRef,
    ) -> Result<(u64, bool), StoreError> {
        let before = self.by_id.len();
        let id = self.mint(caller, reference.clone())?;
        Ok((id, self.by_id.len() > before))
    }

    /// The reference behind a handle id, only for the plugin it was minted for.
    /// Output: the reference, or `secret_handle_refused` for an unknown id or another plugin's.
    pub fn resolve(&self, caller: &'static str, id: u64) -> Result<&SecretRef, StoreError> {
        match self.by_id.get(&id) {
            Some((plugin, found)) if *plugin == caller => Ok(found),
            _ => Err(StoreError::code("secret_handle_refused")),
        }
    }

    /// Drops one handle. Input: its id. Output: none.
    fn drop_id(&mut self, id: u64) {
        self.by_id.remove(&id);
    }
}

/// The secrets capabilities of one owner request: plugins name secrets and get handles;
/// a value moves only from the owner's paste into the store, never to a plugin.
pub struct Secrets<'r> {
    store: &'r dyn Store,
    namespace: &'r str,
    scopes: &'r Scopes,
    handles: &'r Mutex<Handles>,
    owner_entry: Mutex<Option<SecretString>>,
}

impl<'r> Secrets<'r> {
    /// Builds the secrets module for one owner request; `owner_entry` is the value the owner
    /// pasted in that request, moved from the client to here and never to a plugin.
    pub fn for_request(
        store: &'r dyn Store,
        namespace: &'r str,
        scopes: &'r Scopes,
        handles: &'r Mutex<Handles>,
        owner_entry: Option<SecretString>,
    ) -> Self {
        assert!(!namespace.is_empty() && namespace.len() <= NAMESPACE_BYTES_MAX);
        Self {
            store,
            namespace,
            scopes,
            handles,
            owner_entry: Mutex::new(owner_entry),
        }
    }

    /// The service and reference one plugin's name stands for. Inputs: the plugin and the
    /// name. Output: both, or the scope or name refusal.
    fn reference_of(
        &self,
        caller: &'static str,
        name: &str,
    ) -> Result<(String, SecretRef), StoreError> {
        let service = self.scopes.service(self.namespace, caller)?;
        let found = reference(&service, checked_name(name)?)?;
        Ok((service, found))
    }

    /// Saves the owner's paste under a name of the calling plugin, once per request.
    /// Inputs: the plugin and the name. Output: the handle id, or a refusal. Every refusal
    /// comes before the store write: a refused scope, name or full handle table leaves the
    /// paste for a later call and the store untouched.
    pub fn put_from_owner(&self, caller: &'static str, name: &str) -> Result<u64, StoreError> {
        let (service, found) = self.reference_of(caller, name)?;
        let mut owner = self
            .owner_entry
            .lock()
            .expect("the owner entry lock is never poisoned");
        if owner.is_none() {
            return Err(StoreError::code("secret_owner_entry_missing"));
        }
        let (id, fresh) = self.lock().reserve(caller, &found)?;
        let entry = owner.take().expect("the entry was checked just above");
        drop(owner);
        match self.store.put(&service, name, &entry) {
            Ok(saved) => {
                assert_eq!(saved, found);
                Ok(id)
            }
            Err(error) => {
                if fresh {
                    self.lock().drop_id(id);
                }
                Err(error)
            }
        }
    }

    /// Finds a name of the calling plugin. Inputs: the plugin and the name. Output: its
    /// handle id, `None` when no item exists, or a refusal; never the value.
    pub fn find(&self, caller: &'static str, name: &str) -> Result<Option<u64>, StoreError> {
        let (_, found) = self.reference_of(caller, name)?;
        match self.store.get(&found) {
            Ok(_) => self.lock().mint(caller, found).map(Some),
            Err(error) if error.0 == "credential_missing" => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Deletes the item behind a handle of the calling plugin and drops the handle.
    /// Inputs: the plugin and the id. Output: nothing, or a refusal; another plugin's
    /// handle is refused and its item kept.
    pub fn forget(&self, caller: &'static str, id: u64) -> Result<(), StoreError> {
        self.scopes.service(self.namespace, caller)?;
        let found = self.lock().resolve(caller, id)?.clone();
        self.store.delete(&found)?;
        self.lock().drop_id(id);
        Ok(())
    }

    /// The handle table, locked. Output: the guard.
    fn lock(&self) -> std::sync::MutexGuard<'_, Handles> {
        self.handles
            .lock()
            .expect("the handle table lock is never poisoned")
    }
}

#[cfg(test)]
mod tests;
