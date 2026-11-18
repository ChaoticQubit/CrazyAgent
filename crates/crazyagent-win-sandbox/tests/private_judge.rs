use crazyagent_win_sandbox::dacl::{
    self, ADMINISTRATORS, ALL_APPLICATION_PACKAGES, OWNER_RIGHTS, SYSTEM, TRUSTED_INSTALLER,
};
use crazyagent_win_sandbox::private::{Facts, Store, judge, judge_folder, label_list, list};

const USER: [u8; 28] = [
    1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 233, 3, 0, 0,
];
const OTHER_USER: [u8; 28] = [
    1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 234, 3, 0, 0,
];
const EVERYONE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
const USERS: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 33, 2, 0, 0];
const AUTHENTICATED_USERS: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 11, 0, 0, 0];
const CREATOR_OWNER: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0];
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
const NO_READ_UP: u32 = 0x2;
const NO_EXECUTE_UP: u32 = 0x4;
const FULL_POLICY: u32 = NO_WRITE_UP | NO_READ_UP | NO_EXECUTE_UP;
const OBJECT_INHERIT: u8 = 0x01;
const CONTAINER_INHERIT: u8 = 0x02;
const INHERITABLE: u8 = 0x03;
const NO_PROPAGATE: u8 = 0x04;
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

