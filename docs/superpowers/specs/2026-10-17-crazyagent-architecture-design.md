# CrazyAgent architecture and qualification proposal

Status: Adopted in DEC-059 following the owner's A answer to Q-049. This is a forward-looking design, not implemented functionality or a release qualification result.

Review: the [builder check](2026-10-17-crazyagent-builder-check.md) found no consequential unanswered question for implementation planning. Its result does not establish runtime qualification or approve the proposal on the owner's behalf.

Date: October 17, 2026.

Authority: [DEC-001 through DEC-059](../../agent-decisions.md) are the accepted product, framework and architecture decisions. DEC-059 adopts P-01 through P-11 as the implementation direction, retaining their stated qualification boundaries. Proposal labels below preserve the reviewed design; earlier pending-approval wording is superseded by DEC-059. The [October 16 roadmap](../plans/2026-10-16-integrated-application-roadmap.md) preserves the historical release sequence; later decision records control expanded scope and supersede narrower assumptions.

## 1. Product and delivery contract

Build one local operating environment for coordinated software development. A human gives work to a central conversation. A workspace leader coordinates authorized projects; project leaders coordinate work items, harnesses and native children. Rust enforces permissions, routing eligibility, resource policy and verification gates. Leaders cannot grant themselves authority.

Both the primary TUI and desktop independently support project setup, settings, briefs, plans and approvals, work supervision, change and evidence inspection, memory review and authorized merges. Both can connect concurrently. Closing either or both clients detaches them from ongoing work. Terminal input and resize ownership require explicit control transfer.

The first public release qualifies macOS, Claude Code and Codex, local Git and GitHub.com, and user-owned or reviewed repositories. RCLI remains the selected leader foundation to qualify and adapt. Launch requirements include live-worker survival and reattachment after an engine-only crash, individual status/cancellation/recovery state for admitted native child workflows, and a target qualification of ten active top-level harness processes across three projects. This is not observed capacity. Workspace and project leaders, curators and verifiers count when they are separately launched harnesses. Runner helpers and native children are measured separately.

Original code is intended for Apache-2.0. Third-party reuse retains notices and requires compatibility review at selected revisions. Launch stores application records privately with restricted permissions and separate credential protection. Application-managed encryption remains required later, without an assigned release date.

The owner supplies 30-40 hours per week with AI coding agents, with approximately 20% reserved for checks, review, fixes, packaging and documentation. November 30 remains a conditional broader-beta target. The expanded launch requirements need a new forecast after qualification; historical dates are not proof of feasible effort.

## 2. Proposed system boundaries

| Proposal | Recommendation | Why and authority boundary |
|---|---|---|
| P-01 | One user-local Rust engine using Tokio, Serde and versioned local messages | One authoritative owner prevents the two interfaces from creating competing task, policy or merge state. UI shells do not own worker lifetime. |
| P-02 | A separate surviving Rust session runner for each managed top-level harness session | A runner owns the harness control connection, pipes or PTY, output capture and process identity across an engine-only crash. It executes admitted operations and reports facts; it is not a second planner or permission authority. |
| P-03 | Ratatui/Crossterm TUI and Tauri 2/React/TypeScript desktop clients | The frameworks are accepted in DEC-058. Both invoke the same engine operations. Desktop Rust code is a narrow bridge, not an independently mutable execution backend. |
| P-04 | SQLite metadata through one rusqlite database worker; separate captured-activity segments | Serialize durable coordination changes and operation identities. Large streams must not become unbounded in-memory queues or a write transaction per token. This does not replace canonical settings files. |
| P-05 | Canonical global and project configuration files with a typed, versioned schema; TOML proposed | Human edits and both settings interfaces use one validation path. Skills and plugins preserve their supported native formats rather than being rewritten into a fictitious universal format. |

Proposed initial local transport is an owner-restricted Unix-domain socket on macOS. Windows named pipes are a future port with explicit access checks; Windows support is not established by choosing a portable API. Control messages have version, request identity, actor, target scope and expected revision. Notifications have cursors. Reconnection uses an authoritative snapshot and subsequent events; a gap triggers resynchronization.

