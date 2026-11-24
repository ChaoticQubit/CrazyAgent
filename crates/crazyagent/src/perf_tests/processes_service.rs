use crate::paths::{Layout, Platform};
use crate::perf_kit::{Row, SAMPLES, time};
use crate::sandbox::grants::socket_fit;
use crate::service::{
    Repair, Service, changeable_by_others, changeable_past_sticky, failure, link_cli, persistable,
    retire_old_copies, retire_old_copies_with, service_name, service_path, transient,
    transient_refused, write_definition,
};
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

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "processes",
        name,
        batch,
    }
}

fn private_folder(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

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
        definition: home.join("LaunchAgents").join(definition),
        user: None,
    }
}

/// One data root holding the copy an older version kept in `<root>/bin`, and the copy
/// `persistable` keeps in `<root>/setup/bin`. Input: the folder to make it in. Output: the
/// layout and the kept engine.
fn old_copies(folder: &Path) -> (Layout, PathBuf) {
    private_folder(&folder.join("bin"));
    executable(&folder.join("bin").join("crazyagent"));
    private_folder(&folder.join("setup").join("bin"));
    let installed = executable(&folder.join("setup").join("bin").join("crazyagent"));
    (Layout::new(folder.to_path_buf()).unwrap(), installed)
}

/// Times the 2.4.4 path of every 2.5 service row, under the 2.5 row names, on the same
/// stand-in service in a temporary home whose label launchd never knew. 2.4.4 had no
/// engine hooks, so `repair`, `may_replace`, `persistable` and `retire_old_copies` call the
/// engine's own checks, and `changeable_above_socket` times `sandbox::grants::socket_fit`,
/// the 2.4.4 check of a granted socket. Prints one PERF line per call, asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn service_calls() {
    let home = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(home.path()).unwrap();
    let service = stand_in(&home, "perf.plist");
    let absent = stand_in(&home, "absent.plist");
    let layout = Layout::new(service.root.clone()).unwrap();
    let text = service.definition_text().unwrap();
    write_definition(&service.definition, &text, Platform::MacOs).unwrap();
    let other = executable(&home.join("other").join("crazyagent"));
    let bin = home.join("local-bin");
    std::fs::create_dir_all(home.join("socket-folder")).unwrap();
    let socket = home.join("socket-folder").join("engine.sock");
    let _listener = crate::spawn_gate::bind_unix_std(&socket).unwrap();
    let roots: Vec<(Layout, PathBuf)> = (0..2 * ONE_SHOT)
        .map(|at| old_copies(&home.join("roots").join(at.to_string())))
        .collect();
    assert!(service.current() && !service.throttled() && service.broken_program().is_none());
    assert_eq!(
        service.installed_program(),
        Some(service.executable.clone())
    );
    assert!(!service.runs_another_program() && !service.may_replace());
    assert_eq!(
        service.repair(&service.executable, true, false),
        Repair::Start
    );
    assert!(!absent.status().unwrap().installed && absent.uninstall().is_ok());
    assert!(absent.stop().is_ok() && transient(Path::new(ON_IMAGE)));
    assert_eq!(
        persistable(&service.executable, &layout, None).unwrap(),
        service.executable
    );
    assert_eq!(
        link_cli(&service.executable, &bin).unwrap(),
        bin.join("crazyagent")
    );
    assert!(socket_fit(&socket).is_ok());
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
            Service::current_user(Platform::MacOs, service.executable.clone(), &layout).unwrap(),
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
        black_box(black_box(&service).repair(&service.executable, true, false));
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
        black_box(black_box(&service).may_replace());
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
        black_box(persistable(&service.executable, &layout, None).unwrap());
    });
    let mut kept = roots[..ONE_SHOT].iter();
    time(&row("service.retire_old_copies", ONE), SAMPLES, || {
        let (layout, installed) = kept.next().unwrap();
        retire_old_copies(layout, installed);
    });
    assert!(kept.next().is_none() && !roots[0].0.root().join("bin").exists());
    let mut kept = roots[ONE_SHOT..].iter();
    time(&row("service.retire_old_copies_with", ONE), SAMPLES, || {
        let (layout, installed) = kept.next().unwrap();
        retire_old_copies_with(layout, installed, Some(&bin));
    });
    assert!(kept.next().is_none() && !roots[ONE_SHOT].0.root().join("bin").exists());
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
            black_box(socket_fit(black_box(&socket)).unwrap());
        },
    );
    time(&row("service.cli_link.link_cli", ONE), SAMPLES, || {
        black_box(link_cli(&service.executable, &bin).unwrap());
    });
}
