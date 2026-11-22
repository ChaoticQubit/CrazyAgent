use super::normalise;
use super::storage::{dump, said, table_entry};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The 2.5 tables whose `digest` column is keyed by the store's random `kept_text_key`.
pub(super) const KEYED_DIGESTS: [&str; 2] = ["kept_texts", "kept_text_refs"];

/// Numbers each kept text's keyed digest from 1 by the text's byte order, so a dump shows
/// `<kept-text-N>` in both kept-text tables where the store's random key decides the digest.
/// Input: the connection. Output: each digest's label, none for a store without the table.
pub(super) fn kept_text_labels(
    connection: &Connection,
) -> Result<BTreeMap<String, String>, String> {
    let held: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='kept_texts')",
            [],
            |row| row.get(0),
        )
        .map_err(said)?;
    if !held {
        return Ok(BTreeMap::new());
    }
    let mut ordered = connection
        .prepare("SELECT digest FROM kept_texts ORDER BY text, digest")
        .map_err(said)?;
    let digests = ordered
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(said)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(said)?;
    let labels: BTreeMap<String, String> = digests
        .into_iter()
        .enumerate()
        .map(|(index, digest)| (digest, format!("<kept-text-{}>", index + 1)))
        .collect();
    Ok(labels)
}

/// Writes the 2.5 kept-text tables with each text's digest under `salt`, as a store's own
/// random `kept_text_key` would key it, and one reference per text. Inputs: the folder, the
/// file name, the salt and the texts. Output: the engine.db path.
fn kept_sample(folder: &Path, file: &str, salt: &str, texts: &[&str]) -> PathBuf {
    let path = folder.join(file);
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE kept_texts (digest TEXT PRIMARY KEY, text BLOB NOT NULL);
         CREATE TABLE kept_text_refs (holder TEXT PRIMARY KEY, digest TEXT NOT NULL, project TEXT);",
    )
    .unwrap();
    for (index, text) in texts.iter().enumerate() {
        let digest = format!("{salt}{index:02}{}", "0".repeat(62 - salt.len()));
        assert_eq!(digest.len(), 64);
        db.execute(
            "INSERT INTO kept_texts VALUES (?1, CAST(?2 AS BLOB))",
            rusqlite::params![digest, text],
        )
        .unwrap();
        db.execute(
            "INSERT INTO kept_text_refs VALUES (?1, ?2, NULL)",
            rusqlite::params![format!("memory_library/note-{index}/text"), digest],
        )
        .unwrap();
    }
    path
}

/// V250-SNAPSHOT-RED: a kept-text digest is keyed by the store's random `kept_text_key`, so
/// two stores holding the same texts dump the same, each digest shown as its text's number
/// in both tables, and a changed text still changes the dump.
#[test]
fn kept_text_digests_dump_as_their_text_so_a_store_key_never_shows() {
    let scratch = tempfile::tempdir().unwrap();
    let texts = ["Tide tables are read at dawn.", "Buoys are checked weekly."];
    let first = dump(&kept_sample(scratch.path(), "a.db", "aa", &texts)).unwrap();
    let rekeyed = dump(&kept_sample(scratch.path(), "b.db", "bb", &texts)).unwrap();
    let changed = dump(&kept_sample(
        scratch.path(),
        "c.db",
        "aa",
        &["Other.", texts[1]],
    ))
    .unwrap();
    assert_eq!(normalise(&first), normalise(&rekeyed));
    assert_ne!(normalise(&first), normalise(&changed));
    let refs = table_entry(&first, "kept_text_refs").unwrap();
    let kept = table_entry(&first, "kept_texts").unwrap();
    let digest_of = |rows: &Value, field: &str, value: &str| {
        rows["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[field] == json!(value) || row[field]["blob"] == json!(value))
            .unwrap()["digest"]
            .clone()
    };
    assert_eq!(
        digest_of(refs, "holder", "memory_library/note-0/text"),
        digest_of(kept, "text", texts[0])
    );
    assert_eq!(digest_of(kept, "text", texts[1]), json!("<kept-text-1>"));
}
