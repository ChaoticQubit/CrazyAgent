use super::*;

#[test]
fn terminal_names_are_short_lowercase_words_with_inner_dashes() {
    let long = "a".repeat(AGENT_ID_BYTES_MAX + 1);
    for good in ["team-1", "research-42", "a", "x9"] {
        assert_eq!(AgentId::new(good).unwrap().as_str(), good);
    }
    for bad in [
        "",
        "-team",
        "team-",
        "Team-1",
        "team_1",
        "team.1",
        "team:1",
        "team 1",
        "_crazyagent",
        long.as_str(),
    ] {
        assert_eq!(
            AgentId::new(bad).unwrap_err().0,
            "terminal_agent_invalid",
            "{bad}"
        );
    }
}

#[test]
fn a_team_agent_terminal_is_named_after_its_team_agent_id() {
    let id = AgentId::team(7).unwrap();
    assert_eq!(id.as_str(), "team-7");
    assert_eq!(id.team_agent(), Some(7));
    assert_eq!(AgentId::team(0).unwrap_err().0, "terminal_agent_invalid");
    assert_eq!(AgentId::new("team-07").unwrap().team_agent(), None);
    assert_eq!(AgentId::new("research-7").unwrap().team_agent(), None);
}

#[test]
fn terminal_frames_use_the_documented_wire_shape() {
    let attach: TerminalRequest =
        serde_json::from_str(r#"{"kind":"attach","agent_id":"team-3"}"#).unwrap();
    assert_eq!(
        attach,
        TerminalRequest::Attach {
            agent_id: AgentId::team(3).unwrap()
        }
    );
    assert!(
        serde_json::from_str::<TerminalRequest>(r#"{"kind":"attach","agent_id":"Team 3"}"#)
            .is_err()
    );
    assert_eq!(
        serde_json::to_string(&TerminalRequest::Input {
            bytes: b"hi\r".to_vec()
        })
        .unwrap(),
        r#"{"kind":"input","bytes":[104,105,13]}"#
    );
    assert_eq!(
        serde_json::from_str::<TerminalRequest>(r#"{"kind":"prompt","text":"run the tests"}"#)
            .unwrap(),
        TerminalRequest::Prompt {
            text: "run the tests".into()
        }
    );
    assert_eq!(
        serde_json::to_string(&TerminalFrame::Attached {
            agent_id: AgentId::team(3).unwrap(),
            cols: 80,
            rows: 24
        })
        .unwrap(),
        r#"{"kind":"attached","agent_id":"team-3","cols":80,"rows":24}"#
    );
    let largest = TerminalFrame::Output {
        bytes: vec![255; CHUNK_BYTES_MAX],
    };
    assert!(crate::protocol::encode_frame(&largest).is_ok());
}

#[test]
fn every_terminal_code_has_plain_words() {
    for code in [
        "terminal_unavailable",
        "terminal_tmux_unsupported",
        "terminal_agent_unknown",
        "terminal_ended",
        "terminal_agent_running",
        "terminal_project_full",
        "terminal_viewers_full",
        "terminal_input_bounds",
        "terminal_size_bounds",
        "terminal_detached",
        "terminal_viewer_gone",
        "terminal_folder_unsafe",
        "terminal_not_answering",
        "owner_grant_stale_or_revoked",
    ] {
        let reason = plain_reason(code);
        assert_ne!(reason, plain_reason("something_else"), "{code}");
        assert!(
            !reason.contains('_') && reason.ends_with('.'),
            "{code}: {reason}"
        );
    }
    assert_eq!(
        plain_reason("terminal_agent_unknown"),
        "This agent has stopped, so its terminal is closed."
    );
}

#[test]
fn a_prompt_is_typed_as_one_submission_whatever_the_pane_mode() {
    let keys = |text: &str, bracketed: bool| {
        prompt_keys(text, bracketed).map(|bytes| String::from_utf8(bytes).unwrap())
    };
    assert_eq!(
        keys("run the tests  \n\n", true).as_deref(),
        Some("\x1b[200~run the tests\x1b[201~")
    );
    assert_eq!(
        keys("first\r\nsecond\rthird", true).as_deref(),
        Some("\x1b[200~first\rsecond\rthird\x1b[201~")
    );
    assert_eq!(
        keys("first\nsecond\n/run it", false).as_deref(),
        Some("first second /run it"),
        "without bracketed paste every line would be its own Enter, so lines join"
    );
    assert_eq!(
        keys("a\x1b[201~b\x07\tc", true).as_deref(),
        Some("\x1b[200~a[201~b\tc\x1b[201~"),
        "a prompt cannot end the paste or press keys"
    );
    assert_eq!(
        keys("\u{e9}t\u{e9}", false).as_deref(),
        Some("\u{e9}t\u{e9}")
    );
    assert_eq!(keys("   \n \x03", true), None);
    assert_eq!(
        keys(&"x".repeat(PROMPT_BYTES_MAX), false).map(|text| text.len()),
        Some(PROMPT_BYTES_MAX)
    );
    assert_eq!(keys(&"x".repeat(PROMPT_BYTES_MAX + 1), false), None);
}
