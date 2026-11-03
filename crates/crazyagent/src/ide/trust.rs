use serde_json::{Map, Value};

/// The workspace trust settings of one project's IDE, which stop tasks from starting by
/// themselves when a folder opens in every case. A reviewed project keeps every CrazyAgent
/// worktree trusted, so Source Control works in each section; an untrusted one keeps VS
/// Code workspace trust on, so the folder opens in Restricted Mode (threat model D3).
/// Input: the project's repository trust. Output: the setting keys and their values, for
/// the engine's locked settings.
pub fn settings(trust: crate::trust::Trust) -> Map<String, Value> {
    let mut keys = Map::new();
    keys.insert(
        "security.workspace.trust.enabled".into(),
        Value::Bool(!crate::trust::Enforcement::for_level(trust).ide_workspace_trust),
    );
    keys.insert("task.allowAutomaticTasks".into(), Value::from("off"));
    keys
}

#[cfg(test)]
mod tests;
