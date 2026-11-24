use super::*;

const PROMPT: &str = "Edit the notes";

/// Starts one sub-agent on `PROMPT` in a project whose checkout tracks `notes.md`, its
/// stand-in running `writes` in its worktree and then keeping its turn open, waits for
/// `ready` to hold over that worktree, runs `before_stop` and stops the agent as the owner
/// does. Inputs: the fixture, whether the agent works in its own clone, the script lines,
/// the wait and what runs before the stop. Output: the runtime, the agent and its worktree.
fn stopped_writer(
    fixture: &Fixture,
    own_clone: bool,
    writes: &str,
    ready: &dyn Fn(&Path) -> bool,
    before_stop: &dyn Fn(i64),
) -> (TeamRuntime, i64, PathBuf) {
    let project = fixture.base.join("project");
    std::fs::write(project.join("notes.md"), "Owner notes.\n").unwrap();
    let home = fixture.base.join("owner-home");
    std::fs::create_dir(&home).unwrap();
    for arguments in [
        &["init", "-q"][..],
        &["config", "user.email", "owner@local"],
        &["config", "user.name", "owner"],
        &["add", "-A"],
        &["commit", "-q", "-m", "start"],
    ] {
        let done = level_git(&project, &home, arguments);
        assert!(done.status.success(), "{done:?}");
    }
    if own_clone {
        crate::sandbox::policy_store::save(
            &fixture.connection,
            PROJECT,
            &kernel_sandbox::policy::AgentPolicy {
                access: kernel_sandbox::policy::AgentAccess::OwnBranch,
                ..Default::default()
            },
        )
        .unwrap();
    }
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, format!("#!/bin/sh\n{writes}exec /bin/sleep 300\n")).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = fixture.host();
    let agents = Arc::new(Agents::open(&fixture.base).unwrap());
    let access = fixture.access.clone();
    let mut team = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap()
    .with_agents(agents.clone())
    .with_terminal(host)
    .unwrap();
    let mut connection = Connection::open(fixture.base.join("engine.db")).unwrap();
    let tx = connection.transaction().unwrap();
    assert_eq!(
        crate::team::execute(
            &tx,
            &fixture.access,
            &crate::team::TeamAction::Delegate {
                project_id: fixture.setup.project_id.clone(),
                harness: Some("codex".into()),
                prompt: PROMPT.into(),
            },
        )
        .unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
    fixture.until(&mut team, "the sub-agent works on the task", |f| {
        f.status()
            .agents
            .iter()
            .any(|agent| agent.status == AgentStatus::Working && agent.process_group.is_some())
    });
    let member = fixture.status().agents.remove(0).id;
    let (worktree, git_dir) = agent_places(fixture, member);
    let folder = project.join(&worktree);
    assert_eq!(
        git_dir == crate::agent_git::git_dir(&worktree),
        own_clone,
        "the agent works in its own clone only under Own branch"
    );
    fixture.until(&mut team, "the sub-agent writes", |_| ready(&folder));
    before_stop(member);
    let listed = agents
        .list(Some(&fixture.setup.project_id), 0, crate::agents::LIST_MAX)
        .unwrap();
    agents.stop(&listed[0].id).unwrap();
    fixture.until(&mut team, "the stopped sub-agent's task ends", |f| {
        f.agent_status(member).0 == AgentStatus::Stopped
    });
    (team, member, folder)
}

/// The script lines that add `page.md` and edit the tracked `notes.md`, and the wait for
/// both. Output: the lines and the wait.
fn page_and_notes() -> (&'static str, fn(&Path) -> bool) {
    (
        "echo 'Written by the sub-agent.' > page.md\necho 'Agent notes.' >> notes.md\n",
        |folder| {
            folder.join("page.md").is_file()
                && std::fs::read_to_string(folder.join("notes.md"))
                    .is_ok_and(|text| text.contains("Agent notes."))
        },
    )
}

/// The id of the task `PROMPT`. Input: the fixture. Output: the id.
fn task_id(fixture: &Fixture) -> i64 {
    fixture
        .connection
        .query_row(
            "SELECT id FROM team_tasks WHERE prompt=?1",
            [PROMPT],
            |row| row.get(0),
        )
        .unwrap()
}

/// The project notes the engine wrote about edits. Input: the fixture. Output: their texts.
fn edit_notes(fixture: &Fixture) -> Vec<String> {
    fixture
        .status()
        .messages
        .into_iter()
        .map(|message| message.text)
        .filter(|text| text.contains("edits from task"))
        .collect()
}

