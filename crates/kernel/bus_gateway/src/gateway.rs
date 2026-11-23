use sdk::secrets::{self, Secret};
use sdk::settings::{self, QuitSettings};
use sdk::{Capability, Refusal, Tier};

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
    secrets: &'p dyn secrets::Provide,
    refusals: &'p dyn Refusals,
}

impl<'p> Gateway<'p> {
    /// Builds a gateway. Inputs: the grant table, the settings and secrets providers and
    /// the refusal sink. Output: the gateway.
    pub fn new(
        grants: &'p Grants,
        settings: &'p dyn settings::Provide,
        secrets: &'p dyn secrets::Provide,
        refusals: &'p dyn Refusals,
    ) -> Self {
        Self {
            grants,
            settings,
            secrets,
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
        let not_granted = Refusal::NotGranted {
            capability: capability.name,
            needs: capability.needs,
        };
        let Some((tier, held)) = self.grants.of(plugin) else {
            return Err(not_granted);
        };
        assert!(*tier != Tier::Kernel);
        if capability.tier > *tier {
            return Err(Refusal::TierTooHigh {
                capability: capability.name,
            });
        }
        if !held.contains(capability.needs) {
            return Err(not_granted);
        }
        Ok(())
    }

    /// Runs one call: refuses and reports it once when not admitted; else checks the
    /// input's bounds, hands that same input to the provider, and checks its output's
    /// bounds, dropping an output over them whole. A bound refusal goes to the caller only,
    /// never to the sink. Several inputs travel as one tuple. Inputs: the plugin id, the
    /// capability, the input, its check, the provider call and the output check. Output:
    /// the provider's result, or the refusal.
    pub(crate) fn call<I, T>(
        &self,
        plugin: &'static str,
        capability: &Capability,
        input: I,
        check_input: impl FnOnce(&I) -> Result<(), Refusal>,
        provide: impl FnOnce(I) -> Result<T, Refusal>,
        check_output: impl FnOnce(&T) -> Result<(), Refusal>,
    ) -> Result<T, Refusal> {
        if let Err(refusal) = self.admit(plugin, capability) {
            self.refusals.refused(plugin, capability, &refusal);
            return Err(refusal);
        }
        check_input(&input)?;
        let output = provide(input)?;
        check_output(&output)?;
        Ok(output)
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
        self.gateway.call(
            self.plugin,
            &settings::SAVE_QUIT,
            settings,
            settings::check_save_quit_input,
            |settings| self.gateway.settings.save_quit(self.plugin, settings),
            settings::check_save_quit_output,
        )
    }

    fn secrets_put_from_owner(&self, name: String) -> Result<Secret, Refusal> {
        self.gateway.call(
            self.plugin,
            &secrets::PUT_FROM_OWNER,
            name,
            |name| secrets::check_put_from_owner_input(name),
            |name| self.gateway.secrets.put_from_owner(self.plugin, name),
            secrets::check_put_from_owner_output,
        )
    }

    fn secrets_find(&self, name: String) -> Result<Option<Secret>, Refusal> {
        self.gateway.call(
            self.plugin,
            &secrets::FIND,
            name,
            |name| secrets::check_find_input(name),
            |name| self.gateway.secrets.find(self.plugin, name),
            secrets::check_find_output,
        )
    }

    fn secrets_forget(&self, secret: Secret) -> Result<(), Refusal> {
        self.gateway.call(
            self.plugin,
            &secrets::FORGET,
            secret,
            secrets::check_forget_input,
            |secret| self.gateway.secrets.forget(self.plugin, secret),
            secrets::check_forget_output,
        )
    }
}
