use super::super::REMOTES_MAX;
use super::{HelperRequest, NEST_ARGS};
use rustix::thread::CapabilitySet;
use std::ffi::OsString;
use std::os::fd::RawFd;
use std::process::ExitCode;

pub(super) const ROUTE: &str = "--route";
pub(super) const EGRESS: &str = "--egress";
const INFO_FD: &str = "--info-fd";
const BLOCK_FD: &str = "--block-fd";
const RO_BIND_DATA: &str = "--ro-bind-data";
const RESOLV_CONF: &str = "/etc/resolv.conf";
/// The bubblewrap flags stage 1 writes itself, each with a descriptor it opened, in this
/// order and right after `--unshare-net`, so the engine never names one ([`placed`]).
pub(super) const DESCRIPTOR_FLAGS: [&str; 3] = [INFO_FD, BLOCK_FD, RO_BIND_DATA];
/// Where the setup child takes slirp4netns from, and only from there: no flag or variable
/// names another program, which would then run with network capabilities in the agent's
/// user namespace (DEC-179 SBX-2).
pub const SLIRP_PATHS: &[&str] = &["/usr/bin/slirp4netns"];
/// What the setup child says when the network namespace it was handed is the one it runs
/// in, so the agent would share this computer's network.
pub const NETWORK_SHARED: &str = "the agent's network is not one of its own";

/// Splits the route flags that lead the helper's arguments from the rest: `--route` with a
/// port the agent may reach on any host, one to [`REMOTES_MAX`] times, each once. Input:
/// the helper arguments. Output: the ports, empty when no such flag leads, and the
/// arguments after them; or the reason the flags are refused.
pub(super) fn leading(args: Vec<OsString>) -> Result<(Vec<u16>, Vec<OsString>), String> {
    if args.first().is_some_and(|flag| flag == ROUTE) {
        return Err(format!("{ROUTE} is refused"));
    }
    Ok((Vec::new(), args))
}

/// Tells whether a request may come with these route ports: only a nest whose bubblewrap
/// unshares the network takes them, so no agent is routed on this computer's network.
/// Inputs: the ports, empty for none, and the request. Output: Ok, or the reason.
pub(super) fn fits(routes: &[u16], request: &HelperRequest) -> Result<(), String> {
    assert!(routes.len() <= REMOTES_MAX);
    Ok(())
}

/// Reads the setup child's arguments, `--egress` first: the stage-1 process it dies with,
/// then the ports the rules let out. Input: the helper arguments. Output: the request, or
/// the reason they are refused.
pub(super) fn request(args: Vec<OsString>) -> Result<HelperRequest, String> {
    assert!(args.first().is_some_and(|flag| flag == EGRESS));
    Err(format!("{EGRESS} is refused"))
}

/// The setup child's work, outside the sandbox: it joins the agent's namespaces, starts
/// slirp4netns, applies the rules and reads them back, and only then lets bubblewrap start
/// the agent. Inputs: the stage-1 process id and the route ports. Output: the exit code,
/// 3 when the network could not be built, which leaves the agent unstarted.
pub(super) fn setup(parent: u32, ports: &[u16]) -> ExitCode {
    assert!(parent > 1 && !ports.is_empty());
    ExitCode::from(3)
}

/// The IPv4 rules inside the agent's network, as `iptables-restore` reads them and in the
/// form `iptables-save` prints them: out only through `tap0` to each port, TCP 53 only to
/// slirp's resolver, never to slirp's gateway, and loopback for the bridged ports. Input:
/// the ports on any host. Output: the rules.
fn egress_rules(any: &[u16]) -> String {
    assert!(!any.is_empty() && any.len() <= REMOTES_MAX);
    String::new()
}

/// The IPv6 rules inside the agent's network: out over loopback only, since slirp4netns
/// runs without IPv6. No input. Output: the rules.
fn egress_rules_v6() -> String {
    String::new()
}

/// The rules as the read-back compares them: the table, chain policy and `-A` lines and
/// `COMMIT`, without `#` lines and without the packet and byte counters. Input: rules as
/// written or as `iptables-save` printed them. Output: the canonical text.
fn canonical_rules(text: &str) -> String {
    assert!(text.len() <= usize::MAX / 2);
    String::new()
}

