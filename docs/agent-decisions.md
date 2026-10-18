# Agent decisions

Current authority: DEC-001 through DEC-072 record the owner's latest accepted requirements, workflow and scoped implementation decisions. Earlier dated sections preserve planning history and may describe proposals subsequently approved or superseded in the numbered register. Q-001 through Q-057 are answered in their corresponding records. DEC-020 through DEC-024 establish project/global memory curation, the human review inbox, immediate provisional project memory, prior approval for global promotion, approved local/cloud processing destinations and retained activity until explicit deletion by default. DEC-025 supports both logging-failure behaviors; DEC-026 sets pause as the default with an explicit continue-with-gaps action and a configurable preferred mode. DEC-027 permits suitable approved routing alternatives while required-target rules remain binding. DEC-028 requires human approval for persistent routing-preference changes while routine selection and approved fallback remain automatic. DEC-029 combines visible requirements and development tests with protected holdout cases withheld from coding workers. DEC-030 requires a verification floor for every change, with additional depth selected by approved deterministic risk rules and a human option to request the full applicable suite. DEC-031 permits bounded automatic retries of temporary verification infrastructure failures under approved rules, preserving attempt history and unresolved defect or unknown-failure states. DEC-032 makes editable files authoritative for shared rules, skill definitions and shareable configuration, with engine validation and derived harness representations. DEC-033 links projects to accepted global defaults for new work, with explicit project overrides for ordinary overridable settings and visible provenance. DEC-034 pins effective configuration per active task, including new and replacement workers, until an explicit task-wide migration. DEC-035 uses managed session configuration by default, with reviewed imports and deliberate reviewed exports to usual native CLI destinations. DEC-036 permits peer-addressed messages within a task team through an engine-controlled mailbox, preserving leader assignment authority. DEC-037 requires explicit coordination links for same-project cross-task peer addressing, with unlinked requests routed through the leader. DEC-038 defines a project as a product workspace containing one or more explicitly registered repositories with scoped task access and attributable worktree and verification state. DEC-039 supports coordinated whole-change-set readiness by default and independently eligible member merges as a selectable mode, with an application setting and explicit project override. DEC-040 prefers a fix-forward recovery recommendation after confirmed partial merge failure, subject to existing authority and gates. DEC-041 supports both fair sharing and strict project-priority scheduling, with fair sharing as the default and an exposed setting. DEC-042 establishes broad workflow configurability through readable canonical files and settings in both interfaces, with shared validation and visible effective values. DEC-043 selects creating a small new application from a product brief as the first complete representative workflow. DEC-044 selects a small API service with persistent data as the first generated application's shape. DEC-045 selects the local inventory and reservations API proof, including concurrent stock consistency and persistence across restart, without sign-in or deployment. DEC-046 makes durable recovery plus live-worker survival and reattachment after an engine-only crash a first-public-release requirement for launch-qualified harnesses. DEC-047 selects a first-release qualification target of 10 active managed harness sessions across 3 projects on an agreed macOS profile, separate from configurable resource limits. DEC-048 permits central approval controls with explicit native-prompt handoff where a supported central integration is unavailable. DEC-049 selects user-owned or reviewed projects for first-release qualification and broader untrusted-repository support as a V2/V3 target. DEC-050 records layered application and execution isolation as a security goal with explicit host privileges and platform qualification. DEC-051 permits a documented separately installed local sandbox runtime prerequisite when the qualified isolation design needs one. DEC-052 selects Apache-2.0 for CrazyAgent's original code, retaining third-party terms and integration-specific compatibility review. DEC-053 requires a workspace-level leader above project leaders in the first public release, with explicit project authorization and preserved project boundaries. DEC-054 requires individual native child-agent status, cancellation and recovery or continuation-state coverage for native workflows in launch scope, subject to adapter qualification. DEC-055 selects private local storage for launch and retains configurable application-managed encryption as a required future capability, with no date assigned. The [stack and reuse research](../reports/Rust%20dual%20interface%20architecture.md), [RCLI qualification](../reports/RCLI%20leader%20reuse%20assessment.md) and [memory architecture assessment](../reports/Project%20and%20global%20memory%20architecture.md) inform the interview. Runtime integration, logging coverage and recovery mechanics, task classification, availability and handoff mechanics, baseline-transition mechanics, concrete verification checks and risk rules, retry classification and limits, flaky-test resolution, holdout feedback and confinement, configuration catalog and serialization, native configuration loading, detailed conflict and task-migration mechanics, messaging delivery and cross-project scope, cross-repository acceptance and recovery execution, resource scheduling algorithms, concrete memory schemas, scale and revised schedule remain unresolved. DEC-056 selects local Git and GitHub.com as launch integration scope, with other hosts later. DEC-057 requires independent complete core workflow coverage in both the TUI and desktop at launch. The recorded product and authority decisions are ready for a technical design synthesis; stack and implementation approval remain outstanding. DEC-058 adopts Ratatui/Crossterm and Tauri 2 with React/TypeScript as the interface framework baseline. A detailed design and qualification proposal is next; other implementation recommendations have not been silently accepted.

Architecture adoption: Q-049 is answered A and recorded in DEC-059. The full architecture and qualification proposal, including P-01 through P-11, is adopted for implementation planning. Implementation and runtime qualification remain outstanding. This supersedes earlier statements that these implementation recommendations were still awaiting an owner decision.

Implementation handoff: Q-050 is answered with subagent-driven execution and the owner's amendments in DEC-060 through DEC-062. The [implementation plan](superpowers/plans/2026-10-17-crazyagent-implementation-plan.md) is active. Use visible Herdr panes with isolated worktrees; skip code/security reviews for this development phase; preserve real testing without mocks or bypasses. The local Git repository is initialized and source work begins on a task branch. No runtime capability is implied by initialization.

## 2026-10-16: CrazyAgent releases through November 30

### Goal

Release a useful open-source integration quickly, then expand toward most of the fifteen feature areas in the original research by November 30, 2026 if the experiment succeeds. The first month ends November 15; the remaining period is an expansion and stabilization window.

### Confirmed constraints

The owner confirmed full-time delivery capacity of approximately 30-40 hours per week with AI coding agents. The owner approved macOS with Claude Code and Codex for the first public release, with additional harnesses and cross-machine support considered for later November releases. The project remains an integrated open-source tool, rather than a company requirement.

### Decisions

The initial proposal to make the first release solve local launch, workspaces, and handoff was superseded by the owner's clarification below. Retain the single-install product direction and reuse existing components when licensing, interfaces, and real workflow checks permit.

Keep native authentication under the user's control. Distinguish portable task records from live agent runtime state. Publish feature-specific support levels instead of claiming identical plugin, permission, credential, or subprocess behavior across harnesses. Feature experiments must not silently weaken permissions or data protection.

Reserve approximately 20% of delivery capacity for verification, review, fixes, release packaging, and documentation. Treat calendar dates as targets and move unfinished capabilities rather than bypassing required release checks. Keep the November 30 ambition conditional on measured usefulness and adapter feasibility.

### Rejected alternatives

A universal fifteen-feature first release would prevent a useful early launch and conceal unsupported behavior. Building separate competing memory, verification, sandbox, and orchestration systems before integration experiments would spend the limited schedule without proving an advantage. A contribution-only strategy would not necessarily solve the owner's need for one usable installation, so a thin integrated distribution remains an allowed outcome.

### Risks

There is currently no Git repository or implementation in this project directory. Existing component suitability and redistribution rights remain unqualified. Provider behavior, account integration rules, permission semantics, and live-process recovery constrain feature fidelity. One full-time builder cannot be assumed to deliver the entire original interpretation of all fifteen features in six weeks. The documented review workflow requires tooling and model access that have not been verified for this project.

### Acceptance criteria

The roadmap maps every original feature F1-F15 to a release, an explicit supported boundary, or a documented deferral. The first release has a runnable real workflow and a recovery check. Expansion decisions use measured recovery usefulness, valid completion, and operating effort against existing tools. Every published release includes supported versions, known limitations, appropriate real-system checks, code review, and the required security review evidence or an explicit unresolved release blocker.

### Unresolved questions

The proposed release sequence, experiment thresholds, and bounded November feature definitions require review with the owner. The first reusable host and implementation stack require a small technical qualification before commitment. External pilot availability is unknown. Additional operating systems, centralized account sync, transparent child-process migration, and any dynamic multi-agent workflow remain outside the confirmed first-release scope. Permission for dynamic workflow features must be obtained before their implementation.

### Source of truth

The original scope is the [research document](Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md). The initial assessment in `reports/Meta harness research verdict.md` is historical and its product framing is superseded by the integrated application scope correction below. This decision record captures the owner's confirmed planning inputs and distinguishes them from planning assumptions.

## 2026-10-16: Integrated application scope correction

### Goal

Reassess and plan one project-centric application that integrates the owner's existing agent workflows. The owner rejected a release sequence centered on launching sessions, worktrees, handoffs, parallel work, usage views, and sandbox use as separate capabilities, because the owner already uses those capabilities across separate tools.

### Confirmed constraints

The full-time capacity of 30-40 hours per week, macOS with Claude Code and Codex first, a fast public release, and the conditional November 30, 2026 target remain in force. The application's integrated project experience is the central product requirement.

### Decisions

A user creates projects inside one application. Each project can run multiple harnesses against one shared project memory and shared configuration, with both global and project-local skills, plugins, rules, and memory considered. Harnesses can communicate through the application. Individual harnesses can have multiple accounts. A project-level testing and verification layer is independent of the coding harnesses and their shared memory and assesses work produced by those harnesses. The owner explicitly confirmed that this layer must verify project code and the integrity of agent-written tests, while preventing coding harnesses from changing its trusted verification rules.

Reopen the research verdict and release design around that complete workflow. Compare existing applications by whether they coordinate this workflow inside one project, rather than treating the existence of each feature in a separate tool as disproof of the application. Reuse remains an implementation strategy and does not replace the requirement for a unified product experience.

### Superseded decisions

The six-release sequence proposed in chat on October 16, beginning with launch/worktree/handoff capabilities, was rejected by the owner and is withdrawn. The earlier assessment's preference for a thin distribution is no longer an approved product direction. Its factual findings may require fresh verification, but they do not settle the revised product verdict.

### Rejected alternatives

A recipe, configuration bundle, or collection of tools that leaves the owner coordinating projects manually does not fulfill this application request. A minimal release must demonstrate the integration between harnesses, project state, and independent verification, rather than only wrapping an already working native session.

### Risks

Unified presentation does not automatically provide shared configuration semantics, enforce independent verification, or make credentials portable. The research must assess the application value separately from those implementation boundaries. A broad November target remains conditional on a successful integrated workflow and measured implementation effort.

### Acceptance criteria

The new verdict explicitly evaluates the integrated project application. The first proposed release contains a complete create-project, launch-harnesses, share-context, communicate, verify-work workflow. Every original F1-F15 feature remains mapped to the revised sequence with supported boundaries. Account support distinguishes legitimate profile selection from prohibited limit evasion. Verification independence is stated concretely rather than inferred from another prompt or agent.

### Unresolved questions

The verification requirement is confirmed. The renewed research proposes shared-state semantics, bounded peer communication, a protected verification authority, and a five-release sequence. These are reviewable design proposals, not implemented or qualified capabilities. The reusable foundation, permitted authentication paths, enforceable isolation, external pilot availability, and measured delivery effort still require qualification. Dynamic orchestration remains unapproved.

### Current research and proposed roadmap

The [renewed verdict](../reports/Integrated%20agent%20application%20verdict.md) evaluates the complete integrated application and records competitor evidence and its limits. The [proposed roadmap](superpowers/plans/2026-10-16-integrated-application-roadmap.md) targets an integrated alpha on October 30, a real-use decision on November 13, and a conditional broader beta on November 30. It maps all F1-F15 requirements without counting partial support as universal completion. Its first release includes both code verification and test-integrity checks under rules that coding harnesses cannot authorize changes to. The dates, foundation and detailed release boundaries remain proposed for owner review; the confirmed product requirements above remain the source of scope authority.

## Numbered decision register

Record IDs are stable. Confirmed owner decisions, proposed recommendations and unresolved questions must remain distinct. Each grilling answer is recorded before the next dependent question. Changes preserve the earlier decision and identify its replacement and reason. This register is the durable source of task decisions; external agent memory is not updated without a direct owner request.

### DEC-001: Approved integrated release roadmap

Date: October 16, 2026. Status: accepted by the owner before the dual-interface scope addition. Topic: product scope and delivery sequence.

Goal: release one integrated project application quickly, then broaden it through a conditional November 30 beta.

Constraints: one owner with AI coding agents, 30-40 hours per week, approximately 20% capacity reserved for checks, reviews, fixes, documentation and release qualification. macOS with Claude Code and Codex is the first qualified release scope.

Decision: the owner approved the five-release roadmap, its deadline tradeoffs and the qualification-first approach. Targets are v1 October 30, v2 November 6, v3 November 13, v4 November 20 and v5 November 30. V1 contains shared project state, native harness sessions, communication and protected independent code and test-integrity verification. The two-interface requirement in DEC-002 requires the effort and release boundaries to be reassessed without silently removing either interface.

Rejected alternatives: the previously withdrawn launch/worktree/handoff-only sequence remains rejected. Universal harness support, transparent live-process migration and instant cross-machine continuity are not November commitments.

Risks: dates depend on reusable foundations, permitted native integration, enforceable verification isolation and measured owner capacity. The directory currently has no Git repository or implementation; implementation needs repository and branch setup before source changes.

Acceptance criteria: preserve a complete integrated journey in the first release, map all F1-F15 requirements honestly, qualify the critical path, and use real repeated work to decide expansion.

Unresolved questions: exact interface parity per release, first pilot project, supported toolchains and stack remain for research and grilling. This approval does not authorize tool installation or dynamic orchestration.

### DEC-002: Rust engine with primary TUI and desktop application

Date: October 16, 2026. Status: accepted requirements; architecture and framework selection unresolved. Topic: engine, interfaces and platform targets.

Goal: deliver the same integrated project product through a terminal user interface and a desktop application. The owner primarily expects to use the TUI; the desktop application serves users who prefer graphical interaction.

Constraints: Rust is the requested main driver or engine. Both interfaces are required. Linux, macOS and Windows are architectural product targets; the earlier macOS-first qualification scope remains unless the owner changes it.

Decision: research both interfaces together, including how they can share engine behavior and project state. Tauri is the owner's desktop framework candidate, not an approved conclusion. Evaluate suitable Rust TUI frameworks and alternatives before selecting the stack. Do not treat the new interface requirement as authorization to duplicate execution, identity, configuration or verification behavior.

Rejected alternatives: a desktop-only or TUI-only final product does not satisfy the updated request. No specific framework or core/client topology has been rejected yet.

Risks: terminal embedding, focus, resize, interactive approval handling, native authentication, Windows PTYs, desktop webviews, process recovery, local IPC and protected verification differ across platforms. Cross-platform compilation alone does not establish functional support. Two interfaces add delivery and qualification work.

Acceptance criteria: a recommendation explains one coherent product through both interfaces, compares credible framework options using primary sources, identifies reusable implementations and their licenses, distinguishes supported platforms from unqualified targets, and assesses the November schedule impact.

Unresolved questions: in-process library versus persistent service, simultaneous clients, session lifetime after UI exit, TUI rendering approach, desktop terminal integration, operating-system support milestones and interface parity remain open.

### DEC-003: Research first, then detailed durable grilling

Date: October 16, 2026. Status: accepted workflow. Topic: architecture discovery and context durability.

Goal: reach detailed shared understanding before implementation and retain every decision across context loss.

Constraints: use deep research for framework and component reuse, then use the grill-me skill. Ask one dependency-aware question at a time, provide a recommendation, and answer factual questions from the available documents or evidence where possible.

Decision: first present the stack and reuse recommendation in chat, then begin the grilling session. Save substantive research in the project and record each decision with a stable item number, status, rationale, constraints, alternatives, risks, acceptance criteria and unresolved questions. Preserve superseded records and link replacements. Keep proposed designs separate from owner-approved choices.

Rejected alternatives: silently choosing Tauri because it was suggested, rebuilding available components without evaluation, treating research recommendations as approved implementation choices, and retaining decisions only in chat.

Risks: research establishes documented suitability, not a completed build, performance benchmark, native-auth qualification or security proof. Implementation must still follow the applicable engineering, testing and review gates.

Acceptance criteria: the research identifies recommended components and qualified alternatives, evidence and licensing limits, what must be built, targeted qualification checks, and the first grilling question. The decision register is updated before each next dependent question.

Unresolved questions: architecture decisions will be resolved one at a time after the research recommendation. No implementation or dependency installation begins during this research pass.

### DEC-004: Sessions continue after both interfaces exit

Date: October 16, 2026. Status: accepted, owner selected A for Q-001. Topic: session lifetime and ownership.

Goal: permit interface switching and ordinary UI exit without terminating running agent work.

Constraints: both the TUI and desktop connect to the same local application. This decision concerns quitting interfaces on the same computer; it does not approve cross-machine migration, automatic login startup or recovery across engine crash, logout or reboot.

Decision: running agents continue until explicitly stopped even after both interfaces exit. A user-local Rust engine owns session processes independently of the clients. Either interface can reconnect to the same live session without starting another inference process.

Rejected alternative: stopping the running work automatically when the last interface exits. Shared libraries may still hold domain code, but separate client-owned session lifetimes do not fulfill this accepted behavior.

Risks: background work consumes resources and may await approvals while no UI is connected. Engine lifecycle, discoverability, secure communication, pending-approval visibility and explicit stopping require further design.

Acceptance criteria: launch a supported session, exit both interfaces, reopen either interface and observe the same running session; explicit stopping terminates the intended session; reconnect does not duplicate work. Crash and reboot recovery remain separate acceptance questions.

Unresolved questions: concurrent-client access and input ownership, engine shutdown controls, approval handling without a connected UI, recovery and startup behavior.

### DEC-005: Selectable grilling questions

Date: October 16, 2026. Status: accepted owner preference. Topic: interview interaction.

Goal: let the owner answer quickly using selection controls instead of typing each response.

Constraints: ask one substantive question at a time, include a recommended answer and explain the relevant tradeoff. Keyboard behavior depends on the host selection control.

Decision: use the available structured question interface with a short list of selectable answers, recommended choice first. Keep a free-text response available for corrections or choices that do not fit. A displayed or preselected choice is not an answer until submitted.

Rejected alternative: requiring a typed reply for every question despite suitable selectable choices.

Risks: choices can hide nuance; preserve owner qualifications in the resulting record and ask a follow-up when needed.

Acceptance criteria: subsequent questions offer selectable choices, submission is recorded before the next dependent question, and unsubmitted choices remain pending.

Observed delivery limit: the question tool accepted Q-002, but the owner reported that no selection card appeared. The desired keyboard-selectable interaction remains recorded. Use visible inline lettered choices as a fallback until a reliable selector is available; do not require the owner to find an unverified card or repeatedly submit hidden duplicate forms. The owner answered the visible fallback with A.

Unresolved questions: reliable keyboard-selector availability in the host client; retain free-text flexibility throughout the interview.

### DEC-006: Shared viewing with explicit terminal control

Date: October 16, 2026. Status: accepted, owner selected A for Q-002. Topic: concurrent client access.

Goal: allow the TUI and desktop to observe the same live session without competing input or terminal dimensions.

Constraints: one engine-owned agent process per session, both interfaces on the same computer, and session lifetime as accepted in DEC-004. This concerns terminal input and resize control, not permission to alter trusted verification policy.

Decision: both interfaces can view the same live session concurrently. Exactly one controls typing and terminal resizing at a time. Control changes through an explicit Take control action; ordinary focus changes do not transfer control.

Rejected alternatives: automatically handing control to the most recently focused interface, and limiting a session to one attached viewer.

Risks: simultaneous takeover requests, stale controlling clients, unexpected disconnections, different screen dimensions and misleading ownership indicators require explicit handling.

Acceptance criteria: two clients see the same agent process and output; the engine accepts terminal input and resize only from the active controller; an explicit takeover transfers control without restarting the session; focus changes alone do not move control. Viewing from another interface does not create a second inference session.

Unresolved questions: takeover notification or consent, stale-control release, display behavior at different dimensions, and native versus custom session presentation. No specific IPC transport or framework is accepted by this decision.

Applicability after DEC-007: retain the accepted single-controller rule for interactive worker-session control where such views exist. Central conversation write access and the relationship between leader dispatch and manual worker takeover remain to be specified; this terminal decision does not silently resolve those policies.

### DEC-007: Central leader conversation coordinates worker hierarchies

Date: October 16, 2026. Status: accepted, owner selected B for Q-003 and clarified the intended interaction. Topic: primary conversation and orchestration.

Goal: let the owner express a request once and receive coordinated progress and results without manually typing into every harness or sub-agent.

Constraints: Rust remains the main engine, with both a primary TUI and a desktop interface. Projects retain shared global/project context and configuration, account choices, inter-harness communication and independent code/test-integrity verification. Leader or worker messages cannot authorize changes to trusted verification rules.

Decision: the primary interaction is a central CrazyAgent leader conversation presented through both interfaces. It takes the owner's input, coordinates delegation to worker harnesses and their sub-agents through supported capabilities, and brings progress and results back to the owner. The application must not require individual worker terminals to be the normal input path. A consistent custom conversation interface and central coordination are core product requirements, not optional visual polish.

Owner clarification: the owner described potentially hundreds of harnesses with many sub-agents in each as the reason manual interaction will not work. For illustration, 100 harnesses with 100 sub-agents each produce 10,000 workers. This expresses a large-hierarchy ambition; the owner has not yet set a first-release concurrency requirement, load benchmark, hierarchy depth or resource budget. Total tracked workers and simultaneous active workers need separate definitions.

Rejected alternative: Q-003 option A, using native Claude/Codex session interfaces as the primary first-release interaction. Native views can still be considered for inspection or debugging, but they do not replace the central conversation requirement.

Superseded recommendation: the stack research's native-PTY-first primary UI path was an unapproved speed optimization and is not the selected product design. Structured control, events, approvals and history now require renewed adapter qualification. Existing qualification and delivery estimates must be revisited for central leader orchestration and custom conversation behavior; they are not accepted estimates for this added scope.

Risks: incorrect delegation, runaway work creation, context growth, cost and resource exhaustion, partial failures, worker-originated prompt injection, lost or duplicated messages, and native sub-agent controls that are opaque or unsupported. A Rust execution engine and an AI planning leader have different authority; their exact boundary remains unresolved. The engine must enforce approved limits independently of model instructions.

Acceptance criteria: the owner submits a goal through either interface; the leader coordinates an authorized task flow across supported workers, collects status and results, and presents them in the shared conversation with applicable verification evidence. The owner can inspect which worker did what without having to manually route every message. Unsupported sub-agent control is explicit rather than simulated as a completed action. No implementation or large-scale qualification is claimed.

Unresolved questions: leader autonomy and plan approval, leader harness/model selection, scope per project, task decomposition and hierarchy depth, sub-agent visibility/control per adapter, concurrency and budgets, cancellation/recovery, central conversation input ownership, permissions, context summarization and first-release scale. The owner has requested central coordination; fully autonomous or unbounded spawning is not implicitly approved. Explain dynamic workflow tradeoffs and obtain explicit choices before implementation.

### DEC-008: Task- and resource-aware routing between leader and workers

Date: October 16, 2026. Status: confirmed feature requirement; detailed policy and approval behavior unresolved. Topic: model/harness routing and resource-aware scheduling.

Goal: match delegated work to appropriate available harness/model combinations without requiring the owner to select and monitor every worker manually.

Constraints: preserve the original F14 task-type routing requirement and retained project context. Supported harness/model/account combinations, project permissions and resource limits constrain dispatch. Native provider capabilities and actual observable availability must be qualified. Named model/provider assignments are owner examples and preferences, not confirmed model availability or comparative performance results.

Decision: include a logical routing and resource layer between leader planning and worker execution. It considers the type and requirements of a task, configured routing rules, supported capabilities and current resource availability when selecting an eligible harness/model. The owner specifically described feature development, debugging and testing/exploration having different preferred providers or models, and resource exhaustion changing which worker can receive work. The operator continues to use the central conversation.

Evidence: the original scope lists D14, F14 and section 5.11 for task-type routing with context/memory retained. The owner's current clarification adds resource awareness to the central leader architecture. This confirms scope rather than introducing an unrelated optimizer.

Recommendation, not yet an accepted implementation choice: place explicit routing rules and scheduling checks in the Rust engine. Let the leader describe a task and its needs; let the engine enforce allowed choices and limits. Start with inspectable rules and supported availability signals. More adaptive optimization can be considered separately after evidence exists.

Rejected interpretations: assuming that the leader's initial allocation must remain fixed despite changing availability, requiring the owner to manually reroute every worker, or treating model choice as the same decision as approval of a work plan. The owner has not approved an arbitrary routing fallback or unbounded automatic spending.

Risks: incorrect task classification, unavailable or stale quota data, capability mismatches, poor fallback quality, oscillating assignments, duplicate work and context loss on reassignment. Providers may not expose authoritative remaining quota; distinguish observed, estimated and unknown availability. A routing choice does not establish that a live native process or its hidden runtime state can migrate.

Acceptance criteria: a supported task matches a visible configured rule; dispatch selects an eligible supported combination; an observed exhausted or unavailable route is handled according to approved fallback/wait behavior; routing reasons are inspectable; project context remains scoped; the router cannot expand permissions, silently change billing mode or modify trusted verification rules. Exact policy, telemetry and recovery checks remain to be designed.

Unresolved questions: plan approval versus automatic dispatch, configuration scopes and precedence, task classification, routing priorities, quality evaluation, fallback and waiting behavior, authoritative versus estimated resource data, accounts, concurrency/budget limits, task reassignment/handoff and first-release depth. The owner said a combination of Q-004 A and B seemed appropriate but explicitly requested clarification; no final approval-mode choice is inferred.

### DEC-009: One initial plan approval followed by bounded automatic execution

Date: October 16, 2026. Status: confirmed by owner answer A to revised Q-004. Topic: authority for leader delegation and automatic routing.

Goal: give the owner one meaningful checkpoint for a substantial coding request while allowing the leader and router to coordinate workers without repeated allocation approvals.

Constraints: retain DEC-007's central conversation, DEC-008's task- and resource-aware routing and the independent verification boundary. Preparing the proposed plan may occur before its execution approval. Native tool approvals, destructive-action policy and trusted verification-policy changes remain separate controls. This decision does not establish exact limits, authorize arbitrary fallback choices or qualify an implementation.

Decision: present the initial work plan, scope and execution limits for owner approval before dispatching its implementation work. After that approval, automatically choose and reassign eligible workers and use already-approved fallback choices within the approved scope, routing rules, resource budget and permissions. Routine worker selection and approved fallback changes do not require separate owner approval. Scope expansion, additional spending beyond the approved budget and permission increases require a new approval. Missing allowed fallbacks and other exceptions must follow an explicit policy that remains to be decided.

Rejected alternatives: beginning substantive worker execution immediately under project defaults without the initial plan approval, treating every worker allocation as a new approval request, or interpreting approval as permission for unbounded delegation and spending. The owner's earlier tentative combination of A and B in DEC-008 is resolved by this explicit selection of revised A; that earlier exchange remains historical evidence.

Risks: a mistaken plan may still be approved; workers may produce scope drift; provider telemetry may be incomplete; reassignments may duplicate work or lose runtime context. The Rust execution authority must enforce concrete permissions and measurable limits independently of model instructions. Semantic scope checks require a defined review and escalation mechanism and cannot be assumed infallible.

Acceptance criteria: the owner can inspect and approve a proposed plan before implementation dispatch; execution records reference the approved plan and limits; supported routing and approved fallback changes proceed without per-worker approval; requests that exceed enforceable limits stop or await an authorized policy response; expanded scope, spending or permissions require a new approval; coding harnesses cannot use plan approval to alter trusted verification rules. Exact checks remain to be specified before implementation.

Unresolved questions: what powers the leader, approval presentation and plan revisions, cancellation, exception and waiting behavior, fallback eligibility, resource accounting, semantic scope review, exact concurrency and spending limits, native approvals, recovery and release-specific depth.

### DEC-010: Reuse RCLI with the ability to customize its source

Date: October 16, 2026. Status: owner-selected architecture direction; static qualification complete, runtime qualification pending. Topic: central leader harness reuse and source control.

Goal: obtain the existing behavior of a developed agent harness while retaining the ability to change context management, tools and other behavior when CrazyAgent requires it.

Constraints: the Rust engine remains CrazyAgent's main execution, routing and policy authority. Both clients retain the central conversation, and trusted verification remains independent. Availability of source does not establish that every hosted product capability is included, that all authentication methods are interchangeable, or that the harness is already a suitable embedded component. No installation, fetched code execution, production integration or broad rewrite is authorized by this documentation decision.

Decision: use Backboard RCLI as the intended reusable foundation for the central leader, with access to modify its open-source implementation when necessary. The owner's answer combines reuse of an existing harness with the customization available from its source. This replaces the original Q-005 framing that treated existing-harness reuse and ownership of custom behavior as mutually exclusive. The exact integration method, extension points and need for a maintained fork remain to be qualified before implementation.

Owner evidence: the owner supplied https://backboard.io/products/cli and explicitly selected RCLI because it is already implemented and its source can be modified. The official product page links https://github.com/Backboard-io/Backboard-R-CLI. Targeted research will distinguish repository facts from product claims and runtime qualification gaps.

