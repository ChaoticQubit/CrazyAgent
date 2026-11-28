use super::{ProfileHooks, ProjectSandbox};
use crate::access::AccessHooks;
use crate::error::SandboxError;
use crate::net::{SandboxNetwork, SandboxRemote};
use crate::paths::{any_case, canonical, fold_case};
use crate::policy::AccessGrant;
use crate::ports::REMOTES_MAX;
use crate::profile::consts::{KEPT_OUT_DENY, PROFILE_BYTES_MAX, SBPL_STRING_MAX};
use crate::profile::seatbelt::{denies_follow_write_allows, regex_bodies, root_pin, sbpl_overlong};
use crate::shared_folders::OPEN_TOPS_MAX;
use crate::test_hooks::{ACCESS, STAND_IN};
use std::fs;
use std::path::{Path, PathBuf};

/// Makes fresh project, home and temp folders. Output: their guard and the three canonical
/// folders.
fn folders() -> (tempfile::TempDir, [PathBuf; 3]) {
    let scratch = tempfile::tempdir().unwrap();
    let root = canonical(scratch.path()).unwrap();
    let made = ["project", "home", "tmp"].map(|name| root.join(name));
    for folder in &made {
        std::fs::create_dir_all(folder).unwrap();
    }
    (scratch, made)
}

/// A sandbox the kernel builds on stand-in hooks writes a profile that denies by default,
/// keeps writes off the launch lock those hooks name and parses.
#[test]
fn a_sandbox_on_stand_in_hooks_denies_their_launch_lock_and_parses() {
    let (_scratch, [project, home, tmp]) = folders();
    let sandbox =
        ProjectSandbox::new(&STAND_IN, &project, &home, &tmp, SandboxNetwork::Open).unwrap();
    let profile = sandbox.profile();
    assert!(profile.starts_with("(version 1)\n(deny default)\n"));
    assert!(profile.contains(&any_case("stand-in.lock")), "{profile}");
    assert!(sbpl_overlong(&profile).is_empty());
}

/// A sandbox the kernel builds refuses a remote list past its bound, whatever the hooks.
#[test]
fn a_sandbox_on_stand_in_hooks_refuses_a_remote_list_past_its_bound() {
    let (_scratch, [project, home, tmp]) = folders();
    let remotes = vec![SandboxRemote::AnyHost(443); REMOTES_MAX + 1];
    assert!(
        ProjectSandbox::new(
            &STAND_IN,
            &project,
            &home,
            &tmp,
            SandboxNetwork::Only(remotes)
        )
        .is_err()
    );
}

mod order_tests {
    use super::*;

    /// The order check the profile asserts holds for a profile as the engine writes it, and fails
    /// once a write allow follows a root pin or once a pin is gone, so the profile's assert can
    /// trip.
    #[test]
    fn a_root_pin_a_later_write_allow_overrides_fails_the_order_check() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        let sandbox = ProjectSandbox::new(
            &STAND_IN,
            &root.join("project"),
            &root.join("home"),
            &root.join("tmp"),
            SandboxNetwork::Open,
        )
        .unwrap();
        let pins: Vec<String> = sandbox
            .writable_roots()
            .map(|root| root_pin(root))
            .collect();
        assert_eq!(pins.len(), 3);
        let text = sandbox.profile();
        let allow = format!(
            "(allow file-read* file-write* (subpath \"{}\"))\n",
            root.join("home").display()
        );

        assert!(denies_follow_write_allows(&text, &pins));
        assert!(!denies_follow_write_allows(
            &format!("{text}{allow}"),
            &pins
        ));
        assert!(!denies_follow_write_allows(
            &text.replace(&pins[1], ""),
            &pins
        ));
    }
}

mod access_grant_tests {
    use super::*;