Bound every message, queue, retained in-memory view, launch pool and retry loop. Set concrete limits in the typed configuration before implementation qualification. Show effective values and their sources. Critical persisted changes are durable before acknowledgement. A slow client must not block harness output capture or another client. Transient notifications are not a durable log.

Application-owned secrets use supported OS secure storage or qualified provider-native stores. Configuration, metadata and logs contain references, not copied refresh tokens or API secrets. Native account sign-in remains provider-specific. Do not assume that a subscription token can be converted to API billing or reused across harnesses.

### Session supervision and recovery

A runner is a separate process with a lifetime independent of the engine and both UIs. Keep the adapter's live input/output connection inside that surviving boundary; reconnecting a new process to a saved conversation is insufficient. The runner's private control endpoint is outside worker access. OS startup/supervision details are part of macOS qualification, with logout/reboot guarantees left outside the accepted engine-only crash guarantee.

Persist operation identities before dispatch. After a crash, reconnect and reconcile actual runner, harness, child, worktree and Git state before new dispatch. Reject stale controller identities. Do not rely only on a reused PID. An operation whose outcome is uncertain stays uncertain until reconciled; never blindly repeat a launch, tool action or merge because an acknowledgement was lost.

Existing admitted work may continue within its recorded authority while the engine is unavailable. New permission requests wait for a valid controller decision. Authority expiration or revocation must stop or contain affected work under a qualified mechanism. If the runner cannot enforce a required grant transition, the affected execution profile is unsupported rather than silently permissive.

The runner continues qualified activity capture while the engine is down. A capture failure follows DEC-025 and DEC-026: controlled pause by default, with an explicit configurable continue-with-gaps mode. Never represent a gap as complete capture. Per-runner crash and machine reboot are separate failure cases; show accurate states without promising identical guarantees.

## 3. Coordination, adapters and shared knowledge

| Proposal | Recommendation | Required behavior |
|---|---|---|
| P-06 | Managed RCLI leader integration before any targeted fork | Preserve the owner's reuse choice. Qualify persistent structured leadership, cancellation and tool confinement. Fork only a demonstrated gap; do not replace the selected harness with a new Rust model loop by default. |
| P-07 | Structured native adapters for Claude and Codex, with explicit capability profiles | Normalize identity, lifecycle, approvals, usage availability and error state. Keep unsupported features visible. Terminal parsing cannot certify child lifecycle or verification. |
| P-08 | Engine-mediated task routing and scoped mailboxes | Separate proposed work from eligible execution. Task type, supported model/harness/account combinations, current availability, limits, destination approvals and permissions constrain dispatch. Messages never grant authority. |

RCLI's one-shot headless mode is not assumed to be a persistent host. Its documented background behavior and permission limitations require an actual integration proof. Keep the adapter process and native control connection in the surviving session boundary when needed. Do not make Python a new runtime dependency solely because a Python SDK reference demonstrates a provider capability.

Represent workspace, project, work item, managed session and native child identity separately. A native child may be a provider thread rather than an OS process. Child cancellation must distinguish request acceptance from confirmed stopping. After recovery, distinguish live continuation, ended work, supported resumption and a replacement. Report unavailable per-child usage honestly. Every native workflow admitted to launch must satisfy DEC-054.

Plan approval establishes bounded delegated work. Routine eligible dispatch and approved fallback can proceed within that authority. Expanded scope, spending or permissions require the applicable approval. Persisted routing preference changes require human approval. Existing credentials do not authorize a new billing route. A workspace leader sees only its admitted projects and cross-project information; it is not automatic access to every memory.

Same-task peers use scoped engine mailboxes. Cross-task peers need explicit coordination links; other communication goes through the appropriate leader. Validate sender identity at the engine. Preserve delivery and processing evidence without claiming exactly-once model behavior.

Project/global assets have explicit provenance and precedence. Ordinary project settings can override ordinary global defaults. Running work pins effective configuration; task migration is explicit, and security revocations take effect through the supported enforcement path. Dynamic project knowledge can become available without silently replacing the pinned policy.

Captured activity, memory candidates and curated knowledge are distinct records. Periodic extraction and worker nominations feed project curation. Project knowledge is immediately available with provenance and review state. Global promotion waits for human approval in the review inbox. Memory is evidence and context, not policy authority. Approved cloud curation receives minimized excerpts under its destination scope. Only observable provider activity is captured; hidden reasoning is not invented or promised.

