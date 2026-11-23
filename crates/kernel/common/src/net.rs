use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The cloud metadata and credential services a VM reaches outside the link-local range,
/// which no routed agent, engine tunnel or plugin may reach (CLOUD-METADATA-ADDRS): Alibaba
/// ECS and Azure's WireServer over IPv4; AWS IMDS, AWS EKS Pod Identity, Google, OCI, Akamai
/// and Scaleway over IPv6. Only these addresses, so the rest of their ranges, such as
/// Tailscale's 100.100.100.100, stays reachable.
pub const CLOUD_METADATA: [IpAddr; 8] = [
    IpAddr::V4(Ipv4Addr::new(100, 100, 100, 200)),
    IpAddr::V4(Ipv4Addr::new(168, 63, 129, 16)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0023)),
    IpAddr::V6(Ipv6Addr::new(0xfd20, 0x00ce, 0, 0, 0, 0, 0, 0x0254)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0x00c1, 0, 0, 0, 0, 0xa9fe, 0xa9fe)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0xa9fe, 0xa9fe, 0, 0, 0, 0, 0x0001)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0x0042, 0, 0, 0, 0, 0, 0x0042)),
];

/// Tells whether `address` is a cloud's metadata or credential service: link-local
/// (169.254.0.0/16, fe80::/10) or one of [`CLOUD_METADATA`], also as an IPv4 address inside
/// an IPv6 one. Input: the address. Output: true when it is one.
pub fn metadata_address(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(ip) => ip.to_ipv4().map_or(address, IpAddr::V4),
        IpAddr::V4(_) => address,
    };
    CLOUD_METADATA.contains(&address)
        || match address {
            IpAddr::V4(ip) => ip.is_link_local(),
            IpAddr::V6(ip) => ip.is_unicast_link_local(),
        }
}