    fn case() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp", "owner-tmp"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        (temp, root)
    }

    /// A socket grant is a connect-only rule ahead of the network section with its write deny after
    /// every write allow, a port grant joins a limited network on loopback, and the temp folder
    /// grant becomes a writable root handed over as TMPDIR with every socket node in it kept
    /// unwritable.
    #[test]
    fn grants_add_a_connect_only_socket_a_loopback_port_and_the_owner_s_temp_folder() {
        let (_temp, root) = case();
        let socket = root.join("owner-tmp").join("discord-ipc-0");
        let owner_tmp = root.join("owner-tmp");
        let sandbox = ProjectSandbox::new(
            &STAND_IN,
            &root.join("project"),
            &root.join("home"),
            &root.join("tmp"),
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        )
        .unwrap()
        .grant_access(
            &[
                AccessGrant::Socket {
                    path: socket.clone(),
                },
                AccessGrant::Port { port: 47_821 },
                AccessGrant::TempFolder,
            ],
            Some(&owner_tmp),
        )
        .unwrap();
        assert_eq!(
            sandbox.network(),
            &SandboxNetwork::Only(vec![
                SandboxRemote::AnyHost(443),
                SandboxRemote::Loopback(47_821)
            ])
        );
        assert_eq!(sandbox.granted_sockets(), std::slice::from_ref(&socket));
        assert_eq!(sandbox.owner_tmp(), Some(owner_tmp.as_path()));
        assert!(sandbox.writes(&owner_tmp.join("note.txt")));
        let text = sandbox.profile();
        let path = socket.display();
        assert!(text.contains(&format!(
            "(allow network-outbound (remote unix-socket (path-literal \"{path}\")))"
        )));
        assert!(!text.contains(&format!(
            "(allow network-bind (local unix-socket (path-literal \"{path}\")))"
        )));
        assert!(text.contains(&format!("(deny file-write* (literal \"{path}\"))")));
        assert!(text.contains("(allow network-outbound (remote ip \"localhost:47821\"))"));
        assert!(text.contains(&format!(
            "(deny file-write*\n  (require-all\n    (subpath \"{}\")\n    (vnode-type SOCKET)))",
            owner_tmp.display()
        )));
        let env = sandbox.base_env(true);
        let tmpdir = env
            .iter()
            .find(|(name, _)| *name == "TMPDIR")
            .map(|(_, value)| value.clone());
        assert_eq!(tmpdir, Some(owner_tmp.clone().into_os_string()));
    }

    /// A grant beyond its kind's bound, a socket in a missing folder, a temp folder grant without
    /// the owner's folder and a port that overfills a limited network are refused; an open network
    /// takes a port grant as it is.
    #[test]
    fn grants_that_cannot_be_held_are_refused() {
        let (_temp, root) = case();
        let fresh = |network| {
            ProjectSandbox::new(
                &STAND_IN,
                &root.join("project"),
                &root.join("home"),
                &root.join("tmp"),
                network,
            )
            .unwrap()
        };
        let socket = |name: &str| AccessGrant::Socket {
            path: root.join("owner-tmp").join(name),
        };
        let only = || SandboxNetwork::Only(Vec::new());
        assert!(
            fresh(only())
                .grant_access(&[socket("a"), socket("b"), socket("c")], None)
                .is_err()
        );
        let missing = AccessGrant::Socket {
            path: root.join("gone").join("x"),
        };
        assert!(fresh(only()).grant_access(&[missing], None).is_err());
        assert!(
            fresh(only())
                .grant_access(&[AccessGrant::TempFolder], None)
                .is_err()
        );
        let full: Vec<SandboxRemote> = (1..=REMOTES_MAX as u16)
            .map(SandboxRemote::AnyHost)
            .collect();
        assert!(
            fresh(SandboxNetwork::Only(full))
                .grant_access(&[AccessGrant::Port { port: 47_821 }], None)
                .is_err()
        );
        let open = fresh(SandboxNetwork::Open)
            .grant_access(&[AccessGrant::Port { port: 47_821 }], None)
            .unwrap();
        assert_eq!(open.network(), &SandboxNetwork::Open);
    }
}

mod profile_string_tests {
    use super::*;

    /// v2.4.0 gate: the folded secret names, 1300 bytes as one regex, split into bodies each
    /// Seatbelt reads, every name kept whole and in order; a list that fits stays one body
    /// in the shape it had, and no names give no body.
    #[test]
    fn regex_bodies_pack_whole_names_under_the_reader_limit() {
        let names: Vec<String> = crate::shared_folders::SECRET_NAMES
            .iter()
            .map(|name| fold_case(name))
            .collect();
        let prefix = format!("^/private/tmp/{}/(.*/)?", "p".repeat(200));
        let bodies = regex_bodies(&prefix, &names, "(/.*)?$");
        assert!(bodies.len() >= 2, "{bodies:#?}");
        assert!(bodies.iter().all(|body| body.len() <= SBPL_STRING_MAX));
        let mut at = 0;
        for body in &bodies {
            let group = &body[prefix.len() + 1..body.len() - "(/.*)?$".len() - 1];
            let start = at;
            while at < names.len() && names[start..=at].join("|").len() <= group.len() {
                at += 1;
            }
            assert_eq!(names[start..at].join("|"), group);
        }
        assert_eq!(at, names.len());
        assert_eq!(
            regex_bodies("^/r/", &["a".into(), "b".into()], "(/.*)?$"),
            vec!["^/r/(a|b)(/.*)?$".to_owned()]
        );
        assert!(regex_bodies("^/r/", &[], "(/.*)?$").is_empty());
    }

