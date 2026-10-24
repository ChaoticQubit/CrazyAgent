# Trunk-Based Development Playbook (git, language-agnostic)

Concrete patterns to apply the trunk-based rules with plain git. Adapt names to the user's host (GitHub/GitLab/Bitbucket).

## Daily loop

```bash
# Start from a fresh trunk every time
git checkout main
git pull --rebase origin main

# Short-lived local branch (hours, not weeks) — optional but convenient
git checkout -b work/short-descriptive-name

# ... make ONE small, self-contained change (aim ~200 lines) ...

git add -p
git commit -m "component: what changed and why"

# Rebase on latest trunk right before review to avoid a stale merge
git fetch origin && git rebase origin/main

# Open a small PR; after one approval, squash-merge to main
```

Keep the branch alive only until the change lands. Delete it after merge. Do not accumulate a stack of unrelated work on one branch.

## Keeping trunk green

- Run the fast test subset locally before pushing (`make test` / `npm test` / `pytest -q`).
- Configure branch protection so a change cannot merge unless CI is green and it has the required approval(s).
- Prefer **rollback over roll-forward** when a landed change breaks trunk: revert first, diagnose second.

```bash
git revert <sha>        # cleanly undo a bad change; re-land fixed later
```

## Migrating a team OFF a feature-branch / gitflow model

1. **Freeze new long-lived branches.** No new `develop`/`feature/*` branches that live more than ~a day.
2. **Drain existing branches.** Merge or abandon current long-lived branches quickly; the longer they live, the worse the merge.
3. **Introduce feature flags** so unfinished work can land on trunk disabled (see the `feature-flags` skill). This is the prerequisite that makes "push to main" safe.
4. **Move stabilization into CI:** required presubmit tests + required review, not a staging branch.
5. **Adopt release branches only if needed** — cut from trunk per release, cherry-pick fixes in, never merge back.
6. **Delete `develop`.** Trunk (`main`) becomes the single Source of Truth.

## Splitting a large change into trunk-safe shards

For a change too big to land at once:

1. Land **new code disabled** (behind a flag or unreferenced) — a pure addition, easy to review and revert.
2. Land **incremental call-site migrations** in small independent commits, each green on its own.
3. Flip the flag / switch the default once all call sites are migrated.
4. Land **removal of the old path** as a final small commit.

Each step is independently reviewable, testable, and revertible — "cattle, not pets."

## One-Version Rule in practice

- One declared version per third-party dependency in the lockfile/manifest; no second pinned copy.
- No `latest` / floating version ranges — pin explicitly and upgrade deliberately and frequently.
- If two internal consumers truly need different versions, that is a design smell; repackage/rename rather than letting callers choose a version.
