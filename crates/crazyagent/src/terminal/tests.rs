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

/// A lead's short temp folder for one row, made private as an agent start makes it and
/// removed with everything in it when the row ends, also when it fails. Holds the home.
#[cfg(unix)]
struct LeadTmp(std::path::PathBuf);

#[cfg(unix)]
impl Drop for LeadTmp {
    fn drop(&mut self) {
        let _ = crate::sandbox::toolchains::remove_tmp(&self.0);
    }
}

/// Whether a path is a socket itself, read without following a link. Input: the path.
/// Output: the answer.
#[cfg(unix)]
fn is_socket(path: &std::path::Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

/// KA-T17B-R2-F1 (rv-code2 kept-cap r1 F1, QUESTION-04:15 B): a lead's next start puts a
/// plain file over each teammates' socket of its earlier starts that nothing listens at, so
/// they never pile up toward the freeze's cap and no listener can bind there again. One a
/// server still answers at stays until a later start finds it dead, a plain file under a
/// teammates' name is left as it is, and no staged file is left behind.
#[cfg(unix)]
#[test]
fn a_leads_next_start_puts_a_plain_file_over_earlier_teammates_sockets_nothing_listens_at() {
    let disk = tempfile::tempdir().unwrap();
    let root = disk.path();
    let lead = AgentId::team(1).unwrap();
    let team = root.join("team");
    let record = crate::team::runtime::agent_teammates_server_pid(&team, 1);
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    let home = crate::team::runtime::agent_home(&team, 1);
    std::fs::create_dir_all(&home).unwrap();
    let tmp = LeadTmp(home);
    let folder = crate::sandbox::toolchains::make_tmp(&tmp.0).unwrap();
    let planted = folder.join("teammates-00000000000000ff.sock");
    std::fs::write(&planted, "not a socket").unwrap();

    let dead = renew_teammates_socket(root, &lead).unwrap();
    assert_eq!(dead.parent(), Some(folder.as_path()));
    drop(crate::spawn_gate::bind_unix_std(&dead).unwrap());
    assert!(is_socket(&dead));
    let live = renew_teammates_socket(root, &lead).unwrap();
    assert!(
        std::fs::symlink_metadata(&dead).is_ok_and(|metadata| metadata.is_file()),
        "a socket nothing listens at becomes a plain file at the next start"
    );
    assert!(
        crate::spawn_gate::bind_unix_std(&dead).is_err(),
        "no listener can bind where the dead socket was"
    );

    let server = crate::spawn_gate::bind_unix_std(&live).unwrap();
    renew_teammates_socket(root, &lead).unwrap();
    assert!(is_socket(&live), "a socket a server still answers at stays");
    drop(server);
    renew_teammates_socket(root, &lead).unwrap();
    assert!(
        std::fs::symlink_metadata(&live).is_ok_and(|metadata| metadata.is_file()),
        "once its server is gone, a later start retires it"
    );
    assert_eq!(
        std::fs::read_to_string(&planted).unwrap(),
        "not a socket",
        "a plain file under a teammates' name stays"
    );
    let staged: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(staged.is_empty(), "{staged:?}: no staged file is left");
}

/// KA-KEPT-CAP-R2-NOTES Note 2 (rv-secreview kept-cap r2): the plain file that takes a dead
/// teammates' socket's place is staged in the lead's own temp folder, which no teammate
/// reads or writes, never in the short temp folder they share. A link in that folder's place
/// or a folder open to others is never staged in, so the dead socket stays. A missing one,
/// which the lead removed as its own TMPDIR, is made private first, so the next start still
/// retires the socket, and no staged file is left in either folder.
#[cfg(unix)]
#[test]
fn a_dead_teammates_socket_is_retired_only_through_the_leads_own_private_temp_folder() {
    use std::os::unix::fs::PermissionsExt;
    let disk = tempfile::tempdir().unwrap();
    let root = disk.path();
    let lead = AgentId::team(1).unwrap();
    let team = root.join("team");
    let record = crate::team::runtime::agent_teammates_server_pid(&team, 1);
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    let home = crate::team::runtime::agent_home(&team, 1);
    std::fs::create_dir_all(&home).unwrap();
    let tmp = LeadTmp(home);
    let folder = crate::sandbox::toolchains::make_tmp(&tmp.0).unwrap();
    let own = crate::sandbox::toolchains::native_lead_tmp(&folder);
    assert_eq!(own.parent(), Some(folder.as_path()));
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), &own).unwrap();
    let dead = renew_teammates_socket(root, &lead).unwrap();
    drop(crate::spawn_gate::bind_unix_std(&dead).unwrap());
    assert!(is_socket(&dead));

    renew_teammates_socket(root, &lead).unwrap();
    assert!(
        is_socket(&dead),
        "a link in the folder's place is never staged in"
    );
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    std::fs::remove_file(&own).unwrap();
    std::fs::create_dir(&own).unwrap();
    std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o755)).unwrap();
    renew_teammates_socket(root, &lead).unwrap();
    assert!(
        is_socket(&dead),
        "a folder open to others is never staged in"
    );

    std::fs::remove_dir(&own).unwrap();
    renew_teammates_socket(root, &lead).unwrap();
    assert!(
        std::fs::symlink_metadata(&dead).is_ok_and(|metadata| metadata.is_file()),
        "a missing lead's own temp folder is made, and the dead socket becomes a plain file"
    );
    let made = std::fs::symlink_metadata(&own).unwrap();
    assert!(made.is_dir(), "the lead's own temp folder is a real folder");
    assert_eq!(made.permissions().mode() & 0o077, 0, "it is made private");
    for place in [&folder, &own] {
        let staged: Vec<_> = std::fs::read_dir(place)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            staged.is_empty(),
            "{staged:?} in {place:?}: no staged file is left"
        );
    }
}