    /// The guard names every string past the limit, none in a profile that parses, and an
    /// unclosed quote too.
    #[test]
    fn sbpl_overlong_names_every_string_past_the_reader_limit() {
        let fits = "a".repeat(SBPL_STRING_MAX);
        let long = "b".repeat(SBPL_STRING_MAX + 1);
        let longer = "c".repeat(2 * SBPL_STRING_MAX);
        let text = format!(
            "(deny file-read* (subpath \"{fits}\") (literal \"{long}\") (regex #\"{longer}\"))"
        );
        assert_eq!(sbpl_overlong(&text), vec![long.as_str(), longer.as_str()]);
        assert!(sbpl_overlong(&format!("(literal \"{fits}\")")).is_empty());
        assert_eq!(sbpl_overlong("(literal \"/open"), vec!["/open"]);
    }

    /// A name is spelled with every code point the case-insensitive volume folds onto its letters,
    /// also those for two or three letters (U+00DF and U+1E9E for `ss`, U+FB00 to U+FB06 for `ff`,
    /// `fi`, `fl`, `ffi`, `ffl` and `st`), and the Greek question mark and varia for `;` and the
    /// backtick, and U+0131 for `i` as NTFS maps it. In a pattern a fold reaches into a wildcard
    /// next to the letters, never into a literal or an alternation, so the env templates the secret
    /// rule gives back stay exact.
    #[test]
    fn a_name_is_spelled_with_every_fold_of_the_volume_also_for_two_or_three_letters() {
        let spelled = [
            ("config", "[cC][oO][nN]([fF](i|I|\u{131})|\u{fb01})[gG]"),
            (
                "instructions.md",
                concat!(
                    "(i|I|\u{131})[nN]((s|S|\u{17f})[tT]|\u{fb05}|\u{fb06})",
                    "[rR][uU][cC][tT](i|I|\u{131})[oO][nN](s|S|\u{17f})\\.[mM][dD]",
                ),
            ),
            (
                ".ssh",
                "\\.((s|S|\u{17f})(s|S|\u{17f})|\u{df}|\u{1e9e})[hH]",
            ),
            (
                "ffi",
                "([fF][fF](i|I|\u{131})|[fF]\u{fb01}|\u{fb00}(i|I|\u{131})|\u{fb03})",
            ),
            (".git", "\\.[gG](i|I|\u{131})[tT]"),
            ("k", "(k|K|\u{212a})"),
            (";`", "(;|\u{37e})(`|\u{1fef})"),
        ];
        for (name, pattern) in spelled {
            assert_eq!(any_case(name), pattern, "{name}");
        }
        assert_eq!(
            fold_case(".*service"),
            ".*((s|S|\u{17f})|\u{df}|\u{1e9e})[eE][rR][vV](i|I|\u{131})[cC][eE]"
        );
        assert_eq!(
            fold_case("of.*"),
            "[oO]([fF]|\u{fb00}|\u{fb01}|\u{fb02}|\u{fb03}|\u{fb04}).*"
        );
        assert_eq!(
            fold_case("secrets?"),
            "(s|S|\u{17f})[eE][cC][rR][eE][tT](s|S|\u{17f})?"
        );
        let templates = fold_case(crate::shared_folders::SECRET_TEMPLATES);
        assert!(!templates.contains(['\u{df}', '\u{1e9e}']));
    }

