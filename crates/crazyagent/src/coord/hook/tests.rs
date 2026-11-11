use super::*;

/// Logs one hook input's tool call for the lead into a fresh real mailbox folder and reads
/// back what the tool log holds. Input: the hook input. Output: the log text.
fn logged(input: Value) -> String {
    let folder = tempfile::tempdir().unwrap();
    let mailbox = Mailbox::open(folder.path()).unwrap();
    log_tools(&mailbox, LEAD_TAG, &input);
    let key = relay::tag_key(LEAD_TAG).unwrap();
    std::fs::read_to_string(folder.path().join(format!("{key}{TOOLS}"))).unwrap_or_default()
}

#[test]
fn log_tools_counts_each_skill_by_name_and_graphify_once() {
    assert_eq!(
        logged(json!({"tool_name": "Skill", "tool_input": {"skill": "brainstorming"}})),
        "Skill\nskill:brainstorming\n"
    );
    assert_eq!(
        logged(json!({"tool_name": "Skill", "tool_input": {"skill": "superpowers:tdd"}})),
        "Skill\nskill:superpowers:tdd\n"
    );
    assert_eq!(
        logged(json!({"tool_name": "Skill", "tool_input": {"skill": "graphify"}})),
        "Skill\ngraphify\nskill:graphify\n"
    );
    assert_eq!(
        logged(json!({
            "tool_name": "Bash",
            "tool_input": {"command": "cat /h/.claude/skills/tdd/SKILL.md"}
        })),
        "Bash\nskill:tdd\n"
    );
    assert_eq!(
        logged(json!({
            "tool_name": "Bash",
            "tool_input": {"command": "/usr/local/bin/graphify query \"pools\""}
        })),
        "Bash\ngraphify\n"
    );
    assert_eq!(
        logged(json!({
            "tool_name": "Bash",
            "tool_input": {"command": "cd /repo && graphify query \"pools\""}
        })),
        "Bash\ngraphify\n",
        "a command line that runs graphify after another program counts it too"
    );
    assert_eq!(
        logged(json!({"tool_name": "Skill", "tool_input": {"skill": "bad name"}})),
        "Skill\n",
        "a skill name that is not plain is not counted by name"
    );
    assert_eq!(
        logged(json!({"tool_name": "bad tool", "tool_input": {}})),
        "",
        "a tool name that is not plain logs nothing"
    );
}

/// Under the FirstEdit nudges (DEC-111 R4) a session's first edit before any code-graph
/// query gets one reminder as `additionalContext` and no permission decision, so the edit
/// stays allowed; its next edit gets none. A session that ran graphify or a graphify MCP
/// tool first gets none, and the Off and TenMinutes options never remind on an edit.
#[test]
fn the_first_edit_without_a_graph_query_gets_one_reminder_and_is_allowed() {
    let (root, folder) = mailbox();
    let roster = |nudges: Nudges| {
        let bytes = serde_json::to_vec(&relay::Roster {
            overlaps: Overlaps::Warn,
            nudges,
            root: None,
        })
        .unwrap();
        relay::put(&folder, relay::ROSTER_FILE, &bytes).unwrap();
    };
    let session = |tag: &str| Identity {
        inbox: Some(tag.to_owned()),
        activity: Some(tag.to_owned()),
        teammate: None,
        cwd: None,
        unnamed: false,
        part: relay::Part::Single,
    };
    let input = json!({
        "tool_name": "Edit",
        "tool_input": {"file_path": root.path().join("a.rs"), "old_string": "a", "new_string": "b"},
    });
    let edit = |tag: &str| edit_answer(&folder, None, &session(tag), &input);

    for quiet in [Nudges::Off, Nudges::TenMinutes] {
        roster(quiet);
        assert_eq!(edit(LEAD_TAG), None, "{quiet:?} never reminds on an edit");
    }
    roster(Nudges::FirstEdit);
    let first = edit(LEAD_TAG).expect("the first edit without a graph query is reminded");
    assert_eq!(
        first,
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": "You are editing before any code-graph query. Run graphify query first if this touches code you have not mapped.",
        }}),
        "a reminder, never a decision: the edit stays allowed"
    );
    assert_eq!(edit(LEAD_TAG), None, "one reminder per session");

    log_tools(
        &folder,
        "%7",
        &json!({"tool_name": "Bash", "tool_input": {"command": "graphify query \"pools\""}}),
    );
    log_tools(
        &folder,
        "%8",
        &json!({"tool_name": "mcp__graphify__query_graph", "tool_input": {"question": "pools"}}),
    );
    assert_eq!(edit("%7"), None, "a session that queried the graph first");
    assert_eq!(edit("%8"), None, "a graphify MCP query counts too");
    assert!(edit("%9").is_some(), "each session is reminded on its own");
}

