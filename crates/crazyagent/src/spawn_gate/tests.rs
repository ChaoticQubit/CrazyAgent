use super::{guarded, kept, output, spawn, status};
use std::process::{Command, Stdio};

/// A command that exits 0 and prints nothing: `true` here.
#[cfg(unix)]
fn succeeding() -> Command {
    Command::new("/usr/bin/true")
}

/// A command that exits 0 and prints nothing: the system `cmd.exe` running `exit 0`.
#[cfg(windows)]
fn succeeding() -> Command {
    let mut command = Command::new(system_shell());
    command.args(["/d", "/c", "exit", "0"]);
    command
}

/// A command that exits 0 after printing `out` to stdout and `err` to stderr, with no newline
/// on stdout: `sh` here.
#[cfg(unix)]
fn printing() -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "printf out; printf err >&2"]);
    command
}

/// A command that exits 0 after printing `out` to stdout, with no newline, and `err` to
/// stderr: the system `cmd.exe`, whose `set /p` prompt with no input prints without one.
#[cfg(windows)]
fn printing() -> Command {
    let mut command = Command::new(system_shell());
    command.args(["/d", "/c", "<nul set /p =out&echo err 1>&2&exit 0"]);
    command
}

/// The `cmd.exe` Windows ships, by its full path. Output: the path.
#[cfg(windows)]
fn system_shell() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe")
}

#[test]
#[should_panic(expected = "spawn_gate entered again on a thread that already holds it")]
fn entering_the_gate_inside_the_gate_panics_instead_of_deadlocking() {
    let _ = guarded(|| spawn(&mut Command::new("/usr/bin/true")));
}

/// V243 P6: a kept start asked for while this thread holds the gate panics before its keeper
/// thread starts, as that thread would wait on the gate forever for a start that forks. The
/// start here forks nothing, so without the guard the call would return and the row fails.
#[test]
#[should_panic(expected = "a kept start waits on a thread that already holds the spawn gate")]
fn a_kept_start_inside_the_gate_panics_instead_of_waiting_for_its_keeper() {
    let _ = guarded(|| kept(|| Ok(())).map(|(started, _keeper)| started));
}

#[test]
fn the_gate_is_usable_again_after_a_reentry_panic() {
    let reentry = std::thread::spawn(|| guarded(|| status(&mut succeeding())));
    assert!(reentry.join().is_err());
    assert!(status(&mut succeeding()).unwrap().success());
}

#[test]
fn output_keeps_the_stdio_the_caller_set() {
    let captured = output(
        printing()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(captured.status.success());
    assert_eq!(captured.stdout, b"out");
    assert!(captured.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn accept_failures_a_later_accept_outlives_are_retried_and_lasting_ones_end_the_server() {
    use rustix::io::Errno;
    for passing in [
        Errno::CONNABORTED,
        Errno::CONNRESET,
        Errno::INTR,
        Errno::MFILE,
        Errno::NFILE,
        Errno::NOBUFS,
        Errno::NOMEM,
        Errno::PROTO,
    ] {
        assert!(
            super::passing(&std::io::Error::from(passing)),
            "{passing:?} is retried"
        );
    }
    for lasting in [Errno::BADF, Errno::INVAL, Errno::NOTSOCK, Errno::OPNOTSUPP] {
        assert!(
            !super::passing(&std::io::Error::from(lasting)),
            "{lasting:?} ends the server"
        );
    }
}

/// An exclusive listener binds on either loopback address, holds its port against a second
/// exclusive bind, which fails with `AddrInUse`, and accepts a connection there (S4B-2W1,
/// G1). On Windows its own row in the listener table never counts against it (W10b).
#[test]
fn an_exclusive_listener_binds_each_loopback_keeps_its_port_and_accepts() {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    for loopback in [
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
    ] {
        let listener = super::bind_tcp_std_exclusive(loopback).unwrap();
        let bound = listener.local_addr().unwrap();
        assert_eq!(bound.ip(), loopback.ip());
        assert_ne!(bound.port(), 0);
        let again = super::bind_tcp_std_exclusive(bound).unwrap_err();
        assert_eq!(again.kind(), std::io::ErrorKind::AddrInUse, "{bound}");
        let _client = super::connect_tcp_std(bound).unwrap();
        let (_, peer) = super::accept_tcp_std(&listener).unwrap();
        assert_eq!(peer.ip(), loopback.ip());
    }
}

/// W10b (row 230 on Windows): an exclusive bind on a loopback address fails with `AddrInUse`
/// on a port this user's wildcard listener holds, in its own family or, for 127.0.0.1, the
/// IPv6 wildcard, which may take both families, so it never sits beside that listener and
/// takes its loopback connections. macOS skips the last case, where its own bind rules decide.
/// Windows reads the listener table after its bind, so a wildcard listener bound after that
/// read still sits beside it there; unix refuses that later bind itself.
#[test]
fn an_exclusive_bind_fails_beside_a_wildcard_listener_of_this_user() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
    let mut cases = vec![("0.0.0.0:0", v4), ("[::]:0", v6)];
    if !cfg!(target_os = "macos") {
        cases.push(("[::]:0", v4));
    }
    for (holder, loopback) in cases {
        let held = super::bind_tcp_std(holder).unwrap();
        let port = held.local_addr().unwrap().port();
        let refused = super::bind_tcp_std_exclusive(SocketAddr::new(loopback, port)).unwrap_err();
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::AddrInUse,
            "{holder} {loopback}"
        );
    }
}

/// A refusing port turns a connect away on every platform. On unix it also stays taken while
/// its two ends live: no port-0 bind of this process gets it and a bind that names it without
/// `SO_REUSEADDR` fails, so a row naming it as a stopped provider never meets a listener there.
#[test]
fn a_refusing_port_turns_connects_away_and_stays_taken_while_held() {
    use std::net::{Ipv4Addr, SocketAddr};
    let (held, port) = super::refusing_tcp_port().unwrap();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let refused = super::connect_tcp_std(address).unwrap_err();
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::ConnectionRefused,
        "{refused}"
    );
    #[cfg(unix)]
    {
        let bound: Vec<_> = (0..64)
            .map(|_| super::bind_tcp_std("127.0.0.1:0").unwrap())
            .collect();
        assert!(
            bound
                .iter()
                .all(|listener| listener.local_addr().unwrap().port() != port),
            "a port-0 bind got the held port {port}"
        );
        let named = super::bind_tcp_std_exclusive(address).unwrap_err();
        assert_eq!(named.kind(), std::io::ErrorKind::AddrInUse, "{named}");
    }
    drop(held);
}

