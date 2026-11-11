use super::*;
use crate::roadmap_status::{ItemState, ItemStatus, RoadmapItem};
use serde_json::json;

const PROJECT: &str = "alpha";

/// An engine root whose database holds the engine tables and one set-up project.
/// Output: the temporary folder, the engine root and the project.
fn engine() -> (tempfile::TempDir, PathBuf, ProjectId) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-owner-roadmap-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("engine");
    let folder = base.join("project");
    for path in [&root, &folder] {
        std::fs::create_dir(path).unwrap();
    }
    let db = crate::questions::open(&root).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
    (tmp, root, ProjectId::new(PROJECT).unwrap())
}

/// Sends one request through this protocol's selector, parsing and handler, as an owner
/// client's frame body. Inputs: the engine root and the body. Output: the reply, or the
/// refusal.
fn send(root: &Path, body: serde_json::Value) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Roadmap));
    run(root, parse(&bytes)?)
}

fn status(root: &Path, after: u32) -> (RoadmapStatus, Option<u32>) {
    let body = json!({"roadmap": {"kind": "read_status", "project": PROJECT, "after": after}});
    match send(root, body).unwrap() {
        Reply::Status { status, next } => (status, next),
        other => panic!("expected a status, got {other:?}"),
    }
}

/// Holds the claim of the one status run a test starts.
struct Held(Option<roadmap_status::Claim>);

impl roadmap_status::Runs for Held {
    fn start(&mut self, _: &ProjectId, claim: roadmap_status::Claim) {
        self.0 = Some(claim);
    }
}

fn code(reply: Result<Reply, EngineError>) -> String {
    match reply {
        Err(problem) => problem.0,
        Ok(other) => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn the_owner_reads_the_status_asks_for_a_refresh_and_saves_each_setting() {
    let (_tmp, root, _project) = engine();
    let (first, next) = status(&root, 0);
    assert!(first.items.is_empty() && next.is_none());
    assert_eq!(first.refresh, Refresh::AfterLanding);
    assert!(!first.stale && !first.running && first.checked_ms.is_none());

    let refresh = || {
        send(
            &root,
            json!({"roadmap": {"kind": "refresh", "project": PROJECT}}),
        )
    };
    assert!(matches!(refresh().unwrap(), Reply::Done {}));
    assert!(status(&root, 0).0.stale);

    let set = |name: &str| {
        let body = json!({"roadmap": {"kind": "set_refresh", "project": PROJECT, "refresh": name}});
        send(&root, body)
    };
    for (name, saved) in [
        ("refresh_only", Refresh::RefreshOnly),
        ("off", Refresh::Off),
        ("after_landing", Refresh::AfterLanding),
    ] {
        assert!(matches!(set(name).unwrap(), Reply::Done {}), "{name}");
        assert_eq!(status(&root, 0).0.refresh, saved);
    }
    assert!(matches!(set("off").unwrap(), Reply::Done {}));
    assert_eq!(code(refresh()), "roadmap_status_off");
    assert!(!status(&root, 0).0.stale, "Off drops a waiting check");
}

#[test]
fn a_long_status_is_read_in_pages_that_each_fit_one_frame() {
    let (_tmp, root, project) = engine();
    let db = crate::questions::open(&root).unwrap();
    let states: Vec<ItemState> = (0..roadmap_status::ITEMS_MAX)
        .map(|n| ItemState {
            item: RoadmapItem {
                section: "\"".repeat(roadmap_status::SECTION_BYTES_MAX),
                text: format!("{n:03}{}", "\\".repeat(roadmap_status::ITEM_BYTES_MAX - 3)),
            },
            status: ItemStatus::InProgress,
            evidence: "\"".repeat(roadmap_status::EVIDENCE_BYTES_MAX),
        })
        .collect();
    roadmap_status::refresh(&db, &project).unwrap();
    let mut held = Held(None);
    assert_eq!(roadmap_status::tick(&db, &mut held, 900).unwrap(), 1);
    let outcome = Ok((states.as_slice(), "docs/plan.md"));
    assert!(roadmap_status::finish(&db, &project, held.0.unwrap(), outcome, 1000).unwrap());

    let (mut after, mut pages, mut held) = (0, 0, Vec::new());
    loop {
        let (page, next) = status(&root, after);
        assert_eq!(page.checked_ms, Some(1000));
        held.extend(page.items.clone());
        let frame = encode_frame(&Reply::Status { status: page, next }).unwrap();
        assert!(frame.len() <= crate::protocol::FRAME_BYTES_MAX + 4);
        pages += 1;
        match next {
            Some(next) => {
                assert!(next > after && next as usize == held.len());
                after = next;
            }
            None => break,
        }
        assert!(pages <= roadmap_status::ITEMS_MAX);
    }
    assert!(pages > 1, "the longest status needs more than one frame");
    assert_eq!(held, states);
    let (past, next) = status(&root, roadmap_status::ITEMS_MAX as u32 + 5);
    assert!(past.items.is_empty() && next.is_none());
}

#[test]
fn a_request_for_an_unknown_project_or_in_an_unknown_shape_is_refused() {
    let (_tmp, root, _project) = engine();
    for kind in ["read_status", "refresh"] {
        let body = json!({"roadmap": {"kind": kind, "project": "missing"}});
        assert_eq!(code(send(&root, body)), "roadmap_status_project_missing");
    }
    let body = json!({"roadmap": {"kind": "set_refresh", "project": "missing", "refresh": "off"}});
    assert_eq!(code(send(&root, body)), "roadmap_status_project_missing");
    let rows: i64 = crate::questions::open(&root)
        .unwrap()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM roadmap_status) + (SELECT COUNT(*) FROM roadmap_status_settings)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "a refused request stores nothing");

    for body in [
        json!({"roadmap": {"kind": "set_refresh", "project": PROJECT, "refresh": "sometimes"}}),
        json!({"roadmap": {"kind": "forget", "project": PROJECT}}),
        json!({"roadmap": {"kind": "refresh", "project": PROJECT, "now": true}}),
        json!({"roadmap": {"kind": "refresh", "project": "../alpha"}}),
    ] {
        assert_eq!(code(send(&root, body)), "roadmap_status_request_invalid");
    }
    let long = "x".repeat(REQUEST_BYTES_MAX);
    let body = json!({"roadmap": {"kind": "refresh", "project": long}});
    assert_eq!(code(send(&root, body)), "roadmap_status_request_bounds");
}
