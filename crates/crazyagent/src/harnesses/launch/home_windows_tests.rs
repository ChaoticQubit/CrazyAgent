use super::low_probe::{Try, low_probe, probe};
use super::{judge_home, validate_home, validate_home_for};
use crate::sandbox::windows::{HELPER_NAME, WindowsSandbox};
use crate::sandbox::{ProjectSandbox, Sandbox, SandboxNetwork};
use crazyagent_win_sandbox::dacl;
use crazyagent_win_sandbox::private::{self, Facts};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ALLOW: u8 = 0;
const DENY: u8 = 1;
const ALLOW_CALLBACK: u8 = 9;
const FOLDER_INHERIT: u8 = 0x03;
const INHERIT_ONLY: u8 = 0x08;
const FULL: u32 = 0x001f_01ff;
const MEDIUM: u32 = 0x2000;
const LOW: u32 = 0x1000;
const NO_WRITE_UP: u32 = 0x1;
const CONTAINER: &str = "S-1-15-2-11-12-13-14-15-16-17";

/// A SID's bytes from its authority and sub-authorities, as Windows lays them out.
/// Inputs: the last authority byte and the sub-authorities. Output: the bytes.
fn sid(authority: u8, subs: &[u32]) -> Vec<u8> {
    let mut bytes = vec![
        1,
        u8::try_from(subs.len()).unwrap(),
        0,
        0,
        0,
        0,
        0,
        authority,
    ];
    for sub in subs {
        bytes.extend_from_slice(&sub.to_le_bytes());
    }
    bytes
}

fn user() -> Vec<u8> {
    sid(5, &[21, 1, 2, 3, 1000])
}

fn container() -> Vec<u8> {
    sid(15, &[2, 11, 12, 13, 14, 15, 16, 17])
}