/// A refusing port stands for a stopped model provider in a row's settings, so it lies where
/// an owner's model port would: below every range the system hands out by itself, which on
/// Linux a kept model port must stay out of or no Landlock agent starts (G1 N8 P4), and apart
/// from the owner's and the model proxy's ports.
#[test]
fn a_refusing_port_lies_below_the_ports_the_system_hands_out() {
    use crate::sandbox::OWNER_PORTS;
    use crate::sandbox::model_proxy::{PICKED_PORTS_START, PROXY_PORTS};
    let held: Vec<_> = (0..4)
        .map(|_| super::refusing_tcp_port().unwrap())
        .collect();
    for (_, port) in &held {
        assert!(*port > 1024 && *port < PICKED_PORTS_START, "{port}");
        assert!(
            !PROXY_PORTS.contains(port) && !OWNER_PORTS.contains(port),
            "{port}"
        );
    }
    assert_eq!(held.len(), 4);
}

/// A low free port lies below every range the system hands out by itself, below the ports
/// Linux node services use, and apart from the owner's and the model proxy's ports. A
/// listener of its own binds it, and the next call in this process names another port.
#[test]
fn a_low_free_port_is_outside_the_system_and_proxy_ranges_and_each_call_moves_on() {
    use crate::sandbox::OWNER_PORTS;
    use crate::sandbox::model_proxy::{PICKED_PORTS_START, PROXY_PORTS};
    use std::net::{Ipv4Addr, SocketAddr};
    let first = super::free_low_tcp_port().unwrap();
    let second = super::free_low_tcp_port().unwrap();
    assert_ne!(first, second);
    for port in [first, second] {
        assert!(
            port > 1024 && port < 30_000 && port < PICKED_PORTS_START,
            "{port}"
        );
        assert!(
            !PROXY_PORTS.contains(&port) && !OWNER_PORTS.contains(&port),
            "{port}"
        );
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = super::bind_tcp_std_exclusive(address).unwrap();
        assert_eq!(listener.local_addr().unwrap(), address);
    }
}

/// 2.4.3 W10: a stream accepted from a non-blocking listener, as the model proxy's are, is
/// blocking on every system, so a read with a timeout waits that long for the client
/// instead of failing at once (Winsock hands the listener's mode to the accepted socket).
/// Input: none. Output: passes or panics.
#[test]
fn a_stream_accepted_from_a_non_blocking_listener_waits_out_its_read_timeout() {
    use std::io::Read;
    use std::time::{Duration, Instant};
    let listener = super::bind_tcp_std("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let _client = super::connect_tcp_std(listener.local_addr().unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let (mut stream, _) = loop {
        match super::accept_tcp_std(&listener) {
            Ok(accepted) => break accepted,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept: {error}"),
        }
    };
    let timeout = Duration::from_millis(300);
    stream.set_read_timeout(Some(timeout)).unwrap();
    let started = Instant::now();
    let read = stream.read(&mut [0u8; 1]);
    let waited = started.elapsed();
    assert!(read.is_err(), "the silent client sent nothing: {read:?}");
    assert!(
        waited >= timeout / 2,
        "the read failed after {waited:?}, not after its timeout"
    );
}

