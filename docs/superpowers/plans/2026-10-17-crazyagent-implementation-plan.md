# CrazyAgent Integrated Application Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the accepted macOS development environment through a shared Rust engine, complete TUI and desktop clients, qualified native harnesses and independent verification.

**Architecture:** A user-local Rust engine owns policy, durable project state and coordination. Separate session runners retain native control connections across engine-only crashes. Both clients use the same local protocol; coding and test execution use a qualified execution boundary.

**Tech Stack:** Rust, Tokio, Serde, SQLite/rusqlite, Ratatui/Crossterm, Tauri 2, React/TypeScript with Vite/Bun frontend tooling, RCLI and native Claude Code/Codex adapters; TOML native configuration. Resolve and record compatible versions and licenses before adding dependencies.

**Spec:** [Adopted architecture](../specs/2026-10-17-crazyagent-architecture-design.md), [DEC-001 through DEC-059](../../agent-decisions.md), and [builder check](../specs/2026-10-17-crazyagent-builder-check.md).

Status: Active under DEC-060 through DEC-062. The owner selected visible Herdr subagent-driven implementation and explicitly deferred code/security reviews for the current development phase. No runtime qualification is implied by repository setup.

Execution override: DEC-061 supersedes task/final review and DeepSec/Codex Security instructions below for this phase; those gates remain deferred, never passed. DEC-062 prohibits mocks and test bypasses at every test level. The owner approved starting repository initialization and implementation; no further method-selection question is pending.

## Global Constraints

1. The first public release qualifies macOS, Claude Code and Codex, local Git and GitHub.com, and user-owned or reviewed repositories. Rust remains the main engine; eventual Linux and Windows support does not count as macOS qualification.
2. Both the primary TUI and desktop independently support project setup, settings, briefs, plans and approvals, work supervision, change and evidence inspection, memory review and authorized merges. Closing clients must not stop managed workers.
3. Original live workers must survive engine-only crashes and reattach. Admitted native child workflows require individual status, cancellation and recovery or continuation-state coverage. Ten active top-level harness processes across three projects is a qualification target; separately launched leader, curator and verifier harnesses count toward that total.
4. Unit tests precede implementation and exercise real modules. Integration, end-to-end and smoke checks use real systems with no mocks. Missing, stale or unknown verification evidence never passes; deterministic policy is not a universal zero-bug guarantee.
5. The owner supplies 30-40 hours per week with AI coding agents, with approximately 20% reserved for checks, review, fixes, packaging and documentation. Reforecast after the first 16 working hours of qualification. November 30 is conditional. This plan does not authorize tool installation, provider sign-in, unapproved provider spending or publication.

## Review Focus

1. A lost acknowledgement must not execute a durable command twice; Task 5 tests replay after disconnect and engine restart.
2. A malicious path, symlink, Git hook or repository configuration must not extend worker authority; Tasks 2 and 6 test the real execution and Git boundaries.
3. A stalled client, full disk or hostile terminal escape must not corrupt policy, conceal capture gaps or control another client; Tasks 3, 5, 10 and 11 own these tests.
4. A native child changing state during recovery must not appear cancelled or recovered without provider evidence; Task 3 tests the race on each admitted adapter.
5. A baseline/configuration change or partial multi-repository merge must not reuse stale evidence or repeat an uncertain mutation; Tasks 9 and 12 test those outcomes.

## Scope, ownership and working method

This is one integrated product plan because the runner, authority, clients and verifier depend on the same session identities and durable protocol. Later operating systems, remote workers, extra Git hosts and the full agent ticket system are separate projects, not scaffolding in this plan.

The initial executor owns Tasks 1-4 in order. Later tasks remain sequential unless the owner selects delegated execution and isolated worktrees with disjoint scopes. Shared native accounts, runtime fixtures, ports and performance workloads have one named owner. No two agents edit one checkout.

The original workspace currently has documents and no Git repository. Before product work, initialize the local repository, create the empty trunk initialization commit, and cut `feat/qualification-foundation` before adding task files. Do not overwrite an existing Git directory if the state changes. Do not set a remote, push or open a PR. Existing documents become task-branch inputs, not commits of task work to trunk. Subsequent changes use short-lived task branches from current trunk and the required review/landing workflow.

Use the established gate order: graph query, relevant decision record, engineering-principles, the owner's ponytail ladder, tiger-style, written plan/TDD, frozen reviews and graph refresh. Gate-4 design synthesis and full-design approval are recorded in DEC-059. Reread relevant decisions after compaction. No new comments; declaration-attached docstrings follow the owner's rule.

No product flag system is introduced solely for this project. Unqualified capabilities are unavailable through admission checks. A settings switch cannot mark an unqualified capability as qualified or bypass the mandatory verification floor.

### First 16 working hours

| Work allowance | Slice | Concrete output |
|---|---|---|
| Hours 0-2 | Task 1 | Executable admission check, prerequisite/version inventory and local evidence layout |
| Hours 2-6 | Task 2 | Real sandbox positive/negative controls and explicit enforcement gaps |
| Hours 6-14 | Task 3 | Native session/recovery/auth/child-control attempts within approved profiles |
| Hours 14-16 | Task 4 | Evidence-based forecast, blocker decisions and next bounded work allocation |

These are checkpoints, not four promised completions. If a task cannot finish within its allowance, retain the failing or unavailable result and continue the checkpoint assessment. Do not bypass confinement to make a provider demo green. After three failed fixes for one assumption, stop that line of work and identify the doubtful assumption.

### Authorization and dependency admission

