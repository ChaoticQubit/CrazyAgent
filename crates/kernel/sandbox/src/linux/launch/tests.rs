use super::*;
use std::os::unix::fs::MetadataExt;

fn helper() -> PathBuf {
    Path::new("/opt/crazyagent").join(HELPER_NAME)
}

#[test]
fn bubblewrap_starts_the_agent_in_a_new_session() {
    let grants = Grants {
        read: Vec::new(),
        write: Vec::new(),
        git_write: Vec::new(),
        guarded: Held::default(),
        hidden: Vec::new(),
    };
    let command = bwrap_command(
        Path::new(BWRAP_PATHS[0]),
        &helper(),
        &grants,
        Path::new("/tmp"),
        &SandboxNetwork::Open,
    );
    assert!(command.get_args().any(|arg| arg == "--new-session"));
}

#[test]
fn bubblewrap_cuts_off_the_network_only_when_nothing_is_allowed() {
    let grants = Grants {
        read: Vec::new(),
        write: Vec::new(),
        git_write: Vec::new(),
        guarded: Held::default(),
        hidden: Vec::new(),
    };
    let unshares = |network: SandboxNetwork| {
        bwrap_command(
            Path::new(BWRAP_PATHS[0]),
            &helper(),
            &grants,
            Path::new("/tmp"),
            &network,
        )
        .get_args()
        .any(|arg| arg == "--unshare-net")
    };
    assert!(unshares(SandboxNetwork::Only(Vec::new())));
    assert!(!unshares(SandboxNetwork::Open));
}

#[test]
fn a_git_folder_grant_reaches_the_helper_and_counts_toward_its_limit() {
    let folder = PathBuf::from("/p/.git/worktrees/agent-1");
    let policy = LandlockPolicy {
        read: vec![PathBuf::from("/usr")],
        git_write: vec![folder.clone()],
        ..LandlockPolicy::default()
    };
    let args = policy.args();
    let at = args.iter().position(|arg| arg == GIT_WRITE);
    assert_eq!(at.map(|at| &args[at + 1]), Some(&OsString::from(&folder)));
    let mut run = args;
    run.extend([OsString::from("--"), OsString::from("/bin/true")]);
    let parsed = match HelperRequest::parse(run) {
        Ok(HelperRequest::Run { policy, .. }) => Some(*policy),
        _ => None,
    };
    assert_eq!(parsed, Some(policy));
    let many = LandlockPolicy {
        git_write: vec![folder; HELPER_PATHS_MAX + 1],
        ..LandlockPolicy::default()
    };
    let mut run = many.args();
    run.extend([OsString::from("--"), OsString::from("/bin/true")]);
    assert_eq!(
        HelperRequest::parse(run).err(),
        Some("too many path grants".to_owned()),
        "the git tier counts toward the helper's path limit"
    );
}

#[test]
#[should_panic(expected = "no confined layout starts on bubblewrap")]
fn bubblewrap_never_takes_a_git_folder_grant() {
    let grants = Grants {
        read: Vec::new(),
        write: Vec::new(),
        git_write: vec![PathBuf::from("/p/.git/worktrees/agent-1")],
        guarded: Held::default(),
        hidden: Vec::new(),
    };
    let _command = bwrap_command(
        Path::new(BWRAP_PATHS[0]),
        &helper(),
        &grants,
        Path::new("/tmp"),
        &SandboxNetwork::Open,
    );
}

#[test]
fn an_open_network_agent_binds_every_port_but_the_kept_ones() {
    let keep_free = [1455, 11434, 41300];
    let ports = bindable(&keep_free);
    assert_eq!(ports.len(), usize::from(u16::MAX) + 1 - keep_free.len());
    assert_eq!(ports[..3], [0, 1, 2]);
    assert_eq!(ports.last(), Some(&u16::MAX));
    for port in keep_free {
        assert!(!ports.contains(&port), "{port}");
        assert!(ports.contains(&(port - 1)), "{port}");
        assert!(ports.contains(&(port + 1)), "{port}");
    }
    assert_eq!(bindable(&[]).len(), usize::from(u16::MAX) + 1);
}

