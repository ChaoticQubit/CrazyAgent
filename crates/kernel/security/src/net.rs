use crate::error::StoreError;

pub mod address;
pub mod capture;
pub mod request;

pub use address::address_allowed;

pub use capture::{CAPTURES_MAX, CaptureRules, SEND_HOSTS_MAX, SendRow};
pub use request::{
    Capture, Captured, Egress, Field, Method, REPLY_BYTES_MAX, REPLY_LIST_MAX,
    REPLY_TEXT_BYTES_MAX, RawReply, Reply, Request, Requests, Sealed, SecretField,
};

/// Whether a target URL may carry a query: the relay refuses one, net.request allows one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    Refused,
    Allowed,
}

/// Where a request may go: the parsed URL, and whether its host is loopback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub url: url::Url,
    pub loopback: bool,
}

/// Checks a URL a kernel request may go to: https anywhere, plain http only to a loopback
/// host, no user, password or fragment, and a query only when `query` allows it.
/// Inputs: the text and the query rule. Output: the target, or `url_target_invalid`.
pub fn target(text: &str, query: Query) -> Result<Target, StoreError> {
    let invalid = || StoreError::code("url_target_invalid");
    let url = url::Url::parse(text).map_err(|_| invalid())?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => return Err(invalid()),
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err(invalid()),
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (query == Query::Refused && url.query().is_some())
    {
        return Err(invalid());
    }
    assert!(url.has_host());
    Ok(Target { url, loopback })
}

#[cfg(test)]
mod tests;