Before installation or account-connected execution, produce a concrete manifest of the missing tools/packages, exact sources, resolved versions, native profile references, intended destinations and a bounded test-call budget. Ask only for missing authorization. Use existing approved installation and account evidence when available; never infer credential portability between harnesses. Do not put credential values in commands, environment, logs, configuration or evidence.

Read-only inventory and ordinary source work can proceed under the selected execution method. Toolchain download, package/tool installation, account connection and model calls remain separately gated. An unavailable account is recorded as unavailable, not as a passing native test.

## File and process boundaries

All paths below are new unless listed as Modify. File names are builder choices selected here for consistent handoffs, not claims that those files already exist.

| Path | Responsibility |
|---|---|
| `Cargo.toml`, `crates/crazyagent/Cargo.toml` | Rust workspace; initially only the core package is a default member |
| `crates/crazyagent/src/lib.rs`, `src/bin/crazyagent.rs` | Library exports and explicit engine/runner/TUI/qualification entry points |
| `crates/crazyagent/src/qualification.rs`, `isolation.rs` | Admission evidence and execution-boundary qualification |
| `crates/crazyagent/src/runner.rs`, `adapters/{mod,rcli,claude,codex}.rs` | Surviving runners and explicit native transports/capabilities |
| `crates/crazyagent/src/{protocol,engine,storage,config}.rs` | Shared protocol, durable commands, SQLite and effective configuration |
| `crates/crazyagent/src/{worktrees,coordination,routing,memory,assets,verification}.rs` | Project operations, leader/team work, shared knowledge and gates |
| `crates/crazyagent/src/tui/{mod,app,views}.rs` | TUI presentation and typed client operations |
| `crates/crazyagent/tests/*.rs` | Real integration, native qualification and PTY flow checks |
| `apps/desktop/src-tauri/{Cargo.toml,build.rs,src/main.rs,src/lib.rs,tauri.conf.json,capabilities/main.json}` | Desktop bridge, packaging and least-authority capabilities |
| `apps/desktop/{package.json,index.html,vite.config.ts,tsconfig.json,wdio.conf.ts}` | Explicit build/test scripts and native desktop test configuration |
| `apps/desktop/src/{app.tsx,engine-client.ts,screens,terminal.tsx}` | Desktop presentation, typed protocol and supplementary terminal |
| `apps/desktop/tests/*.ts` | Real native desktop E2E |
| `docs/qualification/{capabilities,forecast,release}.md` | Sanitized findings and reproducible qualification instructions |
| `.github/workflows/{checks,native-qualification}.yml` | Required checks and controlled real macOS qualification |

Generated lockfiles and framework outputs are produced by their owning tools after authorized dependency resolution. Do not hand-edit generated files, including CHANGELOG.md. Add Apache-2.0 for original code and retain third-party notices. Ignore graph cache, credentials, activity, local databases and genuinely ephemeral tool state; do not ignore shared DeepSec state by assumption.

### Shared type contract

The owning task defines these types before another task consumes them. Serialize identifiers and event cursors as validated strings across the Rust/TypeScript boundary; do not depend on JavaScript integer precision.

| Task | Types and required fields |
|---|---|
| 1 | `Capability`: confinement, live recovery, native auth, child control; `Outcome`: proven, failed, unavailable; `Artifact { path, sha256 }`; `QualificationReport { source_revision, profile_id, versions, results, artifacts, gaps }` |
| 2 | `SandboxProfile { id, grants, network_policy, credential_refs, resource_limits }`; `BoundaryEvidence { profile_id, allowed, denied, unenforced, artifacts }`; `allowed` and `denied` are `BTreeMap<String, bool>` keyed by named probe case |
| 3 | `HarnessKind`: RCLI, Claude, Codex; `HarnessSpec { kind, executable, version, profile_id, auth_profile_ref, worktree }`; `RunnerIdentity { runner_id, harness_id, started_at, controller_epoch }`; `RecoveryEvidence { original_identity, observed_identity, child_states, duplicate_dispatches, artifacts }`; `ChildState` is `Running`, `Waiting`, `Stopped`, `Completed` or `Unknown` |
| 5 | `WireRequest { protocol_version, client_id, request_id, expected_revision, command }`; `WireReply { request_id, revision, outcome }`; `ProjectSpec { name, repositories }`; `ProjectId`, `SessionId`, `TaskId`, `PlanId`, `ConfigId`, `SnapshotId`, `BaselineId`, `EnvironmentId` and `Cursor` string newtypes; `EffectiveConfig`, `SettingDescriptor`, `ExecutionContext`, `Client` |
| 6-9 | Task 6 defines `WorktreeLease`; Task 7 defines `WorkPlan`, `RouteRequest`, `RouteDecision`, `ScopedMessage`; Task 8 defines `KnowledgeCandidate`, `KnowledgeEntry`, `KnowledgeStatus`, `AssetSnapshot`; Task 9 defines `VerifyRequest`, `VerificationReceipt`, `RiskAssessment` |

`ExecutionContext { project_id, task_id, config_id, snapshot_id, baseline_id, allowed_destinations, budget_ref }` is durable. Credentials remain references to native/provider or operating-system storage. No secret value travels in this object.

## Task 1: Qualification admission and evidence contract

**Files:** Create the root/core Cargo manifests, `crates/crazyagent/src/{lib,qualification}.rs`, `src/bin/crazyagent.rs`, `docs/qualification/capabilities.md`, LICENSE and `.gitignore`.

**Interfaces:** Produce Task-1 types and `admit(report: &QualificationReport, required: &[Capability]) -> Result<(), AdmissionError>`. Admission requires matching source/profile, intact evidence and proven outcomes for every required capability.

