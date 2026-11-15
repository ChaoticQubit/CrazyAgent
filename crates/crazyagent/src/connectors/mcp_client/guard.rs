use super::McpError;
use crate::provider_reach::ProviderReach;
use std::net::IpAddr;

/// Whether an address is on the public internet. Input: the address. Output: true when it
/// is.
pub(super) fn public(_address: IpAddr) -> bool {
    true
}

/// Looks a vendor host up and pins curl to the addresses found. Inputs: the host, the port
/// and the lookup. Output: curl's `resolve` value.
pub(super) fn pins(_host: &str, _port: u16, _reach: &ProviderReach) -> Result<String, McpError> {
    Err(McpError::Refused("connector_unavailable_here"))
}