use crate::coord::{BODY_BYTES_MAX, MessageId, MessageKind};

fn mailbox() -> (tempfile::TempDir, Mailbox) {
    let root = tempfile::Builder::new()
        .prefix("ca-hook-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let folder = Mailbox::open(root.path()).unwrap();
    (root, folder)
}

fn post(folder: &Mailbox, id: MessageId, kind: MessageKind, body: &str) -> String {
    let letter = Letter {
        id,
        to: LEAD_TAG.to_owned(),
        from: "Ada".to_owned(),
        kind,
        question: None,
        work: None,
        reply_to: None,
        body: body.to_owned(),
    };
    let stem = relay::letter_stem(id).unwrap();
    relay::put(
        folder,
        &format!("{stem}{INBOX}"),
        &serde_json::to_vec(&letter).unwrap(),
    )
    .unwrap();
    stem
}

#[test]
fn delivered_letters_say_which_take_a_reply_and_which_take_a_note() {
    let (root, folder) = mailbox();
    let note = post(&folder, 1, MessageKind::Note, "Overlap on calc.py.");
    let prompt = post(&folder, 2, MessageKind::Prompt, "Write the docs.");
    let text = deliver(&folder, LEAD_TAG).unwrap();
    assert_eq!(
        text,
        "CrazyAgent team messages for you (2). Answer a question or work for you with update_work kind reply and its id in reply_to. A note takes no reply; to answer one, send kind note with to.\n\n[crazyagent msg 1] from Ada (note, it takes no reply):\n> Overlap on calc.py.\n\n[crazyagent msg 2] from Ada (work for you):\n> Write the docs."
    );
    for stem in [&note, &prompt] {
        assert!(root.path().join(format!("{stem}{SHOWN}")).is_file());
        assert!(!root.path().join(format!("{stem}{INBOX}")).exists());
    }
    assert_eq!(deliver(&folder, LEAD_TAG), None);
}

#[test]
fn a_full_inbox_is_handed_over_within_the_context_budget() {
    let (root, folder) = mailbox();
    let body = "x".repeat(BODY_BYTES_MAX);
    for id in 1..=INBOX_MAX as MessageId {
        post(&folder, id, MessageKind::Note, &body);
    }
    let text = deliver(&folder, LEAD_TAG).unwrap();
    assert!(text.chars().count() <= CONTEXT_CHARS_MAX);
    assert!(
        text.starts_with("CrazyAgent team messages for you (2). "),
        "{}",
        &text[..120]
    );
    let waiting = std::fs::read_dir(root.path())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(INBOX)
        })
        .count();
    assert_eq!(waiting, INBOX_MAX - 2);
}

