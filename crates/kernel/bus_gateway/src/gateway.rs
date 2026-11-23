use sdk::{Capability, Refusal, Tier};

use crate::Grants;

/// Where the gateway reports every call it refused, once per refused call.
pub trait Refusals {
    /// Records one refused call. Inputs: the caller's plugin id, the capability's static
    /// facts and the refusal. Output: none.
    fn refused(&self, plugin: &'static str, capability: &Capability, refusal: &Refusal);
}

/// The in-process bus gateway: it checks each SDK call against the caller's grants and
/// runs an admitted call through its bound checks and the provider the caller hands in.
pub struct Gateway<'p> {
    grants: &'p Grants,
    refusals: &'p dyn Refusals,
}

impl<'p> Gateway<'p> {
    /// Builds a gateway. Inputs: the grant table and the refusal sink. Output: the gateway.
    pub fn new(grants: &'p Grants, refusals: &'p dyn Refusals) -> Self {
        Self { grants, refusals }
    }

    /// Binds a caller to one plugin id; the only way a caller's identity enters a call.
    /// Input: the plugin id. Output: the caller that plugin's host calls the SDK through.
    pub fn bind(&self, plugin: &'static str) -> Caller<'_> {
        Caller {
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

/// A caller bound to one plugin id by the gateway; its fields are private, so code outside
/// the gateway can neither build one nor change whose calls it makes.
#[derive(Clone, Copy)]
pub struct Caller<'g> {
    gateway: &'g Gateway<'g>,
    plugin: &'static str,
}

impl Caller<'_> {
    /// The plugin id the gateway bound. Output: the id.
    pub fn plugin(&self) -> &'static str {
        self.plugin
    }

    /// Runs one call as the bound plugin: admit, the input check, the provider and the
    /// output check, in that order, with the refusal sink on an admit refusal only. Inputs:
    /// the capability, the input (several travel as one tuple), its check, the provider
    /// call and the output check. Output: the provider's result, or the refusal.
    pub fn call<I, T>(
        &self,
        capability: &Capability,
        input: I,
        check_input: impl FnOnce(&I) -> Result<(), Refusal>,
        provide: impl FnOnce(I) -> Result<T, Refusal>,
        check_output: impl FnOnce(&T) -> Result<(), Refusal>,
    ) -> Result<T, Refusal> {
        self.gateway.call(
            self.plugin,
            capability,
            input,
            check_input,
            provide,
            check_output,
        )
    }

    fn net_request(&self, request: net::Request) -> Result<net::Reply, Refusal> {
        self.gateway.call(
            self.plugin,
            &net::REQUEST,
            request,
            net::check_request_input,
            |request| self.gateway.net.request(self.plugin, request),
            net::check_request_output,
        )
    }
}