    /// Path text with a long fold run, such as a project folder named with 7 `s` or 10 `f` or a
    /// top-level folder with 60, is spelled with no panic and no runaway list: a stretch past the
    /// spelling limit takes one of its pieces at each place, its fewest pieces' worth of places and
    /// the rest up to one per letter optional, since Seatbelt reads no `{m,n}` count. A pattern
    /// keeps every stretch exact.
    #[test]
    fn a_long_fold_run_in_path_text_is_spelled_in_bounded_length_never_a_panic() {
        let s = "((s|S|\u{17f})|\u{df}|\u{1e9e})";
        assert_eq!(any_case("ssss"), format!("{s}{s}{s}?{s}?"));
        let f = "([fF]|\u{fb00})";
        assert_eq!(any_case("ffffff"), format!("{f}{f}{f}{f}?{f}?{f}?"));
        let root = format!("/Users/o/Glasssssss/{}/{}", "f".repeat(10), "s".repeat(60));
        let spelled = any_case(&root);
        assert!(spelled.len() <= 20 * root.len(), "{}", spelled.len());
        assert!(!spelled.contains('{'), "{spelled}");
        assert!(std::panic::catch_unwind(|| fold_case("sssss")).is_err());
    }

    /// The default secret rule and 64 long open top-level names give a profile with no string
    /// past the limit; a project folder so deep that one secret name no longer fits, and the
    /// most open names at the longest length, which pass the 64 KiB cap, are each refused at
    /// install with `Invalid`, never a panic, before a profile sandbox-exec cannot load is
    /// written.
    #[test]
    fn a_profile_keeps_every_string_seatbelt_reads_or_is_refused_at_install() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (home, tmp, profiles) = (base.join("home"), base.join("tmp"), base.join("sb"));
        let project = base.join("project");
        for folder in [&home, &tmp, &profiles, &project] {
            fs::create_dir_all(folder).unwrap();
        }
        let open: Vec<String> = (0..64)
            .map(|at| format!("{at:02}{}", "n".repeat(40)))
            .collect();
        let wide = ProjectSandbox::new(&STAND_IN, &project, &home, &tmp, SandboxNetwork::Open)
            .unwrap()
            .keep_project_secrets(&project, &[])
            .unwrap()
            .open_only_tops(&open)
            .unwrap();
        let text = wide.profile();
        assert!(sbpl_overlong(&text).is_empty());
        assert!(text.matches("(require-not (regex #\"").count() >= 2);
        assert!(text.matches(KEPT_OUT_DENY).count() >= 3);
        wide.install(&profiles.join("wide.sb")).unwrap();

        let most: Vec<String> = (0..OPEN_TOPS_MAX)
            .map(|at| format!("{at:03}{}", "n".repeat(252)))
            .collect();
        let crowded = ProjectSandbox::new(
            &STAND_IN,
            &project,
            &home,
            &tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .keep_project_secrets(&project, &[])
        .unwrap()
        .open_only_tops(&most)
        .unwrap();
        let text = crowded.profile();
        assert!(sbpl_overlong(&text).is_empty());
        assert!(text.len() > PROFILE_BYTES_MAX);
        let target = profiles.join("crowded.sb");
        match crowded.install(&target) {
            Err(SandboxError::Invalid(reason)) => {
                assert_eq!(reason, "the sandbox profile is larger than its 64 KiB cap")
            }
            other => panic!("{other:?}"),
        }
        assert!(!target.exists());

        let mut deep = base.join("deep");
        while deep.as_os_str().len() < 1000 {
            let room = 1000 - deep.as_os_str().len();
            deep = deep.join("d".repeat(room.clamp(1, 200)));
        }
        fs::create_dir_all(&deep).unwrap();
        let narrow = ProjectSandbox::new(
            &STAND_IN,
            &deep,
            &home,
            &tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .keep_project_secrets(&deep, &[])
        .unwrap();
        assert!(!sbpl_overlong(&narrow.profile()).is_empty());
        let target = profiles.join("deep.sb");
        match narrow.install(&target) {
            Err(SandboxError::Invalid(reason)) => assert_eq!(
                reason,
                "a path or name in the sandbox profile is longer than Seatbelt reads"
            ),
            other => panic!("{other:?}"),
        }
        assert!(!target.exists());
    }
}

mod firmlink_tests {
    use super::*;