/// A Codex `apply_patch` call names its files in its patch (PA-12 proof on Codex 0.167.1):
/// every added, updated, deleted and moved-to file, placed under the call's own folder, at
/// most `PATCH_PATHS_MAX`, and a longer patch is `TooMany` (PA1213-F2). A shell call
/// carrying a patch, whose working folder or `cd` the hook cannot see, and a patch with no
/// absolute folder are `Unplaced` (PA1213-F1). Claude Code's edits keep their one absolute
/// file, and a shell call with no patch names none.
#[test]
fn a_codex_patch_names_every_file_it_writes_under_its_cwd() {
    let patch = "*** Begin Patch\n*** Add File: a.txt\n+hi\n*** Update File: src/b.rs\n*** Move to: src/c.rs\n@@\n-x\n+y\n*** Delete File: /abs/d.rs\n*** End Patch\n";
    let named = |tool: &str, command: String, cwd: &str| {
        edited(&json!({"tool_name": tool, "tool_input": {"command": command}, "cwd": cwd}))
    };
    let expected: Vec<PathBuf> = ["/w/a.txt", "/w/src/b.rs", "/w/src/c.rs", "/abs/d.rs"]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    assert_eq!(
        named("apply_patch", patch.to_owned(), "/w"),
        Edit::Files(expected)
    );
    for shell in [
        format!("apply_patch <<'EOF'\n{patch}EOF\n"),
        format!("cd src && apply_patch <<'EOF'\n{patch}EOF\n"),
    ] {
        assert_eq!(named("Bash", shell, "/w"), Edit::Unplaced);
    }
    assert_eq!(named("apply_patch", patch.to_owned(), "w"), Edit::Unplaced);
    assert_eq!(
        named("Bash", "ls -la".to_owned(), "/w"),
        Edit::Files(Vec::new())
    );
    assert_eq!(
        named("Bash", "echo '*** Add File: a.txt'".to_owned(), "/w"),
        Edit::Files(Vec::new())
    );
    let files = |count: usize| -> String {
        let body: String = (0..count)
            .map(|n| format!("*** Add File: f{n}.txt\n+x\n"))
            .collect();
        format!("*** Begin Patch\n{body}*** End Patch\n")
    };
    assert!(matches!(
        named("apply_patch", files(PATCH_PATHS_MAX), "/w"),
        Edit::Files(all) if all.len() == PATCH_PATHS_MAX
    ));
    assert_eq!(
        named("apply_patch", files(PATCH_PATHS_MAX + 1), "/w"),
        Edit::TooMany
    );
    assert_eq!(
        edited(
            &json!({"tool_name": "Write", "tool_input": {"file_path": "/w/e.rs", "content": ""}})
        ),
        Edit::Files(vec![PathBuf::from("/w/e.rs")])
    );
    assert_eq!(
        edited(&json!({"tool_name": "Write", "tool_input": {"file_path": "e.rs"}})),
        Edit::Files(Vec::new())
    );
}

