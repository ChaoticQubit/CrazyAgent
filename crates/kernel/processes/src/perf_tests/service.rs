use crate::perf_kit::{Row, SAMPLES, time};
use crate::service::{
    Hooks, Repair, Service, ServiceError, changeable_above_socket, changeable_by_others,
    changeable_past_sticky, failure, link_cli, persistable, retire_old_copies,
    retire_old_copies_with, service_name, service_path, transient, transient_refused,
    write_definition,
};
use kernel_common::Platform;
use std::ffi::OsStr;
use std::hint::black_box;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const FAST: usize = 1000;
const FILES: usize = 10;
const ONE: usize = 1;
const ONE_SHOT: usize = SAMPLES / 10 + SAMPLES;
const UID: u32 = 501;
const SHELL_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const STATUS_OUTPUT: &str =
    "gui/501/dev.crazyagent.engine = {\n\tstate = running\n\tpid = 4242\n}\n";
const TEXT: &str = "the engine service is not installed";
const ON_IMAGE: &str = "/Volumes/CrazyAgent/CrazyAgent.app/Contents/MacOS/crazyagent";
const CONTROL_TEXT: &str = "a control text of one hundred and twenty bytes that both twins copy and format the same way, so a row shows the binary!.";

/// Times each kind of call the held walk of a socket's path makes, alone, on the socket's
/// own folder: an open for search and its close, a read through the held folder with its
/// access list, a read of the socket by name through that folder, a volume read, and one
/// `lstat` of the socket for scale. Input: the socket. Output: none; prints one PERF line
/// per call kind.
fn held_steps(socket: &Path) {
    use std::os::fd::AsFd;
    let folder = socket.parent().expect("the socket sits in a folder");
    let name = std::ffi::CString::new(folder.as_os_str().as_encoded_bytes()).unwrap();
    let leaf = std::ffi::CString::new(socket.file_name().unwrap().as_encoded_bytes()).unwrap();
    let held = crazyagent_macos_fs::open_search(None, &name, false).unwrap();
    let read = crazyagent_macos_fs::held_entry(held.as_fd()).unwrap();
    assert!(read.folder() && !read.link());
    let device = read.device;
    time(
        &row("service.changeable.held.open_close", FILES),
        SAMPLES,
        || {
            drop(black_box(
                crazyagent_macos_fs::open_search(None, black_box(&name), false).unwrap(),
            ));
        },
    );
    time(
        &row("service.changeable.held.entry", FILES),
        SAMPLES,
        || {
            black_box(crazyagent_macos_fs::held_entry(held.as_fd()).unwrap());
        },
    );
    time(
        &row("service.changeable.held.entry_at", FILES),
        SAMPLES,
        || {
            black_box(crazyagent_macos_fs::held_entry_at(Some(held.as_fd()), &leaf).unwrap());
        },
    );
    time(
        &row("service.changeable.held.volume", FILES),
        SAMPLES,
        || {
            black_box(crazyagent_macos_fs::held_volume_keeps_lists(held.as_fd(), device).unwrap());
        },
    );
    time(&row("service.changeable.lstat", FILES), SAMPLES, || {
        black_box(std::fs::symlink_metadata(black_box(socket)).unwrap());
    });
}

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "processes",
        name,
        batch,
    }
}

fn no_version(_root: &Path) -> Option<String> {
    None
}

fn prepare_root(root: &Path) -> Result<(), ServiceError> {
    private_folder(root)?;
    private_folder(&root.join("logs"))
}

fn private_folder(path: &Path) -> Result<(), ServiceError> {
    std::fs::create_dir_all(path).map_err(|error| failure(error.to_string()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| failure(error.to_string()))
}

fn is_private_folder(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && metadata.permissions().mode() & 0o777 == 0o700)
}

/// The engine checks every row passes: no engine version known at any root, and private
/// folders made and read with mode 0700, as the engine's own checks do.
const HOOKS: Hooks = Hooks {
    reported_version: no_version,
    prepare_root,
    private_folder,
    is_private_folder,
};

fn executable(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_path_buf()
}

/// A login service that is never registered with launchd: a unique label, and an engine,
/// data root and definition file inside a private temporary home. Input: the home.
/// Output: the service.
fn stand_in(home: &Path, definition: &str) -> Service {
    Service {
        platform: Platform::MacOs,
        name: format!("dev.crazyagent.engine.perf-{}", std::process::id()),
        executable: executable(&home.join("app").join("crazyagent")),
        root: home.join("engine"),
        path: SHELL_PATH.into(),
        home: Some(home.to_path_buf()),
        definition: home.join("LaunchAgents").join(definition),
        user: None,
    }
}

