use super::manifest::{Manifest, Refused};
use super::registry::Harness;
use crate::config::ModelConnection;
use crate::engine::EngineError;
use crate::model_gateway::relay::Relay;

/// What a listed harness is given to reach the model of one pool account: the address it
/// sends model requests to, the placeholder it presents there, the model, and the
/// environment its manifest's `accounts` entry asks for. It never holds a saved key.
pub struct Handover {
    base_url: String,
    token: String,
    model: String,
    env: Vec<(String, String)>,
}

impl Handover {
    /// The address the harness sends model requests to. Output: the engine relay's loopback
    /// URL for a keyed account, the account's own address for a keyless one.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The placeholder the harness presents. Output: the relay token, or the local
    /// placeholder for a keyless account; never a saved key.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The model the account names. Output: the model id.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The environment the manifest's `accounts` entry asks for, values filled in.
    /// Output: name and value pairs in file order.
    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }
}

impl std::fmt::Debug for Handover {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Handover")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field(
                "env",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Hands one pool account to a listed harness. Inputs: the harness, the stored connection
/// and the key relay started for a keyed account. Output: the hand-over, or a refusal code.
pub fn handover(
    _harness: &Harness,
    _connection: &ModelConnection,
    _relay: Option<&Relay>,
) -> Result<Handover, EngineError> {
    Err(EngineError("launch_harness_unsupported".into()))
}

/// Checks the `accounts` block of a manifest. Input: the manifest. Output: Ok, or a
/// refusal that names the setting.
pub fn check(_manifest: &Manifest) -> Result<(), Refused> {
    Ok(())
}

/// The sentence a team start shows when no account of the project's pool can run a listed
/// harness. Input: the harness. Output: the plain sentence.
pub fn no_account_line(_harness: &Harness) -> String {
    String::new()
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
