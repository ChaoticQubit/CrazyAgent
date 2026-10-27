use serde_json::{Map, Value};

/// The settings that keep every CrazyAgent worktree trusted, so Source Control works in
/// each section, and that stop tasks from starting by themselves when a folder opens.
/// Output: the setting keys and their values, for the engine's locked settings.
pub fn settings() -> Map<String, Value> {
    let mut keys = Map::new();
    keys.insert(
        "security.workspace.trust.enabled".into(),
        Value::Bool(false),
    );
    keys.insert("task.allowAutomaticTasks".into(), Value::from("off"));
    keys
}

#[cfg(test)]
mod tests;