/// The capabilities the setup child raises into its ambient set before it starts the
/// `iptables` programs, which a non-zero uid would otherwise lose on exec. No input.
/// Output: exactly CAP_NET_ADMIN and CAP_NET_RAW.
fn ambient_wanted() -> CapabilitySet {
    CapabilitySet::empty()
}

/// The resolver file bound over `/etc/resolv.conf` inside the agent's network: one
/// nameserver, slirp's resolver, then the host's `search` and `options` lines and
/// `options use-vc`, since the agent looks names up over TCP. Input: the host's file, empty
/// when it cannot be read. Output: the file.
fn resolver_text(host: &str) -> String {
    assert!(host.len() <= usize::MAX / 2);
    String::new()
}

/// Tells whether an `iptables` program uses the nf_tables backend, from what `--version`
/// printed: the legacy backend takes a lock the setup child cannot hold. Input: the text.
/// Output: true only for nf_tables.
fn nf_tables(version: &str) -> bool {
    assert!(version.len() <= usize::MAX / 2);
    false
}

/// Writes stage 1's descriptor flags into the nest's bubblewrap arguments, right after
/// `--unshare-net`: the info and block pipes and the resolver file bound over
/// `/etc/resolv.conf`. Inputs: the nest's arguments and the three descriptors. Output: the
/// arguments with the flags, which [`placed`] accepts.
fn insert(args: &[OsString], fds: [RawFd; 3]) -> Vec<OsString> {
    assert!(fds.iter().all(|fd| *fd > 2));
    args.to_vec()
}

