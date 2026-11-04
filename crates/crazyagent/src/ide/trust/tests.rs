use super::*;

#[test]
fn written_settings_turn_off_automatic_tasks() {
    let keys = settings(crate::trust::Trust::Reviewed);
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

#[test]
fn an_untrusted_project_keeps_workspace_trust_on_so_the_ide_opens_restricted() {
    let keys = settings(crate::trust::Trust::Untrusted);
    assert_eq!(
        keys.get("security.workspace.trust.enabled"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        keys.get("task.allowAutomaticTasks"),
        Some(&Value::from("off"))
    );
    assert_eq!(keys.len(), 2);
}
