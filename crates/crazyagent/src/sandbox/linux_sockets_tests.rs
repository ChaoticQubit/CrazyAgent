use super::*;
use kernel_sandbox::elf::test_support::executable;
use kernel_sandbox::error::SandboxError;
use kernel_sandbox::host::LinuxHost;
use kernel_sandbox::lines::*;
#[cfg(target_os = "linux")]
use kernel_sandbox::linux::launch as linux;
use kernel_sandbox::profile::ProjectSandbox;
use kernel_sandbox::profile::consts::*;
use kernel_sandbox::profile::linux_unbridged_notice_on;
use std::path::PathBuf;

struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for folder in ["project", "home", "tmp"] {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    fs::write(
        root.join("glibc"),
        executable(true, false, Some("/lib64/ld-linux-x86-64.so.2")),
    )
    .unwrap();
    fs::write(
        root.join("musl"),
        executable(true, false, Some("/lib/ld-musl-x86_64.so.1")),
    )
    .unwrap();
    fs::write(root.join("static"), executable(true, false, None)).unwrap();
    for (script, interpreter) in [
        ("on_glibc", root.join("glibc")),
        ("on_musl", root.join("musl")),
        ("on_script", root.join("on_glibc")),
        ("on_relative", PathBuf::from("node")),
    ] {
        fs::write(root.join(script), format!("#!{}\n", interpreter.display())).unwrap();
    }
    Case { _temp: temp, root }
}

fn sandbox(case: &Case, network: SandboxNetwork, sockets: LinuxSockets) -> ProjectSandbox {
    ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &case.root.join("project"),
        &case.root.join("home"),
        &case.root.join("tmp"),
        network,
    )
    .unwrap()
    .limit_sockets(sockets)
}

fn model() -> SandboxNetwork {
    SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)])
}

fn local() -> SandboxNetwork {
    SandboxNetwork::Only(vec![SandboxRemote::Loopback(8080)])
}

#[test]
fn the_setting_defaults_to_tcp_only_and_keeps_its_wire_names() {
    let case = case();
    let fresh = ProjectSandbox::new(
        &crate::sandbox::PROFILE_HOOKS,
        &case.root.join("project"),
        &case.root.join("home"),
        &case.root.join("tmp"),
        model(),
    )
    .unwrap();
    assert_eq!(fresh.linux_sockets(), LinuxSockets::TcpOnly);
    for (sockets, name) in [
        (LinuxSockets::TcpOnly, "\"tcp_only\""),
        (LinuxSockets::Refuse, "\"refuse\""),
        (LinuxSockets::TcpPorts, "\"tcp_ports\""),
    ] {
        assert_eq!(serde_json::to_string(&sockets).unwrap(), name);
        assert_eq!(serde_json::from_str::<LinuxSockets>(name).unwrap(), sockets);
    }
    assert!(serde_json::from_str::<LinuxSockets>("\"udp\"").is_err());
}

#[test]
fn tcp_only_starts_a_glibc_harness_and_refuses_one_that_cannot_ask_over_tcp() {
    let case = case();
    let named = sandbox(&case, model(), LinuxSockets::TcpOnly);
    assert!(named.resolves_over_tcp());
    assert_eq!(named.linux_refusal(&case.root.join("glibc")), None);
    for program in ["musl", "static", "on_musl", "absent"] {
        assert_eq!(
            named.linux_refusal(&case.root.join(program)),
            Some(LINUX_RESOLVER_NEEDS_UDP),
            "{program}"
        );
    }
    for network in [
        local(),
        SandboxNetwork::Only(Vec::new()),
        SandboxNetwork::Open,
    ] {
        let quiet = sandbox(&case, network, LinuxSockets::TcpOnly);
        assert!(!quiet.resolves_over_tcp());
        assert_eq!(quiet.linux_refusal(&case.root.join("musl")), None);
    }
}

#[test]
fn tcp_only_judges_a_script_by_its_interpreter_and_names_a_deeper_chain_plainly() {
    let case = case();
    let named = sandbox(&case, model(), LinuxSockets::TcpOnly);
    assert_eq!(named.linux_refusal(&case.root.join("on_glibc")), None);
    assert_eq!(
        named.linux_refusal(&case.root.join("on_musl")),
        Some(LINUX_RESOLVER_NEEDS_UDP)
    );
    for program in ["on_script", "on_relative"] {
        assert_eq!(
            named.linux_refusal(&case.root.join(program)),
            Some(LINUX_HARNESS_UNCHECKED),
            "{program}"
        );
    }
    let quiet = sandbox(&case, local(), LinuxSockets::TcpOnly);
    assert_eq!(quiet.linux_refusal(&case.root.join("on_script")), None);
    let unchecked = crate::config::plain_problem(LINUX_HARNESS_UNCHECKED).unwrap();
    for part in [
        "cannot check",
        "'TCP only'",
        "does not start",
        "'TCP ports only'",
        "reach any host",
        "Settings",
    ] {
        assert!(unchecked.contains(part), "{unchecked}");
    }
}

