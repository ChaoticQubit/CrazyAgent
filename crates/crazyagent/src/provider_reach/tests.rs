use super::*;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const TEST_NET: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);

type Kept<T> = Arc<Mutex<Vec<T>>>;

fn at(ip: impl Into<IpAddr>, port: u16) -> SocketAddr {
    SocketAddr::new(ip.into(), port)
}

fn refusal() -> io::Result<()> {
    Err(io::ErrorKind::ConnectionRefused.into())
}

fn no_address() -> io::Result<Vec<SocketAddr>> {
    Err(io::Error::other("nodename nor servname provided, or not known"))
}

/// A reach on stand-ins that keeps every name it looked up and every address it dialed.
/// Inputs: the lookup's answer per name and the connect's answer per address. Output: the
/// reach, the names and the addresses.
fn stand_in(
    found: impl Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync + 'static,
    dial: impl Fn(SocketAddr) -> io::Result<()> + Send + Sync + 'static,
) -> (ProviderReach, Kept<String>, Kept<SocketAddr>) {
    let looked: Kept<String> = Arc::default();
    let dialed: Kept<SocketAddr> = Arc::default();
    let (names, addresses) = (Arc::clone(&looked), Arc::clone(&dialed));
    let reach = ProviderReach::new(
        move |name, _| {
            names.lock().unwrap().push(name.to_owned());
            found(name)
        },
        move |address| {
            addresses.lock().unwrap().push(address);
            dial(address)
        },
    );
    (reach, looked, dialed)
}

/// A remote name with no address, as an error within the lookup's bound or an empty
/// answer, can't be reached, and nothing is dialed (DEC-163 addendum 8).
#[test]
fn a_remote_name_with_no_address_is_unreached_and_nothing_is_dialed() {
    let (no_name, looked, dialed) = stand_in(|_| no_address(), |_| Ok(()));
    assert!(no_name.unreached_name("models.example.test", 443));
    assert_eq!(*looked.lock().unwrap(), ["models.example.test"]);
    let (empty, _, empty_dialed) = stand_in(|_| Ok(Vec::new()), |_| Ok(()));
    assert!(empty.unreached_name("models.example.test", 443));
    assert!(dialed.lock().unwrap().is_empty());
    assert!(empty_dialed.lock().unwrap().is_empty());
}

/// A remote name is unreached only when every address it gives refuses; one that answers,
/// runs out of time, is denied or is reset names nothing, since a slow network is not a
/// dead provider (DEC-163 Q4).
#[test]
fn a_remote_name_is_unreached_only_when_every_address_it_gives_refuses() {
    let v6: Ipv6Addr = "2001:db8::1".parse().unwrap();
    let addresses = vec![at(TEST_NET, 443), at(v6, 443)];
    let given = addresses.clone();
    let (refusing, _, dialed) = stand_in(move |_| Ok(given.clone()), |_| refusal());
    assert!(refusing.unreached_name("models.example.test", 443));
    assert_eq!(*dialed.lock().unwrap(), addresses);
    for kind in [
        io::ErrorKind::TimedOut,
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::ConnectionReset,
    ] {
        let given = addresses.clone();
        let (reach, _, _) = stand_in(
            move |_| Ok(given.clone()),
            move |address| {
                if address.is_ipv4() {
                    refusal()
                } else {
                    Err(kind.into())
                }
            },
        );
        assert!(
            !reach.unreached_name("models.example.test", 443),
            "{kind:?}"
        );
    }
    let given = addresses.clone();
    let (one_answers, _, _) = stand_in(
        move |_| Ok(given.clone()),
        |address| if address.is_ipv4() { refusal() } else { Ok(()) },
    );
    assert!(!one_answers.unreached_name("models.example.test", 443));
}

/// A lookup that ran out of its bound names nothing and dials nothing.
#[test]
fn a_lookup_that_ran_out_of_time_names_nothing_and_dials_nothing() {
    let (slow, _, dialed) = stand_in(|_| Err(io::ErrorKind::TimedOut.into()), |_| refusal());
    assert!(!slow.unreached_name("models.example.test", 443));
    assert!(dialed.lock().unwrap().is_empty());
}

/// A lookup that failed on this machine, with too many open files or no memory, carries the
/// system's error number and says nothing about the name: neither a remote name nor a
/// managed account's hosts are named, and nothing is dialed. Only the resolver's own answer,
/// which carries no error number, reads as no address (DEC-163 addendum 8, r1 F1).
#[test]
fn a_lookup_that_failed_on_this_machine_names_nothing_and_dials_nothing() {
    /// EMFILE, ENFILE and ENOMEM in POSIX numbering.
    const SYSTEM_ERRORS: [i32; 3] = [24, 23, 12];
    const HOSTS: &[(&str, u16)] = &[("chatgpt.com", 443), ("api.openai.com", 443)];
    for number in SYSTEM_ERRORS {
        let (failed, looked, dialed) =
            stand_in(move |_| Err(io::Error::from_raw_os_error(number)), |_| refusal());
        assert!(!failed.unreached_name("models.example.test", 443), "{number}");
        assert!(!failed.offline(HOSTS), "{number}");
        assert_eq!(*looked.lock().unwrap(), ["models.example.test", "chatgpt.com"], "{number}");
        assert!(dialed.lock().unwrap().is_empty(), "{number}");
    }
    assert_eq!(no_address().unwrap_err().raw_os_error(), None);
}