    /// A root named through /System/Volumes/Data is refused, since Seatbelt matches only the short
    /// name and every rule written in the long one would never match; an extra read path typed that
    /// way is checked under its short name, so one holding the engine's data is refused.
    #[test]
    fn a_root_named_through_the_data_volume_is_refused_and_a_read_path_is_checked_short() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp", "engine"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        let long = |path: &Path| PathBuf::from(format!("/System/Volumes/Data{}", path.display()));
        assert!(long(&root.join("project")).is_dir());
        let network = || SandboxNetwork::Only(Vec::new());
        let (home, tmp) = (root.join("home"), root.join("tmp"));
        let refused = ProjectSandbox::new(
            &STAND_IN,
            &long(&root.join("project")),
            &home,
            &tmp,
            network(),
        );
        assert!(
            matches!(&refused, Err(SandboxError::Invalid(reason)) if reason.contains("canonical")),
            "{refused:?}"
        );
        assert!(
            ProjectSandbox::new(&STAND_IN, &root.join("project"), &home, &tmp, network()).is_ok()
        );
        assert_eq!(
            crate::extra_paths::checked(
                &ACCESS,
                long(&root).to_str().unwrap(),
                &root.join("engine")
            ),
            Err("read_path_engine_data")
        );
    }
}

/// A run with no warden fires nothing and reports a clean stop.
#[test]
fn a_run_without_a_warden_reports_a_clean_stop() {
    assert_eq!(
        crate::profile::warden::fire_reported(None, "stand-in"),
        Ok(())
    );
}

mod clone_confinement_tests {
    use super::*;

    /// Makes a clone folder with a real `.git` folder in the stand-in project state's
    /// `worktrees/`, as `confine_to_clone` takes one. Input: the project. Output: the clone.
    pub(super) fn clone_in(project: &Path) -> PathBuf {
        let clone = project
            .join(".stand-in-state")
            .join("worktrees")
            .join("agent-1");
        fs::create_dir_all(clone.join(".git")).unwrap();
        canonical(&clone).unwrap()
    }

    /// A clone confinement is refused when the agent's temp folder is the project folder,
    /// since that root would leave the whole project writable beside the clone.
    #[test]
    fn a_clone_confinement_is_refused_when_the_temp_folder_is_the_project() {
        let (_scratch, [project, home, _tmp]) = folders();
        let clone = clone_in(&project);
        let sandbox =
            ProjectSandbox::new(&STAND_IN, &project, &home, &project, SandboxNetwork::Open)
                .unwrap();
        assert!(matches!(
            sandbox.confine_to_clone(&clone),
            Err(SandboxError::Invalid(_))
        ));
    }

    /// A root added after a clone confinement that holds the project, such as the project
    /// folder as an extra worktree, is refused, while disjoint home and temp folders keep
    /// the clone the only writable project path.
    #[test]
    fn a_root_added_after_a_clone_confinement_cannot_reach_the_project() {
        let (_scratch, [project, home, tmp]) = folders();
        let clone = clone_in(&project);
        let confined = ProjectSandbox::new(&STAND_IN, &project, &home, &tmp, SandboxNetwork::Open)
            .unwrap()
            .confine_to_clone(&clone)
            .unwrap();
        let outside: Vec<&PathBuf> = confined
            .writable_roots()
            .filter(|root| {
                (root.starts_with(&project) || project.starts_with(root))
                    && !root.starts_with(&clone)
            })
            .collect();
        assert!(outside.is_empty(), "{outside:?}");
        assert!(matches!(
            confined.allow_worktree(&project),
            Err(SandboxError::Invalid(_))
        ));
    }
}

/// A worktree folder in the hooks' state `worktrees/` as `git worktree add` leaves it,
/// without running git: its `.git` link names an admin folder in the project's
/// `.git/worktrees/`, whose `gitdir` names it back, beside the project's object store.
/// Input: the hooks and the project. Output: the canonical worktree.
fn worktree_in(hooks: &ProfileHooks, project: &Path) -> PathBuf {
    let worktree = (hooks.access.state_folder)(project)
        .join("worktrees")
        .join("agent-1");
    let admin = project.join(".git").join("worktrees").join("agent-1");
    for made in [&worktree, &admin, &project.join(".git").join("objects")] {
        fs::create_dir_all(made).unwrap();
    }
    let worktree = canonical(&worktree).unwrap();
    let admin = canonical(&admin).unwrap();
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    fs::write(
        admin.join("gitdir"),
        format!("{}\n", worktree.join(".git").display()),
    )
    .unwrap();
    worktree
}

