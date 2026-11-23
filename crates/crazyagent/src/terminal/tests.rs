use super::*;
use kernel_processes::terminal::ended_code;

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

/// V243-PANE-SIZE-ACROSS-WINDOWS: a sizer's input-only attach keeps the wire shape an engine
/// without `follows` reads, an attach without the key reads as a sizer, and a follower's
/// carries `"follows":true` both ways.
#[test]
fn an_input_only_attach_names_follows_only_for_a_follower() {
    let sizer = TerminalRequest::AttachInput {
        agent_id: AgentId::team(3).unwrap(),
        pane: None,
        cols: 80,
        rows: 24,
        follows: false,
    };
    let shape = r#"{"kind":"attach_input","agent_id":"team-3","pane":null,"cols":80,"rows":24}"#;
    assert_eq!(serde_json::to_string(&sizer).unwrap(), shape);
    assert_eq!(
        serde_json::from_str::<TerminalRequest>(shape).unwrap(),
        sizer
    );
    let follower = TerminalRequest::AttachInput {
        agent_id: AgentId::team(3).unwrap(),
        pane: None,
        cols: 80,
        rows: 24,
        follows: true,
    };
    let followed = r#"{"kind":"attach_input","agent_id":"team-3","pane":null,"cols":80,"rows":24,"follows":true}"#;
    assert_eq!(serde_json::to_string(&follower).unwrap(), followed);
    assert_eq!(
        serde_json::from_str::<TerminalRequest>(followed).unwrap(),
        follower
    );
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
        APP_PANE_UNKNOWN,
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

/// #686 r1 F2: an unknown app pane of a live window is a refusal, not an ended terminal, so
/// an owner view stays open and is never told its agent stopped; an unknown agent still ends.
#[test]
fn an_unknown_app_pane_is_refused_not_read_as_an_ended_terminal() {
    assert!(!ended_code(APP_PANE_UNKNOWN));
    assert!(ended_code("terminal_agent_unknown"));
    assert_ne!(
        plain_reason(APP_PANE_UNKNOWN),
        plain_reason("terminal_agent_unknown")
    );
}

/// The hooks crazyagent opens its tmux host with name every team path as the kernel rows'
/// copy of the team state layout does, so a layout change that the copy misses fails here.
#[cfg(unix)]
#[test]
fn the_tmux_hooks_name_every_team_path_as_the_kernel_test_layout_does() {
    use kernel_processes::terminal::tmux::test_seams::{test_hooks, test_lead_paths};
    let real = tmux::tmux_hooks();
    let copy = test_hooks();
    let (lead, lead_copy) = (tmux::lead_paths(), test_lead_paths());
    for state in [Path::new("/private/tmp/team"), Path::new("/a b/team")] {
        for id in [1, 42, i64::MAX] {
            assert_eq!(
                (real.agent_profile)(state, id),
                (copy.agent_profile)(state, id)
            );
            assert_eq!(
                (real.agent_teammates_profile)(state, id),
                (copy.agent_teammates_profile)(state, id)
            );
            for mates in [false, true] {
                assert_eq!(
                    (real.agent_servers_state)(state, id, mates),
                    (copy.agent_servers_state)(state, id, mates)
                );
            }
            assert_eq!(
                (lead.agent_home)(state, id),
                (lead_copy.agent_home)(state, id)
            );
            assert_eq!(
                (lead.agent_teammates_socket_name)(state, id),
                (lead_copy.agent_teammates_socket_name)(state, id)
            );
            assert_eq!(
                (lead.agent_teammates_server_pid)(state, id),
                (lead_copy.agent_teammates_server_pid)(state, id)
            );
            assert_eq!(
                (lead.agent_servers_deadline)(state, id),
                (lead_copy.agent_servers_deadline)(state, id)
            );
            let home = (lead.agent_home)(state, id);
            assert_eq!((lead.tmp_path)(&home), (lead_copy.tmp_path)(&home));
        }
    }
    assert_eq!(FOLDER, kernel_processes::terminal::tmux::test_seams::FOLDER);
}

/// The confinement crazyagent opens its tmux host with holds the sandbox's own runner,
/// keeper and shim words and variables, and the kernel rows' copy holds the same.
#[cfg(unix)]
#[test]
fn the_confinement_value_matches_the_sandbox_constants() {
    let real = crate::sandbox::confinement();
    let copy = kernel_processes::terminal::tmux::test_seams::test_confinement();
    assert_eq!(real.sandbox_exec, crate::sandbox::SANDBOX_EXEC);
    assert_eq!(copy.sandbox_exec, crate::sandbox::SANDBOX_EXEC);
    #[cfg(target_os = "macos")]
    {
        assert_eq!(real.keeper, crate::sandbox::KEEPER);
        assert_eq!(copy.keeper, crate::sandbox::KEEPER);
        assert_eq!(real.pane_shim, crate::sandbox::PANE_SHIM);
        assert_eq!(copy.pane_shim, crate::sandbox::PANE_SHIM);
        assert_eq!(
            real.keeper_deadline_variable,
            crate::sandbox::KEEPER_DEADLINE
        );
        assert_eq!(
            copy.keeper_deadline_variable,
            crate::sandbox::KEEPER_DEADLINE
        );
        assert_eq!(real.keeper_state_variable, crate::sandbox::KEEPER_STATE);
        assert_eq!(copy.keeper_state_variable, crate::sandbox::KEEPER_STATE);
    }
}

/// The kernel rows' copy of the pane keeper keeps and takes back out every command, freeze
/// record included, exactly as the sandbox's own keeper does.
#[cfg(target_os = "macos")]
#[test]
fn the_kernel_keeper_copy_keeps_and_unkeeps_as_the_sandbox_does() {
    use kernel_processes::terminal::tmux::test_seams as copy;
    let plain = |profile: &str| -> Vec<String> {
        [
            crate::sandbox::SANDBOX_EXEC,
            "-f",
            profile,
            "/usr/local/bin/claude",
            "--resume",
        ]
        .map(String::from)
        .to_vec()
    };
    let longest = format!("/{}", "a".repeat(1000));
    let longer = format!("/{}", "a".repeat(1001));
    let records = [
        None,
        Some("/private/tmp/team/state"),
        Some(longest.as_str()),
        Some(longer.as_str()),
        Some("private/tmp/state"),
        Some("/private/tmp/a\nb"),
        Some("/"),
    ];
    let argvs = [
        plain("/private/tmp/lead.sb"),
        plain("/private/tmp/other.sb"),
        vec![
            "/bin/sh".into(),
            "-f".into(),
            "/p.sb".into(),
            "/bin/x".into(),
        ],
        plain("/private/tmp/lead.sb")[..3].to_vec(),
    ];
    for argv in &argvs {
        for record in records {
            let record = record.map(Path::new);
            let kept = crate::sandbox::kept_pane(argv.clone(), record);
            assert_eq!(
                kept,
                copy::kept_pane(argv.clone(), record),
                "{argv:?} {record:?}"
            );
            if let Some(kept) = kept {
                assert_eq!(
                    crate::sandbox::unkept_pane(&kept),
                    copy::unkept_pane(&kept),
                    "{kept:?}"
                );
                let mut bent = kept.clone();
                bent[crate::sandbox::KEEPER.len() - 2].push(' ');
                assert_eq!(crate::sandbox::unkept_pane(&bent), copy::unkept_pane(&bent));
            }
        }
        assert_eq!(crate::sandbox::unkept_pane(argv), copy::unkept_pane(argv));
    }
}

/// The kernel rows' copy of the pane variable gate refuses every name the sandbox's own
/// gate refuses for a plain value, in any letter case.
#[cfg(unix)]
#[test]
fn the_kernel_variable_gate_copy_refuses_the_names_the_sandbox_refuses() {
    use std::ffi::OsStr;
    let copy = kernel_processes::terminal::tmux::test_seams::test_confinement().variable_allowed;
    for name in [
        "TERM",
        "LANG",
        "PATH",
        "PERL5LIB",
        "PERL5OPT",
        "PERLLIB",
        "PERL5DB",
        "perl5opt",
        "CRAZYAGENT_KEEPER_STATE",
        "CRAZYAGENT_KEEPER_DEADLINE",
        "crazyagent_warden_fd",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "SSH_AUTH_SOCK",
        "GIT_ASKPASS",
        "GIT_CONFIG_KEY_0",
        "AWS_REGION",
        "OPENAI_API_KEY",
        "MY_SECRET",
        "DB_PASSWORD",
    ] {
        let value = OsStr::new("/private/tmp/planted");
        assert_eq!(
            crate::sandbox::agent_variable_allowed(OsStr::new(name), value),
            copy(OsStr::new(name), value),
            "{name}"
        );
    }
}