#[test]
fn the_helper_refuses_kept_ports_that_do_not_fit_its_policy() {
    let parse = |args: &[&str]| HelperRequest::parse(args.iter().map(OsString::from).collect());
    assert_eq!(
        parse(&[PROBE, KEEP_FREE, "1455"]),
        Ok(HelperRequest::Probe {
            limit_network: false,
            open_sockets: false,
            keep_free: true,
            cut: None,
        })
    );
    assert_eq!(
        parse(&[PROBE]),
        Ok(HelperRequest::Probe {
            limit_network: false,
            open_sockets: false,
            keep_free: false,
            cut: None,
        })
    );
    assert_eq!(
        parse(&[PROBE, LIMIT_NETWORK, KEEP_FREE, "1455"]),
        Ok(HelperRequest::Probe {
            limit_network: true,
            open_sockets: false,
            keep_free: true,
            cut: None,
        })
    );
    assert_eq!(
        parse(&[LIMIT_NETWORK, KEEP_FREE, "1455", "--", "/bin/true"]),
        Ok(HelperRequest::Run {
            policy: Box::new(LandlockPolicy {
                limit_network: true,
                keep_free: vec![1455],
                ..LandlockPolicy::default()
            }),
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
        })
    );
    for refused in [
        &[KEEP_FREE, "0", "--", "/bin/true"][..],
        &[KEEP_FREE, "--", "/bin/true"],
        &[PROBE, KEEP_FREE],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
    let many: Vec<OsString> = (1..=KEEP_FREE_MAX + 1)
        .flat_map(|port| [OsString::from(KEEP_FREE), OsString::from(port.to_string())])
        .chain(["--".into(), "/bin/true".into()])
        .collect();
    assert_eq!(
        HelperRequest::parse(many),
        Err("too many kept ports".into())
    );
}

/// V243-LINUX-PROBE-STDERR-CAP: a failed probe that says far more than it should is read
/// only to [`PROBE_SAID_MAX`] bytes and still ends with its own status; bytes that are not
/// UTF-8 stay within the cap once replaced.
#[test]
fn a_failed_probe_is_read_only_to_its_cap_and_still_ends() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let fake = |name: &str, says: &str| {
        let path = temp.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{says} >&2\nexit 3\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let ascii = fake("ascii", "yes x | head -c 1048576");
    let (said, ended) = probe_said(&mut Command::new(&ascii)).unwrap();
    assert_eq!(said.len(), PROBE_SAID_MAX);
    assert!(said.starts_with("x\nx\n"));
    assert_eq!(ended, "exit status: 3");
    let raw = fake("raw", "head -c 1048576 /dev/zero | tr '\\000' '\\377'");
    let (said, ended) = probe_said(&mut Command::new(&raw)).unwrap();
    assert!(
        said.len() <= PROBE_SAID_MAX && said.len() > PROBE_SAID_MAX - 4,
        "{}",
        said.len()
    );
    assert!(
        said.chars()
            .all(|letter| letter == char::REPLACEMENT_CHARACTER)
    );
    assert_eq!(ended, "exit status: 3");
}

/// V243-LINUX-PROBE-STDERR-CAP: what a failed probe said goes to the log as one line, with
/// its newlines and other control characters escaped, so a probe cannot write log lines
/// of its own or terminal escapes.
#[test]
fn a_failed_probe_is_logged_on_one_line_with_its_control_characters_escaped() {
    let line = probe_log(
        &[OsStr::new("--probe")],
        "exit status: 3",
        "first\nsecond\u{7}\u{1b}[31m red\r",
    );
    assert!(!line.chars().any(char::is_control), "{line:?}");
    assert!(
        line.starts_with(r#"linux_probe_failed: ["--probe"] exit status: 3: "#),
        "{line:?}"
    );
    assert!(
        line.contains(r"first\nsecond\u{7}\u{1b}[31m red\r"),
        "{line:?}"
    );
}

/// V243-LINUX-PROBE-STDERR-CAP (#592 F1): what a failed probe that says far too much
/// writes to the log is one line, built from at most [`PROBE_SAID_MAX`] bytes, with its
/// control characters escaped. Writing what it said raw fails this row.
#[test]
fn a_failed_probe_writes_one_escaped_line_to_the_log() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let talker = temp.path().join("talker");
    let says = "yes \"$(printf 'first\\033[31m red\\r')\" | head -c 1048576 >&2";
    fs::write(&talker, format!("#!/bin/sh\n{says}\nexit 3\n")).unwrap();
    fs::set_permissions(&talker, fs::Permissions::from_mode(0o755)).unwrap();
    let mut log = Vec::new();
    let reason = probe_failure(Command::new(&talker).arg("--probe"), &mut log);
    assert_eq!(reason.as_deref(), Some("first[31m red"));
    let line = String::from_utf8(log).unwrap();
    let body = line
        .strip_suffix('\n')
        .expect("the log line ends in a newline");
    let start: String = body.chars().take(120).collect();
    assert!(!body.chars().any(char::is_control), "{start:?}");
    assert!(
        body.starts_with(
            r#"linux_probe_failed: ["--probe"] exit status: 3: "first\u{1b}[31m red\r\n"#
        ),
        "{start:?}"
    );
    assert!(body.len() <= 6 * PROBE_SAID_MAX + 64, "{}", body.len());
}

#[test]
fn a_closed_bubblewrap_agent_gets_no_name_resolver_and_starts_behind_the_socket_filter() {
    let grants = Grants {
        read: Vec::new(),
        write: Vec::new(),
        git_write: Vec::new(),
        guarded: Held::default(),
        hidden: Vec::new(),
    };
    let command = bwrap_command(
        Path::new(BWRAP_PATHS[0]),
        &helper(),
        &grants,
        Path::new("/tmp"),
        &SandboxNetwork::Only(Vec::new()),
    );
    let args: Vec<&OsStr> = command.get_args().collect();
    assert!(
        !args.contains(&OsStr::new("/run/systemd/resolve")),
        "{args:?}"
    );
    let [split, helper, filter] = args[args.len() - 3..] else {
        panic!("{args:?}");
    };
    assert_eq!(split, "--", "{args:?}");
    assert_eq!(
        Path::new(helper).file_name(),
        Some(OsStr::new(HELPER_NAME)),
        "{args:?}"
    );
    assert_eq!(filter, "--filter-sockets", "{args:?}");
    let bound = args
        .windows(3)
        .any(|three| three[0] == "--ro-bind" && three[1] == helper && three[2] == helper);
    assert!(bound, "the helper is bound into the namespace: {args:?}");
}

#[test]
fn route_flags_lead_only_a_nest_that_unshares_the_network_and_no_caller_names_its_descriptors() {
    use bridge::{BRIDGE, UNSHARE_NET};
    let nest = |head: &[&str], options: &[&str]| -> Vec<OsString> {
        let mut args: Vec<OsString> = head.iter().map(OsString::from).collect();
        args.extend([NEST, "--", BWRAP_PATHS[0]].map(OsString::from));
        args.extend(NEST_ARGS.map(OsString::from));
        args.extend(options.iter().map(OsString::from));
        args.push("--".into());
        args.push(helper().into());
        args.push(PROBE.into());
        args
    };
    let read = |args: Vec<OsString>| {
        read_request(args).map(|(routes, bridge, _, _)| (routes, bridge.map(|held| held.ports)))
    };
    let route = [egress::ROUTE, "443"];
    let bridged = [BRIDGE, "18080"];
    assert_eq!(read(nest(&route, &[UNSHARE_NET])), Ok((vec![443], None)));
    let both = [egress::ROUTE, "443", egress::ROUTE, "8443", BRIDGE, "18080"];
    assert_eq!(
        read(nest(&both, &[UNSHARE_NET])),
        Ok((vec![443, 8443], Some(vec![18080])))
    );
    assert_eq!(read(nest(&[], &[])), Ok((Vec::new(), None)));
    assert!(
        read(nest(&route, &[])).is_err(),
        "a route needs --unshare-net"
    );
    assert!(
        read(nest(&[], &[UNSHARE_NET])).is_err(),
        "--unshare-net needs a port"
    );
    let mut probe: Vec<OsString> = route.iter().map(OsString::from).collect();
    probe.push(PROBE.into());
    assert!(read(probe).is_err(), "a route leads a nest only");
    for options in [
        &[UNSHARE_NET, "--info-fd", "5"][..],
        &[UNSHARE_NET, "--block-fd", "6"],
        &[UNSHARE_NET, "--ro-bind-data", "7", "/etc/resolv.conf"],
        &["--info-fd", "5", UNSHARE_NET],
        &[UNSHARE_NET, "--args", "5"],
        &[UNSHARE_NET, "--userns-block-fd", "5"],
        &[UNSHARE_NET, "--sync-fd", "5"],
        &[UNSHARE_NET, "--json-status-fd", "5"],
    ] {
        assert!(read(nest(&route, options)).is_err(), "{options:?}");
        assert!(read(nest(&bridged, options)).is_err(), "{options:?}");
    }
}

#[test]
fn a_slirp4netns_or_bubblewrap_that_others_may_write_to_is_never_run() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let moded = |mode: u32| {
        let path = root.join(format!("slirp4netns-{mode:o}"));
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    };
    assert!(trusted(&moded(0o755)));
    assert!(trusted(&moded(0o700)));
    for refused in [0o757, 0o775, 0o777, 0o722] {
        assert!(!trusted(&moded(refused)), "{refused:o}");
    }
}

