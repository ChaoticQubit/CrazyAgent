use std::io;
use std::mem::{offset_of, size_of};
use std::net::{IpAddr, SocketAddr};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

/// How many times one family's listener table is read while it grows between the size the
/// system asked for and the read.
const TABLE_READS_MAX: usize = 4;

/// Whether a TCP listener on another address holds the port of `own`, a listener of this
/// process, where it could take `own`'s connections: any other address of `own`'s family,
/// or the IPv6 unspecified address beside an IPv4 `own`, since a dual-stack listener there
/// takes both families. Input: the listener's address. Output: the answer, or the I/O error
/// of reading the system's listener table.
pub fn listener_beside(own: SocketAddr) -> io::Result<bool> {
    let port = own.port();
    let beside = listeners(own.is_ipv6())?
        .into_iter()
        .any(|address| address.port() == port && address.ip() != own.ip());
    if beside || own.is_ipv6() {
        return Ok(beside);
    }
    Ok(listeners(true)?
        .into_iter()
        .any(|address| address.port() == port && address.ip().is_unspecified()))
}

/// The local address of every TCP listener of one family on this machine, from the system's
/// listener table. Relies on the table pointer naming a 4-byte aligned buffer of exactly the
/// size passed, or being null with size 0. Input: whether to read the IPv6 table. Output:
/// the addresses, or the I/O error.
fn listeners(v6: bool) -> io::Result<Vec<SocketAddr>> {
    let family = u32::from(if v6 { AF_INET6 } else { AF_INET });
    let mut words: Vec<u32> = Vec::new();
    for _ in 0..TABLE_READS_MAX {
        let mut size = u32::try_from(words.len() * size_of::<u32>()).map_err(io::Error::other)?;
        let table = if words.is_empty() {
            std::ptr::null_mut()
        } else {
            words.as_mut_ptr().cast()
        };
        let code = unsafe {
            GetExtendedTcpTable(table, &mut size, 0, family, TCP_TABLE_OWNER_PID_LISTENER, 0)
        };
        if code == NO_ERROR && !words.is_empty() {
            let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_ne_bytes()).collect();
            return Ok(addresses(&bytes, v6));
        }
        if code != NO_ERROR && code != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        words = vec![0; (size as usize).div_ceil(size_of::<u32>())];
    }
    Err(io::Error::other(
        "the listener table kept growing while it was read",
    ))
}

/// The local addresses in a listener table's bytes: its entry count of rows, at most the
/// whole rows the bytes hold. Inputs: the table's bytes and whether it is the IPv6 table.
/// Output: the addresses.
fn addresses(bytes: &[u8], v6: bool) -> Vec<SocketAddr> {
    let (count_at, start, size, address, port) = if v6 {
        (
            offset_of!(MIB_TCP6TABLE_OWNER_PID, dwNumEntries),
            offset_of!(MIB_TCP6TABLE_OWNER_PID, table),
            size_of::<MIB_TCP6ROW_OWNER_PID>(),
            offset_of!(MIB_TCP6ROW_OWNER_PID, ucLocalAddr),
            offset_of!(MIB_TCP6ROW_OWNER_PID, dwLocalPort),
        )
    } else {
        (
            offset_of!(MIB_TCPTABLE_OWNER_PID, dwNumEntries),
            offset_of!(MIB_TCPTABLE_OWNER_PID, table),
            size_of::<MIB_TCPROW_OWNER_PID>(),
            offset_of!(MIB_TCPROW_OWNER_PID, dwLocalAddr),
            offset_of!(MIB_TCPROW_OWNER_PID, dwLocalPort),
        )
    };
    let length = if v6 { 16 } else { 4 };
    assert!(address + length <= size && port + 2 <= size);
    let count = bytes
        .get(count_at..count_at + 4)
        .map_or(0, |n| u32::from_ne_bytes([n[0], n[1], n[2], n[3]]) as usize);
    let rows = bytes.get(start..).unwrap_or_default().chunks_exact(size);
    rows.take(count)
        .map(|row| {
            let ip = if v6 {
                let mut ip = [0u8; 16];
                ip.copy_from_slice(&row[address..address + 16]);
                IpAddr::from(ip)
            } else {
                let mut ip = [0u8; 4];
                ip.copy_from_slice(&row[address..address + 4]);
                IpAddr::from(ip)
            };
            SocketAddr::new(ip, u16::from_be_bytes([row[port], row[port + 1]]))
        })
        .collect()
}
