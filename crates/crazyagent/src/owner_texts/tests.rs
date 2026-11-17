use super::*;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde_json::json;

const HOLDER: &str = "landing_change_sets/c1/summary";

/// An empty in-memory store. Output: the connection.
fn store() -> Connection {
    Connection::open_in_memory().unwrap()
}

/// The number of rows in a text table. Inputs: the store and the table. Output: the count.
fn count(db: &Connection, table: &str) -> i64 {
    assert!(table.starts_with("owner_text"));
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// V250-OWNER-TEXT: one text kept by two holders is stored once under one digest; releasing
/// one holder keeps it for the other, and releasing the last deletes it.
#[test]
fn a_shared_text_survives_until_its_last_holder_is_released() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let prompt = b"the owner's secret prompt with sk-ant-api03-key";
    let first = keep(&tx, "landing_change_sets/c1/prompt_line", None, prompt).unwrap();
    let second = keep(&tx, "landing_change_sets/c2/prompt_line", None, prompt).unwrap();
    assert_eq!(first, second, "one text, one digest");
    assert_eq!(count(&tx, "owner_texts"), 1);
    release(&tx, "landing_change_sets/c1/prompt_line").unwrap();
    assert_eq!(
        text(&tx, &first).unwrap().as_deref(),
        Some(&prompt[..]),
        "the other holder keeps it"
    );
    release(&tx, "landing_change_sets/c2/prompt_line").unwrap();
    assert_eq!(text(&tx, &first).unwrap(), None);
    assert_eq!(
        (count(&tx, "owner_texts"), count(&tx, "owner_text_refs")),
        (0, 0)
    );
    release(&tx, "landing_change_sets/c2/prompt_line").unwrap();
}

/// V250-OWNER-TEXT: a holder kept again with another text releases its old text; kept again
/// with the same text it changes nothing.
#[test]
fn a_holder_kept_again_with_another_text_releases_the_old_one() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let old = keep(&tx, HOLDER, None, b"first summary").unwrap();
    let new = keep(&tx, HOLDER, None, b"second summary").unwrap();
    assert_ne!(old, new);
    assert_eq!(text(&tx, &old).unwrap(), None);
    assert_eq!(
        text(&tx, &new).unwrap().as_deref(),
        Some(&b"second summary"[..])
    );
    assert_eq!(keep(&tx, HOLDER, None, b"second summary").unwrap(), new);
    assert_eq!(
        (count(&tx, "owner_texts"), count(&tx, "owner_text_refs")),
        (1, 1)
    );
}

/// V250-OWNER-TEXT: a text one byte over its bound is refused before anything is written;
/// one at the bound is kept whole.
#[test]
fn a_text_over_its_bound_is_refused_and_nothing_is_written() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let over = vec![b'x'; TEXT_BYTES_MAX + 1];
    assert_eq!(
        keep(&tx, HOLDER, None, &over).unwrap_err().0,
        "owner_text_too_large"
    );
    initialize(&tx).unwrap();
    assert_eq!(
        [
            count(&tx, "owner_texts"),
            count(&tx, "owner_text_refs"),
            count(&tx, "owner_text_key")
        ],
        [0, 0, 0]
    );
    let digest = keep(&tx, HOLDER, None, &over[..TEXT_BYTES_MAX]).unwrap();
    assert_eq!(
        text(&tx, &digest).unwrap().map(|kept| kept.len()),
        Some(TEXT_BYTES_MAX)
    );
}

/// V250-OWNER-TEXT: replay reads a kept text by its digest, and for a digest with no text
/// gives empty bytes and names the digest.
#[test]
fn replay_names_a_missing_text_and_reads_a_kept_one() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let kept = keep(&tx, HOLDER, None, b"kept").unwrap();
    let unknown = TextDigest::new("f".repeat(64)).unwrap();
    assert_ne!(kept, unknown);
    let mut missing = Vec::new();
    assert_eq!(replayed(&tx, &kept, &mut missing).unwrap(), b"kept");
    assert!(missing.is_empty());
    assert_eq!(replayed(&tx, &unknown, &mut missing).unwrap(), b"");
    assert_eq!(missing, [unknown]);
}

