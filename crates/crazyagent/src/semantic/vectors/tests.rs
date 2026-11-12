use super::*;
use rusqlite::params;
use std::sync::atomic::{AtomicUsize, Ordering};

const D: usize = 64;
const ONE: u64 = (D * 4) as u64;
static KEYS: AtomicUsize = AtomicUsize::new(0);

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    crate::search_index::initialize(&db).unwrap();
    initialize(&db).unwrap();
    db
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}

fn chunk(db: &Connection, project: Option<&str>, source: Source, text: &str, at_ms: i64) -> i64 {
    let key = format!("key-{}", KEYS.fetch_add(1, Ordering::Relaxed));
    db.execute(
        "INSERT INTO search_chunks(project, source, key, ordinal, text, at_ms) VALUES (?1, ?2, ?3, 0, ?4, ?5)",
        params![project, source.as_str(), key, text, at_ms],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn plugin(sha: char) -> Plugin {
    Plugin::new("acme", &sha.to_string().repeat(64), D, 8)
}

fn unit(axis: usize) -> Vec<f32> {
    let mut vector = vec![0.0; D];
    vector[axis % D] = 1.0;
    vector
}

fn axis_of(text: &str) -> usize {
    text.split_whitespace()
        .find_map(|word| word.strip_prefix("axis").and_then(|n| n.parse().ok()))
        .unwrap_or(0)
}

fn by_axis(
    seen: &mut Vec<String>,
) -> impl FnMut(&[&str]) -> Result<Vec<Vec<f32>>, EngineError> + '_ {
    move |texts: &[&str]| {
        seen.extend(texts.iter().map(|text| (*text).to_owned()));
        Ok(texts.iter().map(|text| unit(axis_of(text))).collect())
    }
}

fn wide() -> Bounds {
    Bounds {
        project_bytes: 1 << 30,
        memory_bytes: 1 << 30,
        round_chunks: 1_000,
        scan_bytes: 1 << 30,
        sweep_rows: 1_000,
    }
}

fn round(
    db: &mut Connection,
    name: &str,
    plugin: &Plugin,
    bounds: &Bounds,
) -> (usize, Vec<String>) {
    let mut seen = Vec::new();
    let embedded = embed_round(db, &project(name), plugin, bounds, &mut by_axis(&mut seen));
    (embedded, seen)
}

fn stored(db: &Connection, key: &str) -> Vec<i64> {
    let mut statement = db
        .prepare("SELECT chunk FROM search_vectors WHERE plugin_key=?1 ORDER BY chunk")
        .unwrap();
    statement
        .query_map([key], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_plugin_key_is_its_name_and_manifest_hash_and_the_tables_are_additive() {
    let acme = plugin('a');
    assert_eq!(acme.key, format!("acme:{}", "a".repeat(64)));
    assert!(acme.key.len() <= KEY_BYTES_MAX);
    assert_eq!((acme.dimensions, acme.batch), (D, 8));
    let mut db = db();
    let first = chunk(&db, Some("alpha"), Source::Session, "hello axis1", 10);
    assert_eq!(round(&mut db, "alpha", &acme, &wide()).0, 1);
    initialize(&db).unwrap();
    assert_eq!(
        stored(&db, &acme.key),
        [first],
        "a second start keeps every vector"
    );
    let bytes: i64 = db
        .query_row("SELECT length(vector) FROM search_vectors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(bytes as u64, ONE, "f32 little endian, one per dimension");
}

#[test]
fn a_changed_plugin_key_re_embeds_and_drops_the_old_vectors() {
    let mut db = db();
    let one = chunk(&db, Some("alpha"), Source::Session, "one", 10);
    let two = chunk(&db, Some("alpha"), Source::Session, "two", 20);
    let (old, new) = (plugin('a'), plugin('b'));
    assert_eq!(round(&mut db, "alpha", &old, &wide()).0, 2);
    assert_eq!(
        round(&mut db, "alpha", &old, &wide()).0,
        0,
        "nothing left for the same key"
    );
    let (embedded, seen) = round(&mut db, "alpha", &new, &wide());
    assert_eq!(embedded, 2, "an edited manifest re-embeds every chunk");
    assert_eq!(seen, ["two", "one"]);
    assert_eq!(stored(&db, &new.key), [one, two]);
    assert!(stored(&db, &old.key).is_empty(), "the old key's vectors go");
    assert_eq!(
        round(&mut db, "alpha", &old, &wide()).0,
        2,
        "switching back re-embeds"
    );
}

#[test]
fn a_key_switched_back_while_the_old_vectors_are_still_going_requeues_what_was_deleted() {
    let mut db = db();
    let (a, b) = (plugin('a'), plugin('b'));
    let ids: Vec<i64> = (1..=3)
        .map(|at| {
            chunk(
                &db,
                Some("alpha"),
                Source::Session,
                &format!("chunk {at}"),
                at,
            )
        })
        .collect();
    assert_eq!(round(&mut db, "alpha", &a, &wide()).0, 3);
    let step = Bounds {
        sweep_rows: 1,
        ..wide()
    };
    assert_eq!(
        round(&mut db, "alpha", &b, &step).0,
        0,
        "the old key's vectors go one step per round"
    );
    assert_eq!(stored(&db, &a.key).len(), 2);
    let embedded: usize = (0..10).map(|_| round(&mut db, "alpha", &a, &step).0).sum();
    assert_eq!(
        embedded, 1,
        "the vector deleted before the switch back is queued and embedded again"
    );
    assert_eq!(stored(&db, &a.key), ids);
    assert_eq!(
        progress(&db, &a.key),
        Progress {
            embedded: 3,
            total: 3
        }
    );
}

#[test]
fn a_new_key_queues_the_chunks_in_bounded_steps_before_it_embeds() {
    let mut db = db();
    let acme = plugin('a');
    for at in 1..=3 {
        chunk(
            &db,
            Some("alpha"),
            Source::Session,
            &format!("chunk {at}"),
            at,
        );
    }
    let step = Bounds {
        sweep_rows: 1,
        ..wide()
    };
    let rounds: Vec<usize> = (0..5)
        .map(|_| round(&mut db, "alpha", &acme, &step).0)
        .collect();
    assert_eq!(
        rounds[0], 0,
        "one step queues at most sweep_rows chunks, so the first round waits"
    );
    assert_eq!(rounds.iter().sum::<usize>(), 3);
    let queued: i64 = db
        .query_row("SELECT COUNT(*) FROM search_vector_pending", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(queued, 0);
}

#[test]
fn deleting_or_rewriting_a_chunk_drops_its_vector_in_the_same_transaction() {
    let mut db = db();
    let acme = plugin('a');
    let one = chunk(&db, Some("alpha"), Source::Session, "one", 10);
    let two = chunk(&db, Some("alpha"), Source::Session, "two", 20);
    round(&mut db, "alpha", &acme, &wide());
    {
        let tx = db.transaction().unwrap();
        tx.execute("DELETE FROM search_chunks WHERE id=?1", [one])
            .unwrap();
        let left: i64 = tx
            .query_row("SELECT COUNT(*) FROM search_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, 1, "the delete took its vector inside the transaction");
    }
    assert_eq!(
        stored(&db, &acme.key),
        [one, two],
        "a rolled back delete keeps it"
    );
    db.execute(
        "UPDATE search_chunks SET text='two again' WHERE id=?1",
        [two],
    )
    .unwrap();
    assert_eq!(
        stored(&db, &acme.key),
        [one],
        "a rewritten chunk loses its stale vector"
    );
    let (embedded, seen) = round(&mut db, "alpha", &acme, &wide());
    assert_eq!((embedded, seen), (1, vec!["two again".to_owned()]));
}

#[test]
fn vectors_left_by_chunks_deleted_before_the_triggers_are_swept_at_start() {
    let mut db = db();
    let acme = plugin('a');
    let gone = chunk(&db, Some("alpha"), Source::Session, "gone", 10);
    let kept = chunk(&db, Some("alpha"), Source::Session, "kept", 20);
    round(&mut db, "alpha", &acme, &wide());
    db.execute_batch("DROP TRIGGER search_vectors_gone;")
        .unwrap();
    db.execute("DELETE FROM search_chunks WHERE id=?1", [gone])
        .unwrap();
    assert_eq!(stored(&db, &acme.key), [gone, kept]);
    initialize(&db).unwrap();
    assert_eq!(stored(&db, &acme.key), [kept]);
    let triggers: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='trigger' AND name LIKE 'search_vectors_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(triggers, 3, "insert, delete and rewrite triggers are back");
}

#[test]
fn a_round_embeds_the_newest_project_chunks_then_app_wide_memories_within_its_bound() {
    let mut db = db();
    let acme = plugin('a');
    for at in 1..=5 {
        chunk(
            &db,
            Some("alpha"),
            Source::Session,
            &format!("alpha {at}"),
            at,
        );
    }
    let other = chunk(&db, Some("beta"), Source::Session, "beta", 9);
    let memory = chunk(&db, None, Source::Memory, "everyone", 3);
    let bounds = Bounds {
        round_chunks: 3,
        ..wide()
    };
    let (embedded, seen) = round(&mut db, "alpha", &acme, &bounds);
    assert_eq!(embedded, 3);
    assert_eq!(
        seen,
        ["alpha 5", "alpha 4", "alpha 3"],
        "newest first, project first"
    );
    let (embedded, seen) = round(&mut db, "alpha", &acme, &bounds);
    assert_eq!(embedded, 3);
    assert_eq!(seen, ["alpha 2", "alpha 1", "everyone"]);
    assert_eq!(round(&mut db, "alpha", &acme, &bounds).0, 0);
    assert!(
        !stored(&db, &acme.key).contains(&other),
        "another project's chunk waits for its round"
    );
    assert!(stored(&db, &acme.key).contains(&memory));
}

#[test]
fn a_full_project_evicts_its_oldest_only_for_newer_chunks_and_a_second_round_sends_nothing() {
    let mut db = db();
    let acme = plugin('a');
    let bounds = Bounds {
        project_bytes: 3 * ONE,
        memory_bytes: ONE,
        ..wide()
    };
    let ten = chunk(&db, Some("alpha"), Source::Session, "ten", 10);
    let twenty = chunk(&db, Some("alpha"), Source::Session, "twenty", 20);
    let thirty = chunk(&db, Some("alpha"), Source::Session, "thirty", 30);
    assert_eq!(round(&mut db, "alpha", &acme, &bounds).0, 3);
    let forty = chunk(&db, Some("alpha"), Source::Session, "forty", 40);
    assert_eq!(round(&mut db, "alpha", &acme, &bounds).0, 1);
    assert_eq!(
        stored(&db, &acme.key),
        [twenty, thirty, forty],
        "the oldest made room"
    );
    chunk(&db, Some("alpha"), Source::Session, "five", 5);
    let (embedded, seen) = round(&mut db, "alpha", &acme, &bounds);
    assert_eq!(
        (embedded, seen.len()),
        (0, 0),
        "an older chunk never displaces a newer one"
    );
    let (embedded, seen) = round(&mut db, "alpha", &acme, &bounds);
    assert_eq!(
        (embedded, seen.len()),
        (0, 0),
        "nor is anything sent again, ten included"
    );
    assert!(!stored(&db, &acme.key).contains(&ten));

    let old = chunk(&db, None, Source::Memory, "old memory", 1);
    let new = chunk(&db, None, Source::Memory, "new memory", 2);
    assert_eq!(
        round(&mut db, "alpha", &acme, &bounds).0,
        1,
        "memories have their own cap"
    );
    let kept = stored(&db, &acme.key);
    assert!(kept.contains(&new) && !kept.contains(&old));
    assert!(
        kept.contains(&forty),
        "a memory never evicts a project's vector"
    );
}

#[test]
fn vectors_of_the_wrong_length_or_not_finite_are_refused_and_the_round_stops() {
    let mut db = db();
    let acme = plugin('a');
    chunk(&db, Some("alpha"), Source::Session, "one", 10);
    let replies: [Box<dyn Fn(usize) -> Result<Vec<Vec<f32>>, EngineError>>; 4] = [
        Box::new(|count| Ok(vec![vec![0.5; D - 1]; count])),
        Box::new(|count| Ok(vec![vec![f32::NAN; D]; count])),
        Box::new(|_| Ok(Vec::new())),
        Box::new(|_| Err(EngineError("app_plugin_unreachable".into()))),
    ];
    for reply in replies {
        let mut embed = |texts: &[&str]| reply(texts.len());
        assert_eq!(
            embed_round(&mut db, &project("alpha"), &acme, &wide(), &mut embed),
            0
        );
        assert!(stored(&db, &acme.key).is_empty());
    }
    assert_eq!(
        round(&mut db, "alpha", &acme, &wide()).0,
        1,
        "a refused batch is tried again"
    );
}

#[test]
fn a_chunk_rewritten_or_deleted_while_its_batch_is_out_is_not_stored_and_a_rewrite_stays_queued() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("engine.db");
    let mut db = Connection::open(&path).unwrap();
    crate::search_index::initialize(&db).unwrap();
    initialize(&db).unwrap();
    let acme = plugin('a');
    let rewritten = chunk(&db, Some("alpha"), Source::Session, "before", 10);
    let deleted = chunk(&db, Some("alpha"), Source::Session, "gone", 20);
    let kept = chunk(&db, Some("alpha"), Source::Session, "kept", 30);
    let other = Connection::open(&path).unwrap();
    let mut embed = |texts: &[&str]| -> Result<Vec<Vec<f32>>, EngineError> {
        other
            .execute(
                "UPDATE search_chunks SET text='after' WHERE id=?1",
                [rewritten],
            )
            .expect("the batch is sent outside any transaction");
        other
            .execute("DELETE FROM search_chunks WHERE id=?1", [deleted])
            .unwrap();
        Ok(texts.iter().map(|_| unit(0)).collect())
    };
    assert_eq!(
        embed_round(&mut db, &project("alpha"), &acme, &wide(), &mut embed),
        1
    );
    assert_eq!(stored(&db, &acme.key), [kept]);
    let (embedded, seen) = round(&mut db, "alpha", &acme, &wide());
    assert_eq!((embedded, seen), (1, vec!["after".to_owned()]));
}

#[test]
fn a_query_is_masked_trimmed_and_cut_before_it_is_stored_or_sent() {
    assert_eq!(
        query_text("  find the login flow \n").as_deref(),
        Some("find the login flow")
    );
    assert_eq!(query_text("   "), None);
    let masked = query_text("password=hunter2 login").unwrap();
    assert!(!masked.contains("hunter2"), "{masked}");
    let long = query_text(&"\u{e9}".repeat(1_500)).unwrap();
    assert!(long.len() <= crate::search_index::CHUNK_BYTES_MAX);
    assert!(long.chars().all(|letter| letter == '\u{e9}'));
}

#[test]
fn an_http_query_is_asked_once_embedded_by_the_indexer_then_read_back_with_its_text_cleared() {
    let mut db = db();
    let acme = plugin('a');
    let query = query_text("deploy steps axis7").unwrap();
    assert_eq!(cached_query(&db, &acme, &query), None);
    ask_query(&db, &acme.key, &query, 100).unwrap();
    ask_query(&db, &acme.key, &query, 200).unwrap();
    let rows: i64 = db
        .query_row("SELECT COUNT(*) FROM search_query_vectors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
    let mut seen = Vec::new();
    assert_eq!(embed_queries(&mut db, &acme, &mut by_axis(&mut seen)), 1);
    assert_eq!(seen, [query.clone()]);
    assert_eq!(cached_query(&db, &acme, &query), Some(unit(7)));
    let text: Option<String> = db
        .query_row("SELECT query FROM search_query_vectors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(text, None, "the query text is not kept once embedded");
    assert_eq!(
        cached_query(&db, &plugin('b'), &query),
        None,
        "keyed by plugin"
    );
}

#[test]
fn queries_are_bounded_per_plugin_and_per_round_and_a_failed_send_is_dropped() {
    let mut db = db();
    let acme = plugin('a');
    for index in 0..300 {
        ask_query(&db, &acme.key, &format!("query {index}"), index).unwrap();
    }
    let rows: i64 = db
        .query_row("SELECT COUNT(*) FROM search_query_vectors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows as usize, QUERY_ROWS_MAX);
    let mut seen = Vec::new();
    assert_eq!(
        embed_queries(&mut db, &acme, &mut by_axis(&mut seen)),
        QUERY_ROUND_MAX
    );
    assert_eq!(
        seen.first().map(String::as_str),
        Some("query 299"),
        "newest first"
    );
    let mut failing = |_: &[&str]| -> Result<Vec<Vec<f32>>, EngineError> {
        Err(EngineError("app_plugin_unreachable".into()))
    };
    assert_eq!(embed_queries(&mut db, &acme, &mut failing), 0);
    let waiting: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM search_query_vectors WHERE vector IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        waiting as usize,
        QUERY_ROWS_MAX - 2 * QUERY_ROUND_MAX,
        "a failed send is dropped, never resent every round"
    );
}

#[test]
fn ask_query_and_cached_query_mask_the_query_themselves() {
    let mut db = db();
    let acme = plugin('a');
    let raw = "  password=hunter2 deploy axis3 ";
    ask_query(&db, &acme.key, raw, 100).unwrap();
    let stored: String = db
        .query_row("SELECT query FROM search_query_vectors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(!stored.contains("hunter2"), "{stored}");
    assert_eq!(Some(stored.clone()), query_text(raw));
    let mut seen = Vec::new();
    assert_eq!(embed_queries(&mut db, &acme, &mut by_axis(&mut seen)), 1);
    assert!(seen.iter().all(|text| !text.contains("hunter2")));
    assert!(
        cached_query(&db, &acme, raw).is_some(),
        "the raw query finds its masked row"
    );
    ask_query(&db, &acme.key, "   ", 200).unwrap();
    let rows: i64 = db
        .query_row("SELECT COUNT(*) FROM search_query_vectors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1, "a query with nothing left is not stored");
}

#[test]
fn an_http_backoff_grows_per_failure_is_capped_and_resets_on_success() {
    let mut backoff = Backoff::new();
    let key = plugin('a').key;
    assert!(backoff.ready(&key, 0));
    backoff.failed(&key, 0);
    assert!(!backoff.ready(&key, BACKOFF_FIRST_MS - 1));
    assert!(backoff.ready(&key, BACKOFF_FIRST_MS));
    backoff.failed(&key, BACKOFF_FIRST_MS);
    assert!(
        !backoff.ready(&key, 3 * BACKOFF_FIRST_MS - 1),
        "the second wait is twice the first"
    );
    assert!(backoff.ready(&key, 3 * BACKOFF_FIRST_MS));
    for _ in 0..40 {
        backoff.failed(&key, 0);
    }
    assert!(!backoff.ready(&key, BACKOFF_MAX_MS - 1));
    assert!(backoff.ready(&key, BACKOFF_MAX_MS), "the wait is capped");
    assert!(
        backoff.ready(&plugin('b').key, 0),
        "another plugin is not held"
    );
    backoff.succeeded(&key);
    assert!(backoff.ready(&key, 0));
    backoff.failed(&key, 0);
    assert!(
        backoff.ready(&key, BACKOFF_FIRST_MS),
        "a success starts the waits over"
    );
}

#[cfg(unix)]
#[test]
fn the_local_model_stamp_follows_the_files_so_a_deleted_or_replaced_model_is_dropped() {
    use crate::semantic::local;
    let root = tempfile::tempdir().unwrap();
    assert_eq!(crate::semantic::local_stamp(root.path()), None);
    let place = local::folder(root.path());
    std::fs::create_dir_all(&place).unwrap();
    let make = || {
        for pin in local::PINNED {
            std::fs::File::create(place.join(pin.name))
                .unwrap()
                .set_len(pin.bytes)
                .unwrap();
        }
    };
    make();
    let first =
        crate::semantic::local_stamp(root.path()).expect("both files at their pinned sizes");
    let held = std::fs::File::open(place.join(local::MODEL.name)).unwrap();
    for pin in local::PINNED {
        std::fs::remove_file(place.join(pin.name)).unwrap();
    }
    assert_eq!(
        crate::semantic::local_stamp(root.path()),
        None,
        "a deleted model is never served from the cache"
    );
    make();
    let second = crate::semantic::local_stamp(root.path()).unwrap();
    assert_ne!(second, first, "a new download is a new model");
    std::fs::File::options()
        .write(true)
        .open(place.join(local::TOKENIZER.name))
        .unwrap()
        .set_len(1)
        .unwrap();
    assert_eq!(
        crate::semantic::local_stamp(root.path()),
        None,
        "a file off its pinned size is not the model"
    );
    drop(held);
}

#[cfg(unix)]
#[test]
fn the_built_in_model_note_names_missing_or_failed_files_and_says_loading_only_before_an_open() {
    use crate::semantic::{local, local_note, open_local};
    let root = tempfile::tempdir().unwrap();
    assert_eq!(local_note(root.path()), NOTE_MODEL_MISSING);
    let place = local::folder(root.path());
    std::fs::create_dir_all(&place).unwrap();
    let make = || {
        for pin in local::PINNED {
            std::fs::File::create(place.join(pin.name))
                .unwrap()
                .set_len(pin.bytes)
                .unwrap();
        }
    };
    make();
    assert_eq!(
        local_note(root.path()),
        NOTE_LOADING,
        "files there and no open yet"
    );
    assert!(
        open_local(root.path(), 1_000).is_none(),
        "zeroed files fail their check"
    );
    assert_eq!(local_note(root.path()), NOTE_MODEL_FAILED);
    assert!(open_local(root.path(), 2_000).is_none());
    assert_eq!(
        local_note(root.path()),
        NOTE_MODEL_FAILED,
        "never loading for good"
    );
    let held = std::fs::File::open(place.join(local::TOKENIZER.name)).unwrap();
    for pin in local::PINNED {
        std::fs::remove_file(place.join(pin.name)).unwrap();
    }
    assert_eq!(local_note(root.path()), NOTE_MODEL_MISSING);
    make();
    assert_eq!(
        local_note(root.path()),
        NOTE_LOADING,
        "a new download is tried again"
    );
    drop(held);
}

#[test]
fn fusion_adds_reciprocal_ranks_with_k_60_and_keeps_full_text_order_on_ties() {
    assert_eq!(fuse(&[1, 2, 3], &[3, 4]), [3, 1, 2, 4]);
    assert_eq!(fuse(&[1, 2], &[]), [1, 2]);
    assert_eq!(fuse(&[], &[5, 6]), [5, 6]);
    assert_eq!(fuse(&[1, 1, 2], &[2]), [2, 1], "a repeated id counts once");
}

#[test]
fn rank_reads_only_the_projects_vectors_of_the_asked_source_with_app_wide_memories_for_recall() {
    let mut db = db();
    let acme = plugin('a');
    let session = chunk(&db, Some("alpha"), Source::Session, "axis1 session", 10);
    let memory = chunk(&db, Some("alpha"), Source::Memory, "axis1 memory", 11);
    let everyone = chunk(&db, None, Source::Memory, "axis1 everyone", 12);
    let other = chunk(&db, Some("beta"), Source::Session, "axis1 beta", 13);
    round(&mut db, "alpha", &acme, &wide());
    round(&mut db, "beta", &acme, &wide());
    let alpha = project("alpha");
    let sessions = rank(&db, &alpha, Source::Session, &acme, &unit(1), &[], &wide());
    assert_eq!((sessions.ids, sessions.note), (vec![session], None));
    let memories = rank(&db, &alpha, Source::Memory, &acme, &unit(1), &[], &wide());
    let mut ids = memories.ids.clone();
    ids.sort_unstable();
    assert_eq!(ids, [memory, everyone]);
    assert!(!memories.ids.contains(&other));
}

#[test]
fn rank_puts_the_nearest_first_and_fuses_at_most_50_vector_hits() {
    let mut db = db();
    let acme = plugin('a');
    for at in 0..60 {
        chunk(&db, Some("alpha"), Source::Session, "axis0 filler", at);
    }
    let near = chunk(&db, Some("alpha"), Source::Session, "axis9 the one", 100);
    round(&mut db, "alpha", &acme, &wide());
    let ranked = rank(
        &db,
        &project("alpha"),
        Source::Session,
        &acme,
        &unit(9),
        &[],
        &wide(),
    );
    assert_eq!(ranked.ids.first(), Some(&near));
    assert_eq!(ranked.ids.len(), NEAREST_MAX);
}

#[test]
fn rank_scores_at_most_the_newest_scan_bytes_and_says_older_parts_rank_by_words() {
    let mut db = db();
    let acme = plugin('a');
    let oldest = chunk(&db, Some("alpha"), Source::Session, "axis5 old", 10);
    let middle = chunk(&db, Some("alpha"), Source::Session, "axis0 mid", 20);
    let newest = chunk(&db, Some("alpha"), Source::Session, "axis0 new", 30);
    round(&mut db, "alpha", &acme, &wide());
    let bounds = Bounds {
        scan_bytes: 2 * ONE,
        ..wide()
    };
    let ranked = rank(
        &db,
        &project("alpha"),
        Source::Session,
        &acme,
        &unit(5),
        &[oldest],
        &bounds,
    );
    assert_eq!(ranked.note, Some(NOTE_NEWEST));
    assert_eq!(
        ranked.ids[0], oldest,
        "full text still finds the older chunk"
    );
    let mut vector_part = ranked.ids.clone();
    vector_part.sort_unstable();
    assert_eq!(vector_part, [oldest, middle, newest]);
}

#[test]
fn rank_says_older_parts_rank_by_words_when_the_cap_left_chunks_out() {
    let mut db = db();
    let acme = plugin('a');
    let old = chunk(&db, Some("alpha"), Source::Session, "axis4 old", 10);
    let new = chunk(&db, Some("alpha"), Source::Session, "axis4 new", 20);
    let bounds = Bounds {
        project_bytes: ONE,
        ..wide()
    };
    assert_eq!(round(&mut db, "alpha", &acme, &bounds).0, 1);
    assert_eq!(stored(&db, &acme.key), [new]);
    let ranked = rank(
        &db,
        &project("alpha"),
        Source::Session,
        &acme,
        &unit(4),
        &[old],
        &wide(),
    );
    assert_eq!(
        ranked.note,
        Some(NOTE_NEWEST),
        "a chunk the cap left out is never a silent drop"
    );
    assert_eq!(ranked.ids, [old, new], "full text order wins the tie");
}

#[test]
fn a_scan_bound_filled_exactly_is_not_a_cut() {
    let mut db = db();
    let acme = plugin('a');
    let one = chunk(&db, Some("alpha"), Source::Session, "axis6 one", 10);
    let two = chunk(&db, Some("alpha"), Source::Session, "axis6 two", 20);
    round(&mut db, "alpha", &acme, &wide());
    let bounds = Bounds {
        scan_bytes: 2 * ONE,
        ..wide()
    };
    let ranked = rank(
        &db,
        &project("alpha"),
        Source::Session,
        &acme,
        &unit(6),
        &[],
        &bounds,
    );
    assert_eq!(ranked.note, None);
    assert_eq!(ranked.ids, [two, one], "equal scores rank newer first");
}

#[test]
fn rank_says_when_none_or_only_part_of_the_history_has_vectors() {
    let mut db = db();
    let acme = plugin('a');
    let one = chunk(&db, Some("alpha"), Source::Session, "axis2 one", 10);
    chunk(&db, Some("alpha"), Source::Session, "axis2 two", 20);
    let alpha = project("alpha");
    let none = rank(
        &db,
        &alpha,
        Source::Session,
        &acme,
        &unit(2),
        &[one],
        &wide(),
    );
    assert_eq!((none.ids, none.note), (vec![one], Some(NOTE_NO_VECTORS)));
    let bounds = Bounds {
        round_chunks: 1,
        ..wide()
    };
    round(&mut db, "alpha", &acme, &bounds);
    let part = rank(
        &db,
        &alpha,
        Source::Session,
        &acme,
        &unit(2),
        &[one],
        &wide(),
    );
    assert_eq!(part.note, Some(NOTE_PARTLY));
    round(&mut db, "alpha", &acme, &bounds);
    let whole = rank(
        &db,
        &alpha,
        Source::Session,
        &acme,
        &unit(2),
        &[one],
        &wide(),
    );
    assert_eq!(whole.note, None);
}

#[test]
fn a_stored_vector_of_the_wrong_length_is_skipped_when_ranking() {
    let mut db = db();
    let acme = plugin('a');
    let good = chunk(&db, Some("alpha"), Source::Session, "axis3 good", 10);
    let bad = chunk(&db, Some("alpha"), Source::Session, "axis3 bad", 20);
    round(&mut db, "alpha", &acme, &wide());
    db.execute(
        "UPDATE search_vectors SET vector=?1 WHERE chunk=?2",
        params![vec![0u8; 300], bad],
    )
    .unwrap();
    let ranked = rank(
        &db,
        &project("alpha"),
        Source::Session,
        &acme,
        &unit(3),
        &[],
        &wide(),
    );
    assert_eq!(ranked.ids, [good]);
}

#[test]
fn progress_counts_the_plugins_vectors_against_every_chunk() {
    let mut db = db();
    let acme = plugin('a');
    chunk(&db, Some("alpha"), Source::Session, "one", 10);
    chunk(&db, Some("alpha"), Source::Session, "two", 20);
    chunk(&db, Some("beta"), Source::Session, "three", 30);
    round(&mut db, "alpha", &acme, &wide());
    assert_eq!(
        progress(&db, &acme.key),
        Progress {
            embedded: 2,
            total: 3
        }
    );
    assert_eq!(
        progress(&db, &plugin('b').key),
        Progress {
            embedded: 0,
            total: 3
        }
    );
}