/// An access list of entries given as type, flags, rights and SID, in the layout Windows
/// reads, so a row can hold any entry type. Input: the entries. Output: the list bytes.
fn list(entries: &[(u8, u8, u32, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (kind, flags, mask, sid) in entries {
        body.push(*kind);
        body.push(*flags);
        body.extend_from_slice(&u16::try_from(8 + sid.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&mask.to_le_bytes());
        body.extend_from_slice(sid);
    }
    let mut bytes = vec![2, 0];
    bytes.extend_from_slice(&u16::try_from(8 + body.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&u16::try_from(entries.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend(body);
    bytes
}

/// The list `private::dir` gives a home, the user, SYSTEM and the administrators, plus one
/// more entry. Inputs: the user and the extra entry. Output: the list bytes.
fn private_with(user: &[u8], extra: (u8, u8, u32, &[u8])) -> Vec<u8> {
    list(&[
        (ALLOW, FOLDER_INHERIT, FULL, user),
        (ALLOW, FOLDER_INHERIT, FULL, &dacl::SYSTEM),
        (ALLOW, FOLDER_INHERIT, FULL, &dacl::ADMINISTRATORS),
        extra,
    ])
}

/// The facts of a local, unlinked folder the user owns, with that list and label.
fn facts(user: &[u8], list: Vec<u8>, label: Option<Vec<u8>>) -> Facts {
    Facts {
        owner: user.to_vec(),
        list: Some(list),
        label,
        reparse: false,
        links: 1,
        remote: false,
    }
}

/// H1 row 1: the home judge passes the user, SYSTEM, the administrators, OWNER RIGHTS and a
/// container, and refuses every other grantee, an inherit-only one included, as the files
/// made in the home inherit it; deny entries only take rights away.
#[test]
fn the_home_judge_passes_only_the_user_the_system_owner_rights_and_containers() {
    let user = user();
    let container = container();
    for passing in [&dacl::OWNER_RIGHTS[..], &container[..], &dacl::SYSTEM[..]] {
        let home = facts(&user, private_with(&user, (ALLOW, 0, FULL, passing)), None);
        assert_eq!(judge_home(&home, &user), Ok(()), "{passing:?}");
    }
    let everyone = sid(1, &[0]);
    let denied = facts(&user, private_with(&user, (DENY, 0, FULL, &everyone)), None);
    assert_eq!(judge_home(&denied, &user), Ok(()));
    let refused = [
        everyone.clone(),
        sid(5, &[32, 545]),
        sid(5, &[11]),
        sid(5, &[21, 1, 2, 3, 1001]),
        dacl::ALL_APPLICATION_PACKAGES.to_vec(),
        sid(15, &[2, 2]),
        sid(15, &[3, 1]),
        sid(15, &[2, 11, 12, 13, 14, 15, 16]),
    ];
    for grantee in &refused {
        for flags in [0, FOLDER_INHERIT | INHERIT_ONLY] {
            let home = facts(
                &user,
                private_with(&user, (ALLOW, flags, 0x1, grantee)),
                None,
            );
            assert!(judge_home(&home, &user).is_err(), "{grantee:?} {flags}");
        }
    }
    let unread = facts(
        &user,
        private_with(&user, (ALLOW_CALLBACK, 0, FULL, &container)),
        None,
    );
    assert!(judge_home(&unread, &user).is_err());
}

/// H1 row 2: the home judge refuses a link, a network volume, a foreign owner, a missing
/// list and a label a lower integrity process may write through, and passes the medium
/// no-write-up label a folder has when it carries none.
#[test]
fn the_home_judge_refuses_a_link_a_share_a_foreign_owner_and_a_low_label() {
    let user = user();
    let plain = || facts(&user, private_with(&user, (ALLOW, 0, FULL, &user)), None);
    assert_eq!(judge_home(&plain(), &user), Ok(()));
    let medium = Some(dacl::labelled(MEDIUM, NO_WRITE_UP, FOLDER_INHERIT));
    assert_eq!(
        judge_home(
            &Facts {
                label: medium,
                ..plain()
            },
            &user
        ),
        Ok(())
    );
    let low = Some(dacl::labelled(LOW, NO_WRITE_UP, FOLDER_INHERIT));
    let open_medium = Some(dacl::labelled(MEDIUM, 0, FOLDER_INHERIT));
    for home in [
        Facts {
            reparse: true,
            ..plain()
        },
        Facts {
            remote: true,
            ..plain()
        },
        Facts {
            owner: sid(5, &[21, 1, 2, 3, 1001]),
            ..plain()
        },
        Facts {
            list: None,
            ..plain()
        },
        Facts {
            label: low,
            ..plain()
        },
        Facts {
            label: open_medium,
            ..plain()
        },
    ] {
        assert!(judge_home(&home, &user).is_err(), "{home:?}");
    }
}

fn run(program: &str, arguments: &[&str]) {
    let status = crate::spawn_gate::status(
        Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(status.success(), "{program} {arguments:?}");
}

fn code(home: &Path) -> String {
    validate_home(home).unwrap_err().0
}

/// H1 row 3: a W1 private home passes, also once a container SID holds modify on it; a home
/// open to users, to ALL APPLICATION PACKAGES, a junction, a file or a relative path is
/// unsafe, and a missing one is unavailable.
#[test]
fn a_private_home_passes_and_an_open_linked_or_relative_one_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(temp.path()).unwrap();
    let home = base.join("home");
    drop(private::dir(&home).unwrap());
    validate_home(&home).unwrap();
    let grant = format!("*{CONTAINER}:(OI)(CI)(M)");
    run(
        "icacls",
        &[home.to_str().unwrap(), "/grant", grant.as_str()],
    );
    validate_home(&home).unwrap();
    for (name, grantee) in [("users", "*S-1-5-32-545"), ("packages", "*S-1-15-2-1")] {
        let open = base.join(name);
        drop(private::dir(&open).unwrap());
        let grant = format!("{grantee}:(OI)(CI)(RX)");
        run(
            "icacls",
            &[open.to_str().unwrap(), "/grant", grant.as_str()],
        );
        assert_eq!(code(&open), "launch_account_home_unsafe", "{name}");
    }
    let linked = base.join("linked");
    run(
        "cmd",
        &[
            "/C",
            "mklink",
            "/J",
            linked.to_str().unwrap(),
            home.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&linked), "launch_account_home_unsafe");
    let file = base.join("file");
    std::fs::write(&file, "x").unwrap();
    assert_eq!(code(&file), "launch_account_home_unsafe");
    assert_eq!(code(Path::new("home")), "launch_account_home_unsafe");
    assert_eq!(
        code(&base.join("missing")),
        "launch_account_home_unavailable"
    );
}

/// H1 row 4: the owner's profile folder and its `.codex` and `.claude` folders are forbidden
/// homes in any letter case, while another private folder in the profile passes.
#[test]
fn the_profile_and_its_codex_and_claude_folders_are_forbidden_homes() {
    let temp = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(temp.path()).unwrap();
    let owner = base.join("profile");
    drop(private::dir(&owner).unwrap());
    for name in [".codex", ".claude", "agent"] {
        drop(private::dir(&owner.join(name)).unwrap());
    }
    for forbidden in [
        owner.clone(),
        owner.join(".codex"),
        owner.join(".claude"),
        owner.join(".CODEX"),
        base.join("PROFILE").join(".Claude"),
    ] {
        assert_eq!(
            validate_home_for(&forbidden, Some(&owner)).unwrap_err().0,
            "launch_account_home_forbidden",
            "{}",
            forbidden.display()
        );
    }
    validate_home_for(&owner.join("agent"), Some(&owner)).unwrap();
    validate_home_for(&owner.join("agent"), None).unwrap();
    assert!(validate_home(&std::env::home_dir().unwrap()).is_err());
}

/// The boundary helper the build puts beside the test binary's folder. Output: its path.
fn helper() -> PathBuf {
    let tests = std::env::current_exe().unwrap();
    let helper = tests.parent().unwrap().with_file_name(HELPER_NAME);
    assert!(
        helper.is_file(),
        "build crazyagent-win-boundary first: {}",
        helper.display()
    );
    helper
}

/// Runs one `cmd /C` line inside the agent's box. Inputs: the installed sandbox, the project
/// folder and the line. Output: whether it succeeded.
fn boxed(installed: &WindowsSandbox, project: &Path, line: &str) -> bool {
    let shell = PathBuf::from(std::env::var("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe");
    let mut command = Command::new(shell);
    command.arg("/C").arg(line).current_dir(project);
    crate::spawn_gate::status(
        Sandbox::wrap(installed, &command)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap()
    .success()
}

/// The access list of a folder, read through a no-follow open that W1's private judge does
/// not gate, since a granted home holds the container's entry. Input: the folder. Output:
/// the list bytes.
fn home_list(path: &Path) -> Vec<u8> {
    use std::os::windows::fs::OpenOptionsExt;
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .custom_flags(
            crate::platform::FILE_FLAG_BACKUP_SEMANTICS
                | crate::platform::FILE_FLAG_OPEN_REPARSE_POINT,
        )
        .open(path)
        .unwrap();
    private::facts(&held).unwrap().list.unwrap()
}

/// H1 row 5 (rv-sec2 #710 Q8 condition 1): a home the boundary granted its container still
/// passes, the container writes a file in it, and a plain low integrity process of the same
/// user can neither create a file there nor change the container's, while it writes in a
/// folder labelled low.
#[test]
fn a_granted_home_passes_and_a_low_integrity_process_cannot_change_it() {
    let temp = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(temp.path()).unwrap();
    let project = base.join("project");
    let home = base.join("agent").join("home");
    let tmp = base.join("agent").join("tmp");
    let profiles = base.join("profiles");
    let grants = base.join("grants");
    for folder in [&project, &base.join("agent"), &profiles, &grants] {
        std::fs::create_dir_all(folder).unwrap();
    }
    drop(private::dir(&home).unwrap());
    drop(private::dir(&tmp).unwrap());
    let sandbox =
        ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
    let installed =
        WindowsSandbox::install(sandbox, &helper(), &profiles.join("agent.sb"), &grants).unwrap();
    let written = home.join("boxed.txt");
    let named = written.to_str().unwrap();
    assert!(
        !named.contains(' '),
        "std quotes a path with a space for cmd: {named}"
    );
    let line = format!("echo boxed>{named}");
    assert!(
        boxed(&installed, &project, &line),
        "the box writes its home"
    );
    assert!(written.is_file());
    validate_home(&home).unwrap();
    let granted = home_list(&home);
    assert!(
        dacl::entries(&granted)
            .unwrap()
            .iter()
            .any(|entry| entry.allows && dacl::package(entry.sid)),
        "the launch granted the home to the container"
    );
    let open = base.join("low-folder");
    std::fs::create_dir(&open).unwrap();
    run(
        "icacls",
        &[open.to_str().unwrap(), "/setintegritylevel", "(OI)(CI)low"],
    );
    let created = home.join("low.txt");
    let beside = open.join("low.txt");
    let low = low_probe(&base);
    let seen = probe(
        &low,
        &[
            (Try::Write, &created),
            (Try::Write, &written),
            (Try::Write, &beside),
        ],
    );
    assert!(
        seen.contains(&format!("REFUSED {}", created.display())),
        "{seen}"
    );
    assert!(
        seen.contains(&format!("REFUSED {}", written.display())),
        "{seen}"
    );
    assert!(
        seen.contains(&format!("WROTE {}", beside.display())),
        "{seen}"
    );
    assert!(!created.exists());
    assert_eq!(std::fs::read_to_string(&written).unwrap().trim(), "boxed");
}

/// Makes a junction at `link` to `target`, or points an existing one at `target` instead.
/// Inputs: the link and the target.
fn junction(link: &Path, target: &Path) {
    if std::fs::symlink_metadata(link).is_ok() {
        run("cmd", &["/C", "rmdir", link.to_str().unwrap()]);
    }
    run(
        "cmd",
        &[
            "/C",
            "mklink",
            "/J",
            link.to_str().unwrap(),
            target.to_str().unwrap(),
        ],
    );
}

/// V244-WIN-MASTER S9a r3 (CodeRabbit CR-1252 2): H1 judges the home a launch holds, not the
/// folder its path reaches later. A home spelled through a junction is held while the
/// junction points at a folder open to users, the junction is then pointed at a private
/// folder, and the held home is still refused as `launch_account_home_unsafe`.
#[test]
fn a_held_home_is_judged_as_held_after_a_junction_above_it_moves() {
    let temp = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(temp.path()).unwrap();
    let (first, second) = (base.join("first"), base.join("second"));
    for parent in [&first, &second] {
        drop(private::dir(parent).unwrap());
        drop(private::dir(&parent.join("home")).unwrap());
    }
    run(
        "icacls",
        &[
            first.join("home").to_str().unwrap(),
            "/grant",
            "*S-1-5-32-545:(OI)(CI)(RX)",
        ],
    );
    let alias = base.join("alias");
    junction(&alias, &first);
    let home = alias.join("home");
    let held = super::hold_home(&home).unwrap();
    junction(&alias, &second);
    validate_home(&home).unwrap();
    assert_eq!(
        super::validate_held_for(&home, &held, None).unwrap_err().0,
        "launch_account_home_unsafe"
    );
}

/// V244-WIN-MASTER S9a r3 (CodeRabbit CR-1252 2): the owner's `.codex` stays a forbidden
/// home when it is held through a junction that then points elsewhere, since H1 compares
/// the held folder itself with the profile's folders, not the name its path reaches now.
#[test]
fn a_held_profile_folder_stays_forbidden_after_a_junction_above_it_moves() {
    let temp = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(temp.path()).unwrap();
    let owner = base.join("profile");
    let other = base.join("other");
    for folder in [&owner, &other] {
        drop(private::dir(folder).unwrap());
        drop(private::dir(&folder.join(".codex")).unwrap());
    }
    let alias = base.join("alias");
    junction(&alias, &owner);
    let home = alias.join(".codex");
    let held = super::hold_home(&home).unwrap();
    junction(&alias, &other);
    validate_home_for(&home, Some(&owner)).unwrap();
    assert_eq!(
        super::validate_held_for(&home, &held, Some(&owner))
            .unwrap_err()
            .0,
        "launch_account_home_forbidden"
    );
}
