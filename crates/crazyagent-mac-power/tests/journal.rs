#![cfg(target_os = "macos")]

mod support;

use crazyagent_mac_power::journal::{ENTRIES_MAX, Entry, Journal, Key, StoreError};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use support::StandIn;

fn entry(key: Key, before: u8) -> Entry {
    Entry {
        key,
        before,
        at_ms: 1_700_000_000_000,
    }
}

#[test]
fn the_journal_folder_is_made_owner_only_and_entries_survive_a_reopen() {
    let stand_in = StandIn::new(0);
    let folder = stand_in.folder();
    let mut journal = Journal::open(&folder, stand_in.my_uid()).expect("journal");
    assert!(journal.entries().is_empty());
    let mode = fs::metadata(&folder).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    journal.record(entry(Key::SleepDisabled, 0)).unwrap();
    let file = fs::metadata(folder.join("journal.json")).unwrap();
    assert_eq!(file.permissions().mode() & 0o777, 0o600);
    assert_eq!(file.uid(), stand_in.my_uid());
    let again = Journal::open(&folder, stand_in.my_uid()).unwrap();
    assert_eq!(again.entries(), &[entry(Key::SleepDisabled, 0)]);
    let mut again = again;
    again.remove(Key::SleepDisabled).unwrap();
    assert_eq!(
        Journal::open(&folder, stand_in.my_uid()).unwrap().entries(),
        &[]
    );
}

#[test]
fn a_folder_with_another_owner_or_mode_or_a_link_is_refused_not_repaired() {
    let stand_in = StandIn::new(0);
    let folder = stand_in.folder();
    fs::create_dir(&folder).unwrap();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    let wide = Journal::open(&folder, stand_in.my_uid());
    assert!(matches!(wide, Err(StoreError::Folder(_))), "{wide:?}");
    assert_eq!(
        fs::metadata(&folder).unwrap().permissions().mode() & 0o777,
        0o755
    );
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    let other_owner = Journal::open(&folder, stand_in.my_uid() + 1);
    assert!(
        matches!(other_owner, Err(StoreError::Folder(_))),
        "{other_owner:?}"
    );

    let linked = stand_in.path("linked");
    symlink(&folder, &linked).unwrap();
    let through_link = Journal::open(&linked, stand_in.my_uid());
    assert!(
        matches!(through_link, Err(StoreError::Folder(_))),
        "{through_link:?}"
    );
    let same_folder = Journal::open(&folder, stand_in.my_uid());
    assert!(
        same_folder.is_ok(),
        "the real folder, ours at 0700, is usable: {same_folder:?}"
    );
}

#[test]
fn a_journal_file_that_is_a_link_too_big_or_not_ours_is_unreadable() {
    let stand_in = StandIn::new(0);
    let folder = stand_in.folder();
    Journal::open(&folder, stand_in.my_uid()).unwrap();
    let target = stand_in.path("elsewhere.json");
    fs::write(&target, "[]").unwrap();
    symlink(&target, folder.join("journal.json")).unwrap();
    let linked = Journal::open(&folder, stand_in.my_uid());
    assert!(
        matches!(linked, Err(StoreError::Unreadable(_))),
        "{linked:?}"
    );
    fs::remove_file(folder.join("journal.json")).unwrap();

    fs::write(folder.join("journal.json"), " ".repeat(4097)).unwrap();
    fs::set_permissions(
        folder.join("journal.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let big = Journal::open(&folder, stand_in.my_uid());
    assert!(matches!(big, Err(StoreError::Unreadable(_))), "{big:?}");

    let many: Vec<String> = (0..=ENTRIES_MAX)
        .map(|_| r#"{"key":"sleep_disabled","before":0,"at_ms":1}"#.to_string())
        .collect();
    fs::write(folder.join("journal.json"), format!("[{}]", many.join(","))).unwrap();
    let too_many = Journal::open(&folder, stand_in.my_uid());
    assert!(
        matches!(too_many, Err(StoreError::Unreadable(_))),
        "{too_many:?}"
    );

    let unknown = r#"[{"key":"sleep_disabled","before":0,"at_ms":1,"path":"/etc"}]"#;
    fs::write(folder.join("journal.json"), unknown).unwrap();
    let with_field = Journal::open(&folder, stand_in.my_uid());
    assert!(
        matches!(with_field, Err(StoreError::Unreadable(_))),
        "{with_field:?}"
    );
    let bad_value = r#"[{"key":"sleep_disabled","before":2,"at_ms":1}]"#;
    fs::write(folder.join("journal.json"), bad_value).unwrap();
    let two = Journal::open(&folder, stand_in.my_uid());
    assert!(matches!(two, Err(StoreError::Unreadable(_))), "{two:?}");
}
