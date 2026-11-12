use super::*;
use crate::scorecard::{Baseline, DailyQuestion, Settings, Window};
use serde_json::json;

/// An engine root whose database holds the authority schemas, the scorecard's own tables
/// included. Output: the temporary folder and the engine root.
fn engine() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-owner-scorecard-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = tmp.path().canonicalize().unwrap().join("engine");
    std::fs::create_dir(&root).unwrap();
    let db = crate::questions::open(&root).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    (tmp, root)
}

/// Sends one request through this protocol's selector, parsing and handler, as an owner
/// client's frame body. Inputs: the engine root and the body. Output: the reply, or the
/// refusal.
fn send(root: &Path, body: serde_json::Value) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Scorecard));
    run(root, parse(&bytes)?)
}

fn read(root: &Path) -> crate::scorecard::Scorecard {
    match send(
        root,
        json!({"scorecard": {"kind": "report", "offset_minutes": 60}}),
    )
    .unwrap()
    {
        Reply::Report { scorecard } => *scorecard,
        other => panic!("expected a report, got {other:?}"),
    }
}

#[test]
fn the_owner_reads_the_scorecard_and_saves_its_settings_by_version() {
    let (_tmp, root) = engine();
    let first = read(&root);
    assert_eq!(first.version, 0);
    assert_eq!(first.settings, Settings::default());
    assert_eq!(first.real, 0);
    assert_eq!(first.targets, crate::scorecard::TARGETS);
    let settings = Settings {
        window: Window::FromDay("2026-11-03".into()),
        baseline: Baseline::PreviousWindow,
        daily_question: DailyQuestion::On,
    };
    let save = |version: u64| {
        send(
            &root,
            json!({"scorecard": {"kind": "set", "settings": settings, "version": version}}),
        )
        .unwrap()
    };
    assert!(matches!(save(0), Reply::Saved { version: 1 }));
    assert!(matches!(save(0), Reply::Stale { current: 1 }));
    let second = read(&root);
    assert_eq!(second.version, 1);
    assert_eq!(second.settings, settings);
    assert_eq!(
        serde_json::to_value(&second.settings).unwrap(),
        json!({
            "window": {"from_day": "2026-11-03"},
            "baseline": "previous_window",
            "daily_question": "on"
        })
    );
}

#[test]
fn a_malformed_or_oversized_request_is_refused_before_it_runs() {
    let (_tmp, root) = engine();
    let refused = |body: serde_json::Value| send(&root, body).unwrap_err().0;
    assert_eq!(
        refused(json!({"scorecard": {"kind": "report", "offset_minutes": 60, "extra": 1}})),
        "scorecard_request_invalid"
    );
    assert_eq!(
        refused(json!({"scorecard": {"kind": "report", "offset_minutes": 15 * 60}})),
        "scorecard_request_invalid"
    );
    for offset_minutes in [i32::MIN, i32::MAX, -15 * 60] {
        assert_eq!(
            refused(json!({"scorecard": {"kind": "report", "offset_minutes": offset_minutes}})),
            "scorecard_request_invalid",
            "{offset_minutes}"
        );
    }
    let settings = |window: serde_json::Value| {
        json!({"scorecard": {"kind": "set", "version": 0, "settings": {
            "window": window,
            "baseline": "first_days",
            "daily_question": "off"
        }}})
    };
    assert_eq!(
        refused(settings(json!("weekly"))),
        "scorecard_request_invalid"
    );
    assert_eq!(
        refused(settings(json!({"from_day": "2026-02-30"}))),
        "scorecard_settings_invalid"
    );
    let padded = json!({
        "scorecard": {"kind": "report", "offset_minutes": 0},
        "pad": "x".repeat(REQUEST_BYTES_MAX)
    });
    let bytes = serde_json::to_vec(&padded).unwrap();
    assert_eq!(parse(&bytes).unwrap_err().0, "scorecard_request_bounds");
    assert_eq!(read(&root).version, 0);
}

#[test]
fn every_scorecard_code_is_listed_for_the_clients() {
    for code in [
        "scorecard_request_bounds",
        "scorecard_request_invalid",
        "scorecard_reply_bounds",
        "scorecard_settings_invalid",
        "scorecard_unavailable",
        "scorecard_outcome_unknown",
    ] {
        assert!(SCORECARD_CODES.contains(&code), "{code}");
    }
    let mut sorted = SCORECARD_CODES.to_vec();
    sorted.sort_unstable();
    assert_eq!(sorted, SCORECARD_CODES);
}
