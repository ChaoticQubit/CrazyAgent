use super::*;

#[test]
fn written_settings_turn_off_automatic_tasks() {
    let keys = settings();
    assert_eq!(
        keys.get("task.allowAutomaticTasks"),
        Some(&Value::from("off"))
    );
    assert_eq!(
        keys.get("security.workspace.trust.enabled"),
        Some(&Value::Bool(false))
    );
    assert_eq!(keys.len(), 2);
}