#[test]
fn a_project_may_hold_another_git_repository_below_its_top_or_where_none_is_ruled_out() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join(".git/hooks")).unwrap();
    fs::write(project.join(".git/config"), "").unwrap();
    fs::create_dir_all(project.join("src/deep")).unwrap();
    fs::write(project.join("src/deep/.gitignore"), "").unwrap();
    fs::write(project.join("a.git"), "").unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir_all(outside.join(".git")).unwrap();
    symlink(&outside, project.join("linked")).unwrap();
    assert!(
        !holds_other_git(&project),
        "its own .git, and a link is not followed"
    );
    let found = |make: &dyn Fn(&Path), undo: &dyn Fn(&Path)| {
        make(&project);
        let held = holds_other_git(&project);
        undo(&project);
        assert!(!holds_other_git(&project));
        held
    };
    for name in ["vendor/lib/.git", "vendor/.GIT", "vendor/.git.", ".Git"] {
        let folder = |project: &Path| fs::create_dir_all(project.join(name)).unwrap();
        let gone = |project: &Path| fs::remove_dir(project.join(name)).unwrap();
        assert!(found(&folder, &gone), "{name}");
    }
    fs::create_dir_all(project.join("vendor/lib")).unwrap();
    let link = |project: &Path| {
        fs::write(project.join("vendor/lib/.git"), "gitdir: x").unwrap();
    };
    let unlink = |project: &Path| fs::remove_file(project.join("vendor/lib/.git")).unwrap();
    assert!(
        found(&link, &unlink),
        "a .git file is rewritten without a create"
    );
    let pointer = |project: &Path| symlink(&outside, project.join("vendor/.git")).unwrap();
    let unpoint = |project: &Path| fs::remove_file(project.join("vendor/.git")).unwrap();
    assert!(found(&pointer, &unpoint), "a link named .git counts");
    let mode = |project: &Path, bits| {
        let permissions = fs::Permissions::from_mode(bits);
        fs::set_permissions(project.join("src/deep"), permissions).unwrap();
    };
    let locked = |project: &Path| mode(project, 0o000);
    let unlocked = |project: &Path| mode(project, 0o755);
    if !rustix::process::geteuid().is_root() {
        assert!(found(&locked, &unlocked), "a folder it cannot read");
    }
    let many = |project: &Path| {
        fs::create_dir(project.join("many")).unwrap();
        for index in 0..GIT_SEARCH_ENTRIES_MAX {
            fs::write(project.join("many").join(index.to_string()), "").unwrap();
        }
    };
    let few = |project: &Path| fs::remove_dir_all(project.join("many")).unwrap();
    assert!(found(&many, &few), "more entries than it reads");
    assert!(
        holds_other_git(&temp.path().join("missing")),
        "no folder to read"
    );
}

