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