/// What a private object reads as on Windows: the user owns it, it has no mandatory label,
/// it is no reparse point, it has one link and it sits on a local volume. Input: its access
/// list. Output: the facts.
fn facts(list: Option<Vec<u8>>) -> Facts {
    Facts {
        owner: USER.to_vec(),
        list,
        label: None,
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
/// refuse a file; a folder's inherit-only entries are judged by `judge_folder` (row below).
#[test]
fn deny_and_inherit_only_entries_do_not_count() {
    let list = private_list(&[
        (DENY, 0, FULL, &EVERYONE[..]),
        (ALLOW, INHERITABLE | INHERIT_ONLY, FULL, &EVERYONE[..]),
    ]);
    assert_eq!(judge(&facts(Some(list)), &USER), Ok(()));
}

/// #512 r2 (rv-sec2 F1, rv-code4 F2): a child is opened by its own path whatever its folder
/// allows, so a folder handing another account any right on what is made in it, inherit-only
/// included, is not private. CREATOR OWNER and OWNER RIGHTS stand for trusted accounts there,
/// deny entries only take rights away, and an unreadable inheritable allow entry is refused.
#[test]
fn a_folder_handing_another_account_access_to_what_is_made_in_it_is_refused() {
    let handed: [(u8, &[u8]); 3] = [
        (INHERITABLE | INHERIT_ONLY, &EVERYONE),
        (OBJECT_INHERIT | INHERIT_ONLY, &AUTHENTICATED_USERS),
        (CONTAINER_INHERIT | INHERIT_ONLY, &USERS),
    ];
    for (flags, other) in handed {
        let granted = private_list(&[(ALLOW, flags, GENERIC_READ, other)]);
        assert_eq!(judge(&facts(Some(granted.clone())), &USER), Ok(()));
        assert!(
            judge_folder(&facts(Some(granted)), &USER).is_err(),
            "an inheritable entry for {other:?} must refuse a folder"
        );
    }
    let unreadable = private_list(&[(OBJECT_ALLOW, INHERITABLE | INHERIT_ONLY, FULL, &USER[..])]);
    assert!(judge_folder(&facts(Some(unreadable)), &USER).is_err());
    let trusted = private_list(&[
        (ALLOW, INHERITABLE | INHERIT_ONLY, FULL, &CREATOR_OWNER[..]),
        (ALLOW, INHERITABLE | INHERIT_ONLY, FULL, &OWNER_RIGHTS[..]),
        (DENY, INHERITABLE | INHERIT_ONLY, FULL, &EVERYONE[..]),
    ]);
    assert_eq!(judge_folder(&facts(Some(trusted)), &USER), Ok(()));
    assert_eq!(judge_folder(&facts(Some(list(&USER, true))), &USER), Ok(()));
}

/// #512 r2 (rv-sec2 F1), V244-WIN-STORE-LABEL-NX (leader 09:14): a folder whose label entry
/// for what is made in it is below medium, or lacks any of no-write-up, no-read-up and
/// no-execute-up, hands that label to SQLite's side files, so the folder is refused, though
/// its own label (none here) is fine; a high inheritable label with the full policy passes.
#[test]
fn a_folder_handing_a_low_label_to_what_is_made_in_it_is_refused() {
    let mut object = facts(Some(private_list(&[])));
    let handed = |policy: u32, level: &[u8]| {
        Some(acl(&[(LABEL, INHERITABLE | INHERIT_ONLY, policy, level)]))
    };
    object.label = handed(FULL_POLICY, &LOW);
    assert_eq!(judge(&object, &USER), Ok(()));
    assert!(judge_folder(&object, &USER).is_err());
    for policy in [0, NO_WRITE_UP, NO_WRITE_UP | NO_READ_UP] {
        object.label = handed(policy, &MEDIUM);
        assert!(judge_folder(&object, &USER).is_err(), "{policy}");
    }
    object.label = handed(NO_WRITE_UP, &HIGH);
    assert!(judge_folder(&object, &USER).is_err());
    object.label = handed(FULL_POLICY, &HIGH);
    assert_eq!(judge_folder(&object, &USER), Ok(()));
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

/// W1 D1 decision (rv-code4 N1): TrustedInstaller may own a private object and OWNER RIGHTS
/// may hold an entry, as for programs: TrustedInstaller has no more power than SYSTEM, and
/// OWNER RIGHTS names the owner, who is already one of the trusted accounts.
#[test]
fn trusted_installer_as_owner_and_an_owner_rights_entry_are_private() {
    let mut installed = facts(Some(private_list(&[])));
    installed.owner = TRUSTED_INSTALLER.to_vec();
    assert_eq!(judge(&installed, &USER), Ok(()));
    let owner_rights = facts(Some(private_list(&[(ALLOW, 0, FULL, &OWNER_RIGHTS[..])])));
    assert_eq!(judge(&owner_rights, &USER), Ok(()));
}

/// W1 D1 (rv-sec3 N4), V244-WIN-STORE-LABEL-NX (leader 09:14): a mandatory label below
/// medium, or one that lets a lower integrity process write, read or execute up, is refused,
/// as #562 F1 saw such a process hold a file opened for execute only; no label, which
/// Windows reads as medium, and a medium or high label with the full policy are private.
#[test]
fn a_label_that_lets_a_lower_integrity_process_write_is_refused() {
    for (level, policy, private) in [
        (&LOW, FULL_POLICY, false),
        (&MEDIUM, 0, false),
        (&HIGH, 0, false),
        (&MEDIUM, NO_WRITE_UP, false),
        (&MEDIUM, NO_WRITE_UP | NO_READ_UP, false),
        (&MEDIUM, NO_WRITE_UP | NO_EXECUTE_UP, false),
        (&HIGH, NO_WRITE_UP, false),
        (&MEDIUM, FULL_POLICY, true),
        (&HIGH, FULL_POLICY, true),
    ] {
        let mut object = facts(Some(private_list(&[])));
        object.label = Some(acl(&[(LABEL, 0, policy, &level[..])]));
        assert_eq!(judge(&object, &USER).is_ok(), private, "{level:?} {policy}");
    }
    let mut inherit_only = facts(Some(private_list(&[])));
    inherit_only.label = Some(acl(&[(
        LABEL,
        INHERITABLE | INHERIT_ONLY,
        NO_WRITE_UP,
        &LOW[..],
    )]));
    assert_eq!(judge(&inherit_only, &USER), Ok(()));
}

/// W1 D1: the list a new private folder or file gets holds full access for exactly the
/// user, SYSTEM and the administrators, inheritable only on a folder, and is judged private.
#[test]
fn the_planned_list_is_private_and_names_only_the_trusted_accounts() {
    for (folder, flags) in [(true, INHERITABLE), (false, 0)] {
        let planned = list(&USER, folder);
        let found = dacl::entries(&planned).unwrap();
        let sids: Vec<&[u8]> = found.iter().map(|entry| entry.sid).collect();
        assert_eq!(sids, [&USER[..], &SYSTEM[..], &ADMINISTRATORS[..]]);
        for entry in &found {
            assert!(
                entry.allows && entry.mask == FULL && entry.flags == flags,
                "{entry:?}"
            );
        }
        assert_eq!(judge(&facts(Some(planned)), &USER), Ok(()));
    }
}

/// #524 r3 (rv-code4 F2, leader 05:45), V244-WIN-STORE-LABEL-NX: only the stores carry the
/// medium label with no-write-up, no-read-up and no-execute-up. A secret folder labels itself
/// and all made in it; the database folder, the data root, labels only the files made directly
/// in it, never itself or a folder below, which may be a root granted to an AppContainer.
#[test]
fn a_store_label_reaches_only_what_the_store_holds() {
    let both = Ok(Some((0x2000, FULL_POLICY)));
    let mut secret = facts(Some(list(&USER, true)));
    secret.label = Some(label_list(Store::Secret));
    let sacl = secret.label.as_deref().unwrap();
    assert_eq!(sacl[9], INHERITABLE);
    assert_eq!(dacl::label(sacl), both);
    assert_eq!(dacl::inherited_label(sacl), both);
    assert_eq!(judge_folder(&secret, &USER), Ok(()));
    let mut database = facts(Some(list(&USER, true)));
    database.label = Some(label_list(Store::Database));
    let sacl = database.label.as_deref().unwrap();
    assert_eq!(sacl[9], OBJECT_INHERIT | NO_PROPAGATE | INHERIT_ONLY);
    assert_eq!(dacl::label(sacl), Ok(None));
    assert_eq!(dacl::inherited_label(sacl), both);
    assert_eq!(judge_folder(&database, &USER), Ok(()));
}

/// #568 rv-code F1 (leader 09:19): two mandatory labels that both apply to the object, as an
/// own label beside one a folder handed down, or that both reach what a folder makes, are
/// refused, not read as the first, since the reader cannot tell which one Windows enforces.
/// One own label and one handed-down label on a folder read apart.
#[test]
fn a_second_label_entry_is_refused_not_skipped() {
    let own = (LABEL, 0, FULL_POLICY, &MEDIUM[..]);
    let inherited = (LABEL, INHERITED, NO_WRITE_UP | NO_READ_UP, &MEDIUM[..]);
    let two = acl(&[own, inherited]);
    assert!(dacl::label(&two).is_err());
    let mut object = facts(Some(private_list(&[])));
    object.label = Some(two);
    assert!(judge(&object, &USER).is_err());
    let handed = acl(&[
        (LABEL, INHERITABLE, FULL_POLICY, &MEDIUM[..]),
        (
            LABEL,
            OBJECT_INHERIT | INHERIT_ONLY,
            NO_WRITE_UP,
            &MEDIUM[..],
        ),
    ]);
    assert!(dacl::inherited_label(&handed).is_err());
    let mut folder = facts(Some(private_list(&[])));
    folder.label = Some(handed);
    assert!(judge_folder(&folder, &USER).is_err());
    let apart = acl(&[
        own,
        (LABEL, INHERITABLE | INHERIT_ONLY, FULL_POLICY, &MEDIUM[..]),
    ]);
    let full = Ok(Some((0x2000, FULL_POLICY)));
    assert_eq!(dacl::label(&apart), full);
    assert_eq!(dacl::inherited_label(&apart), full);
}