#[test]
fn the_route_answer_is_asked_once_per_engine_and_never_when_refused() {
    let kept: KeptRoute = Mutex::new(None);
    let probes = &std::cell::Cell::new(0u32);
    let probe = |answer: bool| {
        move || {
            probes.set(probes.get() + 1);
            answer
        }
    };
    let (bwrap, helper) = (Path::new(BWRAP_PATHS[0]), helper());
    assert!(
        !kept_route(&kept, bwrap, &helper, true, false, probe(true)),
        "as root"
    );
    assert_eq!(probes.get(), 0, "a refusal starts no probe");
    assert!(kept_route(&kept, bwrap, &helper, false, false, probe(true)));
    assert!(
        kept_route(&kept, bwrap, &helper, false, false, probe(false)),
        "kept"
    );
    assert_eq!(probes.get(), 1, "page, notice and launch read one answer");
    assert!(
        !kept_route(&kept, bwrap, &helper, true, false, probe(true)),
        "refused while kept"
    );
    assert!(
        kept_route(&kept, bwrap, &helper, false, false, probe(false)),
        "still kept"
    );
    assert_eq!(probes.get(), 1);
    let other = Path::new("/usr/local/bin/bwrap");
    assert!(
        !kept_route(&kept, other, &helper, false, false, probe(false)),
        "another bubblewrap"
    );
    assert!(
        !kept_route(&kept, other, &helper, false, false, probe(true)),
        "a failed answer too"
    );
    assert_eq!(probes.get(), 2);
}

/// rv-sa241 SBX-2-R2 N1: the host read asks a kept false route answer again, so one
/// failed probe (a timeout while the engine starts under load) leaves launches unrouted
/// only until the next host read. The owner's read of the project catalog or a project's
/// agent policy runs that read at most once per `HOST_AGE_MAX`, and an engine nobody
/// reads keeps the false answer until a restart. A launch reads what is kept and never
/// waits on the probe, a true answer is never asked again, and a false probe never
/// replaces a true another caller kept meanwhile.
#[test]
fn a_false_route_answer_is_asked_again_by_the_host_read_and_a_true_one_is_kept() {
    let kept: KeptRoute = Mutex::new(None);
    let probes = &std::cell::Cell::new(0u32);
    let probe = |answer: bool| {
        move || {
            probes.set(probes.get() + 1);
            answer
        }
    };
    let (bwrap, helper) = (Path::new(BWRAP_PATHS[0]), helper());
    assert!(
        !kept_route(&kept, bwrap, &helper, false, false, probe(false)),
        "the first probe failed"
    );
    assert!(
        !kept_route(&kept, bwrap, &helper, false, false, probe(true)),
        "a launch reads the kept false"
    );
    assert_eq!(probes.get(), 1, "a launch never asks a kept answer again");
    assert!(
        kept_route(&kept, bwrap, &helper, false, true, probe(true)),
        "the host read asks a false answer again"
    );
    assert_eq!(probes.get(), 2);
    assert!(
        kept_route(&kept, bwrap, &helper, false, false, probe(false)),
        "a launch reads the new true"
    );
    assert!(
        kept_route(&kept, bwrap, &helper, false, true, probe(false)),
        "a true answer is never asked again"
    );
    assert_eq!(probes.get(), 2);
    assert!(
        !kept_route(&kept, bwrap, &helper, true, true, probe(true)),
        "refused as root whatever is kept"
    );
    assert_eq!(probes.get(), 2);
    let asked = (bwrap.to_path_buf(), helper.clone());
    let racing: KeptRoute = Mutex::new(Some((asked.0.clone(), asked.1.clone(), false)));
    let found_meanwhile = || {
        *racing.lock().unwrap() = Some((asked.0.clone(), asked.1.clone(), true));
        false
    };
    assert!(
        kept_route(&racing, bwrap, &helper, false, true, found_meanwhile),
        "a true another caller kept while this probe failed holds"
    );
    assert!(
        kept_route(&racing, bwrap, &helper, false, false, probe(false)),
        "and stays kept"
    );
}

