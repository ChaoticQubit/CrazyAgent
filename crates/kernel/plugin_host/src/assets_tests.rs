use super::*;
use rusqlite::Connection;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

const WORDS: &[u8] = b"Use plain words.";

/// The serde text of a fixed `(project, config, assets)` triple as the engine wrote it at
/// f9e642e1a (assets.rs unchanged since c6936f38f): newtype ids as plain strings, `Global`
/// as a bare string, `Project` as a one-key map, bytes as a number array.
const GOLDEN_TRIPLE: &str = concat!(
    r#"["p","c",[{"source":{"id":"rules","name":"rules","scope":{"Project":"p"},"#,
    r#""kind":"Rules","path":"/x/rules.md"},"#,
    r#""content_hash":"a4d4c1c252aa89fc4360f1408639f2f44c27fb7ebe5ca5c930b5caac1de3871f","#,
    r#""bytes":[85,115,101,32,112,108,97,105,110,32,119,111,114,100,115,46]},"#,
    r#"{"source":{"id":"g-skill","name":"skill","scope":"Global","kind":"Skill","#,
    r#""path":"/x/skill.md"},"#,
    r#""content_hash":"a4d4c1c252aa89fc4360f1408639f2f44c27fb7ebe5ca5c930b5caac1de3871f","#,
    r#""bytes":[85,115,101,32,112,108,97,105,110,32,119,111,114,100,115,46]}]]"#
);

/// The SHA-256 of `GOLDEN_TRIPLE`, taken with `shasum -a 256` over its bytes.
const GOLDEN_HASH: &str = "0c52bd85c6dc9b89d3d27d1011295d8c9cafb5dac0b96bde2429068fea45f378";

/// A source with the given fields. Inputs: id, name, scope, kind and path. Output: it.
fn source(id: &str, name: &str, scope: Scope, kind: Kind, path: PathBuf) -> Source {
    Source {
        id: id.into(),
        name: name.into(),
        scope,
        kind,
        path,
    }
}

/// An in-memory store with the asset tables. Output: the connection.
fn store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    db
}

/// Compares file identities exactly, standing in for the engine's renumber rule.
/// Inputs: pinned and live pairs. Output: whether they are equal.
fn same(pinned: &[(u64, u64)], live: &[(u64, u64)]) -> bool {
    pinned == live
}

const ADMIT: AdmitFacts = AdmitFacts {
    project_exists: true,
    asset_bytes_max: 1024,
    asset_sources_max: 8,
};

/// Snapshot facts for one project and config, with room for every fixture. Inputs: the
/// project and config ids. Output: the facts.
fn taken<'a>(project_id: &'a str, config_id: &'a str) -> SnapshotFacts<'a> {
    SnapshotFacts {
        project_id,
        config_id,
        asset_bytes_max: 1024,
        records_max: 8,
        frame_bytes_max: 65536,
        same_files: same,
    }
}

/// A canonical scratch folder, so the no-follow walk meets no symlinked temp prefix.
/// Output: the folder guard and its canonical path.
#[cfg(unix)]
fn scratch() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    (root, base)
}

