use super::*;

#[test]
fn a_task_branch_takes_only_a_plain_lower_case_name() {
    assert_eq!(task_branch("docs-2").as_deref(), Some("crazyagent/docs-2"));
    assert_eq!(
        task_branch(&"a".repeat(NAME_BYTES_MAX)).as_deref(),
        Some(format!("crazyagent/{}", "a".repeat(NAME_BYTES_MAX)).as_str())
    );
    for refused in [
        "",
        "Docs",
        "-docs",
        "docs-",
        "a--b",
        "a/b",
        "a b",
        "a.b",
        "a_b",
        "é",
        "a".repeat(NAME_BYTES_MAX + 1).as_str(),
    ] {
        assert_eq!(task_branch(refused), None, "{refused:?}");
    }
}

#[test]
fn a_teammate_branch_hangs_off_its_task_with_a_double_hyphen() {
    let task = "crazyagent/docs";
    assert_eq!(
        teammate_branch(task, "Ada Lovelace").as_deref(),
        Some("crazyagent/docs--ada-lovelace")
    );
    assert_eq!(
        teammate_branch(task, "--Linus__T.--").as_deref(),
        Some("crazyagent/docs--linus-t")
    );
    assert_eq!(teammate_branch(task, " -_. "), None);
    assert_eq!(teammate_branch(task, ""), None);
    let long = teammate_branch(task, &format!("{}-tail", "b".repeat(60))).unwrap();
    assert_eq!(
        long,
        format!("crazyagent/docs--{}", "b".repeat(NAME_BYTES_MAX))
    );
    let cut = teammate_branch(task, &format!("{}-x", "c".repeat(NAME_BYTES_MAX - 1))).unwrap();
    assert_eq!(
        cut,
        format!("crazyagent/docs--{}", "c".repeat(NAME_BYTES_MAX - 1))
    );

    assert!(is_teammate_branch(task, "crazyagent/docs--ada"));
    for other in [
        "crazyagent/docs",
        "crazyagent/docs--",
        "crazyagent/docs/ada",
        "crazyagent/docsx--ada",
        "crazyagent/other--ada",
    ] {
        assert!(!is_teammate_branch(task, other), "{other}");
    }
}

#[test]
fn an_agent_branch_is_a_task_folder_or_teammate_name_under_the_prefix() {
    for branch in [
        "crazyagent/docs",
        "crazyagent/agent-7",
        "crazyagent/Agent_7",
        "crazyagent/docs--ada",
    ] {
        assert!(is_agent_branch(branch), "{branch}");
    }
    for branch in [
        "docs",
        "agent/agent-7",
        "crazyagent/",
        "crazyagent/a/b",
        "crazyagent/../b",
        "crazyagent/docs--ada/b",
        "crazyagent/a b",
        format!("crazyagent/{}", "a".repeat(FOLDER_BYTES_MAX + 1)).as_str(),
    ] {
        assert!(!is_agent_branch(branch), "{branch}");
    }
    assert_eq!(
        linked_branch(Path::new("/state/worktrees/agent-7")).as_deref(),
        Some("crazyagent/agent-7")
    );
    assert_eq!(linked_branch(Path::new("/state/worktrees/agent 7")), None);
}