/// rv-sa241 SBX-2-R2 LOW-1: the nest answer the page and every launch read keeps a nest
/// found for the same bubblewrap and helper without asking again, and asks again after a
/// nest was not found and for another bubblewrap, whose miss keeps the found one.
#[test]
fn a_found_nest_is_kept_and_a_missing_one_is_asked_again() {
    let kept: KeptNest = Mutex::new(None);
    let probes = &std::cell::Cell::new(0u32);
    let probe = |answer: bool| {
        move || {
            probes.set(probes.get() + 1);
            answer
        }
    };
    let (bwrap, helper) = (Path::new(BWRAP_PATHS[0]), helper());
    assert!(!kept_nest(&kept, bwrap, &helper, probe(false)));
    assert!(
        kept_nest(&kept, bwrap, &helper, probe(true)),
        "a missing nest is asked again"
    );
    assert!(
        kept_nest(&kept, bwrap, &helper, probe(false)),
        "a found one is kept"
    );
    assert_eq!(
        probes.get(),
        2,
        "page, notice and launch read one found nest"
    );
    let other = Path::new("/usr/local/bin/bwrap");
    assert!(
        !kept_nest(&kept, other, &helper, probe(false)),
        "another bubblewrap is asked"
    );
    assert!(
        kept_nest(&kept, bwrap, &helper, probe(false)),
        "its miss keeps the found one"
    );
    assert_eq!(probes.get(), 3);
    let racing: KeptNest = Mutex::new(None);
    let found_meanwhile = || {
        *racing.lock().unwrap() = Some((bwrap.to_path_buf(), helper.clone()));
        false
    };
    assert!(
        kept_nest(&racing, bwrap, &helper, found_meanwhile),
        "a nest another caller found while this probe failed holds"
    );
}

#[test]
fn the_bubblewrap_a_nest_starts_is_checked_as_the_file_it_opened() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let moded = |name: &str, mode: u32| {
        let path = root.join(name);
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    };
    let plain = moded("plain", 0o755);
    let unread = moded("unread", 0o711);
    let setuid = moded("setuid", 0o4755);
    let linked = root.join("linked");
    std::os::unix::fs::symlink(&setuid, &linked).unwrap();
    for held in [&plain, &unread] {
        let file = trusted_file(held).unwrap_or_else(|| panic!("{}", held.display()));
        let opened = file.metadata().unwrap();
        let named = fs::metadata(held).unwrap();
        assert_eq!((opened.dev(), opened.ino()), (named.dev(), named.ino()));
    }
    for refused in [&setuid, &moded("setgid", 0o2755), &linked, &root] {
        assert!(trusted_file(refused).is_none(), "{}", refused.display());
        assert!(!trusted(refused), "{}", refused.display());
    }
    assert!(trusted_file(&root.join("absent")).is_none());
    let file = trusted_file(&plain).unwrap();
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o4755)).unwrap();
    let mode = file.metadata().unwrap().mode();
    assert_ne!(
        mode & SET_ID_BITS,
        0,
        "the open file is the one a swap changes"
    );
}