/// A Codex agent's edit hook runs the edit guard alone (PA-12): it never hands the
/// first-edit reminder, which the engine gives Codex from its rollouts, it holds an edit of
/// a session the engine placed nowhere, and it keeps a native teammate's patch inside the
/// teammate's own worktree with the same answer Claude Code gets.
#[test]
fn a_codex_edit_runs_the_guard_alone() {
    let (root, folder) = mailbox();
    let base = root.path().to_path_buf();
    let roster = |overlaps: Overlaps| {
        let bytes = serde_json::to_vec(&relay::Roster {
            overlaps,
            nudges: Nudges::FirstEdit,
            root: Some(base.to_string_lossy().into_owned()),
        })
        .unwrap();
        relay::put(&folder, relay::ROSTER_FILE, &bytes).unwrap();
    };
    let who = |teammate: Option<&str>, unnamed: bool| Identity {
        inbox: Some(LEAD_TAG.to_owned()),
        activity: Some(LEAD_TAG.to_owned()),
        teammate: teammate.map(str::to_owned),
        cwd: None,
        unnamed,
        part: relay::Part::Single,
    };
    let patch = |file: &str| {
        json!({
            "tool_name": "apply_patch",
            "tool_input": {"command": format!("*** Begin Patch\n*** Add File: {file}\n+x\n*** End Patch\n")},
            "cwd": base,
        })
    };
    assert_eq!(Event::parse("codex-edit"), Some(Event::CodexEdit));
    assert_eq!(
        Event::CodexEdit.timeout_seconds(),
        Event::Edit.timeout_seconds()
    );

    roster(Overlaps::Warn);
    assert_eq!(
        in_mailbox(Event::CodexEdit, &folder, &who(None, false), &patch("a.rs")),
        None,
        "no first-edit reminder from a Codex hook"
    );
    let claude = json!({"tool_name": "Edit", "tool_input": {"file_path": base.join("a.rs")}});
    assert!(
        in_mailbox(Event::Edit, &folder, &who(None, false), &claude).is_some(),
        "Claude Code's edit hook still reminds"
    );
    let held = in_mailbox(Event::CodexEdit, &folder, &who(None, true), &patch("a.rs")).unwrap();
    assert_eq!(held["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(
        held["hookSpecificOutput"]["permissionDecisionReason"],
        UNNAMED_EDIT
    );
    assert_eq!(
        in_mailbox(
            Event::CodexEdit,
            &folder,
            &who(None, true),
            &json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": base})
        ),
        None,
        "a shell call that edits nothing is never held, named or not"
    );

    roster(Overlaps::Block);
    let outside = in_mailbox(
        Event::CodexEdit,
        &folder,
        &who(Some("ana"), false),
        &patch("a.rs"),
    )
    .unwrap();
    assert_eq!(outside["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(outside["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(
        outside["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .starts_with("CrazyAgent: you are teammate ana."),
        "{outside}"
    );
    assert_eq!(
        in_mailbox(
            Event::CodexEdit,
            &folder,
            &who(None, false),
            &json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": base})
        ),
        None,
        "a shell call with no patch edits nothing"
    );

    let reason = |answer: Option<Value>| {
        answer.unwrap()["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let shell = json!({
        "tool_name": "Bash",
        "tool_input": {"command": "cd src && apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: a.rs\n@@\n-x\n+y\n*** End Patch\nEOF\n"},
        "cwd": base,
    });
    let wide: String = (0..=PATCH_PATHS_MAX)
        .map(|n| format!("*** Add File: f{n}.rs\n+x\n"))
        .collect();
    let wide = json!({
        "tool_name": "apply_patch",
        "tool_input": {"command": format!("*** Begin Patch\n{wide}*** End Patch\n")},
        "cwd": base,
    });
    for (overlaps, refused) in [
        (Overlaps::Block, true),
        (Overlaps::Wait, true),
        (Overlaps::Warn, false),
        (Overlaps::Off, false),
    ] {
        roster(overlaps);
        let me = who(None, false);
        let answers = [
            in_mailbox(Event::CodexEdit, &folder, &me, &shell),
            in_mailbox(Event::CodexEdit, &folder, &me, &wide),
        ];
        assert_eq!(answers.iter().all(Option::is_some), refused, "{overlaps:?}");
        if refused {
            let [shell, wide] = answers;
            assert_eq!(reason(shell), UNPLACED_PATCH, "PA1213-F1");
            assert_eq!(reason(wide), PATCH_TOO_LARGE, "PA1213-F2");
        }
    }

    roster(Overlaps::Block);
    let answering = {
        let path = base.clone();
        std::thread::spawn(move || {
            let folder = Mailbox::open(&path).unwrap();
            for _ in 0..250 {
                if let Some((_, stem)) = folder.entries(ASK).into_iter().next() {
                    let bytes =
                        relay::read_entry(&folder, &format!("{stem}{ASK}"), relay::ASK_BYTES_MAX)
                            .unwrap();
                    let answer = serde_json::to_vec(&Answer {
                        held: vec![Held {
                            by: "Linus".to_owned(),
                            pattern: "src/**".to_owned(),
                        }],
                    })
                    .unwrap();
                    relay::put(&folder, &format!("{stem}{ANSWER}"), &answer).unwrap();
                    return serde_json::from_slice::<Ask>(&bytes).unwrap();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("the guard asked nothing");
        })
    };
    let two = json!({
        "tool_name": "apply_patch",
        "tool_input": {"command": "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: src/b.rs\n+z\n*** End Patch\n"},
        "cwd": base,
    });
    let held = in_mailbox(Event::CodexEdit, &folder, &who(None, false), &two);
    let asked = answering.join().unwrap();
    assert_eq!(
        (
            asked.to.as_str(),
            asked.path.as_str(),
            asked.more.as_slice()
        ),
        (LEAD_TAG, "src/a.rs", ["src/b.rs".to_owned()].as_slice()),
        "one question covers the whole patch (PA1213-F2)"
    );
    assert_eq!(
        reason(held),
        "CrazyAgent: files this patch writes are claimed by Linus (src/**). Overlapping edits are set to Block in this project: message the holder with update_work, or start_work on other files."
    );
}
