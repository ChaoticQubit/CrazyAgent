use super::*;
use crate::sandbox::policy::{AgentAccess, AgentPolicy};

/// An engine store in memory with every table the engine makes at start and one project.
/// Input: the project. Output: the connection.
fn store(project: &ProjectId) -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO projects (id, body) VALUES (?1, '{}')",
            [project.as_str()],
        )
        .unwrap();
    connection
}

/// Saves one access level for the project through the owner's save, in its own immediate
/// transaction. Inputs: the store, the project, the version the owner saw and the level.
/// Output: the save's outcome.
fn save(
    connection: &mut Connection,
    project: &ProjectId,
    version: u64,
    access: AgentAccess,
) -> Outcome {
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let policy = AgentPolicy {
        access,
        ..AgentPolicy::default()
    };
    let outcome = set_agent_policy(&tx, project, version, &policy).unwrap();
    tx.commit().unwrap();
    outcome
}

/// On Windows, where agents run Sandboxed alone in this version, saving Own branch is
/// refused with `agent_access_unavailable` and Full access with its O3a code, and neither
/// writes a policy; saving Sandboxed applies and reads back.
#[test]
fn on_windows_saving_sandboxed_applies_and_a_wider_level_is_refused_by_its_code() {
    let project = ProjectId::new("access-windows").unwrap();
    let mut connection = store(&project);
    for (access, code) in [
        (AgentAccess::OwnBranch, "agent_access_unavailable"),
        (AgentAccess::Full, "sandbox_full_unavailable_windows"),
    ] {
        assert_eq!(
            save(&mut connection, &project, 0, access),
            rejected(code),
            "{access:?}"
        );
        assert!(
            !crate::sandbox::policy::saved(&connection, project.as_str()).unwrap(),
            "a refused {access:?} writes no policy"
        );
    }
    assert_eq!(
        save(&mut connection, &project, 0, AgentAccess::Sandboxed),
        Outcome::Applied
    );
    assert_eq!(
        crate::sandbox::policy::load(&connection, project.as_str())
            .unwrap()
            .access,
        AgentAccess::Sandboxed
    );
}
