use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether a kernel request may connect to one address, as an IP literal in its URL or as
/// any address its host name resolved to: loopback and public unicast only. Refused are the
/// unspecified, private, shared (100.64/10), link-local and cloud metadata
/// ([`kernel_common::net::metadata_address`]), broadcast, multicast, documentation and
/// unique-local forms, an IPv4-mapped or IPv4-compatible IPv6 form of any of them, and every
/// other IPv4-compatible IPv6 address. Input: the address. Output: the answer.
pub fn address_allowed(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(ip) if ip == Ipv6Addr::LOCALHOST => return true,
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(mapped) => IpAddr::V4(mapped),
            None if ip.segments()[..6] == [0; 6] => return false,
            None => address,
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
    let [first, second, ..] = ip.octets();
    if ip.is_loopback() {
        return true;
    }
    !(ip.is_unspecified()
        || first == 0
        || ip.is_private()
        || (first == 100 && (64..128).contains(&second))
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || first >= 240)
}

/// Whether an IPv6 address is public unicast. Input: the address, never loopback, mapped
/// or compatible. Output: the answer.
fn v6_allowed(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        || first == 0x0064)
}
