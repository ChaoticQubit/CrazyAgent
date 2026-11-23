use std::net::IpAddr;

use crate::net::{CLOUD_METADATA, metadata_address};

#[test]
fn the_cloud_metadata_list_is_as_its_sources_write_it() {
    let listed = [
        "100.100.100.200",
        "168.63.129.16",
        "fd00:ec2::254",
        "fd00:ec2::23",
        "fd20:ce::254",
        "fd00:c1::a9fe:a9fe",
        "fd00:a9fe:a9fe::1",
        "fd00:42::42",
    ];
    let parsed: Vec<IpAddr> = listed.iter().map(|text| text.parse().unwrap()).collect();
    assert_eq!(parsed, CLOUD_METADATA);
}

#[test]
fn a_metadata_address_is_link_local_or_listed_also_inside_ipv6() {
    for address in [
        "169.254.169.254",
        "169.254.0.1",
        "fe80::1",
        "100.100.100.200",
        "168.63.129.16",
        "fd00:ec2::254",
        "::ffff:169.254.169.254",
        "::ffff:100.100.100.200",
        "::ffff:168.63.129.16",
    ] {
        assert!(metadata_address(address.parse().unwrap()), "{address}");
    }
    for address in [
        "100.100.100.100",
        "168.63.129.17",
        "fd00:ec2::255",
        "10.0.0.5",
        "127.0.0.1",
        "::1",
    ] {
        assert!(!metadata_address(address.parse().unwrap()), "{address}");
    }
}
