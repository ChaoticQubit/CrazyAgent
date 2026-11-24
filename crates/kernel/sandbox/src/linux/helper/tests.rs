use super::*;
use ::landlock::{Access, AccessFs};

#[test]
fn only_fully_enforced_rules_with_no_new_privs_hold_the_boundary() {
    assert!(accepted(&RulesetStatus::FullyEnforced, true));
    assert!(!accepted(&RulesetStatus::PartiallyEnforced, true));
    assert!(!accepted(&RulesetStatus::NotEnforced, true));
    assert!(!accepted(&RulesetStatus::FullyEnforced, false));
    assert!(!accepted(&RulesetStatus::PartiallyEnforced, false));
}

#[test]
fn a_git_folder_grant_makes_no_link_and_moves_nothing_across_folders() {
    let rights = git_folder_rights();
    for kept_out in [
        AccessFs::MakeSym,
        AccessFs::Refer,
        AccessFs::Execute,
        AccessFs::MakeChar,
        AccessFs::MakeBlock,
        AccessFs::MakeFifo,
        AccessFs::MakeSock,
    ] {
        assert!(!rights.contains(kept_out), "{kept_out:?}");
    }
    for kept in [
        AccessFs::ReadFile,
        AccessFs::ReadDir,
        AccessFs::WriteFile,
        AccessFs::Truncate,
        AccessFs::MakeReg,
        AccessFs::MakeDir,
        AccessFs::RemoveFile,
        AccessFs::RemoveDir,
    ] {
        assert!(rights.contains(kept), "{kept:?}");
    }
    assert!(AccessFs::from_all(FS_ABI).contains(rights));
}

#[test]
fn the_ports_linux_hands_out_are_read_as_two_rising_numbers_or_not_at_all() {
    assert_eq!(picked_ports("32768\t60999\n"), Some(32768..=60999));
    assert_eq!(picked_ports("1024 65535"), Some(1024..=65535));
    assert_eq!(picked_ports("40000 40000"), Some(40000..=40000));
    for text in [
        "",
        "32768",
        "60999 32768",
        "1 2 3",
        "low high",
        "32768 70000",
        "-1 5",
    ] {
        assert_eq!(picked_ports(text), None, "{text:?}");
    }
}

#[test]
fn guarded_tcp_ports_refuse_the_stream_sockets_that_are_not_plain_tcp() {
    assert_eq!(refused_sockets(false, false).unwrap().len(), 1);
    assert_eq!(refused_sockets(false, true).unwrap().len(), 4);
    assert_eq!(
        refused_sockets(true, true).unwrap().len(),
        refused_sockets(true, false).unwrap().len()
    );
}

#[test]
fn the_socket_filter_request_takes_no_policy_and_names_the_harness() {
    let parse = |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
    assert_eq!(
        parse(&[FILTER_SOCKETS, "--", "/bin/true", "-x"]),
        Ok(HelperRequest::Filter {
            program: PathBuf::from("/bin/true"),
            args: vec![OsString::from("-x")],
            cut: None,
            bound: Vec::new(),
        })
    );
    assert!(parse(&[FILTER_SOCKETS, READ, "/usr", "--", "/bin/true"]).is_err());
    assert!(parse(&[FILTER_SOCKETS, "--", "bin/true"]).is_err());
    assert!(parse(&[PROBE, FILTER_SOCKETS]).is_err());
}

