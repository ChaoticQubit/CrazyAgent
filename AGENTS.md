# CrazyAgent project instructions

## Current authority

Read [docs/agent-decisions.md](docs/agent-decisions.md), the [adopted architecture](docs/superpowers/specs/2026-10-17-crazyagent-architecture-design.md) and the [implementation plan](docs/superpowers/plans/2026-10-17-crazyagent-implementation-plan.md). DEC-060 through DEC-075 govern the current implementation phase and scoped authorizations. Later explicit owner instructions take precedence. Local documentation is ignored by owner policy; never force-add it.

The owner explicitly selected leader-coordinated subagent implementation in visible Herdr panes, one writer per isolated Git worktree. Do not substitute invisible background agents or several writers in one checkout.

The latest owner-supplied AGENTS workflow supersedes DEC-061's historical review deferral under DEC-075. Apply the required frozen normal code-review and Codex Security tracks after implementation and before any authorized push or PR. Preserve DeepSec evidence and follow the owner's model, authentication, coverage and round limits. The leader coordinates reviews; do not scan changing worker source or silently install tools or substitute models. Report missing prerequisites and pending reviews honestly. Preserve testing and runtime security controls. Do not edit global agent instructions or installed skills.

## Work ownership and durable context

1. Start from current local trunk on a short-lived task branch. Never write task commits on trunk. Preserve unrelated work; ask before destructive Git operations.
2. Read the assigned brief before editing. Each worker has a disjoint write scope and its own worktree. The leader owns integration, the decision register and shared task status.
3. Keep worker reports and real red/green command evidence. Agents may commit; do not push, create external PRs or publish. Do not delete worktrees, panes or another agent's state.
4. Query the graph before source inspection. Use context-mode to process large output, then inspect the named files. Refresh the graph after code changes; coordinate writes to the shared graph cache with the leader.
5. After compaction, recover from the decision register, execution ledger and actual Git history rather than conversation recollection.

## Implementation and testing

Maintain docs/implementation-decisions.md under DEC-067. Record high-level and function-level choices with stable IMP IDs, alternatives, pros/cons, costs, real verification and revisit conditions. Each production function must map to relevant decisions; common decisions may cover several symbols. Record new choices durably as they are made in the worker's own .local-state/implementation-decisions.md, and have the leader consolidate them into the local ignored canonical log before task integration. Label retrospective inferences. One named writer owns the shared log at a time.

Use engineering-principles, the owner's minimum-working-code ladder, tiger-style and TDD. Follow the latest owner workflow for the ordered design, implementation, verification and frozen review gates. Historical review deferral is not a current exemption.

Write the missing-behavior test, observe the intended failure, implement the minimum behavior, build and rerun tests. No mocks at any level. Use real production functions and real filesystem/process/database/native systems. Temporary real files and synthetic input data are permitted; fabricated provider/engine replies are not runtime evidence.

Do not disable assertions, weaken expectations, add skips or ignore a failure to obtain a green result. An unavailable native runtime, account or test prerequisite is a blocker for that capability and is reported separately. Never count zero tests or unexecuted tests as behavior proof.

Use Rust runtime assertions for internal invariants that the test can trip. Validate untrusted input with ordinary errors. No unsafe Rust. Do not add comments; only declaration-attached docstrings under the owner's rule. Do not hand-edit generated files or CHANGELOG.md.

Run `make check` for the available workspace suite. It checks formatting and lint, tests, builds and tests again; failures propagate. The foundation starts with no application tests, and this is not evidence of implemented behavior.

## Development principles from workflow research

1. Measure accepted outcomes. For each slice, name the user-visible behavior, acceptance criteria and smallest real check before coding. Record verified outcomes, human intervention, recovery failures, elapsed time and resource or token use when measured. Agent count, code volume and a green command with zero relevant tests are not completion metrics.
2. Compose the smallest working parts. Prefer the existing implementation, standard library and approved dependencies before writing a new component. Keep harness integration narrow and preserve Rust ownership of policy and durable state. Do not fork or port a harness until a demonstrated requirement cannot be met through its supported interface.
3. Separate reasoning from execution authority. Agents may propose plans and tool actions; deterministic engine rules admit destinations, credentials, worktrees, resources and effects. Keep credentials and protected engine/verifier state outside worker authority. A terminal, worktree, permission prompt or saved conversation alone proves no isolation or live-worker recovery guarantee.
4. Define verification before delegated work. Preserve human-approved acceptance baselines and independent code and test-integrity checks. Bind results to the actual source and configuration tested. Retain failures, interrupted attempts and unavailable prerequisites; never let a coding worker weaken the check that decides its own completion.
5. Bound parallel capacity deliberately. Use named resource ownership, bounded queues, explicit CPU, memory, process, time, output, retry and spending limits, plus cancellation and cleanup evidence where applicable. Treat extra workers and remote execution as capacity decisions. Measure behavior before claiming a performance or reliability gain.

DEC-070 applies these principles to our own development and product acceptance. Symvibe is a competitor and interaction reference. fx is a future harness candidate; Vercel Sandbox is a future optional execution backend. Preserve the accepted V1 scope and existing local execution work. Additional harnesses, sandbox choices and competitor features require their own qualified slices when scheduled. Research findings describe documented behavior, not a demonstrated advantage of CrazyAgent.

## Execution boundaries

Keep secrets out of source, commands, environment, logs and reports. Store only profile/credential references. Do not install tools, modify global configuration, connect accounts or spend against a new provider/destination without the required authorization.

Use existing local tools and offline dependency resolution first. Declare missing prerequisites concretely rather than downloading silently. Native/provider qualification uses only approved profiles and budgets.

The Rust engine is the policy authority. Preserve scope, worktree ownership, mandatory verification, memory promotion approval and current-contract acceptance. Review deferral does not weaken these controls.

All launches must retain the accepted live-worker recovery, individual native child controls and independently complete TUI/desktop requirements. An unqualified capability is unavailable, not silently replaced.

## Repository commands and layout

`make check` is the local test/build/test entry point. `make test`, `make build`, `make lint` and `make fmt-check` are narrower checks. Cargo runs locked and offline by default. DEC-068 permits required V1 dependency acquisition from official registries into project-local caches with exact locks; respect the current named resource owner and inspect acquired build/install scripts before execution. Keep CARGO_INCREMENTAL=0. No global tool installation is authorized.

Rust code lives in `crates/crazyagent`. Desktop code will be added at `apps/desktop` when its task starts. Canonical documents live in `docs`; existing research is retained in `reports` and `research_notes`. Local worktrees, graph cache, build products and execution scratch are ignored; agent instructions are tracked.

Keep one named owner for shared resources. Initially the qualification worker owns Rust build execution in its worktree, the inventory worker reads tool/runtime readiness without starting services or provider calls, and the leader owns integration and the shared graph cache.
