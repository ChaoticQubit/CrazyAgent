use super::*;

/// While a project move is pending, a master session cannot take a working copy of the
/// repository in the folder being moved; it can once the move is finished or forgotten.
#[test]
fn a_pending_project_move_refuses_a_master_working_copy_of_its_repository() {
    use crate::protocol::{Outcome, SessionId, TaskId};
    let case = Case::new();
    let project = ProjectId::new(PROJECT).unwrap();
    let reserve = |case: &Case| {
        let tx = case.db.unchecked_transaction().unwrap();
        crate::worktrees::reserve_master_bootstrap(
            &tx,
            &case.engine,
            &project,
            &TaskId::new("task-1").unwrap(),
            &SessionId::new("session-1").unwrap(),
            text(&case.repo),
        )
        .unwrap()
    };
    case.plan(AddMode::Move, &[]).unwrap();
    assert_eq!(
        reserve(&case),
        Outcome::Rejected {
            code: "move_project_in_progress".into()
        }
    );
    assert_eq!(case.finish(false), Some(Finished::Left));
    assert_eq!(
        reserve(&case),
        Outcome::Rejected {
            code: "repository_not_admitted".into()
        }
    );
}
