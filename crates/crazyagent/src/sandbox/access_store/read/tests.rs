use super::*;
use rusqlite::Connection;

/// A project that never saved extra read-only folders has none, also once the table exists
/// because another project saved some; nothing comes from the engine's settings. Input: an
/// empty database. Output: the read of each project.
#[test]
fn read_paths_default_is_empty_and_not_read_from_settings() {
    let connection = Connection::open_in_memory().unwrap();
    assert!(load(&connection, "app").unwrap().is_empty());
    let other = [PathBuf::from("/tmp/other/docs")];
    save(&connection, "other", &other).unwrap();
    assert!(load(&connection, "app").unwrap().is_empty());
    assert_eq!(load(&connection, "other").unwrap(), other);
}