- [ ] **Step 1:** Create the minimal build manifests and write `qualification::tests::missing_evidence_never_admits`: an empty report fails; an unavailable required result fails; a report for another profile fails; altered artifact bytes fail their stored hash; artifact references cannot escape the admitted evidence directory.

```rust
assert!(admit(&empty_report, &[Capability::LiveRecovery]).is_err());
assert!(admit(&unavailable_report, &[Capability::LiveRecovery]).is_err());
assert!(admit(&tampered_report, &[Capability::Confinement]).is_err());
```
- [ ] **Step 2:** Run `cargo test -p crazyagent --lib qualification::tests::missing_evidence_never_admits`; expect a missing implementation failure, not a network or toolchain error counted as a red test.
- [ ] **Step 3:** Implement the admission function, evidence serialization and CLI report/admit entry points. Inventory installed versions without installing or auto-downloading tools. Store raw local evidence under a private ignored directory and publish only sanitized summaries.
- [ ] **Step 4:** Repeat the unit check; expect PASS. Run the CLI against a report containing unavailable native results; expect nonzero admission and named missing capabilities. Unit fixtures establish the admission logic, not native capability.
- [ ] **Step 5:** Inspect the exact diff, update the graph and commit the scoped files as `feat: add qualification admission records`. Preserve unrelated owner documents and record source revision in later evidence.

## Task 2: Prove the execution boundary before native dispatch

**Files:** Create `crates/crazyagent/src/isolation.rs`, `tests/qualification_boundary.rs`; modify the CLI, admission report and capabilities document.

**Interfaces:** Consume Task-1 types. Produce Task-2 types and `run_boundary_probe(profile: &SandboxProfile, candidate_root: &Path, trusted_root: &Path) -> Result<BoundaryEvidence, BoundaryError>`.

- [ ] **Step 1:** Write a real `boundary_denies_escape_and_trusted_writes` integration check. Under the candidate runtime, permitted writes in an isolated candidate repository succeed; writes through traversal and symlinks into private trusted fixtures fail; child processes inherit restrictions; forbidden network and credential paths fail.

```rust
assert_eq!(evidence.allowed.get("candidate-write"), Some(&true));
assert_eq!(evidence.denied.get("trusted-write"), Some(&true));
assert_eq!(evidence.denied.get("symlink-escape"), Some(&true));
assert_eq!(evidence.denied.get("forbidden-network"), Some(&true));
```
- [ ] **Step 2:** Run `cargo test -p crazyagent --test qualification_boundary -- --ignored --exact boundary_denies_escape_and_trusted_writes` with an explicitly admitted nonsecret profile path; expect failure before implementation. Missing runtime/profile is UNAVAILABLE and blocks capability admission.
- [ ] **Step 3:** Implement the probe using the existing runtime candidate. Use disposable fixtures only. Record effective filesystem/network/process/CPU/memory controls, required host privileges and unenforced limits. Do not expose unrestricted host sockets or Apple Events as a convenience fallback.
- [ ] **Step 4:** Repeat the real check and inspect each positive/negative artifact. An unsupported required control blocks the applicable profile. Queue the real engine-control endpoint denial test for Task 5 when that endpoint exists; do not claim it tested now.
- [ ] **Step 5:** Update the capability report/graph and commit as `feat: qualify execution boundary controls`. If the candidate fails, preserve evidence and propose a bounded repair or replacement at Task 4.

## Task 3: Surviving native runner, authentication and child controls

**Files:** Create `crates/crazyagent/src/runner.rs`, `adapters/{mod,rcli,claude,codex}.rs`, `tests/qualification_recovery.rs`; modify CLI and capabilities.

**Interfaces:** Consume admitted sandbox profiles. Produce Task-3 types, `spawn_runner(spec: &HarnessSpec) -> Result<RunnerIdentity, RunnerError>` and `recover_runner(identity: &RunnerIdentity, controller_epoch: &str) -> Result<RecoveryEvidence, RunnerError>`.

- [ ] **Step 1:** Write real checks `original_worker_survives_engine_crash` and `native_child_cancel_race_is_truthful`. Assert original runner/harness identity, preserved native connection/worktree, retained capture, no repeated operation, observed child status and confirmed cancellation outcome. A new process restoring a transcript must fail the original-worker assertion.

```rust
assert_eq!(evidence.original_identity.runner_id, evidence.observed_identity.runner_id);
assert_eq!(evidence.original_identity.harness_id, evidence.observed_identity.harness_id);
assert_eq!(evidence.original_identity.started_at, evidence.observed_identity.started_at);
assert_ne!(evidence.original_identity.controller_epoch, evidence.observed_identity.controller_epoch);
assert_eq!(evidence.duplicate_dispatches, 0);
assert_eq!(cancelled_child.state, ChildState::Stopped);
```
- [ ] **Step 2:** Run `cargo test -p crazyagent --test qualification_recovery -- --ignored` once for every admitted launch harness/profile. Required profile coverage includes RCLI leader operation and Claude/Codex native auth/control; record each tested authentication mode rather than claiming all modes. Use approved destinations and bounded calls only.
- [ ] **Step 3:** Implement independently supervised runners and explicit adapters. Qualify lifetime across parent death, UI closure and engine restart on macOS. Retain native control and capture in the runner; reject stale controller epochs; persist operation identities before dispatch. Use native structured transports where qualified, and declare unsupported child workflows unavailable. RCLI one-shot mode alone does not establish persistent leadership.
- [ ] **Step 4:** Repeat both real checks, including a child transition during recovery and a provider permission request while the engine is down. Already authorized work may continue; new authority waits. Test logging failure with pause as default and explicit continue-with-gaps. Capture only observable provider content, never claim hidden reasoning. Qualify relevant credential isolation and revocation behavior.
- [ ] **Step 5:** Record exact native/runtime versions, control mechanism, auth modes, source revision, evidence and gaps. Update graph and commit as `feat: qualify surviving native sessions`. Keep engine-only recovery distinct from runner crash, reboot and future remote recovery.

