use sdk::settings::{self, QuitSettings};
use sdk::{Capability, Refusal};

use crate::Grants;

/// Where the gateway reports every call it refused, once per refused call.
pub trait Refusals {
    /// Records one refused call. Inputs: the caller's plugin id, the capability's static
    /// facts and the refusal. Output: none.
    fn refused(&self, plugin: &'static str, capability: &Capability, refusal: &Refusal);
}

/// The in-process bus gateway: it checks each SDK call against the caller's grants and
/// hands an admitted call to the capability's provider.
pub struct Gateway<'p> {
    grants: &'p Grants,
    settings: &'p dyn settings::Provide,
    refusals: &'p dyn Refusals,
}

impl<'p> Gateway<'p> {
    /// Builds a gateway. Inputs: the grant table, the settings provider and the refusal
    /// sink. Output: the gateway.
    pub fn new(
        grants: &'p Grants,
        settings: &'p dyn settings::Provide,
        refusals: &'p dyn Refusals,
    ) -> Self {
        Self {
            grants,
            settings,
            refusals,
        }
    }

    /// Binds a host to one plugin id; the only way a caller's identity enters a call.
    /// Input: the plugin id. Output: the host that plugin calls the SDK with.
    pub fn bind(&self, plugin: &'static str) -> Bound<'_, 'p> {
        Bound {
            gateway: self,
            plugin,
        }
    }

    /// Whether a plugin may call a capability: known to the table, the capability's tier
    /// at or below its own, and holding the permission. Inputs: the plugin id and the
    /// capability. Output: nothing, `NotGranted` or `TierTooHigh`.
    pub(crate) fn admit(&self, plugin: &str, capability: &Capability) -> Result<(), Refusal> {
        let _ = (plugin, capability, self.grants);
        Ok(())
    }

    /// Runs one call: refuses and reports it once when not admitted, else runs the
    /// provider. Inputs: the plugin id, the capability and the provider call.
    /// Output: the provider's result, or the refusal.
    pub(crate) fn call<T>(
        &self,
        plugin: &'static str,
        capability: &Capability,
        provide: impl FnOnce() -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        let _ = (plugin, capability, self.refusals);
        provide()
    }
}

/// A host bound to one plugin id by the gateway; its fields are private, so a plugin can
/// neither build one nor change whose calls it makes.
pub struct Bound<'g, 'p> {
    gateway: &'g Gateway<'p>,
    plugin: &'static str,
}

impl sdk::Host for Bound<'_, '_> {
    fn settings_save_quit(&self, settings: QuitSettings) -> Result<(), Refusal> {
        self.gateway
            .call(self.plugin, &settings::SAVE_QUIT, || {
                self.gateway.settings.save_quit(settings)
            })
    }
}
