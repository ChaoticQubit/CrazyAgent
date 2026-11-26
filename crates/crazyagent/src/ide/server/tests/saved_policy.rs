use super::*;

#[test]
fn a_project_with_no_saved_policy_opens_sandboxed() {
    let data = scratch();
    let policy = ide_policy(data.path(), &project("none")).unwrap();
    assert_eq!(policy.access, AgentAccess::Sandboxed);
}
