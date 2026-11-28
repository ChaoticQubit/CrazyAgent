use sdk::{Capability, Refusal, Tier};

use crate::Grants;

/// The most served hops one call chain may take: a caller's served call is hop 1, and each
/// served call its provider makes adds one.
pub const HOPS_MAX: u8 = 4;

/// Where the gateway reports every call it refused, once per refused call.
pub trait Refusals {
    /// Records one refused call. Inputs: the caller's plugin id, the capability's static
    /// facts and the refusal. Output: none.
    fn refused(&self, plugin: &'static str, capability: &Capability, refusal: &Refusal);
}

/// Whether the owner has a plugin on: the gateway asks it before any grant check, so a plugin
/// turned off makes no call at all. A source that cannot tell must answer off.
pub trait Switches {
    /// Whether a plugin is on. Input: the plugin id. Output: true when its calls may run.
    fn is_on(&self, plugin: &str) -> bool;
}

/// Switches with every plugin on, for tests that are not about switching; the engine always
/// hands the gateway its own off set.
#[cfg(any(test, feature = "test-support"))]
pub struct AllOn;

#[cfg(any(test, feature = "test-support"))]
impl Switches for AllOn {
    fn is_on(&self, _: &str) -> bool {
        true
    }
}

/// The in-process bus gateway: it checks each SDK call against the caller's grants and
/// runs an admitted call through its bound checks and the provider the caller hands in.
pub struct Gateway<'p> {
    grants: &'p Grants,
    refusals: &'p dyn Refusals,
    switches: &'p dyn Switches,
}

impl<'p> Gateway<'p> {
    /// Builds a gateway. Inputs: the grant table, the refusal sink and the owner's plugin
    /// switches. Output: the gateway.
    pub fn new(grants: &'p Grants, refusals: &'p dyn Refusals, switches: &'p dyn Switches) -> Self {
        Self {
            grants,
            refusals,
            switches,
        }
    }

    /// Binds a caller to one plugin id; the only way a caller's identity enters a call.
    /// Input: the plugin id. Output: the caller that plugin's host calls the SDK through.
    pub fn bind(&self, plugin: &'static str) -> Caller<'_> {
        Caller {
            gateway: self,
            plugin,
            depth: 0,
        }
    }

    /// Whether a plugin may call a capability: switched on, known to the table, the
    /// capability's tier at or below its own, and holding the permission. Inputs: the plugin
    /// id and the capability. Output: nothing, `PluginOff`, `NotGranted` or `TierTooHigh`.
    pub(crate) fn admit(&self, plugin: &str, capability: &Capability) -> Result<(), Refusal> {
        if !self.switches.is_on(plugin) {
            return Err(Refusal::PluginOff {
                capability: capability.name,
            });
        }
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

/// A caller bound to one plugin id by the gateway, at the served hop its call chain has
/// reached; its fields are private, so code outside the gateway can neither build one nor
/// change whose calls it makes.
#[derive(Clone, Copy)]
pub struct Caller<'g> {
    gateway: &'g Gateway<'g>,
    plugin: &'static str,
    depth: u8,
}

impl<'g> Gateway<'g> {
    /// Runs one served call: admits the caller and reports an admit refusal once; checks the
    /// input; then finds the stem's provider in the table and hands it a caller bound to the
    /// provider's own id one hop deeper; then checks the output. The hop cap, an unserved
    /// stem and a switched-off provider fail with a gateway code and are never reported. A
    /// grant, tier or switch refusal the provider returns becomes
    /// `capability_provider_refused`; its rejection or failure passes unchanged. Inputs: the
    /// caller's plugin id and hop, the capability, the input, its check, the provider call
    /// and the output check. Output: the provider's result, or the refusal.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn serve<I, T>(
        &'g self,
        plugin: &'static str,
        depth: u8,
        capability: &Capability,
        input: I,
        check_input: impl FnOnce(&I) -> Result<(), Refusal>,
        provide: impl FnOnce(Caller<'g>, I) -> Result<T, Refusal>,
        check_output: impl FnOnce(&T) -> Result<(), Refusal>,
    ) -> Result<T, Refusal> {
        assert!(
            capability.tier != Tier::Kernel,
            "serve runs only plugin-tier capabilities"
        );
        if let Err(refusal) = self.admit(plugin, capability) {
            self.refusals.refused(plugin, capability, &refusal);
            return Err(refusal);
        }
        check_input(&input)?;
        if depth >= HOPS_MAX {
            return Err(failed("capability_call_too_deep"));
        }
        let stem = capability.name.split_once('.').map(|(stem, _)| stem);
        let stem = stem.expect("a capability name is dotted");
        let Some(provider) = self.grants.provider_of(stem) else {
            return Err(failed("capability_unserved"));
        };
        if !self.switches.is_on(provider) {
            return Err(failed("capability_provider_off"));
        }
        let (provider_tier, _) = self
            .grants
            .of(provider)
            .expect("a served stem's provider is in the table");
        assert!(
            *provider_tier <= capability.tier,
            "a provider's tier is at or below the capability's"
        );
        let served = Caller {
            gateway: self,
            plugin: provider,
            depth: depth + 1,
        };
        let output = provide(served, input).map_err(|refusal| match refusal {
            Refusal::NotGranted { .. }
            | Refusal::TierTooHigh { .. }
            | Refusal::PluginOff { .. } => failed("capability_provider_refused"),
            other => other,
        })?;
        check_output(&output)?;
        Ok(output)
    }
}

/// The `Failed` refusal of a gateway code. Input: the code. Output: the refusal.
fn failed(code: &str) -> Refusal {
    Refusal::Failed {
        code: code.to_owned(),
    }
}

impl<'g> Caller<'g> {
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

    /// Runs one call of a plugin-served capability as the bound plugin: admit, the input
    /// check, the provider the gateway's table names for the capability's stem, called with
    /// a caller bound to that provider one hop deeper, and the output check. Inputs: the
    /// capability, the input (several travel as one tuple), its check, the provider call
    /// and the output check. Output: the provider's result, or the refusal.
    pub fn serve<I, T>(
        &self,
        capability: &Capability,
        input: I,
        check_input: impl FnOnce(&I) -> Result<(), Refusal>,
        provide: impl FnOnce(Caller<'g>, I) -> Result<T, Refusal>,
        check_output: impl FnOnce(&T) -> Result<(), Refusal>,
    ) -> Result<T, Refusal> {
        self.gateway.serve(
            self.plugin,
            self.depth,
            capability,
            input,
            check_input,
            provide,
            check_output,
        )
    }
}
