use super::*;

#[test]
fn every_production_auth_control_is_rejected_by_worker_bridge_without_state_changes() {
    let mut protocol = ready();
    let mut controls = vec![begin_browser(&mut protocol, "native-worker-denied")];
    controls.push(auth(
        protocol
            .receive(&completion(json!({
                "loginId":"native-worker-denied","success":false,"error":AUTH_ERROR
            })))
            .unwrap(),
    ));
    let outgoing = request(&protocol.start_device_code_login().unwrap());
    controls.push(auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        device_result("native-device-denied"),
    )));
    let outgoing = request(&protocol.cancel_login("native-device-denied").unwrap());
    controls.push(auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        json!({"status":"canceled"}),
    )));
    let outgoing = request(&protocol.read_account(false).unwrap());
    controls.push(auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        json!({
            "account":{"type":"chatgpt","email":EMAIL,"planType":"plus"},"requiresOpenaiAuth":false
        }),
    )));
    let outgoing = request(&protocol.logout_account().unwrap());
    controls.push(auth(reply(
        &mut protocol,
        outgoing["id"].as_i64().unwrap(),
        json!({}),
    )));
    let outgoing = request(&protocol.read_account(false).unwrap());
    controls.push(auth(
        protocol
            .receive(&wire(
                json!({"id":outgoing["id"],"error":{"code":-1,"message":AUTH_ERROR}}),
            ))
            .unwrap(),
    ));

    let mut bridge = bridge();
    for control in controls {
        assert_eq!(
            bridge.translate(Event::AuthControl(control), None),
            Err(BridgeError::Unsupported)
        );
    }
    let initialized = bridge.translate(Event::Initialized, None).unwrap();
    assert_eq!(initialized.kind, EventKind::ProtocolInitialized);
    assert_eq!(initialized.native_thread_id, None);
    bridge
        .translate(
            Event::ThreadStarted {
                thread_id: "native-coding-thread".into(),
            },
            None,
        )
        .unwrap();
    let initialized = bridge.translate(Event::Initialized, None).unwrap();
    assert_eq!(
        initialized.native_thread_id.as_deref(),
        Some("native-coding-thread")
    );
    assert_eq!(
        bridge.support(Capability::NativeAdmission),
        Support::Unavailable
    );
}

#[test]
fn worker_bridge_also_rejects_legacy_auth_errors_and_unsupported_auth_method_events() {
    let mut bridge = bridge();
    for method in [LOGIN, COMPLETED, READ, CANCEL, LOGOUT] {
        assert_eq!(
            bridge.translate(
                Event::RequestFailed {
                    id: 2,
                    method,
                    code: -1,
                    message: AUTH_ERROR.into()
                },
                None
            ),
            Err(BridgeError::Unsupported)
        );
        assert_eq!(
            bridge.translate(
                Event::UnsupportedNotification {
                    method: method.into()
                },
                None
            ),
            Err(BridgeError::Unsupported)
        );
        assert_eq!(
            bridge.translate(
                Event::UnsupportedRequest {
                    id: WireId::Number(42),
                    method: method.into()
                },
                None
            ),
            Err(BridgeError::Unsupported)
        );
    }
    assert_eq!(
        bridge
            .translate(Event::Initialized, None)
            .unwrap()
            .native_thread_id,
        None
    );
}
