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

/// The lead socket and record paths the rows keep their records in. Output: the paths.
#[cfg(unix)]
fn lead_paths() -> LeadPaths {
    crate::terminal::tmux::test_seams::test_lead_paths()
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

/// PLCC-L1: a lead's start marks its teammates' server pin pending, so a record that is
/// missing or names another socket refuses every server instead of trusting the first to
/// answer. A pid at the record's bound is read and one past the largest is refused.
#[cfg(unix)]
#[test]
fn a_teammates_server_pin_record_missing_or_for_another_socket_refuses_every_server() {
    let root = tempfile::tempdir().unwrap();
    let lead = AgentId::team(1).unwrap();
    let record = crate::terminal::tmux::test_seams::test_layout::agent_teammates_server_pid(
        &root.path().join("team"),
        1,
    );
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    let earlier = renew_teammates_socket(root.path(), &lead, &lead_paths()).unwrap();
    keep_teammates_server_pid(root.path(), &lead, &earlier, 4242, &lead_paths()).unwrap();
    let latest = renew_teammates_socket(root.path(), &lead, &lead_paths()).unwrap();
    let refused = || Err(failure(OWN_SERVER_UNAVAILABLE));
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &earlier, &lead_paths()),
        refused(),
        "a socket the latest start did not name"
    );
    let name = latest.file_name().unwrap().to_str().unwrap();
    let old = earlier.file_name().unwrap().to_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(&record).unwrap(),
        format!("{name} pending")
    );
    for text in [
        format!("{old} 7"),
        format!("{old} pending"),
        format!("{name} Pending"),
        format!("{name} pendin"),
        format!("{name} pending "),
        format!("{name} 4294967296"),
    ] {
        std::fs::write(&record, &text).unwrap();
        assert_eq!(
            teammates_server_pid(root.path(), &lead, &latest, &lead_paths()),
            refused(),
            "{text:?}"
        );
    }
    std::fs::write(&record, format!("{name} 4294967295")).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &latest, &lead_paths()),
        Ok(Some(4_294_967_295))
    );
    std::fs::remove_file(&record).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &latest, &lead_paths()),
        refused(),
        "a missing record"
    );
}

/// A record write never collides with another writer's staged file: a file
/// staged under the name this process once used for every write, as a concurrent writer or
/// an earlier engine with the same pid left it, neither fails the write nor is removed by it.
#[cfg(unix)]
#[test]
fn a_record_write_leaves_another_writers_staged_file_alone_and_lands() {
    let root = tempfile::tempdir().unwrap();
    let kept = root.path().join("lead.pid");
    let theirs = kept.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&theirs, "staged by another writer").unwrap();
    assert_eq!(keep_record(&kept, "mine"), Ok(()));
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), "mine");
    assert_eq!(
        std::fs::read_to_string(&theirs).unwrap(),
        "staged by another writer"
    );
    let staged: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        staged.len(),
        2,
        "{staged:?}: the write leaves no staged file of its own"
    );
}

/// A native lead's teammates' server pid record (PLC c PC-L1) is read back for the socket it
/// was kept for, holds nothing for a later start's socket, and refuses every server once it
/// is damaged or swapped for a link.
#[cfg(unix)]
#[test]
fn a_teammates_server_pid_record_is_read_for_its_own_socket_and_refused_once_damaged() {
    let root = tempfile::tempdir().unwrap();
    let lead = AgentId::team(1).unwrap();
    let record = crate::terminal::tmux::test_seams::test_layout::agent_teammates_server_pid(
        &root.path().join("team"),
        1,
    );
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    let earlier = renew_teammates_socket(root.path(), &lead, &lead_paths()).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &earlier, &lead_paths()),
        Ok(None)
    );
    keep_teammates_server_pid(root.path(), &lead, &earlier, 4242, &lead_paths()).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &earlier, &lead_paths()),
        Ok(Some(4242))
    );
    let latest = renew_teammates_socket(root.path(), &lead, &lead_paths()).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &latest, &lead_paths()),
        Ok(None)
    );

    let name = latest.file_name().unwrap().to_str().unwrap();
    for text in [
        format!("{name} "),
        format!("{name} 0"),
        format!("{name} +7"),
        format!("{name} 7 8"),
        format!("{name} 99999999999"),
        format!("{name}7"),
        "teammates-x.sock 7".into(),
    ] {
        std::fs::write(&record, &text).unwrap();
        assert_eq!(
            teammates_server_pid(root.path(), &lead, &latest, &lead_paths()),
            Err(failure(OWN_SERVER_UNAVAILABLE)),
            "{text:?}"
        );
    }
    std::fs::remove_file(&record).unwrap();
    let elsewhere = root.path().join("elsewhere.pid");
    std::fs::write(&elsewhere, format!("{name} 7")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &record).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &latest, &lead_paths()),
        Err(failure(OWN_SERVER_UNAVAILABLE))
    );
}