/// One data root holding the copy an older version kept in `<root>/bin`, and the copy
/// `persistable` keeps in `<root>/setup/bin`. Input: the folder to make it in. Output: the
/// root and the kept engine.
fn old_copies(folder: &Path) -> (PathBuf, PathBuf) {
    let root = folder.to_path_buf();
    private_folder(&root.join("bin")).unwrap();
    executable(&root.join("bin").join("crazyagent"));
    private_folder(&root.join("setup").join("bin")).unwrap();
    let installed = executable(&root.join("setup").join("bin").join("crazyagent"));
    (root, installed)
}

/// Times every public call of the service module that does not register a job with
/// launchd, on a stand-in service in a temporary home whose label launchd never knew.
/// `install` and `start` get no row: each bootstraps a job into the owner's login session.
/// Two control rows copy and format a fixed text the same way in both twins, so a gap they
/// show is the test binary, not the service code; a NOTE line says whether the runner's own
/// `~/.local/bin/crazyagent`, which `retire_old_copies` reads, is there.
/// `link_cli_repoint` names the other of two executables each call, so every call swaps
/// the link. Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn service_calls() {
    let home = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(home.path()).unwrap();
    let service = stand_in(&home, "perf.plist");
    let absent = stand_in(&home, "absent.plist");
    let text = service.definition_text().unwrap();
    write_definition(&service.definition, &text, Platform::MacOs).unwrap();
    let other = executable(&home.join("other").join("crazyagent"));
    let bin = home.join("local-bin");
    std::fs::create_dir_all(home.join("socket-folder")).unwrap();
    let socket = home.join("socket-folder").join("engine.sock");
    let _listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
    let roots: Vec<(PathBuf, PathBuf)> = (0..2 * ONE_SHOT)
        .map(|at| old_copies(&home.join("roots").join(at.to_string())))
        .collect();
    assert!(service.current() && !service.throttled() && service.broken_program().is_none());
    assert_eq!(
        service.installed_program(),
        Some(service.executable.clone())
    );
    assert!(!service.runs_another_program() && !service.may_replace(&HOOKS));
    assert_eq!(
        service.repair(&service.executable, true, false, &HOOKS),
        Repair::Start
    );
    assert!(!absent.status().unwrap().installed && absent.uninstall().is_ok());
    assert!(absent.stop().is_ok() && transient(Path::new(ON_IMAGE)));
    assert_eq!(
        persistable(&service.executable, &service.root, None, &HOOKS).unwrap(),
        service.executable
    );
    assert_eq!(
        link_cli(&service.executable, &bin).unwrap(),
        bin.join("crazyagent")
    );
    let cli = std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/bin/crazyagent"));
    let present = cli
        .as_deref()
        .is_some_and(|link| std::fs::symlink_metadata(link).is_ok());
    println!(
        "NOTE service.retire_old_copies home_link={}",
        if present { "present" } else { "absent" }
    );
    assert_eq!(CONTROL_TEXT.len(), 120);
    time(&row("service.control.string_from", FAST), SAMPLES, || {
        black_box(String::from(black_box(CONTROL_TEXT)));
    });
    time(&row("service.control.format", FAST), SAMPLES, || {
        black_box(format!(
            "{}: {}",
            black_box(ON_IMAGE),
            black_box(CONTROL_TEXT)
        ));
    });
    time(&row("service.failure", FAST), SAMPLES, || {
        black_box(failure(black_box(TEXT)));
    });
    time(&row("service.service_path", ONE), SAMPLES, || {
        black_box(service_path(
            black_box(Some(OsStr::new(SHELL_PATH))),
            Some(&home),
            None,
        ));
    });
    time(&row("service.service_name", FILES), SAMPLES, || {
        black_box(service_name(
            Platform::MacOs,
            black_box(&service.root),
            None,
        ));
    });
    time(&row("service.Service.current_user", ONE), SAMPLES, || {
        black_box(
            Service::current_user(
                Platform::MacOs,
                service.executable.clone(),
                &service.root,
                None,
            )
            .unwrap(),
        );
    });
    time(
        &row("service.Service.definition_text", FAST),
        SAMPLES,
        || {
            black_box(black_box(&service).definition_text().unwrap());
        },
    );
    time(
        &row("service.Service.register_commands", FAST),
        SAMPLES,
        || {
            black_box(black_box(&service).register_commands(UID));
        },
    );
    time(
        &row("service.Service.unregister_commands", FAST),
        SAMPLES,
        || {
            black_box(black_box(&service).unregister_commands(UID));
        },
    );
    time(&row("service.Service.start_command", FAST), SAMPLES, || {
        black_box(black_box(&service).start_command(UID, true));
    });
    time(&row("service.Service.stop_command", FAST), SAMPLES, || {
        black_box(black_box(&service).stop_command(UID));
    });
    time(
        &row("service.Service.status_command", FAST),
        SAMPLES,
        || {
            black_box(black_box(&service).status_command(UID));
        },
    );
    time(&row("service.Service.parse_status", FAST), SAMPLES, || {
        black_box(black_box(&service).parse_status(true, black_box(STATUS_OUTPUT)));
    });
    time(&row("service.Service.status", ONE), SAMPLES, || {
        black_box(absent.status().unwrap());
    });
    time(&row("service.Service.current", FILES), SAMPLES, || {
        black_box(black_box(&service).current());
    });
    time(&row("service.Service.stale", FILES), SAMPLES, || {
        black_box(black_box(&service).stale(&other));
    });
    time(&row("service.Service.throttled", FILES), SAMPLES, || {
        black_box(black_box(&service).throttled());
    });
    time(
        &row("service.Service.broken_program", FILES),
        SAMPLES,
        || {
            black_box(black_box(&service).broken_program());
        },
    );
    time(
        &row("service.Service.installed_program", FILES),
        SAMPLES,
        || {
            black_box(black_box(&service).installed_program());
        },
    );
    time(&row("service.Service.repair", FILES), SAMPLES, || {
        black_box(black_box(&service).repair(&service.executable, true, false, &HOOKS));
    });
    time(&row("service.Service.refuse", FILES), SAMPLES, || {
        black_box(black_box(&service).refuse());
    });
    time(
        &row("service.Service.runs_another_program", FILES),
        SAMPLES,
        || {
            black_box(black_box(&service).runs_another_program());
        },
    );
    time(&row("service.Service.may_replace", FILES), SAMPLES, || {
        black_box(black_box(&service).may_replace(&HOOKS));
    });
    time(&row("service.Service.uninstall", ONE), SAMPLES, || {
        absent.uninstall().unwrap();
    });
    time(&row("service.Service.stop", ONE), SAMPLES, || {
        absent.stop().unwrap();
    });
    time(&row("service.transient", FAST), SAMPLES, || {
        black_box(transient(black_box(Path::new(ON_IMAGE))));
    });
    time(&row("service.transient_refused", FAST), SAMPLES, || {
        black_box(transient_refused(black_box(Path::new(ON_IMAGE))));
    });
    time(&row("service.persistable", FILES), SAMPLES, || {
        black_box(persistable(&service.executable, &service.root, None, &HOOKS).unwrap());
    });
    let mut kept = roots[..ONE_SHOT].iter();
    time(&row("service.retire_old_copies", ONE), SAMPLES, || {
        let (root, installed) = kept.next().unwrap();
        retire_old_copies(root, installed, &HOOKS);
    });
    assert!(kept.next().is_none() && !roots[0].0.join("bin").exists());
    let mut kept = roots[ONE_SHOT..].iter();
    time(&row("service.retire_old_copies_with", ONE), SAMPLES, || {
        let (root, installed) = kept.next().unwrap();
        retire_old_copies_with(root, installed, Some(&bin), &HOOKS);
    });
    assert!(kept.next().is_none() && !roots[ONE_SHOT].0.join("bin").exists());
    time(&row("service.write_definition", ONE), SAMPLES, || {
        write_definition(&service.definition, &text, Platform::MacOs).unwrap();
    });
    time(
        &row("service.changeable.changeable_by_others", FILES),
        SAMPLES,
        || {
            black_box(changeable_by_others(black_box(&service.executable)));
        },
    );
    time(
        &row("service.changeable.changeable_past_sticky", FILES),
        SAMPLES,
        || {
            black_box(changeable_past_sticky(black_box(&service.executable)));
        },
    );
    time(
        &row("service.changeable.changeable_above_socket", FILES),
        SAMPLES,
        || {
            black_box(changeable_above_socket(black_box(&socket)));
        },
    );
    held_steps(&socket);
    time(&row("service.cli_link.link_cli", ONE), SAMPLES, || {
        black_box(link_cli(&service.executable, &bin).unwrap());
    });
    let repointed = executable(&home.join("app-other").join("crazyagent"));
    let mut flip = false;
    time(
        &row("service.cli_link.link_cli_repoint", ONE),
        SAMPLES,
        || {
            flip = !flip;
            let named = if flip {
                &repointed
            } else {
                &service.executable
            };
            black_box(link_cli(named, &bin).unwrap());
        },
    );
}