/// Checks bubblewrap's arguments as stage 1 starts it for a routed agent: [`NEST_ARGS`],
/// `--unshare-net`, then the descriptor flags in order, each once, and the resolver file
/// bound over `/etc/resolv.conf` alone. Input: the arguments. Output: Ok, or the reason.
fn placed(args: &[OsString]) -> Result<(), String> {
    assert!(args.len() >= NEST_ARGS.len());
    Err("the descriptor flags are refused".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_PORT: &str = concat!(
        "*filter\n",
        ":INPUT ACCEPT [0:0]\n",
        ":FORWARD DROP [0:0]\n",
        ":OUTPUT DROP [0:0]\n",
        "-A OUTPUT -o lo -j ACCEPT\n",
        "-A OUTPUT -d 10.0.2.2/32 -j DROP\n",
        "-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT\n",
        "-A OUTPUT -o tap0 -p tcp -m tcp --dport 443 -j ACCEPT\n",
        "COMMIT\n",
    );

    const V6: &str = concat!(
        "*filter\n",
        ":INPUT ACCEPT [0:0]\n",
        ":FORWARD DROP [0:0]\n",
        ":OUTPUT DROP [0:0]\n",
        "-A OUTPUT -o lo -j ACCEPT\n",
        "COMMIT\n",
    );

    const SAVED: &str = concat!(
        "# Generated by iptables-save v1.8.10 (nf_tables) on Sun Nov 15 23:10:00 2026\n",
        "*filter\n",
        ":INPUT ACCEPT [4:240]\n",
        ":FORWARD DROP [0:0]\n",
        ":OUTPUT DROP [12:720]\n",
        "-A OUTPUT -o lo -j ACCEPT\n",
        "-A OUTPUT -d 10.0.2.2/32 -j DROP\n",
        "-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT\n",
        "-A OUTPUT -o tap0 -p tcp -m tcp --dport 443 -j ACCEPT\n",
        "COMMIT\n",
        "# Completed on Sun Nov 15 23:10:00 2026\n",
    );

    fn words(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    fn nest_args(after: &[&str]) -> Vec<OsString> {
        let mut args = words(&NEST_ARGS);
        args.extend(words(after));
        args
    }

    #[test]
    fn the_rules_let_an_agent_out_only_through_its_ports_and_tcp_53_to_the_sandbox_resolver() {
        assert_eq!(egress_rules(&[443]), ONE_PORT);
        let second = "-A OUTPUT -o tap0 -p tcp -m tcp --dport 8443 -j ACCEPT\nCOMMIT\n";
        assert_eq!(
            egress_rules(&[443, 8443]),
            ONE_PORT.replace("COMMIT\n", second)
        );
        assert_eq!(egress_rules_v6(), V6);
        for rules in [egress_rules(&[443]), egress_rules(&[443, 8443]), egress_rules_v6()] {
            assert!(rules.ends_with("COMMIT\n"), "{rules}");
            assert!(rules.contains("\n:OUTPUT DROP [0:0]\n"), "{rules}");
            assert!(!rules.contains("udp"), "{rules}");
        }
        let dns: Vec<String> = egress_rules(&[443, 8443])
            .lines()
            .filter(|line| line.contains("--dport 53 "))
            .map(str::to_string)
            .collect();
        assert_eq!(
            dns,
            ["-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT"]
        );
    }

    #[test]
    fn the_read_back_drops_comments_and_counters_and_keeps_every_policy_and_rule() {
        let canonical = concat!(
            "*filter\n",
            ":INPUT ACCEPT\n",
            ":FORWARD DROP\n",
            ":OUTPUT DROP\n",
            "-A OUTPUT -o lo -j ACCEPT\n",
            "-A OUTPUT -d 10.0.2.2/32 -j DROP\n",
            "-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT\n",
            "-A OUTPUT -o tap0 -p tcp -m tcp --dport 443 -j ACCEPT\n",
            "COMMIT\n",
        );
        assert_eq!(canonical_rules(ONE_PORT), canonical);
        assert_eq!(canonical_rules(SAVED), canonical);
        let widened = SAVED.replace(":OUTPUT DROP [12:720]", ":OUTPUT ACCEPT [12:720]");
        let added = SAVED.replace("COMMIT\n", "-A OUTPUT -o tap0 -p udp -j ACCEPT\nCOMMIT\n");
        let dropped = SAVED.replace("-A OUTPUT -d 10.0.2.2/32 -j DROP\n", "");
        for changed in [widened, added, dropped] {
            assert_ne!(canonical_rules(&changed), canonical, "{changed}");
        }
        assert_eq!(canonical_rules(V6), canonical_rules(&format!("# saved\n{V6}")));
    }

    #[test]
    fn the_setup_child_raises_exactly_net_admin_and_net_raw_into_its_ambient_set() {
        assert_eq!(
            ambient_wanted(),
            CapabilitySet::NET_ADMIN | CapabilitySet::NET_RAW
        );
        assert_eq!(ambient_wanted().bits(), 0x3000);
        assert_eq!(ambient_wanted().iter().count(), 2);
    }

    #[test]
    fn the_resolver_file_names_only_the_sandbox_resolver_and_keeps_search_and_options() {
        let resolved = concat!(
            "# This is /run/systemd/resolve/stub-resolv.conf managed by man:systemd-resolved(8).\n",
            "nameserver 127.0.0.53\n",
            "options edns0 trust-ad\n",
            "search lan\n",
        );
        assert_eq!(
            resolver_text(resolved),
            "nameserver 10.0.2.3\noptions edns0 trust-ad\nsearch lan\noptions use-vc\n"
        );
        let several = "nameserver 192.168.1.1\nnameserver 8.8.8.8\n; old\nnameserver ::1\n";
        assert_eq!(
            resolver_text(several),
            "nameserver 10.0.2.3\noptions use-vc\n"
        );
        assert_eq!(resolver_text(""), "nameserver 10.0.2.3\noptions use-vc\n");
        let spaced = "search\t a.example   b.example\nsortlist 10.0.0.0/8\ndomain lan\n";
        assert_eq!(
            resolver_text(spaced),
            "nameserver 10.0.2.3\nsearch a.example b.example\noptions use-vc\n"
        );
        let long = format!("search {}\n", "a".repeat(400));
        assert_eq!(resolver_text(&long), "nameserver 10.0.2.3\noptions use-vc\n");
        let many = "options ndots:1\n".repeat(64);
        let kept = resolver_text(&many);
        assert_eq!(kept.lines().filter(|line| line.starts_with("nameserver")).count(), 1);
        assert!(kept.lines().count() <= 10, "{kept}");
    }

    #[test]
    fn only_an_nf_tables_iptables_is_run() {
        assert!(nf_tables("iptables-restore v1.8.10 (nf_tables)\n"));
        assert!(nf_tables("ip6tables-restore v1.8.9 (nf_tables)"));
        for refused in ["iptables-restore v1.8.7 (legacy)\n", "", "nf_tables", "v1.8.10\n"] {
            assert!(!nf_tables(refused), "{refused:?}");
        }
    }

    #[test]
    fn stage_one_writes_its_descriptor_flags_right_after_unshare_net() {
        let args = nest_args(&["--unshare-net", "--chdir", "/p", "--", "/h", "--probe"]);
        let written = insert(&args, [5, 6, 7]);
        let expected = nest_args(&[
            "--unshare-net",
            INFO_FD,
            "5",
            BLOCK_FD,
            "6",
            RO_BIND_DATA,
            "7",
            RESOLV_CONF,
            "--chdir",
            "/p",
            "--",
            "/h",
            "--probe",
        ]);
        assert_eq!(written, expected);
        assert_eq!(placed(&written), Ok(()));
    }

    #[test]
    fn bubblewrap_starts_only_with_the_descriptor_flags_in_order_and_the_resolver_file() {
        let tail = ["--chdir", "/p", "--", "/h", "--probe"];
        let with = |flags: &[&str]| {
            let mut after = vec!["--unshare-net"];
            after.extend_from_slice(flags);
            after.extend_from_slice(&tail);
            nest_args(&after)
        };
        let right = [INFO_FD, "5", BLOCK_FD, "6", RO_BIND_DATA, "7", RESOLV_CONF];
        assert_eq!(placed(&with(&right)), Ok(()));
        let swapped = [BLOCK_FD, "6", INFO_FD, "5", RO_BIND_DATA, "7", RESOLV_CONF];
        let elsewhere = [INFO_FD, "5", BLOCK_FD, "6", RO_BIND_DATA, "7", "/etc/hosts"];
        let linked = [INFO_FD, "5", BLOCK_FD, "6", RO_BIND_DATA, "7", "/run/resolv.conf"];
        let standard = [INFO_FD, "2", BLOCK_FD, "6", RO_BIND_DATA, "7", RESOLV_CONF];
        let shared = [INFO_FD, "5", BLOCK_FD, "5", RO_BIND_DATA, "7", RESOLV_CONF];
        let short = [INFO_FD, "5", BLOCK_FD, "6"];
        for refused in [&swapped[..], &elsewhere, &linked, &standard, &shared, &short] {
            assert!(placed(&with(refused)).is_err(), "{refused:?}");
        }
        let mut twice = with(&right);
        let at = twice.len() - tail.len();
        twice.splice(at..at, words(&[BLOCK_FD, "8"]));
        assert!(placed(&twice).is_err());
        let mut unshared = nest_args(&right);
        unshared.extend(words(&tail));
        assert!(placed(&unshared).is_err(), "no --unshare-net");
    }

    #[test]
    fn route_flags_lead_the_arguments_with_ports_each_once() {
        let rest = words(&["--bridge", "18080", "--nest", "--"]);
        let mut args = words(&[ROUTE, "443", ROUTE, "8443"]);
        args.extend(rest.clone());
        assert_eq!(leading(args), Ok((vec![443, 8443], rest.clone())));
        assert_eq!(leading(rest.clone()), Ok((Vec::new(), rest.clone())));
        let mut most = Vec::new();
        for port in 1..=REMOTES_MAX {
            most.extend(words(&[ROUTE, port.to_string().as_str()]));
        }
        assert_eq!(
            leading(most.clone()).map(|(ports, _)| ports.len()),
            Ok(REMOTES_MAX)
        );
        most.extend(words(&[ROUTE, "9999"]));
        assert!(leading(most).is_err(), "more than REMOTES_MAX");
        for refused in [
            &[ROUTE][..],
            &[ROUTE, "0"],
            &[ROUTE, "65536"],
            &[ROUTE, "port"],
            &[ROUTE, "443", ROUTE, "443"],
        ] {
            assert!(leading(words(refused)).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn the_setup_child_takes_its_parent_and_the_route_ports_alone() {
        assert_eq!(
            request(words(&[EGRESS, "1234", "443", "8443"])),
            Ok(HelperRequest::Egress {
                parent: 1234,
                ports: vec![443, 8443],
            })
        );
        let nine: Vec<String> = (1..=9).map(|port| port.to_string()).collect();
        let mut too_many = vec![EGRESS, "1234"];
        too_many.extend(nine.iter().map(String::as_str));
        for refused in [
            &[EGRESS][..],
            &[EGRESS, "1234"],
            &[EGRESS, "1", "443"],
            &[EGRESS, "pid", "443"],
            &[EGRESS, "1234", "0"],
            &[EGRESS, "1234", "443", "443"],
            &[EGRESS, "1234", "--route", "443"],
            too_many.as_slice(),
        ] {
            assert!(request(words(refused)).is_err(), "{refused:?}");
        }
    }
}
