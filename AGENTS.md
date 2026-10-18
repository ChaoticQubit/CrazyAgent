# CrazyAgent project instructions

## Current authority

Read [docs/agent-decisions.md](docs/agent-decisions.md), the [adopted architecture](docs/superpowers/specs/2026-10-17-crazyagent-architecture-design.md) and the [implementation plan](docs/superpowers/plans/2026-10-17-crazyagent-implementation-plan.md). DEC-060 through DEC-062 govern the current implementation phase. Later explicit owner instructions take precedence.

The owner explicitly selected leader-coordinated subagent implementation in visible Herdr panes, one writer per isolated Git worktree. Do not substitute invisible background agents or several writers in one checkout.

The owner explicitly deferred code and security reviews for this development phase. Do not launch task reviewers, normal code-review jobs, DeepSec or Codex Security reviews. Mark those gates deferred, never passed. Preserve testing and runtime security controls. Do not edit global agent instructions or installed skills.

## Work ownership and durable context

1. Start from current local trunk on a short-lived task branch. Never write task commits on trunk. Preserve unrelated work; ask before destructive Git operations.
2. Read the assigned brief before editing. Each worker has a disjoint write scope and its own worktree. The leader owns integration, the decision register and shared task status.
3. Keep worker reports and real red/green command evidence. Agents may commit; do not push, create external PRs or publish. Do not delete worktrees, panes or another agent's state.
4. Query the graph before source inspection. Use context-mode to process large output, then inspect the named files. Refresh the graph after code changes; coordinate writes to the shared graph cache with the leader.
5. After compaction, recover from the decision register, execution ledger and actual Git history rather than conversation recollection.

## Implementation and testing

Maintain docs/implementation-decisions.md under DEC-067. Record high-level and function-level choices with stable IMP IDs, alternatives, pros/cons, costs, real verification and revisit conditions. Each production function must map to relevant decisions; common decisions may cover several symbols. Record new choices durably as they are made in the worker's own .local-state/implementation-decisions.md, and have the leader consolidate them into the tracked log before task integration. Label retrospective inferences. One named writer owns the shared log at a time.

Use engineering-principles, the owner's minimum-working-code ladder, tiger-style and TDD. The review portions of skills are overridden by DEC-061 during this phase.

Write the missing-behavior test, observe the intended failure, implement the minimum behavior, build and rerun tests. No mocks at any level. Use real production functions and real filesystem/process/database/native systems. Temporary real files and synthetic input data are permitted; fabricated provider/engine replies are not runtime evidence.

Do not disable assertions, weaken expectations, add skips or ignore a failure to obtain a green result. An unavailable native runtime, account or test prerequisite is a blocker for that capability and is reported separately. Never count zero tests or unexecuted tests as behavior proof.

Use Rust runtime assertions for internal invariants that the test can trip. Validate untrusted input with ordinary errors. No unsafe Rust. Do not add comments; only declaration-attached docstrings under the owner's rule. Do not hand-edit generated files or CHANGELOG.md.

Run `make check` for the available workspace suite. It checks formatting and lint, tests, builds and tests again; failures propagate. The foundation starts with no application tests, and this is not evidence of implemented behavior.

## Boundaries

Keep secrets out of source, commands, environment, logs and reports. Store only profile/credential references. Do not install tools, modify global configuration, connect accounts or spend against a new provider/destination without the required authorization.

Use existing local tools and offline dependency resolution first. Declare missing prerequisites concretely rather than downloading silently. Native/provider qualification uses only approved profiles and budgets.

The Rust engine is the policy authority. Preserve scope, worktree ownership, mandatory verification, memory promotion approval and current-contract acceptance. Review deferral does not weaken these controls.

All launches must retain the accepted live-worker recovery, individual native child controls and independently complete TUI/desktop requirements. An unqualified capability is unavailable, not silently replaced.

## Repository commands and layout

`make check` is the local test/build/test entry point. `make test`, `make build`, `make lint` and `make fmt-check` are narrower checks. Cargo currently runs offline and adds no external dependency.

Rust code lives in `crates/crazyagent`. Desktop code will be added at `apps/desktop` when its task starts. Canonical documents live in `docs`; existing research is retained in `reports` and `research_notes`. Local worktrees, graph cache, build products and execution scratch are ignored; agent instructions are tracked.

Keep one named owner for shared resources. Initially the qualification worker owns Rust build execution in its worktree, the inventory worker reads tool/runtime readiness without starting services or provider calls, and the leader owns integration and the shared graph cache.