## Task 4: Issue the first forecast and qualification decision

**Files:** Create `docs/qualification/forecast.md`; modify capabilities, this plan and relevant decision-register sections.

**Interfaces:** Consume immutable Task-1 through Task-3 evidence. Produce a forecast with proven, failed and unavailable capabilities, remaining work, measured time and a bounded next allocation.

- [ ] **Step 1:** At 16 working hours, freeze the evidence inventory and source revisions. Check that every attempted capability has an outcome and every claimed pass has artifacts.
- [ ] **Step 2:** Run the admission CLI for the declared launch profiles; preserve nonzero output and individual missing capabilities rather than replacing it with a green summary.
- [ ] **Step 3:** Compare the observed integration work with remaining capacity through November 30, preserving the 20% reserve. State the earliest evidence-supported forecast and unresolved dependency or account constraints.
- [ ] **Step 4:** For any failed mandatory boundary, live-recovery or child-control requirement, present one concrete repair/replacement with cost and scope impact. Obtain a decision before weakening a launch requirement, adding host privilege or extending external spending.
- [ ] **Step 5:** Save the forecast, relevant decision records and graph state; commit as `docs: record initial qualification forecast`. Continue within the adopted design when admitted; otherwise leave affected work blocked and proceed only on independent authorized tasks.

## Task 5: Durable Rust authority, local protocol and effective settings

**Files:** Create `crates/crazyagent/src/{protocol,engine,storage,config}.rs`, `tests/{engine_replay,engine_boundary}.rs`; modify core manifests and CLI.

**Interfaces:** Produce Task-5 types plus `InputLease { session_id, client_id, controller_epoch }`, `Engine::apply(request: WireRequest) -> Result<WireReply, EngineError>`, `resolve_config(global: &Path, project: &Path) -> Result<EffectiveConfig, ConfigError>` and `Client::request(request: WireRequest) -> Result<WireReply, ClientError>` and `acquire_input(session_id: &SessionId, client_id: &str, controller_epoch: &str) -> Result<InputLease, EngineError>`. Native input and resize require the current exclusive lease; viewers remain concurrent.

- [ ] **Step 1:** Write unit checks `rejects_invalid_request`, `task_config_is_pinned` and `one_input_owner_across_clients`; write real integration checks `replay_after_lost_ack_executes_once` and `worker_cannot_control_engine`. Cover invalid/oversized frames, unsupported protocol versions, stale revisions, queue saturation, reconnect cursor gaps, configuration migration and malformed settings preserving the last valid configuration.

```rust
assert_eq!(first_reply.outcome, replay_reply.outcome);
assert_eq!(first_reply.revision, replay_reply.revision);
assert_eq!(stored_projects.len(), 1);
assert_eq!(task.config_id, original_config_id);
```
- [ ] **Step 2:** Run `cargo test -p crazyagent --lib`, then `cargo test -p crazyagent --test engine_replay --test engine_boundary`; expect relevant missing behavior failures. Integration uses the actual engine, SQLite and admitted worker boundary.
- [ ] **Step 3:** Implement owner-restricted macOS local transport, dedicated SQLite work, durable operation deduplication before acknowledgement, ordered event cursors and snapshot resynchronization. Define typed configuration/setting metadata with default, origin, scope and effect timing. Link global defaults, allow ordinary project overrides, pin task configuration, preserve explicit migration and apply revocations immediately.
- [ ] **Step 4:** Repeat checks with an actual disconnect/restart and stalled client. Verify durable authority survives SQLite errors without false acknowledgement. Separate bulk activity from critical state; bound all queues and document selected limits in typed configuration. Engine control is unavailable to coding sandboxes except explicitly scoped broker operations. Credential values are absent from requests, events and stored configuration.
- [ ] **Step 5:** Run the relevant tests, graph refresh and scoped diff inspection; commit as `feat: add durable local engine authority`.

## Task 6: Projects, repositories and one-writer worktrees

**Files:** Create `crates/crazyagent/src/worktrees.rs`, `tests/worktree_ownership.rs`; modify engine/project commands.

**Interfaces:** Consume `ProjectSpec`, project/task identities and effective settings. Produce `WorktreeLease { project_id, task_id, repository, path, branch, owner_session }` and `allocate_worktree(project: &ProjectSpec, task_id: &TaskId, session_id: &SessionId) -> Result<WorktreeLease, WorktreeError>`.

- [ ] **Step 1:** Write real `dirty_work_is_preserved_and_writer_is_unique`: preserve tracked/untracked bytes, refuse a second concurrent writer including native descendants, and reject paths outside admitted repositories. Add hostile Git hook/filter/config and symlink cases in disposable repositories.