Static qualification: inspected upstream revision [51bee17e02344de5fffd89523ce2b73d5042f216](https://github.com/Backboard-io/Backboard-R-CLI/commit/51bee17e02344de5fffd89523ce2b73d5042f216), manifest version 3.0.4. The repository contains an MIT license and a TypeScript/Bun implementation, with documented precompiled distribution. Its headless prompt/NDJSON interface and MCP support provide useful integration surfaces. A complete bidirectional host interface for live approvals, cancellation and reconnect has not been demonstrated. Direct provider keys and compatible local endpoints are documented without a Backboard login; reuse of Claude Code/Codex subscription sign-in is not established. The direct-provider client declares no hosted memory capability; Backboard's server-side memory and provider router are distinct from CrazyAgent's canonical memory and task/resource worker scheduler. Details and pinned sources are in the qualification report and its notes. No fetched code, installer, authentication flow or model request was executed.

Integration recommendation, not an additional accepted decision: keep the main execution/routing/policy engine in Rust and host the pinned RCLI implementation as a separate process. Try an adapter and narrowly scoped Rust orchestration tools first; maintain a small fork only for demonstrated missing behavior. Preserve explicit host-side approval and resource enforcement instead of enabling a global permission bypass. Do not rewrite the existing harness into Rust by default or assume its permission rules establish independent verification isolation.

Rejected alternatives: building a new leader reasoning loop from scratch by default, assuming reuse precludes source-level customization, or treating a vendor's entire platform as part of the CLI source. No decision has been made to require Backboard's hosted service, rewrite RCLI into Rust or replace native Claude Code/Codex workers with direct API calls.

Risks: maintenance of modifications, upstream drift, integration gaps, dependence on external services, credential and billing differences, duplicated memory/routing ownership and the harness's tool authority exceeding the approved task. These are qualification concerns, not reasons to erase the owner's selected direction.

Acceptance criteria: verify the official source revision and license; identify runtime and distribution dependencies; demonstrate a supported interface for the central conversation and orchestration tools; identify which context and provider capabilities work without Backboard; qualify credentials and billing; preserve Rust-enforced execution limits and verification isolation; document any required modifications and their maintenance cost. Static source inspection alone does not satisfy the demonstration criteria.

Unresolved questions: structured integration versus a fork, provider and backend policy, leader credentials and billing, context and memory ownership, tool confinement, cancellation and recovery, platform qualification, upstream update policy and revised delivery effort.

### DEC-011: Flexible supported model access across all agent roles

Date: October 16, 2026. Status: confirmed by the owner's clarification of Q-006; adapter support and configuration details remain unqualified. Topic: authentication, model access and role independence.

Goal: let the owner use supported subscription sign-in, provider API keys or compatible local endpoints throughout the application without restricting an access method to the leader or workers.

Constraints: actual harness/provider/model capabilities determine valid combinations. Subscription sign-in is not interchangeable with a provider API key, and local endpoints must implement a supported interface. Native sub-agents may inherit a parent session's model/account settings when their harness does not expose independent selection. Desired flexibility does not establish that every combination is available today. Keep DEC-009's approved permissions and resource limits and the independent verification boundary.

Decision: support flexible model-access methods across the leader, worker harnesses and sub-agents wherever the relevant integration exposes them. Subscription sign-in, API keys and compatible local endpoints are all valid product options; the role does not force a particular method. Permit a project to combine different supported methods. Authentication and model selection must remain within verified adapter capabilities rather than being represented as universally interchangeable.

Owner evidence: the owner explicitly expanded Q-006 beyond the leader, saying access could be subscription sign-in, an API key or a compatible local endpoint for any harness, model or sub-agent. This resolves the question by broadening the requirement rather than selecting one globally fixed method.

Rejected alternatives: limiting API/local access to the leader, forcing workers to use subscription sign-in, requiring subscription sign-in throughout the application, or assuming selection of RCLI requires Backboard-hosted access. The original leader-only A/B framing in Q-006 is superseded. The earlier RCLI assessment's worker-native-sign-in examples are allowed configurations, not exclusive requirements after this decision.

Risks: unsupported authentication combinations, accidental account or project credential crossover, unclear billing, inherited sub-agent identity, inconsistent capability exposure and silent switching between access methods. A configured credential does not by itself authorize a paid fallback or expanded spending. Those actions remain governed by the approved routing and budget policy.

Acceptance criteria: the capability view identifies supported access methods for each qualified integration; supported roles can select the appropriate configured account/endpoint without a hardcoded leader-versus-worker restriction; a project can use mixed supported methods; unsupported combinations and inherited sub-agent settings are explicit; routing honors selected identities and approved limits; no login token is copied into an incompatible integration to simulate support. Live checks and the first-release compatibility matrix remain to be specified.

Unresolved questions: credential/profile storage and scope, configuration precedence, model discovery, account selection, provider/backend defaults, per-sub-agent controls, routing eligibility, cross-method fallback, spending accounting, local-model qualification, initial compatibility coverage and leader implementation authority.

### DEC-012: Coordination-only leader by default with configurable implementation

Date: October 16, 2026. Status: confirmed by the owner's answer to Q-007. Topic: default leader permissions and application settings.

Goal: keep the default leader focused on coordinating workers while allowing an owner who wants direct leader implementation to enable it explicitly.

Constraints: preserve DEC-009's initial plan approval, scope and permission limits; DEC-011's supported authentication flexibility; and the independent verification boundary. Application configuration does not automatically expand the permissions of an already-approved task. Exact handling of changes during an active run remains unresolved, and any permission expansion still requires the applicable approval.

Decision: default to Q-007 option A. The leader plans, requests scoped inspection, delegates implementation, monitors work, obtains owner decisions and synthesizes results; worker harnesses perform code changes and implementation commands. Expose an application setting that allows the owner to choose option B, permitting the leader to implement directly as well as delegate within its approved authority. This is a configurable behavior, not a permanent restriction to coordination-only operation.

Owner evidence: the owner explicitly selected A by default and requested an application setting through which the user can choose A or B. No project-specific override, automatic mode switching or settings lifecycle behavior was selected by this answer.

Rejected alternatives: defaulting to direct leader implementation, making coordination-only mode mandatory with no setting, or treating implementation-enabled mode as permission to bypass task approval, resource limits or trusted verification.

Risks: accidental implementation permissions, confusing configured versus active-task authority, inconsistent behavior between clients and blurred responsibility for code changes. The selected mode and the authority used by the current task need to be visible and enforced independently of the leader's instructions.

Acceptance criteria: initial application settings select coordination-only; the owner can select implementation-enabled mode in settings; both clients reflect the same configured application mode; runtime dispatch checks the effective mode and approved task permissions; coordination-only prevents direct leader implementation; enabling implementation preserves scoped approvals and independent verification. The active-run transition policy must be resolved before implementing mode changes.

Unresolved questions: whether project overrides are needed, when a changed setting becomes effective, explicit reapproval for active-run permission changes, how the leader chooses direct implementation versus delegation in B, inspection tool limits, presentation of configured versus effective permissions and runtime qualification.

### DEC-013: Parallel work items with isolated worktrees and agent teams

Date: October 17, 2026. Status: confirmed by the owner's expanded answer to Q-008. Topic: primary project workflow, work isolation and delegation hierarchy.

Goal: provide one operating environment for agent-driven software development in which multiple independent pieces of work progress concurrently, each using the appropriate harnesses and agents without requiring the owner to coordinate every process manually.

Constraints: preserve the central leader interface, approved task scope and limits, task/resource routing, supported authentication flexibility, configurable leader implementation and independent verification. Worktrees separate Git working directories; they do not themselves provide process confinement or remove integration conflicts. Shared project context must remain scoped appropriately and cannot establish that workers never drift. The owner's example of ten tickets and many harnesses/agents illustrates the required topology, not a qualified concurrency limit or benchmark.

Decision: treat multiple concurrent work items within a project as core behavior. A work item can originate as a natural-language request, ticket, feature, bug or other task. Each work item uses isolated worktrees and can be delegated to multiple harnesses, with agents and native sub-agents beneath those harnesses where supported. All remain coordinated through the application and share the applicable project memory, rules, skills and other canonical context. Preserve distinct plans, approvals, ownership, status and results for each work item. Resource limits and real dependencies can delay individual operations; the product must not impose a one-active-goal-per-project model by default.

Owner evidence: the owner explained that worktrees are the reason independent tasks should run concurrently. Their example has ten tickets progressing in parallel, multiple harnesses per ticket and multiple agents/sub-agents within those harnesses, with common project context and visible progress. They explicitly stated that tickets are an example rather than the exclusive entry point.

Workspace authority: the owner's standing engineering rule requires a separate worktree for each parallel writing agent. Do not interpret a per-ticket workspace as permission for several writers to mutate one checkout. Exact per-work-item and per-worker worktree topology, branches, ownership, integration and cleanup remain to be designed. Native sub-agent controls that cannot be independently observed or assigned must be represented as adapter limitations rather than invented capabilities.

Rejected alternatives: serializing all independent owner requests within a project, limiting a work item to one harness, requiring the owner to manually communicate with every worker, making tickets mandatory for all work, or treating shared memory as proof that divergence and conflicting changes cannot occur. Q-008 option B is rejected; option A is confirmed and expanded by the owner.

Risks: multiplied resource consumption, dependency conflicts, changes that merge cleanly but conflict semantically, inconsistent or stale context, ambiguous ownership, hidden native sub-agent activity, duplicated work, orphaned processes/worktrees and integration bottlenecks. Isolation, context provenance, explicit ownership and independent checks must make failures detectable and recoverable; zero drift is not a validated guarantee.

Acceptance criteria: demonstrate two independent work items progressing concurrently in separate workspaces; associate supported harnesses and agents with the correct work item; preserve the applicable shared project context and identify what context/rules were supplied; attribute changes, status and results to the right work item; respect approved capacity and writer ownership; expose blocked dependencies and unsupported adapter controls; perform independent verification before accepting completed work. First-release load limits and the complete integration flow still require qualification.

Unresolved questions: work-item identity and lifecycle, dependency scheduling, per-writer worktree and branch topology, task decomposition, context versioning and updates, account/resource accounting, native sub-agent visibility, concurrency limits, cancellation/recovery, integration and merge authority, cleanup, release scope and measured performance.

### DEC-014: Agent-oriented work tracking with human visibility

Date: October 17, 2026. Status: confirmed product direction; full ticket system is an eventual extension with no newly accepted release date. Topic: work tracking, observability and integrated agent-focused ticket management.

Goal: let agents understand, execute and update structured work while humans can see progress, problems, review/testing stages and resource usage across the project hierarchy.

Constraints: support the DEC-013 work-item model even before a complete ticket product exists. Natural-language requests and other work sources remain valid. Preserve the distinction between worker-reported progress and independent verification evidence. Provider and native sub-agent telemetry may be incomplete; distinguish known, estimated and unavailable measurements rather than implying exact consumption is universally observable. The owner described the full ticket management system as an eventual addition, so this answer does not add it to a particular November milestone.

Decision: make work-item progress and resource visibility core parts of the integrated application. Humans must be able to inspect what is running, current stage, blockers/problems, review and testing progress, token consumption and resource consumption at supported levels of the work-item/harness/agent hierarchy. Add an agent-focused ticket management system as an eventual integrated capability: agents should be able to understand the work, act on it, update progress and coordinate with a human. Work records should drive delegation and connect to the corresponding execution state rather than serving only as human-facing descriptions.

Owner evidence: the owner wants ticket management designed primarily for agents, with human participation, and described tracking all ten parallel tickets through implementation, review and testing while exposing token/resource consumption. They framed the application as an operating system for software development using multiple harnesses and agents in one place.

Rejected alternatives: a collection of disconnected session tools with no common project/work progress view, a human-only ticket surface that agents cannot reliably use, treating an agent's stated completion as verified completion, or silently committing a full new ticket-management product to the existing delivery calendar.

Risks: stale or fabricated progress, conflicting state updates, inaccessible or overly dense hierarchy views, inaccurate aggregate usage, mismatched external and internal ticket states, duplicate work and premature expansion into a general-purpose issue tracker. The work schema, update authority and evidence requirements remain design questions.

Acceptance criteria: show supported work items and their assigned workers with current stage and problems; expose available usage with source and measurement limits; let humans inspect linked review/test outcomes and distinguish them from assertions; retain a consistent execution identity across views. Before releasing the full ticket system, qualify an agent creating/reading/updating a work record and a human following its execution through accepted completion. Exact schema, interface, synchronization and gates remain unselected.

Unresolved questions: work-state model, structured ticket contract, agent update permissions and concurrency, dependencies, acceptance/evidence fields, human review surfaces, telemetry collection and retention, external tracker integration, offline behavior, interface parity, release placement and build-versus-reuse research for the ticket system.

### DEC-015: Configurable manual, automatic and conditional merge policy

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-009. Status: confirmed by the owner's expanded answer. Topic: merge approval, risk/impact conditions and application settings.

Goal: retain human approval by default while letting the owner configure fully automatic merging or conditional approval based on the outputs of the required review, testing and other gates.

Constraints: every mode preserves required gates, source-bound evidence for the integration candidate and applicable repository controls. Automatic merge is a merge-approval policy, separate from DEC-009's initial work-plan approval and other tool/permission controls. Existing repository requirements can block an operation; the application must not fabricate external review approvals or bypass required checks. Coding workers cannot modify trusted verification rules or establish merge eligibility by asserting their own risk label. No external merge, push or publication is performed or authorized by this discovery record.

Decision: require human merge approval by default. Provide application settings for automatic merging without an application human merge-approval prompt once the candidate satisfies the required gates and approved merge authority. Also provide conditional policy: the owner defines conditions using assessed risk/impact or other supported gate outputs to determine whether a candidate merges automatically or requires human approval. The owner's example is automatic merging for low-impact PRs and human approval for medium- and high-impact PRs. That example demonstrates supported policy, not a universally mandated threshold. Rust enforces the selected policy against the relevant gate evidence rather than relying on a leader instruction alone.

Owner evidence: the owner selected human approval as the default, explicitly requested an automatic setting that does not ask for human merge approvals, and described configurable conditions based on PR risk/impact assessed after review, testing and other gates.

Mode semantics: manual mode waits for the authorized human approval after gates pass; automatic mode proceeds without that application merge-approval prompt when eligible; conditional mode applies the owner's configured conditions to choose the approval requirement. A failed or missing required gate prevents eligibility rather than becoming a reason to silently bypass a gate. The classification algorithm, condition precedence, unmatched/unknown classification behavior and active-run policy changes remain to be decided. Whether risk and impact are one field or separate dimensions is not established by the owner's examples.

Rejected alternatives: hardcoded human approval for every merge, automatic merge as the only mode, a fixed unchangeable low-risk threshold, requiring a human confirmation for each otherwise eligible merge in automatic mode, or treating automatic approval as permission to skip review/testing/verification. Original Q-009 A/B is expanded into configurable modes with conditional policy.

Risks: incorrect or manipulated classification, stale gate results, unsafe policy changes, conflicting conditions, confusing PR labels with trusted assessment, races between classification and target-branch changes, and the combined effect of parallel changes. Risk classification is evidence-based estimation rather than a proof that a change is safe. The policy decision must be inspectable and tied to the accepted source candidate and relevant configured policy.

Acceptance criteria: a fresh configuration waits for human merge approval; explicitly selected automatic mode merges an eligible candidate without that prompt; an owner-defined low-impact-auto/medium-high-manual policy produces the intended outcomes; required gate failure blocks every mode; changes to the candidate invalidate evidence as required by the verification contract; the UI shows classification, relevant gate results and the condition that selected the approval outcome; both clients reflect the configured behavior. Actual implementation, classifier quality and runtime race handling remain unqualified.

Unresolved questions: classification authority and evidence, risk versus impact dimensions, hard policy floors, condition fields and syntax, precedence and unmatched/unknown results, project overrides, policy edit authority, active-run changes, evidence expiry/invalidation, integration ordering, external PR-provider requirements and bounded recovery/retry behavior.

### DEC-016: Rule-based risk minimums with independent review escalation

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-010. Status: confirmed by owner selection A. Topic: classification authority for conditional merging.

Goal: make classification predictable at explicit policy boundaries while retaining independent assessment of semantic changes that fixed rules may miss.

Constraints: retain every required review/testing/verification gate and DEC-015's manual, automatic and conditional merge modes. Classification belongs to the accepted review/policy pipeline, not to the coding worker's chosen label. An AI review remains a potentially variable input; deterministic policy evaluation does not imply that repeating an AI review will reproduce its previous assessment.

Decision: use Q-010 A. Owner-controlled project rules establish minimum risk classifications. Independent review examines the candidate and can raise its classification but cannot downgrade below an applicable configured minimum. Rust combines the applicable rules, accepted review evidence and required gate results, then applies the configured merge policy. Classification and policy outcomes must be attributable to their inputs and rule versions. Exact category definitions, rule syntax, review configuration and risk-versus-impact dimensions remain unselected.

Owner evidence: the owner explicitly selected A because they want as much determinism as possible in the meta-harness layer, particularly its gates and rules, to improve confidence in code generation, testing and shipping.

Rejected alternatives: relying on an independent review agent's grade without configured classification floors, allowing a reviewer to lower the grade below an applicable project minimum, allowing coders to choose their own authoritative grade, or interpreting a grade as permission to skip another gate.

Risks: incomplete rule coverage, false escalation, stale review evidence, nondeterministic reviewer judgment, manipulation of review inputs, conflicting rules and false confidence in coarse categories. Rules bound the classification but do not prove the change is correct or secure.

Acceptance criteria: an applicable project minimum prevents a lower final classification; an accepted higher review assessment raises it; conditional merge behavior uses the resulting classification and the correct policy version; review and policy evidence identify the assessed candidate; coding-worker labels cannot replace accepted evidence; incomplete required gates prevent merge eligibility. Unmatched rules, unknown grades and contradictory evidence must have an explicit policy before implementation.

Unresolved questions: initial minimum rules, category definitions and dimensions, classifier evidence format, review-model/version capture, combining multiple reviewers, unknown/conflicting results, rules administration, candidate invalidation and measured classification quality.

### DEC-017: Deterministic control and reproducible acceptance evidence

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-010 clarification. Status: confirmed engineering and product requirement; exact guarantees and implementation remain to be qualified. Topic: determinism across the application control layer.

Goal: reduce the uncertainty of agent-driven development by making gates, rules and acceptance decisions reproducible, inspectable and evidence-backed.

Constraints: models, external providers, operating systems and tests may produce variable outcomes. Deterministic decision logic and correct software are different properties. A passing suite establishes evidence for the checks and requirements it covers, not a universal guarantee of defect-free code. Preserve independent verification, source-bound results and owner-controlled policy changes rather than turning the owner's confidence goal into an unsupported correctness claim.

Decision: prioritize deterministic enforcement in the Rust control layer, including explicit gates and rules, classification floors, approval requirements and merge eligibility. For the same captured candidate, versioned policy and complete accepted decision inputs, policy evaluation must produce the same decision. Time, target-branch revision and external signals used by a decision must be explicit recorded inputs; changing them creates a new evaluation context. Record model-review output as evidence rather than silently regenerating it while claiming an identical replay. New code, policy or accepted review evidence constitutes a new evaluation input. Required missing or failed evidence cannot be represented as a passing gate. Coding agents cannot weaken protected checks or rewrite acceptance rules to make their work pass.

Owner evidence: the owner expanded their classification choice into a general requirement that the application, meta-harness layer and software-development operating environment be as deterministic as possible, especially in its gates and rules. The desired outcome is trustworthy knowledge of what has been tested and accepted for shipping.

Implementation recommendations, not additional accepted stack choices: version policy and baseline inputs, bind receipts to immutable candidate snapshots, capture runner/environment identities, retain review outputs and decision explanations, and make policy replay independent of a fresh model call. Exact state-machine, persistence, isolation, event ordering and environment mechanisms belong to the later design and qualification stages.

Rejected alternatives: acceptance based solely on a model's statement of success, silent policy/check changes to obtain a pass, reusing stale evidence for changed code, or promising that this layer makes generation and all test environments inherently deterministic.

Risks: nondeterministic test environments and tools, insufficient requirements, inadequate checks, missing evidence, unsound test generation, unstable review assessments, policy-version races and reproducibly wrong decisions. Determinism strengthens repeatability and diagnosis but must be paired with validation of the criteria, checks and implementation.

Acceptance criteria: repeated policy evaluation over identical recorded inputs yields identical outcomes; the decision explains which rules, gates and accepted evidence produced it; changed inputs invalidate or replace dependent results explicitly; failed, incomplete, unsupported and stale checks cannot masquerade as accepted success; the UI exposes the verification performed and its scope. Hands-on qualification of these behaviors is still required.

Unresolved questions: trusted-check authorship and approval, acceptance-contract contents, evidence capture and authenticity, snapshot/environment granularity, event ordering, retry and flaky-test policy, external-state capture, cross-platform behavior, supported replay guarantees and limits of the initial qualified release.

### DEC-018: Automatic acceptance drafting with human approval and supplied baselines

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-011. Status: confirmed by the owner's combined A/B answer and project-setup clarification. Topic: acceptance criteria, verification baseline authorship and admission.

Goal: prepare a useful definition of success automatically while letting humans approve, amend or supply the project's trusted acceptance criteria and baseline.

Constraints: preserve independent verification and the separation between coding authority and trusted policy. AI-generated, manually entered and imported material is a draft until accepted through the human approval flow. Source availability, authorship or upload alone does not establish test quality or safe execution. Supported import formats, validation and admission checks remain to be designed. Project setup approval is distinct from merge approval; selecting automatic merge does not automatically approve a new verification baseline.

Decision: use AI-assisted drafting by default and support human authorship alongside it. When a project is created and its requirements are available, automatically start an appropriate verification-planning agent to understand the supplied requirements and propose acceptance criteria, the verification baseline and related checks. Present the proposal to the human. The human can approve it, edit it, add criteria/checks or upload their own material, including replacing or supplementing the draft. Save the approved result as the protected, versioned verification contract; the drafting agent and coding workers cannot approve it on the human's behalf or silently mutate the accepted authority.

Scope clarification: this extends the task-specific Q-011 recommendation to initial project setup. Project-level criteria and baseline establish the common foundation. Task-specific checks remain part of planning under that foundation, with acceptance criteria and the verification contract included in the existing initial work-plan approval. Freeze approved versions for the corresponding verification; the handling of later baseline changes for already-running work is the next open question. The actual distinction between criteria, executable checks and baseline policy still needs a concrete schema.

Owner evidence: the owner selected both paths with A as the default, described an agent automatically starting once the project and requirements exist, and explicitly requested presenting the draft for human approval while allowing humans to add to it or upload their own.

Rejected alternatives: requiring humans to author every trusted check from scratch, forbidding human edits/imports because AI drafting is the default, treating AI generation as automatic approval, allowing the coding harness to weaken accepted checks, or interpreting the combined answer as selection of only one authorship source.

Risks: misunderstood requirements, incomplete or vacuous generated checks, unsafe imported executable content, conflicting human and generated criteria, approval of a misleading summary, stale baselines, ambiguous project/task scope and excessive planning resource usage. Automatic drafting must use a configured supported model-access path and applicable resource authority; missing requirements or unavailable credentials must be explicit instead of being represented as a completed baseline.

Acceptance criteria: requirements supplied during project setup trigger a visible draft-preparation flow; a human can inspect the proposal, edit/add content and import supported material; generated, human-authored and imported content can contribute to the approved baseline; only explicit human acceptance admits the result to trusted authority; the saved version and applicable scope are identifiable; coding workers cannot change it through project files or shared memory; work-plan acceptance references the relevant approved project and task contracts. Draft quality, import safety, runtime isolation and the first supported formats still require qualification.

Unresolved questions: project requirements ingestion, missing-information dialogue, baseline/check schema, import formats and validation, quality review of generated checks, approval presentation and identity, planning-agent access and budgets, project/task composition, baseline revision handling, active-work migration and the future ticket-system link.

### DEC-019: Recorded baseline versions with current-contract acceptance

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-012. Status: confirmed owner's selection A. Topic: approved baseline changes during parallel work.

Goal: keep active work reproducible while ensuring accepted results satisfy the current applicable approved requirements.

Constraints: only human-approved baseline revisions change trusted verification authority under DEC-018. A recorded baseline version does not retain revoked permissions or override emergency-stop controls. Work-plan approval, verification-baseline approval and merge approval remain distinct authorities.

Decision: pin the approved verification-baseline version for each running work item. Newly planned work uses the updated approved baseline. Do not silently replace an active run's recorded checks when a revision is approved. Before accepting or merging its result, evaluate the result against the current applicable approved baseline and perform required revalidation or a controlled plan upgrade. Changed acceptance requirements can require rework or renewed plan approval before completion. Evidence from the recorded baseline remains historical evidence; it is not automatically valid for a revised contract.

Owner evidence: the owner answered A to Q-012, selecting recorded baseline versions for active work and current applicable baseline checks before acceptance.

Rejected alternatives: automatically pausing every affected work item on each baseline revision; silently changing an active run's verification contract; accepting a result solely because it passed an older baseline; treating a snapshot as permission to ignore revoked authority.

Risks: delayed discovery that new requirements invalidate ongoing work, incorrect applicability analysis, stale evidence reused across revisions, repeated revalidation and race conditions between baseline approval and acceptance. The implementation must define a consistent policy decision point rather than assume a previously current baseline remains current indefinitely.

Acceptance criteria: each active work item identifies its approved baseline version; approving a revision does not silently change that record; new plans reference the updated applicable version; acceptance identifies the current applicable approved contract and sufficient evidence for it; affected evidence is revalidated or the plan is upgraded before completion; permission revocations and emergency stops remain effective regardless of the recorded baseline. Exact evidence reuse and impact-detection rules still require design and qualification.

Unresolved questions: project/task baseline composition, revision applicability, impact detection, critical-update handling, evidence invalidation and reuse, controlled-upgrade presentation, approval identity, policy freshness at acceptance and migration of existing work records.

### DEC-020: Log-derived project and global memory agents

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-013. Status: confirmed owner's requested architecture; subsequently approved with the assessment in DEC-021 and publication timing resolved in DEC-022. Implementation remains unqualified. Topic: activity capture, asynchronous memory extraction and cross-project promotion.

Goal: preserve available evidence of agent activity for later analysis and continuously extract useful project knowledge and reusable global knowledge without relying only on manual memory entry.

Constraints: the owner asks for comprehensive capture of chats, exposed reasoning, model text, tool calls and other available activity, and asks whether unavailable reasoning can be obtained. This records the desired coverage, not proof that a provider or native harness exposes every internal token, reasoning process, tool result or sub-agent event. Actual supported capture, provider restrictions, privacy, secret handling, retention, storage bounds and failure behavior require qualification. Existing protected decisions, permissions, rules and verification baselines remain outside coding workers' memory-write authority.

Decision: use a project-level memory-agent role for each project and a separate global memory-agent role. The project role periodically or otherwise actively examines the project's recorded activity for useful knowledge. Workers and other sub-agents can also send candidate memories directly to that role, including an explicit human request to remember something. The project role maintains project memory and shows the human what changed, with controls to approve or keep, edit, reject or remove the proposed content. Project memory roles can nominate broadly reusable items to the global memory role. The global role curates global memory with corresponding human control. The owner requests an architectural assessment before finalizing the mechanism.

Approval boundary: the owner's description includes memory being updated and then presented for approval. It does not yet unambiguously decide whether new content is available to other workers before approval, whether project and global activation use different defaults, or how long unreviewed material persists. Do not interpret this answer as either automatic canonical publication or mandatory preapproval of every entry. Resolve that dependency in the next focused question.

Subsequent resolution: DEC-021 records explicit approval of the architecture assessment and human review inbox. DEC-022 resolves the activation uncertainty above: project knowledge is immediately retrievable as provisional after curation; global promotion requires prior human approval. Historical unresolved questions below retain the context of this initial answer and defer to those later records.

Owner evidence: the owner described comprehensive activity logs, one memory role per project, periodic log analysis, direct memory pings from workers, human requests to remember material, visible review/edit/delete controls, and nominations from project roles to a global memory role. The owner explicitly asked whether a better or faster architecture would avoid problems.

Rejected alternatives: treating this as only the previous A or B without preserving the hierarchy and ingestion paths; making workers' direct memory requests bypass curation; treating raw logs as already curated memory; silently making every project log available to the global role; promising access to unexposed provider internals.

Risks: sensitive data in logs or global memory, mistaken or malicious source material becoming trusted instructions, context contamination across projects, duplicate and conflicting entries, stale memories, excessive extraction cost, storage growth, incomplete event capture being represented as complete, and approvals becoming a bottleneck. The distinction between source evidence and a curated claim must remain visible.

Acceptance criteria: activity records identify their project and originating run/harness; a project's memory role can consume its own available records and receive explicit candidate submissions; proposed or published memory changes are human-visible with review/edit/removal controls; global candidates have explicit project provenance and a promotion path rather than unrestricted raw-log access; the system distinguishes requested logging coverage from observed supported coverage; protected authority cannot be changed through ordinary memory ingestion. Publication timing, execution cadence, retrieval semantics and concrete quality gates are not yet accepted technical choices.

Unresolved questions: preapproval versus publication followed by review; different project/global activation policies; log and reasoning visibility per harness; redaction and encryption; retention/export/deletion; periodic versus event-driven scheduling and budgets; definition of useful or global knowledge; source references, deduplication, conflicts and freshness; retrieval of provisional content; retraction of derived memories and effects on already-running work; backing storage and reuse candidates.

### DEC-021: Approve asynchronous memory architecture and human review inbox

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-013 follow-up assessment. Status: explicitly approved by the owner. Topic: activity capture, memory curation, provenance, project boundaries and human controls.

Goal: turn available agent activity into useful shared knowledge with bounded background work, clear evidence and human correction controls.

Constraints: Rust remains the main engine and owns storage access, scheduling and publication authority. Capture capabilities vary across providers, harness versions and native sub-agent paths. Preserve exposed activity with visible coverage gaps rather than promise access to every private internal reasoning token. Approval of this architecture does not establish measured throughput, runtime confinement, a database choice, exact extraction cadence or permission to send stored logs to an arbitrary provider.

Decision: adopt the architecture presented in the five-part assessment. Capture supported conversations, exposed thinking, tool calls/results, command output, approvals, usage and worker relationships. Separate activity records, candidate memories and retrievable versioned knowledge. Retain source references, scope, uncertainty and relevant revisions so a repeated claim is not silently treated as established fact. Each project memory agent is a scheduled logical role that can process new events incrementally after task completion, periodically or on direct worker/human requests. Batch related work and apply extraction budgets rather than continuously run one model for every project or repeatedly feed complete histories into workers.

Project/global boundary: project curators nominate reusable knowledge to a separate global curator. Global curation receives authorized nominations with necessary provenance rather than unrestricted project-log access. Keep project-specific facts scoped, generalize global candidates and remove private details before wider publication. Exact permission and redaction mechanisms remain to be designed.

Human controls and corrections: provide a review inbox with source inspection, version history and keep/confirm, edit and remove controls, including batch review. Corrections affect future retrieval, flag dependent memories and can notify active workers. Removing an entry cannot erase context already consumed by a running agent. Logs and extracted claims remain data; ordinary memory cannot change protected permissions, rules, human decisions or verification baselines. DEC-022 resolves the publication timing for this inbox.

Owner evidence: the owner explicitly approved the recommendation, architecture, system and human review inbox, then selected Q-014 A. This accepts the architecture explained in chat; detailed implementation proposals in the research report remain subject to their unresolved qualifications.

Rejected alternatives: making raw activity and curated knowledge the same record; granting every project memory curator unrestricted cross-project access; continuously running an inference process per project as a requirement; repeatedly analyzing complete project histories; treating model assertions as verification proof; assuming deletion retracts already consumed context.

Risks: incomplete logging, sensitive material surviving redaction, mistaken or malicious memories influencing work, cross-project leakage, stale evidence, excessive extraction cost and delayed correction propagation. Asynchronous execution removes curation from the coding turn's immediate workflow but still uses shared resources.

Acceptance criteria: supported activity is attributable to its project/run and capture gaps are visible; candidate and retrievable-memory states are distinct; incremental scheduled jobs and direct submissions feed the same scoped curation path; source-linked knowledge and uncertainty are visible; global candidates require an explicit promotion path; the review inbox supports keep/edit/remove and source history; corrections reach future retrieval and affected active work according to a defined policy; memory writes cannot alter protected authority. Performance, capture completeness and runtime isolation require implementation evidence before shipping claims.

Unresolved questions: permitted cloud/local processing of captured logs, exact storage and memory schema, redaction and encryption, retention/deletion/export, scheduling and extraction budgets, capture/reconnect/sub-agent coverage per adapter, deduplication and freshness, retrieval ranking, correction delivery and impact handling for active work, configuration overrides and revised delivery effort.

### DEC-022: Immediate provisional project memory and approved global promotion

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-014. Status: confirmed owner's explicit selection A. Topic: memory activation timing and human review authority.

Goal: let workers benefit promptly from curated project discoveries while controlling the wider impact of global knowledge.

Constraints: immediate availability applies after project curation, not to arbitrary raw logs or unprocessed worker submissions. Provisional status, source references and uncertainty remain visible. Memory is not verification proof or permission to change protected authority. Project/global access controls and redaction still apply.

Decision: make ordinary curated project knowledge immediately retrievable by authorized project workers as provisional memory. Present updates in the human review inbox with keep/confirm, edit and remove controls. Hold proposed global promotions until explicit human approval, then make the approved content available within its authorized global scope. Selecting this policy does not approve future global candidates in advance.

Owner evidence: the owner selected A and explicitly stated that project knowledge should become immediately available while global promotion waits for approval.

Rejected alternatives: requiring prior human approval for every ordinary project-memory addition; publishing global candidates automatically; treating provisional project claims as confirmed merely because another worker retrieved them; interpreting a general architecture approval as approval of every future memory entry.

Risks: a mistaken project memory can influence workers before review; human review can lag; a rejected entry may already have shaped a worker's context; global approvals may become a queue. The versioned review and correction behavior in DEC-021 remains necessary.

Acceptance criteria: a successfully curated project entry is retrievable with a provisional label before human review; the inbox identifies the change and supports confirmation, editing and removal; an unapproved global candidate is excluded from global retrieval; global approval identifies the content/version being promoted; rejecting a candidate prevents that promotion; protected rules, permissions, decisions and verification baselines remain unchanged by ordinary memory publication.

Unresolved questions: inbox batching and notifications, approval identity and content revision handling, whether any future configuration can alter activation modes, review aging, stale-entry lifecycle, derived-memory invalidation and propagation to already-running workers.

### DEC-023: Approved cloud or local destinations for memory processing

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-015. Status: confirmed owner's selection A. Topic: memory-model processing permission and destination changes.

Goal: support automatic background curation with the owner's selected cloud or local models while controlling where captured project activity is processed.

Constraints: supported subscription sign-in, API keys and local endpoints remain available according to DEC-011. Authentication does not itself grant permission to process stored activity. Existing approved scope, resource authority, project isolation, minimization and redaction still apply. Approval of a model destination does not establish provider retention guarantees or authorize a new spending path.

Decision: allow local or cloud models for memory curation under an explicit project processing policy. During project setup, the human approves the permitted provider/account destinations. Curators may then process authorized, minimized and redacted excerpts automatically within that policy and the existing resource authority. Changing to an unapproved destination requires human approval; configuring another credential alone does not extend the policy. The global memory role receives only authorized nominations and needs an approved processing destination for that material. This decision authorizes the product behavior; it does not initiate any provider call or approve any future global memory content.

Owner evidence: the owner answered A to Q-015, selecting the configured local/cloud memory model with processing destinations approved during project setup.

Rejected alternatives: requiring local-only curation as the default; treating any available credential as permission to send logs to its provider; silently rerouting excerpts to an unapproved destination; granting the global curator unrestricted project-log access.

Risks: misleading consent presentation, unexpected data transfer through fallback routing, an account identity changing beneath an approved profile, imperfect redaction, provider-specific storage behavior and confusion between processing permission and permission to publish global memory. Exact identity and policy enforcement require implementation evidence.

Acceptance criteria: project setup exposes and records the allowed memory-processing destinations; curation can use an approved local or cloud destination without approval on every batch; unapproved destinations are blocked pending approval; destination policy is enforced on retries and fallbacks; excerpts remain scoped, minimized and redacted; the global role processes only authorized nominations through an approved destination; global publication still requires DEC-022 approval; configured credentials and actual processing authorization are distinguishable to the human.

Unresolved questions: policy schema and inherited defaults, approval identity, provider/account profile changes, revocation during active jobs, consent presentation, fallback behavior when all approved destinations are unavailable, provider-specific integration capabilities and data handling, retention/deletion/export, encryption and extraction budgets.

### DEC-024: Retain captured activity until explicit deletion by default

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-016. Status: confirmed owner's selection A. Topic: default activity-log retention and optional expiry.

Goal: preserve older conversations and source evidence for later debugging, research and memory inspection under the human's control.

Constraints: this applies to app-managed captured activity and does not establish third-party provider retention or deletion behavior. Capture scope, visible coverage gaps, redaction and access controls remain unchanged. Curated memory, decision records and required verification evidence have separate retention rules. Retaining logs by default does not imply unlimited physical storage.

Decision: keep captured activity logs until the human explicitly deletes them by default. Show storage usage and support an optional configurable automatic-expiry policy. Automatic expiry is not enabled merely by project creation or routine memory curation. Older captured conversations and available source evidence remain eligible for inspection while retained. Exact expiry scope, duration settings, archiving, storage limits and disk-pressure behavior require further design.

Owner evidence: the owner answered A to Q-016, choosing retention until explicit deletion with configurable optional expiry.

Rejected alternatives: expiring ordinary logs after 30 days by default; silently deleting older logs to satisfy an unstated age limit; treating a log-retention choice as a universal retention policy for all memory, decisions, verification evidence or provider-side copies.

Risks: storage growth, longer retention of sensitive material, inadequate visibility of disk usage, source references becoming unavailable after explicit deletion or enabled expiry, and confusion between local deletion and deletion from exports, backups or remote services.

Acceptance criteria: new projects do not automatically age out captured activity; retained logs remain inspectable until explicit deletion or an enabled expiry policy applies; storage usage is visible; expiry requires an explicit configuration choice and exposes its effect; provenance identifies an underlying source that has expired or been deleted; ordinary log deletion does not silently apply the same lifecycle to curated memory, decision records or required verification evidence. No actual cleanup is authorized by this design decision.

Unresolved questions: storage budgets and warning thresholds, behavior when logging cannot continue, expiry defaults after opt-in, applying expiry to historical logs, pinning and archival format, export/backup/provider-side copies, project deletion, derived-memory effects and evidence retention rules.

### DEC-025: Support pause and continue modes for logging failures

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-017. Status: confirmed owner's combined answer; the default is subsequently resolved in DEC-026. Topic: work continuity when durable activity capture fails.

Goal: support both preservation of activity history and continued development with explicitly incomplete logs according to the selected behavior.

Constraints: these are alternative operating modes for affected work, not a promise that the same worker can simultaneously pause and continue. Neither mode silently deletes retained history, enables expiry, hides capture gaps, bypasses required verification evidence or terminates unrelated processes. Native worker pause/checkpoint support remains unqualified.

Decision: support both Q-017 behaviors. The pause path stops new affected dispatch and requests a safe checkpoint, pause or controlled stop of active owned workers, showing actual worker state and recovery options. The continue path allows otherwise runnable work to proceed with a persistent logging-degraded indication and explicit gaps. The owner's answer establishes both capabilities but does not select the default, configuration scope or per-incident selection flow. Resolve the default in Q-018 before treating either as automatic product behavior.

Subsequent resolution: DEC-026 records Q-018 A, selecting pause as the default with an explicit Continue with logging gaps action and a configurable preferred policy. Configuration inheritance, project overrides and incident-choice lifetime remain open.

Owner evidence: the owner answered "both" to Q-017 after seeing the pause and continue-with-gaps options.

Rejected alternatives: discarding either requested mode; assuming that "both" selected pause by default or continue by default; claiming a pause request proves a worker stopped; representing missing activity as a complete record.

Risks: unclear active policy, work continuing while the human expects a pause, unwanted interruptions, native workers remaining active after a pause request, lost activity during mode transitions and incorrect gate claims based on missing evidence.

Acceptance criteria: both modes are representable and visibly distinguishable; the pause path differentiates requested and confirmed worker state; the continue path identifies degraded capture and unavailable intervals; retained history and independent verification requirements remain protected; the product exposes the selected behavior and recovery status. Actual worker-control guarantees and storage-failure recovery require implementation evidence.

Unresolved questions: default mode, settings scope and overrides, per-incident switching, approval for continuing with gaps, warning thresholds, affected-project detection, bounded buffering, native pause support, recovery/resume behavior and handling a failure of the control store itself.

### DEC-026: Pause by default on logging failure with explicit continue override

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-018. Status: confirmed owner's selection A. Topic: logging-failure default, incident override and preferred mode.

Goal: protect the activity record by default while preserving the owner's ability to choose continued work with explicit gaps.

Constraints: both modes remain available under DEC-025. Pause requests and confirmed worker state must be distinguished because native harnesses and already-running tools may not stop immediately. Affect only the relevant owned work. Neither mode silently deletes retained logs, enables expiry or bypasses required verification evidence.

Decision: default to the pause path when durable activity capture fails. Stop new affected dispatch, request a safe pause/checkpoint from active owned workers and show the logging problem and actual worker states. Offer an explicit Continue with logging gaps action. Allow the preferred policy to be changed to continue mode in application settings. Continuing exposes a persistent degraded-capture state and missing intervals rather than claiming complete history. Exact per-project overrides and the duration of a one-time incident choice remain open.

Owner evidence: after selecting both logging-failure modes in Q-017, the owner answered A to Q-018, selecting pause by default with an explicit continue action and a configurable preferred policy.

Rejected alternatives: continuing by default; removing the continue capability; treating a pause request as confirmed suspension; silently treating one incident override as a permanent settings change; claiming that continued work has complete logs.

Risks: unexpected interruptions, an active tool proceeding after a pause request, configuration or incident overrides being unclear, unrecoverable activity gaps, and unsafe or surprising recovery/resume behavior.

Acceptance criteria: a project with default settings uses the pause path on a logging failure; new affected dispatch stops; worker state distinguishes a requested pause from a confirmed one; the explicit continue action selects the degraded-capture path with visible gaps; settings can prefer continue mode; retained history and required verification remain protected. Native control behavior, state durability during a store failure and recovery transitions need qualification.

Unresolved questions: warning thresholds, affected scope, settings inheritance and project overrides, one-time override lifetime, mode changes during failure, pause/checkpoint support per adapter, bounded buffering, failure of the control store, recovery/resume authority and replay of partially recorded events.

### DEC-027: Prefer approved available alternatives during routing

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-019. Status: confirmed owner's selection A. Topic: preferred-target availability, fallback and required-target constraints.

Goal: keep eligible work moving through approved capacity without relaxing the task's requirements or execution authority.

Constraints: preserve DEC-008 task/resource routing and DEC-009 bounded execution after initial approval. Eligible alternatives must satisfy the approved plan, routing policy, capability requirements, permissions, applicable processing destinations and budget. Explicit required-target rules override ordinary preferences. Provider/model examples are not verified comparative performance rankings, and actual availability must be qualified.

Decision: treat a task's routing target as a preference by default. When that preferred harness/model is unavailable, dispatch to a suitable alternative already permitted by the approved plan and routing policy. Record the selected target and the reason for the change. A rule marking a target as required prevents fallback outside that requirement. The router cannot invent an unapproved or unsuitable fallback when no eligible target exists. This selects the default routing behavior; it does not qualify live quota detection, native-session transfer or worktree handoff.

Owner evidence: the owner answered A to Q-019, selecting suitable already-approved alternatives when the preferred target is unavailable.

Rejected alternatives: waiting for the preferred target by default despite suitable approved alternatives; treating preferences as permission to violate a required-target rule; using available credentials as authority for arbitrary fallback; expanding spend or permissions without approval.

Risks: stale or estimated availability, incorrect capability matching, unexpected cost or quality differences between eligible targets, context loss during reassignment, duplicate work and unexplained target changes. Mid-task transitions need explicit ownership and evidence rules rather than assuming an initial dispatch is equivalent to a safe handoff.

Acceptance criteria: preferred-target unavailability can select a suitable approved alternative without a new routine selection prompt; the selected target and reason are inspectable; required-target rules and all hard constraints remain binding; no eligible alternative produces a visible queued/blocked state according to the eventual waiting policy rather than an invented fallback; scope, budget and permission expansion still require approval. Availability and handoff behavior need runtime qualification.

Unresolved questions: initial candidate rankings, policy revision authority, task classification, authoritative versus estimated availability, account ordering, queue fairness, deadlines, all-targets-unavailable behavior, reassignment checkpoints, worktree ownership transfer, retry policy and applicable override scopes.

### DEC-028: Human approval for persistent routing-preference changes

Date: October 17, 2026. Task: CrazyAgent architecture grilling, Q-020. Status: confirmed owner's selection A. Topic: routing-policy revision authority and recommendations from observed outcomes.

Goal: improve routing with inspectable evidence while keeping persistent preferences under human control.

Constraints: DEC-027 availability-aware selection and already-approved fallbacks remain automatic. Ranking approval does not expand destinations, budgets, permissions or verification authority, and cannot override required-target rules. Observed latency, cost or verification outcomes are evidence for a recommendation rather than proof that a model caused a better result.

Decision: execute versioned, human-approved routing preferences. The system may analyze recorded outcomes and propose a different persistent ranking, but a human must approve the policy change before it affects future dispatch. Do not silently update persistent preferences from observed outcomes or treat ordinary memory publication as approval of a routing change. Record the accepted policy version and relevant decision inputs so routing can be explained under DEC-017. This does not require a prompt for every worker selection or already-approved fallback.

Owner evidence: the owner answered A to Q-020, choosing recommendations with human approval rather than automatic persistent ranking revisions.

Rejected alternatives: automatically revising persistent rankings within previously approved candidate lists; interpreting fallback approval as permission for adaptive policy changes; requiring fresh approval for every routine dispatch under an unchanged policy; treating noisy performance observations as a definitive comparative ranking.

Risks: stale preferences, noisy or biased recommendations, misleading summaries, policy versions changing while plans are queued, ambiguous effect on active work, and confusing an approved routing policy with approval of additional cost or data access.

Acceptance criteria: routing uses an identifiable approved preference version; proposed changes remain inactive until human approval; accepted revisions and their supporting observations are inspectable; routine eligible-worker selection and approved fallback still run automatically; changing rankings cannot bypass required targets or hard execution constraints; the system does not quietly promote a recommendation from logs or shared memory into active routing policy.

Unresolved questions: proposal presentation and approval identity, initial rankings, evaluation metrics and windows, minimum evidence, update cadence, queued/active-plan version applicability, rollback behavior, conflict handling and settings inheritance.

### DEC-029: Visible development tests with protected verification holdouts

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-021.

Status: accepted requirement; holdout execution, feedback and confinement remain unimplemented and unqualified.

Topic: what coding workers can inspect while independent verification assesses their changes.

Goal: keep requirements and ordinary test-first development usable while checking behavior against additional cases that coding workers have not inspected.

Constraints: holdouts assess the approved acceptance contract rather than introduce undisclosed requirements. Humans retain access to inspect and approve protected checks. Coding workers cannot approve, edit or weaken the trusted baseline. Retain DEC-018's human admission, DEC-019's current-contract acceptance requirement and independent checks of both code and test integrity. Hidden cases do not establish universal correctness or prove their own quality.

Decision: use Q-021 A, the hybrid visibility model. Coding workers can read approved acceptance criteria and ordinary development tests. The independent verifier additionally runs protected holdout cases whose exact implementation and fixtures are withheld from coding workers. Return actionable failure feedback without disclosing the whole holdout set. Humans can inspect and approve those cases. Protect the visibility boundary across files, tools, logs and memory as well as direct UI access. This records the required boundary, not a claim that native harnesses or worktrees already enforce it.

Owner evidence: the owner answered "A" to Q-021, selecting hybrid verification over exposing all approved verification source and fixtures to coding workers.

Rejected alternatives: making every protected check and fixture readable to coding workers as the default; hiding the acceptance criteria or ordinary development tests; using holdouts to enforce requirements that were never admitted to the approved contract; treating read-only worktrees or withheld UI elements alone as proven isolation.

Risks: repeated feedback can reveal protected cases indirectly; project logs, tool output or memory can leak fixtures; code under test may inspect its execution environment; low-quality holdouts can reject valid behavior or miss defects. Protection and useful debugging feedback must be qualified together. No mechanism for eliminating these risks has been selected.

Acceptance criteria: coding workers can inspect the applicable approved requirements and use ordinary development tests; humans can inspect the protected holdouts and admit them through the baseline approval flow; a qualified worker cannot retrieve protected implementation or fixtures through authorized files, tools, logs or memory; the independent verifier executes applicable approved cases and returns actionable feedback; evidence identifies the candidate and applicable contract/check versions without exposing protected material to workers; failed, missing, stale or unsupported required checks cannot become a passing result. Runtime evidence is required before claiming these criteria work.

Unresolved questions: holdout generation and admission presentation, storage and execution isolation, code-under-test access, failure-feedback detail and repeated-attempt policy, leakage detection and response, supported languages and tools, verification depth and budgets, and cross-platform qualification.

### DEC-030: Mandatory verification floor with deterministic risk-based depth

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-022.

Status: accepted requirement; concrete suites, selection rules and execution costs remain unqualified.

Topic: selecting verification depth for each candidate change.

Goal: require meaningful code and test-integrity evidence for every change while applying additional verification where approved rules require it.

Constraints: the applicable approved acceptance contract determines the mandatory checks. Preserve independent verification, DEC-029's holdout boundary, DEC-016's risk minimums and independent escalation, and DEC-017's reproducible policy evaluation. Risk-based selection cannot silently weaken a required baseline or treat an unsupported or unexecuted check as success. Passing configured checks does not prove general correctness.

Decision: use Q-022 A. Every change runs the mandatory verification floor, covering the required code-correctness and test-integrity checks in its approved contract. Explicit, versioned, owner-approved risk rules select additional required checks for relevant risk categories or affected areas. Possible additional checks include expanded mutation testing and adversarial scenarios where supported; this decision does not select tools or claim those capabilities already work. Humans can require the full applicable suite for an individual change. Routine check selection within approved policy is automatic. Faster preliminary feedback is not final acceptance, and every applicable required gate must pass before acceptance or merge eligibility.

Owner evidence: the owner answered "A" to Q-022, selecting a mandatory floor plus deeper verification under deterministic risk rules rather than every configured deep check for every change.

Rejected alternatives: requiring every configured deep check for every change regardless of risk as the default; allowing workers to choose which required checks to omit; lowering verification requirements because of timeouts, missing tooling, exhausted budgets or a desired delivery date; treating partial feedback as completed verification.

Risks: an inadequate floor or incomplete risk rules can miss defects; incorrect change classification can omit relevant additional checks; mutation and adversarial checks may be costly or unavailable for a language or harness; changing candidate, classification or contract can make previous evidence insufficient. Required-check selection and its assumptions must remain visible and attributable.

Acceptance criteria: each candidate identifies the applicable contract, risk inputs, rule versions and required-check set; all candidates receive the approved floor; matching risk rules add their required checks; raised classification and changed inputs trigger reevaluation of required checks and dependent evidence; the human full-suite request adds all configured applicable checks without removing mandatory ones; failed, missing, stale, timed-out or unsupported required evidence cannot yield acceptance or merge eligibility; the UI distinguishes preliminary feedback from completed verification. Qualification must exercise both ordinary and escalated changes before claiming these behaviors work.

Unresolved questions: initial verification floor, concrete risk categories and matching rules, treatment of unknown or conflicting classification, check/tool adapters, mutation and adversarial coverage, execution budgets and measured runtime, retry and flaky-test policy, full-suite request presentation, and cross-platform support.

### DEC-031: Bounded automatic retries for temporary verification infrastructure failures

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-023.

Status: accepted requirement; eligible failure rules, retry limits and runtime classification remain unimplemented and unqualified.

Topic: retrying verification of an unchanged candidate after a temporary infrastructure failure.

Goal: recover from eligible temporary service failures without requiring a human action for every attempt or concealing evidence of defects.

Constraints: use explicit, approved failure-classification and retry rules. Preserve DEC-017's recorded inputs and evidence validity, DEC-018's baseline authority, DEC-029's holdout protection and DEC-030's required-check set. Retrying cannot expand approved spend, permissions or processing destinations. A changed candidate is a new verification context, not a retry of identical code.

Decision: use Q-023 A. The independent verifier may automatically retry an unchanged candidate when approved rules identify a temporary infrastructure failure. Retries are bounded; every attempt retains its result, failure classification and supporting evidence. A completed rerun can satisfy a gate only when the approved policy permits it and the evidence remains valid for the candidate, contract and required checks. Assertion failures, unknown causes and conflicting pass/fail behavior remain unresolved rather than being rerun until a green result replaces their history. Reaching the retry cap produces a visible blocked result. Ordinary authorized code repair continues under the approved work plan; changed code receives fresh applicable verification. No numeric limit, delay or quarantine mechanism is selected by this decision.

Owner evidence: the owner answered "A" to Q-023, accepting bounded policy-controlled automatic retries rather than requiring a human request for every rerun after a temporary infrastructure failure.

Rejected alternatives: a human action for every eligible infrastructure retry as the default; unlimited reruns until green; treating a later success as permission to conceal earlier failures; allowing coding workers to relabel defects as infrastructure; using a retry to skip checks or weaken the acceptance contract.

Risks: classification may mistake a product defect for a service failure; repeated attempts consume resources and may reveal holdout information through feedback; retrying against changed external state can make evidence incomparable; a retry cap can leave otherwise recoverable work blocked. Classification provenance, current evidence validity and retained attempt history require qualification.

Acceptance criteria: an eligible infrastructure failure can trigger an automatic retry within approved limits; an assertion failure or unknown cause does not enter a retry-to-success path; conflicting pass/fail evidence remains visibly unresolved; every attempt and classification is attributable to the candidate and policy; reaching the cap stops further automatic retries and exposes the blocker; a passing rerun satisfies a required gate only under the applicable approved retry policy; changed code receives a new verification context; scope, spend, access and destination limits continue to apply. Runtime tests must demonstrate these behaviors before they are reported as implemented.

Unresolved questions: eligible infrastructure failures and evidence, conservative handling of ambiguous failures, retry caps and delays, transient-service readiness checks, resolution of conflicting results and known flaky tests, environment identity, deduplication and scheduling, human presentation, and cross-platform/tool qualification.

### DEC-032: Editable canonical files for shared rules, skills and configuration

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-024.

Status: accepted requirement; formats, paths, adapters and file-change admission remain unimplemented and unqualified.

Topic: authoring and storing shared rules, skill definitions and shareable configuration.

Goal: provide one inspectable source of truth that both interfaces and supported harness adapters use, with project assets suitable for Git review.

Constraints: this decision concerns authored shared assets, not automatic publication of logs, credentials or curated memory into Git. Preserve existing approval requirements for protected verification, routing and permission policy. File contents, Git commits and imports are not authority to weaken those policies. Native skills, plugins and permission systems are not assumed interchangeable, and executable content retains its compatibility and access-control requirements.

Decision: use Q-024 A. Editable canonical files are the source of truth for shared rules, skill definitions and shareable configuration. Shareable project assets live in a project location suitable for Git review; user-global assets use a separate user-controlled location. The TUI and desktop edit the same canonical content. The Rust engine validates and indexes the content and produces qualified harness-specific representations. The application may keep indexes, runtime state and revision metadata in a database, but it must not create a competing independent authoring source. Exact formats, paths, native-file coexistence and revision-application rules are not selected here.

Owner evidence: the owner answered "A" to Q-024, selecting editable canonical files over app-managed canonical records with explicit file import/export.

Rejected alternatives: app-managed records as the only canonical authoring source; treating generated harness files as independent competing sources; assuming a file edit or Git commit automatically approves protected policy changes; storing secrets or publishing captured activity into project Git merely because configuration is file-based.

Risks: simultaneous external and UI edits can conflict; malformed content can interrupt configuration loading; native formats may lose meaning during conversion; project files can contain hostile executable content or proposed policy changes; stale generated representations can cause workers to use different revisions. File admission, provenance and compatibility require explicit handling and qualification.

Acceptance criteria: the same canonical content is inspectable outside CrazyAgent and editable through both interfaces; project and user-global locations are distinct; the engine validates content before using it and identifies the effective source revision; qualified adapters produce attributable harness representations without creating another authoring source; malformed or conflicting changes remain visible and do not silently replace accepted policy; protected policy changes still follow their approval flows; logs, memory and credentials retain their separate storage and access rules. Runtime evidence must demonstrate these behaviors before implementation is claimed.

Unresolved questions: file formats and locations, global/project inheritance and ordinary-setting precedence, external-edit admission and conflict recovery, active-worker version application, native-file coexistence, adapter fidelity, executable plugin trust and installation, sensitive-field detection, and cross-platform path handling.

### DEC-033: Linked global defaults with explicit project overrides

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-025.

Status: accepted requirement; field-level inheritance, conflict handling and active-worker revision application remain unimplemented and unqualified.

Topic: how global shared-asset updates reach individual projects.

Goal: reuse global rules, skill definitions and shareable defaults without maintaining independent copies in every project, while preserving deliberate project differences.

Constraints: this is inheritance for ordinary authored assets and overridable defaults, not a means to weaken protected authority. Project overrides cannot bypass mandatory owner constraints, verification baselines, routing approvals or access controls. Preserve DEC-032's canonical files and qualified native adapters. Existing project/global memory curation and publication rules remain unchanged. This decision does not authorize mid-task configuration replacement or determine task-level override semantics.

Decision: use Q-025 A. Projects remain linked to applicable accepted global defaults. When resolving configuration for new work, inherit accepted global rules, skill definitions and shareable settings, then apply explicit project overrides for ordinary overridable settings. Accepted global updates reach future work where the project has not overridden the relevant value. Show effective values and their source, including visible project overrides and relevant revisions. Independent copies requiring explicit synchronization are not the default inheritance model. Exact eligible fields, merge behavior and active-worker adoption remain unresolved.

Owner evidence: the owner answered "A" to Q-025, selecting linked global defaults with explicit project overrides over independent project copies synchronized manually.

Rejected alternatives: creating independent project copies of all selected global defaults as the default; hiding a project override or its provenance; silently treating ordinary precedence as permission to weaken protected policy; assuming every installed native feature is compatible merely because a global definition exists.

Risks: one global change can affect future work in several projects; an old project override can hide an intended global improvement; asset-name collisions and incompatible native features can produce ambiguous behavior; external file edits can race with resolution. Scope, revisions, effective values and unresolved conflicts need clear presentation and qualification.

Acceptance criteria: new work in a linked project uses applicable accepted global values where no project override exists; an explicit project override wins for a supported ordinary overridable value and remains visible; a later accepted global update reaches future work in non-overridden projects without copying settings manually; effective values identify their global or project source and revision; protected constraints cannot be weakened by this resolution; unsupported or conflicting values remain explicit rather than silently disappearing; existing memory approval and publication behavior is preserved. Runtime evidence must qualify inheritance and override behavior before claiming implementation.

Unresolved questions: inheritable field catalogue, scalar/list/asset merge rules, name collisions, deletion and disabling semantics, external-edit admission, per-project opt-outs or pinned global versions, task-level overrides, active-worker adoption, and cross-platform adapter qualification.

### DEC-034: Task-pinned configuration with explicit coordinated migration

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-026.

Status: accepted requirement; bundle capture, native loading, checkpoint migration and descendant controls remain unimplemented and unqualified.

Topic: applying shared rules, skills and ordinary configuration updates to active tasks.

Goal: keep each active task's workers on one recorded configuration revision while allowing deliberate adoption of newer shared assets.

Constraints: preserve DEC-033's inheritance for new independent tasks and DEC-032's canonical files. Pinning configuration does not freeze ordinary curated project knowledge: DEC-022 keeps it immediately retrievable by authorized workers. DEC-019 separately governs verification-baseline versions and current-contract acceptance. Permission revocations and emergency stops override continued execution; a snapshot cannot preserve revoked authority. Changes that expand scope, spend or permissions retain their approval requirements. A refresh cannot erase instructions already consumed by a model.

Decision: use Q-026 A. Pin the effective rules, skills and ordinary configuration bundle when a task starts. Its existing workers, newly spawned workers and replacement workers use that task revision. New independent tasks resolve the latest applicable accepted configuration under DEC-033. Show available updates and allow an explicitly requested task-wide migration at a safe checkpoint, coordinating any necessary worker refresh or restart and recording the transition. Do not automatically migrate an active task merely because global or project files changed. Precise migration controls and native adapter capabilities remain to be designed and qualified.

Owner evidence: the owner answered "A" to Q-026, selecting recorded task configuration until explicit upgrade rather than automatic adoption of accepted updates at coordinated checkpoints.

Rejected alternatives: automatically replacing active-task configuration whenever an accepted update appears; letting newly spawned or replacement workers silently adopt a different revision from their task; freezing project memory along with configuration; treating an old snapshot as continuing authorization after revocation; claiming synchronization merely because generated files were updated.

Risks: long-running tasks can retain outdated ordinary instructions; native harnesses or descendants may load ambient configuration that differs from the recorded bundle; migration can require restart or context rebuilding; partial migration can leave workers at different revisions. Required loading and transition behavior needs runtime evidence, including honest reporting of workers that cannot adopt a revision.

Acceptance criteria: task evidence identifies its effective configuration revision; new and replacement workers for that task receive the same revision; accepted shared-asset edits do not silently change an active task; new independent tasks resolve current accepted configuration; an explicit migration records old/new revisions and actual adoption state across the task; workers that cannot adopt are not reported as synchronized; memory retrieval, current-contract verification and revoked-authority handling retain their existing policies. Runtime qualification must demonstrate these behaviors before claiming support.

Unresolved questions: bundle contents and capture point, task and descendant identity, migration request/approval presentation, checkpoint definition, refresh and restart support per harness, stale ambient native settings, partial-transition recovery, evidence invalidation after migration, and cross-platform qualification.

### DEC-035: Managed harness configuration with reviewed import and deliberate export

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-027.

Status: accepted requirement; managed scopes, native configuration loading and authentication coexistence remain unimplemented and unqualified.

Topic: coexistence between CrazyAgent's canonical assets and configuration used by ordinary native CLI sessions.

Goal: reuse selected existing user assets while controlling which sessions receive CrazyAgent-generated configuration and which native files an export changes.

Constraints: preserve DEC-032's canonical source and DEC-034's task-pinned revisions. Managed configuration is not proof of operating-system isolation or protection against ambient native configuration. Preserve supported native authentication; do not assume a clean configuration mode can reuse every subscription login. No credential migration, installation, native flag, tool or storage path is selected. This product decision does not authorize this interview session to change the owner's global configuration.

Decision: use Q-027 A. Offer a reviewed import of selected existing rules, skills and shareable settings into the canonical assets. Generate configuration for CrazyAgent-managed sessions in designated scopes where the harness supports it. Leave existing native configuration files unchanged by default. Offer a deliberate, reviewed export when the user wants to update the usual CLI environment. Automatic synchronization into ordinary native configuration destinations is not the default. Unsupported native loading or scope control must remain explicit rather than being reported as successfully managed.

Owner evidence: the owner answered "A" to Q-027, selecting managed session configuration with reviewed imports and deliberate exports instead of default synchronization into usual native CLI configuration.

Rejected alternatives: synchronizing ordinary native CLI destinations automatically by default; treating native files and canonical assets as competing authoring sources; importing credentials or enabling executable content merely because shareable settings were selected; assuming a harness's configuration-cleaning mode preserves all authentication and context behavior.

Risks: native harnesses can still discover ambient settings; conversion can omit unsupported fields or alter semantics; a reviewed import can include unsafe executable hooks or plugins; exports can conflict with concurrent external edits; direct CLI sessions can diverge intentionally from managed sessions. Qualification must cover effective configuration, preserved authentication, conflicts and exact write scope.

Acceptance criteria: setup can preview and admit selected supported content into canonical assets; normal managed-session generation does not rewrite existing native configuration files; generated representations identify their source and task revision; supported authentication works through its qualified integration; an explicit export identifies its destination and proposed changes and handles conflicts without silently losing unrelated content; unsupported loading, fields or scope controls remain visible; existing task configuration and protected-policy approval requirements remain enforced. Runtime evidence is required before claiming these capabilities work.

Unresolved questions: supported import fields and formats, import trust and executable-content review, managed paths and native loading per harness, ambient-setting detection, export preview and conflict recovery, authentication coexistence, task-revision enforcement, and cross-platform qualification.

### DEC-036: Engine-controlled peer messaging within an assigned task team

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-028.

Status: accepted requirement; mailbox delivery, native worker identity and communication controls remain unimplemented and unqualified.

Topic: peer-addressed communication between workers assigned to the same task.

Goal: let teammates exchange findings and coordination requests without requiring the leader model to repeat every routine message, while retaining centralized enforcement and oversight.

Constraints: the leader retains approved work-plan and assignment authority; the Rust engine retains enforcement. Messages cannot grant permissions, authorize spawning, transfer worktree write ownership, change protected policy or become trusted verification evidence. Required human approvals remain. This decision does not authorize unrestricted cross-task or cross-project addressing, automatic broadcast to every worker, decentralized task allocation or unbounded spawning. Native sub-agent capture and transport are not assumed to work.

Decision: use Q-028 A. Workers assigned to the same task may address findings, questions and coordination requests to teammates through an engine-controlled mailbox. Direct addressing does not bypass the engine. The engine enforces sender identity, recipients, authorized scope and bounded delivery. Record exchanges and expose them in both interfaces, with relevant status and escalation information supplied to the leader. Control message loops, excessive traffic and conflicting requests. Exact transport, delivery guarantees, payloads and limits remain to be selected and qualified.

Owner evidence: the owner answered "A" to Q-028, allowing teammate messages through the Rust engine rather than requiring the leader to relay every worker-to-worker exchange.

Rejected alternatives: mandatory leader-model relay of every same-task worker message as the default; unrecorded side channels presented as managed communication; trusting a model-provided sender string as identity; treating peer requests as authority to expand assignments or access; permitting unlimited traffic merely because recipients share a task.

Risks: message loops and excessive traffic can consume context and resources; conflicting peer suggestions can distract workers or cause unauthorized edits; spoofed identity can misattribute requests; native descendants may communicate through channels the engine does not capture; leader summaries can omit important information. Identity, scope, traffic limits, delivery state and oversight require qualification.

Acceptance criteria: authorized same-task workers can exchange peer-addressed messages through the engine without a leader-model relay for each message; records identify the actual sender, recipient and task; unauthorized addressing and forged identity are rejected; both interfaces expose recorded exchanges and relevant escalations reach the leader; messages cannot change assignment, worktree or protected-policy authority; bounded-delivery and loop controls are explicit; unsupported native channels remain visibly outside claimed managed coverage. Runtime evidence must demonstrate these behaviors before support is claimed.

Unresolved questions: cross-task and cross-project scope, identity binding for native descendants, transport, payload limits, acknowledgments, ordering and deduplication, retry and expiry, loop prevention, leader-summary selection, offline recipients, and cross-platform qualification.

### DEC-037: Explicit coordination links for same-project cross-task messages

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-029.

Status: accepted requirement; link representation, lifecycle and recipient authorization remain unimplemented and unqualified.

Topic: communication between workers assigned to different tasks in one project.

Goal: let related task teams coordinate without leader-model relay for every message while limiting unrelated traffic and preserving task authority.

Constraints: this controls message addressing, not blanket access to another task's data, worktree or permissions. DEC-022's already-authorized project memory remains retrievable without a communication link. A link is not automatically an execution-order dependency. Preserve leader assignment authority, Rust enforcement, one-writer worktree ownership, independent verification and existing approvals for expansions beyond authorized scope. Cross-project messaging and decentralized allocation or spawning are not authorized by this decision.

Decision: use Q-029 A. Require explicit task-to-task coordination links for cross-task peer addressing within a project. The leader establishes relevant links, such as producer/consumer task relationships, within the approved plan and coordination authority. Workers on linked teams may exchange permitted messages through the engine without human approval or leader-model relay for every message. Unlinked cross-task requests go through the leader to assess and route, including establishing an appropriate link within existing authority. Retain DEC-036's identity, recording, scope and traffic controls. Project-wide peer addressing without links is not the default.

Owner evidence: the owner answered "A" to Q-029, selecting explicit coordination links rather than allowing any worker to address any authorized same-project worker by default.

Rejected alternatives: project-wide cross-task addressing without explicit links as the default; requiring a human action for every message between linked teams; treating communication links as file-access grants or scheduling dependencies; making authorized project-memory retrieval depend on a communication link.

Risks: missing links can delay discovery of unexpected dependencies; stale links can continue to permit unwanted traffic; a leader can establish inappropriate links unless authority is checked; task membership changes can invalidate recipient eligibility; coordination requests can be mistaken for assignments. Link lifecycle, provenance and recipient authorization require qualification.

Acceptance criteria: authorized same-task communication remains available under DEC-036; permitted cross-task messages between linked teams are delivered through the engine without per-message leader relay; unlinked cross-task requests are routed through the leader rather than directly delivered; link creation and changes are attributable to approved coordination authority; recipients and task membership are checked; links do not grant data access, write ownership or assignment authority; shared project-memory retrieval retains its existing policy. Runtime evidence must qualify link creation, denial, removal and task-membership changes before claiming support.

Unresolved questions: link schema and directionality, creation and removal controls, task completion and membership lifecycle, unexpected-dependency discovery, relation to scheduling dependencies, message authorization after link changes, UI presentation, cross-project scope, and delivery qualification.

### DEC-038: Product projects with explicitly registered repositories

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-030.

Status: accepted product model; repository registration, configuration composition and multi-repository execution remain unimplemented and unqualified.

Topic: the relationship between a CrazyAgent project and its repositories.

Goal: keep one product conversation and shared project knowledge across related repositories while preserving explicit repository and task boundaries.

Constraints: project membership does not grant blanket access to every member repository or the surrounding filesystem. Tasks identify repositories they may access or change; permissions and one-writer worktree ownership remain explicit. Preserve canonical assets, task configuration revisions, independent verification and same-project communication authority. This choice does not promise atomic merges or releases across repositories, a supported remote-host catalogue or qualified multi-repository execution by a particular release date.

Decision: use Q-030 A. A CrazyAgent project is a product workspace containing one or more explicitly registered repositories. Related frontend, backend and shared-library repositories can share one central conversation, project knowledge and coordinated task context. Worktrees, source revisions, permissions and verification evidence remain attributable to each repository. Registration and task scope are explicit rather than inferred from filesystem proximity. Exact repository-specific configuration, workspace file locations and cross-repository acceptance remain to be designed. Implementation and release qualification require a measured plan after the model is resolved.

Owner evidence: the owner answered "A" to Q-030, selecting a product workspace with one or more repositories rather than exactly one repository per project.

Rejected alternatives: forcing every repository into a separate project and knowledge context; inferring repository membership or access from neighboring directories; treating shared project context as permission to modify every repository; claiming repository-local Git operations form an atomic cross-repository transaction.

Risks: repositories can have incompatible configuration or toolchains; shared knowledge needs repository provenance to avoid applying a fact to the wrong codebase; a task can finish in one repository while remaining blocked in another; repository changes and target revisions can invalidate combined evidence; broader project context increases the importance of access controls. These behaviors require explicit design and runtime qualification.

Acceptance criteria: one project can explicitly register one repository or several; the central conversation and authorized shared knowledge refer to the product project; tasks declare their allowed repositories and operations; worktree ownership, source identities and verification results identify the relevant repository; unregistered or unauthorized repositories are not silently added or exposed; task status distinguishes progress in member repositories rather than treating one repository's success as completion of all work; configuration conflicts and unsupported execution paths remain visible. Runtime qualification is required before claiming any multi-repository flow works.

Unresolved questions: registration and removal, local clone identity, remote/branch support, workspace file locations, per-repository overrides, cross-repository task and acceptance composition, merge order and partial failures, toolchain isolation, repository provenance in memory, and release sequencing.

### DEC-039: Configurable cross-repository merge readiness with coordinated default

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-031 and the owner's follow-up clarification.

Status: accepted requirement; both readiness modes and their settings remain unimplemented and unqualified.

Topic: readiness before merging member changes of one task across several repositories.

Goal: support coordinated and incremental integration workflows while retaining required verification, explicit partial state and existing merge authority.

Constraints: readiness mode is separate from DEC-015's manual, automatic and conditional merge-approval policy. Neither mode may skip combined checks required by the current applicable approved contract, required review/testing gates or repository controls. Only repositories changed by the task belong to its change set. Evidence remains bound to candidate, target, contract and applicable policy identities; changed inputs require appropriate revalidation. Separate repository merges are not atomic, and this decision does not authorize automatic rollback or deployment/release.

Decision: support both Q-031 modes, with A as the default. A requires every planned member change to be ready under its applicable gates and merge policy, plus successful required combined verification for the recorded candidate set, before the first merge. Then merge in an explicit order, rechecking applicable conditions as targets change; the task stays incomplete until all required work is accepted. B allows each member repository to merge when its applicable code, review, project-contract and merge-policy requirements pass, without an additional whole-change-set readiness barrier by default. B still records partial integration and waits for all required work and task-level acceptance before reporting completion. Expose the mode as an application setting with an explicit project override, so projects can choose their workflow. Mode changes remain owner-controlled policy choices rather than worker attempts to bypass a gate. Applicability to an already active change set remains to be designed.

Owner evidence: the owner initially answered "A" to Q-031, then clarified: "both should be fine. A by default," with the user able to switch A/B in application or project settings. This clarification replaces an A-only interpretation before any implementation.

Rejected alternatives: supporting only coordinated readiness; making independent member merges the default; conflating readiness mode with permission to auto-merge; treating B as permission to omit applicable combined checks; presenting a partially merged set as fully complete or atomically committed.

Risks: A can delay independently safe changes and still leave partial integration if a later merge fails; B exposes intermediate states while other work remains unmerged or blocked; settings changes can make evidence or readiness stale; unclear change-set membership can add unnecessary blockers or omit a required member. Integration order, intermediate compatibility and failure recovery need explicit qualification in both modes.

Acceptance criteria: a fresh configuration defaults to A; application settings can select A or B; an explicit project selection overrides the inherited readiness default within approved policy; the effective mode and source are visible and attributable; A blocks the first member merge until whole-change-set readiness is satisfied; B allows an independently eligible member to proceed without an additional group-readiness barrier; both enforce all applicable contract checks and existing merge approvals; changed inputs invalidate dependent readiness as required; partial progress and failures remain explicit, and task completion requires all required work and acceptance. Runtime evidence must qualify both modes and setting precedence before claiming support.

Unresolved questions: change-set membership and revisions, detailed readiness evidence, explicit merge order, intermediate compatibility, task/group risk composition, settings versioning and active-set changes, approval presentation, partial-failure recovery, and release qualification.

### DEC-040: Prefer fixing forward after confirmed partial merge failure

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-032.

Status: accepted recovery preference; state reconciliation, viability assessment and recovery execution remain unimplemented and unqualified.

Topic: the first recovery plan proposed after some cross-repository member changes merged and the remaining sequence cannot proceed.

Goal: preserve accepted work and complete the approved task safely where a viable forward repair exists, while making the partial state and alternatives explicit.

Constraints: first reconcile actual repository state; an ambiguous network result is not proof of success or failure. Ordinary incremental progress under DEC-039 B is not itself a failed sequence. Recovery follows existing work-plan, permission, verification and merge-approval policies, including automatic modes where already authorized. Scope or authority expansion still needs approval. This preference does not grant unconditional execution authority, history rewriting, force-pushes, deployment rollback or evidence erasure.

Decision: use Q-032 A. After confirming partial merge failure, pause the affected failed sequence, expose merged, unmerged and blocked members, and first prepare a fix-forward plan. Preserve already merged history and propose the smallest repair or updated remaining changes that can safely complete the approved task. Revalidate the affected candidate set before resuming the merge sequence. If fixing forward is unsafe or not viable, surface the reason and propose an alternative, such as evaluated revert candidates, rather than treating the preference as an obligation to continue. Unrelated work is not automatically stopped. Existing authority and applicable gates determine whether execution is automatic or requires human approval.

Owner evidence: the owner answered "A" to Q-032, preferring a fix-forward recommendation over preparing reverts first.

Rejected alternatives: preparing reverts first as the default recovery recommendation; automatically undoing merged work merely because a later member is blocked; treating a later repair as permission to conceal partial failure; continuing a known unsafe fix-forward path; introducing a new mandatory human prompt for repairs that existing policy already authorizes automatically.

Risks: a partial state can remain while repair is prepared and verified; subsequent repository work can invalidate the recovery candidate; a proposed repair can expand scope or create new compatibility problems; external effects may not be reversed by any Git operation; misleading remote status can produce the wrong plan. State reconciliation, current evidence and viability require qualification.

Acceptance criteria: a confirmed failed sequence exposes actual member states and pauses further affected merges; the first viable recommendation prefers forward repair while preserving accepted history; unsafe or unworkable forward repair is identified and alternatives are presented; recovery candidates receive applicable verification and current policy checks before execution; already-authorized automatic behavior remains possible, while scope or authority expansion requires approval; unrelated work is not stopped solely because the sequence failed; recovery and partial-state evidence remain inspectable. Runtime qualification must demonstrate these behaviors before claiming recovery support.

Unresolved questions: authoritative remote-state reconciliation, transient merge-operation failures, viable-repair rules, recovery budgets and termination, emergency response, alternative-plan selection, current-target revalidation, interaction with deployment systems, and cross-platform/provider execution.

### DEC-041: Configurable project scheduling with fair sharing by default

Date: October 17, 2026.

Task: CrazyAgent architecture grilling, Q-033.

Status: accepted requirement; both scheduling modes, their algorithms and effective settings remain unimplemented and unqualified.

Topic: allocation of scarce new-launch capacity across projects with eligible queued work.

Goal: support different user workflows and resource constraints through a visible scheduling setting rather than one fixed allocation policy.

Constraints: scheduling order cannot expand approved scope, spend, account access, destinations or resource limits. Required prerequisites still determine eligibility. Preserve DEC-027's approved-target fallback policy. This controls new launches rather than permission to interrupt running workers or infer priority from keyboard focus. Exact numerical limits and native-descendant enforcement remain unqualified. A project-local edit must not silently grant authority over other projects' shared resources.

Decision: support both Q-033 modes and expose the selection in settings, with A as the default. A shares new-launch opportunities fairly across eligible projects, respecting explicit priority weights while avoiding indefinite starvation of lower-priority eligible work. B gives the highest-priority project's eligible queued work available compatible capacity first, allowing lower-priority work to use capacity when higher-priority work has no eligible demand for it. Show the effective mode, priorities and queue reasons. Exact algorithms, settings scope and how project priorities or constraints compose with shared allocation remain to be designed. No throughput, latency or qualified concurrency guarantee is established here.

Owner evidence: the owner explicitly requested both modes, "A by default," and the ability to switch A/B in settings according to workflow, constraints and resources. The same message establishes the general configuration requirement recorded separately in DEC-042.

Rejected alternatives: supporting only fair sharing; making strict priority the default; hard-coding one scheduling workflow; equating priority with authority to exceed budgets or interrupt workers; presenting a fairness preference as a measured latency guarantee.

Risks: strict priority can indefinitely delay lower-priority work; fair allocation can delay urgent work and depends on meaningful accounting; incompatible account capacity can make apparent free slots unusable; unknown native consumption can make budgets less precise; conflicting setting scopes can produce unexpected allocation. Algorithms, observations and configuration composition require qualification.

Acceptance criteria: the default mode is A; a user can select A or B through the exposed configuration contract; both interfaces show the same effective mode and explain queue reasons; qualified A behavior distributes opportunities under its defined priority/fairness rules; qualified B behavior selects higher-priority eligible demand first without needlessly withholding unusable capacity; both retain resource, account and prerequisite constraints and do not stop existing workers merely to reorder launches. Runtime evidence must qualify both policies before implementation claims.

Unresolved questions: fairness accounting, priority weights and tie-breaks, shared scheduler or resource-pool scope, project-level priority and quota settings, configuration changes during queued work, numerical concurrency, native-descendant accounting, starvation visibility, and measured throughput.

### DEC-042: Broad workflow configurability through files and both interfaces

Date: October 17, 2026.

Task: CrazyAgent product configuration requirement, supplied with the Q-033 answer.

Status: accepted product requirement; the settings catalog, schema, serialization and settings interfaces remain unimplemented and unqualified.

Topic: configuration as a primary product capability for users with different workflows and resources.

Goal: make most owner-adjustable workflow behavior easy to discover, inspect and configure without editing framework code, using both readable files and application interfaces.

Constraints: preserve DEC-032's one canonical file-based authoring source, Rust validation and derived native representations; preserve applicable inheritance, task-revision and protected-policy approval rules. Configurability changes policies through authorized configuration, not through model messages that bypass enforcement. Existing explicit trust boundaries remain authoritative until deliberately revised. A broad configuration requirement does not prove every native feature is supported or authorize every previously rejected architectural alternative. YAML and JSON were examples, not a mandate to implement multiple equivalent parsers or a final format selection.

Decision: design the framework for broad configurability across different user workflows, resource constraints and levels of experience. Most owner-adjustable policies and resource settings must be configurable rather than fixed to one recommended workflow. Treat recommended defaults separately from supported alternative behavior. Keep settings in discoverable, human-readable canonical configuration files and expose supported settings through both the TUI and desktop settings interfaces, editing the same underlying model. Use shared engine validation and a documented settings contract so files and UI have consistent meaning. Show defaults, effective values, source/scope and eligible overrides, with explanations and actionable validation errors. Each setting must declare its scope and change-application behavior; shared allocation controls and per-project constraints cannot have ambiguous authority. The exact file format, schema and UI organization remain technical design work.

Owner evidence: the owner requested compatibility with a large group of users, said users should be able to configure most things easily, asked for visible YAML/JSON-style configuration and settings in both the TUI and desktop, and rejected hard-coding the framework to one workflow. The owner repeated that fair scheduling is the default while strict priority is selectable.

Rejected alternatives: fixed workflow policy implemented only in code; file-only settings that leave ordinary users without interface controls; UI-only settings hidden in opaque app records; separate settings models that drift between TUI, desktop and files; treating every recommended default as the sole supported capability; repeatedly requiring the owner to restate general configurability for each routine policy preference.

Risks: excessive options can overwhelm users; combinations can conflict or weaken intended controls if admission is unclear; schema and interface drift can produce inconsistent behavior; external file edits and live changes can race with work; supporting several serializations without need can multiply compatibility and migration work. Clear scopes, validation, defaults, discoverability and qualified combinations are necessary.

Acceptance criteria: an owner can inspect and edit supported settings in readable files and through both interfaces without changing application code; edits share canonical meaning and engine validation; effective defaults, inherited values and overrides are discoverable; unsupported values and invalid combinations produce actionable errors rather than silent behavior; setting changes obey declared application timing and existing approval/version rules; the settings catalog distinguishes configurable policy, protected authority rules and backend capability limits; supported alternative workflows include both scheduling modes and the separately approved merge-readiness modes. File/UI round trips and representative configuration combinations require runtime qualification.

Unresolved questions: complete setting catalog and groups, canonical serialization and schema versioning, exact scopes and override precedence per setting, import/export and migration, external-edit conflicts, error recovery, UI navigation/search and accessibility, change previews and timing, and qualification coverage for combinations.

### DEC-043: New-application creation as the first complete representative workflow

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and first end-to-end qualification.
Status: Accepted owner requirement; implementation and qualification remain pending.
Topic: First complete representative workflow.
Goal: Prove that the integrated environment can turn a product brief into a working small application through coordinated harnesses and independent verification.
Constraints: DEC-002 retains the Rust engine, primary TUI and desktop direction, eventual cross-platform support and macOS-first qualification. DEC-007 retains the central leader conversation. DEC-013 retains scoped parallel work and one writer per worktree. DEC-018 requires human-approved acceptance criteria and verification baselines. DEC-042 retains broad workflow configurability.

Decision: Use Q-034 option C for the first complete representative proof: create a small new application from a product brief. Exercise project creation, acceptance drafting and approval, scaffolding, coordinated implementation and verification as one visible creation journey. Include the initial Claude Code and Codex integrations, shared project assets and context, visible work state, and independent verification of both application code and agent-written test integrity. Supporting technical qualification may precede this complete scenario. The selection prioritizes the first proof; existing-repository bug fixes and feature development remain supported product workflows. It does not select the application type, domain, generated application's technology stack, a real repository, publication destination or implementation start date.

Owner evidence: The owner answered "C" to Q-034 after the choices explicitly distinguished a reproducible bug fix, a small feature in an existing repository and a small new application from a product brief.

Rejected alternatives: A reproducible bug fix and a small existing-repository feature as the first complete representative proof. Neither alternative is rejected as an application capability or future qualification scenario.

Risks: Creating an application combines several unqualified integration steps, so a failed proof may need narrower reproduction to locate the cause. A small generated application can succeed while larger repositories, other settings, other platforms or higher concurrency remain unqualified. Choosing this scenario does not establish a revised release deadline.

Acceptance criteria: Record the selected product brief, approved acceptance criteria and baseline, approved execution scope, relevant configuration and source revisions. The completed scenario must show actual coordination through the leader, the initial harness integrations, shared project context, visible status and independent code and test-integrity evidence. Failed, missing, stale or unsupported checks remain explicit and cannot become a successful completion claim. Keep the representative proof's result separate from broader product readiness and release qualification.

Unresolved questions: Application shape and domain; exact brief and measurable acceptance cases; fixtures and independent test-integrity checks; supported interface behavior at this milestone; concrete qualification sequence, runtime compatibility, measurements and revised delivery forecast.

### DEC-044: Persistent API service as the first generated application

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and first end-to-end qualification.
Status: Accepted owner requirement; implementation and qualification remain pending.
Topic: Application shape for the first complete creation proof.
Goal: Demonstrate coordinated application creation through an API with observable request behavior and durable data.
Constraints: DEC-043 selects creating a new small application from a product brief as the first complete representative workflow. DEC-018 retains human approval of the acceptance criteria and verification baseline. DEC-002 retains CrazyAgent's own primary TUI and desktop direction. Existing execution scope, credential, configuration, verification and publication authority remain unchanged.

Decision: Use Q-035 option B: the first generated application will be a small API service with persistent data. Its representative evidence centers on real API request validation, data behavior and persistence, rather than requiring a generated browser or desktop interface for this first scenario. CrazyAgent must still expose coordination and work state through its own applicable interfaces, use the initial Claude Code and Codex integrations, and independently verify code and agent-written test integrity as required by DEC-043. The API domain, product brief, programming language, framework, database, authentication, background jobs and deployment remain unselected.

Owner evidence: The owner answered "B" to Q-035, which described a small API service with persistent data, request validation and real API integration.

Rejected alternatives: A browser application with local persistence and a desktop application as the first generated application's shape. Both remain supported project directions and possible later qualification scenarios.

Risks: A successful API scenario does not qualify generated frontend behavior, desktop packaging or every backend architecture. A service that responds correctly in one process lifetime has not established durable data behavior. Additional authentication, external services or deployment requirements could enlarge the first proof and need explicit selection.

Acceptance criteria: Define the concrete brief and approve measurable API behavior before implementation. The proof must use the actual running service and real persistence for integration and end-to-end checks, including validation and persistence across service restart where applicable to the approved brief. Retain source-bound verification evidence and the independent test-integrity result. Keep missing or unsupported coverage explicit and separate this proof from broader platform, interface, scale and release qualification.

Unresolved questions: Concrete API domain and operations; authentication and external-service scope; exact acceptance examples, persistence and concurrency guarantees; generated application's technology stack; interface and harness qualification sequence; measurements and revised delivery forecast.

### DEC-045: Local inventory and reservations API as the creation proof

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and first end-to-end qualification.
Status: Accepted owner requirement; implementation and qualification remain pending.
Topic: Concrete API proof brief and scope.
Goal: Exercise coordinated application creation with observable API behavior, durable data and a business invariant that independent verification can challenge.
Constraints: DEC-043 selects a new application from a product brief. DEC-044 selects a persistent API service. DEC-018 retains human approval of the concrete acceptance criteria and baseline before execution. The initial Claude Code and Codex integrations, central leader, shared context, visible status and independent code and test-integrity verification remain required by DEC-043.

Decision: Use Q-036 option A: a local inventory and reservations API using sample data. The brief includes creating and listing inventory items, reserving and releasing stock, rejecting invalid requests and preserving data across service restart. Reservations must not exceed available stock, including under concurrent requests. The first fixture excludes sign-in and external deployment. These are demonstration-scope choices, not restrictions on CrazyAgent's supported project types or settings. CrazyAgent's own account support, permission controls and verification authority remain unchanged. Exact endpoint contracts, stock and reservation lifecycle semantics, concurrency cases, language, framework, database and test implementation remain to be specified and approved through the applicable design and verification process.

Owner evidence: The owner answered "A" to Q-036, selecting the local inventory and reservations API with sample data, concurrent stock protection and restart persistence, without sign-in or deployment.

Rejected alternatives: Adding user accounts and authenticated access to the first API proof, and substituting a different owner-supplied API brief at this stage. Neither choice is rejected as a later application capability or qualification scenario.

Risks: Ordinary successful endpoint calls do not establish concurrent stock correctness or persistence. A small local fixture does not qualify production authentication, distributed transactions, external deployment, every project type or broader release readiness. The fixture must remain distinct from CrazyAgent's future agent-oriented ticket system.

Acceptance criteria: Produce and admit measurable cases for item creation and listing, stock reservation and release, invalid request handling, concurrent attempts that cannot over-reserve stock and data retained across a real service restart. Run integration and end-to-end checks against the actual service and real persistence. Preserve source-bound evidence and independently assess the integrity of agent-written tests. Make failed, missing and unsupported coverage visible. Implement neither sign-in nor external deployment as an implicit addition to this proof.

Unresolved questions: Precise API and reservation semantics, test inputs and concurrent execution measurements, generated application's stack, independent test-integrity method, concrete implementation and harness qualification sequence, release recovery guarantees and revised delivery forecast.

### DEC-046: Live-worker survival and reattachment as a first-release gate

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and first-public-release qualification.
Status: Accepted owner requirement; supervision, recovery implementation and runtime qualification remain pending.
Topic: Recovery guarantee after an engine-only crash.
Goal: Keep launch-qualified workers running through a coordinator failure and restore control without duplicate dispatch or loss of authority boundaries.
Constraints: DEC-004 already separates interface lifetime from session lifetime. DEC-009 preserves bounded approved execution authority. DEC-017 prevents unknown or incomplete evidence becoming a pass. DEC-019 and DEC-034 retain applicable baseline and configuration rules. DEC-025 and DEC-026 retain logging-failure policies. This requirement covers an engine-only crash while the machine and worker processes can remain alive.

Decision: Select Q-037 option B. The first public release must provide durable recovery and preserve live worker processes through an engine-only crash for the launch-qualified harnesses, then reattach to those same surviving workers. Recover recorded project, task, approval and available activity state, preserve worktrees and reconcile actual worker and repository state before resuming dispatch. Worker-lifetime separation, recovery of control and prevention of duplicate CrazyAgent dispatch are release-blocking qualifications. A replacement process resumed from saved native conversation state does not by itself satisfy live-worker survival. Unsupported behavior must remain an explicit release gap; a simpler durable-restart capability does not silently fulfill this requirement.

Owner evidence: The owner answered "B" to Q-037, which required durable recovery plus workers surviving an engine-only crash and engine reattachment without duplicate work for the initially supported harnesses.

Rejected alternatives: Treating durable restart recovery alone as sufficient for the first public release, and silently counting a replacement worker as an uninterrupted surviving worker. Durable recovery remains part of the accepted stronger requirement.

Risks: The current proposed single-owner architecture may need separate execution supervision and capture lifetime so a coordinator failure does not terminate workers or lose the ability to control them. Reattachment, identity, output buffering, cancellation, pending approvals and stale ownership require qualification. An uncertain tool side effect must not be blindly replayed; this decision does not assert universal exactly-once effects in external tools or providers. The stronger gate may change delivery feasibility and must enter the revised forecast.

Acceptance criteria: On the qualified harness and operating-system profile, demonstrate that active workers survive a controlled engine-only crash and that the restarted engine identifies and reconnects to the original live workers. Check that it does not launch duplicate workers or reissue uncertain work without reconciliation. Verify retained worktrees, available activity, actual task state and continued enforcement of existing permissions, baseline, configuration, logging and verification rules. Report unobservable or missing state explicitly. Retain evidence distinguishing actual process survival from native conversation resumption in a new process.

Unresolved questions: Supervision and durable-capture mechanism; safe worker authority while the coordinator is unavailable; ownership fencing and reattachment protocol; native child-agent coverage; output and approval handling during failure; restart and startup policy; supported crash conditions and test profile; qualified concurrency and revised delivery forecast. Live processes surviving reboot, logout, explicit termination or machine loss, cross-machine recovery and automatic login startup are not approved guarantees.

### DEC-047: Ten active harness sessions across three projects as the launch qualification target

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and first-public-release qualification.
Status: Accepted owner qualification target; no measured capacity or runtime result established.
Topic: First-public-release concurrency.
Goal: Establish a bounded, measurable initial concurrency target for the integrated environment.
Constraints: DEC-002 retains macOS-first qualification and eventual cross-platform direction. DEC-013 retains scoped parallel work and one writer per worktree. DEC-014 requires visible progress and resource reporting. DEC-041 supports configurable scheduling modes. DEC-046 requires live-worker survival and reattachment after an engine-only crash. Real machine resources, account restrictions, approved spend and execution authority remain binding.

Decision: Select Q-038 option A: target and demonstrate 10 active managed harness sessions across 3 projects for the first public release on an agreed macOS test profile. One counted session is one separately launched top-level harness process, including a leader or verification harness when separately launched. Native child agents are measured separately where observable and do not inherit a demonstrated scale claim. Queued work and saved session history are not active execution. This is the initial qualification target, not a hard-coded maximum, a default resource setting or a claim that every machine and account combination can support it. Runtime concurrency limits and scheduling remain configurable.

Owner evidence: The owner answered "A" to Q-038, selecting 10 active managed harness sessions across 3 projects on an agreed macOS test profile.

Rejected alternatives: Making 50 active managed harness sessions across 10 projects the initial qualification target, and selecting a different owner-specified target at this stage. Higher configured concurrency and later qualification at larger scales remain possible.

Risks: A process count without a defined workload, machine profile, duration and provider conditions can misrepresent useful concurrency. Idle or queued sessions cannot establish the target. Provider quotas, output volume, child agents and local testing work may dominate resource use. A result on one profile does not establish portable minimum system requirements or unlimited scale.

Acceptance criteria: Define and record the macOS machine profile, harness versions and mix, active workload, account conditions, authorized budget, qualification duration and measurements before execution. Demonstrate the stated active-session and project counts with real qualified harnesses, visible resource and task state, scheduling behavior and the required crash-recovery behavior under the defined workload. Include separately observed child-agent and non-harness resource use where available. Preserve evidence and report failed or incomplete coverage without silently substituting a smaller target. No benchmark run, account use, installation or spend is authorized by this decision alone.

Unresolved questions: Hardware profile and minimum requirements, exact workload and adapter mix, test duration, responsiveness and resource bounds, provider limits and available budget, native child-agent measurement, recovery-under-load procedure, interface parity and revised delivery forecast.

### DEC-048: Central approvals with explicit native-prompt fallback

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and first-public-release integration scope.
Status: Accepted owner requirement; native request handling and fallback qualification remain pending.
Topic: Permission approval integration in core workflows.
Goal: Keep approval status and ordinary coordination central while permitting a deliberate native-prompt handoff when the supported adapter cannot expose a permission request centrally.
Constraints: DEC-007 retains the central leader as the normal conversation and task-control interface. DEC-006 requires explicit terminal input ownership. DEC-009 retains approved delegation boundaries. Existing baseline and merge policies remain unchanged. DEC-011 does not make native authentication portable across harnesses. DEC-046 requires valid state reconciliation after engine recovery.

Decision: Select Q-039 option A. Present permission requests through CrazyAgent's central controls where the supported integration exposes and completes them. If such central handling is unavailable for a request, show the affected blocked task and approval source centrally, then allow the user to deliberately take control of the relevant native terminal or provider-supported prompt. Return to the central workflow after the native interaction. This is an explicit compatibility fallback, not a return to routine per-harness task conversations. App-owned plan, baseline and merge approvals continue under their existing central policies. No blanket native permission, prompt auto-acceptance or change to automatic merge eligibility is granted.

Owner evidence: The owner answered "A" to Q-039, selecting central controls where supported plus an explicit native-prompt fallback.

Rejected alternatives: Requiring every permission request in the launch-qualified core workflows to have complete central inbox handling before first release, and silently bypassing a native permission request when the adapter cannot handle it centrally. More complete central integration remains a possible later improvement.

Risks: Native prompts may not expose the same structured state and response evidence as an adapter protocol. A stale or incorrectly attributed request could target the wrong worker, especially after recovery. Native handoff can interrupt a user's centralized workflow and needs clear task and control ownership. It does not resolve provider sign-in compatibility or unsupported native capabilities.

Acceptance criteria: Demonstrate central handling for qualified request categories and deliberate native fallback for qualified unsupported categories on the actual initial harness integrations. Show which task is blocked, the source worker and how the user takes and releases terminal or native-prompt control. Keep request and response identity tied to the actual current action and valid authority. Reconcile prompt state after recovery before accepting a response. Record observed outcomes and expose unavailable evidence without inventing approval or completion. Preserve existing plan, baseline, merge and resource policies.

Unresolved questions: Supported request categories and protocols; native prompt discovery and blocked-state visibility; request identity, expiry and stale-response handling; control transfer and accessibility in both clients; auditable response coverage; headless or unattended behavior; authentication compatibility and runtime qualification.

### DEC-049: Reviewed projects first, broader repository trust support in V2 or V3

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and release security scope.
Status: Accepted owner scope; first-release and future containment qualifications remain pending.
Topic: Repository trust boundary by release.
Goal: Deliver the first release promptly while retaining broader repository compatibility as an explicit product direction.
Constraints: Existing permission controls, independent verification, test integrity and execution-isolation requirements remain. A repository's instructions or scripts cannot grant themselves authority. Release labels describe scope targets and do not establish new calendar commitments.

Decision: Use Q-040 option A for the first public release: qualify user-owned or reviewed projects first. Retain the goal of supporting broader third-party and intentionally untrusted repositories, targeted for V2 or V3. This is a staged qualification plan, not a permanent restriction on project types and not a claim that all repositories are already safe to execute. The later milestone requires an explicit threat model, implemented isolation and evidence before broader containment is advertised. Existing controls for generated code and code under test remain required in the first release.

Owner evidence: The owner described the ideal as execution of any repository with limited access to resources, credentials, network and host files, then explicitly selected A for fast initial development and broader repository support in V2 or V3.

Rejected alternatives: Requiring hostile-repository containment qualification before the first public release, permanently limiting the product to reviewed projects, and treating a configurable sandbox label as proof of containment.

Risks: Users may confuse a sandboxed-execution direction with a guarantee for hostile repositories. V2/V3 scope and the meaning of later release labels still need to be reconciled with the historical roadmap and revised delivery forecast. Dependencies and generated code can remain risky within a user-owned project, so this staged scope cannot waive existing controls.

Acceptance criteria: Publish and test the first release's explicit qualified repository trust boundary without claiming arbitrary hostile-repository containment. Retain permission controls and independent verification for the selected API proof and other qualified initial workflows. Track the broader repository-support milestone with a defined containment boundary and adversarial qualification evidence before claiming it complete.

Unresolved questions: Later milestone placement and dates, precise hostile-repository threat cases, execution and application isolation mechanisms, operating-system capability matrix, runtime and authentication compatibility, resource limits and qualification evidence.

### DEC-050: Layered application and execution isolation with explicit host privileges

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and security architecture requirements.
Status: Accepted owner security goal; component boundaries, enforcement and runtime qualification remain pending.
Topic: Application boundary and per-execution isolation.
Goal: Restrict access to host files, credentials, network destinations and execution resources through complementary containment boundaries.
Constraints: DEC-049 stages repository trust qualification without removing first-release execution controls. The Rust engine must enforce approved authority; the TUI and desktop must share the same protected execution path. Project or model instructions cannot override protected policy. Runtime compatibility, native sign-in and real resource limits must be qualified rather than assumed.

Decision: Preserve the owner's two-layer direction: restrict the application's exposed authority and separately isolate the executions it manages. Every managed execution must have a declared isolation and access profile, with enforcement and unsupported capabilities visible. Design explicit narrow host-integration privileges for operations that need real repository, credential and process access; do not describe an unrestricted host component as sandboxed merely because its client is isolated. The precise placement and implementation of privileged components remain a design decision. The two layers are complementary security boundaries, not a mandate for exactly two nested containers or virtual machines, and their implementation is not yet proven.

Owner evidence: The owner explicitly requested sandboxing around the application itself and again around individual executions, with limited network, memory or resource, credential and host-file access.

Research clarification: Checked Tauri's official security documentation on October 17, 2026. Its WebView-to-core capability boundary does not itself constrain the Rust core or plugin code's system access. Tauri selection alone therefore cannot establish application-wide operating-system confinement or worker execution isolation. This is a framework limitation to account for, not a final selection of Tauri or a sandbox backend. Source: [Tauri security and trust boundaries](https://v2.tauri.app/security/).

Rejected alternatives: Treating frontend permissions as operating-system confinement for every Rust or child process, assuming one outer boundary automatically isolates each worker or the code under test, and advertising configurable resource limits without actual enforcement and coverage.

Risks: A privileged host integration can become an escape path if its commands accept overly broad authority. Shared writable mounts, inherited credentials, network permissions and surviving worker processes can undermine intended boundaries. Layering does not establish protection against every attack, and operating systems or native harnesses may expose different capabilities. Resource isolation and access-control isolation require separate measurements.

Acceptance criteria: Define the application boundary, execution boundaries, privileged host operations and permitted communication paths. Inventory host-file and credential access, network destinations and CPU, memory and process controls with actual enforcement or explicit gaps. Apply the protected execution path to both clients, coding work and code under test, preserving verifier independence and the accepted crash-recovery requirement. Verify boundaries on the supported platform and harness combinations before claiming their guarantees; keep the first-release repository trust scope from DEC-049 explicit.

Unresolved questions: Sandbox backend and packaging prerequisites; precise host-integration placement and protocol; per-task and per-worker boundary details; account and native authentication handling; filesystem and credential grants; network mediation; CPU, memory and process limits; verifier separation; crash supervision and capture behavior; adversarial test cases and future cross-platform qualification.

### DEC-051: Permit a documented local sandbox runtime prerequisite

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and first-release installation constraints.
Status: Accepted owner installation constraint; no sandbox backend selected or installed.
Topic: Execution-isolation runtime prerequisites.
Goal: Permit reuse of an existing isolation implementation while preserving the required security boundary and practical first-release delivery.
Constraints: DEC-002 retains the shared Rust engine, primary TUI, desktop and macOS-first qualification. DEC-011 retains supported account and authentication requirements without assuming credential portability. DEC-046 retains live-worker survival and reattachment qualification. DEC-049 stages repository trust support. DEC-050 requires explicit application and execution isolation boundaries, actual enforcement and visible gaps.

Decision: Select Q-041 option A. The first public release may require one documented separately installed local sandbox runtime if needed by the qualified isolation design. Prefer reuse and qualification of an existing implementation over creating a new isolation engine solely to avoid a prerequisite. A native or bundled implementation remains eligible if it meets the selected guarantees. This permits an installation prerequisite; it does not select a runtime, require a particular vendor, establish its license or authorize installation on the owner's machine now.

Owner evidence: The owner answered "A" to Q-041, permitting a documented local sandbox runtime prerequisite when the qualified design needs one.

Rejected alternatives: Making a standalone installation without a separately installed or managed container or virtual-machine runtime a first-release requirement, and claiming that availability of a runtime alone proves the application's selected isolation guarantees.

Risks: Installation and update friction, runtime availability, host privileges, licensing terms and resource overhead can affect adoption. Native harness authentication, project tools and crash supervision may behave differently inside the selected execution boundary. Different runtime configurations can provide different actual guarantees.

Acceptance criteria: Identify the selected runtime and supported versions or profiles through the applicable design and qualification process. Document installation ownership, prerequisites, actual enforced boundaries, permission grants, provider and native harness compatibility, resource overhead and update responsibility. Show actionable unavailable-runtime or incompatible-profile states through the supported interfaces. Do not silently disable required controls or claim stronger isolation than the active backend provides. Qualify the initial repository scope and accepted recovery behavior before claiming support.

Unresolved questions: Backend and version selection, dependency and distribution licenses, installer and update behavior, host integration, account and native authentication, resource measurements, crash supervision, alternative native or bundled profiles and later platform support.

### DEC-052: Apache-2.0 for CrazyAgent's original code

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and open-source reuse requirements.
Status: Accepted owner license choice for original code; repository license application and dependency compatibility review remain pending.
Topic: Open-source project license.
Goal: Support broad adoption and reuse while making third-party license and attribution obligations explicit.
Constraints: The project is intended to be open source. DEC-010 permits intended reuse and customization of RCLI subject to actual qualification and upstream terms. DEC-051 permits a separately installed sandbox runtime without selecting its license or distribution terms. The project's chosen license cannot override third-party rights.

Decision: Select Q-042 option A and use Apache-2.0 for CrazyAgent's new original code. Preserve original licenses, copyright notices and applicable attribution for reused code and dependencies; review the actual integration and distribution model before incorporating or shipping them. Do not assume that the project license permits incorporating every open-source component under Apache-2.0. Merely operating CrazyAgent does not automatically apply its license to a user's repository; incorporated or generated covered material requires provenance assessment. No license file is applied or third-party code relicensed during this interview.

Owner evidence: The owner answered "A" to Q-042, whose recommended permissive-license proposal was Apache-2.0.

Rejected alternatives: Selecting AGPL-3.0 strong copyleft for CrazyAgent's original code at this stage. This decision is not a blanket classification of every copyleft dependency or an assertion of compatibility for an unreviewed component.

Risks: A useful dependency or intended reuse pattern may impose obligations incompatible with the proposed integration or distribution. Losing provenance or required notices can undermine compliance. The chosen license does not establish permission to reuse provider services, credentials, trademarks or code from unrelated sources.

Acceptance criteria: Apply the selected license accurately to the original implementation through the applicable repository setup and review process. Maintain a record of incorporated components, versions, origins, licenses and required notices; check integration and distribution compatibility before adoption and release. Retain relevant upstream obligations and distinguish the framework's license from the user's project and actual generated-code provenance.

Unresolved questions: Dependency and runtime selection, exact incorporated or modified source, license and notice inventory format, distribution and update model, contributor process and final release compliance evidence.

### DEC-053: Workspace-level leader above project leaders at launch

Date: October 17, 2026.
Task: CrazyAgent architecture grilling and first-public-release coordination scope.
Status: Accepted owner launch requirement; hierarchy, context handling and runtime qualification remain pending.
Topic: Central conversation scope across projects.
Goal: Let a single workspace-level conversation coordinate explicitly authorized projects through their project leaders.
Constraints: DEC-007 retains the central conversation and separates planning from Rust enforcement. DEC-013 retains bounded work and one writer per worktree. DEC-021 preserves project and global memory boundaries. DEC-037 does not grant unrestricted cross-project peer communication. DEC-038 supports multiple repositories inside a project. DEC-041 retains configurable resource scheduling. DEC-047's 10-session target includes separately launched leader processes.

Decision: Select Q-043 option B. The first public release must include a workspace-level AI leader that coordinates selected and authorized projects through their project leaders. Project-scoped leader conversations and coordination remain part of that hierarchy. Preserve each project's permissions, processing destinations, memory scope, verification contracts and execution boundaries. A workspace conversation or global status view does not automatically authorize reading every project's content. Coordination must use authorized project information and preserve provenance; it does not automatically promote project knowledge to global memory or permit unlinked cross-project peer access. The Rust engine remains the policy and execution authority.

Owner evidence: The owner answered "B" to Q-043, requiring a workspace-level leader at launch in addition to project leaders.

Rejected alternatives: Treating project-scoped leaders plus a global status dashboard as sufficient first-release coordination scope, and giving the workspace leader unrestricted project access merely because it is at the top of the hierarchy.

Risks: Cross-project prompts and summaries may expose data to an unapproved destination if scopes are composed incorrectly. Conflicting project policies, stale status, delegation loops and duplicated authority need explicit handling. Additional leader sessions consume the already selected capacity target and may affect delivery feasibility. A hierarchy is not permission for unbounded autonomous spawning.

Acceptance criteria: Demonstrate a workspace conversation delegating approved work through project leaders across explicitly selected projects, with visible source project, authority, status and provenance. Honor each project's required approvals and acceptance gates; a parent plan cannot silently bypass them. Include separately launched workspace and project leaders in the 10-session, 3-project qualification accounting. Verify that unauthorized project data and peer access remain unavailable. Qualify the hierarchy alongside the shared clients, independent verification and accepted recovery behavior; preserve explicit gaps.

Unresolved questions: Workspace and project context contracts, approved summary and retrieval mechanics, data-destination compatibility, role routing and accounts, delegation and completion semantics, project onboarding authority, input ownership, cross-project dependency handling, native child-agent control, qualification workload and revised delivery forecast.

### DEC-054: Individual native child-agent control is a launch requirement

Date: October 17, 2026.

Task: Architecture grilling, Q-044.

Status: Accepted owner decision; required adapter behavior remains unqualified.

Topic: Native child-agent identity, status, cancellation and recovery or continuation state in the first public release.

Goal: Let the human supervise the native child-agent workflows admitted into launch scope through the central application, including after an engine interruption.

Constraints: DEC-046 requires live-worker survival and reattachment after an engine-only crash. DEC-047 separately counts top-level harness processes and native child agents. DEC-048 allows an explicit native permission-prompt fallback; it does not prove native child lifecycle control. Harnesses may expose different child-agent APIs and process structures. No provider capability has been runtime-qualified by this decision.

Decision: Select Q-044 option B. Individual native child-agent status, cancellation and recovery or continuation-state coverage are required for native workflows included in the first public release. Each qualified adapter must expose stable child identity and parent linkage, meaningful lifecycle state, the outcome of a child-specific cancellation request, and the child's actual state after recovery. Distinguish an accepted cancellation request from confirmed termination, and distinguish a live reattached child from a new resumed session or replacement worker. An adapter that cannot establish the required behavior has a qualification gap; narrowing or changing the affected launch workflow must be explicit. Parent-only status and cancellation, or substitution with app-managed workers, must not be presented as proof of native child-agent coverage. Native children need not have separate OS processes, credentials or worktrees, and this choice does not require exact per-child token accounting where the provider cannot expose it.

Owner evidence: The owner answered B to Q-044, with a trailing backslash treated as a typing artifact.

Rejected alternatives: Treating native child lifecycle visibility and control as optional partial coverage for launch; silently using parent-session control as individual child control; claiming that app-managed replacement workers qualify native child behavior.

Risks: Some harness protocols may lack stable child identifiers, targeted cancellation or recovery state. Adapter qualification can reduce the native workflows that can ship or change the delivery forecast. The expanded first-release scope has no new verified effort or completion-date estimate.

Acceptance criteria: Every native child-agent workflow advertised as launch-qualified has evidence for child identity and parent linkage, observable lifecycle transitions, child-specific cancellation with an accurate outcome, and recovery or continuation state after an engine interruption. Tests distinguish live continuation from restart, resume or replacement and report unsupported behavior truthfully. Existing permission, isolation, baseline and verification requirements remain binding.

Unresolved questions: Which native workflows and adapter versions can meet this requirement; what mechanisms provide identity, cancellation and recovery evidence; what scope or schedule changes are needed if an adapter cannot qualify. These are qualification and design work, not resolved provider capabilities.

### DEC-055: Private local storage at launch, application-managed encryption later

Date: October 17, 2026.

Task: Architecture grilling, Q-045.

Status: Accepted owner decision; implementation and storage protection remain unqualified.

Topic: Launch scope and future capability for protection of stored conversations, activity and memory.

Goal: Release with a clear local storage boundary while retaining application-managed encryption as a required future capability.

Constraints: DEC-020 through DEC-024 govern captured activity, curated memory and retention. DEC-042 requires visible, validated configuration. DEC-049 and DEC-050 retain the repository trust and isolation requirements. This choice does not relax credential protection, worker isolation, verification independence or access controls. OS disk encryption cannot be assumed enabled on every device.

Decision: Select Q-045 option A for the first public release: private application storage with restricted OS file permissions and separately protected credentials. Application-managed encryption of the conversation and memory store is not a launch requirement. Disclose reliance on OS disk encryption for offline disk protection and report its status only where it can be established reliably. Option B remains a required future capability: application-managed encryption of captured conversations, activity, curated memory and sensitive derived indexes. Preserve support for configurable storage modes when both are implemented. Do not expose an unavailable encryption mode as working protection. No later release number or completion date is assigned to B by this decision.

Owner evidence: The owner accepted A for the first release, explicitly stated that both modes are configurable, and required B to be implemented eventually.

Rejected alternatives: Making application-managed encryption a launch prerequisite; dropping B from the roadmap; describing file permissions as encryption; claiming OS disk encryption is enabled without evidence; presenting a future setting as an implemented guarantee.

Risks: Under A, processes with sufficient file access under the user's account can read application records, and an unencrypted device lacks offline disk protection. Native CLI artifacts and repository files outside CrazyAgent-managed storage need separate inventory and handling. Future encryption must address keys, recovery, migration, derived indexes, backups, exports, temporary artifacts and ongoing crash supervision. Neither storage mode replaces isolation against running workers.

Acceptance criteria: Launch documentation and both interfaces accurately describe the implemented storage mode and its protection boundary. CrazyAgent-managed records use private storage permissions, and credential protection is qualified separately. Unsupported disk-encryption detection is reported as unknown. The roadmap retains B as required future work, with the affected storage surfaces and qualification needs recorded. Once B exists, configuration is validated through the shared engine and both interfaces under DEC-042.

Unresolved questions: Storage implementation, supported OS protection checks, future encryption and key-management components, migration and recovery design, encrypted-mode default when available, and the release milestone for B.

### DEC-056: Local Git and GitHub.com are the launch integration scope

Date: October 17, 2026.

Task: Architecture grilling, Q-046.

Status: Accepted owner decision; integrations remain unimplemented and unqualified.

Topic: Git hosting coverage required for the first public release.

Goal: Support local repository workflows and one complete hosted pull-request workflow while preserving future support for other hosts.

Constraints: DEC-015 governs human and configured automatic merge authority. DEC-017 requires valid, current verification evidence. DEC-038 through DEC-040 govern multiple repositories, change-set readiness and partial-merge recovery. A hosting integration does not override these requirements or supply blanket authority for remote changes.

Decision: Select Q-046 option A. Qualify local Git workflows and GitHub.com integration for the first public release. Local-only projects must work without a GitHub account. The hosted workflow includes pull-request creation, review and check status, and policy-authorized merges. Keep repository operations, verification evidence and merge policy independent of a particular hosting provider. GitLab, Bitbucket, self-hosted services and other hosting adapters are later integration work rather than launch prerequisites. No additional adapter implementation or release date is selected here.

Owner evidence: The owner answered A to Q-046 after the local Git plus GitHub.com launch scope and later-host support were presented.

Rejected alternatives: Requiring additional hosting providers before the first release; requiring a GitHub account for local-only work; embedding GitHub-specific assumptions into core repository and verification policy; treating provider acceptance as a replacement for the application's verification gates.

Risks: Local and GitHub-hosted state can diverge, and permissions, branch protection, stale head revisions or asynchronous checks can block an operation. Multiple repository merges remain non-atomic. GitHub.com qualification does not establish support for GitHub Enterprise or other hosts.

Acceptance criteria: A local-only project can complete the applicable local development and verification flow without GitHub authentication. A qualified GitHub.com workflow creates a pull request and displays current review and check state, with merge behavior bound to the authorized head, current evidence and configured approval policy. Permission failures, stale state and partial completion are reported accurately. Future provider support is not advertised as implemented.

Unresolved questions: Adapter and authentication mechanisms, supported repository and account configurations, rate-limit and event handling, stale-state reconciliation, exact provider check mappings, GitHub Enterprise and other host milestones, and the revised implementation forecast.

### DEC-057: Complete core workflows in both interfaces at launch

Date: October 17, 2026.

Task: Architecture grilling, Q-047.

Status: Accepted owner decision; interface implementation and qualification remain outstanding.

Topic: Independent core workflow coverage in the TUI and desktop for the first public release.

Goal: Let a person complete the primary development workflow through either interface, including desktop users who do not use the TUI.

Constraints: Both interfaces share the engine's behavior, project state, permissions and evidence. DEC-006 governs simultaneous clients and explicit terminal input control. DEC-042 governs configuration through both interfaces. DEC-048 permits a qualified native provider approval fallback. Interface parity is a launch requirement, not evidence that either client already exists.

Decision: Select Q-047 option A. Both the TUI and desktop must independently support the complete core workflow at launch: project creation and configuration, leader briefs, plan review and approvals, launching and supervising work, inspecting proposed changes and verification evidence, memory review, and policy-authorized merge actions. Application-owned core operations must not require switching to the other interface. Each interface may use its own layout, navigation and interaction model. Advanced inspectors and native CLI surfaces need not be identical. Preserve DEC-048's explicit native provider approval fallback where an adapter cannot centralize a provider prompt; that exception does not permit missing application-owned core controls.

Owner evidence: The owner answered A to Q-047 after complete core workflow coverage in both interfaces and a limited desktop alternative were presented.

Rejected alternatives: A launch desktop limited to monitoring and approvals while requiring the TUI for setup, initiating work or settings; duplicating policy or execution behavior separately in each client; requiring identical layouts or every advanced native tool surface.

Risks: Two independently usable clients increase implementation, accessibility and end-to-end qualification work. Inconsistent state presentation or differing controls could misrepresent authority or progress. The delivery forecast must include the accepted parity requirement and cannot assume a reduced desktop scope.

Acceptance criteria: A user can complete the accepted local application proof and its applicable project, leader, supervision, verification and merge workflow independently through each interface. Both clients expose effective settings and memory review actions, report the same authoritative project state, and respect shared permissions and terminal input ownership. Qualified native provider handoffs are explicit. No desktop core workflow is advertised as complete based only on TUI qualification.

Unresolved questions: Client frameworks, concrete navigation and layouts, shared protocol and state synchronization, accessibility details, per-interface end-to-end tooling, detailed qualification matrix and the revised delivery forecast.

### DEC-058: Ratatui and Tauri interface framework baseline

Date: October 17, 2026.

Task: Architecture grilling, Q-048.

Status: Accepted framework baseline for the detailed design; installation, implementation and runtime qualification remain outstanding.

Topic: TUI and desktop frameworks around the shared Rust engine.

Goal: Build both independently complete core interfaces using suitable maintained framework foundations while keeping execution authority in Rust.

Constraints: DEC-002 selects Rust as the main engine and establishes eventual cross-platform targets with macOS-first qualification. DEC-004 requires session lifetime independent of UI lifetime. DEC-046 and DEC-054 retain live-worker recovery and native child-agent control requirements. DEC-057 requires complete core workflows in both interfaces. Choosing frameworks does not prove any of those runtime properties.

Decision: Select Q-048 option A. Adopt Ratatui with Crossterm for the primary TUI and Tauri 2 with React and TypeScript for the desktop as the framework baseline for the detailed design. Both clients use the shared Rust engine and its authoritative state and policy. Keep the supplementary terminal integration and platform behavior subject to qualification. This choice does not accept all other recommendations in the Q-048 technical synthesis: Tokio, storage, session-runner topology, transport details, adapter mechanisms and the sandbox backend remain proposed implementation calls or unresolved design decisions.

Owner evidence: The owner answered A to Q-048 after the framework baseline and its qualification boundary were presented.

Rejected alternatives: Continuing framework comparison before adopting a baseline; separate execution authorities for the two interfaces; treating Rust as requiring every desktop presentation component to be written in Rust.

Risks: Platform webviews, terminal behavior, accessibility and testing integrations need actual qualification. Native dependency and packaging requirements differ across operating systems. The framework baseline alone does not supply durable session supervision, provider authentication or operating-system confinement.

Acceptance criteria: The detailed design uses Ratatui and Crossterm for the TUI and Tauri 2, React and TypeScript for the desktop, with shared engine operations. Record and qualify resolved dependency versions and third-party licenses before reuse. Verify the accepted core workflow independently through each interface. If a concrete qualification blocker requires a framework change, record a superseding decision with the evidence.

Unresolved questions: Dependency versions, client navigation and components, engine transport, session supervision, storage and configuration formats, sandbox selection, adapter integration, packaging, qualification tooling and revised delivery estimates.

### DEC-059: Full architecture and qualification plan adopted

Date: October 17, 2026.

Task: Architecture grilling, Q-049; transition to implementation planning.

Status: Accepted design baseline. Detailed implementation planning follows; runtime qualification and implementation remain outstanding.

Topic: Shared Rust engine, surviving session runners, dual interfaces and protected execution.

Goal: Turn the accepted product requirements into a concrete, reviewable implementation plan without silently reducing launch scope or claiming unproved runtime capabilities.

Constraints: DEC-001 through DEC-058 remain authoritative. Both interfaces must support the complete core workflow. Original live workers must survive engine-only crashes and reattach, and admitted native child-agent workflows require individual controls. External tool installation, provider sign-in, unapproved provider spending and publication have separate authorization boundaries. The first 16 working hours trigger a delivery reforecast rather than a promise that every qualification gate finishes in that period.

Decision: Select Q-049 option A. Adopt the October 17 architecture and qualification proposal, including P-01 through P-11, as the baseline for the detailed implementation plan. Select one user-local authoritative Rust engine using Tokio and Serde, a surviving session runner for each top-level harness, a shared versioned local protocol, Ratatui/Crossterm and Tauri 2 with React/TypeScript clients, SQLite through rusqlite for operational state and canonical versioned configuration files using TOML as the native format. Adopt the managed RCLI leader foundation, explicit native Claude/Codex capability adapters, deterministic eligibility/routing, scoped mailboxes, narrow trusted host operations and independent verification of immutable source against approved baselines. Evaluate Anthropic sandbox-runtime as the initial existing execution-runtime candidate; its suitability remains conditional on evidence, and adoption is not a claim that a backend already meets the boundary.

Owner evidence: The owner answered A to Q-049 after the complete architecture, qualification sequence, first-16-working-hours forecast checkpoint and remaining proof obligations were presented.

Rejected alternatives: Continuing architectural comparison before implementation planning; treating a documentation review as runtime proof; replacing required native child-agent controls with app-managed workers; downgrading accepted launch requirements without an explicit superseding decision.

Risks: Surviving native control connections, native child lifecycle coverage, authentication inside the execution boundary, enforceable resource limits, macOS sandbox integration and both real interface workflows remain unqualified. These requirements may change the November 30 forecast or require a replacement component. RCLI is an external harness dependency, not a Rust implementation of all model behavior.

Acceptance criteria: Write the detailed implementation plan against DEC-001 through DEC-059 and the adopted design. Include the ordered qualification gates, reproducible evidence, unit/integration/E2E/smoke strategy, appropriate review and security gates, and the first 16 working hours checkpoint. Preserve all 58 previous decision records. Record selected dependency versions and licenses before reuse. A failed qualification gate produces a documented repair, replacement or owner scope/date decision, never an implicit downgrade.

Unresolved questions: Compatible dependency versions and exact adapter mechanisms; the qualifying sandbox backend; evidence for native authentication, live recovery and child-agent controls; measured resource defaults and minimum host profile; packaging/toolchain readiness; and the revised delivery forecast. These are explicit qualification or builder tasks within the accepted design.


### DEC-060: Visible Herdr subagent-driven implementation

Date: October 17, 2026.

Task: Q-050 execution handoff and repository foundation.

Status: Accepted for the current development phase.

Topic: Execution ownership and visible isolated workers.

Goal: The leader coordinates multiple scoped implementation agents while the owner can inspect their work in Herdr.

Constraints: DEC-059 remains the architecture baseline. One agent per Git worktree; disjoint write scopes; shared resources have one owner. No push, external PR or global configuration mutation is implied.

Decision: Answer Q-050 with B and the owner's explicit amendments. Use subagent-driven implementation in visible, labeled panes of the existing CrazyAgent Herdr workspace. The leader prepares task context, creates isolated worktrees, coordinates dependencies and integrates tested results. Begin with the requested local repository, Makefile/build entry points and tracked project instructions, then notify the owner that setup is ready and start the bounded implementation slices.

Owner evidence: The owner described this conversation as the leader and requested multiple sub-agents implementing in separate panes of the Herdr multiplexer, then explicitly asked to initialize the repository and project agent file.

Rejected alternatives: Native-only implementation; invisible background agents substituted for requested panes; several writers sharing one checkout; creating a remote repository or publishing without authorization.

Risks: Pane labels alone do not establish actual worktree/process identity. Parallel work must not race on shared manifests, build outputs, ports, accounts or project state.

Acceptance criteria: Verify actual pane topology, worker process and worktree/branch identities. Give each worker a focused brief and durable report path. Preserve source/test results and exact commits. Agents commit but never push. Notify the owner when foundation setup is complete.

Unresolved questions: Exact bounded lane assignment follows the checked repository/toolchain inventory.

### DEC-061: Skip code and security reviews during current development

Date: October 17, 2026.

Task: Q-050 execution handoff and repository foundation.

Status: Accepted for the current development phase.

Topic: Owner-directed temporary review override.

Goal: Develop quickly using real test/build/test cycles without independent review overhead.

Constraints: This is a project-local execution exception. It does not remove input validation, isolation, permission checks, deterministic gates, tests or honest evidence requirements. Global agent/skill files remain unchanged.

Decision: Skip task reviewer agents, independent normal code review and security review during the current development phase, including the planned DeepSec/Codex Security workflow. Annotate these review gates as deferred rather than passed. This explicit owner direction supersedes conflicting review requirements in project planning and skills for this phase.

Owner evidence: The owner said to skip the review process entirely for now, including code and security reviews, and to use testing, building and testing until real tests pass.

Rejected alternatives: Running review agents despite the owner's instruction; editing global AGENTS or skill files to implement the exception; claiming skipped reviews completed.

Risks: Defects that independent review might detect remain an acknowledged evidence gap. Passing tests do not supply security-review coverage.

Acceptance criteria: Track the override in the project AGENTS file and execution ledger. Start no reviewer/DeepSec/Codex Security job. Report actual tests separately from deferred review coverage. Revisit reviews only when the owner requests them or the development-phase authority changes.

Unresolved questions: No review-restoration date is selected.

### DEC-062: Real tests without mocks or bypasses

Date: October 17, 2026.

Task: Q-050 execution handoff and repository foundation.

Status: Accepted for the current development phase.

Topic: Testing policy for source implementation.

Goal: Prove behavior of actual development code through honest failing-test, build and passing-test cycles.

Constraints: The prohibition includes unit tests as well as integration, E2E and smoke. Synthetic input data and temporary real files/repositories are ordinary test inputs; they do not prove a native/provider capability.

Decision: Write tests against the real production functions and real systems. Do not introduce mocks, fake provider/engine responses, disabled assertions, ignored failing checks, weakened expectations or a skip reported as a pass. Establish the missing-behavior failure, implement the behavior, build and rerun relevant tests plus the available full suite. Report unavailable runtime/profile prerequisites explicitly.

Owner evidence: The owner required real tests of the development code, no mocked tests and no bypass merely to make tests pass.

Rejected alternatives: The earlier unit-mock last-resort allowance for this phase; simulated integration results; modifying a test to hide a production defect.

Risks: Some native/provider checks require an admitted runtime, account or budget. Until those are available, the relevant capability is unavailable rather than proven.

Acceptance criteria: Retain red/green command evidence. Tests execute actual code; integration/E2E/smoke use real systems. Make check propagates every test/build/lint failure. Record missing prerequisites and do not count non-executed tests as passed.

Unresolved questions: The exact available native test prerequisites are inventoried before connected execution.


### DEC-063: Targeted Herdr control from the leader session

Date: October 17, 2026.

Task: Q-051 visible implementation-worker launch.

Status: Accepted owner exception for this session.

Topic: Herdr session guard and workspace authority.

Goal: Launch the approved implementation workers visibly without changing global configuration or disturbing unrelated workspaces.

Constraints: The leader session has no HERDR_ENV context. The installed Herdr skill normally stops pane control in that condition. This exception covers only the existing CrazyAgent workspace w11, explicit observed pane IDs, labeled agents and their assigned worktree directories. Preserve the owner's focus and all other workspaces.

Decision: The owner authorizes targeted pane control in w11 from this session. Do not fabricate Herdr environment variables. Create qualification and runtime-inventory panes with no-focus operations, verify their actual processes and worktree identities, and retain durable execution evidence. No unrelated pane control, cleanup, global configuration change or tool installation is authorized.

Owner evidence: The owner selected "A. Allow targeted CrazyAgent pane control from this session (recommended)" in response to the narrow exception question.

Rejected alternatives: Blocking the already-authorized implementation after the owner exception; invisible workers; pretending the session satisfied the installed guard.

Risks: Workspace and pane identifiers can become stale. Check live identities before each targeted action. A successful pane creation or submitted prompt alone does not establish that implementation is running.

Acceptance criteria: Observe both labeled agents in w11, verify each assigned worktree and branch, preserve focus and unrelated workspaces, and record actual readiness and task evidence.

Unresolved questions: None for this scoped exception. Runtime and provider qualification remain separate gates.


### DEC-064: Trust the owned CrazyAgent repository for worker startup

Date: October 17, 2026.

Task: Q-052 visible implementation-worker launch.

Status: Accepted owner authorization.

Topic: Codex folder trust for the two worker panes.

Goal: Allow the approved coding agents to read, edit and run the owned repository within the workspace sandbox.

Constraints: Codex's observed dialog applies trust to /Users/chaoticqubit/Projects/CrazyAgent and saves that trust decision. This does not disable the workspace sandbox or authorize global configuration edits, tool installs, provider account changes or publication.

Decision: Select Trust and continue in the qualification and runtime worker panes. Preserve the installed Codex workspace sandbox and the disjoint worker scopes. The owner selected this action explicitly after the blocked dialog was inspected.

Rejected alternatives: Answering a persistent trust prompt without the owner decision; restarting duplicate agents; disabling the sandbox to avoid the prompt.

Risks: Repository-local settings can execute code under Codex's folder-trust model. The owner was shown the repository path and persistence of the trust decision before approving.

Acceptance criteria: Both original worker processes pass the folder-trust screen, accept their assigned briefs and operate in their own verified worktrees. Record actual execution evidence separately from the trust decision.

Unresolved questions: None for folder trust. Native runtime/provider qualification remains unproven.


### DEC-065: Implement the qualification evidence contract before native workflows

Date: October 17, 2026.

Task: Implementation plan Task 1.

Status: Implemented technical decision under DEC-059, DEC-060 and DEC-062; no new product-scope approval is implied.

Topic: Trusted admission context, evidence integrity and initial implementation limits.

Goal: Prevent absent, altered, mismatched or unavailable evidence from admitting a required capability while native qualification is still pending.

Constraints: Required source revision and profile come from trusted invocation arguments, independently of report JSON. Synthetic input and temporary real files establish admission logic only. The application must not turn self-consistent worker-authored assertions into trusted native verification results.

Decision: Use Serde/serde_json for bounded JSON records, sha2 for standard SHA-256, rustix for directory-relative no-follow evidence opens, and tempfile only for actual temporary test files. The report command emits unavailable native outcomes and explicit gaps. Admission rejects unbound context, mismatched identity, unproven required outcomes, missing or malformed evidence, changed bytes and escaping paths. The later engine and protected verification layer must authenticate evidence producers and control evidence authority.

Implementation bounds: Report input is limited to 1 MiB; artifact input to 16 regular files of at most 16 MiB each; paths to 1024 bytes and 32 components; map/gap collections to 128 entries. These are the current internal resource ceilings, not a claim that workflow configuration is complete. Record and test any future policy migration rather than accepting unbounded input.

Rejected alternatives: Custom cryptography, native/provider mocks, accepting unavailable as proven, trusting expected identity supplied only by the same untrusted report, unlimited evidence reads and weakening lint/tests to obtain a pass.

Risks: The source/profile contract does not authenticate a report producer or prove a native capability. Cross-platform compilation and behavior beyond the tested macOS profile remain unqualified. RCLI and standalone sandbox-runtime acquisition/profile prerequisites remain unresolved.

Acceptance evidence: Integrated source d1aa990 passed make check in the leader checkout with 11 unit tests and 3 actual-binary integration tests, zero ignored, strict Clippy and formatting, build and repeated tests. The worker retained actual failing behavior tests before implementation. A direct report command returned all four native outcomes unavailable. Source commit 37ead0f was integrated as d1aa990; inventory commit c14aa20 as 734e146. Both visible worker panes remain open.

Unresolved questions: A pinned runtime acquisition plan, execution-profile grants, accounts/budget for native tests and the remaining implementation tasks. Code/security reviews remain explicitly deferred for this development phase.


### DEC-066: Continue implementation through the complete V1 release

Date: October 17, 2026.

Task: Continued implementation after Task 1.

Status: Accepted owner direction.

Topic: Delivery target and autonomous execution.

Goal: Continue the accepted implementation plan through the complete first public release, rather than stopping after each individual task.

Constraints: The approved product scope, real-test policy, visible isolated Herdr workers, temporary review deferral and existing installation/account/publication boundaries remain binding. A missing prerequisite does not become a pass.

Decision: Interpret the owner's P1 reference as V1 following the owner's explicit selection of option A. Continue implementation and testing autonomously. Ask only for unresolved consequential product/authority decisions or genuinely missing permission, and continue independent authorized work while awaiting replies.

Rejected alternatives: Treating P1 as the initial Tasks 1-4 checkpoint after the owner selected the complete first public release; stopping at every task to request continuation.

Risks: Native runtimes, accounts, dependency acquisition and UI qualification may require explicit authority or expose feasibility problems. Record those gaps and retain the accepted release criteria.

Acceptance criteria: The complete core workflow operates independently through the TUI and desktop with admitted native profiles and the remaining accepted release gates. Source progress alone does not establish V1 completion.

Unresolved questions: Exact missing dependency/runtime installation and bounded provider test authority will be presented as concrete requests.

### DEC-067: Maintain a detailed implementation decision log

Date: October 17, 2026.

Task: Continued V1 implementation and durable architecture context.

Status: Accepted owner requirement; recording workflow selected within authorized implementation scope.

Topic: High-level and function-level design rationale.

Goal: Preserve why each meaningful architecture and implementation choice was made, the alternatives, tradeoffs and cost of choosing differently.

Decision: Keep docs/implementation-decisions.md as the tracked implementation rationale log, separate from this owner/product authority register. Assign stable IMP identifiers. Include date/status, task, affected files/symbols, problem/constraints, choice, rationale, alternatives, pros/cons and costs, verification, risks and revisit conditions. Map every production function to relevant decisions; shared decisions may cover several functions without repeating the same rationale. Distinguish historical evidence from retrospective inference.

Constraints: Workers keep their new decisions durably in their own .local-state implementation reports while editing code. The leader consolidates them into the tracked log before integrating each task. One named writer owns the shared log at a time. Supersede records explicitly; never erase prior rationale. The documentation worker initially owns the new log for existing Task 1, then returns ownership to the leader.

Rejected alternatives: Relying only on chat history, scattering decisions across code comments, pretending retrospective rationale was contemporaneously recorded, or several workers racing on one shared document.

Risks: Documentation can drift from source. Record exact symbols and test evidence, maintain a coverage index and update affected records when behavior changes. An explanation inferred from source must not be presented as an owner-approved product decision.

Acceptance criteria: Existing Task 1 has a complete production-symbol rationale map; each later integrated task carries its high- and low-level decision records and real verification evidence. Preserve no-comment and no-review requirements.

Unresolved questions: None for this recording workflow.


### DEC-068: Project-local pinned runtime and locked V1 dependency acquisition

Date: October 17, 2026.

Task: Continued V1 implementation and Task 2 native boundary qualification.

Status: Accepted owner authorization.

Topic: Dependency acquisition and disposable local probes.

Goal: Unblock real native runtime tests and the accepted V1 implementation without global/system changes.

Decision: The owner selected option A authorizing the proposed project-local sandbox-runtime 0.0.80 acquisition, resolution and locking of required V1 dependencies from official registries, and disposable local boundary probes. Record exact versions, sources, licenses and integrity metadata. Prefer existing caches. Disable lifecycle scripts unless a later concrete execution step is explicitly understood and authorized. Keep tool destinations and caches inside the ignored project-local state.

Constraints: No global/system configuration changes, provider calls, credential use, external publication or host privilege changes are authorized. The leader owns shared runtime acquisition. Workers must coordinate shared runtime/probe resources and preserve real capability evidence. Account-connected testing requires separate bounded authority.

Proposal: docs/qualification/dependency-acquisition.md records the immediate runtime source, published integrity, intended destination, dependency policy and exclusions.

Rejected alternatives: Floating npx installs, global installs, silently downloading dependencies before permission, or substituting a missing runtime with mocked qualification.

Risks: Dependency resolution can introduce transitive supply-chain inputs. Generate lockfiles, retain integrity and do not execute lifecycle hooks implicitly. Successful acquisition is not capability proof.

Acceptance criteria: Pinned runtime and dependency metadata are retained, local tests use actual disposable fixtures, global state and provider accounts remain unchanged, and all native outcomes reflect actual enforcement.

Unresolved questions: Bounded provider account/test authority and any concrete native limitation that needs a scope or runtime replacement decision.


## DEC-069: Bounded resource-boundary prototype in the existing Docker VM

Date: October 17, 2026.

Task: Task 2 resource enforcement replacement experiment.

Status: Accepted owner authorization for the prototype only.

Topic: Linux cgroup resource controls in the already-running Docker Desktop VM.

Goal: Obtain actual task-scoped memory, process and CPU-controller evidence after the measured macOS resource-limit gaps, without weakening launch requirements or installing another VM.

Decision: The owner selected option A authorizing the concrete bounded Docker prototype in docs/qualification/resource-boundary-proposal.md. Acquire the pinned official Python arm64 image, then run at most one uniquely named disposable prototype container at a time. Every probe is bounded by 128 MiB RAM, 0.5 CPU, 16 PIDs and 15 seconds, with tighter tested limits permitted. Record image identity, actual cgroup configuration, positive controls and observed enforcement outcomes.

Constraints: No credentials, provider calls, privileged containers, host sockets, host PID namespace, added capabilities or changes to unrelated containers, VM settings or host configuration. Use no network during execution, a read-only root, no-new-privileges and only dedicated disposable nonsecret mounts if required. Remove only prototype containers whose identity has been verified. The leader owns this shared resource slot. This permission does not select Docker as the permanent product runtime or authorize native account qualification.

Rejected alternatives: Continuing unsupported Darwin memory values without a concrete hypothesis; representing per-user process limits as per-task limits; treating CPU rate, CPU-time budget and wall time as equivalent; or silently installing another VM.

Risks: Linux worker processes alter native compatibility assumptions. Passing resource probes alone does not qualify authentication, persistent control, live recovery, filesystem/network isolation or release packaging. Existing Docker Desktop is a prototype prerequisite, not an already-approved product dependency.

Acceptance criteria: The exact bounded experiment preserves immutable image/artifact provenance and records each measured pass, failure or unavailable control truthfully. No unrelated workload or configuration is changed. A separate evidence-based decision precedes permanent runtime adoption.

Unresolved questions: Whether this candidate satisfies the complete worker boundary and native harness requirements; final runtime selection and product integration forecast. Initial probes are estimated at 30-60 minutes; integration and further qualification at 2-4 engineering days, with provider compatibility additional.

### DEC-070: Preserve V1 direction and adopt evidence-driven workflow principles

Date: October 17, 2026.

Task: Resume complete V1 after the fx, Vercel Sandbox and Symvibe research.

Status: Accepted from the owner's explicit continuation instruction.

Topic: Competitor classification, future execution choices and the development workflow.

Goal: Complete the accepted V1 without expanding its scope to imitate another application, while retaining useful research and improving how implementation outcomes are measured.

Constraints: Rust remains the policy and durable-state authority. Keep independently complete TUI and desktop requirements, visible isolated Herdr workers, no mocked or bypassed tests and the existing review deferral. Existing dependency, account, spending and runtime permissions retain their exact bounds. Research evidence is not runtime qualification.

Decisions: Preserve all completed research in tracked reports and research_notes. Treat Symvibe as a proprietary competitor and interaction reference rather than a code dependency. Continue the open-source application under the accepted plan; common workspace features are acceptable overlap. Retain fx on the future harness list alongside RCLI and other qualified harnesses. Retain Vercel Sandbox and other sandbox environments as selectable future execution backends, without adopting or integrating one now. Add the five podcast-derived principles to project AGENTS.md as concrete development rules: measure accepted verified outcomes; compose small reusable components; separate reasoning from execution authority; define independent verification before work; and bound parallel capacity by explicit resources and budgets.

Rejected alternatives: Pivoting V1 into a clone of Symvibe's board and studios; selecting fx or Vercel Sandbox from source inspection alone; replacing the Rust authority; treating more agents, a terminal UI, native resume or a worktree as proof of stronger orchestration, recovery or sandboxing; and expanding V1 to all future adapters.

Pros and costs: This keeps implementation focused, preserves open-source and interface goals, and makes future adapters comparable against the same acceptance contract. It postpones additional harness/backend breadth and requires explicit evidence before claiming reliability, determinism or security advantages.

Risks: Symvibe's undocumented properties remain unknown. CrazyAgent's stricter rules are design requirements until real tests demonstrate enforcement. Current execution and provider qualification gaps still constrain the release forecast.

Acceptance criteria: Research and provenance are retained; AGENTS.md translates the principles into actionable implementation rules; the execution ledger identifies actual completed, failed and unavailable checks; and work resumes from the existing engine integration and resource-probe handoffs without implementing the research candidates.

Unresolved questions: No new owner decision is needed for this workflow update. Permanent runtime adoption, account-connected qualification scope and budgets remain separate pending decisions when concrete evidence makes them necessary.

## DEC-071: Approve the bounded combined Linux containment experiment

Date: October 18, 2026. Task: accepted Task 2/4 qualification follow-up. Topic: Q-056. Status: accepted by the owner's selection A on the explicit experiment card.

Goal: measure filesystem, network and process non-exposure together with the existing resource ceilings before considering a permanent execution backend.

Constraints: use only existing Docker Desktop and the already acquired pinned official image; at most five sequential disposable containers, one at a time, 64 MiB RAM, 0.5 CPU, 16 PIDs, fifteen seconds maximum per container and twenty minutes total. Mount only fresh synthetic candidate/protected/probe fixtures under the exact proposal. No real credentials, provider calls, host sockets, privileges, other workloads or global/system changes. The Rust engine remains a private host fixture. Preserve exact identity checks, bounded output and owned-container/engine cleanup.

Decision: authorize the experiment specified in [the concrete containment proposal](qualification/linux-containment-proposal.md). This extends DEC-069 only for these bounded combined controls. The leader owns the Docker slot and native execution; an isolated worker may implement the proposed probe without changing the authorized scope. Reuse current local tools and the existing image with no pull. If existing Docker file sharing or limits cannot support the fixture, retain unavailable evidence and stop instead of changing configuration or raising ceilings.

Rejected alternatives: deferring all runtime evidence while implementing dependent features; permanently adopting Docker from resource-only results; or treating unexposed host paths as proof of permission enforcement on an exposed bridge. Pros: concrete profile-bound observations with low bounded local resource use. Costs and risks: up to twenty minutes of local experiment time; finite probes cannot establish arbitrary escape resistance, cumulative CPU-time budgets, native account compatibility, child control or live recovery.

Acceptance criteria: real host and container positive controls precede negative claims; record exact kernel outcomes and distinguish non-exposure from permission denial; preserve source/image/payload/config hashes, counters, timing, failed/unavailable observations and exact cleanup. No native profile becomes admitted from this approval or a successful subset. Permanent runtime adoption, any larger experiment and account/budget authority remain separate decisions. Unresolved: measured outcome and complete native execution contract.

## Technical synthesis for Q-048: interface framework recommendation

Date: October 17, 2026.

Status: Research recommendation after the product and authority interview. This is not an accepted framework decision, a complete implementation design, a measured performance result or runtime qualification.

Basis: DEC-002 requires a Rust engine and both interfaces. DEC-004 separates session lifetime from UI lifetime. DEC-046 requires live workers to survive an engine-only crash. DEC-054 requires individual native child-agent controls for qualified launch workflows. DEC-057 requires independently complete core workflows in both clients. These accepted constraints supersede the early report's native-terminal-first primary interaction and any assumption that the desktop owns execution lifetime.

Design practices: Apply software-practices engineering principles through evidence-based choices, reuse, explicit long-term ownership and early qualification. Apply the owner's ponytail ladder by retaining a small local application and selected libraries instead of adopting an unrelated product's complete execution authority. Apply tiger-style through bounded queues, explicit state transitions, validated trust boundaries and testable internal invariants.

| Layer | Recommendation | Responsibility and qualification |
|---|---|---|
| Engine | Rust with Tokio and typed protocol data | One user-local authority for projects, policy, configuration, routing and evidence. Keep its lifetime independent of both clients. Local transport and actual process supervision remain design work. |
| Primary TUI | Ratatui with a compatible Crossterm backend | Application interaction, central conversation and the complete core workflow. Native terminal views are supplementary. Ratatui's backend guidance supports this choice; verify the selected dependency versions together. |
| Desktop | Tauri 2 with React and TypeScript; xterm.js where a native terminal surface is needed | Shared engine operations with desktop navigation, forms and approvals. React and TypeScript are proposed builder choices, not owner decisions. Platform webview behavior, accessibility and real application testing need qualification. |
| Local state | SQLite through one rusqlite database worker for an independent core | Persist coordination state, operation identities and evidence metadata. Canonical configuration and shared assets remain files under DEC-032 and DEC-042. Separate high-volume captured activity from bounded UI notification queues. Storage schema, backups and config format remain design choices. |
| Harness reuse | Managed RCLI foundation plus structured Claude and Codex adapters; portable-pty for actual terminal needs | Preserve the selected RCLI reuse intent. Give adapter identity and lifecycle events authority over inferred terminal text. Do not assume one-shot headless execution is a persistent leader host or that a PTY library supplies recovery. |

Framework evidence: [Tauri's process model](https://v2.tauri.app/concept/process-model/) describes a Rust core and platform webviews. [Ratatui's backend comparison](https://ratatui.rs/concepts/backends/comparison/) recommends Crossterm for most applications. These support the proposed fit; they do not establish a startup, memory or throughput benchmark for CrazyAgent. Electron remains a fallback if platform or testing evidence makes a concrete case; all-Rust desktop alternatives remain possible, with a separate qualification cost rather than an assumed improvement.

Recovery inference: To preserve a live harness across an engine crash, a surviving session process must own its control connection and any PTY or pipes, output capture and identity. The engine can then reconnect and reconcile actual state. A small separate Rust runner is the current candidate, with shared code and no second policy authority. Runner topology, OS supervision, authentication, command fencing, bounded capture and failure behavior require a detailed design and real crash tests. [Tokio process options](https://docs.rs/tokio/latest/tokio/process/struct.Command.html) and [portable-pty](https://docs.rs/portable-pty/latest/portable_pty/) are primitives rather than a complete durable supervision system.

Adapter evidence: The [Claude SDK reference](https://code.claude.com/docs/en/agent-sdk/python) documents stopping a background task by identifier and a subsequent stopped notification. [Codex App Server](https://learn.chatgpt.com/docs/app-server) documents structured thread and turn operations, including interruption; some child-thread filtering is experimental. These are promising integration surfaces, not proof that either adapter meets DEC-046 and DEC-054. Test child identity, targeted cancellation outcomes and post-crash state with the actual chosen versions, authentication modes and confinement. Do not automatically add Python as a runtime solely because that SDK reference demonstrates the capability.

RCLI evidence: The [upstream repository](https://github.com/Backboard-io/Backboard-R-CLI) describes one-shot headless operation, permission limits and different background-agent behavior in that mode. Preserve DEC-010's managed-process or thin-adapter-first approach. Qualify persistent structured leadership before relying on it; a targeted source modification is a fallback for demonstrated gaps, not an assumed completed integration. Permission bypass is not a substitute for the accepted authority model. The earlier frozen source assessment remains a separate revision-specific result.

Isolation boundary: [Tauri security](https://v2.tauri.app/security/) does not make Rust host code or spawned harnesses automatically confined. [Anthropic sandbox-runtime](https://github.com/anthropics/sandbox-runtime) is a candidate for the macOS whole-process boundary, with filesystem and network controls; its defaults and host integrations need explicit qualification. The two-layer isolation goal in DEC-050 remains binding. No final sandbox backend, whole-application confinement proof or cross-platform execution qualification is selected by the UI framework question.

Testing evidence: [Tauri's WebDriver guidance](https://v2.tauri.app/develop/tests/webdriver/) now documents WebdriverIO with an embedded server on macOS, Windows and Linux. Direct tauri-driver remains limited to Windows and Linux. Qualify the actual app through the embedded route on macOS, without mocked IPC in integration or end-to-end qualification. Keep test-control endpoints out of shipping builds. This is a proposed route, not an executed test.

Storage evidence: [rusqlite](https://github.com/rusqlite/rusqlite) provides Rust SQLite bindings, and [SQLite WAL guidance](https://sqlite.org/wal.html) describes concurrency and checkpoint behavior. A single database worker is an engineering recommendation for this local owner model; durability, backup and capture pressure still need tests. DEC-055 governs launch protection and future encryption, independently of the database library.

Next boundary: Q-048 approves or revises the interface framework baseline only. Storage, supervision, adapter and isolation recommendations remain proposed calls for the detailed design. No implementation, dependency installation, account connection or revived calendar promise follows from this research note. Record resolved package revisions and third-party license obligations during qualification.

## Grilling coverage and recording protocol

Interview refinement from DEC-042: distinguish recommended defaults and configurable policies from genuinely exclusive architecture, authority or capability choices. General configurability does not need to be reapproved for each routine preference. Ask about material unresolved constraints, priorities and tradeoffs; infer answers from accepted records where possible. Do not interpret every A/B answer as removing the other coherent workflow, or silently add every unapproved architectural alternative as a runtime mode. Record new defaults, capabilities and authority changes explicitly.

Status: the stack and reuse research is complete and reviewed. Q-001 is answered in DEC-004, Q-002 in DEC-006, Q-003 in DEC-007, revised Q-004 in DEC-009, Q-005 in DEC-010, Q-006 in DEC-011, Q-007 in DEC-012, Q-008 in DEC-013 and DEC-014, Q-009 in DEC-015, Q-010 in DEC-016 and DEC-017, Q-011 in DEC-018, and Q-012 in DEC-019. Q-013's project/global memory-agent architecture is recorded in DEC-020 and approved in DEC-021. Q-014 is answered A in DEC-022. Q-015 is answered A in DEC-023. Q-016 is answered A in DEC-024. Q-017 is answered with both modes in DEC-025. Q-018 is answered A in DEC-026. Q-019 is answered A in DEC-027. Q-020 is answered A in DEC-028. Q-021 is answered A in DEC-029. Q-022 is answered A in DEC-030. Q-023 is answered A in DEC-031. Q-024 is answered A in DEC-032. Q-025 is answered A in DEC-033. Q-026 is answered A in DEC-034. Q-027 is answered A in DEC-035. Q-028 is answered A in DEC-036. Q-029 is answered A in DEC-037. Q-030 is answered A in DEC-038. Q-031 is answered with both modes, A by default and application/project configurability, in DEC-039. Q-032 is answered A in DEC-040. Q-033 is answered with both scheduling modes and A by default in DEC-041. DEC-042 records the owner's broad configuration requirement and refines the interview. Q-034 is answered C in DEC-043: create a small new application from a product brief. Q-035 is answered B in DEC-044: a small API service with persistent data. Q-036 is answered A in DEC-045: a local inventory and reservations API proof with concurrent stock consistency and persistence across restart. Q-037 is answered B in DEC-046: durable recovery plus live-worker survival and reattachment after an engine-only crash is a first-release requirement. Q-038 is answered A in DEC-047: target 10 active managed harness sessions across 3 projects on an agreed macOS qualification profile. Q-039 is answered A in DEC-048: central approval controls where supported with explicit native-prompt fallback. Q-040 is answered A for the first release in DEC-049, with broader untrusted-repository support targeted for V2/V3. DEC-050 records the owner's layered isolation requirement and the distinction between that goal and a proven sandbox. Q-041 is answered A in DEC-051: a documented local sandbox runtime prerequisite is permitted when needed by the qualified design. Q-042 is answered A in DEC-052: select Apache-2.0 for CrazyAgent's original code, with third-party compatibility and notices reviewed separately. Q-043 is answered B in DEC-053: workspace-level AI coordination through project leaders is required at launch. Q-044 is answered B and recorded in DEC-054: individual native child-agent status, cancellation and recovery or continuation-state coverage are required for native launch workflows. Q-045 is answered A for launch with B required later, recorded in DEC-055. Both modes are intended to be configurable when implemented; no release date for B is selected. DEC-008 records the owner's routing clarification. RCLI static qualification is complete, with runtime integration still unproven. The following are coverage areas, not approved technical choices. Resolve dependencies before moving to the next question. Questions use Q-001 onward; accepted or rejected decisions use the next available DEC number. Unanswered questions remain open, and research recommendations remain proposed unless subsequently accepted in the numbered register. Q-046 is answered A and recorded in DEC-056: local Git plus GitHub.com at launch, with other hosts later. Q-047 is answered A and recorded in DEC-057: both interfaces independently support the complete core workflow at launch. The current product and authority interview supports proceeding to technical design synthesis; unresolved implementation, qualification and delivery details remain explicit. A refreshed technical synthesis distinguishes source-documented framework and adapter capabilities from unqualified runtime requirements. Q-048 is answered A and recorded in DEC-058: the interface framework baseline is adopted for detailed design, with runtime qualification outstanding.

Technical design checkpoint: the [architecture and qualification proposal](superpowers/specs/2026-10-17-crazyagent-architecture-design.md) is adopted in DEC-059. A fresh builder check found no consequential unanswered design question; its [frozen target and limits](superpowers/specs/2026-10-17-crazyagent-builder-check.md) are retained. Q-049 is answered A. Detailed implementation planning is the next step; runtime qualification and implementation are not complete.

The [research recommendation](../reports/Rust%20dual%20interface%20architecture.md) proposes a user-local Rust owner with Ratatui/Crossterm and Tauri/xterm.js clients, native PTY sessions initially, selective reuse by default, and explicit qualification of whole-process confinement and native authentication. A Vibe Kanban fork is only a bounded comparison because of company shutdown, sunset project routes and maintenance ownership. The proposed 29-44-hour qualification envelope replaces the earlier 22-hour estimate. October 30 becomes a stretch demonstrator recommendation, November 6 a conditional public-alpha fallback, and a two-week use decision moves with the actual alpha date. These are research findings and proposals, not newly accepted architecture, dates or authorization to install dependencies.

| Phase | Coverage and dependency |
|---|---|
| 1. Product behavior and session ownership | Primary real workflow, units of work, project identity, what closing each interface means, session lifetime, simultaneous clients, explicit stopping, recovery expectations and first pilot project. |
| 2. Core and client contracts | Foundation reuse, Rust ownership, TUI interaction and terminal attachment, desktop parity, local communication, state snapshots/events, concurrency, bounded logs, cancellation, error states, performance and accessibility. |
| 3. Harnesses and shared project state | Native adapters, accounts and permissions, global/project/session precedence, memory provenance and conflict handling, skills/plugins/config compatibility, message delivery and sender identity, Git/worktree ownership and continuation boundaries. |
| 4. Trusted verification and isolation | Coder and test-worker authority, host integrations, approved rules/baselines, source-bound results, integrity checks and their limits, stale/forged evidence, supported test types, credential access and owner policy changes. |
| 5. Shipping and expansion | Supported OS milestones, release-specific interface parity, dependency/license choices, installation/update behavior, unit/integration/E2E/smoke qualification, CI and security-review gates, measured utility, maintenance and revised dates. |

For each answered question, record the question, the owner's answer, evidence used, the resulting decision ID and any new dependency. Record enough rationale to distinguish a deliberate boundary from an omission. Before the next dependent question, write the relevant decision fields: date, topic, goal, constraints, decision and status, alternatives, risks, acceptance criteria and unresolved questions. After compaction, reread the active records before continuing. Keep this register authoritative rather than duplicating decisions in multiple competing documents.

### Q-001: Session lifetime after interfaces exit

Date: October 16, 2026. Status: answered; see accepted DEC-004.

Question: should running agents continue on the same computer after the owner quits both the TUI and desktop app?

Recommended answer: yes, continue until explicitly stopped, with either interface able to reconnect to the same sessions. A user-local engine owns the processes while the interfaces attach and detach.

Reason and tradeoff: this makes switching interfaces and closing a terminal predictable, while adding a background process, resource visibility, explicit stop controls and secure local communication. It does not promise survival across an engine crash, OS logout, reboot or migration to another machine.

Alternative: terminate or deliberately stop work when the last interface exits. That can simplify lifecycle management but changes the product's continuation behavior. The owner rejected this alternative by selecting A.

Dependencies unlocked: engine topology, native session ownership, reconnect behavior, shutdown controls, concurrent-client input ownership and foundation selection.

Owner answer: A, keep running until explicitly stopped. Resulting decision ID: DEC-004. Interface/framework choices and revised dates remain proposed until their relevant grilling answers are recorded. The owner also requested selectable questions, recorded in DEC-005.

### Q-002: Concurrent interface access and terminal control

Date: October 16, 2026. Status: answered; see accepted DEC-006. Depends on DEC-004.

Question: when the TUI and desktop are open on the same computer, how should they access the same live harness session?

Recommended answer: both can view the session, with one explicit input and resize controller at a time. The other interface can request or take control through a deliberate action. This prevents interleaved typing and conflicting terminal dimensions.

Alternatives: automatically transfer control to the most recently focused interface, or allow only one interface to attach to a session at a time. The automatic option reduces switching actions but can unexpectedly move control; exclusive attachment is simpler but prevents concurrent viewing.

Constraints: no duplicate agent process merely to display a session. The owner selected the recommended explicit-control concurrency policy; terminal presentation and transport remain unresolved.

Owner answer: A, both can watch and explicitly switch control. Resulting decision ID: DEC-006.

### Q-003: Native harness interfaces versus a unified conversation interface

Date: October 16, 2026. Status: answered; see accepted DEC-007. Depends on DEC-002, DEC-004 and DEC-006.

Question: for the first release, should an opened session show the native Claude Code or Codex interface inside CrazyAgent, or a new consistent conversation interface built by CrazyAgent for both harnesses?

Historical recommended answer, not selected: preserve native harness interfaces initially, alongside common project memory, configuration, messages and verification panels. The TUI can focus a native terminal session; the desktop can show that same session in a terminal view. This recommendation aimed to reduce initial adapter and rendering scope, but it does not satisfy the clarified primary interaction in DEC-007.

Alternative: build one consistent CrazyAgent conversation, tool-event, approval and history interface for both harnesses from the first release. This improves visual consistency but requires additional integration and rendering work plus qualification of each provider's supported capabilities and authentication path. Native CLI parity must not be assumed.

Constraints: both choices must preserve shared project state, peer communication, native permission handling and protected verification. Native terminal text does not become trustworthy structured execution evidence. No second inference process may be started merely to render the existing session.

Evidence: the current stack report and its interfaces and harnesses research notes compare engine-owned PTYs with Codex app-server and Claude programmatic paths. No native-session or structured-client implementation has been qualified.

Owner answer: B, with a central leader conversation that takes the owner's input and coordinates individual harnesses and their sub-agents. The owner should not type inputs into each worker. Resulting decision ID: DEC-007.

### Q-004: Approval before leader delegation

Date: October 16, 2026. Status: answered by owner selection A to the revised question; recorded in DEC-009. Depends on DEC-007 and DEC-008.

Original question: for a substantial coding request, should the leader first present a plan and worker allocation for one owner approval, or begin delegating automatically within configured project limits?

Original recommendation: present the plan and proposed worker allocation for one approval, then coordinate execution within that approved scope without asking the owner to direct every worker. This gives a checkpoint for mistaken decomposition or unexpectedly large work while preserving automatic coordination.

Original alternative: begin and adapt work immediately within preconfigured project permissions, worker limits and budget. This removes the plan-approval step but increases reliance on the leader's initial interpretation and enforceable limits. Neither option authorizes unbounded recursion, permission expansion or trusted verification-policy changes.

Tradeoff disclosed before the choice: automatic planning and delegation reduce manual coordination but create risks of unnecessary work, extra resource consumption and error propagation across a hierarchy. The engine must enforce the chosen authority independently. Native tool approvals and destructive-action policy remain separate questions.

Owner clarification: routing by task type and current resource availability belongs between the leader and the workers. The owner suggested a combination of A and B but asked for the distinction to be explained and the question restated. This is not a final approval-mode selection. The routing requirement is recorded separately in DEC-008.

Revised question: for a substantial coding task, should the owner approve the initial work plan and limits once before automatic routing starts, or should the leader start planning and dispatching immediately under preconfigured project rules?

Revised A, recommended hybrid: approve the work plan and its scope/limits once; then automatically select and reassign workers within the approved routing rules. Already-approved fallback choices do not require per-worker approval. A scope expansion, extra budget, permission increase or missing allowed fallback must be handled under explicit exception policy, still to be specified.

Revised B: let the leader start and adapt the plan immediately under preconfigured project limits, with automatic routing and exception handling. This removes initial plan approval and increases dependence on correct interpretation and enforceable policy. Both choices retain the routing layer and separate native tool permissions and verification authority.

Owner answer: A. Resulting approval-mode decision ID: DEC-009. Approve the initial plan and limits once, then allow automatic routing, reassignment and already-approved fallbacks within that authority. Resource/fallback details and exact limits remain future questions rather than being silently selected by this option.

### Q-005: What powers the central leader

Date: October 16, 2026. Status: answered with a source-customizable reuse approach; recorded in DEC-010. Depends on DEC-007 through DEC-009.

Question: for the first release, should an existing supported coding harness power the leader behind CrazyAgent's central conversation, or should CrazyAgent build a separate model/API orchestration runtime?

A, recommended: use an existing supported harness as the leader, selected for the project. CrazyAgent provides its own conversation interface and scoped orchestration tools backed by the Rust engine; the harness supplies the leader's model interaction and reasoning loop. Claude Code or Codex are candidate integrations, subject to structured adapter, tool access and authentication qualification. This reuses an existing runtime and keeps a separate model loop out of the initial implementation. Supported subscription versus API billing is not inferred from this choice. Leader replacement and continuity are separate questions.

B: build a dedicated model/API orchestration runtime for the leader, separate from worker harnesses. CrazyAgent owns the planning loop, model calls, context management and tool execution protocol. This provides more direct control over leader behavior but adds integration, authentication, billing, retry and recovery work. It does not imply that existing coding subscriptions can fund these API calls.

Shared behavior: both options retain CrazyAgent's custom central conversation, initial plan approval, task/resource routing and the Rust engine's enforcement of permissions and limits. Neither makes the leader the trusted verifier or authorizes unlimited nested agents. No specific leader model, automatic leader switching policy or authentication method is selected by this question.

Owner answer: combine reuse with customization by using Backboard RCLI and modifying its available source when needed. The owner supplied https://backboard.io/products/cli. Resulting decision ID: DEC-010. The earlier A/B framing is superseded by this answer; actual integration capabilities, backend dependencies, credentials and maintenance remain subject to qualification.

### Q-006: Leader model access and billing boundary

Date: October 16, 2026. Status: answered by a broader all-role flexibility requirement; recorded in DEC-011. Depends on DEC-010 and RCLI static qualification.

Question: may the RCLI leader use separately configured model API access or a compatible local model, while Claude Code/Codex workers retain their native sign-ins, or must the leader also run entirely through existing coding-subscription sign-in?

A, recommended for the selected RCLI direction: allow an explicitly configured provider API key or compatible local endpoint for the leader. Remote provider calls may have usage charges separate from coding subscriptions. Workers retain their native authentication; selecting RCLI does not replace them with direct API workers. A local endpoint is an option subject to capability and performance qualification, not a promise that any local model can coordinate the project well. Whether Backboard is an optional supported backend, which default provider/model is used and exact spending limits remain separate questions. No actual credential entry or paid model request is authorized by answering this design question.

B: require the leader to use existing Claude Code/Codex subscription sign-in as well. This adds an integration constraint that the inspected RCLI source and documentation have not demonstrated. Retain the owner's reuse/customization direction, but qualify a supported subscription-backed leader path before committing to its implementation or delivery estimate. Do not copy native login tokens into RCLI or assume API keys share subscription allowances.

Evidence: RCLI documents direct providers and compatible endpoints without Backboard login; inspected source does not establish compatibility with the native coding subscriptions. Having editable source does not itself establish an authorized or technically supported authentication integration.

Owner answer: any supported access method may be used for any harness, model or sub-agent, not just for the leader. Subscription sign-in, API keys and compatible local endpoints are all allowed product options. Resulting decision ID: DEC-011. The earlier leader-only A/B framing is superseded; supported combinations, configuration and routing policy remain to be qualified.

### Q-007: Leader implementation authority

Date: October 16, 2026. Status: answered with A as the default and B selectable in application settings; recorded in DEC-012. Depends on DEC-007, DEC-009, DEC-010 and DEC-011.

Question: should the central leader coordinate implementation exclusively through workers, or may it also edit project code and execute implementation commands itself?

A, recommended: the leader plans, delegates, monitors, synthesizes results and obtains owner decisions. It can request scoped project inspection through engine-controlled tools, but code changes and implementation commands go to worker harnesses. This makes the leader's responsibility and permissions clearer, at the cost of a worker handoff even for a small edit. Exact inspection tools and confinement remain to be designed.

B: the leader may also implement small tasks directly within the approved plan, while delegating other work to workers. This can avoid a handoff for simple changes but requires implementation permissions for the leader and an explicit rule for when it implements versus delegates. It does not grant broader scope, bypass native approvals or remove Rust enforcement of approved limits.

Shared boundary: both options preserve the custom central conversation, task/resource router, supported authentication flexibility and independent verification. The leader cannot modify trusted verification policy or convert its own assertion of success into accepted verification evidence. This question defines product authority rather than claiming runtime isolation already works.

Owner answer: default to A, but make A versus B configurable in application settings. Resulting decision ID: DEC-012. The application-level choice is confirmed; project overrides and the effect of changing the setting during an active run remain unresolved.

### Q-008: Concurrent owner goals within a project

Date: October 16, 2026. Status: answered October 17 with parallel work items, worktrees, agent teams and agent-focused tracking; recorded in DEC-013 and DEC-014. Depends on DEC-007 through DEC-009 and DEC-012.

Question: while one owner request is still running in a project, may the owner start a second independent request and have both progress concurrently, or should the second request wait? For example, building login while fixing an unrelated search bug. This concerns separate owner goals, not multiple workers collaborating on a single approved goal.

A, recommended for the integrated project workflow: allow multiple independent goals to progress concurrently when approved capacity and workspace ownership permit it. Track each goal's plan, approval, workers, status and results separately while keeping the central conversation as the operator interface. Queue conflicting work or work that exceeds approved limits. Exact workspace isolation, conflict detection, presentation and first-release limits remain to be designed.

B: allow one active owner goal per project and queue independent later requests until it finishes or is stopped. The active goal may still use multiple worker harnesses and sub-agents in parallel. Status questions and clarification of the active goal do not become queued independent goals. This simplifies scheduling and integration but prevents unrelated goals from progressing simultaneously in one project.

Tradeoff disclosed before the choice: concurrent goals improve parallel progress but add resource contention, overlapping-file conflicts, independent approval tracking and integration coordination. The scheduler must respect bounded capacity and workspace authority; the choice does not approve unlimited workers or simultaneous writes to the same files. Existing initial approval and verification requirements apply to each independent goal.

Owner answer: multiple independent work items must run concurrently using isolated worktrees, with multiple harnesses and agents/sub-agents available for each item. Work shares applicable project context, memory and rules, and the human sees stages, problems and token/resource consumption. Tickets are one example of work items; an agent-focused ticket system is an eventual integrated extension. Resulting decision IDs: DEC-013 and DEC-014. The example of ten parallel tickets is illustrative, not a qualified numeric limit. One-active-goal option B is rejected.

### Q-009: Default merge authority for verified work

Date: October 17, 2026. Status: answered with manual default and configurable automatic/conditional policy; recorded in DEC-015. Depends on DEC-009, DEC-013, DEC-014 and the independent verification requirement.

Question: after a work item's changes pass the required review and trusted verification, should the default behavior ask for human approval before merging into the project's target branch, or merge automatically when the approved project policy permits it?

A, recommended: prepare the integration candidate and required checks automatically, then request human approval before changing the target branch. This keeps concurrent execution automatic while retaining a checkpoint for accepting combined results. It adds an approval step at integration; it does not require the owner to direct each worker.

B: merge automatically after all applicable review, verification, dependency and project-policy gates pass under previously approved merge authority. This reduces human integration work but gives the system authority to advance the target branch; stale or incomplete evidence, conflicts and failed combined checks must block advancement. Existing repository rules and required PR approvals remain binding.

Tradeoff disclosed before the choice: isolated worktrees enable parallel changes but do not prove that the combined application is correct. Both options require source-bound evidence and verification of the candidate being accepted, rather than relying only on isolated worker success. Neither choice authorizes external pushes, publishing/releases, changes to verification rules or a bypass of required repository controls. Exact integration ordering, branch topology, conflict handling and configuration options remain future decisions.

Owner answer: require human approval by default, allow automatic merging without application human merge-approval prompts when selected in settings, and allow configurable conditions based on gate-assessed risk/impact. The owner gave low-impact automatic merging and medium/high-impact human approval as an example. Resulting decision ID: DEC-015. Classification, conditions and policy lifecycle details remain open; required review/testing/verification gates are retained.

### Q-010: Authority for merge-risk classification

Date: October 17, 2026. Status: answered A with a broader determinism requirement; recorded in DEC-016 and DEC-017. Depends on DEC-015 and the independent verification requirement.

Question: should the classification used by conditional merging combine explicit project rules with independent review evidence, or should an independent review agent assign the classification without rule-based minimums?

A, recommended hybrid: explicit owner-controlled project rules set minimum risk classifications, while independent review evaluates semantic impact and can raise the classification. The Rust policy engine combines these inputs with required gate results and enforces the owner's merge conditions. For example, an owner could configure authentication or schema changes to have a higher minimum classification; these are illustrations rather than approved thresholds. A reviewer cannot downgrade below an applicable configured floor. Exact rules, evidence, conflict handling and classification dimensions remain to be designed.

B: the independent review agent assigns the classification from the change and available gate evidence, without maintained rule-based classification floors. Rust still enforces the owner's merge conditions and required gates. This reduces explicit classification-rule maintenance but makes the grade more dependent on review-model judgment, repeatability and resistance to misleading inputs. It does not let the coding worker self-approve or bypass verification.

Shared boundary: every required gate remains mandatory in both choices. Classification is tied to the candidate being reviewed; arbitrary ticket labels or worker assertions do not substitute for accepted gate evidence. Missing/contradictory classification behavior in conditional mode is a separate policy question. This classification choice does not add human merge prompts to otherwise eligible automatic-mode merges.

Owner answer: A, because the application should maximize determinism in the meta-harness control layer, gates and rules to improve confidence in what is built and shipped. Resulting decision IDs: DEC-016 for rule-based classification minimums plus independent review, and DEC-017 for the broader deterministic-control requirement. Exact mechanisms and quality guarantees remain to be qualified.

### Q-011: Authorship of trusted acceptance checks

Date: October 17, 2026. Status: answered with automatic AI drafting by default and human approval/edit/import support; recorded in DEC-018. Depends on DEC-009, DEC-016, DEC-017 and the independent code/test-integrity requirement.

Question: should a separate verification worker draft task-specific acceptance checks for approval during initial planning, or should the trusted checks be authored and maintained by the human/project maintainers?

A, recommended: during planning, an independent verification worker drafts checks from the task's acceptance criteria and the project baseline. The owner reviews the acceptance criteria and verification contract as part of the existing initial plan approval. Freeze the approved checks and policy for implementation and verification; coding workers cannot edit that protected authority. This adds agent assistance to check preparation while retaining a human-approved definition of success. Generated checks still need quality/integrity review; independence does not establish that they are sufficient or correct.

B: the human/project maintainers supply and maintain the trusted checks. The verification layer executes them and audits coding-agent-written tests against that baseline; it does not independently author new authoritative checks. Agents may propose additions, but a maintainer controls their authorship and admission. This reduces generated content in the trusted definition while requiring more manual test maintenance.

Shared boundary: coding workers may still write normal unit/integration tests as required by the project; those tests are subject to integrity evaluation and are not automatically promoted into trusted verification policy. Existing approved project checks remain required. Changes to a frozen verification contract use an explicit controlled update rather than silent weakening. Exact contract fields, approval presentation, test-generation checks and update lifecycle remain future decisions. Neither option guarantees complete test coverage or bypasses source-bound execution evidence.

Owner answer: both A and B, with automatic AI drafting as the default. Once the project and requirements are available, an agent should draft acceptance criteria and the baseline, present them to the human and save the approved result. Humans can add to the draft or upload their own material. Resulting decision ID: DEC-018. Project-level setup is explicitly included; later revision/migration behavior and supported import formats remain unresolved.

### Q-012: Approved baseline updates during active work

Date: October 17, 2026. Status: answered A; accepted in DEC-019. Depends on DEC-013, DEC-017 and DEC-018.

Question: after the human approves a new project baseline while work items are running, should active items keep their recorded baseline until a controlled upgrade/integration check, or pause and adopt the new baseline immediately?

A, recommended: pin the approved baseline version for each running work item. Newly planned work uses the updated baseline; active work does not receive silent changes to its checks. Before accepting or merging its result, evaluate against the current applicable approved baseline and perform any required revalidation or controlled plan upgrade. This preserves reproducible runs while allowing parallel progress. Changed acceptance requirements can still require rework or a new plan approval before completion.

B: pause affected active work when a new baseline is approved, update its verification contract through a controlled transition, invalidate dependent old evidence and resume under the new version once any required plan changes are approved. This aligns affected work sooner but may interrupt or repeat ongoing work. Only affected work is in scope; exact dependency and impact detection remains to be designed.

Shared boundary: unapproved drafts do not change trusted authority; neither choice silently edits a run's recorded checks or accepts obsolete evidence as current. Final acceptance must satisfy the current applicable approved contract. Snapshotting a baseline does not retain revoked permissions or bypass emergency-stop controls. Exact project/task applicability, critical-update handling, policy freshness and transition mechanics remain open.

Owner answer: A. Resulting decision ID: DEC-019.

### Q-013: Publishing worker discoveries into shared project memory

Date: October 17, 2026. Status: architecture answered in DEC-020 and approved with the assessment in DEC-021; publication timing resolved in DEC-022. Depends on DEC-007, DEC-013, DEC-017 and DEC-018.

Question: should ordinary worker discoveries enter canonical shared project memory through automatic curation, or require human approval for every addition?

A, recommended: workers submit discoveries to a designated memory-curation path. It checks source references, scope and conflicts, then publishes versioned knowledge updates automatically within the owner's approved policy. Ambiguous or conflicting claims remain explicitly unresolved rather than silently overwriting an existing fact. The human can inspect history and correct or retract entries. This reduces manual approval work but requires qualification of curation quality and mistaken-memory recovery.

B: require human approval before each proposed addition becomes canonical shared project memory. Workers can still exchange task messages and provisional findings immediately, but the shared knowledge record changes only after approval. This gives the human tighter editorial control and creates a potential queue when many workers discover information.

Shared boundary: this question concerns learned project knowledge, such as a discovered build command or a source-linked implementation fact. Human decisions, permissions, verification baselines and approved rules remain protected authority and cannot be changed by a worker or curator relabeling text as memory. Shared knowledge is not proof that a claim is true. Source material remains data rather than permission to execute instructions. Both choices retain provenance, version history and an explicit distinction between confirmed, provisional and disputed claims. Exact storage, publication checks, freshness, retrieval, conflict resolution and any configurable modes remain open.

Owner answer: project memory roles extract knowledge from comprehensive available activity logs and direct candidate submissions, present changes for human review, and nominate reusable knowledge to a global memory role. The owner subsequently approved the asynchronous architecture and human review inbox in DEC-021 and selected the activation policy in DEC-022. Resulting decision IDs: DEC-020, DEC-021 and DEC-022.

### Q-014: When curated memories become available to other workers

Date: October 17, 2026. Status: answered A; accepted in DEC-022. Depends on DEC-017, DEC-018 and DEC-020.

Question: should ordinary project discoveries be available as provisional shared knowledge before human review, or should every project/global memory update wait for approval before other workers can retrieve it?

A, recommended: after curation, ordinary project knowledge becomes available immediately with an explicit provisional label, source references and version history. The human sees a review inbox and can keep or confirm, edit, or remove entries. Global promotions wait for explicit human approval before becoming available across projects. This keeps project learning responsive while applying tighter control to information shared more widely. It permits a mistaken project memory to influence investigation before review, so provisional claims cannot count as verification evidence or change protected authority.

B: both project and global updates remain staged until the human approves them. Other workers can continue receiving direct task messages and provisional findings through the collaboration channel, but retrieval from the curated shared-memory store uses approved entries only. This makes memory publication more controlled and may create a review queue during parallel work.

Shared boundary: both options preserve source provenance, visible uncertainty, correction/removal history and separation from protected rules, permissions, decisions and verification baselines. Curation does not prove truth; an ordinary memory cannot authorize tool execution or a policy change. Raw project logs are not automatically promoted to global memory. Exact cadence, notification batching, configurable modes, stale-entry behavior and retraction propagation remain open.

Owner answer: A. The owner explicitly approved immediate project-memory availability and prior approval for global promotion. Resulting decision ID: DEC-022.

### Q-015: Permitted model destinations for memory curation

Date: October 17, 2026. Status: answered A; accepted in DEC-023. Depends on DEC-009, DEC-011, DEC-020 and DEC-021.

Question: may the memory roles process authorized log excerpts with a configured cloud model, or should memory curation default to local models with cloud processing enabled only through a separate project opt-in?

A, recommended: allow local or cloud memory models according to an explicit project processing policy. During project setup, the human approves the allowed provider/account destinations for curation. The curator can then process permitted, minimized and redacted excerpts automatically within that policy and the existing resource authority. Configuring credentials alone does not authorize a new destination. Changing to an unapproved destination requires approval. The global role receives only authorized nominations and needs an approved processing destination for that material as well.

B: default to local-model curation only. A separate explicit project opt-in is required before a cloud model can process stored activity for memory. When no suitable local endpoint is available and cloud processing has not been enabled, show curation as blocked while retaining available activity according to the storage policy; do not silently switch to a cloud provider. The global role follows the same local-first processing boundary.

Shared boundary: subscription sign-in, API keys and local endpoints remain supported according to DEC-011, wherever the integration permits them. Account authentication and permission to process stored project activity are separate controls. Neither option authorizes unrestricted cross-project log access, secret disclosure, a new spending path, or unsupported provider data-handling promises. Exact consent presentation, inherited defaults, retention and provider-specific behavior remain open.

Owner answer: A. Resulting decision ID: DEC-023.

### Q-016: Default retention for captured activity logs

Date: October 17, 2026. Status: answered A; accepted in DEC-024. Depends on DEC-020, DEC-021 and DEC-023.

Question: should captured activity logs remain available until the human deletes them, or should ordinary logs expire automatically after a configured period?

A, recommended for the owner's analysis goal: retain captured activity logs until the human explicitly deletes them, with visible storage usage and an optional configurable expiry policy. This preserves older conversations and source evidence for later analysis. It increases storage requirements and the amount of retained sensitive material. Exact storage limits, warnings, archiving and behavior under disk pressure remain to be designed.

B: expire ordinary activity logs after 30 days by default, with configurable duration and an explicit pin/export path for runs worth retaining. Curated memory, decision records and required verification evidence have separate retention rules; routine log expiry does not silently delete those records. This bounds ordinary history by age but can make an older memory's underlying source transcript unavailable. Thirty days is a proposed product default, not a measured storage or privacy requirement.

Shared boundary: capture scope and redaction remain unchanged; this concerns retained activity rather than access to hidden reasoning. Expiry and deletion behavior must be visible and user-controlled through the chosen policy. Provenance must show when underlying evidence has expired or been deleted. Exact handling of exports, backups, derived memories, provider-side copies, pinned verification artifacts and project deletion remains open. No cleanup or deletion is authorized by asking this question.

Owner answer: A. Resulting decision ID: DEC-024.

### Q-017: Work behavior when activity logging cannot continue

Date: October 17, 2026. Status: answered both; capabilities accepted in DEC-025, with the default still open in Q-018. Depends on DEC-017, DEC-021 and DEC-024.

Question: if the logging store reaches its limit or stops accepting durable writes, should affected work pause, or continue with explicit gaps in the activity record?

A, recommended: stop dispatching new affected work and request a safe pause/checkpoint or controlled stop of active owned workers. Show the logging problem and recovery options, then resume through a defined recovery transition once durable capture is available again. A pause request is not proof that a native harness or running tool has stopped; show actual worker state and any unrecorded interval. Capture and worker-control behavior require adapter qualification. This prioritizes a reliable activity record and may interrupt work.

B: let otherwise runnable work continue while showing a persistent logging-degraded state and explicit capture gaps. Available code and artifacts can survive, but missing activity cannot later be reconstructed by assertion. This prioritizes continuity and accepts incomplete activity history. Missing required verification evidence still blocks any claim that the corresponding gate passed.

Shared boundary: warn before a configured storage limit where possible; a logging quota can fail even while the worktree remains writable. Neither option silently deletes retained history, enables expiry, pretends a gap is complete, bypasses protected verification or terminates unrelated processes. Exact warning thresholds, affected-project scope, bounded buffering, native pause support, recovery/resume approval and notification behavior remain open. This is a product-policy question and authorizes no live interruption or cleanup.

Owner answer: both. Resulting decision ID: DEC-025. The answer accepts both operating modes without choosing a default.

### Q-018: Default logging-failure behavior

Date: October 17, 2026. Status: answered A; accepted in DEC-026. Depends on DEC-024 and DEC-025.

Question: with both logging-failure modes supported, should the application pause affected work by default or continue it by default?

A, recommended: default to the pause path. Stop new affected dispatch, request a safe pause/checkpoint from active owned workers, and offer an explicit Continue with logging gaps action. Show actual worker state until a pause is confirmed. The user can select the continue mode as the preferred policy in settings.

B: default to continuing otherwise runnable work with a persistent logging-degraded indication and explicit gaps, while offering a Pause affected work action. The user can select the pause mode as the preferred policy in settings.

Shared boundary: both capabilities remain available under DEC-025. This choice concerns the initial default and proposed mode selection, not actual live interruption. Configuration inheritance, project overrides, mode changes during failure, scope of a one-time override and recovery/resume policy remain open. Neither mode bypasses independent verification or silently removes retained history.

Owner answer: A. Resulting decision ID: DEC-026.

### Q-019: Preferred worker unavailable during task routing

Date: October 17, 2026. Status: answered A; accepted in DEC-027. Depends on DEC-008, DEC-009, DEC-011 and DEC-023.

Question: when a task's preferred harness/model is temporarily unavailable, should routing use a suitable already-approved alternative or wait for the preferred target by default?

A, recommended: treat the target as a preference by default. If it is unavailable, dispatch to a suitable alternative already permitted by the approved plan and routing policy. Preserve task requirements, capability requirements, permissions, processing destinations and budget. Record the selected target and reason for the change. A rule explicitly marking a target as required still prevents fallback outside that requirement.

B: treat the preferred target as required by default and queue the task until that target is available. Do not choose another target merely because it is available; an explicit fallback rule or a changed approved plan is needed. This preserves the requested target and may leave otherwise usable capacity idle.

Shared boundary: DEC-009 already allows automatic use of approved fallbacks; this question selects the default routing preference, not new permission to choose arbitrary models, accounts or spending paths. Both choices obey explicit required-target rules and all hard constraints. When no allowed suitable target exists, the router cannot invent one. Exact account ordering, task classification, availability detection, deadlines, handoff format, queue fairness and override scope remain open. Named provider/model examples are preferences rather than proven performance rankings.

Owner answer: A. Resulting decision ID: DEC-027.

### Q-020: Authority to revise persistent routing preferences

Date: October 17, 2026. Status: answered A; accepted in DEC-028. Depends on DEC-008, DEC-009, DEC-017 and DEC-027.

Question: should observed outcomes produce routing recommendations for human approval, or may the router automatically revise its persistent ranking of already-approved targets?

A, recommended: execute versioned, human-approved routing preferences. The system can analyze recorded latency, cost and verification outcomes and recommend a different ranking, but the human approves the policy change before it affects future dispatch. Availability-aware selection and already-approved fallbacks still run automatically under DEC-027. This keeps preference changes explicit and easier to trace, while adapting more slowly to new evidence.

B: delegate automatic ranking changes among already-approved targets within explicit owner-approved bounds. Record the observations, scoring policy, resulting ranking version and reason for each change, and provide inspection and rollback. This permits adaptation without approval on each ranking revision but adds evaluation, feedback-quality and recovery complexity. Observed outcomes are not by themselves proof that one model caused a better result.

Shared boundary: neither choice can add an unapproved provider/account destination, raise budgets, expand permissions, change protected verification criteria or override required-target rules. A changed ranking is a new recorded routing input under DEC-017; deterministic selection still requires the complete policy version and decision inputs. Exact scoring, evaluation windows, minimum evidence, update cadence, rollback effects and supported settings remain open. This question seeks approval of the adaptive behavior if selected; no adaptive optimizer is authorized by earlier fallback approval alone.

Owner answer: A. Resulting decision ID: DEC-028.

### Q-021: Coding-worker visibility of protected verification checks

Date: October 17, 2026. Status: answered A; recorded in DEC-029. Depends on DEC-015, DEC-017, DEC-018 and DEC-019.

Question: should independent verification include separate holdout cases hidden from coding workers, or expose all approved verification source and fixtures to them as read-only material?

A, recommended: use a hybrid. Coding workers can see the approved acceptance criteria and ordinary development tests, while the independent verifier also runs separate protected holdout cases whose exact implementation and fixtures are withheld from coding workers. Humans can inspect and approve those checks. Return actionable failure feedback without disclosing the whole holdout set. Holdouts assess the approved requirements rather than introduce undisclosed requirements. This adds a read-access boundary and feedback design that must be qualified across files, tools, logs and memory.

B: make all approved verification source and fixtures readable to coding workers, while keeping them protected from worker modification. Workers receive detailed failures and can inspect the exact checks. This improves direct inspectability and debugging but leaves the possibility of tailoring an implementation to the known cases. Independent execution and test-integrity checks remain required.

Shared boundary: ordinary development tests remain available for test-first implementation. Humans retain access to the protected checks under both choices. Workers cannot approve or weaken the verification baseline. Neither visibility choice proves general correctness, test quality or effective confinement. Secrets and unrelated private data remain protected; source visibility does not authorize disclosure through logs or memory. Exact holdout generation, admission, execution, failure feedback, leakage prevention and language/tool support remain open.

Owner answer: A, the hybrid of visible acceptance criteria and ordinary development tests with protected holdout cases withheld from coding workers. Resulting decision ID: DEC-029.

### Q-022: Additional verification depth for each change

Date: October 17, 2026. Status: answered A; recorded in DEC-030. Depends on DEC-016, DEC-017, DEC-018 and DEC-029.

Question: should additional verification depth follow owner-approved deterministic risk rules, or should every change run every configured deep check applicable to that project?

A, recommended: use a mandatory verification floor for every change, with additional checks selected by explicit, versioned risk rules. The floor covers the required code and test-integrity checks in the approved contract. More costly checks, such as expanded mutation testing or additional adversarial scenarios where supported, become required for the relevant risk categories or affected areas. Humans can require the full applicable suite for a particular change. This keeps the policy reproducible while controlling routine turnaround and resource use, but the floor and selection rules need qualification to avoid gaps.

B: require the full configured deep-check suite applicable to the project for every change, regardless of its risk classification. This reduces reliance on risk-based check selection but increases execution time and resource use for routine changes. The exact suite, supported adapters and acceptable runtime still need qualification.

Shared boundary: every check required by the applicable approved contract must complete successfully before acceptance or merge eligibility. Faster preliminary feedback does not count as final acceptance. Missing tooling, budget pressure, a timeout or an unsupported test type cannot silently remove a required check or count as success. Independent verification, holdout protection and owner-approved baseline changes remain required. Neither option promises that every possible check exists or that passing proves defect-free software. Exact check selection, thresholds, budgets and language support remain open.

Owner answer: A, a mandatory verification floor for every change plus deeper checks selected by approved deterministic risk rules, with a human option to require the full applicable suite. Resulting decision ID: DEC-030.

### Q-023: Automatic retries for temporary verification infrastructure failures

Date: October 17, 2026. Status: answered A; recorded in DEC-031. Depends on DEC-017, DEC-018, DEC-029 and DEC-030.

Question: may the verifier automatically retry an unchanged candidate when an approved rule identifies a temporary infrastructure failure, or must a human request every such rerun?

A, recommended: permit bounded automatic retries for temporary infrastructure failures identified by explicit, approved rules, such as a required test service being unavailable. Record every attempt, its classification and supporting evidence. A completed rerun can satisfy the gate only when the approved retry policy permits it and the result remains valid for the candidate, contract and required checks. Assertion failures, conflicting pass/fail behavior and unknown failure causes do not become passing results by repeatedly rerunning until green; they remain unresolved until repaired or handled through an approved policy. Reaching the retry cap leaves a visible blocked result. Exact eligible failures, limits and timing remain to be designed and qualified.

B: require a human to request each rerun of an unchanged candidate after any failed verification attempt, including temporary infrastructure failures. Keep the same evidence and integrity requirements. This gives direct control over retry expenditure and investigation but interrupts unattended work for recoverable service failures.

Shared boundary: ordinary authorized code repair remains possible under the work plan in both options; a changed candidate requires new applicable verification rather than being treated as a retry of identical inputs. Neither option permits workers to relabel a defect as infrastructure, weaken the baseline, conceal failed attempts or turn missing evidence into a pass. A retry cannot expand approved spend, permissions or processing destinations. Runtime failure classification, repeated pass/fail handling and isolation still require qualification. No retry count, delay, quarantine mechanism or new human-approval requirement for ordinary code fixes has been accepted by this pending question.

Owner answer: A, bounded automatic retries for temporary infrastructure failures identified by approved rules, preserving every attempt and leaving defects, conflicting results and unknown causes unresolved. Resulting decision ID: DEC-031.

### Q-024: Source of truth for shared rules, skill definitions and configuration

Date: October 17, 2026. Status: answered A; recorded in DEC-032. Depends on DEC-002, DEC-003, DEC-011, DEC-013 and DEC-021.

Question: should shared rules, skill definitions and shareable configuration use editable files as their source of truth, or app-managed records with explicit import/export?

A, recommended: use editable canonical files for these assets. Keep shareable project assets in a project location suitable for Git review, and user-global assets in a separate user-controlled location. The TUI and desktop edit the same canonical content; the Rust engine validates and indexes it and produces qualified harness-specific representations. File changes remain inspectable outside the application. App databases can hold indexes, runtime state and revision metadata without becoming a second competing authoring source. Exact formats, paths, file-change admission, compatibility and conflict handling remain open.

B: use app-managed records as the canonical source. Edit shared assets through the TUI and desktop, with explicit import/export for files, Git workflows and harness-specific representations. This concentrates editing and validation in the application, but outside edits require an import and exported copies need clear freshness and conflict handling. Exact storage and export formats remain open.

Shared boundary: both choices retain one canonical shared-asset model and adapters rather than claiming that every native skill, plugin, permission or configuration feature is interchangeable. This question covers authored rules, skill definitions and shareable configuration, not automatic publication of logs, credentials or curated memory into Git. Executable skills/plugins still require compatibility and access controls. A project-file edit, Git commit or import alone cannot admit changes to protected verification, routing or permission policy; existing approval requirements remain. Global/project precedence, native-file coexistence, version application to active workers, plugin installation and trust remain unresolved. Neither choice selects a library, final database or implementation path.

Owner answer: A, editable canonical files for shared rules, skill definitions and shareable configuration, edited by both interfaces and validated by the Rust engine with derived harness-specific representations. Resulting decision ID: DEC-032.

### Q-025: Global shared-asset inheritance versus independent project copies

Date: October 17, 2026. Status: answered A; recorded in DEC-033. Depends on DEC-028 and DEC-032.

Question: should projects inherit global shared-asset defaults, with explicit project overrides, or receive independent copies that change only through explicit synchronization?

A, recommended: keep projects linked to global defaults. When resolving configuration for new work, inherit applicable accepted global rules, skill definitions and shareable settings, then apply explicit project overrides for ordinary overridable settings. Accepted global changes reach future work in projects that have not overridden the relevant value. Show the effective value and its source so an override is visible. This reduces duplicated configuration, but a global change can affect future work across several projects and needs clear scope and revision tracking.

B: copy the selected global defaults into independent project files at setup. Later global changes do not alter a project's configuration until an explicit synchronization is reviewed and accepted. This gives projects more insulation from global edits, but requires synchronization and makes differences between projects accumulate unless maintained deliberately.

Shared boundary: this choice concerns ordinary authored assets and defaults, not an override hierarchy for protected authority. Project overrides or copied files cannot weaken mandatory owner constraints, verification baselines, routing approvals or access controls. Both options must show effective settings, provenance and conflicts; unsupported native features remain explicit. Existing memory curation and publication rules are unchanged. This question does not decide whether active workers adopt a new configuration revision mid-task, how a worker is refreshed, or how permission revocations take effect. Exact eligible fields, conflict semantics, file layout and task-level overrides remain unresolved.

Owner answer: A, linked global defaults for new work with explicit project overrides for ordinary overridable settings and visible effective-value provenance. Resulting decision ID: DEC-033.

### Q-026: Applying shared configuration updates to active tasks

Date: October 17, 2026. Status: answered A; recorded in DEC-034. Depends on DEC-013, DEC-019, DEC-022, DEC-032 and DEC-033.

Question: should an active task retain its recorded rules, skills and ordinary configuration until an explicit migration, or automatically adopt accepted updates at coordinated checkpoints?

A, recommended: pin the task's effective configuration bundle when it starts. Its existing, newly spawned and replacement workers use that task revision. New independent tasks resolve current accepted defaults under DEC-033. Show available updates, and allow an explicit task-wide migration at a safe checkpoint, with any necessary worker refresh or restart. This keeps an active team consistent and its configuration history inspectable, but long-running tasks may use older ordinary instructions until migrated.

B: automatically migrate active tasks to accepted shared-configuration updates at coordinated checkpoints. Coordinate the task's workers, record the transition and show which workers have adopted the new revision. This propagates improvements into long-running work, but adds interruptions and adapter-dependent refresh or restart work; tasks can change behavior during execution even though the transition is recorded.

Shared boundary: both choices retain one identifiable task configuration revision and explicit transition state rather than silently mixing worker revisions. Native harness refresh, checkpoint and descendant-agent controls remain unqualified; workers that cannot adopt the required revision cannot be reported as synchronized. Neither option can erase instructions already consumed by a model. Ordinary curated project knowledge remains immediately retrievable under DEC-022, separate from configuration revision pinning. DEC-019 still governs approved verification baselines and current-contract acceptance. Permission revocations and emergency stops override continued execution; a snapshot cannot preserve revoked authority. Scope, spend or permission expansion still follows existing approval requirements. Exact migration authorization, checkpoint definition, descendant handling and recovery mechanics remain open.

Owner answer: A, keep the task's effective rules, skills and ordinary configuration pinned until an explicit coordinated migration; new and replacement workers use that task revision, while new independent tasks resolve current accepted configuration. Resulting decision ID: DEC-034.

### Q-027: Managed session configuration versus usual native CLI synchronization

Date: October 17, 2026. Status: answered A; recorded in DEC-035. Depends on DEC-011, DEC-032, DEC-033 and DEC-034.

Question: should generated harness configuration stay in CrazyAgent-managed session scopes by default, or should CrazyAgent also synchronize the usual native CLI configuration destinations?

A, recommended: import selected existing rules, skills and shareable settings through a reviewed flow into canonical assets, then generate configuration for CrazyAgent-managed sessions in designated scopes where the harness supports it. Leave existing native configuration files unchanged by default. Offer a deliberate, reviewed export when the user wants to update the usual CLI environment. This limits unintended effects on sessions outside CrazyAgent, but those sessions will not automatically receive every CrazyAgent configuration change.

B: after explicit setup of synchronization destinations and authority, keep the usual native CLI configuration synchronized from canonical assets as the default. Show changes and conflicts, preserve unrelated content, and provide recovery for overwritten content. This lets direct CLI use receive the same supported settings, but changes can affect sessions outside CrazyAgent and require stronger conflict and compatibility handling. Native-file edits must be reconciled through the canonical admission flow rather than becoming a second independent source of truth.

Shared boundary: both options preserve DEC-034's task revisions and DEC-032's canonical authority; unsupported isolation, native loading or synchronization cannot be reported as working. Preserve supported native authentication rather than assuming a clean configuration mode can reuse every subscription login. This question does not select a native flag, storage path, credential migration, third-party synchronization tool or installation. Credentials, unsupported fields, executable hooks/plugins, ambient configuration and concurrent outside edits require qualification and appropriate access controls. No product choice here authorizes this interview session to edit the owner's global configuration.

Owner answer: A, managed session configuration by default, with reviewed imports of selected existing assets and deliberate reviewed exports to usual native CLI configuration. Resulting decision ID: DEC-035.

### Q-028: Peer-addressed worker messages versus leader relay

Date: October 17, 2026. Status: answered A; recorded in DEC-036. Depends on DEC-007, DEC-009, DEC-013, DEC-017 and DEC-020.

Question: may workers assigned to the same task address messages to teammates through the Rust engine, or must the leader relay every worker-to-worker exchange?

A, recommended: allow peer-addressed messages within the assigned task team through an engine-controlled mailbox. Workers can exchange findings, questions and coordination requests without requiring the leader model to repeat every routine message. Record the exchanges and expose them in both interfaces; give the leader relevant status and escalation information. The engine enforces sender identity, recipients, scope and bounded delivery. This supports parallel coordination but requires controls against message loops, excessive traffic and conflicting requests. Direct addressing does not mean bypassing the engine.

B: route every worker-to-worker exchange through the leader, which decides what to relay. Workers still exchange information, but the leader is the mandatory intermediary. This concentrates coordination and gives one model control over relaying, while adding leader inference work and making routine peer communication depend on leader availability and attention.

Shared boundary: the leader retains work-plan and assignment authority under either choice, and the Rust engine retains enforcement. A message cannot grant permissions, authorize spawning, transfer worktree write ownership, change protected policy or become trusted verification evidence. Required human approvals still apply. This question does not authorize unrestricted cross-task or cross-project messaging, automatic broadcast to every worker, decentralized task allocation or unbounded agent spawning. Native sub-agent transport, identity, delivery and capture remain unqualified; unsupported channels cannot be described as managed. Exact payloads, delivery acknowledgments, ordering, deduplication, traffic limits and leader summaries remain open.

Owner answer: A, peer-addressed messages within the assigned task team through the Rust engine, with recorded exchanges, bounded delivery and leader oversight while assignment and permission authority remain unchanged. Resulting decision ID: DEC-036.

### Q-029: Cross-task message addressing within one project

Date: October 17, 2026. Status: answered A; recorded in DEC-037. Depends on DEC-009, DEC-013, DEC-022 and DEC-036.

Question: should workers on different tasks in the same project communicate through explicit coordination links, or be able to address any authorized worker in that project by default?

A, recommended: require explicit task-to-task coordination links for cross-task peer addressing. The leader establishes relevant links, such as between a task producing an API and a task consuming it, within the approved plan and coordination authority. Workers on linked teams can then exchange permitted messages through the engine without a human approval or leader-model relay for each message. Unlinked cross-task requests go through the leader to assess and route. This keeps coordination purposeful and limits unrelated traffic, but unexpected dependencies may need a new link or leader relay.

B: allow a worker to address any authorized same-project worker by default, including workers on other tasks. Keep engine identity, recording, scope and traffic controls. This supports discovery of unexpected dependencies without maintaining communication links, but increases the potential for unrelated traffic, loops and distracting requests as the number of teams grows. It does not mean broadcasting every message to every worker.

Shared boundary: this choice controls message addressing, not blanket access to another task's data, worktree or permissions. Shared project knowledge remains retrievable under DEC-022; a coordination link is not required merely to retrieve already-authorized project memory. A link is not automatically an execution-order dependency. Existing plan and permission approvals still apply to expansions beyond approved authority. Leader assignment authority, engine enforcement, one-writer worktree ownership and independent verification remain intact. Neither option authorizes cross-project messaging or decentralized spawning and allocation. Exact link representation, recipient discovery, payload authorization, limits and delivery remain unresolved.

Owner answer: A, explicit task-to-task coordination links for same-project cross-task peer messages, established within approved leader authority, with unlinked requests routed through the leader and shared project memory unchanged. Resulting decision ID: DEC-037.

### Q-030: One or multiple repositories within a project

Date: October 17, 2026. Status: answered A; recorded in DEC-038. Depends on DEC-002, DEC-013, DEC-014, DEC-032 and DEC-037.

Question: should a CrazyAgent project represent a product workspace containing one or more explicitly registered repositories, or correspond to exactly one repository?

A, recommended: make a project a product workspace that can contain one or more explicitly registered repositories. A frontend repository, backend repository and shared library can belong to one project with one central conversation and shared project knowledge. Tasks identify the repositories they may access or change; worktrees, source revisions, permissions and verification evidence remain attributable to each repository. This supports coordinated product work without requiring separate project contexts for each repository, but adds repository registration, configuration composition and cross-repository verification complexity.

B: define one project as one repository. Keep conversation, memory, task worktrees and verification associated with that repository. A product split across repositories uses separate projects, with cross-project coordination still to be designed. This simplifies the project boundary and repository-local workflows, but separates context for work that spans a frontend, backend or shared library repository.

Shared boundary: project membership is not permission to access or modify every member repository; task scope and one-writer worktree ownership remain explicit. Repositories are explicitly registered rather than inferred from access to the surrounding filesystem. Neither choice promises an atomic merge or release across repositories, a supported remote-host catalogue, unrestricted cross-project access, or a qualified multi-repository runtime by a particular release date. This chooses the project concept; implementation and release qualification still need a measured plan. Project-file locations, repository-specific overrides, cross-repository acceptance and partial-failure handling remain unresolved.

Owner answer: A, one product project may contain one or more explicitly registered repositories, sharing conversation and authorized project knowledge while tasks retain explicit repository scope, worktrees and attributable verification evidence. Resulting decision ID: DEC-038.

### Q-031: Whole-change-set readiness before cross-repository merges

Date: October 17, 2026. Status: answered both, A by default; recorded in DEC-039. Depends on DEC-015, DEC-017, DEC-019, DEC-030 and DEC-038.

Question: for one task changing several repositories, must the whole change set be ready before the first member merges, or may repositories merge independently as their applicable gates pass?

A, recommended: use coordinated change-set readiness. Before the first merge, every planned member change must be ready under its applicable gates and merge policy, and required combined verification must pass for the recorded candidate set. Then merge in an explicit order, rechecking applicable conditions as targets change and keeping the task visibly incomplete until all required work is accepted. This avoids deliberately starting integration while another planned member is still unready, but can delay a change that would otherwise be safe to merge independently. Separate repository merges still are not atomic; a later failure can leave a partially merged set requiring recovery.

B: permit each member repository to merge when its own applicable code, review, project-contract and merge-policy requirements pass, without adding a whole-change-set readiness barrier by default. Track the task as partially integrated until all required member work and task-level acceptance complete. This supports incremental integration of independently safe changes, but the plan must account for intermediate compatibility and the possibility that later member changes remain blocked.

Shared boundary: neither option can skip combined checks required by the approved contract or weaken DEC-015's manual, automatic and conditional merge policies or repository controls. Only repositories changed by this task belong to its change set; unrelated project repositories do not acquire an arbitrary readiness requirement. Evidence remains bound to applicable candidate, target and contract identities; changed inputs require appropriate revalidation. Neither option promises atomic cross-repository Git operations, automatic rollback of already merged work, or deployment/release authorization. Partial progress and failures remain explicit. Exact change-set membership, readiness evidence, integration order, approval presentation and recovery policy remain unresolved.

Owner answer: initially A, then clarified to support both modes with A as the default and an application setting or project setting to select A/B for the user's workflow. Record an application default with an explicit project override. Resulting decision ID: DEC-039.

### Q-032: First recovery recommendation after a partial cross-repository merge failure

Date: October 17, 2026. Status: answered A; recorded in DEC-040. Depends on DEC-009, DEC-015, DEC-017, DEC-038 and DEC-039.

Question: after confirming that some member changes merged but the remaining sequence cannot proceed, should the leader first prepare a fix-forward plan or a revert plan?

A, recommended: prefer fixing forward. Preserve already merged history and prepare the smallest repair or updated remaining changes that can complete the approved task safely. Revalidate the affected candidate set before resuming the merge sequence. This preserves accepted work and can avoid reversing useful changes, but the partial state may remain while a repair is prepared and verified. If fixing forward is unsafe or not viable, surface that and propose an alternative rather than treating the preference as an obligation to continue.

B: prefer preparing new revert commits for already merged members when viable, then propose a corrected attempt. Validate proposed reverts against the current repository state and applicable project contract. This prioritizes reversing the partial code integration, but can conflict with subsequent work, remove useful accepted changes and fail to reverse external effects. Reverts are new changes subject to gates and authority, not a guaranteed restoration of the entire environment.

Shared boundary: first reconcile actual member repository state; an ambiguous network response is not proof that a merge failed or succeeded. Ordinary incremental progress under DEC-039 B is not itself a partial failure. Pause the affected failed sequence and expose merged, unmerged and blocked members; unrelated work is not automatically stopped. This question chooses the first recovery recommendation, not unconditional execution authority. Recovery actions follow existing work-plan, permission, verification and merge-approval policies, including automatic modes where already authorized. Scope or authority expansion still requires approval. Neither option authorizes history rewriting, force-pushes, automatic rollback of deployments or erasure of evidence. Exact viability rules, recovery budgets, emergency handling and execution mechanics remain unresolved.

Owner answer: A, first prepare a fix-forward plan after reconciling confirmed partial failure, proposing an alternative when forward repair is unsafe or not viable; execution retains existing scope, gates and approval policies. Resulting decision ID: DEC-040.

### Q-033: Fair sharing versus strict project priority for scarce launch capacity

Date: October 17, 2026. Status: answered both, A by default; recorded in DEC-041. Depends on DEC-008, DEC-009, DEC-013, DEC-017 and DEC-027.

Question: when several projects have eligible queued work competing for limited capacity, should new launches share progress across projects or favor the highest-priority project until its eligible demand is satisfied?

A, recommended: share new-launch opportunities fairly across eligible projects, respecting explicit priority weights while avoiding indefinite starvation of lower-priority eligible work. A higher-priority project receives a larger share rather than automatically taking every available slot. This supports several projects making progress, but urgent work can receive less immediate capacity than under strict priority. Exact fairness, weights, tie-breaks and queue accounting remain to be designed and measured.

B: use strict project priority. Give the highest-priority project's eligible queued work the available compatible capacity first; lower-priority projects use capacity when higher-priority work has no eligible demand for it. This focuses resources on the chosen priority, but sustained high-priority demand can leave other projects waiting indefinitely. It does not reserve incompatible or unusable capacity unnecessarily.

Shared boundary: this chooses ordering among eligible new launches, not numerical concurrency limits or permission to expand budgets. Existing scope, spend, account, destination and resource constraints still apply, as does DEC-027's approved-target fallback policy. Work with failed required prerequisites is not made eligible by its priority. This choice does not authorize interrupting already running workers or infer project priority from keyboard focus. Running-work pause/cancellation and emergency controls retain their separate policies. Queue reasons, effective priorities and unavailable or uncertain resource observations must be visible. Neither choice claims a qualified scale, enforceable native-descendant cap or throughput guarantee. Concrete algorithms, settings scopes, capacity measurements and within-project ordering remain unresolved.

Owner answer: support both scheduling modes, A by default, with an exposed setting to choose according to workflow and resources. The same answer explicitly requires broad configurability through readable files and both interfaces. Resulting decision IDs: DEC-041 for scheduling and DEC-042 for the general configuration contract.

### Q-034: First real end-to-end qualification workflow

Date: October 17, 2026. Status: answered C; recorded in DEC-043. Depends on DEC-002, DEC-007, DEC-013, DEC-018 and DEC-042.

Question: which real workflow should anchor the first end-to-end proof and guide the remaining implementation priorities?

A, recommended: fix one reproducible bug in an existing repository. Begin with a demonstrable failure and approved expected behavior; coordinate work across the initial harnesses and independent verification until the repair and test-integrity checks pass. This gives a concrete before/after result for the integrated workflow, but does not by itself qualify feature discovery or greenfield scaffolding.

B: deliver one small feature in an existing repository. Start from a brief, approve measurable acceptance criteria, coordinate implementation and independently verify the result. This exercises more requirements-to-code planning, but needs careful scope to keep the first proof assessable.

C: create a small new application from a product brief. Exercise project creation, acceptance drafting, scaffolding, implementation and verification together. This tests a broad creation journey, but combines more unqualified steps and makes it harder to isolate the first integration failure.

Shared boundary: this selects a first representative qualification scenario, not an exclusive product capability or another workflow setting. It does not abandon the other scenarios, select a real repository or authorize publishing work. The proof must exercise the central conversation, initial harness coordination, shared assets/context, visible state and independent code/test-integrity verification; supporting technical spikes still precede execution as needed. One successful scenario does not qualify all platforms, settings, scales or release scope. Exact repository, task, fixtures, measurements and acceptance remain to be selected.

Owner answer: C. Create a small new application from a product brief as the first complete representative proof. Resulting decision ID: DEC-043.

### Q-035: Application shape for the first creation proof

Date: October 17, 2026. Status: answered B; recorded in DEC-044. Depends on DEC-002, DEC-018, DEC-042 and DEC-043.

Question: What kind of small application should the first product brief describe?

A, recommended: a browser application with local persistence, such as a reading-list manager. Exercise visible user flows, adding and editing records, search and persistence across restart. This provides concrete UI and data behavior to verify without adding account or deployment requirements to this first proof.

B: a small API service with persistent records. Exercise request validation, data behavior and real API integration; browser UI is not the main evidence for this scenario.

C: a small desktop application. Exercise installation or launch, application UI and persistent data, adding packaging and desktop-specific behavior to the first proof.

Shared boundary: this selects the first generated application's shape, not a restriction on CrazyAgent's supported project types or another workflow setting. Browser application does not mean CrazyAgent becomes browser-only; its own primary TUI and desktop direction remain. Examples are illustrative, and no domain, framework, database, packaging system, credential, external deployment or publication is selected by this question. Define the exact brief and measurable acceptance after the owner selects the scenario.

Owner answer: B. Use a small API service with persistent data as the first generated application's shape. Resulting decision ID: DEC-044.

### Q-036: Concrete API proof brief and scope

Date: October 17, 2026. Status: answered A; recorded in DEC-045. Depends on DEC-018, DEC-043 and DEC-044.

Question: Which brief should define the first API creation proof?

A, recommended: a local inventory and reservations API using sample data. Create and list items, reserve and release stock, reject invalid requests, prevent reservations exceeding available stock even under concurrent requests, and preserve data across restart. Keep this first fixture local, without sign-in or external deployment. This provides a concrete business invariant for independent verification.

B: the same inventory and reservations API with user accounts and authenticated access from the first proof. Add explicit ownership and access-control acceptance criteria to the approved brief; the identity mechanism and credential handling remain to be designed.

C: use a different API idea supplied by the owner. Capture a short product brief and agree on a bounded set of measurable acceptance criteria before implementation.

Shared boundary: the fixture's scope is not a restriction on CrazyAgent's supported application types or a framework setting. No proposed API domain or operation is accepted until the owner selects it. Local-only sample data does not authorize weakening CrazyAgent's own security or verification controls. No technology stack, package installation, deployment, external account or publication is authorized by this question.

Owner answer: A. Use the local inventory and reservations API brief, without sign-in or external deployment. Resulting decision ID: DEC-045.

### Q-037: First-release recovery guarantee after an engine crash

Date: October 17, 2026. Status: answered B; recorded in DEC-046. Depends on DEC-004, DEC-007, DEC-009, DEC-017, DEC-019 and DEC-034.

Question: Is durable restart recovery sufficient for the first public release, or must preserving live workers through an engine-only crash also be a release gate?

A, recommended: require durable recovery first. Recover recorded project, task, approval and available activity state; preserve worktrees; reconcile worker and repository state before continuing. Use supported native session continuation where qualified, and make an unavailable continuation or replacement explicit under existing authority. The first release does not claim that workers continue uninterrupted while the engine itself is down.

B: require A plus preserving running worker processes and reattaching to them after an engine-only crash for the launch-qualified harnesses. Treat independent worker supervision, recovery of control and prevention of duplicate dispatch as release-blocking qualifications; if unsupported, report the gap rather than silently claiming this guarantee.

Shared boundary: this is a first-release capability requirement, not a routine preference that can be satisfied by adding a settings toggle. Neither option promises live operating-system processes survive a reboot, logout, termination or machine loss. Both must reconcile uncertain side effects before resending work, preserve authority boundaries and avoid presenting interrupted or unknown work as verified completion. Native resume, child-agent coverage, durable capture bounds, automatic startup and configurable post-recovery execution policy remain separate qualification or design questions. Closing either or both interfaces remains covered by DEC-004 and must not be conflated with an engine crash.

Owner answer: B. Require durable recovery plus live-worker survival and reattachment after an engine-only crash for the first public release. Resulting decision ID: DEC-046.

### Q-038: First-public-release concurrency qualification target

Date: October 17, 2026. Status: answered A; recorded in DEC-047. Depends on DEC-002, DEC-013, DEC-014, DEC-041 and DEC-046.

Question: What concurrent session scale should we target and demonstrate for the first public release?

A, recommended: 10 active managed harness sessions across 3 projects on an agreed macOS test profile.

B: 50 active managed harness sessions across 10 projects on an agreed macOS test profile.

C: an owner-specified target. State the active managed harness-session count and project count.

Shared boundary: these are proposed qualification targets, not observed capacity, hard-coded product caps or default resource settings. One session means one separately launched top-level harness process, including leader or verification harnesses when separately launched; native child agents are tracked separately where observable and do not inherit a proven scale claim. Queued tickets and retained session history do not count as active execution. The actual workload, machine profile, account limits, authorized spend, reliability and resource measurements remain to be agreed and measured. Concurrency settings remain configurable and constrained by real resources and permissions. A chosen number does not establish unlimited child-agent scaling, guarantee the same capacity on every machine or authorize benchmark spend, accounts, installation or execution.

Owner answer: A. Target 10 active managed harness sessions across 3 projects on an agreed macOS profile for first-release qualification. Resulting decision ID: DEC-047.

### Q-039: Native approval handoff in first-release core workflows

Date: October 17, 2026. Status: answered A; recorded in DEC-048. Depends on DEC-006, DEC-007, DEC-009, DEC-011, DEC-035 and DEC-046.

Question: May first-release core workflows require an explicit handoff to a native harness permission prompt, or must supported permission approvals be completed through CrazyAgent's central inbox?

A, recommended: central approval controls where the supported integration exposes them, with an explicit native-prompt fallback for otherwise unsupported permission requests. Show the blocked task and approval source centrally; the user deliberately takes control of the relevant native terminal or provider-supported prompt, then returns to the central workflow. App-owned plan, baseline and merge approvals remain governed by their existing central policies.

B: require central inbox handling for the permission requests needed by the launch-qualified core workflows. An adapter must expose and complete those requests through the supported integration without requiring the user to take over a worker's native prompt. Treat missing support as a qualification gap; block the affected unsupported action rather than bypassing a permission check.

Shared boundary: both preserve the central leader as the normal conversation and task-control interface. This asks about first-release approval integration coverage, not a restriction on future configurable workflows or a demand for all native UI features. Ordinary provider sign-in or browser authentication is a separate compatibility concern. Neither option grants blanket permission, authorizes auto-accepting native prompts, changes existing approval policy or guarantees capabilities the native backend does not expose. Native approval requests and responses must remain tied to the actual worker, requested action and current valid authority, including after recovery. The supported request categories and runtime behavior require qualification.

Owner answer: A. Use central approvals where supported, with an explicit native-prompt handoff when central handling is unavailable. Resulting decision ID: DEC-048.

### Q-040: First-public-release repository trust boundary

Date: October 17, 2026. Status: answered A for first release, with future broader support; recorded in DEC-049 and DEC-050. Depends on DEC-009, DEC-017, DEC-018, DEC-029 and DEC-035.

Question: Which repository trust boundary must the first public release qualify?

A, recommended: qualify user-owned or reviewed projects first. Keep the agreed permission controls, protected verification and test-integrity checks; generated code, test execution and repository content do not become authority to alter those controls. Do not claim the initial release is qualified to contain an intentionally hostile repository.

B: also require intentionally untrusted third-party repositories as a supported launch scenario. Define and qualify the broader containment boundary for repository scripts and generated execution, including access to host files, credentials and network destinations, before making this launch claim.

Shared boundary: this defines the initial qualified threat model, not a setting that changes an unqualified backend into a proven isolation boundary. Neither option authorizes automatic execution merely from opening or importing a project, weakens verification independence, treats repository instructions as permission grants or promises vulnerability-free execution. Both retain controls for generated code and test execution, including preventing coding workers and code under test from altering verification authority. The containment mechanism, explicit threat cases, operating-system coverage and evidence remain design and qualification work. Broader configurable modes remain possible when their capabilities are implemented and qualified.

Owner answer: A for fast initial development; broader third-party and untrusted-repository support targeted for V2 or V3. The owner also requested application and per-execution isolation with limited host, credential, network and resource access. Resulting decision IDs: DEC-049 and DEC-050.

### Q-041: Sandbox runtime prerequisite for the first public release

Date: October 17, 2026. Status: answered A; recorded in DEC-051. Depends on DEC-002, DEC-011, DEC-046, DEC-049 and DEC-050.

Question: May the first public release require a separately installed local sandbox runtime if the qualified isolation design needs one?

A, recommended: permit one documented runtime prerequisite when needed. Prefer reuse of an existing isolation implementation and qualify its real boundaries, native harness compatibility, authentication and resource overhead. A native or bundled path remains possible if it meets the same selected guarantees.

B: require a standalone installation experience. The supported core workflows must work through shipped application components and native operating-system facilities without a separately installed or managed container or virtual-machine runtime. Treat this packaging requirement as part of backend selection and qualification.

Shared boundary: this selects an installation constraint, not a sandbox product or a permission to install a tool now. Existing account support, first-release trust scope, execution controls and crash-survival qualification remain. Neither option turns a missing or weaker backend into a proven stronger boundary through settings. Actual backend choice, licensing, distribution, update ownership, supported profiles and measurements remain open.

Owner answer: A. Permit a documented separately installed local sandbox runtime prerequisite if the qualified isolation design needs one. Resulting decision ID: DEC-051.

### Q-042: Open-source license direction for CrazyAgent's original code

Date: October 17, 2026. Status: answered A; recorded in DEC-052. Depends on DEC-001, DEC-010, DEC-042 and DEC-051.

Question: Which licensing direction should govern CrazyAgent's original code?

A, recommended: a permissive license, proposing Apache-2.0. Prioritize broad adoption and reuse, including commercial and proprietary derivatives under the applicable license conditions. Apache-2.0 includes an express contributor patent grant with defined scope and conditions.

B: strong copyleft, proposing AGPL-3.0. Prioritize source reciprocity for covered distributed versions and modified versions offered for remote network interaction, under the license's actual conditions. Commercial use is still permitted.

Shared boundary: this concerns the project's original code, not ownership of third-party code or an assertion that every dependency is compatible. Review the actual selected code, integration and distribution model before incorporating it; retain required licenses and notices. Merely using CrazyAgent does not automatically apply its license to a user's project; incorporated or generated covered material needs its own provenance and license assessment. No license file or third-party relicense is applied by this question. Alternative license preferences can be supplied explicitly.

Research basis: Checked the license texts on October 17, 2026. [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0) describes copyright and patent grants and redistribution conditions. [AGPL-3.0 text published by the Open Source Initiative](https://opensource.org/license/agpl-3.0) describes covered source obligations and remote network interaction; the GNU-hosted text could not be fetched during this check.

Owner answer: A. Select Apache-2.0 for CrazyAgent's original code, with third-party obligations reviewed separately. Resulting decision ID: DEC-052.

### Q-043: First-release leader scope across projects

Date: October 17, 2026. Status: answered B; recorded in DEC-053. Depends on DEC-007, DEC-013, DEC-021, DEC-037, DEC-038, DEC-041 and DEC-047.

Question: Must a workspace-level AI leader coordinate across projects before the first public release?

A, recommended: project-scoped leader conversations are sufficient for the first release. Use one consistent conversation interface with a separate scope for each project, coordinating its repositories, task teams and harnesses. Keep global project status and resource visibility in the shared application; a workspace-level AI delegation layer is not a launch requirement.

B: also require a workspace-level AI leader in the first release. Allow one conversation to coordinate explicitly selected and authorized projects through their project leaders, while preserving project-specific permissions, memory, verification and execution boundaries.

Shared boundary: this is a launch capability requirement, not a return to per-worker conversations or an exclusive forever choice. Both preserve the single application, central conversation interface, multi-repository project support and configurable scheduling. A global status view is not permission for an AI to read all project content. B would require explicit project and data scope and compatible processing destinations; it would not automatically promote project knowledge to global memory or grant cross-project peer access. Additional separately launched leader processes count within DEC-047's qualification target. Exact account, routing, context and authority mechanics remain to be designed and qualified.

Owner answer: B. Require a workspace-level AI leader that coordinates explicitly selected projects through their project leaders in the first public release. Resulting decision ID: DEC-053.

### Q-044: Native child-agent visibility and control at launch

Date: October 17, 2026. Status: answered B; recorded in DEC-054. Depends on DEC-007, DEC-011, DEC-013, DEC-014, DEC-046, DEC-047 and DEC-053.

Question: What visibility and lifecycle-control coverage must the first release qualify for child agents created inside a native harness?

A, recommended: fully manage separately launched CrazyAgent workers, and expose native child-agent information and controls only to the extent actually supported and qualified by the backend. Label unavailable status, usage or controls clearly rather than claiming complete individual management. Parent or session-level controls must be accurately described.

B: require individual status, cancellation and recovery or continuation-state coverage for native child-agent workflows in the launch scope. If an adapter cannot establish that coverage, do not qualify that native child-agent workflow for launch merely because its parent harness can run.

Shared boundary: a CrazyAgent-launched worker and a child agent created internally by a native harness are different control surfaces. This asks about the launch capability and evidence requirement, not whether settings can manufacture missing native APIs. Both retain the approved hierarchical product direction, scope and resource limits, independent verification, actual isolation and explicit recovery evidence. No hidden native child receives an assumed per-agent credential, worktree, usage figure or crash-recovery guarantee. Mandatory controls cannot be waived because observation is partial. Exact provider capabilities, supported operations, native child coverage and any app-managed alternative remain technical qualification work.

Owner answer: B. Individual native child-agent status, cancellation and recovery or continuation-state coverage are required for native workflows in launch scope. Resulting decision ID: DEC-054.

### Q-045: First-release protection for stored conversations and memory

Date: October 17, 2026. Status: answered A for launch, B required later; recorded in DEC-055. Depends on DEC-020, DEC-021, DEC-024, DEC-046, DEC-049, DEC-050 and DEC-054.

Question: What protection must the first public release provide for CrazyAgent-managed conversations, captured activity and curated memory when stored on disk?

A. Private application storage with restricted OS file permissions, plus separately protected credentials. Application-managed encryption of the conversation and memory store is not a launch requirement. Document the dependency on OS disk encryption and its status where observable, without assuming every device has it enabled. This is the recommended initial scope if the accepted threat model permits it.

B. Application-managed encryption of conversations, captured activity, memory and sensitive derived indexes is a launch requirement. Qualify maintained encryption or storage components, key availability and recovery, crash supervision, and the handling of backups, exports and temporary plaintext artifacts before advertising this capability.

Recommendation: A for the initial release, subject to an explicit threat model and clear storage disclosure. Encryption layers address different threats: application encryption and OS disk encryption are distinct choices, and neither replaces access controls against running workers. If independently encrypted local records are a required product property, choose B and include that qualification in the release forecast. See [OWASP Cryptographic Storage Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Cryptographic_Storage_Cheat_Sheet.html).

Boundary: This question defines required launch capability, not whether settings may eventually support both modes. Both options retain credential protection, worker isolation, independent verification, retention controls and applicable sensitive-data minimization. Repository files and native CLI artifacts outside CrazyAgent-managed storage are separate surfaces; neither option automatically encrypts them. No encryption library, storage backend or key-recovery policy is selected by the question.

Owner answer: A for the first release. B remains required for a later release, with both modes configurable when implemented. Resulting decision ID: DEC-055.

### Q-046: Git hosting integrations required for the first release

Date: October 17, 2026. Status: answered A; recorded in DEC-056. Depends on DEC-001, DEC-015, DEC-017, DEC-038, DEC-039 and DEC-040.

Question: Which Git hosting integrations must qualify before the first public release?

A. Local Git workflows plus GitHub.com integration for hosted pull-request creation, review and check status, and policy-authorized merges. Other hosts can follow in later releases. This is the recommended launch scope.

B. Local Git workflows, GitHub.com and additional Git hosting integrations must qualify at launch. Identify the additional required hosts before planning their adapters and qualification.

Recommendation: A. Keep local repository and verification behavior independent of Git hosting while qualifying one hosted pull-request workflow first. This is a launch integration choice, not a permanent restriction on supported repositories or a setting that can enable an unimplemented adapter.

Boundary: Under either proposed scope, local-only projects do not require a GitHub account. Existing human or configured automatic merge authority and protected verification gates remain binding. No account connection, remote mutation, tool installation or particular API implementation is authorized by this question. The original research discusses Git hosting competitors and requests but does not establish a launch provider contract.

Owner answer: A. Local Git and GitHub.com qualify for launch; additional hosting providers follow later. Resulting decision ID: DEC-056.

### Q-047: Core workflow parity between the TUI and desktop at launch

Date: October 17, 2026. Status: answered A; recorded in DEC-057. Depends on DEC-002, DEC-004, DEC-006, DEC-007, DEC-021, DEC-042, DEC-047, DEC-048, DEC-053 and DEC-056.

Question: Must a desktop user be able to complete the same core development workflow as a TUI user without opening the TUI in the first public release?

A. Yes. Both interfaces independently support the complete core workflow: create and configure projects, give the leader a brief, review plans and approvals, launch and supervise work, inspect verification evidence and proposed changes, use the memory review inbox, and carry out permitted merge actions. This is the recommended scope because the desktop is intended for people who are not comfortable using a TUI.

B. The TUI supports the complete core workflow at launch, while the desktop initially focuses on monitoring and approval actions. Users may need the TUI for project setup, initiating work or configuration until later desktop releases.

Recommendation: A for the core workflow. Share engine behavior and state while allowing each interface its own layout and interaction model. This defines launch capability and qualification scope, not a requirement to make every advanced inspector or native CLI surface identical.

Boundary: Both interfaces remain required products. DEC-048's explicit native provider approval fallback remains permitted where an adapter cannot centralize a particular provider prompt. That qualified fallback does not excuse missing application-owned project, leader, configuration, memory review or verification controls. No UI framework or implementation mechanism is selected by this question.

Owner answer: A. Both interfaces independently support the complete core workflow at launch. Resulting decision ID: DEC-057.

### Q-048: Interface framework baseline

Date: October 17, 2026. Status: answered A; recorded in DEC-058. Depends on DEC-002, DEC-004, DEC-046, DEC-054 and DEC-057, and the technical synthesis for Q-048.

Question: Adopt Ratatui with Crossterm for the TUI and Tauri 2 with React and TypeScript for the desktop, both using the shared Rust engine?

A. Adopt this interface framework baseline for the detailed design, subject to platform, accessibility, security and runtime qualification. This is the recommended choice.

B. Compare or change an interface framework before adopting the baseline. Identify the concern or preferred alternative next.

Recommendation: A. It follows the owner's Rust and Tauri direction and keeps both interfaces connected to one execution authority. It preserves native terminal components as supplementary tools and leaves room to revise a dependency if qualification reveals a concrete blocker.

Boundary: This choice selects interface frameworks for the detailed design. It does not approve a complete implementation design, pick the sandbox backend, establish native child-agent recovery, or prove every future OS is supported. The remaining recommendations in the technical synthesis are not silently accepted by an A answer.

Owner answer: A. Adopt Ratatui/Crossterm for the TUI and Tauri 2 with React/TypeScript for the desktop around the shared Rust engine. Resulting decision ID: DEC-058.

### Q-049: Full architecture and qualification direction

Date: October 17, 2026. Status: answered A; recorded in DEC-059. Depends on DEC-001 through DEC-058 and the completed architecture builder check.

Question: Adopt the written architecture and qualification proposal, including P-01 through P-11, as the baseline for the detailed implementation plan?

A. Adopt the proposal for implementation planning. This is the recommended choice.

B. Revise the architecture before adoption. Identify the concern to resolve next.

Recommendation: A. Use one authoritative Rust engine with separate surviving session runners; connect both approved interfaces to the same versioned local protocol; use SQLite for durable operational state and canonical versioned configuration files, with TOML as the proposed native format; integrate RCLI and native Claude/Codex capabilities through explicit adapters; keep routing, permissions, memory scope and verification decisions in the engine. Qualify an existing execution sandbox before admitting workflows, with Anthropic sandbox-runtime as the initial candidate rather than an assumed working solution.

Qualification order: prove original live-worker recovery and protected execution boundaries, then independently prove the inventory/reservations application workflow through each interface, then qualify shared coordination, GitHub behavior and the 10-session/3-project target. Reforecast after the first 16 working hours. Preserve all accepted launch requirements; a failed gate triggers a documented replacement or scope/date decision rather than an implicit downgrade.

Boundary: Adoption selects the implementation direction and qualification plan. It does not assert that runtime capabilities have passed, finalize compatible dependency versions, authorize installing tools or signing into providers, authorize unapproved provider spending, or publish a repository. The detailed implementation plan follows the owner's answer. Previously accepted decisions remain authoritative.

Design: [Architecture and qualification proposal](superpowers/specs/2026-10-17-crazyagent-architecture-design.md).

Review: [Builder check and frozen target](superpowers/specs/2026-10-17-crazyagent-builder-check.md).

Owner answer: A. Adopt the full architecture and qualification proposal as the implementation planning baseline. Resulting decision ID: DEC-059.

### Q-050: Implementation plan and execution method

Date: October 17, 2026. Status: answered B with explicit development-phase amendments; recorded in DEC-060 through DEC-062. Depends on DEC-059 and the written implementation plan.

Question: Does the implementation plan capture the adopted design, and which execution method should implement it?

A. Adopt the plan and use native execution. One implementer carries the sequential tasks. A fresh normal reviewer supplies the frozen code-review track alongside the required Codex Security track; no third track is added. This is the recommended choice.

B. Adopt the plan and use subagent-driven execution. Fresh implementers and reviewers work task-by-task in isolated worktrees, with additional context and coordination cost and the same final review gates.

C. Revise the plan before execution. Identify the concern to resolve.

Recommendation: A for the first qualification and shared engine/runner contracts because those tasks depend closely on one another. The plan keeps all accepted launch requirements and the first 16 working hours forecast checkpoint.

Boundary: An A or B answer authorizes starting source implementation under the selected method. It does not authorize tool installation, account connection, unapproved provider spending, external test PR creation or publication. First produce the concrete prerequisite/profile manifest and request only missing authorization.

Plan: [Integrated application implementation plan](superpowers/plans/2026-10-17-crazyagent-implementation-plan.md).

Owner answer: Subagent-driven implementation in visible Herdr panes, led from this conversation. Initialize the repository and project files, notify when setup is ready, then begin implementation. Skip code and security reviews for now; use real tests with no mocks or bypasses. Resulting decision IDs: DEC-060, DEC-061 and DEC-062.

### Q-051: Narrow Herdr session exception

Date: October 17, 2026. Status: answered A; recorded in DEC-063.

Question: May the leader control only the existing CrazyAgent workspace w11 from this session despite the installed skill's missing-HERDR_ENV guard, creating labeled worker panes while preserving focus and all other workspaces?

A. Allow targeted CrazyAgent pane control from this session. This is the recommended option.

B. Wait for an attached Herdr session before launching visible workers.

Owner answer: A. Allow targeted CrazyAgent pane control from this session. Resulting decision ID: DEC-063.

### Q-052: Codex worker folder trust

Date: October 17, 2026. Status: answered A; recorded in DEC-064.

Question: May the leader select Trust and continue in both worker panes, saving trust for the CrazyAgent repository while retaining the workspace sandbox?

A. Trust CrazyAgent and start both workers. This is the recommended option.

B. Leave both workers paused.

Owner answer: A. Trust CrazyAgent and start both workers. Resulting decision ID: DEC-064.

### Q-053: P1 delivery boundary

Date: October 17, 2026. Status: answered A; recorded in DEC-066.

Question: Does P1 mean the complete first public release V1, or the initial runtime qualification checkpoint?

A. V1: the complete first public release in the accepted plan.

B. Initial checkpoint: Tasks 1-4, including native isolation/recovery proof and the first forecast.

Owner answer: A. V1: the complete first public release in the accepted plan. Resulting decision ID: DEC-066.

### Q-054: Project-local dependency acquisition

Date: October 17, 2026. Status: answered A; recorded in DEC-068.

Question: May the leader acquire proposed sandbox-runtime 0.0.80 locally, resolve and lock required V1 packages from official registries, and run disposable local boundary probes, with no global/system changes, provider calls, credentials or publication?

A. Allow project-local runtime and locked V1 dependency acquisition.

B. Allow only sandbox-runtime 0.0.80 and its locked dependencies.

C. Keep all work offline; the owner will install missing dependencies.

Owner answer: A. Allow project-local runtime and locked V1 dependency acquisition. Resulting decision ID: DEC-068.

### Q-055: Bounded Docker resource-boundary prototype

Date: October 17, 2026. Status: answered A; recorded in DEC-069.

Question: May the leader run the bounded Linux resource-boundary prototype in the existing Docker Desktop VM using a pinned official image, one disposable container at a time, at most 128 MiB RAM, 0.5 CPU, 16 PIDs and 15 seconds per probe, without credentials, provider calls, privileged containers, host sockets or changes to other containers?

A. Authorize the bounded Docker prototype.

B. Continue investigating a macOS-native boundary.

Owner answer: A. Authorize the bounded Docker prototype. Resulting decision ID: DEC-069. Permanent runtime adoption remains a later decision.

### Q-057: One boundary-only topology diagnostic

Date: October 18, 2026. Status: pending owner response. Proposal: docs/qualification/linux-topology-diagnostic-proposal.md, frozen source 8cfbf15, with twenty-four passing leader-native helper methods. Scope: one invocation, at most one boundary container, the unchanged pinned image/profile, 64 MiB RAM, 0.5 CPU, sixteen PIDs, fifteen-second lifetime and twenty-minute total allocation, synthetic mounts only, no credentials/providers/host sockets/privileges/unrelated workloads. Preserve original evidence and separate exclusive marker; no retry or permanent runtime adoption. DEC-071 requires the first failed experiment to stop; this is a separate proposed attempt. Independent Task 8 continues while approval is pending. No new execution authority is inferred until the owner answers.
### DEC-072: Authorize exactly one bounded topology diagnostic

Date: October 18, 2026. Task: Q-057 and the retained DEC-071 failure. Status: owner explicitly selected A, approve this one-container diagnostic. Topic: separate development-only native execution authority.

Goal: retain actual interface/route observations before the unchanged strict network expectation, without retrying the original failed experiment or adopting a runtime.

Constraints: the concrete proposal is docs/qualification/linux-topology-diagnostic-proposal.md, frozen worker source 8cfbf15, integrated as ecc533c. All twenty-four leader-native helpers passed. One invocation and at most one newly created boundary container using the existing pinned image and Docker Desktop endpoint. Keep 64 MiB RAM, no extra swap, 0.5 CPU, sixteen PIDs, fifteen-second container lifetime, twenty-minute total allocation, existing kill deadlines, finite output/storage, exact identity checks and cleanup. Mount only fresh synthetic candidate/protected/payload paths; no credentials, provider/model calls, host socket mounts, privileges, other workloads or permanent Docker/configuration changes.

Decision: approve the exact boundary-only diagnostic command and its internal preflight from that frozen worktree. The leader owns native execution and the Docker slot. No worker runs Docker. Use the distinct exclusive linux-topology-diagnostic-attempt.json marker and preserve the original source, attempt marker, report and fixture. Stop after one case or any earlier failure/unavailable/cleanup uncertainty. Do not automatically retry or proceed to the other four resource cases. A successful diagnostic is distinct from five-case qualification and leaves native admission and permanent runtime selection false.

Owner evidence: the owner answered the selectable Q-057 card with A. Approve this one-container diagnostic. The card explicitly retained all limits and stated that independent Task 8 work continues either way. This supersedes Q-057's earlier pending status; no earlier gate is reclassified as passed.

Rejected alternatives: automatic rerun under DEC-071, relaxing the loopback-only expectation, a full five-case rerun, global Docker changes, adding host privilege or using an account/provider. These are outside this approval.

Risks: actual topology may still fail the expectation; names/routes alone establish neither useful connectivity nor its denial. A timeout or unavailable daemon remains a failed/unavailable result. Retain every observation and confirmed cleanup without overstating isolation. The prepared code/security reviews are deferred under DEC-061, not passed.

Acceptance criteria: exactly the selected boundary case at most, source/binary/profile provenance, bounded topology captured before validation, retained primary traceback, unchanged canaries and actual host engine state, exact container stop/removal/absence and owned engine/socket cleanup. Any missing evidence, overflow, deadline or unconfirmed cleanup fails. Record the actual result and outstanding native/profile gaps before any later runtime decision.

Unresolved questions: topology cause and profile compatibility await this observation. Full combined containment, aggregate CPU/disk budgets, native harness authentication/control/recovery and permanent runtime adoption remain unqualified and are not authorized by this decision.
