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
