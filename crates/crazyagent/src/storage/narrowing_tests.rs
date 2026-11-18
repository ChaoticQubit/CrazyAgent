use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

/// A private engine root of this user, as storage opens one.
/// Output: the folder, removed when dropped.
fn root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-narrow-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

/// A revocation of the TUI client at `epoch`, for the database `db_instance`.
/// Inputs: the operation name and the epoch. Output: the intent.
fn revocation(name: &str, epoch: i64) -> Intent {
    Intent {
        db_instance: "a".repeat(64),
        change: Change::Revoke {
            operation_id: RequestId::new(name).unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: epoch,
        },
    }
}

/// The intent file names in the folder, sorted. Input: the engine root. Output: the names.
fn names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(FOLDER))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Writes raw bytes as an intent file the way a hand or another program would.
/// Inputs: the engine root, the name, the bytes and the mode. Output: the path.
fn plant(root: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
    let path = root.join(FOLDER).join(name);
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[test]
fn a_written_intent_is_a_private_file_listed_once_and_gone_after_its_removal() {
    let root = root();
    let intents = Intents::open(root.path()).unwrap();
    assert!(
        !root.path().join(FOLDER).exists(),
        "opening makes no folder"
    );
    assert_eq!(intents.list().unwrap(), Listing::default());
    let id = intents.write(&revocation("revoke-one", 3)).unwrap();
    let folder = std::fs::symlink_metadata(root.path().join(FOLDER)).unwrap();
    assert!(folder.is_dir());
    assert_eq!(folder.mode() & 0o7777, 0o700);
    let file = std::fs::symlink_metadata(root.path().join(FOLDER).join(id.as_str())).unwrap();
    assert!(file.is_file());
    assert_eq!((file.mode() & 0o7777, file.nlink()), (0o600, 1));
    assert!(file.len() <= MAX_BYTES as u64);
    let listed = intents.list().unwrap();
    assert_eq!(listed.refused, 0);
    assert_eq!(
        listed.intents,
        vec![(id.clone(), revocation("revoke-one", 3))]
    );
    intents.remove(&id).unwrap();
    assert!(intents.list().unwrap().intents.is_empty());
    assert!(names(root.path()).is_empty());
    intents.remove(&id).unwrap();
}

#[test]
fn intents_are_listed_in_the_order_they_were_written() {
    let root = root();
    let intents = Intents::open(root.path()).unwrap();
    let written: Vec<IntentId> = (0..20)
        .map(|index| {
            intents
                .write(&revocation(&format!("revoke-{index}"), index))
                .unwrap()
        })
        .collect();
    let listed: Vec<IntentId> = intents
        .list()
        .unwrap()
        .intents
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(listed, written);
}

#[test]
fn a_kind_that_does_not_narrow_access_or_an_unknown_field_is_refused_and_left_in_place() {
    let root = root();
    let intents = Intents::open(root.path()).unwrap();
    let id = intents.write(&revocation("revoke-kept", 1)).unwrap();
    let good = std::fs::read(root.path().join(FOLDER).join(id.as_str())).unwrap();
    let text = String::from_utf8(good).unwrap();
    let reactivate = text.replace("\"revoke\"", "\"reactivate\"");
    assert_ne!(reactivate, text);
    let extra = text.replacen('{', "{\"widen\":true,", 1);
    let version = text.replace("\"version\":1", "\"version\":2");
    assert_ne!(version, text);
    let planted = [
        plant(
            root.path(),
            "00000000000000a1",
            reactivate.as_bytes(),
            0o600,
        ),
        plant(root.path(), "00000000000000a2", extra.as_bytes(), 0o600),
        plant(root.path(), "00000000000000a3", version.as_bytes(), 0o600),
        plant(root.path(), "00000000000000a4", b"not json", 0o600),
    ];
    let listed = intents.list().unwrap();
    assert_eq!(listed.refused, planted.len());
    assert_eq!(listed.intents, vec![(id, revocation("revoke-kept", 1))]);
    assert!(planted.iter().all(|path| path.exists()));
}

#[test]
fn a_link_a_shared_mode_a_second_name_a_bad_name_or_an_oversized_file_is_refused_and_left() {
    let root = root();
    let intents = Intents::open(root.path()).unwrap();
    let id = intents.write(&revocation("revoke-real", 1)).unwrap();
    let real = root.path().join(FOLDER).join(id.as_str());
    let bytes = std::fs::read(&real).unwrap();
    let outside = root.path().join("outside");
    std::fs::write(&outside, &bytes).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = root.path().join(FOLDER).join("00000000000000b1");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let shared = plant(root.path(), "00000000000000b2", &bytes, 0o644);
    let linked = plant(root.path(), "00000000000000b3", &bytes, 0o600);
    std::fs::hard_link(&linked, root.path().join("second-name")).unwrap();
    let named = plant(root.path(), "not-an-intent", &bytes, 0o600);
    let mut large = bytes.clone();
    large.resize(MAX_BYTES + 1, b' ');
    let oversized = plant(root.path(), "00000000000000b4", &large, 0o600);
    std::fs::create_dir(root.path().join(FOLDER).join("00000000000000b5")).unwrap();
    let listed = intents.list().unwrap();
    assert_eq!(listed.refused, 6);
    assert_eq!(listed.intents.len(), 1);
    assert_eq!(listed.intents[0].0, id);
    for path in [&link, &shared, &linked, &named, &oversized] {
        assert!(std::fs::symlink_metadata(path).is_ok(), "{path:?} is left");
    }
    assert_eq!(std::fs::read(&outside).unwrap(), bytes);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn an_intent_over_the_size_bound_is_never_written() {
    let root = root();
    let intents = Intents::open(root.path()).unwrap();
    let long = Intent {
        db_instance: "a".repeat(MAX_BYTES),
        ..revocation("revoke-long", 1)
    };
    assert_eq!(
        intents.write(&long).unwrap_err().0,
        "narrowing_intent_bounds"
    );
    assert!(names(root.path()).is_empty());
}

#[test]
fn at_most_the_listing_cap_of_intents_is_read_and_the_rest_are_refused() {
    let root = root();
    let intents = Intents::open(root.path()).unwrap();
    for index in 0..=MAX_INTENTS {
        intents
            .write(&revocation(&format!("revoke-{index}"), 1))
            .unwrap();
    }
    let listed = intents.list().unwrap();
    assert_eq!(listed.intents.len(), MAX_INTENTS);
    assert_eq!(listed.refused, 1);
    assert_eq!(names(root.path()).len(), MAX_INTENTS + 1);
}

#[test]
fn a_shared_or_linked_intent_folder_is_refused() {
    let root = root();
    let intents = Intents::open(root.path()).unwrap();
    std::fs::create_dir(root.path().join(FOLDER)).unwrap();
    std::fs::set_permissions(
        root.path().join(FOLDER),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let refused = |intents: &Intents| {
        [
            intents.list().unwrap_err().0,
            intents
                .write(&revocation("revoke-shared", 1))
                .unwrap_err()
                .0,
            intents
                .remove(&IntentId::parse(b"00000000000000c1").unwrap())
                .unwrap_err()
                .0,
        ]
    };
    assert_eq!(refused(&intents), ["narrowing_intents_unsafe"; 3]);
    let other = root.path().join("elsewhere");
    std::fs::create_dir(&other).unwrap();
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_dir(root.path().join(FOLDER)).unwrap();
    std::os::unix::fs::symlink(&other, root.path().join(FOLDER)).unwrap();
    assert_eq!(refused(&intents), ["narrowing_intents_unsafe"; 3]);
    assert_eq!(std::fs::read_dir(&other).unwrap().count(), 0);
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        Intents::open(root.path()).unwrap_err().0,
        "narrowing_intents_unsafe"
    );
}

#[test]
fn a_logout_intent_reads_back_as_written() {
    let root = root();
    let intents = Intents::open(root.path()).unwrap();
    let logout = Intent {
        db_instance: "c".repeat(64),
        change: Change::Logout(NativeAuthLogout {
            request_id: RequestId::new("logout-kept").unwrap(),
            scope: NativeAuthScope {
                expected_database_instance: "c".repeat(64),
                connection: CredentialRef::new("connection-z").unwrap(),
                expected_config_id: ConfigId::new("d".repeat(64)).unwrap(),
                expected_source_hash: "e".repeat(64),
            },
        }),
    };
    let id = intents.write(&logout).unwrap();
    assert_eq!(intents.list().unwrap().intents, vec![(id, logout)]);
}
