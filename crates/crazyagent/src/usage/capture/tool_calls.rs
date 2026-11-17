use super::*;

/// Names the tool call one Codex rollout line records, the way adoption counts it: a
/// function or custom tool call by its name and parsed arguments, a local shell call by its
/// command, and a skill it read by `skill:` and the skill's name too. Arguments longer than
/// `ARGUMENTS_BYTES_MAX` count the call by its name alone. Input: the parsed line. Output:
/// the counted names, at most two, none for any other line.
pub(super) fn codex_tools(value: &Value) -> Vec<String> {
    let payload = &value["payload"];
    if value["type"] != "response_item" {
        return Vec::new();
    }
    let (name, input) = match payload["type"].as_str() {
        Some("function_call") => (
            payload["name"].as_str().unwrap_or_default(),
            payload["arguments"]
                .as_str()
                .filter(|arguments| arguments.len() <= ARGUMENTS_BYTES_MAX)
                .and_then(|arguments| serde_json::from_str(arguments).ok())
                .unwrap_or(Value::Null),
        ),
        Some("custom_tool_call") => (payload["name"].as_str().unwrap_or_default(), Value::Null),
        Some("local_shell_call") => ("local_shell_call", payload["action"].clone()),
        _ => return Vec::new(),
    };
    let names: Vec<String> = crate::coord::adoption::tool_key(name, &input)
        .into_iter()
        .chain(crate::coord::adoption::skill_key(name, &input))
        .collect();
    assert!(names.len() <= 2);
    names
}

/// Names the tool calls one Claude Code transcript line records, the way adoption counts
/// them: each `tool_use` item of an assistant line by its name and input, and a skill it
/// called or read by `skill:` and the skill's name too. Input: the parsed line. Output: the
/// counted names, at most two a call, none for any other line.
pub(super) fn claude_tools(value: &Value) -> Vec<String> {
    if value["type"] != "assistant" {
        return Vec::new();
    }
    let mut names = Vec::new();
    for item in value["message"]["content"].as_array().into_iter().flatten() {
        if item["type"] != "tool_use" {
            continue;
        }
        let name = item["name"].as_str().unwrap_or_default();
        let called: Vec<String> = crate::coord::adoption::tool_key(name, &item["input"])
            .into_iter()
            .chain(crate::coord::adoption::skill_key(name, &item["input"]))
            .collect();
        assert!(called.len() <= 2);
        names.extend(called);
    }
    names
}