## 4. Isolation, Git and verification

| Proposal | Recommendation | Qualification boundary |
|---|---|---|
| P-09 | First qualify an existing macOS whole-process sandbox runtime, with Anthropic sandbox-runtime as the initial candidate | Apply declared filesystem, network, credential and resource profiles to entire harness/tool trees and candidate execution. Inspect effective defaults. Selection is conditional on actual enforcement, auth compatibility and recovery. |
| P-10 | Keep trusted host operations narrow and outside worker write/control authority | Engine-controlled Git operations, credential access, sandbox setup and accepted evidence need real host privileges. Inventory each operation and allowed communication path. Do not call an unrestricted trusted host component OS-sandboxed. |
| P-11 | A separately confined verification path over an immutable candidate snapshot and approved baseline | Code and agent-written tests are checked under independent authority. Candidate code cannot edit protected checks, accepted receipts, engine policy or trusted executables. |

The application boundary restricts exposed operations: the desktop receives narrow engine commands, the TUI uses the same authority, and harness tools receive scoped work operations rather than arbitrary engine administration. The execution boundary enforces the declared OS-level restrictions around code-generating and code-executing processes. These are the two complementary layers from DEC-050; they are not a promise that every host process is in two nested containers.

Tauri capabilities do not confine its Rust host code or spawned harnesses. File permissions and an independent verifier directory do not protect against an unsandboxed same-user process. Reject a launch profile that lets candidate code reach protected control endpoints or modify the baseline/receipt authority. Declare actual CPU, memory, process and network enforcement and any gaps. Do not invent resource-cap guarantees from a filesystem sandbox.

The initial sandbox candidate must pass the same admission checks for the workspace/project leaders, coding harnesses, tools/plugins, memory processors that execute tools, and code under test. Provider sign-in and necessary host interactions require narrow, reviewed integration; broad application automation or unrestricted host sockets are not an acceptable convenience fallback. If the candidate fails a required boundary, stop the affected qualification and propose a replacement runtime with its impact. A separately installed runtime is allowed, but no installation is authorized by this draft.

The engine creates per-work-item worktrees and enforces one concurrent writer per worktree, including native descendants. Other team members can inspect or use their own admitted worktrees. Protect shared Git metadata; worktrees alone are not isolation. Trusted host Git operations must account for repository-controlled hooks, filters and executable configuration. Preserve dirty work and require confirmation for destructive cleanup.

A project may register multiple repositories. Whole-change-set readiness is the default before the first merge; individually eligible merges remain configurable. Cross-repository merges are sequential, not atomic. On a confirmed partial merge failure, reconcile and pause the affected sequence, prefer fix-forward, and reverify. Independent work continues within its authority.

The verifier receives the candidate source identity and human-approved acceptance baseline. It applies the mandatory code and test-integrity floor plus deterministic risk-selected depth. AI findings are recorded inputs to deterministic policy, not deterministic model outputs. Preserve all verification attempts; only eligible bounded transient infrastructure retries are automatic.

Receipts bind source, baseline, configuration, environment and check identities to actual outcomes. Missing, failed, stale or unknown evidence cannot pass. Before acceptance or merge, apply the current approved contract where required by DEC-019. Human-approved baseline changes are separate from ordinary plan approval and ordinary automatic merge settings. A low-risk auto-merge policy cannot weaken these gates or provider branch protection.

Developer-visible checks and protected holdouts remain separate. The human can inspect protected checks. Coding-worker feedback is actionable without routinely leaking the full protected suite. Test integrity means evidence that qualified checks detect selected defects and resist the admitted tampering paths; it does not prove absence of every bug.

## 5. Complete core interfaces

The common journey is project creation, brief and requirement capture, human review of proposed acceptance criteria, leader planning, approved parallel work, status and available usage, independent verification, change inspection and authorized merge. Both interfaces expose settings with effective values, provenance, validation errors and application timing, plus the memory review inbox.

Desktop: React/TypeScript presents the central conversation, project navigation and task details. Rust commands validate identities and scope before reaching the engine. Bundle trusted assets, sanitize untrusted rendered content, and give terminal surfaces explicit input-control state. xterm.js is the proposed supplementary terminal renderer.