```rust
assert!(second_writer.is_err());
assert_eq!(tracked_before, tracked_after);
assert_eq!(untracked_before, untracked_after);
```
- [ ] **Step 2:** Run `cargo test -p crazyagent --test worktree_ownership`; expect missing behavior. Use real Git; never clean or reset an owner's repository to make the test pass.
- [ ] **Step 3:** Implement project creation with one or more repositories, task-owned branches/worktrees and a lease ledger. Keep Git mutations in narrow trusted host operations; account for repository-controlled hooks, filters, shared Git metadata and config. Do not assume worktrees alone supply confinement.
- [ ] **Step 4:** Repeat checks with concurrent requests, process interruption and a pre-existing branch/path. Confirm the ledger reconciles actual Git state and refuses uncertain ownership. Task 12 owns acceptance/merge; this task cannot bypass verification or merge work.
- [ ] **Step 5:** Update graph and commit as `feat: isolate task worktrees and ownership`.

## Task 7: Central leadership, routing, teams and scoped communication

**Files:** Create `crates/crazyagent/src/{coordination,routing}.rs`, `tests/coordination_flow.rs`; modify engine commands and admitted RCLI adapter.

**Interfaces:** Produce `WorkPlan { id, scope, tasks, dependencies, authority, config_id }`, `RouteRequest`, `RouteDecision { target_profile_ref, reasons, budget_ref }`, `ScopedMessage`, `route(request: &RouteRequest, config: &EffectiveConfig) -> Result<RouteDecision, RouteError>`, and `approve_plan(plan: &WorkPlan, expected_revision: &str) -> Result<(), PlanError>`.

- [ ] **Step 1:** Write unit checks `required_target_never_silently_falls_back` and `resource_budget_applies_to_all_roles`. Write real `one_brief_runs_through_workspace_and_project_leaders`: authorize project scope, approve a plan, allocate two independent work items, route to admitted Claude/Codex profiles and inspect native child status.

```rust
assert!(route(&binding_request, &unavailable_config).is_err());
assert_eq!(route(&eligible_request, &approved_config)?.target_profile_ref, allowed_profile_ref);
```
- [ ] **Step 2:** Run routing unit checks, then `cargo test -p crazyagent --test coordination_flow -- --ignored` with approved profiles; expect relevant failures. No simulated leader responses count as integration.
- [ ] **Step 3:** Implement RCLI-backed workspace/project leadership and typed proposals. Rust validates approved scope, destination, accounts, quotas and dependencies before dispatch. Default leaders coordinate; implementing mode is configurable within the same boundary. Use fair sharing by default with strict-priority mode for future launches. Native nested agents do not evade budgets or worktree ownership.
- [ ] **Step 4:** Repeat integration with resource exhaustion, eligible fallback, binding required targets and denied cross-project requests. Same-task peers use engine mailboxes; cross-task communication requires explicit links or leaders. Persistent preference changes require human approval. Report usage as known, estimated or unavailable with provenance.
- [ ] **Step 5:** Update graph and commit as `feat: coordinate leaders and routed task teams`.

## Task 8: Shared assets, curated memory and the human inbox

**Files:** Create `crates/crazyagent/src/{memory,assets}.rs`, `tests/knowledge_scope.rs`; modify configuration, engine commands and native asset adapters.

**Interfaces:** Produce `KnowledgeStatus` (`Provisional`, `Approved`, `Rejected`), `KnowledgeCandidate { id, project_id, source_refs, content, proposed_scope }`, `KnowledgeEntry { id, scope, status, provenance, content }`, `AssetSnapshot { config_id, sources, hashes, adapter_projection }`, `nominate(candidate: KnowledgeCandidate) -> Result<KnowledgeEntry, MemoryError>` and `promote_global(candidate_id: &str, approval_id: &str) -> Result<KnowledgeEntry, MemoryError>`.

- [ ] **Step 1:** Write unit checks `global_promotion_requires_approval` and `memory_is_not_authority`. Write real `projects_share_only_authorized_knowledge`, asserting immediate provisional project visibility, global approval, provenance, correction/deletion and cross-project denial. Include source revocation and an injected instruction attempting to change policy.

```rust
assert!(promote_global(&candidate_id, "").is_err());
assert_eq!(project_entry.status, KnowledgeStatus::Provisional);
assert_eq!(authority_before, authority_after);
```
- [ ] **Step 2:** Run memory/asset unit checks and `cargo test -p crazyagent --test knowledge_scope -- --ignored`. Test actual engine storage and admitted native asset loading; use approved local/cloud curation destinations for any model-connected step.
- [ ] **Step 3:** Implement bounded asynchronous project/global curation and direct nominations from user/agents. Keep activity, candidates and curated knowledge distinct. Build managed session asset projections for global/project rules, skills and supported plugins without rewriting global CLI configuration. Qualify semantics per adapter; do not claim universal plugin compatibility.
- [ ] **Step 4:** Repeat checks through actual sessions, including changed knowledge during pinned task settings, curator queue pressure and full disk. Retention defaults to explicit deletion with configurable expiry. Minimize/redact captured activity, preserve visible capture gaps, and require approval before global promotion. App-managed encryption remains a required later capability, not a claimed launch feature.
- [ ] **Step 5:** Update graph and commit as `feat: add scoped knowledge and memory review`.

## Task 9: Protected baselines, code verification and test integrity

**Files:** Create `crates/crazyagent/src/verification.rs`, `tests/verification_integrity.rs`; modify engine authorization, storage and isolation.

**Interfaces:** Produce `VerifyRequest { snapshot_id, baseline_id, config_id, environment_id, checks }`, `VerificationReceipt { request, source_digest, outcomes, artifacts, completed_at }`, `RiskAssessment { deterministic_floor, raised_risk, reasons }`, `verify(request: &VerifyRequest) -> Result<VerificationReceipt, VerificationError>` and `VerificationReceipt::accepts(&self, current: &VerifyRequest) -> Result<(), VerificationError>`. Acceptance validates current inputs, artifact integrity and all required outcomes.