/// A blocking accept whose client comes 200 ms late waits for it in a few waits, not in a
/// retry loop that wakes every few milliseconds (V243-POLL-WAITS).
#[cfg(unix)]
#[test]
fn a_blocking_accept_waits_for_a_late_client_in_a_few_waits() {
    use std::time::Duration;
    let listener = super::bind_tcp_std("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let client = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        super::connect_tcp_std(address).unwrap()
    });
    let before = super::accept_waits();
    let accepted = super::accept_tcp_std(&listener);
    let waits = super::accept_waits() - before;
    let _client = client.join().unwrap();
    assert!(accepted.is_ok(), "{accepted:?}");
    assert!(waits <= 3, "{waits} waits for a client 200 ms late");
}

/// A folder planted at the root of the current drive, where any local user may create one,
/// removed when this drops. Input: the folder's full path.
#[cfg(windows)]
struct Planted(std::path::PathBuf);

#[cfg(windows)]
impl Drop for Planted {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Plants a copy of the system `cmd.exe` as `probe.exe` in a new folder at the root of the
/// current drive, as another local user could (V244-WIN-ROOT-RELATIVE-PROGRAM, probe P9).
/// Input: the folder's name, made unique with this process's id. Output: the folder's
/// keeper, its name and the planted file's full path.
#[cfg(windows)]
fn planted(name: &str) -> (Planted, String, std::path::PathBuf) {
    let name = format!("{name}-{}", std::process::id());
    let folder = std::path::absolute(format!("\\{name}")).unwrap();
    std::fs::create_dir(&folder).unwrap();
    let kept = Planted(folder.clone());
    let probe = folder.join("probe.exe");
    std::fs::copy(system_shell(), &probe).unwrap();
    (kept, name, probe)
}

/// Starts a program as `cmd.exe /d /c exit 7` through the gate with no stdio, so an exit of
/// 7 shows the program ran. Input: the program as the caller names it. Output: the exit code,
/// or the error that kept it from starting.
#[cfg(windows)]
fn exit_seven(program: &std::ffi::OsStr) -> std::io::Result<Option<i32>> {
    status(
        Command::new(program)
            .args(["/d", "/c", "exit", "7"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .map(|status| status.code())
}

/// Whether a start was refused because its program could resolve where another user plants
/// it. Input: the start's result. Output: the answer.
#[cfg(windows)]
fn refused_relative<T>(started: &std::io::Result<T>) -> bool {
    started.as_ref().is_err_and(|error| {
        error.kind() == std::io::ErrorKind::PermissionDenied
            && error.to_string() == "spawn_relative_program_refused"
    })
}

/// A Unix-style program path such as `/usr/bin/git` has a root but no drive on Windows, so it
/// resolves on the current drive, where another local user can plant the program. The gate
/// refuses it, with or without `.exe` and with either slash, while the same planted file
/// started by its full path runs.
#[cfg(windows)]
#[test]
fn a_root_relative_program_is_refused_where_another_user_could_plant_it_and_its_full_path_runs() {
    let (_kept, name, probe) = planted("crazyagent-p9");
    assert_eq!(
        exit_seven(probe.as_os_str()).unwrap(),
        Some(7),
        "the planted copy runs by its full path"
    );
    for program in [
        format!("/{name}/probe"),
        format!("/{name}/probe.exe"),
        format!("\\{name}\\probe.exe"),
    ] {
        let started = exit_seven(program.as_ref());
        assert!(
            refused_relative(&started),
            "{program} resolves on the current drive, so the gate refuses it: {started:?}"
        );
    }
}

/// A drive-relative or folder-relative program resolves against a current folder the engine
/// does not choose, and an empty one names nothing, so the gate refuses each. A bare file
/// name still starts, from the places Windows searches.
#[cfg(windows)]
#[test]
fn a_drive_relative_or_folder_relative_program_is_refused_and_a_bare_name_is_not() {
    for program in [
        "C:probe.exe",
        "sub\\probe.exe",
        "sub/probe.exe",
        ".\\probe.exe",
        "",
    ] {
        let started = exit_seven(program.as_ref());
        assert!(
            refused_relative(&started),
            "{program:?} is neither absolute nor a bare name: {started:?}"
        );
    }
    assert_eq!(
        exit_seven("cmd.exe".as_ref()).unwrap(),
        Some(7),
        "a bare name starts"
    );
}
