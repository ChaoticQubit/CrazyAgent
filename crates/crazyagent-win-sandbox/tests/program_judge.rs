use crazyagent_win_sandbox::dacl::{
    ADMINISTRATORS, ALL_APPLICATION_PACKAGES, OWNER_RIGHTS, SYSTEM, TRUSTED_INSTALLER,
};
use crazyagent_win_sandbox::private::{Facts, judge_program};

const USER: [u8; 28] = [
    1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 233, 3, 0, 0,
];
const OTHER_USER: [u8; 28] = [
    1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 234, 3, 0, 0,
];
const EVERYONE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
const USERS: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 33, 2, 0, 0];
const AUTHENTICATED_USERS: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 11, 0, 0, 0];
const CONTAINER: [u8; 40] = [
    1, 8, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 5, 0, 0,
    0, 6, 0, 0, 0, 7, 0, 0, 0,
];
const LOW: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 16, 0, 0x10, 0, 0];
const MEDIUM: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 16, 0, 0x20, 0, 0];
const HIGH: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 16, 0, 0x30, 0, 0];

const ALLOW: u8 = 0;
const DENY: u8 = 1;
const OBJECT_ALLOW: u8 = 5;
const LABEL: u8 = 0x11;
const NO_WRITE_UP: u32 = 0x1;
const INHERITABLE: u8 = 0x03;
const INHERIT_ONLY: u8 = 0x08;
const FULL: u32 = 0x001f_01ff;
const READ_EXECUTE: u32 = 0x0012_00a9;
const CHANGES: [u32; 9] = [
    0x0000_0002,
    0x0000_0004,
    0x0000_0010,
    0x0000_0100,
    0x0001_0000,
    0x0004_0000,
    0x0008_0000,
    0x4000_0000,
    0x1000_0000,
];

/// Builds an access list in the byte layout Windows copies out: the eight-byte header, then
/// each entry's type, flags, size, mask and SID. Input: (type, flags, mask, SID) per entry.
/// Output: the list bytes.
fn acl(entries: &[(u8, u8, u32, &[u8])]) -> Vec<u8> {
    let size = 8 + entries.iter().map(|entry| 8 + entry.3.len()).sum::<usize>();
    let mut bytes = vec![2, 0];
    bytes.extend((size as u16).to_le_bytes());
    bytes.extend((entries.len() as u16).to_le_bytes());
    bytes.extend([0, 0]);
    for (kind, flags, mask, sid) in entries {
        bytes.extend([*kind, *flags]);
        bytes.extend(((8 + sid.len()) as u16).to_le_bytes());
        bytes.extend(mask.to_le_bytes());
        bytes.extend_from_slice(sid);
    }
    assert_eq!(bytes.len(), size);
    bytes
}

/// The list a program a package manager installed usually carries: full access for the
/// user, SYSTEM and the administrators, read and run for the users group and every app
/// container, plus any extra entries. Input: the extras. Output: the list bytes.
fn program_list(extra: &[(u8, u8, u32, &[u8])]) -> Vec<u8> {
    let mut entries: Vec<(u8, u8, u32, &[u8])> = vec![
        (ALLOW, 0, FULL, &USER[..]),
        (ALLOW, 0, FULL, &SYSTEM[..]),
        (ALLOW, 0, FULL, &ADMINISTRATORS[..]),
        (ALLOW, 0, READ_EXECUTE, &USERS[..]),
        (ALLOW, 0, READ_EXECUTE, &ALL_APPLICATION_PACKAGES[..]),
    ];
    entries.extend_from_slice(extra);
    acl(&entries)
}

/// What a plain program reads as: the user owns it, it has no label, it is no reparse
/// point, it has one name and it sits on a local volume. Input: its access list. Output:
/// the facts.
fn program(list: Option<Vec<u8>>) -> Facts {
    Facts {
        owner: USER.to_vec(),
        list,
        label: None,
        reparse: false,
        links: 1,
        remote: false,
    }
}

/// A program everyone may read and run passes while only the trusted accounts may change
/// it, owned by the user, SYSTEM, the administrators or TrustedInstaller; an OWNER RIGHTS
/// entry names the owner, who is already trusted.
#[test]
fn a_program_only_the_trusted_accounts_may_change_passes() {
    for owner in [&USER[..], &SYSTEM, &ADMINISTRATORS, &TRUSTED_INSTALLER] {
        let mut owned = program(Some(program_list(&[])));
        owned.owner = owner.to_vec();
        assert_eq!(judge_program(&owned, &USER), Ok(()), "{owner:?}");
    }
    let owner_rights = program(Some(program_list(&[(ALLOW, 0, FULL, &OWNER_RIGHTS[..])])));
    assert_eq!(judge_program(&owner_rights, &USER), Ok(()));
}