- [ ] **Step 1:** Write unit checks `stale_receipt_never_accepts` and `ai_cannot_lower_risk_floor`. Write real `broken_candidate_and_tampered_tests_fail_gate`: mutate a disposable copy of real code and test setup, run real tools and assert failed integrity/acceptance; deny worker writes to baseline, receipts and trusted configuration.

```rust
assert!(receipt.accepts(&changed_request).is_err());
assert!(tampered_receipt.accepts(&current_request).is_err());
assert!(broken_candidate_acceptance.is_err());
```
- [ ] **Step 2:** Run verification unit checks and `cargo test -p crazyagent --test verification_integrity -- --ignored`; expect missing protection/validation failures. Missing tools or an unexecuted suite remains unavailable.
- [ ] **Step 3:** Implement human-reviewed AI-drafted/imported baselines, immutable source snapshots, current-contract acceptance and receipt binding. Separate trusted verification from coding harnesses. Use out-of-process trusted test control where needed; do not assume a test loaded into candidate-controlled execution can conceal its contents. Qualify holdout confinement and disclose remaining feedback limits.
- [ ] **Step 4:** Repeat checks after baseline/config/head change, immediate revocation, missing evidence, holdout mutation and real infrastructure interruption. Every change gets the mandatory floor; risk selects extra depth and the human may request the full applicable suite. Only approved temporary infrastructure failures receive bounded retries; assertion failures do not retry until green.
- [ ] **Step 5:** Update graph and commit as `feat: enforce protected verification receipts`.

## Task 10: Complete core workflow in the TUI

**Files:** Create `crates/crazyagent/src/tui/{mod,app,views}.rs`, `tests/tui_flow.rs`; modify CLI and client bindings.

**Interfaces:** Consume only typed `Client` operations and events. Produce `run_tui(client: Client) -> Result<(), TuiError>`; the real PTY driver records `ObservedWorkflow { project_created, memory_reviewed, receipt_inspected }`; no TUI-owned scheduling or gate authority.

- [ ] **Step 1:** Write unit checks for navigation/event reduction, including Unicode, resize and hostile escape content. Write real PTY `tui_core_workflow`: project creation, settings/provenance, brief/plan approval, supervision, child control, memory inbox, evidence and merge decision.

```rust
assert!(!rendered_transcript.contains('\u{001b}'));
assert!(observed_workflow.project_created);
assert!(observed_workflow.memory_reviewed);
assert!(observed_workflow.receipt_inspected);
```
- [ ] **Step 2:** Run TUI unit checks and `cargo test -p crazyagent --test tui_flow -- --ignored` against the real engine and admitted native sessions; expect missing views/behavior. A renderer snapshot does not establish this flow.
- [ ] **Step 3:** Implement Ratatui/Crossterm views with keyboard selections, visible focus, editable scoped settings and accurate reconnect/progress/error states. Preserve the single central conversation; native terminal attachment is supplementary with explicit input ownership and safe terminal restoration.
- [ ] **Step 4:** Repeat the flow with simultaneous desktop/client connections, stale settings revision, terminal resize, reconnect and permission fallback. Sanitize transcript rendering; allow terminal control only in the explicitly owned terminal surface. Closing the TUI leaves work running.
- [ ] **Step 5:** Update graph and commit as `feat: deliver the complete core TUI`.

## Task 11: Complete core workflow in the desktop client

**Files:** Create desktop package manifests, `apps/desktop/src-tauri` files from the map, `src/{app.tsx,engine-client.ts,screens,terminal.tsx}`, and `tests/{client-contract.test,desktop-flow}.ts`; modify workspace membership.

**Interfaces:** Consume the same protocol through `engineRequest(request: WireRequest): Promise<WireReply>` in the desktop client and a narrow Rust `engine_request` bridge. Produce the same core operations as Task 10; no browser-owned dispatch authority.

- [ ] **Step 1:** Write presentation/client unit checks and real native `desktop_core_workflow` E2E covering the Task-10 workflow independently. Add keyboard focus, readable status, reconnect, native permission fallback and hostile terminal/log text cases.

```typescript
await expect($('[data-testid="project-progress"]')).toBeDisplayed();
await expect($('[data-testid="memory-review-inbox"]')).toBeDisplayed();
await expect($('[data-testid="verification-receipt"]')).toBeDisplayed();
```
- [ ] **Step 2:** Run `bun run --cwd apps/desktop test:unit` and `bun run --cwd apps/desktop test:e2e`; expect missing behavior. Qualification must resolve the actual macOS embedded WebdriverIO route and versions; a mocked renderer or another platform's driver is not macOS proof.
- [ ] **Step 3:** Implement Tauri 2/React/TypeScript screens, typed client bridge and supplementary xterm.js terminal. Restrict Tauri capabilities to required operations, validate all bridge input, keep credentials in native/provider stores and exclude test-control endpoints from release builds.
- [ ] **Step 4:** Repeat native E2E with a real engine, native sessions and the TUI attached. Verify independent complete settings/memory/approval/verification controls, bounded terminal traffic, safe links, client closure and correct engine authority after reconnect.
- [ ] **Step 5:** Update graph and commit as `feat: deliver the complete core desktop`.

## Task 12: Prove creation, collaboration and verified Git outcomes

**Files:** Create `crates/crazyagent/tests/integrated_proof.rs`, `apps/desktop/tests/inventory-proof.ts`, `docs/qualification/proof-brief.md`; modify Git/engine acceptance and both flow drivers.

