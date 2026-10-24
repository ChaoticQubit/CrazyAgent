---
name: trunk-based-development
description: >
  Trunk-based development and version control (SWE at Google ch. 16, 18, 22): branching and merge strategy, whether to cut a feature branch, pushing to main, long-lived branches and merge conflicts, monorepo and the One-Version Rule, release branches, large-scale change split into small commits.
---

# Trunk-Based Development (Google-style)

Guide the user toward **one shared trunk, small frequent commits, and no long-lived branches** — the branch model Google uses and that DORA research links to high-performing teams. When advising on version control, default to these rules and explain the reasoning; only recommend deviations when the user has a concrete, justified constraint.

## Core rules

1. **One repository, one trunk, one Source of Truth.** There is exactly one authoritative branch (`main`/`master`/trunk). "Done" means *merged to trunk*. Even with a DVCS like Git, define a single central branch that is authoritative.

2. **Develop directly against trunk in small increments.** Treat work-in-progress as a short-lived branch that lives hours-to-a-day, then merges. Commit small, self-contained changes regularly. The unit of progress is a small reviewed change landed on trunk — not a big branch merged later.

3. **Avoid long-lived development branches.** Quote to the user when they reach for one:
   > "A version control policy that makes extensive use of dev branches as a means toward product stability is inherently misguided."
   Small merges beat big merges; the same commits reach trunk eventually, so batching them into a branch only adds a painful merge and coordination overhead. Google runs <10 long-lived dev branches across ~1,000 teams — reserve them for genuinely unusual compatibility-over-time needs.

4. **Get stability from tests, CI, and code review — not from branch isolation.** Merge-strategy meetings and a "merge coordinator" are overhead that does not scale. If the answer to instability is "coordinate the merge," the real fix is better tests and CI.

5. **The One-Version Rule** (verbatim):
   > "Developers must never have a choice of 'What version of this component should I depend upon?'"
   For every dependency there is exactly one version to depend on. Don't fork an internal library to a second version; if you must diverge, repackage/rename it so no one faces a version choice. New components should be added at trunk, not pinned to some branch snapshot.

6. **Hide incomplete work at runtime, not on a branch.** New code lands on trunk **disabled** — behind a feature flag, behind visibility restrictions, or built to coexist with the old code path in the same binary — until it is ready. (See the `feature-flags` skill.) This is what replaces the feature branch.

## Decision guide: "Should I make a feature branch?"

Default answer: **No — land small commits on trunk and flag-guard anything unfinished.** Recommend a branch only for:

- **Release branches** (see below) — cut from trunk to stabilize a specific release.
- A rare, explicitly-justified long-lived branch for long-term external compatibility requirements — flag the cost explicitly and expect it to be expensive.

If the user insists on a shared feature branch, steer them to: split the feature into small trunk-safe commits, land the plumbing first (disabled), and integrate continuously. The larger a change grows in isolation, the worse the eventual merge.

## Release branches

- Release branches are **benign**, unlike dev branches. Create one only if a release will live longer than a few hours and needs isolation from ongoing trunk development.
- **Cherry-pick fixes from trunk into the release branch**, never the reverse. Keep cherry-picks to a minimum.
- **Expect to abandon the release branch. Never re-merge it into trunk.**
- Teams practicing continuous deployment often skip release branches entirely: to fix a problem, fix it at trunk and redeploy.

## Small commits & large-scale changes

When a change is too big to land atomically (touches many files, would conflict with everyone, or crosses repos), it is a **Large-Scale Change (LSC)**. Counterintuitively, the largest *atomic* change a codebase can absorb shrinks as the codebase and team grow.

- **Shard the big change into small, independently-committable, independently-testable pieces.** A failure in a 25-file change is easy to find; in a 10,000-file change it is impossible.
- Treat LSC commits as **"cattle, not pets"** — nameless and cheap to roll back or drop on a merge conflict or flaky test.
- Prefer behavior-preserving refactors that run through the language's auto-formatter so machine output looks human-written.
- Centralize LSCs (deprecated-API migrations, infra upgrades) in an expert/infrastructure team that internalizes the cost — don't push an "unfunded mandate" onto every consuming team. See the `engineering-principles` skill for deprecation and LSC workflow detail.

## Monorepo (optional, not required)

- Google keeps nearly all source in one monorepo, which makes the One-Version Rule trivial and maximizes tooling consistency.
- **The monorepo is not the point — adherence to the One-Version principle is.** You can approximate it with a "virtual monorepo" over many small repos as long as they agree on what trunk is, on commit ordering, and on dependency mechanisms.
- If you use separate repos, keep inter-repo dependencies **unpinned / at-head / trunk-based** rather than pinned to stale snapshots.
- Use **OWNERS files**: a plain text file per directory listing approvers, applied hierarchically and additively. Ownership is cheap to update.

## When advising, do

- Recommend the smallest change that makes progress and can be reviewed and reverted cleanly.
- Push instability fixes toward tests/CI/flags rather than branch coordination.
- Name the specific rule and quote it when it clarifies (One-Version Rule, "dev branches ... inherently misguided").
- Explain the tradeoff rather than being dogmatic when the user has a real constraint (e.g., open-source fork, regulated release isolation).

See `references/branching-playbook.md` for git command patterns and migration steps from a feature-branch workflow to trunk-based.