/// rv-code4 F1 (#622): a second mount of a hidden folder's disk that shows the folder, a
/// folder above it or one inside it refuses the launch wherever it is mounted (farm run
/// 44410206144 read the folder through one); another disk, a folder beside it and the
/// folder's own mount do not, and a mount list that does not read plainly refuses too.
#[test]
fn a_second_mount_of_a_hidden_folders_disk_refuses_the_launch() {
    let line = |id: u32, device: &str, root: &str, point: &str| {
        format!("{id} 1 {device} {root} {point} rw,relatime shared:1 - ext4 /dev/sda1 rw\n")
    };
    let base = line(21, "8:1", "/", "/") + &line(22, "8:2", "/", "/boot");
    let reason = |hidden: &str, text: &str| match mounted_again(&[PathBuf::from(hidden)], text) {
        Ok(()) => None,
        Err(SandboxError::Unavailable(reason)) => Some(reason),
        Err(other) => panic!("{other:?}"),
    };
    let folder = "/home/u/work/notes/private";
    let again = Some(HIDDEN_MOUNTED_AGAIN.to_owned());
    let unread = Some(HIDDEN_MOUNTS_UNREAD.to_owned());
    assert_eq!(reason(folder, &base), None);
    let seconds = [
        (line(23, "8:1", "/home/u", "/mnt/alias"), &again),
        (line(23, "8:1", "/", "/srv/root"), &again),
        (line(23, "8:1", folder, "/srv/view"), &again),
        (
            line(23, "8:1", "/home/u/work/notes/private/sub", "/srv/part"),
            &again,
        ),
        (
            line(23, "8:1", "/home/u", "/home/u/work/worktrees/alias"),
            &again,
        ),
        (line(23, "8:1", "/home/u", "/mnt/my\\040alias"), &again),
        (
            line(23, "8:1", "/home/u/work/context", "/srv/context"),
            &None,
        ),
        (
            line(23, "8:1", "/home/u/work/my\\040notes", "/srv/notes"),
            &None,
        ),
        (line(23, "8:9", "/home/u", "/mnt/other"), &None),
        (line(23, "0:4", "net:[4026532]", "/run/netns/one"), &None),
    ];
    for (second, expected) in seconds {
        assert_eq!(
            reason(folder, &(base.clone() + &second)),
            *expected,
            "{second}"
        );
    }
    let spaced = base.clone() + &line(23, "8:1", "/home/u/my\\040work", "/srv/spaced");
    assert_eq!(reason("/home/u/my work/private", &spaced), again);
    let own = line(21, "8:1", "/", "/") + &line(24, "8:3", "/u", "/home");
    assert_eq!(reason(folder, &own), None);
    assert_eq!(
        reason(folder, &(own + &line(25, "8:3", "/", "/mnt/home"))),
        again
    );
    for broken in [
        String::new(),
        base.clone() + "garbage\n",
        base.clone() + &line(23, "8:1", "/a\\0", "/b"),
        base.clone() + &line(23, "8:1", "/a\\08x", "/b"),
        base.clone() + &line(23, "eight", "/a", "/b"),
        base.clone() + &line(23, "8:1", "/a", "relative"),
    ] {
        assert_eq!(reason(folder, &broken), unread, "{broken}");
    }
}

/// V24x-LINUX-NEST-MOUNT-PROPAGATION: a launch with hidden folders asks the helper for a
/// private tree with each folder as a cover on both hiding backends, the nested policy
/// reading back from its own words, and a launch without them asks for none.
#[test]
fn a_launch_with_hidden_folders_asks_the_helper_for_a_private_tree_on_both_backends() {
    let folder = PathBuf::from("/w/project/private");
    let policy = LandlockPolicy {
        read: vec![PathBuf::from("/usr")],
        nested: true,
        cut: Some(vec![folder.clone()]),
        ..LandlockPolicy::default()
    };
    let args = policy.args();
    let at = args
        .iter()
        .position(|arg| arg == PRIVATE_MOUNTS)
        .expect("the nested run asks for a private tree");
    assert_eq!(
        args[at + 1..at + 3],
        [OsString::from(COVER), folder.clone().into_os_string()]
    );
    let mut words = args.clone();
    words.extend([OsString::from("--"), OsString::from("/bin/true")]);
    match HelperRequest::parse(words) {
        Ok(HelperRequest::Run { policy: back, .. }) => assert_eq!(*back, policy),
        other => panic!("the nested policy reads back: {other:?}"),
    }
    let plain = LandlockPolicy {
        cut: None,
        ..policy.clone()
    };
    assert!(!plain.args().iter().any(|arg| arg == PRIVATE_MOUNTS));
    let grants = |hidden: Vec<PathBuf>| Grants {
        read: Vec::new(),
        write: Vec::new(),
        git_write: Vec::new(),
        guarded: Held::default(),
        hidden,
    };
    let tail = |grants: &Grants| -> Vec<OsString> {
        bwrap_command(
            Path::new(BWRAP_PATHS[0]),
            &helper(),
            grants,
            Path::new("/tmp"),
            &SandboxNetwork::Open,
        )
        .get_args()
        .skip_while(|arg| *arg != FILTER_SOCKETS)
        .map(OsStr::to_os_string)
        .collect()
    };
    assert_eq!(
        tail(&grants(vec![folder.clone()])),
        [
            OsString::from(FILTER_SOCKETS),
            OsString::from(PRIVATE_MOUNTS),
            OsString::from(COVER),
            folder.into_os_string(),
        ]
    );
    assert_eq!(tail(&grants(Vec::new())), [OsString::from(FILTER_SOCKETS)]);
}

fn guarded() -> LandlockPolicy {
    LandlockPolicy {
        read: vec![PathBuf::from("/usr")],
        connect: vec![443, 8080],
        limit_network: true,
        guard: Guard {
            loopback: vec![8080],
            git: Some(GitOwn {
                trees: vec![PathBuf::from("/p")],
                kids: vec![PathBuf::from("/p/worktrees")],
                exact: vec![PathBuf::from("/p/modules/.git")],
            }),
            signals: true,
            held: Some(Held {
                entries: vec![PathBuf::from("/p/.git/worktrees/a/HEAD")],
                trees: vec![PathBuf::from("/p/.git/worktrees/a/modules")],
                stores: vec![PathBuf::from("/c/.git/objects")],
            }),
        },
        ..LandlockPolicy::default()
    }
}

