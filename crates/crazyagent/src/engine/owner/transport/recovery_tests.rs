use super::*;

fn root() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    (directory, root)
}

/// Sends one dismiss through this protocol's request parsing and handler, as an owner
/// client's frame body. Inputs: the engine root, the kind and number. Output: Ok once
/// dismissed, or the refusal code.
fn dismiss(root: &Path, kind: ActionKind, id: i64) -> Result<(), EngineError> {
    let body = serde_json::json!({
        "recovery": {"kind": "dismiss", "action_kind": kind, "id": id}
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Recovery));
    let reply = run(root, parse(&bytes)?)?;
    assert!(matches!(reply, Reply::Dismissed));
    Ok(())
}

#[test]
fn the_owner_dismisses_a_notice_once_and_never_an_intent_still_checked() {
    let (_directory, root) = root();
    let ledger = ledger::open(&root).unwrap();
    let checked = ledger::begin(
        &ledger,
        ActionKind::AgentSpawn,
        "team-3",
        "{\"team_agent\":3,\"pane\":true}",
    )
    .unwrap();
    let notice = ledger::begin(
        &ledger,
        ActionKind::AgentSpawn,
        "team-4",
        "{\"team_agent\":4,\"pane\":false,\"unknown\":true}",
    )
    .unwrap();
    let accept = ledger::begin(
        &ledger,
        ActionKind::LandingAccept,
        "set-1",
        "{\"project\":\"p\",\"change_set\":\"set-1\",\"landed\":false}",
    )
    .unwrap();

    let refused = [
        dismiss(&root, ActionKind::AgentSpawn, checked.get())
            .unwrap_err()
            .0,
        dismiss(&root, ActionKind::WorktreeCreate, notice.get())
            .unwrap_err()
            .0,
    ];
    assert_eq!(refused, ["ledger_not_notice", "ledger_missing"]);
    dismiss(&root, ActionKind::AgentSpawn, notice.get()).unwrap();
    dismiss(&root, ActionKind::LandingAccept, accept.get()).unwrap();
    let again = dismiss(&root, ActionKind::AgentSpawn, notice.get())
        .unwrap_err()
        .0;
    assert_eq!(again, "ledger_finished");

    let left = ledger::pending(&ledger, ActionKind::AgentSpawn).unwrap();
    assert_eq!(
        left.iter().map(|action| action.id).collect::<Vec<_>>(),
        vec![checked]
    );
    assert!(
        ledger::pending(&ledger, ActionKind::LandingAccept)
            .unwrap()
            .is_empty()
    );
    let outcome: String = ledger
        .query_row(
            "SELECT outcome FROM ledger_actions WHERE id=?1",
            [accept.get()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "failed");
    for code in refused.iter().chain([&again]) {
        assert!(RECOVERY_CODES.contains(&code.as_str()), "{code}");
    }
}

#[test]
fn malformed_or_oversized_recovery_requests_are_refused_before_the_ledger() {
    for body in [
        r#"{"recovery":{"kind":"dismiss","action_kind":"agent_spawn"}}"#,
        r#"{"recovery":{"kind":"dismiss","action_kind":"spawn","id":1}}"#,
        r#"{"recovery":{"kind":"dismiss","action_kind":"agent_spawn","id":1,"outcome":"done"}}"#,
        r#"{"recovery":{"kind":"retry","id":1}}"#,
        r#"{"recovery":{"kind":"dismiss","action_kind":"agent_spawn","id":"1"}}"#,
    ] {
        assert_eq!(
            parse(body.as_bytes()).unwrap_err().0,
            "recovery_request_invalid",
            "{body}"
        );
    }
    let long = format!(
        r#"{{"recovery":{{"kind":"dismiss","action_kind":"agent_spawn","id":1}},"pad":"{}"}}"#,
        "x".repeat(REQUEST_BYTES_MAX)
    );
    assert_eq!(
        parse(long.as_bytes()).unwrap_err().0,
        "recovery_request_bounds"
    );
}

#[test]
fn recovery_codes_cover_every_refusal_of_the_protocol_and_stay_sorted() {
    let source = include_str!("recovery.rs");
    let found: Vec<&str> = ["failure(\"", "code: \""]
        .into_iter()
        .flat_map(|opening| source.split(opening).skip(1))
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert!(found.len() >= 7, "{found:?}");
    for code in found {
        assert!(
            RECOVERY_CODES.contains(&code),
            "{code} is not in RECOVERY_CODES"
        );
    }
    let mut sorted = RECOVERY_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, RECOVERY_CODES);
}