/// The notes about edits the engine sent one agent, leaving out its other notes, such as the
/// stop report a lead gets. Inputs: the fixture and the agent. Output: their bodies.
fn agent_notes(fixture: &Fixture, agent: i64) -> Vec<String> {
    let mut statement = fixture
        .connection
        .prepare(
            "SELECT body FROM coord_messages WHERE to_kind='agent' AND to_agent=?1 AND kind='note'
             ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([agent], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .filter(|body| body.contains("edits from task"))
        .collect()
}

#[test]
fn an_own_clone_turn_that_leaves_edits_uncommitted_names_them_in_a_project_note() {
    let fixture = Fixture::new(1);
    let (writes, ready) = page_and_notes();
    let (mut team, member, folder) = stopped_writer(&fixture, true, writes, &ready, &|_| {});
    assert!(
        folder.join("page.md").is_file(),
        "the edits stay in the clone"
    );
    let task = task_id(&fixture);
    assert_eq!(
        edit_notes(&fixture),
        [format!(
            "The edits from task {task} (codex, sub-agent {member}) are not committed, so nothing landed; they stay only in its worktree: notes.md, page.md."
        )]
    );
    assert!(
        agent_notes(&fixture, member).is_empty(),
        "the agent itself is not told"
    );
    let landing = crate::landing::open(&fixture.base).unwrap();
    assert!(
        crate::landing::list(&landing, &fixture.setup.project_id)
            .unwrap()
            .is_empty()
    );
    assert!(team.shutdown().unwrap().is_empty());
}

#[test]
fn an_own_clone_turn_that_changes_nothing_writes_no_note() {
    let fixture = Fixture::new(1);
    let (mut team, member, _) = stopped_writer(
        &fixture,
        true,
        "",
        &|folder| folder.join("notes.md").is_file(),
        &|_| {},
    );
    assert_eq!(edit_notes(&fixture), Vec::<String>::new());
    assert!(agent_notes(&fixture, member).is_empty());
    assert!(team.shutdown().unwrap().is_empty());
}

#[test]
fn a_linked_turn_still_lands_its_edits_as_a_change_set() {
    let fixture = Fixture::new(1);
    let (writes, ready) = page_and_notes();
    let (mut team, member, _) = stopped_writer(&fixture, false, writes, &ready, &|_| {});
    let task = task_id(&fixture);
    assert_eq!(
        edit_notes(&fixture),
        [format!(
            "The edits from task {task} (codex, sub-agent {member}) are waiting for you in Changes."
        )]
    );
    let landing = crate::landing::open(&fixture.base).unwrap();
    let sets = crate::landing::list(&landing, &fixture.setup.project_id).unwrap();
    assert_eq!(sets.len(), 1, "{sets:#?}");
    assert_eq!(sets[0].files, 2);
    assert!(team.shutdown().unwrap().is_empty());
}

#[test]
fn a_worker_turn_that_leaves_edits_uncommitted_tells_its_lead_only() {
    let fixture = Fixture::new(1);
    let (writes, ready) = page_and_notes();
    let lead = std::cell::Cell::new(0);
    let (mut team, member, _) = stopped_writer(&fixture, true, writes, &ready, &|member| {
        let parent = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
        fixture
            .connection
            .execute(
                "UPDATE team_agents SET parent=?1 WHERE id=?2",
                params![parent, member],
            )
            .unwrap();
        lead.set(parent);
    });
    let task = task_id(&fixture);
    assert_eq!(
        agent_notes(&fixture, lead.get()),
        [format!(
            "The edits from task {task} (codex, sub-agent {member}) are not committed, so nothing landed; they stay only in its worktree: notes.md, page.md."
        )]
    );
    assert_eq!(
        edit_notes(&fixture),
        Vec::<String>::new(),
        "under the default Lead landing the owner is not told"
    );
    assert!(agent_notes(&fixture, member).is_empty());
    assert!(team.shutdown().unwrap().is_empty());
}

#[test]
fn a_turn_with_many_odd_names_names_what_fits_and_counts_the_rest() {
    let fixture = Fixture::new(1);
    let writes = "i=0\nwhile [ $i -lt 80 ]; do\n  printf x > \"$(printf 'file-%03d-%0100d\\tline' $i 0)\"\n  i=$((i+1))\ndone\n";
    let (mut team, member, _) = stopped_writer(
        &fixture,
        true,
        writes,
        &|folder| {
            std::fs::read_dir(folder)
                .map(|entries| entries.count() >= 82)
                .unwrap_or(false)
        },
        &|_| {},
    );
    let task = task_id(&fixture);
    let notes = edit_notes(&fixture);
    assert_eq!(notes.len(), 1, "{notes:#?}");
    let note = &notes[0];
    let head = format!(
        "The edits from task {task} (codex, sub-agent {member}) are not committed, so nothing landed; they stay only in its worktree: "
    );
    assert!(note.starts_with(&head), "{note}");
    assert!(note.len() <= crate::team::NOTE_BYTES_MAX, "{}", note.len());
    assert!(!note.chars().any(char::is_control), "{note:?}");
    let shown = note[head.len()..].split(", ").count();
    assert!(shown >= 1 && shown < 80, "{shown}");
    assert!(
        note.ends_with(&format!(" and {} more.", 80 - shown)),
        "{note}"
    );
    assert!(team.shutdown().unwrap().is_empty());
}