#[test]
fn refuse_starts_no_limited_network_agent() {
    let case = case();
    for network in [model(), local(), SandboxNetwork::Only(Vec::new())] {
        let refused = sandbox(&case, network, LinuxSockets::Refuse);
        assert!(!refused.resolves_over_tcp());
        assert_eq!(
            refused.linux_refusal(&case.root.join("glibc")),
            Some(LINUX_LIMITED_NETWORK_OFF)
        );
    }
    let open = sandbox(&case, SandboxNetwork::Open, LinuxSockets::Refuse);
    assert_eq!(open.linux_refusal(&case.root.join("glibc")), None);
}

#[test]
fn only_a_nested_supervised_launch_with_its_network_held_is_full() {
    let case = case();
    let tcp = |network| sandbox(&case, network, LinuxSockets::TcpOnly);
    for held in [
        SandboxNetwork::Open,
        SandboxNetwork::Only(Vec::new()),
        local(),
    ] {
        let held = tcp(held);
        assert!(linux_holds_fully(true, true, false, &held));
        assert!(
            !linux_holds_fully(false, true, false, &held),
            "no process space"
        );
        assert!(
            !linux_holds_fully(true, false, false, &held),
            "no supervisor"
        );
    }
    let hosted = tcp(model());
    assert!(linux_holds_fully(true, true, true, &hosted), "routed");
    assert!(
        !linux_holds_fully(true, true, false, &hosted),
        "a port on any host from this computer's network"
    );
    let low = tcp(SandboxNetwork::Only(vec![
        SandboxRemote::AnyHost(443),
        SandboxRemote::Loopback(80),
    ]));
    assert!(low.routed().is_none());
    assert!(
        !linux_holds_fully(true, true, true, &low),
        "a loopback port below the bridge keeps it on this computer's network"
    );
    for sockets in [LinuxSockets::TcpPorts, LinuxSockets::Refuse] {
        let other = sandbox(&case, local(), sockets);
        assert!(!linux_holds_fully(true, true, true, &other), "{sockets:?}");
    }
    let open = sandbox(&case, SandboxNetwork::Open, LinuxSockets::TcpPorts);
    assert!(
        linux_holds_fully(true, true, false, &open),
        "an open network has no limit to hold"
    );
    let below = tcp(SandboxNetwork::Only(vec![SandboxRemote::Loopback(80)]));
    assert!(
        !linux_holds_fully(true, true, false, &below),
        "a loopback port below the bridge is not bridged"
    );
}

#[test]
fn a_full_launch_is_one_no_network_line_names_on_a_host_that_holds_everything() {
    let case = case();
    let host = LinuxHost {
        nested: true,
        scoped: true,
        supervised: true,
        root: false,
        armored: false,
        routed: true,
    };
    let networks = [
        SandboxNetwork::Open,
        SandboxNetwork::Only(Vec::new()),
        local(),
        model(),
        SandboxNetwork::Only(vec![SandboxRemote::Loopback(80)]),
        SandboxNetwork::Only(vec![
            SandboxRemote::AnyHost(443),
            SandboxRemote::Loopback(80),
        ]),
        SandboxNetwork::Only(vec![
            SandboxRemote::AnyHost(443),
            SandboxRemote::Loopback(8080),
        ]),
    ];
    for network in networks {
        let launched = sandbox(&case, network, LinuxSockets::TcpOnly);
        let routed = launched.routed().is_some();
        let full = linux_holds_fully(true, true, routed, &launched);
        for choice in [
            kernel_sandbox::policy::AgentNetwork::None,
            kernel_sandbox::policy::AgentNetwork::Https,
            kernel_sandbox::policy::AgentNetwork::Open,
        ] {
            let line = linux_unbridged_notice_on(Some(host), choice, &launched);
            assert_eq!(
                full,
                line.is_none(),
                "{:?} under {choice:?}: {line:?}",
                launched.network()
            );
        }
    }
}

