use super::HelperRequest;
use std::ffi::OsString;
use std::os::fd::RawFd;
use std::path::Path;

pub(super) const BRIDGE: &str = "--bridge";
pub(super) const BRIDGE_FD: &str = "--bridge-fd";
pub(super) const BROKER: &str = "--broker";
pub(super) const UNSHARE_NET: &str = "--unshare-net";
const V4: u8 = 4;
const V6: u8 = 6;

/// One listener the broker carries: its family ([`V4`] or [`V6`]), its port and its
/// descriptor in the broker.
type Listener = (u8, u16, RawFd);

/// The loopback bridge of a nested agent that has a network of its own, as the flags that
/// lead the helper's arguments give it: the ports carried to this computer's loopback and,
/// inside the nest only, the descriptor the helper hands its listeners over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bridge {
    pub fd: Option<RawFd>,
    pub ports: Vec<u16>,
}

impl Bridge {
    /// Splits the bridge flags that lead the helper's arguments from the rest. Input: the
    /// helper arguments. Output: the bridge, none when no such flag leads, and the
    /// arguments after it; or the reason the flags are refused.
    pub fn leading(args: Vec<OsString>) -> Result<(Option<Self>, Vec<OsString>), String> {
        Ok((None, args))
    }

    /// Tells whether a request may come with these leading flags. Inputs: the bridge, if
    /// any, and the request. Output: Ok, or the reason.
    pub fn fits(bridge: Option<&Self>, request: &HelperRequest) -> Result<(), String> {
        let _ = (bridge, request);
        Ok(())
    }
}

/// Reads the broker's arguments, `--broker` first. Input: the helper arguments. Output:
/// the request, or the reason they are refused.
pub(super) fn broker_request(args: Vec<OsString>) -> Result<HelperRequest, String> {
    Err(format!("{BROKER} with {} arguments", args.len()))
}

/// Writes the bridge flags for the helper bubblewrap starts, right after its name. Inputs:
/// the nest's arguments, the descriptor, the ports and this helper. Output: the arguments
/// with the flags, or the reason.
pub(super) fn inner(
    args: &[OsString],
    fd: RawFd,
    ports: &[u16],
    helper: &Path,
) -> Result<Vec<OsString>, String> {
    let _ = (fd, ports, helper);
    Ok(args.to_vec())
}

/// Writes the handover's text. Input: family and port of each listener, in the order the
/// descriptors are sent. Output: the bytes.
fn entries(listeners: &[(u8, u16)]) -> Vec<u8> {
    Vec::with_capacity(listeners.len())
}

/// Reads a handover back in the broker. Inputs: the text, the descriptors and the broker's
/// ports. Output: each listener, or the reason the handover is refused.
fn listed(text: &[u8], fds: &[RawFd], ports: &[u16]) -> Result<Vec<Listener>, String> {
    Err(format!("{} {} {}", text.len(), fds.len(), ports.len()))
}

