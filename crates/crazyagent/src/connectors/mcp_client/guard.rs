use super::{Access, McpError};
use crate::app_plugins::connector::{McpEndpoint, ReadOnly};
use crate::provider_reach::ProviderReach;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The most addresses of one vendor name curl is pinned to.
const PINNED_MAX: usize = 8;
/// The most addresses one lookup may give; a name with more is not called.
const ADDRESSES_MAX: usize = 32;
const HEADER_BYTES_MAX: usize = 1024;

fn refused() -> McpError {
    McpError::Refused("connector_address_refused")
}

/// The one address a session calls: its URL, the vendor's read-only header when the
/// session sends one, and whether it is the rows' stand-in on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub(super) url: url::Url,
    pub(super) header: Option<(String, String)>,
    pub(super) loopback: bool,
}

/// Checks one address before anything is looked up or sent: https to a declared host
/// that is a name, with no user, password, query, fragment or port 0. A test build also
/// takes plain http to `127.0.0.1`. Inputs: the URL and the declared hosts. Output:
/// whether it is that stand-in, or `connector_address_refused`.
fn checked(url: &url::Url, hosts: &[String]) -> Result<bool, McpError> {
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(refused());
    }
    let host = url.host_str().ok_or_else(refused)?;
    if !hosts.iter().any(|declared| declared == host) {
        return Err(refused());
    }
    match (url.scheme(), url.host()) {
        ("https", Some(url::Host::Domain(_))) => Ok(false),
        ("http", Some(url::Host::Ipv4(address)))
            if cfg!(test) && address == Ipv4Addr::LOCALHOST =>
        {
            Ok(true)
        }
        _ => Err(refused()),
    }
}

fn header_safe(name: &str, value: &str) -> bool {
    let token = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'-';
    !name.is_empty()
        && name.len() <= HEADER_BYTES_MAX
        && name.bytes().all(token)
        && !value.is_empty()
        && value.len() <= HEADER_BYTES_MAX
        && value.bytes().all(|byte| (0x20..0x7f).contains(&byte))
}

/// Picks and checks the address a session calls. Both of a connector's addresses are
/// checked whichever is used, so a bad one is never kept for later. Inputs: the endpoint
/// and whether writes are off. Output: the target, or `connector_address_refused`.
pub(super) fn target(endpoint: &McpEndpoint, access: Access) -> Result<Target, McpError> {
    let loopback = checked(&endpoint.url, &endpoint.hosts)?;
    let normal = Target {
        url: endpoint.url.clone(),
        header: None,
        loopback,
    };
    match &endpoint.read_only {
        ReadOnly::None => Ok(normal),
        ReadOnly::Url(url) => {
            let loopback = checked(url, &endpoint.hosts)?;
            Ok(match access {
                Access::Full => normal,
                Access::ReadOnly => Target {
                    url: url.clone(),
                    header: None,
                    loopback,
                },
            })
        }
        ReadOnly::Header { name, value } => {
            if !header_safe(name, value) {
                return Err(refused());
            }
            Ok(match access {
                Access::Full => normal,
                Access::ReadOnly => Target {
                    header: Some((name.clone(), value.clone())),
                    ..normal
                },
            })
        }
    }
}

fn embedded(high: u16, low: u16) -> Ipv4Addr {
    let [a, b] = high.to_be_bytes();
    let [c, d] = low.to_be_bytes();
    Ipv4Addr::new(a, b, c, d)
}

fn public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    let special = a == 0
        || a == 10
        || (a == 100 && (64..128).contains(&b))
        || a == 127
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224;
    !special
}

fn public_v6(address: Ipv6Addr) -> bool {
    if let Some(inner) = address.to_ipv4_mapped() {
        return public_v4(inner);
    }
    let parts = address.segments();
    if parts[0] == 0x64 && parts[1] == 0xff9b && parts[2..6] == [0, 0, 0, 0] {
        return public_v4(embedded(parts[6], parts[7]));
    }
    if parts[0] == 0x2002 {
        return public_v4(embedded(parts[1], parts[2]));
    }
    let global = parts[0] & 0xe000 == 0x2000;
    let special = (parts[0] == 0x2001 && (parts[1] < 0x0200 || parts[1] == 0x0db8))
        || (parts[0] == 0x3fff && parts[1] < 0x1000);
    global && !special
}

/// Whether an address is on the public internet: global unicast, and not loopback,
/// private, shared, link-local, multicast, reserved, a documentation or benchmark range,
/// or one of those inside an IPv6 address that carries an IPv4 one (mapped, NAT64, 6to4).
/// Input: the address. Output: true when the engine may send a connector's key there.
pub(super) fn public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_v4(address),
        IpAddr::V6(address) => public_v6(address),
    }
}

/// Looks a vendor host up and pins curl to what was found, so curl never looks the name
/// up again and cannot be steered elsewhere between the check and the call. Every address
/// must be public, also one past those pinned. Inputs: the host, the port and the lookup.
/// Output: curl's `resolve` value, `connector_host_unknown` or `connector_host_private`.
pub(super) fn pins(host: &str, port: u16, reach: &ProviderReach) -> Result<String, McpError> {
    let unknown = || McpError::Refused("connector_host_unknown");
    let found = reach.addresses(host, port).map_err(|_| unknown())?;
    if found.is_empty() || found.len() > ADDRESSES_MAX {
        return Err(unknown());
    }
    if !found.iter().all(|address| public(address.ip())) {
        return Err(McpError::Refused("connector_host_private"));
    }
    let mut pinned: Vec<IpAddr> = Vec::with_capacity(PINNED_MAX);
    for address in &found {
        let address = address.ip();
        if pinned.len() < PINNED_MAX && !pinned.contains(&address) {
            pinned.push(address);
        }
    }
    assert!(!pinned.is_empty() && pinned.len() <= PINNED_MAX);
    let mut pins = format!("{host}:{port}:");
    for (at, address) in pinned.iter().enumerate() {
        if at > 0 {
            pins.push(',');
        }
        match address {
            IpAddr::V4(address) => pins.push_str(&address.to_string()),
            IpAddr::V6(address) => pins.push_str(&format!("[{address}]")),
        }
    }
    Ok(pins)
}