/// The check dials only addresses the model proxy would: never loopback, unspecified,
/// link-local or 0.0.0.0/8, also inside IPv6, so a remote name's DNS never steers the
/// engine to this machine (CS-4). With nothing left to dial it names nothing; with some
/// left only those are dialed.
#[test]
fn addresses_the_model_proxy_never_dials_are_never_dialed() {
    let link: Ipv6Addr = "fe80::1".parse().unwrap();
    let local = vec![
        at(Ipv4Addr::LOCALHOST, 443),
        at(Ipv6Addr::LOCALHOST, 443),
        at(Ipv4Addr::UNSPECIFIED, 443),
        at(Ipv4Addr::new(169, 254, 0, 9), 443),
        at(link, 443),
        at(Ipv4Addr::LOCALHOST.to_ipv6_mapped(), 443),
        at(Ipv4Addr::new(0, 1, 2, 3), 443),
    ];
    let given = local.clone();
    let (reach, _, dialed) = stand_in(move |_| Ok(given.clone()), |_| refusal());
    assert!(!reach.unreached_name("rebound.example.test", 443));
    assert!(!reach.unreached_at(&local));
    assert!(
        dialed.lock().unwrap().is_empty(),
        "no address on this machine is dialed"
    );
    let mixed = [at(Ipv4Addr::LOCALHOST, 443), at(TEST_NET, 443)];
    assert!(reach.unreached_at(&mixed));
    assert_eq!(*dialed.lock().unwrap(), [at(TEST_NET, 443)]);
}

/// A name that gives more addresses than the check dials names nothing, since it can't say
/// every address refused; at the bound every address is dialed.
#[test]
fn a_name_that_gives_more_addresses_than_the_bound_names_nothing() {
    let last = u8::try_from(ADDRESSES_MAX).unwrap() + 1;
    let many: Vec<SocketAddr> = (1..=last)
        .map(|octet| at(Ipv4Addr::new(192, 0, 2, octet), 443))
        .collect();
    assert_eq!(many.len(), ADDRESSES_MAX + 1);
    let given = many.clone();
    let (reach, _, dialed) = stand_in(move |_| Ok(given.clone()), |_| refusal());
    assert!(!reach.unreached_name("many.example.test", 443));
    assert!(!reach.unreached_at(&many));
    assert!(dialed.lock().unwrap().is_empty());
    assert!(reach.unreached_at(&many[..ADDRESSES_MAX]));
    assert_eq!(dialed.lock().unwrap().len(), ADDRESSES_MAX);
}

/// A managed account is offline only when none of its hosts is found within the bound; a
/// host found or a lookup that ran out of time names nothing, and its hosts are only looked
/// up, never dialed (DEC-163 Q5).
#[test]
fn a_managed_account_is_offline_only_when_none_of_its_hosts_is_found_and_nothing_is_dialed() {
    const HOSTS: &[(&str, u16)] = &[("chatgpt.com", 443), ("api.openai.com", 443)];
    let (offline, looked, dialed) = stand_in(|_| no_address(), |_| Ok(()));
    assert!(offline.offline(HOSTS));
    assert_eq!(*looked.lock().unwrap(), ["chatgpt.com", "api.openai.com"]);
    let (empty, _, empty_dialed) = stand_in(|_| Ok(Vec::new()), |_| Ok(()));
    assert!(empty.offline(HOSTS));
    let (one_found, _, found_dialed) = stand_in(
        |name| {
            if name == "api.openai.com" {
                Ok(vec![at(TEST_NET, 443)])
            } else {
                no_address()
            }
        },
        |_| refusal(),
    );
    assert!(!one_found.offline(HOSTS));
    let (slow, _, slow_dialed) = stand_in(
        |name| {
            if name == "chatgpt.com" {
                Err(io::ErrorKind::TimedOut.into())
            } else {
                no_address()
            }
        },
        |_| Ok(()),
    );
    assert!(!slow.offline(HOSTS));
    for list in [dialed, empty_dialed, found_dialed, slow_dialed] {
        assert!(
            list.lock().unwrap().is_empty(),
            "a managed account's hosts are looked up, never dialed"
        );
    }
}

/// A lookup answers within its bound or reads TimedOut, and with the most lookups out at
/// once the next one reads TimedOut at once without starting, until one of them ends.
#[test]
fn a_lookup_runs_within_its_bound_and_no_more_than_the_cap_are_out_at_once() {
    let address = at(TEST_NET, 443);
    assert_eq!(bounded(move || Ok(vec![address])).unwrap(), [address]);
    let mut releases = Vec::new();
    let started = Instant::now();
    let callers: Vec<_> = (0..LOOKUPS_OUT_MAX)
        .map(|_| {
            let (release, held) = std::sync::mpsc::channel::<()>();
            releases.push(release);
            std::thread::spawn(move || {
                bounded(move || {
                    let _ = held.recv();
                    Ok(Vec::new())
                })
            })
        })
        .collect();
    for caller in callers {
        assert_eq!(
            caller.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }
    let waited = started.elapsed();
    assert!(
        waited >= PROVIDER_LOOKUP_TIME && waited < PROVIDER_LOOKUP_TIME * 2,
        "{waited:?}"
    );
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let at_cap = Instant::now();
    let refused = bounded(move || {
        flag.store(true, Ordering::SeqCst);
        Ok(vec![address])
    });
    assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(at_cap.elapsed() < Duration::from_millis(500));
    assert!(
        !ran.load(Ordering::SeqCst),
        "a lookup past the cap never starts"
    );
    drop(releases);
    let freed = Instant::now();
    loop {
        match bounded(move || Ok(vec![address])) {
            Ok(found) => {
                assert_eq!(found, [address]);
                break;
            }
            Err(_) => {
                assert!(
                    freed.elapsed() < Duration::from_secs(5),
                    "the held lookups never ended"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
