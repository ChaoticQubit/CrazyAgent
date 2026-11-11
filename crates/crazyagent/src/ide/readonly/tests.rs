use super::*;
use crate::ide::workspace::tests::{Agent, Case};

fn everything(agent: &Agent) -> String {
    format!("{}/**", agent.worktree.display())
}

fn file(agent: &Agent, path: &str) -> String {
    format!("{}/{path}", agent.worktree.display())
}

fn next_turn(case: &Case, agent: &Agent) {
    let changed = case
        .db
        .execute(
            "UPDATE team_agents SET turns=turns+1 WHERE id=?1",
            [agent.id],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

#[test]
fn readonly_globs_cover_running_agents_only() {
    let mut case = Case::new();
    let master = case.agent("master", "working");
    let working = case.agent("sub", "working");
    let starting = case.agent("sub", "starting");
    let idle_changed = case.agent("sub", "working");
    let stopped = case.agent("sub", "working");
    let failed = case.agent("sub", "working");
    case.change(&idle_changed);
    case.status(&idle_changed, "idle");
    case.status(&stopped, "stopped");
    case.status(&failed, "failed");
    let locked = settings(&case.db, &case.project).unwrap();
    assert_eq!(
        locked,
        WorkspaceSettings {
            readonly_include: vec![
                everything(&master),
                everything(&working),
                everything(&starting),
            ],
            readonly_exclude: vec![],
        }
    );
    unlock(&case.db, working.id, "src/lib.rs").unwrap();
    unlock(&case.db, working.id, "docs/a[1]*{x}?.md").unwrap();
    assert_eq!(
        unlock(&case.db, stopped.id, "src/lib.rs").unwrap_err().0,
        "ide_agent_not_running"
    );
    let unlocked = settings(&case.db, &case.project).unwrap();
    assert_eq!(unlocked.readonly_include, locked.readonly_include);
    assert_eq!(
        unlocked.readonly_exclude,
        vec![
            file(&working, "docs/a[[]1][*][{]x[}][?].md"),
            file(&working, "src/lib.rs"),
        ]
    );
    next_turn(&case, &working);
    assert_eq!(settings(&case.db, &case.project).unwrap(), locked);
    unlock(&case.db, working.id, "src/lib.rs").unwrap();
    assert_eq!(
        settings(&case.db, &case.project).unwrap().readonly_exclude,
        vec![file(&working, "src/lib.rs")]
    );
    case.status(&working, "idle");
    assert_eq!(
        settings(&case.db, &case.project).unwrap(),
        WorkspaceSettings {
            readonly_include: vec![everything(&master), everything(&starting)],
            readonly_exclude: vec![],
        }
    );
    let kept: i64 = case
        .db
        .query_row(
            "SELECT COUNT(*) FROM ide_unlocked WHERE agent=?1",
            [working.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 1);
}

#[test]
fn unlocked_files_stop_at_their_bound_per_agent() {
    let mut case = Case::new();
    let agent = case.agent("sub", "working");
    for index in 0..UNLOCKED_PER_AGENT_MAX {
        unlock(&case.db, agent.id, &format!("file-{index}")).unwrap();
    }
    assert_eq!(
        unlock(&case.db, agent.id, "one-more").unwrap_err().0,
        "ide_unlocked_full"
    );
    unlock(&case.db, agent.id, "file-0").unwrap();
    next_turn(&case, &agent);
    unlock(&case.db, agent.id, "one-more").unwrap();
    let kept: i64 = case
        .db
        .query_row(
            "SELECT COUNT(*) FROM ide_unlocked WHERE agent=?1",
            [agent.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 1);
}