/// Writes a file owned by this user. Inputs: folder, name and text. Output: its path.
#[cfg(unix)]
fn file(base: &Path, name: &str, text: &str) -> PathBuf {
    let path = base.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn the_snapshot_hash_bytes_are_unchanged() {
    let assets = vec![
        Content {
            source: source(
                "rules",
                "rules",
                Scope::Project("p".into()),
                Kind::Rules,
                "/x/rules.md".into(),
            ),
            content_hash: hash(WORDS),
            bytes: WORDS.to_vec(),
        },
        Content {
            source: source(
                "g-skill",
                "skill",
                Scope::Global,
                Kind::Skill,
                "/x/skill.md".into(),
            ),
            content_hash: hash(WORDS),
            bytes: WORDS.to_vec(),
        },
    ];
    assert_eq!(
        serde_json::to_string(&("p", "c", &assets)).unwrap(),
        GOLDEN_TRIPLE
    );
    assert_eq!(hash(GOLDEN_TRIPLE.as_bytes()), GOLDEN_HASH);
    assert_eq!(snapshot_hash("p", "c", &assets), GOLDEN_HASH);
}

#[test]
fn revoke_of_a_missing_source_is_refused() {
    let mut db = store();
    let tx = db.transaction().unwrap();
    assert_eq!(
        revoke(&tx, "absent"),
        Err(Failure::Refused("asset_source_missing".into()))
    );
    tx.execute(
        "INSERT INTO asset_sources(id,scope,name,body) VALUES ('a','\"Global\"','a','{}')",
        [],
    )
    .unwrap();
    assert_eq!(revoke(&tx, "a"), Ok(()));
    assert_eq!(
        revoke(&tx, "a"),
        Err(Failure::Refused("asset_source_missing".into()))
    );
}

#[cfg(unix)]
#[test]
fn admit_pins_the_file_identity_and_refuses_a_native_plugin() {
    let (_root, base) = scratch();
    let path = file(&base, "rules.md", "Use plain words.");
    let mut db = store();
    let tx = db.transaction().unwrap();
    let rules = source("rules", "rules", Scope::Global, Kind::Rules, path.clone());
    assert_eq!(admit(&tx, &ADMIT, &rules), Ok(()));
    let (scope, body): (String, String) = tx
        .query_row(
            "SELECT scope,body FROM asset_sources WHERE id='rules'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(scope, "\"Global\"");
    let pinned: Admitted = serde_json::from_str(&body).unwrap();
    let live = std::fs::metadata(&path).unwrap();
    assert_eq!((pinned.device, pinned.inode), (live.dev(), live.ino()));
    assert_eq!(pinned.source, rules);

    let native = source("n", "native", Scope::Global, Kind::NativePlugin, path.clone());
    assert_eq!(
        admit(&tx, &ADMIT, &native),
        Err(Failure::Refused("native_plugin_unavailable".into()))
    );
    let missing = AdmitFacts {
        project_exists: false,
        ..ADMIT
    };
    let project = source("q", "q", Scope::Project("p".into()), Kind::Rules, path.clone());
    assert_eq!(
        admit(&tx, &missing, &project),
        Err(Failure::Refused("project_missing".into()))
    );
    let link = base.join("link.md");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let linked = source("l", "link", Scope::Global, Kind::Skill, link);
    assert_eq!(
        admit(&tx, &ADMIT, &linked),
        Err(Failure::Refused("asset_path_denied".into()))
    );
    let rows: i64 = tx
        .query_row("SELECT COUNT(*) FROM asset_sources", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}

#[cfg(unix)]
#[test]
fn a_snapshot_hash_covers_project_config_and_assets() {
    let (_root, base) = scratch();
    let global = file(&base, "global.md", "global text");
    let project = file(&base, "project.md", "project text");
    let skill = file(&base, "skill.md", "skill text");
    let mut db = store();
    let tx = db.transaction().unwrap();
    for admitted in [
        source("g-rules", "rules", Scope::Global, Kind::Rules, global),
        source("p-rules", "rules", Scope::Project("p".into()), Kind::Rules, project.clone()),
        source("g-skill", "skill", Scope::Global, Kind::Skill, skill),
        source("q-rules", "rules", Scope::Project("q".into()), Kind::Rules, project.clone()),
    ] {
        assert_eq!(admit(&tx, &ADMIT, &admitted), Ok(()));
    }
    assert_eq!(snapshot(&tx, &taken("p", "c"), "s1"), Ok(()));
    let first = read(&tx, &ReadFacts { project_id: "p", task_config_id: "c" }, "s1").unwrap();
    let ids: Vec<&str> = first.assets.iter().map(|a| a.source.id.as_str()).collect();
    assert_eq!(ids, ["p-rules", "g-skill"]);
    assert_eq!(first.content_hash, snapshot_hash("p", "c", &first.assets));
    assert_eq!(first.assets[0].bytes, b"project text");

    assert_eq!(snapshot(&tx, &taken("p", "c2"), "s2"), Ok(()));
    let other = read(&tx, &ReadFacts { project_id: "p", task_config_id: "c2" }, "s2").unwrap();
    assert_eq!(other.assets, first.assets);
    assert_ne!(other.content_hash, first.content_hash);

    std::fs::write(&project, "new text").unwrap();
    assert_eq!(snapshot(&tx, &taken("p", "c"), "s3"), Ok(()));
    let changed = read(&tx, &ReadFacts { project_id: "p", task_config_id: "c" }, "s3").unwrap();
    assert_ne!(changed.content_hash, first.content_hash);
    assert_eq!(changed.assets[0].content_hash, hash(b"new text"));
}

#[cfg(unix)]
#[test]
fn a_read_refuses_a_revoked_or_replaced_source() {
    let (_root, base) = scratch();
    let path = file(&base, "rules.md", "Use plain words.");
    let mut db = store();
    let tx = db.transaction().unwrap();
    let rules = source("rules", "rules", Scope::Global, Kind::Rules, path.clone());
    assert_eq!(admit(&tx, &ADMIT, &rules), Ok(()));
    assert_eq!(snapshot(&tx, &taken("p", "c"), "s1"), Ok(()));
    assert_eq!(revoke(&tx, "rules"), Ok(()));
    let facts = ReadFacts {
        project_id: "p",
        task_config_id: "c",
    };
    assert_eq!(
        read(&tx, &facts, "s1"),
        Err(Failure::Refused("asset_source_revoked".into()))
    );
    assert_eq!(
        snapshot(&tx, &taken("p", "c"), "s2"),
        Err(Failure::Refused("asset_source_revoked".into()))
    );

    let skill = file(&base, "skill.md", "skill text");
    let mut fresh = store();
    let tx = fresh.transaction().unwrap();
    let pinned = source("skill", "skill", Scope::Global, Kind::Skill, skill.clone());
    assert_eq!(admit(&tx, &ADMIT, &pinned), Ok(()));
    let before = std::fs::metadata(&skill).unwrap().ino();
    let swap = file(&base, "swap.md", "skill text");
    std::fs::rename(&swap, &skill).unwrap();
    assert_ne!(std::fs::metadata(&skill).unwrap().ino(), before);
    assert_eq!(
        snapshot(&tx, &taken("p", "c"), "s3"),
        Err(Failure::Refused("asset_source_replaced".into()))
    );
}

#[cfg(unix)]
#[test]
fn a_read_from_another_project_or_config_is_refused() {
    let (_root, base) = scratch();
    let path = file(&base, "rules.md", "Use plain words.");
    let mut db = store();
    let tx = db.transaction().unwrap();
    let rules = source("rules", "rules", Scope::Global, Kind::Rules, path);
    assert_eq!(admit(&tx, &ADMIT, &rules), Ok(()));
    assert_eq!(snapshot(&tx, &taken("p", "c"), "s1"), Ok(()));
    let at = |project_id, task_config_id| ReadFacts {
        project_id,
        task_config_id,
    };
    assert!(read(&tx, &at("p", "c"), "s1").is_ok());
    for (project, config) in [("q", "c"), ("p", "c2")] {
        assert_eq!(
            read(&tx, &at(project, config), "s1"),
            Err(Failure::Refused("asset_scope_denied".into()))
        );
    }
    assert_eq!(
        read(&tx, &at("p", "c"), "absent"),
        Err(Failure::Refused("asset_snapshot_missing".into()))
    );
}