**Interfaces:** Consume the prior clients, plans, worktree leases and verification receipts. Produce `merge_candidate(project_id: &ProjectId, receipt: &VerificationReceipt, approval_id: Option<&str>) -> Result<MergeOutcome, MergeError>`; `MergeOutcome` records each repository result and uncertain operations.

- [ ] **Step 1:** Write the protected proof criteria: create/list inventory, reserve/release, input validation, no overselling under concurrency and persistence after restart. No sign-in or deployment is required for this sample API. Write real separate TUI and desktop runs starting from the same brief in different fresh projects.

```rust
assert_eq!(initial_stock, 1);
assert_eq!(concurrent_requests, 2);
assert_eq!(successful_reservations, 1);
assert_eq!(restarted_stock, 0);
assert!(gate_receipt.accepts(&proof_request).is_ok());
```
- [ ] **Step 2:** Run `cargo test -p crazyagent --test integrated_proof -- --ignored` and `bun run --cwd apps/desktop test:proof`; expect missing behavior. Generated source and verification use real harnesses/tools; do not prewrite the sample solution or replay a stored conversation.
- [ ] **Step 3:** Implement receipt-bound Git acceptance, human approval by default and configured automatic/conditional modes after mandatory gates. Support whole-set readiness by default and configured per-repository readiness. Reconcile actual Git/provider state after an uncertain mutation; pause/fix forward on partial success. Do not claim atomic multi-repository merges.
- [ ] **Step 4:** Repeat with a real authorized GitHub.com test repository, correct PR/head/baseline checks, denied approval, resource fallback, project-memory updates and an injected partial merge failure in disposable local repositories. Use gh-axi for authorized GitHub operations. Local-only projects must work without a GitHub account.
- [ ] **Step 5:** Save evidence linking briefs, source/receipts, auth modes, UI flows and Git outcomes; update graph and commit as `feat: prove integrated verified delivery`. External test PR creation needs explicit authorization beyond architecture adoption.

## Task 13: Qualify ten active sessions across three projects

**Files:** Create `crates/crazyagent/tests/load_profile.rs`; modify qualification reports and both real client drivers.

**Interfaces:** Consume admitted harnesses, profiles and engine statistics. Produce `LoadReport { active_harnesses, project_count, native_children_observed, duplicate_dispatches, hardware, versions, workload, samples, failures }` as measured QF-05 evidence.

- [ ] **Step 1:** Define a reproducible bounded real workload using 10 active top-level harness processes across 3 projects. Count separate leaders/curators/verifiers inside the ten; measure native children and runners separately. Record the host profile before execution and obtain missing account/budget approval.

```rust
assert_eq!(report.active_harnesses, 10);
assert_eq!(report.project_count, 3);
assert!(report.native_children_observed);
assert_eq!(report.duplicate_dispatches, 0);
```
- [ ] **Step 2:** Run `cargo test -p crazyagent --test load_profile -- --ignored --exact ten_sessions_three_projects`; expect unavailable or unmet evidence until actual qualification. Ten idle terminals are not the workload.
- [ ] **Step 3:** Measure and correct actual bottlenecks. Record bounded queue/storage/dispatch defaults and limits in typed settings with reasons. Initial engineering responsiveness budgets are engine control p95 at most 250 ms and visible event delivery p95 at most 500 ms on the recorded profile; these are selected qualification targets, not measured claims or universal hardware promises.
- [ ] **Step 4:** Repeat relevant engine-crash, native-child, log-backpressure and client-reconnect checks under load. Verify no duplicate dispatch, scope leakage, inaccurate cancellation, evidence corruption or missing gap disclosure. Record raw samples and any target revision with its benchmark reason.
- [ ] **Step 5:** Publish the sanitized qualification/minimum-profile report, update forecast/graph and commit as `perf: qualify the multi-project workload`. A failing launch requirement remains a blocker or explicit owner decision.

## Task 14: Review, package and qualify release installation

**Files:** Create `.github/workflows/{checks,native-qualification}.yml`, `crates/crazyagent/tests/smoke.rs`, `docs/qualification/release.md`, release configuration and third-party notices; modify packaging only as required.

**Interfaces:** Consume immutable source, complete QF-01 through QF-05 evidence and the accepted forecast. Produce installable macOS artifacts, real installation evidence and the required review ledgers.

- [ ] **Step 1:** Wire unit checks, formatting/lints, real integration/E2E/smoke qualification and dependency/license checks. Include `cargo fmt --all -- --check`, `cargo clippy -p crazyagent --all-targets -- -D warnings` and `cargo test -p crazyagent --lib`. Implement and run `cargo test -p crazyagent --test smoke -- --ignored`: the actual engine IPC and SQLite are healthy, admitted runtime/runner heartbeat is reachable, and the desktop reports completed startup; this is infrastructure health rather than feature proof. Verify expected named tests actually executed, including explicitly ignored native tests, rather than accepting a zero-test exit. Run native accounts only on the explicitly controlled macOS runner, never with secrets exposed to arbitrary fork code. A missing native job is not a passing release gate.
- [ ] **Step 2:** Boot the real test desktop with its test-only WebDriver HTTP surface and the real proof API. Run pinned Nuclei/template revisions with medium/high/critical severity gating against declared applicable HTTP surfaces. Preserve target/template IDs and findings. This covers those surfaces, not native IPC or the whole application. If no applicable meaningful template profile qualifies, record the gap and obtain the gate-policy decision before publication; do not create a production HTTP control service just for scanning.
- [ ] **Step 3:** Freeze a clean committed review target with repository/base/head/scope. Preserve all existing DeepSec raw candidates, findings, history and metadata outside the checkout, export all findings including resolved, run deterministic discovery and snapshot refreshed candidates. Run exactly two parallel tracks against the same frozen target: normal code review and Codex Security using ChatGPT auth, Daybreak Blue, xhigh and the entire evidence directory. Read the owner's security-review prompt. No DeepSec AI processing or fallback model/billing.
- [ ] **Step 4:** Reconcile every finding/candidate with source IDs, hashes and confirmed/refuted/duplicate/already-fixed/deferred dispositions. Fix required defects and run relevant checks. Permit one confirm round only after substantial fixes. Record model, scan ID, coverage, gaps, elapsed time and reported usage/cost. Missing setup, inputs or dispositions means partial review. Do not edit the target during either review.
- [ ] **Step 5:** Package and test install/start/update/uninstall on the recorded macOS profile without destroying user projects/state. Qualify signing/notarization only with authorized credentials; label any unsigned prerelease accurately. Refresh graph after reviewed changes, reconcile the release forecast and present the exact artifacts/review evidence for separately authorized publication.