#[test]
fn the_socket_filter_request_carries_each_bound_root_and_no_other_request_does() {
    let parse = |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
    assert_eq!(
        parse(&[
            FILTER_SOCKETS,
            BOUND,
            "/w/home",
            "7",
            "9",
            "--",
            "/bin/true"
        ]),
        Ok(HelperRequest::Filter {
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
            cut: None,
            bound: vec![crate::linux::places::Bound {
                root: PathBuf::from("/w/home"),
                dev: 7,
                ino: 9,
            }],
        })
    );
    for refused in [
        &[
            BOUND,
            "/w/home",
            "7",
            "9",
            FILTER_SOCKETS,
            "--",
            "/bin/true",
        ][..],
        &[FILTER_SOCKETS, BOUND, "w/home", "7", "9", "--", "/bin/true"],
        &[FILTER_SOCKETS, BOUND, "/w/home", "7", "--", "/bin/true"],
        &[READ, "/usr", BOUND, "/w/home", "7", "9", "--", "/bin/true"],
        &[PROBE, BOUND, "/w/home", "7", "9"],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
    let mut many = vec![FILTER_SOCKETS.to_string()];
    for _ in 0..=HELPER_PATHS_MAX {
        many.extend([BOUND.into(), "/w".into(), "1".into(), "2".into()]);
    }
    many.extend(["--".into(), "/bin/true".into()]);
    let refused = HelperRequest::parse(many.iter().map(OsString::from).collect()).unwrap_err();
    assert!(refused.contains("bound roots"), "{refused}");
}

#[test]
fn the_scope_question_stands_alone() {
    let parse = |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
    assert_eq!(parse(&[SCOPED]), Ok(HelperRequest::Scoped));
    assert!(parse(&[SCOPED, PROBE]).is_err());
    assert!(parse(&[PROBE, SCOPED]).is_err());
    assert!(parse(&[SCOPED, "--", "/bin/true"]).is_err());
    assert!(parse(&[READ, "/usr", SCOPED, "--", "/bin/true"]).is_err());
}

#[test]
fn the_slirp_request_takes_a_process_above_one_and_a_descriptor_above_two() {
    let parse = |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
    let slirp = egress::SLIRP;
    assert_eq!(
        parse(&[slirp, "4242", "5"]),
        Ok(HelperRequest::Slirp {
            child: 4242,
            ready: 5
        })
    );
    assert_eq!(
        parse(&[slirp, "2147483647", "3"]),
        Ok(HelperRequest::Slirp {
            child: 2_147_483_647,
            ready: 3
        })
    );
    for refused in [
        &[slirp][..],
        &[slirp, "4242"],
        &[slirp, "4242", "5", "6"],
        &[slirp, "1", "5"],
        &[slirp, "0", "5"],
        &[slirp, "2147483648", "5"],
        &[slirp, "+4242", "5"],
        &[slirp, "-4242", "5"],
        &[slirp, "4242", "2"],
        &[slirp, "4242", " 5"],
        &[slirp, "4242", "0x5"],
        &[slirp, "", "5"],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
    assert!(parse(&[READ, "/usr", slirp, "4242", "5"]).is_err());
    assert!(parse(&[PROBE, slirp, "4242", "5"]).is_err());
}

#[test]
fn the_helper_starts_only_a_fixed_bubblewrap_for_a_nest_and_takes_no_policy_with_it() {
    let parse = |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
    let nest = |bwrap: &str, tail: &[&str]| {
        let mut words = vec![NEST, "--", bwrap];
        words.extend(NEST_ARGS);
        words.extend(tail);
        parse(&words)
    };
    let tail = [
        "--chdir",
        "/work",
        "--",
        "/opt/helper",
        NESTED,
        "--",
        "/bin/true",
    ];
    let passed: Vec<OsString> = NEST_ARGS.iter().chain(&tail).map(OsString::from).collect();
    for bwrap in BWRAP_PATHS {
        assert_eq!(
            nest(bwrap, &tail),
            Ok(HelperRequest::Nest {
                bwrap: PathBuf::from(bwrap),
                args: passed.clone(),
            })
        );
    }
    assert!(nest("/tmp/bwrap", &tail).is_err());
    assert!(nest("usr/bin/bwrap", &tail).is_err());
    assert!(
        nest(BWRAP_PATHS[0], &[]).is_err(),
        "nothing to start inside"
    );
    let own = [NEST, "--", BWRAP_PATHS[0], "--bind", "/", "/", "--", "/x"];
    assert!(parse(&own).is_err(), "only the fixed arguments");
    assert!(parse(&[NEST, READ, "/usr", "--", BWRAP_PATHS[0]]).is_err());
    assert!(parse(&[NEST, PROBE]).is_err());
    assert!(parse(&[PROBE, NEST]).is_err());
    assert!(parse(&[NEST]).is_err());
}

/// Between the fixed nest arguments and the nest's `--` the helper takes only `--unshare-net` and
/// then `--chdir` with an absolute path, the options its own builders write, and needs a command
/// after that `--`, so no caller adds a capability, a seccomp filter, a namespace or a bind to the
/// agent's bubblewrap.
#[test]
fn a_nest_takes_only_its_network_and_working_folder_before_the_command() {
    use bridge::UNSHARE_NET;
    let nest = |options: &[&str]| {
        let mut words = vec![NEST, "--", BWRAP_PATHS[0]];
        words.extend(NEST_ARGS);
        words.extend(options);
        HelperRequest::parse(words.iter().map(OsString::from).collect())
    };
    let helper = "/opt/helper";
    for kept in [
        &["--", helper][..],
        &[UNSHARE_NET, "--", helper],
        &["--chdir", "/work", "--", helper],
        &[UNSHARE_NET, "--chdir", "/work", "--", helper, NESTED],
    ] {
        assert!(nest(kept).is_ok(), "{kept:?}");
    }
    for refused in [
        &[UNSHARE_NET, "--cap-add", "CAP_NET_ADMIN", "--", helper][..],
        &["--seccomp", "5", "--", helper],
        &["--userns", "5", "--", helper],
        &["--bind", "/", "/", "--", helper],
        &["--chdir", "work", "--", helper],
        &["--chdir", "/work/../etc", "--", helper],
        &["--chdir", "/work", UNSHARE_NET, "--", helper],
        &[UNSHARE_NET, UNSHARE_NET, "--", helper],
        &["--chdir", "/a", "--chdir", "/b", "--", helper],
        &["--chdir", "--", helper],
        &[helper],
        &[UNSHARE_NET, "--"],
    ] {
        assert!(nest(refused).is_err(), "{refused:?}");
    }
}

/// A nest also takes the empty read-only folders a hidden folder gets, each as the exact
/// pair `--tmpfs P --remount-ro P` on one clean absolute path below the root, after the
/// network and before the working folder, at most [`HIDDEN_MAX`] of
/// them; any other shape, a lone half, two paths or one more pair is refused.
#[test]
fn a_nest_takes_exact_read_only_empty_folder_pairs_before_its_working_folder() {
    use bridge::UNSHARE_NET;
    let nest = |options: &[&str]| {
        let mut words = vec![NEST, "--", BWRAP_PATHS[0]];
        words.extend(NEST_ARGS);
        words.extend(options);
        HelperRequest::parse(words.iter().map(OsString::from).collect())
    };
    let helper = "/opt/helper";
    let (hidden, other) = ("/work/private", "/work/notes/inner");
    for kept in [
        &["--tmpfs", hidden, "--remount-ro", hidden, "--", helper][..],
        &[
            UNSHARE_NET,
            "--tmpfs",
            hidden,
            "--remount-ro",
            hidden,
            "--tmpfs",
            other,
            "--remount-ro",
            other,
            "--chdir",
            "/work",
            "--",
            helper,
            NESTED,
        ],
    ] {
        assert!(nest(kept).is_ok(), "{kept:?}");
    }
    for refused in [
        &["--tmpfs", hidden, "--", helper][..],
        &["--remount-ro", hidden, "--", helper],
        &["--tmpfs", hidden, "--remount-ro", other, "--", helper],
        &[
            "--tmpfs",
            "work/private",
            "--remount-ro",
            "work/private",
            "--",
            helper,
        ],
        &[
            "--tmpfs",
            "/work/../etc",
            "--remount-ro",
            "/work/../etc",
            "--",
            helper,
        ],
        &["--tmpfs", "/", "--remount-ro", "/", "--", helper],
        &["--remount-ro", hidden, "--tmpfs", hidden, "--", helper],
        &[
            "--chdir",
            "/work",
            "--tmpfs",
            hidden,
            "--remount-ro",
            hidden,
            "--",
            helper,
        ],
        &[
            "--tmpfs",
            hidden,
            "--remount-ro",
            hidden,
            UNSHARE_NET,
            "--",
            helper,
        ],
        &["--ro-bind", hidden, hidden, "--", helper],
    ] {
        assert!(nest(refused).is_err(), "{refused:?}");
    }
    let paths: Vec<String> = (0..=HIDDEN_MAX)
        .map(|number| format!("/work/hidden-{number}"))
        .collect();
    let pairs = |count: usize| {
        let mut words: Vec<&str> = Vec::new();
        for path in &paths[..count] {
            words.extend(["--tmpfs", path.as_str(), "--remount-ro", path.as_str()]);
        }
        words.extend(["--", helper]);
        nest(&words)
    };
    assert!(pairs(HIDDEN_MAX).is_ok());
    assert!(pairs(HIDDEN_MAX + 1).is_err());
}

/// The cut's proofs read the helper's own status and mount list plainly: one thread, no `shared:`
/// or `master:` field on any mount, a mount at each cover, and all five capability sets zero;
/// anything else does not hold.
#[test]
fn the_cut_reads_threads_propagation_covers_and_capabilities_plainly() {
    let zero = "0000000000000000";
    let status = |threads: &str, caps: [&str; 5]| {
        format!(
            "Name:\tcrazyagent-land\nThreads:\t{threads}\nCapInh:\t{}\nCapPrm:\t{}\n\
             CapEff:\t{}\nCapBnd:\t{}\nCapAmb:\t{}\nNoNewPrivs:\t1\n",
            caps[0], caps[1], caps[2], caps[3], caps[4]
        )
    };
    let bare = status("1", [zero; 5]);
    assert!(alone(&bare));
    assert!(!alone(&status("2", [zero; 5])));
    assert!(!alone("Name:\tcrazyagent-land\n"));
    assert!(capabilities_bare(&bare));
    for set in 0..5 {
        let mut caps = [zero; 5];
        caps[set] = "0000000000200000";
        assert!(!capabilities_bare(&status("1", caps)), "set {set}");
    }
    assert!(!capabilities_bare(&status(
        "1",
        [zero, zero, "x", zero, zero]
    )));
    assert!(!capabilities_bare(
        "Threads:\t1\nCapInh:\t0000000000000000\n"
    ));
    let private = "21 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw\n\
                   40 21 0:50 / /home/u/work/private ro,nosuid - tmpfs tmpfs ro\n";
    assert!(propagation_free(private));
    assert!(propagation_free(
        "21 1 8:1 / / rw unbindable - ext4 /dev/sda1 rw\n"
    ));
    for taking in [
        "shared:1",
        "master:1",
        "master:1 propagate_from:2",
        "shared:3 master:1",
    ] {
        let line = format!("21 1 8:1 / / rw,relatime {taking} - ext4 /dev/sda1 rw\n");
        assert!(!propagation_free(&line), "{taking}");
    }
    assert!(!propagation_free(""));
    assert!(!propagation_free(
        "21 1 8:1 / / rw,relatime ext4 /dev/sda1 rw\n"
    ));
    assert!(covered(private, Path::new("/home/u/work/private")));
    assert!(!covered(private, Path::new("/home/u/work")));
    assert!(!covered(private, Path::new("/home/u/work/private/inner")));
    let spaced = "41 21 0:51 / /home/u/my\\040work ro - tmpfs tmpfs ro\n";
    assert!(covered(spaced, Path::new("/home/u/my work")));
    assert!(!covered("garbage\n", Path::new("/")));
}

/// Only a nested run, a probe or the socket filter asks for a private tree, with at most
/// [`HIDDEN_MAX`] distinct clean absolute covers below the root; a cover alone, a flag twice or a
/// nest is refused.
#[test]
fn a_private_tree_comes_with_a_nested_run_a_probe_or_the_filter_and_absolute_covers() {
    let parse = |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
    let covers = Some(vec![PathBuf::from("/w/h")]);
    let run = parse(&[
        READ,
        "/usr",
        NESTED,
        PRIVATE_MOUNTS,
        COVER,
        "/w/h",
        "--",
        "/bin/true",
    ]);
    let Ok(HelperRequest::Run { policy, .. }) = run else {
        panic!("a nested run takes a private tree: {run:?}");
    };
    assert_eq!(policy.cut, covers);
    let probe = |cut: Option<Vec<PathBuf>>| HelperRequest::Probe {
        limit_network: false,
        open_sockets: false,
        keep_free: false,
        cut,
    };
    assert_eq!(
        parse(&[PROBE, PRIVATE_MOUNTS, COVER, "/proc"]),
        Ok(probe(Some(vec![PathBuf::from("/proc")])))
    );
    assert_eq!(parse(&[PROBE, PRIVATE_MOUNTS]), Ok(probe(Some(Vec::new()))));
    assert_eq!(parse(&[PROBE]), Ok(probe(None)));
    assert_eq!(
        parse(&[
            FILTER_SOCKETS,
            PRIVATE_MOUNTS,
            COVER,
            "/w/h",
            "--",
            "/bin/true"
        ]),
        Ok(HelperRequest::Filter {
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
            cut: covers,
            bound: Vec::new(),
        })
    );
    for refused in [
        &[READ, "/usr", PRIVATE_MOUNTS, "--", "/bin/true"][..],
        &[READ, "/usr", NESTED, COVER, "/w/h", "--", "/bin/true"],
        &[PROBE, COVER, "/proc"],
        &[FILTER_SOCKETS, COVER, "/w/h", "--", "/bin/true"],
        &[
            READ,
            "/usr",
            NESTED,
            PRIVATE_MOUNTS,
            COVER,
            "w/h",
            "--",
            "/bin/true",
        ],
        &[
            READ,
            "/usr",
            NESTED,
            PRIVATE_MOUNTS,
            COVER,
            "/",
            "--",
            "/bin/true",
        ],
        &[
            READ,
            "/usr",
            NESTED,
            PRIVATE_MOUNTS,
            COVER,
            "/w/../h",
            "--",
            "/bin/true",
        ],
        &[
            READ,
            "/usr",
            NESTED,
            PRIVATE_MOUNTS,
            PRIVATE_MOUNTS,
            "--",
            "/bin/true",
        ],
        &[
            READ,
            "/usr",
            NESTED,
            PRIVATE_MOUNTS,
            COVER,
            "/w/h",
            COVER,
            "/w/h",
            "--",
            "/bin/true",
        ],
        &[NEST, PRIVATE_MOUNTS, "--", BWRAP_PATHS[0]],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
    let many: Vec<String> = (0..=HIDDEN_MAX).map(|at| format!("/w/h{at}")).collect();
    let mut words = vec![READ, "/usr", NESTED, PRIVATE_MOUNTS];
    for cover in &many {
        words.extend([COVER, cover.as_str()]);
    }
    words.extend(["--", "/bin/true"]);
    assert!(parse(&words).is_err(), "more covers than hidden folders");
}

#[test]
fn a_nested_policy_reads_back_from_its_own_arguments() {
    let policy = LandlockPolicy {
        read: vec![PathBuf::from("/usr")],
        nested: true,
        ..LandlockPolicy::default()
    };
    let mut words = policy.args();
    assert_eq!(words.last(), Some(&OsString::from(NESTED)));
    words.extend(["--".into(), "/bin/true".into()]);
    assert_eq!(
        HelperRequest::parse(words),
        Ok(HelperRequest::Run {
            policy: Box::new(policy),
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
        })
    );
    let parse = |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
    assert!(parse(&[NESTED, DIE_WITH, "4242", "--", "/bin/true"]).is_err());
    assert!(parse(&[PROBE, NESTED]).is_err());
    assert!(parse(&[FILTER_SOCKETS, NESTED, "--", "/bin/true"]).is_err());
}

#[test]
fn the_call_supervisor_probes_stand_alone() {
    let parse = |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
    assert_eq!(
        parse(&[PROBE_SUPERVISOR, "/tmp/probe"]),
        Ok(HelperRequest::Supervisor {
            folder: PathBuf::from("/tmp/probe"),
        })
    );
    assert_eq!(
        parse(&[PROBE_SUPERVISED, "8080", "/tmp/probe"]),
        Ok(HelperRequest::Supervised {
            port: 8080,
            folder: PathBuf::from("/tmp/probe"),
        })
    );
    for refused in [
        &[PROBE_SUPERVISOR][..],
        &[PROBE_SUPERVISOR, "probe"],
        &[PROBE_SUPERVISOR, "/tmp/probe", "more"],
        &[PROBE_SUPERVISOR, GUARD_SIGNALS],
        &[PROBE_SUPERVISOR, "--", "/bin/true"],
        &[PROBE, PROBE_SUPERVISOR],
        &[PROBE_SUPERVISED],
        &[PROBE_SUPERVISED, "8080"],
        &[PROBE_SUPERVISED, "0", "/tmp/probe"],
        &[PROBE_SUPERVISED, "8080", "probe"],
        &[PROBE_SUPERVISED, "8080", "/tmp/probe", "more"],
    ] {
        assert!(parse(refused).is_err(), "{refused:?}");
    }
    let probe = HelperRequest::Supervisor {
        folder: PathBuf::from("/tmp/probe"),
    };
    assert_eq!(Bridge::fits(None, &probe), Ok(()));
}
