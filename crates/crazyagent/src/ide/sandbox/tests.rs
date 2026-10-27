use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

const PORT: u16 = 45_731;

fn folder(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// Runs `program` with `args` the way the IDE server starts: under the installed profile
/// through [`command`], from a cleared environment. Output: whether it exited 0, and its
/// standard output.
fn run(profile: &Path, program: &str, args: &[&std::ffi::OsStr]) -> (bool, String) {
    let mut command = super::command(profile, Path::new(program));
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crate::spawn_gate::output(&mut command).unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn a_process_launched_the_ide_way_reads_its_capability_but_no_owner_key_or_engine_database() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let owner = data.join("o00000000");
    let ide = data.join(crate::ide::FOLDER);
    let mine = ide.join("p1");
    let other = ide.join("p2");
    let build = ide.join("builds").join("v1");
    let root = base.join("project");
    let (home, extensions) = (mine.join("server"), mine.join("extensions"));
    for path in [&owner, &home, &extensions, &other, &build, &root] {
        folder(path);
    }
    let files = [
        (owner.join("owner-desktop.cap"), "owner desktop key"),
        (owner.join("owner-tui.cap"), "owner tui key"),
        (owner.join("r00.cap"), "runner key"),
        (data.join("engine.db"), "engine database"),
        (data.join("engine.db-wal"), "engine database log"),
        (mine.join("capability"), "my capability"),
        (mine.join("token"), "my token"),
        (other.join("capability"), "another capability"),
        (build.join("server-main.js"), "build file"),
        (root.join("README"), "project file"),
    ];
    for (path, text) in &files {
        fs::write(path, text).unwrap();
    }
    let tmp = crate::sandbox::toolchains::make_tmp(&home).unwrap();
    let (capability, token) = (mine.join("capability"), mine.join("token"));
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[extensions.as_path()],
        readable: &[token.as_path(), capability.as_path()],
    };
    let profile = install(&mine, &grants, PORT).unwrap();
    assert_eq!(profile, mine.join(PROFILE));
    let read = |path: &Path| run(&profile, "/bin/cat", &[path.as_os_str()]);
    for (path, text) in &files {
        let allowed = [
            &capability,
            &token,
            &build.join("server-main.js"),
            &root.join("README"),
        ]
        .contains(&path);
        let (ran, printed) = read(path.as_path());
        assert_eq!(ran, allowed, "{}", path.display());
        assert_eq!(printed == *text, allowed, "{}", path.display());
    }
    assert!(!run(&profile, "/bin/ls", &[data.as_os_str()]).0);
    assert!(!run(&profile, "/bin/ls", &[owner.as_os_str()]).0);
    let touch = |path: &Path| run(&profile, "/usr/bin/touch", &[path.as_os_str()]).0;
    assert!(touch(home.join("state").as_path()));
    assert!(touch(extensions.join("state").as_path()));
    assert!(touch(root.join("edited").as_path()));
    assert!(!touch(mine.join("planted").as_path()));
    assert!(!touch(data.join("planted").as_path()));
    assert!(!touch(owner.join("owner-desktop.cap").as_path()));
    assert!(!touch(mine.join(PROFILE).as_path()));
    assert_eq!(
        fs::read_to_string(owner.join("owner-desktop.cap")).unwrap(),
        "owner desktop key"
    );
    let socket = owner.join("owner.sock");
    let stray = owner.join("other.sock");
    let _listeners = [
        crate::spawn_gate::bind_unix_std(&socket).unwrap(),
        crate::spawn_gate::bind_unix_std(&stray).unwrap(),
    ];
    let connect = |path: &Path| {
        run(
            &profile,
            "/usr/bin/perl",
            &[
                std::ffi::OsStr::new("-MIO::Socket::UNIX"),
                std::ffi::OsStr::new("-e"),
                std::ffi::OsStr::new("IO::Socket::UNIX->new(Peer => $ARGV[0]) or exit 3"),
                path.as_os_str(),
            ],
        )
        .0
    };
    assert!(connect(socket.as_path()));
    assert!(!connect(stray.as_path()));
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

#[test]
fn an_ide_profile_refuses_a_project_inside_the_engine_data_root() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let data = fs::canonicalize(scratch.path()).unwrap();
    let home = data.join("ide").join("p1").join("server");
    let build = data.join("ide").join("builds").join("v1");
    let root = data.join("projects").join("inside");
    for path in [&home, &build, &root] {
        folder(path);
    }
    let tmp = crate::sandbox::toolchains::make_tmp(&home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        readable: &[],
    };
    assert_eq!(profile(&grants, PORT).unwrap_err().0, "ide_sandbox_invalid");
    let elsewhere = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let project = fs::canonicalize(elsewhere.path()).unwrap();
    let outside = Grants {
        root: &project,
        ..grants
    };
    assert!(profile(&outside, PORT).is_ok());
    assert_eq!(profile(&outside, 0).unwrap_err().0, "ide_sandbox_invalid");
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}
