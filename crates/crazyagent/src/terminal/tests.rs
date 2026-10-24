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