/// The engine filters an owner paste as the desktop's
/// `pasteText` does: every C0 control, DEL and C1 control is dropped except tab, line feed
/// and carriage return, then CR LF and LF become CR, so a paste cannot press Escape, end a
/// bracketed paste or send a Ctrl key; other text passes unchanged.
#[test]
fn an_owner_paste_keeps_only_tab_and_enter_of_the_control_characters() {
    let every: String = (0u32..=0x9f).filter_map(char::from_u32).collect();
    let expected: Vec<u8> = b"\t\r\r".iter().copied().chain(0x20u8..0x7f).collect();
    assert_eq!(paste_keys(&every), expected);
    assert_eq!(paste_keys("a\r\nb\nc\rd"), b"a\rb\rc\rd");
    assert_eq!(paste_keys("x\x1b[201~y\x03z"), b"x[201~yz");
    assert_eq!(
        paste_keys("\u{e9}\u{6f22}\u{1f642}"),
        "\u{e9}\u{6f22}\u{1f642}".as_bytes()
    );
    assert!(paste_keys("\x00\x1b\u{9b}\x7f").is_empty());
}

/// A host whose app pane request answers one code and that records what it types by name:
/// a tmux host whose control connect failed, or a host with no pane ids.
struct PinRefused {
    answer: &'static str,
    typed: std::sync::Mutex<Vec<Vec<u8>>>,
}

impl Host for PinRefused {
    fn spawn(&self, _: &AgentId, _: &PaneCommand) -> Result<PaneRef, HostError> {
        Err(failure("terminal_unavailable"))
    }

    fn attach(&self, _: &AgentId) -> Result<TerminalStream, HostError> {
        Err(failure("terminal_unavailable"))
    }

    fn input(&self, _: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        self.typed.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }

    fn resize(&self, _: &AgentId, _: u16, _: u16) -> Result<(), HostError> {
        Ok(())
    }

    fn kill(&self, _: &AgentId) -> Result<(), HostError> {
        Ok(())
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        Ok(Vec::new())
    }

    fn app_pane_at(&self, _: &AgentId) -> Result<PaneAt, HostError> {
        Err(failure(self.answer))
    }
}

/// Only a host that answers `PINS_UNSUPPORTED` types a prompt by name. A host
/// whose app pane request failed otherwise, as a tmux control connect can fail once, fails
/// the prompt and types nothing by name, where the active pane may be a teammate's.
#[test]
fn a_failed_app_pane_request_fails_the_prompt_and_types_nothing_by_name() {
    let agent = AgentId::team(1).unwrap();
    let failed = PinRefused {
        answer: "terminal_unavailable",
        typed: std::sync::Mutex::default(),
    };
    let refused = type_prompt(&failed, &agent, "hello");
    assert_eq!(refused, Err(failure("terminal_unavailable")));
    assert!(
        failed.typed.lock().unwrap().is_empty(),
        "nothing typed by name"
    );
    let unpinned = PinRefused {
        answer: PINS_UNSUPPORTED,
        typed: std::sync::Mutex::default(),
    };
    assert_eq!(type_prompt(&unpinned, &agent, "hello"), Ok(true));
    let keys = [prompt_keys("hello", true).unwrap(), b"\r".to_vec()].concat();
    assert_eq!(unpinned.typed.lock().unwrap().concat(), keys);
}