#[test]
fn a_guarded_policy_reads_back_from_its_own_arguments() {
    let policy = guarded();
    let mut words = policy.args();
    let guard: Vec<OsString> = [
        GUARD_LOOPBACK,
        "8080",
        GUARD_GIT,
        OWN_TREE,
        "/p",
        OWN_KIDS,
        "/p/worktrees",
        OWN_GIT,
        "/p/modules/.git",
        GUARD_SIGNALS,
        GUARD_HELD,
        "/p/.git/worktrees/a/HEAD",
        GUARD_HELD_TREE,
        "/p/.git/worktrees/a/modules",
        GUARD_HELD_STORE,
        "/c/.git/objects",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    assert!(words.ends_with(&guard), "{words:?}");
    words.extend(["--".into(), "/bin/true".into()]);
    assert_eq!(
        HelperRequest::parse(words),
        Ok(HelperRequest::Run {
            policy: Box::new(policy),
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
        })
    );
    assert!(LandlockPolicy::default().guard.args().is_empty());
}

#[test]
fn each_guard_stands_alone_and_a_guard_that_does_not_fit_its_policy_is_refused() {
    let parse = |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
    let held = |words: &[&str]| match parse(words) {
        Ok(HelperRequest::Run { policy, .. }) => Some(policy.guard),
        _ => None,
    };
    let signals = Guard {
        signals: true,
        ..Guard::default()
    };
    let git = Guard {
        git: Some(GitOwn::default()),
        ..Guard::default()
    };
    assert_eq!(held(&[GUARD_SIGNALS, "--", "/bin/true"]), Some(signals));
    assert_eq!(held(&[GUARD_GIT, "--", "/bin/true"]), Some(git));
    assert_eq!(held(&["--", "/bin/true"]), Some(Guard::default()));
    let tree = Guard {
        held: Some(Held {
            entries: Vec::new(),
            trees: vec![PathBuf::from("/p/t")],
            stores: Vec::new(),
        }),
        ..Guard::default()
    };
    assert_eq!(
        held(&[GUARD_HELD_TREE, "/p/t", "--", "/bin/true"]),
        Some(tree)
    );
    let store = Guard {
        held: Some(Held {
            stores: vec![PathBuf::from("/c/s")],
            ..Held::default()
        }),
        ..Guard::default()
    };
    assert_eq!(
        held(&[GUARD_HELD_STORE, "/c/s", "--", "/bin/true"]),
        Some(store)
    );
    for refused in [
        &[OWN_TREE, "/p", "--", "/bin/true"][..],
        &[OWN_KIDS, "/p", GUARD_GIT, "--", "/bin/true"],
        &[GUARD_GIT, OWN_GIT, "p/.git", "--", "/bin/true"],
        &[GUARD_GIT, GUARD_GIT, "--", "/bin/true"],
        &[GUARD_LOOPBACK, "8080", "--", "/bin/true"],
        &[LIMIT_NETWORK, GUARD_LOOPBACK, "8080", "--", "/bin/true"],
        &[GUARD_SIGNALS, DIE_WITH, "4242", "--", "/bin/true"],
        &[PROBE, GUARD_SIGNALS],
        &[PROBE, GUARD_GIT],
        &[FILTER_SOCKETS, GUARD_SIGNALS, "--", "/bin/true"],
        &[GUARD_HELD, "p/.git/worktrees/a/HEAD", "--", "/bin/true"],
        &[GUARD_HELD_TREE, "--", "/bin/true"],
        &[GUARD_HELD, "/p/a", DIE_WITH, "4242", "--", "/bin/true"],
        &[PROBE, GUARD_HELD, "/p/a"],
        &[GUARD_HELD_STORE, "c/s", "--", "/bin/true"],
        &[PROBE, GUARD_HELD_STORE, "/c/s"],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
    let limited = [LIMIT_NETWORK, CONNECT, "8080", GUARD_LOOPBACK];
    let ported = |more: &[&'static str]| -> Vec<&'static str> {
        [&limited[..], more, &["--", "/bin/true"][..]].concat()
    };
    for refused in [&["0"][..], &["8081"], &["8080", OPEN_SOCKETS]] {
        assert!(parse(&ported(refused)).is_err(), "{refused:?}");
    }
    let fits = held(&ported(&["8080"]));
    assert_eq!(fits.map(|guard| guard.loopback), Some(vec![8080]));
}

#[test]
fn a_guard_keeps_its_ports_on_this_machine_and_leaves_the_others_to_every_host() {
    let policy = guarded();
    let held = policy.guard.supervised(&policy.connect);
    let reach = held
        .reach
        .as_ref()
        .expect("a guarded port is a guarded connect");
    assert_eq!(reach.any, [443]);
    assert_eq!(reach.loopback, [8080]);
    assert_eq!(held.git, policy.guard.git);
    assert!(held.signals);
    let alone = Guard {
        signals: true,
        ..Guard::default()
    };
    let held = alone.supervised(&policy.connect);
    assert_eq!((held.reach, held.git, held.signals), (None, None, true));
}

/// Each supervisor probe gets a new folder only this user may open, under a name no
/// stale, concurrent or planted folder takes, and none where the data folder refuses
/// one (rv-secreview linux-t4c F1).
#[test]
fn each_supervisor_probe_gets_a_new_private_folder_under_the_engine_data_folder() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().canonicalize().unwrap();
    let first = private_folder(&data).expect("a folder in the data folder");
    let second = private_folder(&data).expect("a second folder beside it");
    assert_ne!(first, second);
    for folder in [&first, &second] {
        assert_eq!(folder.parent(), Some(data.as_path()));
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(PROBE_FOLDER_PREFIX), "{name}");
        assert!(private_empty(folder), "{}", folder.display());
    }
    let file = data.join("file");
    fs::write(&file, "").unwrap();
    assert_eq!(private_folder(&file), None);
    assert_eq!(private_folder(&data.join("missing")), None);
}

/// The helper runs its supervisor probe only in the private folder the engine made for
/// it: a link to it, a folder others may open, one that holds anything, a file or no
/// folder at all fails it (rv-secreview linux-t4c F1).
#[test]
fn the_supervisor_probe_takes_only_an_empty_folder_only_this_user_may_open() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().canonicalize().unwrap();
    let private = data.join("private");
    fs::DirBuilder::new().mode(0o700).create(&private).unwrap();
    assert!(private_empty(&private));
    let linked = data.join("linked");
    symlink(&private, &linked).unwrap();
    assert!(!private_empty(&linked), "a link to it");
    fs::write(private.join("made"), "").unwrap();
    assert!(!private_empty(&private), "a folder that holds a file");
    fs::remove_file(private.join("made")).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!private_empty(&private), "a folder others may open");
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(private_empty(&private));
    let file = data.join("file");
    fs::write(&file, "").unwrap();
    assert!(!private_empty(&file), "a file");
    assert!(!private_empty(&data.join("missing")), "no folder");
}

