use super::*;

/// The agent home a private home from [`private_home`] sits in. Input: the private home.
/// Output: the agent home.
fn agent_home(home: &Path) -> PathBuf {
    home.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Moves the harness folder of an agent home aside and plants a link to `target` in its
/// place, as a harness that can write its agent home could. Inputs: the agent home and the
/// target.
fn plant(base: &Path, target: &Path) {
    fs::rename(base.join("sample"), base.join("moved")).unwrap();
    std::os::unix::fs::symlink(target, base.join("sample")).unwrap();
}

/// A folder outside the agent home holding what another private home of the same account
/// holds: `account-1` with the two rendered files and a launch state folder, each folder
/// 0700 as a real private home is. Output: the folder's guard and the folder.
fn victim() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().canonicalize().unwrap();
    let agent = target.join("account-1").join("agent");
    fs::create_dir_all(&agent).unwrap();
    for path in [target.clone(), target.join("account-1"), agent.clone()] {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(agent.join("mcp.json"), "kept").unwrap();
    fs::write(agent.join("AGENTS.md"), "kept").unwrap();
    fs::create_dir(target.join("account-1").join(STATE)).unwrap();
    (folder, target)
}

/// Asserts that nothing of the victim folder was deleted, replaced or locked.
/// Input: the victim folder.
fn untouched(target: &Path) {
    let account = target.join("account-1");
    for name in ["mcp.json", "AGENTS.md"] {
        assert_eq!(
            fs::read_to_string(account.join("agent").join(name))
                .ok()
                .as_deref(),
            Some("kept"),
            "{name}"
        );
    }
    assert!(account.join(STATE).is_dir());
    assert!(fs::symlink_metadata(account.join(LOCK)).is_err());
}

/// T18 LEAD-1 (rv-sa241): once a listed launch's private home is made, a harness that
/// swaps its harness folder for a link to another folder holding `<account>/<render path>`
/// leads neither the launch's end nor the next launch there: the end deletes nothing
/// through the link, and the next launch is refused before it locks or writes anything.
#[test]
fn a_link_planted_at_the_harness_folder_is_followed_by_neither_the_drop_nor_the_next_launch() {
    let (_folder, home) = private_home();
    let base = agent_home(&home);
    let (_victim, target) = victim();
    let installed = context().install_listed(&manifest(BLOCKS), &home, true);
    assert!(installed.is_ok(), "{:?}", installed.as_ref().err());

    plant(&base, &target);
    drop(installed);
    untouched(&target);

    let next = context().install_listed(&manifest(BLOCKS), &home, true);
    assert_eq!(
        next.err().map(|problem| problem.0).as_deref(),
        Some("launch_account_home_unsafe")
    );
    untouched(&target);
}

/// T18 LEAD-1 (rv-sa241): an engine that adopts a listed launch's pane walks to its private
/// home from the agent home without following a link, so a link planted at the harness
/// folder is refused and the folder it names is neither locked nor emptied.
#[test]
fn an_adopted_listed_launch_never_follows_a_link_planted_at_the_harness_folder() {
    let (_folder, home) = private_home();
    let (_victim, target) = victim();
    plant(&agent_home(&home), &target);
    let files = InstalledFiles {
        home: home.clone(),
        codex: false,
        rendered: vec!["agent/mcp.json".to_owned(), "agent/AGENTS.md".to_owned()],
        listed: true,
    };

    let adopted = Installed::adopt(&files);

    assert_eq!(
        adopted.err().map(|problem| problem.0).as_deref(),
        Some("launch_account_home_unsafe")
    );
    untouched(&target);
}

/// T18 LEAD-1: a listed launch runs only in a private home laid out as `<agent home>/<its
/// harness id>/<account>`, so the walk from the agent home names every folder on the way;
/// another harness's folder, a home of any other shape and an account name that is no
/// account reference are refused, and nothing is written there.
#[test]
fn a_listed_launch_runs_only_in_its_own_harness_folder_of_an_agent_home() {
    let (_folder, home) = private_home();
    let base = agent_home(&home);
    let (_flat_folder, flat) = victim();
    let wrong = [
        base.join("other").join("account-1"),
        base.join("sample").join("account 1"),
        flat.join("account-1"),
    ];
    for path in &wrong {
        fs::create_dir_all(path).unwrap();
        fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for path in &wrong {
        let refused = context().install_listed(&manifest(BLOCKS), path, true);
        assert_eq!(
            refused.err().map(|problem| problem.0).as_deref(),
            Some("launch_account_home_unsafe"),
            "{}",
            path.display()
        );
        assert!(fs::symlink_metadata(path.join(LOCK)).is_err());
    }
    untouched(&flat);
}

/// T18 LEAD-1 code r1 (rv-code2): a launch of a listed harness with no project context
/// holds its private home through the same no-follow walk from the agent home, so once its
/// harness folder is swapped for a link the next hold is refused and the folder the link
/// names is never locked.
#[test]
fn a_context_less_listed_launch_never_locks_through_a_link_planted_at_the_harness_folder() {
    let (_folder, home) = harness_home("standin-acp");
    let base = agent_home(&home);
    let held = lock_only(&home, "standin-acp");
    assert!(held.is_ok(), "{:?}", held.as_ref().err());
    drop(held);
    let (_victim, target) = victim();
    fs::rename(base.join("standin-acp"), base.join("moved")).unwrap();
    std::os::unix::fs::symlink(&target, base.join("standin-acp")).unwrap();

    let refused = lock_only(&home, "standin-acp");

    assert_eq!(
        refused.err().map(|problem| problem.0).as_deref(),
        Some("launch_account_home_unsafe")
    );
    untouched(&target);
}
