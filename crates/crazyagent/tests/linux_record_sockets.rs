#![cfg(target_os = "linux")]
use kernel_sandbox::linux::launch::{LinuxBackend, LinuxSandbox};
use kernel_sandbox::net::{SandboxNetwork, SandboxRemote};
use kernel_sandbox::profile::{ProjectSandbox, command::Sandbox};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HELPER: &str = env!("CARGO_BIN_EXE_crazyagent-landlock");

const BIND: &str = "import socket, sys
try:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
except OSError as e:
    print('socket=%d' % e.errno)
    sys.exit(0)
try:
    s.bind(sys.argv[1])
    print('bound')
except OSError as e:
    print('bind=%d' % e.errno)
";

/// The Landlock ABI this kernel serves, as `landlock_create_ruleset` with the version flag
/// answers it: 0 or less when Landlock is off or missing. No input. Output: the version.
fn landlock_abi() -> i64 {
    unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            1u32,
        )
    }
}

/// Runs the bind script on `socket`, confined in `sandbox` or, with none, unsandboxed.
/// Inputs: the sandbox, if any, the socket path and the working folder. Output: the one
/// line the script printed.
fn bind(sandbox: Option<&LinuxSandbox>, socket: &Path, folder: &Path) -> String {
    let mut harness = Command::new("/usr/bin/python3");
    harness
        .args(["-I", "-c", BIND])
        .arg(socket)
        .current_dir(folder);
    let mut command = match sandbox {
        Some(sandbox) => sandbox.wrap(&harness).unwrap(),
        None => harness,
    };
    let output = kernel_processes::spawn_gate::output(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A Linux agent under the production helper makes no Unix socket in any engine record
/// folder, on an open network and on a limited one: the socket filter refuses the socket
/// with EACCES and no node is left, while the same bind run unsandboxed succeeds there, so
/// the row sees a bind when one happens. The kernel's Landlock ABI is printed, and one
/// below the helper's file rules fails the row.
#[test]
fn a_linux_agent_makes_no_unix_socket_in_any_engine_record_folder() {
    let abi = landlock_abi();
    println!("landlock abi {abi}");
    assert!(
        abi >= 3,
        "this kernel serves Landlock ABI {abi}, the helper needs 3"
    );
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().canonicalize().unwrap();
    let [project, home, tmp, runtime]: [PathBuf; 4] =
        ["project", "home", "tmp", "runtime"].map(|name| root.join(name));
    for folder in [&project, &home, &tmp, &runtime] {
        fs::create_dir(folder).unwrap();
    }
    fs::write(runtime.join("engine.db"), "engine secret\n").unwrap();
    let kinds = crazyagent::sandbox::PROFILE_HOOKS.plain_record_folders;
    assert_eq!(kinds.len(), 5);
    for kind in kinds {
        fs::create_dir(runtime.join(kind)).unwrap();
    }
    let endpoint = kernel_processes::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = endpoint.local_addr().unwrap().port();
    let networks = [
        SandboxNetwork::Open,
        SandboxNetwork::Only(vec![SandboxRemote::Loopback(port)]),
    ];
    for network in networks {
        let policy = ProjectSandbox::new(
            &crazyagent::sandbox::PROFILE_HOOKS,
            &project,
            &home,
            &tmp,
            network.clone(),
        )
        .unwrap()
        .guard_engine(&runtime)
        .unwrap();
        let sandbox = LinuxSandbox::detect_among(policy, Path::new(HELPER), &[]).unwrap();
        assert!(
            matches!(sandbox.backend(), LinuxBackend::Landlock { .. }),
            "Linux test machines need Landlock: {:?}",
            sandbox.backend()
        );
        for kind in kinds {
            let socket = runtime.join(kind).join("s.sock");
            assert!(socket.as_os_str().len() < 108, "{}", socket.display());
            let inside = bind(Some(&sandbox), &socket, &project);
            println!("{network:?} {kind} confined {inside}");
            assert_eq!(inside, "socket=13", "{network:?} {kind}");
            assert!(fs::symlink_metadata(&socket).is_err(), "{network:?} {kind}");
            let outside = bind(None, &socket, &project);
            assert_eq!(outside, "bound", "{network:?} {kind}");
            fs::remove_file(&socket).unwrap();
        }
    }
}