/// The engine makes the probe's folder, hands it to the helper and removes it whatever
/// the probe answered, so a failed or killed probe leaves nothing a later one trips on,
/// two probes at once each get their own, and as root no probe runs (rv-secreview
/// linux-t4c F1, rv-code4 linux-t4c F1).
#[test]
fn a_supervisor_probe_runs_in_its_own_folder_the_engine_removes_after() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let place = temp.path().canonicalize().unwrap();
    let data = place.join("data");
    fs::create_dir(&data).unwrap();
    let seen = place.join("seen");
    let fake = |name: &str, answer: u8| {
        let helper = place.join(name);
        let script = format!(
            "#!/bin/sh\n[ \"$1\" = {PROBE_SUPERVISOR} ] && [ -d \"$2\" ] || exit 9\n\
             printf '%s\\n' \"$2\" >> '{}'\n: > \"$2/made\"\nexit {answer}\n",
            seen.display()
        );
        fs::write(&helper, script).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        helper
    };
    let holding = fake("holding", 0);
    let failing = fake("failing", 1);
    assert!(supervises(&holding, None, &data, false));
    assert!(!supervises(&failing, None, &data, false));
    let both = std::thread::scope(|scope| {
        let one = scope.spawn(|| supervises(&holding, None, &data, false));
        let two = scope.spawn(|| supervises(&holding, None, &data, false));
        [one.join().unwrap(), two.join().unwrap()]
    });
    assert_eq!(both, [true, true]);
    let told = fs::read_to_string(&seen).unwrap();
    let folders: std::collections::BTreeSet<&str> = told.lines().collect();
    assert_eq!((told.lines().count(), folders.len()), (4, 4), "{told}");
    for folder in &folders {
        assert!(Path::new(folder).starts_with(&data), "{folder}");
    }
    assert_eq!(
        fs::read_dir(&data).unwrap().count(),
        0,
        "every folder removed"
    );
    assert!(!supervises(&holding, None, &data, true), "never as root");
    assert_eq!(
        fs::read_to_string(&seen).unwrap(),
        told,
        "as root no probe runs"
    );
    assert_eq!(
        fs::read_dir(&data).unwrap().count(),
        0,
        "and no folder is made"
    );
}

/// The helper refuses every variable that makes glibc load code, the dynamic loader's
/// and gconv's, and no other: a name that only starts with `LD`, holds `LD_` later, or
/// differs in case still passes (DEC-179 SBX-3 fold, rv-sa241 MED-1, rv-ds241 F2).
#[test]
fn the_helper_refuses_only_the_variables_that_load_code() {
    for name in [
        "LD_PRELOAD",
        "LD_AUDIT",
        "LD_LIBRARY_PATH",
        "LD_BIND_NOW",
        "GLIBC_TUNABLES",
        "GCONV_PATH",
    ] {
        assert!(loader_variable(OsStr::new(name)), "{name}");
    }
    for name in [
        "LDFLAGS",
        "OLD_PWD",
        "PATH",
        "ld_preload",
        "GLIBC_TUNABLES_X",
        "",
    ] {
        assert!(!loader_variable(OsStr::new(name)), "{name}");
    }
}