/// Any other account given any right that changes the program, its data, attributes,
/// deletion, list or owner, generic write or all, is refused: everyone, the users group,
/// authenticated users, every app container, one container and another user. Another
/// owner and a missing list are refused too, as a missing list lets everyone change it.
#[test]
fn another_account_that_may_change_a_program_is_refused() {
    let others: [&[u8]; 6] = [
        &EVERYONE,
        &USERS,
        &AUTHENTICATED_USERS,
        &ALL_APPLICATION_PACKAGES,
        &CONTAINER,
        &OTHER_USER,
    ];
    for other in others {
        for right in CHANGES {
            let list = program_list(&[(ALLOW, 0, right, other)]);
            assert!(
                judge_program(&program(Some(list)), &USER).is_err(),
                "{other:?} {right:#x}"
            );
        }
    }
    let mut foreign = program(Some(program_list(&[])));
    foreign.owner = OTHER_USER.to_vec();
    assert!(judge_program(&foreign, &USER).is_err());
    assert!(judge_program(&program(None), &USER).is_err());
}

/// A program that is a link or a junction, has a second name or sits on a network volume
/// is refused. These are the twins of the Unix regular-file and link-count checks, read
/// from the handle's own facts.
#[test]
fn a_linked_second_named_or_network_program_is_refused() {
    let mut linked = program(Some(program_list(&[])));
    linked.reparse = true;
    assert!(judge_program(&linked, &USER).is_err());
    let mut named_twice = program(Some(program_list(&[])));
    named_twice.links = 2;
    assert!(judge_program(&named_twice, &USER).is_err());
    let mut remote = program(Some(program_list(&[])));
    remote.remote = true;
    assert!(judge_program(&remote, &USER).is_err());
}

/// A deny entry and an inherit-only allow entry do not apply to the program, so they never
/// refuse it. An allow entry that applies and that the reader cannot parse is refused, as
/// the check cannot tell what it grants.
#[test]
fn only_allow_entries_that_apply_to_the_program_count() {
    let deny = program_list(&[(DENY, 0, FULL, &EVERYONE[..])]);
    assert_eq!(judge_program(&program(Some(deny)), &USER), Ok(()));
    let inherit_only = program_list(&[(ALLOW, INHERITABLE | INHERIT_ONLY, FULL, &USERS[..])]);
    assert_eq!(judge_program(&program(Some(inherit_only)), &USER), Ok(()));
    let unreadable = program_list(&[(OBJECT_ALLOW, 0, FULL, &USERS[..])]);
    assert!(judge_program(&program(Some(unreadable)), &USER).is_err());
}

/// A label below medium, or one without no-write-up, lets a lower integrity process write
/// the program and is refused; no label, which Windows reads as medium, and a medium or high
/// label with no-write-up pass, as running a program needs no read or execute policy.
#[test]
fn a_label_that_lets_a_lower_integrity_process_write_a_program_is_refused() {
    for (level, policy, passes) in [
        (&LOW, NO_WRITE_UP, false),
        (&MEDIUM, 0, false),
        (&HIGH, 0, false),
        (&MEDIUM, NO_WRITE_UP, true),
        (&HIGH, NO_WRITE_UP, true),
    ] {
        let mut labelled = program(Some(program_list(&[])));
        labelled.label = Some(acl(&[(LABEL, 0, policy, &level[..])]));
        assert_eq!(
            judge_program(&labelled, &USER).is_ok(),
            passes,
            "{level:?} {policy}"
        );
    }
    let mut inherit_only = program(Some(program_list(&[])));
    inherit_only.label = Some(acl(&[(
        LABEL,
        INHERITABLE | INHERIT_ONLY,
        NO_WRITE_UP,
        &LOW[..],
    )]));
    assert_eq!(judge_program(&inherit_only, &USER), Ok(()));
}

/// The system's own programs carry a second name in its component store, so a program
/// TrustedInstaller owns may have more than one name while its list still gives no one else
/// a right to change it. A program the user, SYSTEM or the administrators own keeps one.
#[test]
fn only_a_trusted_installer_program_may_have_more_than_one_name() {
    let mut inbox = program(Some(program_list(&[])));
    inbox.owner = TRUSTED_INSTALLER.to_vec();
    inbox.links = 2;
    assert_eq!(judge_program(&inbox, &USER), Ok(()));
    let mut writable = inbox.clone();
    writable.list = Some(program_list(&[(ALLOW, 0, CHANGES[0], &USERS[..])]));
    assert!(judge_program(&writable, &USER).is_err());
    for owner in [&USER[..], &SYSTEM, &ADMINISTRATORS] {
        let mut named_twice = program(Some(program_list(&[])));
        named_twice.owner = owner.to_vec();
        named_twice.links = 2;
        assert!(judge_program(&named_twice, &USER).is_err(), "{owner:?}");
    }
}
