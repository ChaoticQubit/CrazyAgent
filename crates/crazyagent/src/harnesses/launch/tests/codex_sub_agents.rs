use super::*;

fn codex_driver_in_a_turn() -> Driver {
    codex_driver_in_a_turn_keeping(false)
}

fn codex_driver_in_a_turn_keeping(native_threads: bool) -> Driver {
    let working_dir: PathBuf = std::env::temp_dir().components().collect();
    let cwd = working_dir.to_str().unwrap().to_owned();
    let home = working_dir.join("launch-test-home");
    let request = LaunchRequest {
        harness: "codex".into(),
        executable: PathBuf::from("/usr/bin/true"),
        working_dir,
        account: AccountBinding::Url {
            home: home.clone(),
            base_url: "http://127.0.0.1:11434".into(),
            key: None,
        },
        model: None,
        tools: native_threads,
        sandbox_profile: None,
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let (approval, sandbox) = if native_threads {
        ("never", json!({"type":"dangerFullAccess"}))
    } else {
        (
            "on-request",
            json!({"type":"readOnly","networkAccess":false}),
        )
    };
    let (mut driver, startup) = Driver::new(&request, native_threads, &[]).unwrap();
    assert_eq!(startup.len(), 1);
    assert!(driver.send("Reply with OK.").unwrap().is_none());
    let (_, writes) = driver
        .receive(&line(json!({"id":1,"result":{
            "userAgent":"unit-agent", "codexHome":home.to_str().unwrap(),
            "platformFamily":"unix", "platformOs":"macos"
        }})))
        .unwrap();
    assert_eq!(writes.len(), 2);
    let (events, writes) = driver
        .receive(&line(json!({"id":2,"result":{
            "thread":{
                "id":"own-thread", "cliVersion":"0.164.0", "createdAt":1, "updatedAt":1,
                "cwd":cwd, "ephemeral":false, "modelProvider":"unit-provider", "preview":"",
                "projectId":null, "sessionId":"own-session", "source":"appServer",
                "status":{"type":"idle"}, "turns":[]
            },
            "cwd":cwd, "model":"unit-model", "modelProvider":"unit-provider",
            "approvalPolicy":approval, "approvalsReviewer":"user",
            "sandbox":sandbox
        }})))
        .unwrap();
    assert!(matches!(events.as_slice(), [LaunchEvent::Ready { .. }]));
    assert_eq!(writes.len(), 1);
    driver
        .receive(&line(json!({"id":3,"result":{"turn":{
            "id":"own-turn", "rootTurnId":"own-turn", "items":[], "status":"inProgress"
        }}})))
        .unwrap();
    driver
}

#[test]
fn a_team_codex_agent_interrupts_a_codex_native_sub_agent_and_finishes_its_turn() {
    let mut driver = codex_driver_in_a_turn();
    let (events, writes) = driver
        .receive(&line(json!({"method":"turn/started","params":{
            "threadId":"child-thread",
            "turn":{"id":"child-turn","rootTurnId":"own-turn","items":[],"status":"inProgress"}
        }})))
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(writes.len(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&writes[0]).unwrap(),
        json!({"id":4,"method":"turn/interrupt","params":{
            "threadId":"child-thread","turnId":"child-turn"
        }})
    );
    let (events, writes) = driver
        .receive(&line(json!({"method":"item/agentMessage/delta","params":{
            "threadId":"child-thread", "turnId":"child-turn", "itemId":"child-item",
            "delta":"child text"
        }})))
        .unwrap();
    assert!(events.is_empty() && writes.is_empty());
    let (events, _) = driver
        .receive(&line(json!({"method":"turn/completed","params":{
            "threadId":"own-thread",
            "turn":{"id":"own-turn","items":[],"status":"completed"}
        }})))
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LaunchEvent::TurnFinished { success: true }))
    );
}

/// PA-7: a native Codex lead's sub-agent threads run on; the lead's own turn still ends
/// on its own `turn/completed`, whatever its children send.
#[test]
fn a_native_codex_lead_keeps_its_codex_sub_agents_running_and_finishes_its_turn() {
    let mut driver = codex_driver_in_a_turn_keeping(true);
    for frame in [
        json!({"method":"turn/started","params":{
            "threadId":"child-thread",
            "turn":{"id":"child-turn","rootTurnId":"own-turn","items":[],"status":"inProgress"}
        }}),
        json!({"method":"item/agentMessage/delta","params":{
            "threadId":"child-thread", "turnId":"child-turn", "itemId":"child-item",
            "delta":"child text"
        }}),
        json!({"method":"turn/completed","params":{
            "threadId":"child-thread",
            "turn":{"id":"child-turn","items":[],"status":"completed"}
        }}),
    ] {
        let (events, writes) = driver.receive(&line(frame)).unwrap();
        assert!(events.is_empty(), "{events:?}");
        assert!(
            writes.is_empty(),
            "a native lead never interrupts its sub-agents"
        );
    }
    let (events, writes) = driver
        .receive(&line(json!({"method":"turn/completed","params":{
            "threadId":"own-thread",
            "turn":{"id":"own-turn","items":[],"status":"completed"}
        }})))
        .unwrap();
    assert!(writes.is_empty());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LaunchEvent::TurnFinished { success: true }))
    );
}