TUI: Ratatui/Crossterm presents the same operations using keyboard navigation. Restore terminal state on exit and failures. Native terminal attachment is a qualified supplementary mode, with explicit input/resize ownership. Do not make embedded native terminal panes the primary interaction or build an unnecessary terminal emulator.

Per-interface screenshots or renderer mocks do not establish core parity. Qualify the real engine and actual client controls. The proposed macOS desktop route is WebdriverIO with Tauri's embedded driver, not an unsupported assumption about direct tauri-driver on macOS. Test-only control surfaces must be absent from release builds. TUI end-to-end qualification drives the actual application through a PTY and checks the whole journey.

## 6. Qualification plan before broad implementation

The following are vertical evidence gates. Timeboxes are review checkpoints, not promises that an unknown capability will pass in that time. Reforecast after the first 16 working hours of qualification. Run paid/provider-connected checks only within an approved destination and budget; this design approval alone does not authorize installation or account connection.

| Gate | Concrete result | Evidence needed |
|---|---|---|
| QF-01: recover one real managed session | Establish a declared isolation profile, start an admitted harness through the candidate runner, crash only the engine, restart and continue the original live worker | Actual runner and harness identities, original control connection retained, captured events, no duplicate dispatch, unchanged worktree, permissions and logging behavior |
| QF-02: protect verifier authority | Run positive and negative controls, including candidate attempts to modify trusted baseline, receipts or control state | Allowed application work succeeds; denied filesystem/control paths fail; test-integrity controls fail when selected defects are introduced; effective sandbox grants and limitations recorded |
| QF-03: complete one proof through each UI | Create the approved local inventory/reservations API from a brief using the central leader and qualified Claude/Codex routes | Separate real TUI and desktop runs; approval, create/list, reserve/release, validation, concurrent no-overselling, restart persistence and evidence inspection all work without mocked integration |
| QF-04: preserve shared project behavior | Exercise global/project assets, pinned settings, scoped messages, memory curation and review, eligible routing/fallback, and a GitHub pull request under admitted policy | Provenance, permission rejection, current-head gate checks, accurate known/estimated/unavailable usage, preserved dirty work, and partial-failure behavior |
| QF-05: qualify load and shipping | Operate ten active top-level harness processes across three projects on a recorded macOS profile, then repeat relevant crash and client reconnect checks | Hardware/software versions, workload, process and native-child counts, responsiveness/resource observations, quotas, gaps, required reviews, packaging and installation evidence |

Start with QF-01 and QF-02 dependencies; do not bypass required confinement to obtain a green adapter demo. The first implementation plan must split these into small owner-assigned slices and specify the first runnable failing check for each. If a native child workflow cannot qualify individual control, it is a launch gap requiring an explicit scope or design decision. App-managed replacement workers are not a substitute proof.

The qualification log records source revision, resolved package/runtime versions, capability profile, actual authentication mode, operation/source/baseline identifiers, expected and observed outcomes, evidence paths and unresolved gaps. A screenshot, successful exit or documentation claim is not sufficient evidence for a process or security guarantee.

Unit tests precede implementation and exercise real modules. Integration, end-to-end and smoke checks use real systems with no mocks. Property checks and concurrency tests target the accepted invariants where useful. Preserve the user's required frozen normal review and Codex Security track with DeepSec evidence before publication; no review has run for product code because there is no implementation.

The future PR pipeline must also boot the relevant real test surface for the required Nuclei medium/high/critical gate. Define the actual reachable surface and template coverage in the implementation plan; do not expose a production HTTP control service solely to make a scanner run, or label a native control channel scanned without evidence.

## 7. Original feature coverage and later work

This map updates the historical interpretation without claiming completed features.