/// A sandbox confined to a worktree, and one confined to a clone, each take the owner's
/// Read-write choice of a top-level project folder: that shared writable folder is no root
/// reaching the project beside the confinement, so the confinement check lets it through.
#[test]
fn a_confined_sandbox_takes_a_shared_writable_top_level_folder() {
    for confined in ["worktree", "clone"] {
        let (_scratch, [project, home, tmp]) = folders();
        let data = project.join("data");
        fs::create_dir(&data).unwrap();
        let sandbox =
            ProjectSandbox::new(&STAND_IN, &project, &home, &tmp, SandboxNetwork::Open).unwrap();
        let sandbox = match confined {
            "worktree" => sandbox.confine_to_worktree(&worktree_in(&STAND_IN, &project)),
            "clone" => sandbox.confine_to_clone(&clone_confinement_tests::clone_in(&project)),
            _ => unreachable!(),
        }
        .unwrap()
        .keep_project_secrets(&project, &[])
        .unwrap();
        let shared = sandbox.allow_shared_write(&data);
        assert!(shared.is_ok(), "{confined}: {:?}", shared.err());
        assert!(shared.unwrap().writes(&data), "{confined}");
    }
}

/// The refusal of a shared writable folder that holds or sits in another writable root.
const NOT_OUTSIDE: &str = "a shared writable folder must lie outside every other writable root";

/// Stand-in hooks whose project state folder is the plain top-level folder `agents`, so a
/// worktree or clone sits in a folder the owner could share. No input. Output: the hooks,
/// kept for the rest of the run.
fn plain_state_hooks() -> &'static ProfileHooks {
    let access = Box::leak(Box::new(AccessHooks {
        state_folder: |root| root.join("agents"),
        ..ACCESS
    }));
    Box::leak(Box::new(ProfileHooks { access, ..STAND_IN }))
}

/// A clone folder with a real `.git` folder in the hooks' state `worktrees/`, as
/// `confine_to_clone` takes one. Input: the hooks and the project. Output: the clone.
fn clone_under(hooks: &ProfileHooks, project: &Path) -> PathBuf {
    let clone = (hooks.access.state_folder)(project)
        .join("worktrees")
        .join("agent-1");
    fs::create_dir_all(clone.join(".git")).unwrap();
    canonical(&clone).unwrap()
}

/// An unconfined sandbox takes no shared writable folder: its project folder is already a
/// writable root holding every top-level folder, so a share never comes before a
/// confinement, where the confinement check would let it hold the worktree or clone.
#[test]
fn an_unconfined_sandbox_takes_no_shared_writable_folder() {
    let (_scratch, [project, home, tmp]) = folders();
    let data = project.join("data");
    fs::create_dir(&data).unwrap();
    let sandbox = ProjectSandbox::new(&STAND_IN, &project, &home, &tmp, SandboxNetwork::Open)
        .unwrap()
        .keep_project_secrets(&project, &[])
        .unwrap();
    assert!(matches!(
        sandbox.allow_shared_write(&data),
        Err(SandboxError::Invalid(reason)) if reason == NOT_OUTSIDE
    ));
}

/// A sandbox confined to a worktree, and one confined to a clone, each take no shared
/// writable folder that holds the confinement: a plain top-level folder named `agents`,
/// where these hooks keep the project state.
#[test]
fn a_confined_sandbox_takes_no_shared_writable_folder_holding_its_confinement() {
    let hooks = plain_state_hooks();
    for confined in ["worktree", "clone"] {
        let (_scratch, [project, home, tmp]) = folders();
        let sandbox =
            ProjectSandbox::new(hooks, &project, &home, &tmp, SandboxNetwork::Open).unwrap();
        let sandbox = match confined {
            "worktree" => sandbox.confine_to_worktree(&worktree_in(hooks, &project)),
            "clone" => sandbox.confine_to_clone(&clone_under(hooks, &project)),
            _ => unreachable!(),
        }
        .unwrap()
        .keep_project_secrets(&project, &[])
        .unwrap();
        let agents = canonical(&project.join("agents")).unwrap();
        assert!(
            matches!(
                sandbox.allow_shared_write(&agents),
                Err(SandboxError::Invalid(reason)) if reason == NOT_OUTSIDE
            ),
            "{confined}"
        );
    }
}