/// Carries one accepted connection to this computer's loopback on the same port and back,
/// then closes it. Inputs: the family the agent used, the port and the accepted connection.
fn relay(family: u8, port: u16, agent: RawFd) {
    let _ = (family, port);
    let _ = nix::unistd::close(agent);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::socket::accept;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    fn words(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn ported(ports: std::ops::Range<u16>) -> Vec<OsString> {
        ports
            .flat_map(|port| [OsString::from(BRIDGE), port.to_string().into()])
            .collect()
    }

    #[test]
    fn bridge_flags_lead_the_arguments_and_leave_the_rest_as_it_was() {
        let inside = words(&[BRIDGE_FD, "5", BRIDGE, "8080", BRIDGE, "11434", "--probe"]);
        let (bridge, rest) = Bridge::leading(inside).unwrap();
        let handed = Bridge {
            fd: Some(5),
            ports: vec![8080, 11434],
        };
        assert_eq!(bridge, Some(handed));
        assert_eq!(rest, words(&["--probe"]));
        let outside = words(&[BRIDGE, "8080", "--nest", "--", "/usr/bin/bwrap"]);
        let (bridge, rest) = Bridge::leading(outside).unwrap();
        let asked = Bridge {
            fd: None,
            ports: vec![8080],
        };
        assert_eq!(bridge, Some(asked));
        assert_eq!(rest, words(&["--nest", "--", "/usr/bin/bwrap"]));
        let later = words(&["--probe", BRIDGE, "8080"]);
        assert_eq!(Bridge::leading(later.clone()), Ok((None, later.clone())));
        assert!(HelperRequest::parse(later).is_err());
        let (bridge, rest) = Bridge::leading(ported(8080..8088)).unwrap();
        assert_eq!(bridge.map(|bridge| bridge.ports.len()), Some(8));
        assert!(rest.is_empty());
    }

    #[test]
    fn a_bridge_takes_only_ports_the_helper_can_listen_on_and_one_descriptor_above_the_standard() {
        let refused: [&[&str]; 8] = [
            &[BRIDGE, "1023"],
            &[BRIDGE, "0"],
            &[BRIDGE, "70000"],
            &[BRIDGE],
            &[BRIDGE_FD, "5"],
            &[BRIDGE_FD, "2", BRIDGE, "8080"],
            &[BRIDGE_FD, "5", BRIDGE_FD, "6", BRIDGE, "8080"],
            &[BRIDGE, "8080", BRIDGE, "8080"],
        ];
        for flags in refused {
            assert!(Bridge::leading(words(flags)).is_err(), "{flags:?}");
        }
        assert!(Bridge::leading(ported(8080..8089)).is_err());
        let lowest = words(&[BRIDGE_FD, "3", BRIDGE, "1024"]);
        let held = Bridge {
            fd: Some(3),
            ports: vec![1024],
        };
        assert_eq!(Bridge::leading(lowest), Ok((Some(held), Vec::new())));
    }

    #[test]
    fn only_a_nest_a_probe_and_a_nested_run_take_a_bridge() {
        let asked = |words: Vec<OsString>| HelperRequest::parse(words).unwrap();
        let nest = |net: &[&str]| {
            let mut flags = words(&["--nest", "--", "/usr/bin/bwrap"]);
            flags.extend(words(&NEST_ARGS));
            flags.extend(words(net));
            flags.extend(words(&["--", "/opt/crazyagent/crazyagent-landlock", "--probe"]));
            asked(flags)
        };
        let (apart, shared) = (nest(&[UNSHARE_NET]), nest(&[]));
        let probe = asked(words(&["--probe"]));
        let nested = asked(words(&["--nested", "--", "/bin/true"]));
        let plain = asked(words(&["--", "/bin/true"]));
        let filter = asked(words(&["--filter-sockets", "--", "/bin/true"]));
        let scoped = asked(words(&["--scoped"]));
        let broker = asked(words(&[BROKER, "4242", "8080"]));
        let bridge = |fd| Bridge {
            fd,
            ports: vec![8080],
        };
        let (outer, handed) = (bridge(None), bridge(Some(5)));
        for request in [&apart, &probe, &nested, &plain, &filter, &scoped, &broker] {
            let taken = [(&apart, &outer), (&probe, &handed), (&nested, &handed)];
            for bridge in [&outer, &handed] {
                let fits = taken.contains(&(request, bridge));
                let said = Bridge::fits(Some(bridge), request);
                assert_eq!(said.is_ok(), fits, "{request:?} with {bridge:?}: {said:?}");
            }
            let alone = Bridge::fits(None, request);
            let own = !std::ptr::eq(request, &apart);
            assert_eq!(alone.is_ok(), own, "{request:?} alone: {alone:?}");
        }
        assert_eq!(Bridge::fits(None, &shared), Ok(()));
        assert!(Bridge::fits(Some(&outer), &shared).is_err());
        assert!(Bridge::fits(Some(&handed), &shared).is_err());
    }

    #[test]
    fn the_broker_is_asked_with_the_process_it_dies_with_and_its_ports_alone() {
        let asked = |flags: &[&str]| HelperRequest::parse(words(flags));
        let carried = HelperRequest::Broker {
            parent: 4242,
            ports: vec![8080, 11434],
        };
        assert_eq!(asked(&[BROKER, "4242", "8080", "11434"]), Ok(carried));
        let refused: [&[&str]; 7] = [
            &[BROKER],
            &[BROKER, "4242"],
            &[BROKER, "1", "8080"],
            &[BROKER, "4242", "80"],
            &[BROKER, "4242", "8080", "8080"],
            &[BROKER, "4242", "8080", "--probe"],
            &["--probe", BROKER, "4242", "8080"],
        ];
        for flags in refused {
            assert!(asked(flags).is_err(), "{flags:?}");
        }
        let mut many = words(&[BROKER, "4242"]);
        many.extend((8080..8089u16).map(|port| OsString::from(port.to_string())));
        assert!(HelperRequest::parse(many).is_err());
    }

    #[test]
    fn the_nest_writes_the_bridge_flags_for_this_helper_and_for_no_other_program() {
        let helper = std::env::current_exe().unwrap();
        let mut args = words(&NEST_ARGS);
        args.extend(words(&[UNSHARE_NET, "--chdir", "/work", "--"]));
        args.push(helper.clone().into());
        args.extend(words(&["--nested", "--", "/bin/sh", "--", "x"]));
        let at = NEST_ARGS.len() + 5;
        let written = inner(&args, 7, &[8080, 11434], &helper).unwrap();
        assert_eq!(written[..at], args[..at]);
        assert_eq!(written[at + 6..], args[at..]);
        let (bridge, rest) = Bridge::leading(written[at..].to_vec()).unwrap();
        let handed = Bridge {
            fd: Some(7),
            ports: vec![8080, 11434],
        };
        assert_eq!(bridge, Some(handed));
        assert_eq!(rest, args[at..]);
        let mut other = args.clone();
        other[at - 1] = "/bin/sh".into();
        assert!(inner(&other, 7, &[8080], &helper).is_err());
        assert!(inner(&args[..at - 2], 7, &[8080], &helper).is_err());
        assert!(inner(&args[..at - 1], 7, &[8080], &helper).is_err());
    }

    #[test]
    fn the_handover_names_each_listener_and_the_broker_takes_only_its_own_ports() {
        let text = entries(&[(V4, 8080), (V6, 8080), (V4, 11434)]);
        assert_eq!(text, [4, 0x1f, 0x90, 6, 0x1f, 0x90, 4, 0x2c, 0xaa]);
        let carried = vec![(V4, 8080, 10), (V6, 8080, 11), (V4, 11434, 12)];
        assert_eq!(listed(&text, &[10, 11, 12], &[8080, 11434]), Ok(carried));
        assert!(listed(&text, &[10, 11], &[8080, 11434]).is_err());
        assert!(listed(&text, &[10, 11, 12, 13], &[8080, 11434]).is_err());
        assert!(listed(&text, &[10, 11, 12], &[8080]).is_err());
        assert!(listed(&text[..8], &[10, 11, 12], &[8080, 11434]).is_err());
        assert!(listed(&[], &[], &[8080]).is_err());
        let unknown = entries(&[(5, 8080)]);
        assert!(listed(&unknown, &[10], &[8080]).is_err());
        let twice = entries(&[(V4, 8080), (V4, 8080)]);
        assert!(listed(&twice, &[10, 11], &[8080]).is_err());
        let half = entries(&[(V6, 8080)]);
        assert!(listed(&half, &[10], &[8080]).is_err());
        let short = entries(&[(V4, 8080)]);
        assert!(listed(&short, &[10], &[8080, 11434]).is_err());
    }

    #[test]
    fn the_broker_carries_a_connection_both_ways_to_the_same_port_on_this_computer() {
        let program = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = program.local_addr().unwrap().port();
        program.set_nonblocking(true).unwrap();
        let nest = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut agent = TcpStream::connect(nest.local_addr().unwrap()).unwrap();
        let accepted = accept(nest.as_raw_fd()).unwrap();
        let carried = std::thread::spawn(move || relay(V4, port, accepted));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut served = loop {
            match program.accept() {
                Ok((served, _)) => break served,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("the broker reached nothing on port {port}: {error}"),
            }
        };
        served.set_nonblocking(false).unwrap();
        agent.write_all(b"ping").unwrap();
        let mut asked = [0u8; 4];
        served.read_exact(&mut asked).unwrap();
        assert_eq!(&asked, b"ping");
        served.write_all(b"pong").unwrap();
        drop(served);
        let mut answer = Vec::new();
        agent.read_to_end(&mut answer).unwrap();
        assert_eq!(answer, b"pong");
        drop(agent);
        carried.join().unwrap();
    }

    #[test]
    fn a_bridged_port_with_nothing_behind_it_closes_the_connection() {
        let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let nest = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut agent = TcpStream::connect(nest.local_addr().unwrap()).unwrap();
        let accepted = accept(nest.as_raw_fd()).unwrap();
        relay(V4, port, accepted);
        let mut answer = Vec::new();
        assert_eq!(agent.read_to_end(&mut answer).ok(), Some(0));
    }
}
