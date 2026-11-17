use super::*;
use crate::owner_profile::{OwnerProfile, WorkingStyle};
use crate::owner_texts::TextDigest;
use rusqlite::OptionalExtension;
use serde_json::json;

/// A valid owner profile whose name marks its text in any body that would carry it. Input:
/// the name. Output: the profile.
fn profile(name: &str) -> OwnerProfile {
    OwnerProfile {
        name: name.into(),
        role: "Founder".into(),
        bio: "Builds tools.".into(),
        preferred_stack: vec!["Rust".into()],
        working_style: WorkingStyle::default(),
    }
}

/// Runs one owner profile save at the store's next revision and commits it, a rejection
/// included, as an owner request does. Inputs: the store and the profile. Output: the
/// save's outcome.
fn save_profile(db: &mut Connection, profile: OwnerProfile) -> Outcome {
    let tx = db.transaction().unwrap();
    let revision = u64::try_from(moved(&tx).0).unwrap();
    let outcome = owner(&tx, &Command::OwnerProfile { profile }, revision).unwrap();
    tx.commit().unwrap();
    outcome
}

/// The stored profile body. Input: the store. Output: the body, none when none is stored.
fn stored(db: &Connection) -> Option<String> {
    db.query_row(
        "SELECT body FROM user_profile WHERE singleton=1",
        [],
        |row| row.get(0),
    )
    .optional()
    .unwrap()
}

/// The digest of every kept text, none before any text is kept. Input: the store. Output:
/// the digests in order.
fn kept(db: &Connection) -> Vec<String> {
    let made: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='kept_texts')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !made {
        return Vec::new();
    }
    db.prepare("SELECT digest FROM kept_texts ORDER BY digest")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Every logged body as text, events and snapshot chunks alike. Input: the store. Output:
/// the bodies in seq order.
fn bodies(db: &Connection) -> Vec<String> {
    db.prepare("SELECT body FROM state_log ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|body| String::from_utf8(body.unwrap()).unwrap())
        .collect()
}

/// Rebuilds the settings tables from the log in one transaction. Input: the store.
/// Output: none.
fn rebuilt(db: &mut Connection) {
    let tx = db.transaction().unwrap();
    rebuild(&tx).unwrap();
    tx.commit().unwrap();
}

/// v2.5 L10 Q5 row: two owner profile saves leave one kept text, the second, and the log
/// carries each save's digest, never the profile's text; replay rebuilds the stored profile
/// from the kept text, skipping the first save, whose text the second let go.
#[test]
fn two_profile_saves_keep_one_text_and_log_only_its_digest() {
    let mut db = store();
    let first = save_profile(&mut db, profile("Ada Lovelace"));
    assert_eq!(first, Outcome::Applied);
    let second = save_profile(&mut db, profile("Grace Hopper"));
    assert_eq!(second, Outcome::Applied);
    let body = stored(&db).unwrap();
    assert!(body.contains("Grace Hopper"), "{body}");
    let kept = kept(&db);
    assert_eq!(kept.len(), 1, "the first save's text is let go");
    let digest = TextDigest::new(kept[0].clone()).unwrap();
    assert_eq!(
        crate::owner_texts::text(&db, &digest).unwrap(),
        Some(body.clone().into_bytes())
    );
    assert_eq!(
        kinds(&db),
        [(1, KIND_PROFILE.to_owned()), (2, KIND_PROFILE.to_owned())]
    );
    let logged = bodies(&db);
    assert_eq!(
        logged.last(),
        Some(&json!({"body": digest.as_str()}).to_string())
    );
    assert!(
        logged
            .iter()
            .all(|logged| !logged.contains("Ada") && !logged.contains("Grace")),
        "{logged:?}"
    );
    rebuilt(&mut db);
    assert_eq!(stored(&db), Some(body));
}

/// v2.5 L10 Q5: a profile save its checks refuse moves no revision, logs nothing and keeps
/// no text, and the stored profile stays as it was.
#[test]
fn a_refused_profile_save_logs_nothing_and_keeps_no_text() {
    let mut db = store();
    let saved = save_profile(&mut db, profile("Ada Lovelace"));
    assert_eq!(saved, Outcome::Applied);
    let before = (moved(&db), stored(&db), kept(&db));
    let long = "a".repeat(crate::owner_profile::NAME_BYTES_MAX + 1);
    for name in [long.as_str(), " "] {
        let refused = save_profile(&mut db, profile(name));
        assert!(matches!(refused, Outcome::Rejected { .. }), "{refused:?}");
    }
    assert_eq!((moved(&db), stored(&db), kept(&db)), before);
}

/// v2.5 L10 Q5 and L1-N1: a snapshot run carries the stored profile as its kept text's
/// digest, never the text, and replay of the run alone rebuilds the stored profile.
#[test]
fn a_snapshot_run_carries_the_profile_digest_never_its_text() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let saved = crate::owner_profile::save(&tx, &profile("Ada Lovelace")).unwrap();
    assert_eq!(saved, Outcome::Applied);
    let mut appender = Appender::own(&tx, engine()).unwrap();
    let done = snapshot(&mut appender, 9).unwrap();
    drop(appender);
    tx.commit().unwrap();
    crate::state_log::verify_snapshot(&db, done).unwrap();
    let body = stored(&db).unwrap();
    let kept = kept(&db);
    assert_eq!(kept.len(), 1);
    let logged = bodies(&db);
    let chunk = logged
        .iter()
        .map(|logged| serde_json::from_str::<serde_json::Value>(logged).unwrap())
        .find(|chunk| chunk["table"] == PROFILE)
        .unwrap();
    assert_eq!(chunk["rows"], json!([[1, kept[0]]]));
    assert!(logged.iter().all(|logged| !logged.contains("Ada")));
    db.execute("DELETE FROM user_profile", []).unwrap();
    rebuilt(&mut db);
    assert_eq!(stored(&db), Some(body));
}

/// v2.5 L10 Q5 upgrade row: the first run keeps a profile stored before the log as a text
/// and carries only its digest, and replay rebuilds the stored profile.
#[test]
fn the_first_run_keeps_a_profile_stored_before_the_log_and_carries_its_digest() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(STATE).unwrap();
    initialize(&db).unwrap();
    let body = serde_json::to_string(&profile("Ada Lovelace")).unwrap();
    db.execute(
        "INSERT INTO user_profile(singleton, body) VALUES (1, ?1)",
        [&body],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    assert!(first_run(&tx, Opening::for_test(3)).unwrap());
    tx.commit().unwrap();
    assert_eq!(kept(&db).len(), 1);
    assert!(bodies(&db).iter().all(|logged| !logged.contains("Ada")));
    db.execute("DELETE FROM user_profile", []).unwrap();
    rebuilt(&mut db);
    assert_eq!(stored(&db), Some(body));
}
