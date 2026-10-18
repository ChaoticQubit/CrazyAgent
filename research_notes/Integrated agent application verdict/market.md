# Integrated agent application: market and workflow reassessment

Research date: October 16, 2026. Read the entire original research document and the corrected scope in `docs/agent-decisions.md`. The owner already runs the separate capabilities. This reassessment evaluates one project application, including multiple native harnesses, shared global/project setup and memory, communication, accounts per harness, and a verification authority that checks both project code and test integrity while keeping its trusted rules outside coding-harness control. No competitor was installed, no account inspected, and no live workflow measured.

## 1. Does the complete integrated application have value?

### Takeaway

**One-word verdict: Promising. Recommendation: build the integrated application, conditional on demonstrating the whole workflow quickly.** The correct question is whether one application reduces project coordination and verification effort. The fact that individual components exist does not answer that question. However, several serious applications already integrate much more of this workflow than a terminal launcher, so integration alone is an insufficient differentiation claim.

### Cited findings

Databricks describes its own users keeping four or five agents open and copying information between agents and collaboration tools. It created Omnigent above existing harnesses specifically to combine composition, policies, collaboration, and cross-device access. This is first-party evidence that the integration problem exists in at least one sophisticated engineering organization, not proof of broad adoption or that CrazyAgent is better. [Databricks announcement, June 13, 2026](https://www.databricks.com/blog/introducing-omnigent-meta-harness-combine-control-and-share-your-agents)

Traycer's application groups agents, workspace folders, artifacts, terminal sessions, browser tabs, diffs, and PRs inside a Task. A Task can cover several folders. Its README explicitly markets common memory and preserved context across models/providers. These are existing integrated-product claims, not separate-tool recipes. The detailed documentation supports shared artifacts and conversation context, while automatic long-lived project-memory consistency still needs qualification. [Task model](https://docs.traycer.ai/concepts/tasks-and-workspace-folders.md), [Traycer README](https://github.com/traycerai/traycer/blob/main/README.md)

Superset documents a single workspace with CLI agents, review, previews, account profiles, usage, and programmatic agent coordination. JetBrains Air positions itself as one system across projects, agents, review, deployment environments, and centralized tools. The direction of competition validates the product category and raises the execution standard simultaneously. [Superset application journey](https://github.com/superset-sh/superset), [Air product](https://www.jetbrains.com/air/)

### Inferences

The owner's insight is sound: composing separately working tools into a coherent project experience can itself create value. A single project should own setup, durable decisions, harness activity, and verification evidence, rather than requiring the user to reconstruct that relationship across tools.

The stronger opportunity is an integrated **project completion contract**: each change has an accountable author, shared context, an exact revision, and verification results whose rules the author cannot rewrite. This remains an application experience. It does not require inventing a new test framework or reducing the product to a verifier CLI.

The initial public release should demonstrate create project → apply global/project context → run Claude and Codex → exchange a meaningful message/result → independently verify their combined change → show what passed and what remains unresolved, all from the app. Releasing the launcher first would repeat the owner's existing workflow without testing the thesis.

### Gaps

No evidence currently establishes that users prefer this proposed application to Traycer, Omnigent, Air, or Superset. No setup-time, attention-cost, completion-quality, or retention measurements were made. The verdict is a reasoned build hypothesis, not measured market validation.

## 2. Which applications already integrate the relevant workflow?

### Takeaway

Traycer is the closest visible application-experience competitor. Omnigent is the strongest permissive runtime/governance competitor and possible base. Superset's current account and orchestration features materially exceed the older matrix. Air is a serious omitted distribution threat. None of the sources examined establishes the exact protected code-and-test verification authority the owner requests, but that absence in this review means **unknown**, not a proven missing feature.

### Cited findings

Evidence labels: **D** = documented integrated experience; **I** = configured integration or narrower behavior; **U** = not established by the examined sources; **M** = measured. There are no M cells. A D cell reports documentation, not verified behavior. Shared files, readable transcripts, a common model conversation, and a consistent multi-session project-memory service are distinct contracts.

| Application | Project journey | Shared memory/context | Global/project skills, plugins, rules/settings | Harness communication and accounts | Independent verification |
|---|---|---|---|---|---|
| **Traycer Desktop** | D: Task holds folders, agents, artifacts, diffs, terminals and PRs | D: shared-memory/context claims, durable artifacts and transcript access; U: consistency/conflict semantics across concurrent project memory | D: Global/workspace skills, shared or provider-only skills, scoped plugins; I: provider support differs | D: cross-agent communication, multi-account Claude/Codex profiles; I: Codex Terminal can send but cannot receive | D: review artifacts and separate evaluator workflow; U: coding-harness-proof test rules and independent evidence execution |
| **Omnigent** | D: native desktop/web/terminal around server/runner, mixed harnesses in one session, projects in current releases | D: session context, side chats, shared sessions; I: Hindsight memory extra; U: one automatic global/project memory contract for every native harness | D: portable agent instructions/tool specs and policies; I: harness-specific semantics, native ambient configuration; U: all plugins/settings identical | D: mixed-harness delegation/cross-vendor review; I: vendor CLI credentials and per-agent defaults; U: full per-harness multi-account UX | D: policy enforcement and Polly cross-review; U: protected project-wide test-integrity authority |
| **Superset** | D: repository → workspace → mixed CLI agent → changes/tests/browser → PR | D: terminal history/handoff; U: a shared persistent project-memory service | D: bundled skills and shared setup across profiles; U: canonical cross-harness memory/config consistency | D: bundled orchestrator uses CLI create/read/send; D: Claude/Codex account profiles | D: tests/review in journey and coordinator checks; U: protected verifier rules or independent test-integrity authority |
| **JetBrains Air** | D: several projects with agent sessions, IDE diffs/review; team cloud environments | D: task context and project semantic index; U: shared persistent multi-harness memory | D: existing agent project files plus `.air` settings/MCP; I: retains agent-specific config | D: multi-agent sessions and access choices; U: arbitrary peer messaging and multiple identities per individual harness | D: separate agentic review; U: trusted rules inaccessible to coding agents and test-tampering checks |
| **Emdash** | D: local/SSH project → task worktree → CLI agent → diff/CI/PR/merge | U: shared persistent memory in reviewed docs | I: detects native CLIs and installs lifecycle hooks; U: all requested shared config | D: many parallel native agents; U: cross-harness peer messaging and per-harness multi-account UI | D: CI inspection/review; U: protected project verifier |
| **Vibe Kanban** | D: issues → workspaces → coding agents → review/preview → PR | U: persistent shared memory in reviewed docs | U: canonical shared setup in reviewed docs | D: 10+ agents; U: peer communication/account management | D: review and checks; U: protected verifier; project explicitly sunsetting |

Traycer sources distinguish the actual integration boundaries. Skills can live once in a shared location or in a provider-specific directory, at global or workspace scope. Profiles keep sign-ins separate; Claude profiles share settings/history and optionally skills/plugins, while Codex profiles retain separate configuration copies. Plugin changes require restarting running agents. [Skills](https://docs.traycer.ai/settings/providers/skills.md), [Providers and profiles](https://docs.traycer.ai/settings/providers)

Traycer's agent communication is unusually close to the owner's requirement: all Chat agents can send and receive; Claude Terminal agents can too; Codex/OpenCode Terminal agents can send but have no inbox. Transcripts and messaging have separate access/availability rules. This is a documented native-interface limitation worth testing before claiming transparent communication. [Agent-to-agent capability matrix](https://docs.traycer.ai/concepts/agent-to-agent.md)

Traycer also has a builder/separate-evaluator workflow in `traycer-autobuild`, with a user-approved spec and rubric. Ordinary review starts another agent and produces a review artifact. The docs say these skills are instructions, not an enforced pipeline. This directly prevents claiming that merely having a separate evaluator agent is novel; the potential difference is protected authority, evidence execution, and test-integrity enforcement. [Artifact workflow](https://docs.traycer.ai/concepts/artifact-workflow.md), [Review workflow](https://docs.traycer.ai/guides/review-agent-changes.md)

Omnigent provides mixed native harnesses, server/runner execution, OS/cloud sandboxes, credential choices and common policies. Its current stable release is v0.18.0, published October 13, 2026. Memory has an optional Hindsight integration. The original research's v1.4-era Traycer snapshot is also stale: Desktop 1.5.1 was published October 10 and the current changelog includes profile synchronization and completed native-subagent continuation. [Omnigent README](https://github.com/omnigent-ai/omnigent), [v0.18.0](https://github.com/omnigent-ai/omnigent/releases/tag/v0.18.0), [Traycer 1.5.1](https://github.com/traycerai/traycer/releases/tag/desktop-v1.5.1), [Traycer changelog](https://docs.traycer.ai/changelog)

Omnigent's Polly review skill uses a fixed diff snapshot and exact SHAs, deterministic tests/lint/typecheck, then a different-vendor reviewer given only the diff and contract. This is meaningful independence of information and model; the examined skill is still a workflow instruction, not proof that a coding agent cannot alter a privileged verification service. [Polly cross-review source](https://github.com/omnigent-ai/omnigent/blob/main/examples/polly/skills/cross-review/SKILL.md)

Omnigent's open issue #1352 requests one centralized, placement-aware MCP configuration across native sessions and hosts, identifying ambient native MCP configuration and duplicated processes. API read-back on October 16 confirmed the issue remains open, last updated September 2. This establishes an explicit outstanding request, not proof that no related implementation has landed elsewhere. [Issue #1352](https://github.com/omnigent-ai/omnigent/issues/1352)

Superset explicitly supports multiple Claude/Codex logins with shared skills, plugins, MCP and settings, while credentials remain per account. A running agent retains its account until relaunched. Its bundled orchestration skill uses workspace creation, agent creation, terminal reads and sends to coordinate any supported mix of workers. [Usage and profiles](https://docs.superset.sh/usage), [Orchestration](https://docs.superset.sh/orchestration)

Air's shared workspace configuration includes agent instruction files, skills, commands, `.air/mcp.json`, worktree/Docker configuration and review prompts. Its review can use a different agent/model, and review guidelines are a project file. The examined docs do not establish an externally protected verifier. Air also differentiates Claude subscription access through its native terminal surface from Claude Agent API/JetBrains access. [Settings](https://www.jetbrains.com/help/air/settings.html), [Agentic review](https://www.jetbrains.com/help/air/agentic-review.html), [Air in IDEs overview](https://www.jetbrains.com/help/air-ides/air-overview.html)

Emdash documents native CLI detection, a task worktree and branch, local or SSH execution, review/CI/PR integration and local SQLite app state. Vibe Kanban documents a comparable issues-to-PR journey but explicitly announces sunset; its company explains the inability to find a business model despite usage. This warns about maintenance economics, not against an explicitly noncommercial open-source project. [Emdash README](https://github.com/generalaction/emdash), [Vibe Kanban README](https://github.com/BloopAI/vibe-kanban), [Company shutdown announcement](https://www.vibekanban.com/blog/shutdown)

### Inferences

The closest copying threat is Traycer: its shared setup, peer communication, profile support, artifacts and builder/evaluator loop put it near the requested journey. The strongest broad platform threat is Omnigent, with runtime/policy/sandbox infrastructure and a permissive community, while Air can distribute adjacent functionality to existing IDE users. Those are strategic inferences, not measured copying probabilities.

The useful differentiation is not “we have two agents” or “we have another reviewer.” It is “this project has one understandable state and a completion result backed by checks that the coding agents cannot rewrite.” It must remain easy to configure and inspect from the same application.

### Gaps

Full plugin parity, task-state migration, native account isolation, auth-policy compliance, and verifier tamper resistance remain unmeasured. Unknown competitor cells must not become red “absent” cells in a launch comparison. Main-branch code and current docs can also be ahead of stable releases; version-pinned qualification remains necessary.

## 3. What should be built, who should adopt it first, and can an existing app be the base?

### Takeaway

Build a coherent application for experienced developers already using Claude and Codex on the same project. Qualify a permissive existing application foundation before writing a new shell. Reusing an app can satisfy the owner if the result has one project UX and independently controlled verification; a recipe of separately managed tools cannot.

### Cited findings

The best permissive GUI-base candidate examined is **Emdash**: the full repository is Apache-2.0, and its desktop package contains Electron packaging, native PTY handling, local SQLite, SSH, ACP, and internal core/plugins/shared/wire packages. This supports evaluating it as an application foundation rather than just embedding another CLI. Buildability and extension effort were not tested. [License](https://github.com/generalaction/emdash/blob/main/LICENSE.md), [Desktop package](https://github.com/generalaction/emdash/blob/main/apps/emdash-desktop/package.json)

**Omnigent** is the stronger runtime/governance-base candidate. Its Apache-2.0 repo includes the Python runtime/server and React web interface, and the product documents a native desktop surface over that interface. The repository also includes SDKs, deployment paths, agent examples and policies. Adding a project verification authority could reuse meaningful infrastructure, with the cost of a larger Python/web/native system to maintain. This is a feasibility inference, not an implementation estimate. [License](https://github.com/omnigent-ai/omnigent/blob/main/LICENSE), [Runtime manifest](https://github.com/omnigent-ai/omnigent/blob/main/pyproject.toml), [Web package](https://github.com/omnigent-ai/omnigent/blob/main/web/package.json), [Policy source](https://github.com/omnigent-ai/omnigent/blob/main/docs/POLICIES.md)

**Traycer is not automatically a complete reusable open-source base.** Its MIT license is real, but CONTRIBUTING describes the published repo as clients, CLI and protocol. The tree contains GUI/Electron clients; the CLI trusts signed released hosts. No host implementation was present in the examined root/clients tree. Therefore the client can be a reuse candidate, but independent runtime availability and redistribution require qualification. A clean source build of the UI alone does not prove that the complete application is self-hostable. [License](https://github.com/traycerai/traycer/blob/main/LICENSE), [Contributing and repository layout](https://github.com/traycerai/traycer/blob/main/CONTRIBUTING.md), [Repository tree](https://github.com/traycerai/traycer)

Superset's repository explicitly uses Elastic License 2.0 and describes restrictions on reselling the hosted product. It should be evaluated as source-available with its actual conditions rather than assumed equivalent to the MIT/Apache candidates or treated as an automatically suitable OSI open-source foundation. [Superset license statement](https://github.com/superset-sh/superset#license--whats-free-forever), [License file](https://github.com/superset-sh/superset/blob/main/LICENSE.md)

Current public API snapshots show interest in the category: Omnigent 10,847 stars, Superset 15,275, Emdash 6,000, Traycer 1,616 and Vibe Kanban 28,378. These are October 16 snapshots and must not be called active users, retention, demand for this specific project, or proof of quality. [Omnigent API](https://api.github.com/repos/omnigent-ai/omnigent), [Superset API](https://api.github.com/repos/superset-sh/superset), [Emdash API](https://api.github.com/repos/generalaction/emdash), [Traycer API](https://api.github.com/repos/traycerai/traycer), [Vibe Kanban API](https://api.github.com/repos/BloopAI/vibe-kanban)

### Inferences

First-adopter hypothesis: solo maintainers and small teams already spending time coordinating native Claude/Codex sessions, skills, memory and accounts, especially those who delegate implementation and review but cannot quickly tell which results are trustworthy. The owner is a credible first design partner because the integration problem comes from an existing workflow, not an imagined future workflow. Willingness of others to adopt remains unverified.

Qualification order: first inspect whether Omnigent can supply the complete app/runtime spine without recreating shared-state UX; compare Emdash if owning a simpler local desktop application is more practical. Choose one coherent base. Combining several whole applications risks recreating the configuration problem inside the product. Traycer deserves the best competitor comparison even if its runtime source boundary makes it a poor fork base.

The most important early experiment is workflow-level: give the app and the owner's current separate-tool workflow the same multi-harness project change, interruption, contradictory context update and weakened test. Compare actual human interventions, setup repair, valid completion and truthful verification results. Include at least one close integrated competitor in the next qualification, because beating only a deliberately clumsy setup would overstate value.

A useful innovative addition would be an evidence view attached to every change: which harness wrote it, which shared decisions it used, what the independent verifier actually executed, the exact revision covered, and which checks are stale after new edits. This makes the “single application” advantage visible while giving the independent layer an actual job. A project memory view that shows conflicting assertions with provenance would likewise improve coordination without pretending every harness has an identical context window.

Build rapidly through complete integrated slices. Hold the November expansion conditional on the core journey reducing manual coordination and retaining a trustworthy completion signal. A November beta with supported boundaries is defensible; promising transparent universal plugin/runtime/account portability is not supported by this research.

### Gaps

No foundation was built or installed. A license-level candidate is not a qualified redistribution, trademark, dependency-license, updater, credential, or security assessment. Source-build and local-only operation must be checked on a pinned revision before committing to a fork. No external pilot recruitment or proof of repeat use exists yet. Research cannot establish that all fifteen original features can reach their strongest interpretation within six weeks of one builder's time.
