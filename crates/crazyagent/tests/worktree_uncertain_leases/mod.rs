use super::*;

/// Makes a Ready lease, lets `change` touch its worktree, stores the lease Uncertain with
/// `renumber` applied, opens the engine once more, and runs `after` once it is closed.
/// Inputs: the change and the after check, each given the lease worktree, and the renumber.
/// Output: the lease state the reopened engine reads.
async fn uncertain_after(
    change: impl FnOnce(&Path),
    renumber: fn(u64) -> Renumber,
    after: impl FnOnce(&Path),
) -> crazyagent::worktrees::LeaseState {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let (repo, config, lease) = ready_lease(&root).await;
    change(Path::new(&lease.path));
    rewrite_rows(&root.join("db"), renumber(volume(&repo) ^ 1), true);
    let engine = Engine::open(&root.join("db"), config).await.unwrap();
    let state = leases(&engine).await.remove(0).state;
    engine.close().await.unwrap();
    after(Path::new(&lease.path));
    state
}

/// LEASE-LEGACY-UNCERTAIN: an Uncertain lease whose worktree still matches its own Git
/// index, the checkout as made or with its agent's change staged, is checked again at the
/// next start and waits for recovery.
#[tokio::test]
async fn an_uncertain_lease_whose_worktree_matches_its_index_waits_for_recovery() {
    use crazyagent::worktrees::LeaseState;
    assert_eq!(
        uncertain_after(|_| {}, |_| Renumber::Nothing, |_| {}).await,
        LeaseState::RecoveryRequired,
        "the checkout as made"
    );
    let staged = |worktree: &Path| {
        std::fs::write(worktree.join("tracked"), "the agent's change\n").unwrap();
        git(worktree, &["add", "tracked"]);
    };
    assert_eq!(
        uncertain_after(staged, |_| Renumber::Nothing, |_| {}).await,
        LeaseState::RecoveryRequired,
        "the agent's change, staged"
    );
}

/// LEASE-LEGACY-UNCERTAIN, rv-sec3 N1 and rv-code4 #161 F1: a checkout that failed after
/// its folder was pinned (a tracked file missing or short, no Git index yet, or the index
/// lock of a cut-off Git writer beside a whole index) stays Uncertain at the next start, on
/// a plain volume and on a renumbered one, so it is never requalified on its pins alone.
#[tokio::test]
async fn a_failed_checkout_stays_uncertain_on_a_plain_and_a_renumbered_volume() {
    use crazyagent::worktrees::LeaseState;
    let missing = |worktree: &Path| std::fs::remove_file(worktree.join("tracked")).unwrap();
    let short = |worktree: &Path| std::fs::write(worktree.join("tracked"), "comm").unwrap();
    let unindexed =
        |worktree: &Path| std::fs::remove_file(admin_folder(worktree).join("index")).unwrap();
    let locked =
        |worktree: &Path| std::fs::write(admin_folder(worktree).join("index.lock"), "").unwrap();
    let damages: [(&str, &dyn Fn(&Path)); 4] = [
        ("missing", &missing),
        ("short", &short),
        ("no index", &unindexed),
        ("index.lock", &locked),
    ];
    for (damage, change) in damages {
        for (place, renumber) in [
            ("plain", (|_| Renumber::Nothing) as fn(u64) -> Renumber),
            ("renumbered", Renumber::Volume as fn(u64) -> Renumber),
        ] {
            assert_eq!(
                uncertain_after(change, renumber, |_| {}).await,
                LeaseState::Uncertain,
                "{damage} on a {place} volume"
            );
        }
    }
}

/// Rewrites a lease worktree's Git index with `read-tree` alone, as a checkout cut off
/// after `read-tree` leaves it, and checks that its entries hold no file stat data.
/// Input: the lease worktree.
fn index_from_read_tree(worktree: &Path) {
    git(worktree, &["read-tree", "HEAD"]);
    index_has_no_stat_data(worktree);
}

/// Checks that a lease worktree's Git index entry for `tracked` holds no file stat data,
/// so nothing has refreshed and rewritten that index since `read-tree`. Input: the lease
/// worktree.
fn index_has_no_stat_data(worktree: &Path) {
    let listed = git(worktree, &["ls-files", "--debug", "--", "tracked"]);
    assert!(listed.contains("size: 0\t"), "{listed}");
}

/// LEASE-LEGACY-UNCERTAIN, rv-sec3 #160 N4 and rv-code4 #161 F1: with an index from
/// `read-tree` alone, which holds no file stat data, the start check compares the files'
/// content without writing that index: a whole worktree waits for recovery, while a file
/// of the same size with other bytes, or a missing file, keeps the lease Uncertain.
#[tokio::test]
async fn an_uncertain_lease_whose_index_has_no_stat_data_is_checked_on_its_content() {
    use crazyagent::worktrees::LeaseState;
    let altered = |worktree: &Path| {
        index_from_read_tree(worktree);
        std::fs::write(worktree.join("tracked"), "COMMITTED\n").unwrap();
    };
    let missing = |worktree: &Path| {
        index_from_read_tree(worktree);
        std::fs::remove_file(worktree.join("tracked")).unwrap();
    };
    let cases: [(&str, &dyn Fn(&Path), LeaseState); 3] = [
        ("whole", &index_from_read_tree, LeaseState::RecoveryRequired),
        ("same size, other bytes", &altered, LeaseState::Uncertain),
        ("missing", &missing, LeaseState::Uncertain),
    ];
    for (case, change, expected) in cases {
        assert_eq!(
            uncertain_after(change, |_| Renumber::Nothing, index_has_no_stat_data).await,
            expected,
            "{case}"
        );
    }
}
