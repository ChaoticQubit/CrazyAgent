use crazyagent_win_sandbox::dacl::{ADMINISTRATORS, ALL_APPLICATION_PACKAGES, SYSTEM};
use crazyagent_win_sandbox::private::{Facts, judge};

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

const ALLOW: u8 = 0;
const DENY: u8 = 1;
const OBJECT_ALLOW: u8 = 5;
const INHERITABLE: u8 = 0x03;
const INHERIT_ONLY: u8 = 0x08;
const INHERITED: u8 = 0x10;
const FULL: u32 = 0x001f_01ff;
const GENERIC_READ: u32 = 0x8000_0000;

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

/// The entries a private folder carries (DESIGN-0140 D1): full, inheritable access for the
/// user, SYSTEM and the administrators, plus any extra entries. Input: the extras. Output:
/// the list bytes.
fn private_list(extra: &[(u8, u8, u32, &[u8])]) -> Vec<u8> {
    let mut entries: Vec<(u8, u8, u32, &[u8])> = vec![
        (ALLOW, INHERITABLE, FULL, &USER[..]),
        (ALLOW, INHERITABLE, FULL, &SYSTEM[..]),
        (ALLOW, INHERITABLE, FULL, &ADMINISTRATORS[..]),
    ];
    entries.extend_from_slice(extra);
    acl(&entries)
}

/// What a private object reads as on Windows: the user owns it, it is no reparse point,
/// it has one link and it sits on a local volume. Input: its access list. Output: the facts.
fn facts(list: Option<Vec<u8>>) -> Facts {
    Facts {
        owner: USER.to_vec(),
        list,
        reparse: false,
        links: 1,
        remote: false,
    }
}

/// W1 D1: a list holding only the user, SYSTEM and the administrators is private.
#[test]
fn a_list_of_the_user_system_and_administrators_is_private() {
    assert_eq!(judge(&facts(Some(private_list(&[]))), &USER), Ok(()));
}

/// W1 D1: any other principal with any right, read alone included, makes the object not
/// private: everyone, the users group, authenticated users, every app container, one
/// container and another user. The Unix twin is mode & 077 != 0.
#[test]
fn any_other_principal_with_only_read_is_refused() {
    let others: [&[u8]; 6] = [
        &EVERYONE,
        &USERS,
        &AUTHENTICATED_USERS,
        &ALL_APPLICATION_PACKAGES,
        &CONTAINER,
        &OTHER_USER,
    ];
    for other in others {
        let list = private_list(&[(ALLOW, 0, GENERIC_READ, other)]);
        assert!(
            judge(&facts(Some(list)), &USER).is_err(),
            "an entry for {other:?} must refuse"
        );
    }
}

/// W1 D1: deny entries and inherit-only entries do not apply to the object, so they never
/// refuse it.
#[test]
fn deny_and_inherit_only_entries_do_not_count() {
    let list = private_list(&[
        (DENY, 0, FULL, &EVERYONE[..]),
        (ALLOW, INHERITABLE | INHERIT_ONLY, FULL, &EVERYONE[..]),
    ]);
    assert_eq!(judge(&facts(Some(list)), &USER), Ok(()));
}

/// W1 D2: an inherited entry applies like an explicit one, which is what keeps SQLite's
/// side files honest: they get their entries by inheritance, not by a protected list.
#[test]
fn an_inherited_entry_counts_like_an_explicit_one() {
    let list = private_list(&[(ALLOW, INHERITED, GENERIC_READ, &USERS[..])]);
    assert!(judge(&facts(Some(list)), &USER).is_err());
}

/// W1 D1: an applying allow entry of a type the reader cannot parse is refused, never
/// skipped.
#[test]
fn an_allow_entry_the_reader_cannot_parse_is_refused() {
    let list = private_list(&[(OBJECT_ALLOW, 0, FULL, &USER[..])]);
    assert!(judge(&facts(Some(list)), &USER).is_err());
}

/// W1 D1: the owner is the user, or SYSTEM or the administrators, which own what an
/// elevated process creates; another user's ownership is refused.
#[test]
fn the_owner_must_be_the_user_or_a_trusted_account() {
    for (owner, private) in [
        (&USER[..], true),
        (&SYSTEM[..], true),
        (&ADMINISTRATORS[..], true),
        (&OTHER_USER[..], false),
    ] {
        let mut object = facts(Some(private_list(&[])));
        object.owner = owner.to_vec();
        assert_eq!(judge(&object, &USER).is_ok(), private, "owner {owner:?}");
    }
}

/// W1 D1: an object with no access list grants everyone everything, so it is refused.
#[test]
fn a_missing_access_list_is_refused() {
    assert!(judge(&facts(None), &USER).is_err());
}

/// W1 D1 and D3: a reparse point, a second hard link and a remote volume are each refused,
/// the twins of the Unix no-follow open, `nlink == 1` and a local folder.
#[test]
fn a_reparse_point_a_second_link_or_a_remote_volume_is_refused() {
    let mut reparse = facts(Some(private_list(&[])));
    reparse.reparse = true;
    let mut linked = facts(Some(private_list(&[])));
    linked.links = 2;
    let mut remote = facts(Some(private_list(&[])));
    remote.remote = true;
    for object in [reparse, linked, remote] {
        assert!(judge(&object, &USER).is_err());
    }
}