/// PLCC-L1: a lead's start marks its teammates' server pin pending, so a record that is
/// missing or names another socket refuses every server instead of trusting the first to
/// answer. A pid at the record's bound is read and one past the largest is refused.
#[cfg(unix)]
#[test]
fn a_teammates_server_pin_record_missing_or_for_another_socket_refuses_every_server() {
    let root = tempfile::tempdir().unwrap();
    let lead = AgentId::team(1).unwrap();
    let record = crate::team::runtime::agent_teammates_server_pid(&root.path().join("team"), 1);
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    let earlier = renew_teammates_socket(root.path(), &lead).unwrap();
    keep_teammates_server_pid(root.path(), &lead, &earlier, 4242).unwrap();
    let latest = renew_teammates_socket(root.path(), &lead).unwrap();
    let refused = || Err(failure(OWN_SERVER_UNAVAILABLE));
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &earlier),
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
            teammates_server_pid(root.path(), &lead, &latest),
            refused(),
            "{text:?}"
        );
    }
    std::fs::write(&record, format!("{name} 4294967295")).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &latest),
        Ok(Some(4_294_967_295))
    );
    std::fs::remove_file(&record).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &latest),
        refused(),
        "a missing record"
    );
}

/// A record write never collides with another writer's staged file (RV-DS241-22): a file
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
    let record = crate::team::runtime::agent_teammates_server_pid(&root.path().join("team"), 1);
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    let earlier = renew_teammates_socket(root.path(), &lead).unwrap();
    assert_eq!(teammates_server_pid(root.path(), &lead, &earlier), Ok(None));
    keep_teammates_server_pid(root.path(), &lead, &earlier, 4242).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &earlier),
        Ok(Some(4242))
    );
    let latest = renew_teammates_socket(root.path(), &lead).unwrap();
    assert_eq!(teammates_server_pid(root.path(), &lead, &latest), Ok(None));

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
            teammates_server_pid(root.path(), &lead, &latest),
            Err(failure(OWN_SERVER_UNAVAILABLE)),
            "{text:?}"
        );
    }
    std::fs::remove_file(&record).unwrap();
    let elsewhere = root.path().join("elsewhere.pid");
    std::fs::write(&elsewhere, format!("{name} 7")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &record).unwrap();
    assert_eq!(
        teammates_server_pid(root.path(), &lead, &latest),
        Err(failure(OWN_SERVER_UNAVAILABLE))
    );
}