/// A link to the engine runtime put inside the project folder, and then the project folder
/// itself swapped for such a link, after the sandbox was built and guarded and before it is
/// installed, still keep the runtime's database out when the agent runs: each read fails inside
/// the sandbox, while the same read succeeds outside it and a project file is read inside it.
#[test]
fn a_link_swapped_in_after_the_build_still_keeps_an_engine_secret_out() {
    let (scratch, [project, home, tmp]) = folders();
    let root = canonical(scratch.path()).unwrap();
    let runtime = root.join("runtime");
    fs::create_dir(&runtime).unwrap();
    let secret = runtime.join("engine.db");
    fs::write(&secret, "engine secret\n").unwrap();
    fs::write(project.join("own.txt"), "own\n").unwrap();
    let sandbox = ProjectSandbox::new(
        &STAND_IN,
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .guard_engine(&runtime)
    .unwrap();
    assert!(sandbox.secret_paths().contains(&secret));
    let profile = root.join("agent.sb");
    let read = |path: &Path| {
        sandbox.install(&profile).unwrap();
        let mut command = sandbox.command(&profile, Path::new("/bin/cat"));
        command.arg(path).current_dir(&home);
        kernel_processes::spawn_gate::collected(&mut command).unwrap()
    };
    let refused = |path: &Path| {
        assert_eq!(fs::read(path).unwrap(), b"engine secret\n");
        let output = read(path);
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"", "{output:?}");
    };
    let own = read(&project.join("own.txt"));
    assert_eq!(own.stdout, b"own\n", "{own:?}");
    std::os::unix::fs::symlink(&runtime, project.join("inner")).unwrap();
    refused(&project.join("inner").join("engine.db"));
    fs::rename(&project, root.join("project.old")).unwrap();
    std::os::unix::fs::symlink(&runtime, &project).unwrap();
    assert!(fs::symlink_metadata(&project).unwrap().is_symlink());
    refused(&project.join("engine.db"));
}

/// Building a sandbox walks each secret, each harness home and the owner home once for its
/// three roots, and each root once, so the walks a build takes are those paths and the three
/// roots.
#[test]
fn a_build_walks_each_secret_once_for_its_three_roots() {
    let (_scratch, [project, home, tmp]) = folders();
    let harnesses = (STAND_IN.access.harness_folders)();
    let owner = crate::paths::owner_home().unwrap();
    let secrets = crate::paths::owner_secrets(&owner, &harnesses).count();
    assert!(secrets > 0);
    let before = crate::paths::walks();
    ProjectSandbox::new(
        &STAND_IN,
        &project,
        &home,
        &tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let walked = crate::paths::walks() - before;
    assert_eq!(walked, secrets + harnesses.len() + 1 + 3, "{walked} walks");
}

/// No agent can make a Unix socket in any engine record folder, which the engine therefore
/// never searches for sockets: inside the guarded sandbox each bind there fails and leaves no
/// socket, while the same script run outside the sandbox binds in each folder, so the row sees
/// a bind when one happens.
#[test]
fn an_agent_cannot_bind_a_unix_socket_in_an_engine_record_folder() {
    use std::os::unix::fs::PermissionsExt;
    let (scratch, [project, home, tmp]) = folders();
    let root = canonical(scratch.path()).unwrap();
    let runtime = root.join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    std::fs::write(runtime.join("engine.db"), "engine secret\n").unwrap();
    let kinds = [
        "agent-skills",
        "agent-instruction-files",
        "agent-instructions-given",
        "agent-home-levels",
        "project-instructions-written",
        "stand-in-records",
    ];
    for kind in kinds {
        let folder = runtime.join(kind);
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let sandbox = ProjectSandbox::new(&STAND_IN, &project, &home, &tmp, SandboxNetwork::Open)
        .unwrap()
        .guard_engine(&runtime)
        .unwrap();
    let profile = root.join("agent.sb");
    sandbox.install(&profile).unwrap();
    let script = "/usr/bin/nc -U -l \"$1\" & p=$!; i=0; \
        while [ $i -lt 30 ] && [ ! -S \"$1\" ] && kill -0 $p 2>/dev/null; do \
        sleep 0.1; i=$((i + 1)); done; \
        kill $p 2>/dev/null; wait $p 2>/dev/null; test -S \"$1\"";
    let bound = |mut command: std::process::Command, socket: &Path| {
        command.args(["-c", script, "sh"]).arg(socket);
        kernel_processes::spawn_gate::collected(&mut command)
            .unwrap()
            .status
            .success()
    };
    for kind in kinds {
        let socket = runtime.join(kind).join("s.sock");
        let inside = sandbox.command(&profile, Path::new("/bin/sh"));
        assert!(!bound(inside, &socket), "{kind}");
        assert!(std::fs::symlink_metadata(&socket).is_err(), "{kind}");
        assert!(
            bound(std::process::Command::new("/bin/sh"), &socket),
            "{kind}"
        );
        std::fs::remove_file(&socket).unwrap();
    }
}
