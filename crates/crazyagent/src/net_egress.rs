use kernel_sandbox::egress::Curl;
use kernel_security::error::StoreError;
use kernel_security::net::{CaptureRules, SendRow};

/// The hosts each bundled plugin's `plugin.toml` `[net].hosts` lists; equal to the
/// manifests, which no runtime code reads. Empty while no bundled plugin asks for
/// `net.request`.
pub const NET_HOSTS: &[(&str, &[&str])] = &[];

/// The reply fields each bundled plugin's `[net.capture]` names, per host; equal to the
/// manifests.
pub const NET_CAPTURES: &[(&str, &str, &[&str])] = &[];

/// The `[net.send]` rows of each bundled plugin's manifest: the field, the hosts its value
/// may also go to, and its `keep_as` name; equal to the manifests.
pub const NET_SENDS: &[SendRow<'static>] = &[];

/// The egress of the bundled plugins, over [`NET_HOSTS`]. Output: the egress.
pub fn bundled() -> Curl {
    Curl::with_hosts(NET_HOSTS)
}

/// The capture rules of the bundled plugins, over [`NET_CAPTURES`]. Output: the rules,
/// or the refusal of a malformed table.
pub fn bundled_captures() -> Result<CaptureRules, StoreError> {
    CaptureRules::new(NET_CAPTURES)?.with_sends(NET_SENDS)
}

#[cfg(test)]
mod tests;