/// V250-OWNER-TEXT, L1-N1: a digest decodes only as 64 lowercase hex digits, so an event body
/// key of this type takes no free text; it encodes as that plain string.
#[test]
fn a_digest_decodes_only_as_64_lowercase_hex() {
    let good = "a".repeat(64);
    let digest: TextDigest = serde_json::from_value(json!(good)).unwrap();
    assert_eq!(serde_json::to_value(&digest).unwrap(), json!(good));
    for bad in [
        "A".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
        String::new(),
        format!("{}g", "a".repeat(63)),
        "the owner's secret prompt with sk-ant-api03-key".into(),
    ] {
        assert!(
            serde_json::from_value::<TextDigest>(json!(bad)).is_err(),
            "{bad}"
        );
    }
}

/// V250-OWNER-TEXT: each store keys its own digests, so a short text's digest in one store
/// is not its digest in another, nor its plain sha256, which anyone could compute.
#[test]
fn two_stores_digest_one_text_differently_from_its_plain_sha256() {
    let digest_in = |db: &mut Connection| {
        let tx = db.transaction().unwrap();
        let digest = keep(&tx, HOLDER, None, b"yes").unwrap();
        tx.commit().unwrap();
        digest
    };
    let one = digest_in(&mut store());
    let two = digest_in(&mut store());
    assert_ne!(one, two, "each store keys its own digests");
    let plain = format!("{:x}", Sha256::digest(b"yes"));
    assert_ne!(one.as_str(), plain);
    assert_ne!(two.as_str(), plain);
}

/// V250-OWNER-TEXT: the keyed digest is HMAC-SHA256 (RFC 4231 test case 2, key "Jefe").
#[test]
fn the_keyed_digest_is_hmac_sha256() {
    let mut key = [0u8; KEY_BYTES];
    key[..4].copy_from_slice(b"Jefe");
    assert_eq!(
        keyed(&key, b"what do ya want for nothing?").as_str(),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

/// V250-OWNER-TEXT: a text is kept only inside its event's transaction.
#[test]
#[should_panic(expected = "a text is kept in its event's transaction")]
fn a_text_kept_outside_a_transaction_is_a_bug() {
    let db = store();
    let _ = keep(&db, HOLDER, None, b"no transaction");
}

/// V250-OWNER-TEXT: a holder key is engine-built, `table/id/column` in ASCII graphic bytes
/// up to its bound; anything else is refused with nothing written.
#[test]
fn a_holder_key_that_is_not_engine_built_is_refused() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let long = "h".repeat(HOLDER_BYTES_MAX + 1);
    for holder in ["", "landing change sets", "a/\u{e9}/b", long.as_str()] {
        assert_eq!(
            keep(&tx, holder, None, b"text").unwrap_err().0,
            "owner_text_holder_invalid",
            "{holder}"
        );
        assert_eq!(
            release(&tx, holder).unwrap_err().0,
            "owner_text_holder_invalid"
        );
    }
    initialize(&tx).unwrap();
    assert_eq!(count(&tx, "owner_texts"), 0);
    let at = "h".repeat(HOLDER_BYTES_MAX);
    keep(&tx, &at, None, b"text").unwrap();
}

/// V250-OWNER-TEXT: a project's generic clear (`removed::clear` deletes every row keyed by
/// project) deletes its holders, and the trigger deletes their texts unless another project
/// or an engine-wide holder has the same digest.
#[test]
fn a_project_clear_takes_its_texts_and_keeps_a_shared_one() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    let (p1, p2) = (ProjectId::new("p1").unwrap(), ProjectId::new("p2").unwrap());
    let own = keep(&tx, "landing_change_sets/c1/summary", Some(&p1), b"p1 only").unwrap();
    let shared = keep(&tx, "landing_change_sets/c1/label", Some(&p1), b"shared").unwrap();
    keep(&tx, "landing_change_sets/c2/label", Some(&p2), b"shared").unwrap();
    let wide = keep(&tx, "user_profile/owner/bio", None, b"p1 only").unwrap();
    assert_eq!(own, wide, "one text, one digest across holders");
    let cleared = tx
        .execute(
            "DELETE FROM owner_text_refs WHERE project=?1",
            [p1.as_str()],
        )
        .unwrap();
    assert_eq!(cleared, 2);
    assert!(text(&tx, &shared).unwrap().is_some(), "p2 still holds it");
    assert!(
        text(&tx, &own).unwrap().is_some(),
        "the engine-wide holder keeps it"
    );
    release(&tx, "user_profile/owner/bio").unwrap();
    assert_eq!(text(&tx, &own).unwrap(), None);
    assert_eq!(count(&tx, "owner_text_refs"), 1);
}