| Original feature | Current design treatment |
|---|---|
| F1: harnesses in one layer | Qualified Claude/Codex workers and RCLI leader integration first; additional adapters later |
| F2: shared memory | Project/global curation, scoped knowledge, provenance and human global promotion |
| F3: shared context and continuation | Retained observable activity, decisions, artifacts and qualified handoffs; no hidden-reasoning or arbitrary live-state transfer promise |
| F4: shared plugins, skills, rules and settings | Canonical supported assets, explicit precedence, adapter projections and visible incompatibilities |
| F5: account and subscription fallback | Supported isolated accounts and approved eligible fallback; actual provider/auth combinations require qualification |
| F6: lifecycle including native children | Live-worker engine-crash survival and individual native child control are launch gates under later DEC-046 and DEC-054 |
| F7: parallel worktree lifecycle | Isolated work items, one concurrent writer, preserved dirty state and explicit destructive cleanup |
| F8: sandboxing | Declared application/execution boundaries and qualified enforcement; reviewed repositories first, hostile-repository expansion later |
| F9: sharing between parallel sessions | Scoped knowledge and direct task mailboxes; explicit cross-task coordination links |
| F10: machines and cloud continuity | Later work; no instant runtime/credential continuity promise |
| F11: usage and activity analysis | Available activity capture and attributed known/estimated/unavailable usage; further analysis expands later |
| F12: OS element/area picker and comments | Later conditional expansion; the core desktop workflow does not claim this implemented |
| F13: independent code and test integrity | Protected human-approved baselines, source-bound evidence, mandatory checks and risk-selected depth |
| F14: automatic task/resource routing | Core engine routing under approved rules and fallback authority; persistent preference changes require human approval |
| F15: one open-source application | One integrated product with both complete core clients, Rust authority, declared prerequisites and Apache-2.0 original code intent |

Application-managed storage encryption remains required later under DEC-055. Broader untrusted repositories target a later V2/V3 qualification milestone under DEC-049, without a new calendar commitment. The full agent-oriented ticket system, additional hosting providers, cross-machine operation and additional OS/harness matrices remain later scope unless an accepted record explicitly brings them into launch.

## 8. Adopted architecture and builder discretion

The owner adopted P-01 through P-11 in DEC-059, with the UI frameworks already accepted. The [implementation plan](../plans/2026-10-17-crazyagent-implementation-plan.md) is written for execution-method selection in Q-050. Adoption does not turn the sandbox candidate, adapter capability or live recovery design into a passing qualification result.

The key tradeoff is an independent runner process and explicit adapter/sandbox qualification in exchange for the accepted live-worker survival and native-child requirements. A desktop-owned engine or a restart-only worker model would be smaller, but does not meet those accepted requirements. Additional helper processes and a small TypeScript leader integration are compatible with Rust as the main engine.

Builder calls still permitted without a new product decision include module/file names, exact message encodings within the stated protocol, bounded defaults justified by qualification, detailed layouts, test helpers and selected compatible dependency versions. A new privilege boundary, weakened gate, changed launch workflow, new external destination/spending authority or removal of a required interface needs the applicable decision or approval.

Open qualification details include native account mechanisms, runner OS registration and crash capture, exact sandbox profile/host integration, resource control coverage, selected adapter revisions, data schema/migrations, settings migration, package signing and the revised forecast. These must be resolved or truthfully classified before claiming the affected capability complete. No product code, installer or provider-connected execution has been created by this draft.

Sources: [decision register](../../agent-decisions.md); [dual-interface research](../../../reports/Rust%20dual%20interface%20architecture.md); [RCLI assessment](../../../reports/RCLI%20leader%20reuse%20assessment.md); [memory architecture](../../../reports/Project%20and%20global%20memory%20architecture.md); [historical roadmap](../plans/2026-10-16-integrated-application-roadmap.md).

Primary technical references: [Tauri process model](https://v2.tauri.app/concept/process-model/), [Tauri security](https://v2.tauri.app/security/), [Ratatui backends](https://ratatui.rs/concepts/backends/comparison/), [Tokio process options](https://docs.rs/tokio/latest/tokio/process/struct.Command.html), [portable-pty](https://docs.rs/portable-pty/latest/portable_pty/), [SQLite WAL](https://sqlite.org/wal.html), [rusqlite](https://github.com/rusqlite/rusqlite), [RCLI](https://github.com/Backboard-io/Backboard-R-CLI), [Claude SDK](https://code.claude.com/docs/en/agent-sdk/python), [Codex App Server](https://learn.chatgpt.com/docs/app-server), [sandbox-runtime](https://github.com/anthropics/sandbox-runtime), [Tauri WebDriver](https://v2.tauri.app/develop/tests/webdriver/) and [xterm.js security](https://xtermjs.org/docs/guides/security/).
