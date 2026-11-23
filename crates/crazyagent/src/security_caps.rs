use kernel_security::error::StoreError;
use kernel_security::secrets::Secrets;
use sdk::secrets::Secret;
use sdk::settings::QuitSettings;
use sdk::{Refusal, secrets, settings};

/// The store codes a plugin's input or handle caused; any other code is a store failure.
const REJECTED_CODES: [&str; 6] = [
    "credential_name_invalid",
    "secret_scope_missing",
    "secret_owner_entry_missing",
    "secret_handle_refused",
    "secret_handles_full",
    "credential_reference_invalid",
];

/// The refusal a plugin sees for a store error, its code unchanged: `Rejected` for a
/// refused input or handle, `Failed` for everything else. Input: the error. Output: the
/// refusal.
fn refusal(error: StoreError) -> Refusal {
    if REJECTED_CODES.contains(&error.0.as_str()) {
        Refusal::Rejected { code: error.0 }
    } else {
        Refusal::Failed { code: error.0 }
    }
}

/// The secrets capabilities a plugin calls, backed by the kernel secrets module of one
/// owner request; ids cross as SDK handles and never as values.
pub struct SecretsCaps<'r>(pub Secrets<'r>);

impl secrets::Provide for SecretsCaps<'_> {
    fn put_from_owner(&self, caller: &'static str, name: String) -> Result<Secret, Refusal> {
        self.0
            .put_from_owner(caller, &name)
            .map(Secret::from_kernel)
            .map_err(refusal)
    }

    fn find(&self, caller: &'static str, name: String) -> Result<Option<Secret>, Refusal> {
        self.0
            .find(caller, &name)
            .map(|found| found.map(Secret::from_kernel))
            .map_err(refusal)
    }

    fn forget(&self, caller: &'static str, secret: Secret) -> Result<(), Refusal> {
        self.0.forget(caller, secret.kernel_id()).map_err(refusal)
    }
}

/// The provider of a gateway built for one capability only: every other call fails with
/// `cap_provider_absent` and opens nothing, so the quit save never reaches the OS store.
pub struct Absent;

/// The failure every `Absent` call answers. Output: the refusal.
fn absent() -> Refusal {
    Refusal::Failed {
        code: "cap_provider_absent".to_owned(),
    }
}

impl secrets::Provide for Absent {
    fn put_from_owner(&self, _: &'static str, _: String) -> Result<Secret, Refusal> {
        Err(absent())
    }

    fn find(&self, _: &'static str, _: String) -> Result<Option<Secret>, Refusal> {
        Err(absent())
    }

    fn forget(&self, _: &'static str, _: Secret) -> Result<(), Refusal> {
        Err(absent())
    }
}

impl settings::Provide for Absent {
    fn save_quit(&self, _: &'static str, _: QuitSettings) -> Result<(), Refusal> {
        Err(absent())
    }
}

#[cfg(test)]
mod tests;
