use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The most distinct addresses one `resolve` line names.
pub const ADDRESSES_MAX: usize = 16;

/// Whether a kernel request may connect to one address, as an IP literal in its URL or as
/// any address its host name resolved to: loopback and public unicast only. Refused are the
/// unspecified, private, shared (100.64/10), IETF protocol (192.0.0/24), benchmark
/// (198.18/15), link-local and cloud metadata ([`kernel_common::net::metadata_address`]),
/// broadcast, multicast, documentation and unique-local forms, an IPv4-mapped or
/// IPv4-translated IPv6 form of any of them, every other IPv4-compatible IPv6 address, and
/// the 6to4, Teredo and discard-only IPv6 ranges, which can carry an IPv4 address of any
/// kind. Input: the address. Output: the answer.
pub fn address_allowed(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(ip) if ip == Ipv6Addr::LOCALHOST => return true,
        IpAddr::V6(ip) => match (ip.to_ipv4_mapped(), ip.segments()) {
            (Some(mapped), _) => IpAddr::V4(mapped),
            (None, [0, 0, 0, 0, 0xffff, 0, high, low]) => {
                IpAddr::V4(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)))
            }
            (None, segments) if segments[..6] == [0; 6] => return false,
            (None, _) => address,
        },
        IpAddr::V4(_) => address,
    };
    if kernel_common::net::metadata_address(address) {
        return false;
    }
    match address {
        IpAddr::V4(ip) => v4_allowed(ip),
        IpAddr::V6(ip) => v6_allowed(ip),
    }
}

/// Whether an IPv4 address is loopback or public unicast. Input: the address. Output: the
/// answer.
fn v4_allowed(ip: Ipv4Addr) -> bool {
    let [first, second, third, _] = ip.octets();
    if ip.is_loopback() {
        return true;
    }
    !(ip.is_unspecified()
        || first == 0
        || ip.is_private()
        || (first == 100 && (64..128).contains(&second))
        || (first == 192 && second == 0 && third == 0)
        || (first == 198 && (second & 0xfe) == 18)
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || first >= 240)
}

/// Whether an IPv6 address is public unicast outside the ranges that carry an IPv4 address.
/// Input: the address, never loopback, mapped, translated or compatible. Output: the answer.
fn v6_allowed(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let first = segments[0];
    !(ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        || first == 0x0064
        || first == 0x2002
        || (first == 0x2001 && segments[1] == 0)
        || (first == 0x0100 && segments[1..4] == [0; 3]))
}

/// Checks the addresses a request may connect to: an IP literal host directly, a host name
/// through one lookup, every resolved address checked before any is handed on and one refused
/// address refusing all. Inputs: the parsed URL, its port, the lookup and the refusal.
/// Output: the `resolve` value `host:port:a,b,...` naming the first [`ADDRESSES_MAX`]
/// distinct addresses in the lookup's order (IPv6 in brackets) for a host name, none for a
/// literal, the refusal, or the lookup's failure.
pub fn pinned<E>(
    parsed: &url::Url,
    port: u16,
    resolve: impl FnOnce(&str, u16) -> Result<Vec<IpAddr>, E>,
    refused: impl Fn() -> E,
) -> Result<Option<String>, E> {
    let name = match parsed.host() {
        Some(url::Host::Ipv4(address)) => {
            return address_allowed(IpAddr::V4(address))
                .then_some(None)
                .ok_or_else(&refused);
        }
        Some(url::Host::Ipv6(address)) => {
            return address_allowed(IpAddr::V6(address))
                .then_some(None)
                .ok_or_else(&refused);
        }
        Some(url::Host::Domain(name)) => name,
        None => return Err(refused()),
    };
    let resolved = resolve(name, port)?;
    if resolved.is_empty() || !resolved.iter().all(|address| address_allowed(*address)) {
        return Err(refused());
    }
    let mut kept: Vec<IpAddr> = Vec::with_capacity(ADDRESSES_MAX);
    for address in resolved {
        if kept.len() == ADDRESSES_MAX {
            break;
        }
        if !kept.contains(&address) {
            kept.push(address);
        }
    }
    assert!(!kept.is_empty() && kept.len() <= ADDRESSES_MAX);
    let addresses: Vec<String> = kept
        .iter()
        .map(|address| match address {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => format!("[{address}]"),
        })
        .collect();
    Ok(Some(format!("{name}:{port}:{}", addresses.join(","))))
}