#[test]
fn the_windows_loopback_refusal_has_its_own_code_and_reads_plainly() {
    assert!(model_loopback_windows(&SandboxError::Unavailable(
        MODEL_LOOPBACK_WINDOWS_LINE.into()
    )));
    assert!(!model_loopback_windows(&SandboxError::Unavailable(
        "other".into()
    )));
    assert!(!model_loopback_windows(&SandboxError::Invalid(
        MODEL_LOOPBACK_WINDOWS_LINE.into()
    )));
    for code in [
        MODEL_LOOPBACK_UNAVAILABLE_WINDOWS,
        kernel_sandbox::policy::SANDBOX_FULL_UNAVAILABLE_WINDOWS,
    ] {
        assert!(
            crate::config::plain_problem(code).is_some(),
            "{code} has a plain sentence"
        );
    }
}

#[test]
fn the_strict_refusal_has_its_own_code_and_reads_plainly() {
    assert_eq!(
        refusal_code(&SandboxError::Unavailable(LINUX_NOT_FULL_LINE.into())),
        Some(LINUX_NOT_FULL)
    );
    assert_eq!(
        refusal_code(&SandboxError::Unavailable("other".into())),
        None
    );
    assert_eq!(
        refusal_code(&SandboxError::Invalid(LINUX_NOT_FULL_LINE.into())),
        None
    );
    let line = crate::config::plain_problem(LINUX_NOT_FULL);
    assert!(line.is_some(), "the strict refusal has a plain sentence");
    let line = line.unwrap();
    for part in [
        "Refuse agents without the full Linux sandbox",
        "Agent access",
        "Settings",
    ] {
        assert!(line.contains(part), "{line}");
    }
}

/// rv-code4 F2 (#622): each Linux refusal of hidden folders, where bubblewrap cannot
/// start agents in namespaces of their own, where a second mount shows the folder and
/// where the helper cannot cut the agent's mounts off (V24x-LINUX-NEST-MOUNT-PROPAGATION),
/// has its own code, so the owner reads what to change instead of the generic line.
#[cfg(target_os = "linux")]
#[test]
fn the_hidden_folder_refusals_have_their_own_codes_and_read_plainly() {
    for (reason, code, parts) in [
        (
            linux::HIDDEN_NEEDS_NAMESPACES,
            LINUX_HIDDEN_NEEDS_NAMESPACES,
            ["bubblewrap", "namespaces", "Hidden folders"],
        ),
        (
            linux::HIDDEN_MOUNTED_AGAIN,
            LINUX_HIDDEN_MOUNTED_AGAIN,
            [
                "mounted a second time",
                "remove that mount",
                "Hidden folders",
            ],
        ),
        (
            kernel_sandbox::linux::helper::HIDDEN_MOUNTS_NOT_PRIVATE,
            LINUX_HIDDEN_MOUNTS_NOT_PRIVATE,
            ["user namespaces", "later mount", "Hidden folders"],
        ),
    ] {
        let refused = SandboxError::Unavailable(reason.into());
        assert_eq!(refusal_code(&refused), Some(code));
        let line = crate::config::plain_problem(code);
        assert!(line.is_some(), "{code} has a plain sentence");
        let line = line.unwrap();
        for part in parts {
            assert!(line.contains(part), "{line}");
        }
    }
}

#[test]
fn each_refusal_reads_plainly_and_names_the_setting() {
    let udp = crate::config::plain_problem(LINUX_RESOLVER_NEEDS_UDP).unwrap();
    for part in [
        "cannot look up names over TCP",
        "'TCP only'",
        "'TCP ports only'",
        "Settings",
        "UDP and other sockets reach any host",
    ] {
        assert!(udp.contains(part), "{udp}");
    }
    let off = crate::config::plain_problem(LINUX_LIMITED_NETWORK_OFF).unwrap();
    for part in [
        "Linux limited-network sockets",
        "does not start",
        "Settings",
    ] {
        assert!(off.contains(part), "{off}");
    }
}

#[test]
fn tcp_ports_starts_any_harness_and_moves_no_lookup_to_tcp() {
    let case = case();
    let ports = sandbox(&case, model(), LinuxSockets::TcpPorts);
    assert!(!ports.resolves_over_tcp());
    for program in [
        "glibc",
        "musl",
        "static",
        "on_glibc",
        "on_musl",
        "on_script",
        "on_relative",
    ] {
        assert_eq!(
            ports.linux_refusal(&case.root.join(program)),
            None,
            "{program}"
        );
    }
}