## Settings coverage

These are implemented settings and policy boundaries, not additional user interviews. Each setting has one typed definition, validation, scope/provenance and application timing in Task 5; Tasks 10/11 expose the same catalog.

| Area | Required behavior | Owning tasks |
|---|---|---|
| Leader and routing | Coordinate-only default; configurable implementing mode; rule/eligibility/resource routing; approved fallback; binding hard targets; approved persistent preference changes | 3, 5, 7 |
| Scheduling and collaboration | Fair sharing default, strict-priority mode; bounded concurrency; scoped messages/links; accounts and destination/budget eligibility | 5-7 |
| Memory/activity | Immediate provisional project knowledge; global prior approval; approved local/cloud curation; retain-until-delete default and expiry option; pause default or explicit/configured continue-with-gaps | 3, 5, 8 |
| Verification and Git | Mandatory floor and risk depth; human full-suite request; bounded infrastructure retry; manual merge default with automatic/conditional modes; whole-set/per-repository readiness and partial-failure handling | 5, 9, 12 |
| Assets and configuration | Canonical global/project files, linked defaults, task pins and explicit migration; managed native projections and reviewed imports/exports; immediate revocation; no credential material in files | 5, 8 |
| Storage and platforms | Private local permissions launch; application-managed encryption later; macOS first; future OS/provider/Git-host capability stays unavailable until implemented and qualified | 2, 5, 14 |

## Conditional release checkpoints

The dates retain the owner's delivery target and historical release rhythm. The Task-4 forecast must replace this table when evidence changes it.

| Target | Outcome |
|---|---|
| First 16 working hours | Admission evidence, sandbox/native recovery findings and revised forecast |
| October 30 | Candidate integrated core: central leaders, admitted native sessions, durable authority, task worktrees and a real TUI path |
| November 6-13 | Protected verification, shared assets/memory, complete core TUI and desktop paths; exact order follows the evidence forecast |
| November 20 | Both independent proof flows and shared coordination/Git behavior qualified |
| November 30 | Conditional public beta with load, installation, reviews and stated qualification coverage |

A partial checkpoint is not a public release claim. Do not silently ship a terminal launcher in place of the accepted product. If capacity cannot support the adopted scope, show the concrete evidence and ask for the date or scope decision.

## Requirement trace and self-review

| Accepted scope | Implementation coverage |
|---|---|
| Rust/dual UI/session ownership and recovery, DEC-002/004/006/046/048/054/057-059 | Tasks 1-5, 10-11, 13-14 |
| Leaders, routing, accounts, worktrees and agent-oriented status, DEC-007-014/027-028/036-041/053 | Tasks 3, 5-7, 10-13 |
| Deterministic gates, risk, baseline/test integrity and multi-repository acceptance, DEC-015-019/029-031/038-040 | Tasks 2, 5-6, 9, 12-14 |
| Activity, curated/global memory and configuration/assets, DEC-020-026/032-035/042/055 | Tasks 3, 5, 8, 10-11 |
| Proof API, load, platform/repository/host/license and delivery contract, DEC-001/003/005/043-045/047/049-052/056 | Tasks 1-4, 10-14 |

F1-F15 in the adopted design map to native accounts/session management (3/5/7), memory/configuration/assets (5/8), project/worktree operations (6/12), isolation (2/3/9), coordination/visibility (7/10/11), resource-aware routing (7/13) and integrated verification (9/12/14). Accepted later requirements supersede narrower historical F6/F14 assumptions.

Self-review completed for source/decision coverage, step specificity, shared type/signature consistency, the five Review Focus cases and plan proportion. The plan intentionally leaves actual compatible versions, sandbox/native mechanism results and measured forecast to the named qualification tasks. No capability is asserted proven. Runtime commands above are planned check entry points, not executed results.

## Execution handoff

Q-050 asks the owner to confirm the plan and select execution:

A. Native execution, recommended for the shared engine/runner/qualification interfaces: one implementer carries each task; the fresh normal reviewer is the code-review track alongside the required Codex Security track at the frozen final review. This is not a third track.

B. Subagent-driven execution: a fresh implementer and reviewer per task, each in an isolated worktree, with extra context/coordination cost and the same final review gates.

C. Revise the plan before execution.

The next bounded action after A or B is Task 1's local repository/toolchain inventory and first admission test. Missing installation/account/budget authorization is requested only after the concrete manifest exists.

