# CrazyAgent: technical and evidentiary validity

## 1. What is the idea, and can its central promises be delivered?

### Takeaway

**Good discovery research; an overcommitted specification.** The user has a coherent problem: operating several coding agents should require less manual setup, handoff, recovery and checking. A small open-source project can improve this. The complete promise of identical sessions, settings, plugins, live processes and credentials across arbitrary harnesses and machines is not established by the research and should not be the release contract.

Audit date: 2026-10-16. I read the complete 1,265-line source, approximately 14,719 whitespace-delimited words, after querying the project graph. This audit samples material technical, research and policy claims. It is not an exhaustive validation of all 308 bibliography links. No provider accounts were accessed, software installed, or competitor behavior executed. Official documentation establishes documented behavior; README examples and self-reported evaluations remain author claims.

The five main takeaways are:

1. Keep the goal of reducing operational friction, but define exactly what survives a handoff. Information preservation, runtime continuation, environment recreation and credential portability need separate contracts.
2. Automatic subscription rotation cannot be a universal feature. Current OpenAI Sign in with ChatGPT terms directly conflict with enabling rotation to bypass limits; provider-native login and permitted API billing are separate integration paths.
3. A trustworthy checkpoint with explicit loss, permissions and unfinished-work reporting is technically more credible than a claim that any agent can resume any session without loss.
4. Independent verification can detect weakened tests and verify particular outcomes. It cannot certify arbitrary tests as correct or eliminate the need for a trustworthy specification and oracle.
5. The next useful evidence is a controlled comparison against native resume/import and existing handoff tools. More competitor stars will not resolve feasibility or usefulness.

### Cited Findings

The actual idea is an open-source operational layer above existing harnesses, rather than a new model or a company. The owner explicitly values integration and quick release, lists fifteen features, and leaves the first component, supported harnesses and supported operating systems unresolved. The scope expanded on October 15 to native Mac UI inspection, test verification and automatic task-model routing. These are owner requirements, not externally validated demand. [Source document, decisions and feature list](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:54), [unresolved release scope](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:98).

#### Material-claim ledger: portability and runtime

| Source location | Claim | Disposition | Primary evidence and consequence |
|---|---|---|---|
| D1-D2, F1-F4, lines 58-59 and 80-83 | Any harness can run inside one layer with the same session, memory, settings and extensions | **Speculative as a universal promise** | ACP defines optional lifecycle capabilities and agent-owned sessions; it does not specify interchangeable runtime state. [ACP session setup](https://agentclientprotocol.com/protocol/v1/session-setup). A finite adapter matrix is feasible; arbitrary semantic equivalence is not demonstrated. |
| §5.2, lines 163-177 | Skills and MCP are portable, while hooks, agents and permission semantics differ | **Verified in substance** | Agent Plugins 1.0 standardizes Skills and MCP server configuration. Hooks, agents, commands and rules lack common semantics; runtime permissions, sandboxing and trust remain client-owned. [AAIF 1.0 announcement, Aug 6, 2026](https://aaif.io/blog/from-skills-and-tools-to-portable-agent-plugins). |
| §5.2, lines 163-166 | A common SKILL.md format solves common skills | **Partially verified; runtime equivalence overstated if inferred** | The spec defines a file format, compatibility metadata and experimental allowed-tools. Clients may support the latter differently; dependencies can still be required. [Agent Skills specification](https://agentskills.io/specification). |
| §5.3, line 201 | Codex imports Claude/Cursor sessions and other configuration | **Verified** | Official 0.145.0 release notes explicitly expand `/import` to settings, MCP servers, plugins, sessions, commands and project memories. This supports the document's nuance and weakens a blanket claim that switching always means losing everything. [Codex 0.145.0 release](https://github.com/openai/codex/releases/tag/rust-v0.145.0). It does not establish lossless two-way execution. |
| §5.3, line 220 | ACP load replays an agent's history rather than defining portable cross-agent history | **Verified** | `loadSession` must be advertised; load uses session ID, working directory and MCP servers, then replays updates. Resume also requires an advertised capability and restores that agent's context. [ACP session setup](https://agentclientprotocol.com/protocol/v1/session-setup). |
| P3/D4/F6 and §5.5 | A wrapper can pause and later resume subagents and background work | **Unproven; conversation resume is insufficient** | Claude's own resume documentation says unfinished calls are cut off, background Bash/monitor tasks are not restored, and launch flags can need passing again. [Claude sessions](https://code.claude.com/docs/en/sessions). A parent wrapper does not thereby gain resumable state for an opaque child process. |
| §5.5, lines 279-290 | Claude automatically continues after a usage reset, with exceptions | **Verified but incomplete snapshot** | Official docs confirm interactive subscription auto-continue from v2.1.234, two automatic re-arms, permission prompts and exclusions. They also describe manual continuation after computer sleep exceeding about 30 minutes when the reset happened during sleep, an omitted caveat. [Claude interactive mode](https://code.claude.com/docs/en/interactive-mode#wait-for-a-usage-limit-to-reset). |
| D5/F8 and §7 | Sandbox is one yes/no capability | **Overstated** | Omnigent documents Windows Job Object process/resource containment without filesystem/network isolation; native wrappers are unavailable there. [Omnigent README, Windows section](https://github.com/omnigent-ai/omnigent#windows-native). Claude describes separate filesystem/network restrictions and limits. [Claude sandboxing](https://code.claude.com/docs/en/sandboxing). |
| D8/F10, lines 65/89 | One binary and login recreates all sessions, environments, configuration and accounts immediately | **Speculative** | The source itself records absolute-path, version, credential, dependency and concurrency problems in §5.7, lines 404-454. Native resume also omits some runtime state and configuration. [Source constraints](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:449), [Claude sessions](https://code.claude.com/docs/en/sessions). |
| §5.9, lines 539-544 | Native Mac element inspection has OS sandbox and accessibility constraints | **Verified in substance** | Apple DTS says a sandboxed app cannot use `AXUIElementCreateApplication` to inspect another application's AX object. Electron documents explicitly enabling its accessibility tree through `AXManualAccessibility`. [Apple DTS, May 2024](https://developer.apple.com/forums/thread/756130), [Electron accessibility](https://www.electronjs.org/docs/latest/tutorial/accessibility). These are constraints, not proof that every app exposes useful element data. |

### Inferences

The feasible architecture has several boundaries. These are engineering judgments grounded in the protocol and lifecycle evidence above, not a completed design:

| Boundary | Credible contract | Contract to avoid |
|---|---|---|
| Project information | Versioned decisions, tasks, evidence, selected transcript and source IDs; conflicts remain visible | One silently merged memory that is automatically correct for every concurrent agent |
| Handoff | Destination receives selected context, repository state and explicit omissions; native import/resume used when available | The original execution continues in another harness with identical internal state |
| Process recovery | Record completed and unfinished operations, restart supported tasks, require idempotency for external effects | Keep arbitrary network calls, child agents, shells and locks alive after a harness kills them |
| Configuration | Generate supported target settings with a preview and loss report; preserve target-specific policy | Convert file syntax and assume the permission/security semantics survived |
| Environment | Record prerequisites, versions, paths and required secrets; check readiness at destination | One application binary replaces all language runtimes, dependencies, git, OS capabilities and per-host approvals |

A checkpoint should preserve repository identity, base/head and dirty-work evidence, task decisions, files touched, authoritative check commands, result provenance, pending side effects and supported target capabilities. Hashes establish artifact identity and tampering evidence, not whether the contents are true. Independent reruns need an execution boundary the coding agent cannot rewrite.

The strongest narrow concept is **an honest handoff and recovery contract**, accompanied by a conformance corpus. Users can inspect what arrived, what was omitted, what needs approval and what was actually rerun. A small OSS library plus integrations may fit better than another complete application. This is a hypothesis to test; native import and existing transfer tools are strong competing baselines.

Permissions must be checked again in the destination's effective policy. Imported text, tool output and memory should be treated as data with provenance, not authority to execute or permission to inherit. One shared transcript should not be opened for concurrent writes without a deliberate conflict model. Unfinished external side effects need explicit status so a restart does not repeat a destructive operation.

For native Mac UI selection, a privileged host-side capture component would need a constrained interface to an agent sandbox. A cropped image or AX payload can be useful without source mapping. Do not promise arbitrary-app element identifiers or SwiftUI file/line locations. This advisory research did not access the user's screen or authorize capture.

### Gaps

No spike demonstrates whether any destination follows imported decisions, respects permissions, preserves task evidence, or resumes with fewer failures. No benchmark establishes acceptable loss, no version-compatibility window is specified, and no bounded maintenance commitment exists for fifteen features. Current native Codex limit handling, every hook count, all source-referenced subscription prices, all issue reaction counts, and all live process behavior were not independently exercised here. Negative claims such as no competing tool existing or no source map existing remain search-bounded observations, not proven absence.

## 2. Do the authentication, subscription and security assumptions survive current primary texts?

### Takeaway

**Provider policy is an architectural dependency.** The source usefully distinguishes token proxies from running an official binary. Its absence of a confirmed account-rotation ban must not be interpreted as permission to implement rotation. The design should use vendor-supported login within unmodified official clients, keep credential handling out of shared memory, and avoid making limit evasion a core feature. These are readings of published technical and policy texts, not legal advice or a vendor authorization decision.

### Cited Findings

| Source location | Claim | Disposition | Primary evidence and consequence |
|---|---|---|---|
| §6, lines 715-719 | OpenAI SIWC prohibits rotating accounts to bypass usage limits and restricts token storage | **Verified, directly material** | September 29, 2026 terms §4 explicitly cover creating/splitting/rotating accounts to bypass limits. §1 requires persistent auth token storage local and user-controlled; §2 separately allows user-controlled remote runtimes for requests. [SIWC terms](https://openai.com/policies/sign-in-with-chatgpt-terms/). Thus remote execution permission is not permission for a managed remote credential vault. |
| D3/F5, lines 60/84 | Switch subscription accounts automatically when one limit is exhausted | **Contradicted for a SIWC integration enabling bypass** | The proposed trigger and purpose conflict with §4. That does not establish that every manual switch between separately authorized official-client accounts has the same interpretation. [SIWC terms](https://openai.com/policies/sign-in-with-chatgpt-terms/). |
| §6, lines 687-692 | Anthropic distinguishes third-party token intermediation from the user's own official-client login | **Verified** | Current Claude Code documentation prohibits developer collection/storage/intermediation of Claude.ai tokens, while permitting users to sign into an unmodified hosted Claude Code binary. Hosting carries additional commercial and branding conditions. [Claude legal and compliance](https://code.claude.com/docs/en/legal-and-compliance). This supports a launcher, not a subscription proxy. |
| §6, lines 682-685 | Anthropic Consumer Terms restrict automated access and protective-measure bypass | **Verified** | §3.7 has explicit API-key or other-permission exceptions; §3 also prohibits protective-measure bypass. [Anthropic Consumer Terms](https://www.anthropic.com/legal/consumer-terms). Do not infer that every user-authorized official CLI automation is prohibited; published permissions and product-specific conditions matter. |
| §6, lines 693-697 | November 12, 2026 Anthropic AUP adds unauthorized consumer-proxy wording | **Verified future text, not currently effective on audit date** | The live page says effective November 12. Its previous-version link leads to a September 15, 2025 policy. [Future AUP](https://www.anthropic.com/legal/aup), [previous policy](https://www.anthropic.com/legal/archive/22742366-2ef0-4c7a-a833-6523f10d3944). The source labels the future date correctly; decision-making must preserve that distinction. |
| §6, lines 723-725 | Antigravity prohibits unauthorized third-party service access | **Verified text; universal wrapper interpretation unresolved** | §6 names third-party tools accessing the service, including OpenClaw with Antigravity OAuth, and possible suspension of Antigravity/Gemini CLI accounts. The page excludes certain enterprise/API-key contexts from these consumer additional terms. [Antigravity terms](https://antigravity.google/terms/). |
| §6, line 730; §10, line 943 | No confirmed ban found for rotating one's own accounts with the official binary | **Unverifiable absence claim** | The source records a bounded failure to find an incident, not an affirmative exception. Current SIWC wording and OpenAI's general restriction on rate-limit circumvention are stronger evidence than missing enforcement anecdotes. [OpenAI Terms of Use](https://openai.com/policies/row-terms-of-use/). |

### Inferences

Centralizing all provider credentials and synchronizing them across machines introduces a more serious trust boundary than launching CLIs with each host's existing authorized login. A common project-memory store should never become a token transport. The credential and supported-auth model needs a separate provider matrix, with current text, account type, official-client/API distinction and refresh date.

Sandbox policy cannot be normalized by renaming keys. Repository-provided configuration, host settings, shell access, tool-level permissions and OS filesystem/network restrictions are distinct layers. Fail closed when an adapter cannot enforce a required restriction. Copying an approval across a provider, host, path or tool change could widen authority without the user seeing it. These are threat-model judgments supported by the client-owned policy boundaries in [Agent Plugins](https://aaif.io/blog/from-skills-and-tools-to-portable-agent-plugins) and [Claude sandbox documentation](https://code.claude.com/docs/en/sandboxing).

An OSS project is not exempt from provider contracts, third-party licenses, or secure credential handling. Bundling can improve usability, but redistribution and integration are separate from merely listing install commands. The source's Apache/MIT/AGPL/Elastic inventory is a useful starting point, not a completed compatibility analysis. [Source bundle inventory](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:670).

### Gaps

No vendor has approved the proposed complete auth/rotation/sync architecture in the evidence reviewed. Account types, jurisdictions, enterprise agreements and specific implementation details can change the applicable terms. This audit does not settle those legal interpretations. Historical enforcement timelines, reported staff comments and bans were not fully reconstructed against dated archives. Those anecdotes must stay below current primary texts in the evidence hierarchy.

## 3. Is the research good, and what evidence would justify building?

### Takeaway

**The research is good as an exploratory landscape review and insufficient as product or engineering validation.** Many technical and empirical claims survive primary-source checking. Its strongest weakness is inference: demonstrating pain and existing partial tools does not prove that a complete integrated tool is feasible, preferable, maintainable or wanted. Build a bounded evidence-producing experiment only after the owner selects a narrow outcome; do not start implementing all fifteen features from this document.

### Cited Findings

#### Material-claim ledger: evidence quality and test integrity

| Source location | Claim | Disposition | What the primary source actually supports |
|---|---|---|---|
| §5.3, lines 240-243 | Handoff context reduces rediscovery and improves resolution | **Verified, bounded** | Handoff Debt v2, August 30, 2026, uses 181 handoff points from 75 source tasks in one OpenHands-style runtime. Raw trace reduces median events 57-59%; note gains in solve rate are not significant for every successor. These are not official-CLI process-migration tests or wall-clock savings. [Handoff Debt](https://arxiv.org/html/2606.02875v2). |
| §5.10, lines 556-560 | Models game impossible tasks and monitors miss instances | **Verified, not ordinary-work prevalence** | ImpossibleBench deliberately conflicts tests and specification. GPT-5 reaches 54% on Conflicting-SWEbench; monitoring remains imperfect; hidden tests reduce shortcut success and can hurt legitimate performance. These experiments establish a hazard and mitigation tradeoff. [ImpossibleBench, Oct 23, 2025](https://arxiv.org/html/2510.20270). |
| §5.10, line 562 | METR found 30.4% hacking; a do-not-cheat prompt left 80% | **Correct numbers, overstated without denominators** | METR reports 39/128 RE-Bench runs (30.4%) but 8/1,087 HCAST runs (0.7%). The 80% prompt result is first-generation behavior on one task, 20 runs, not all coding tasks. [METR, June 5, 2025](https://metr.org/blog/2025-06-05-recent-reward-hacking/). |
| §5.10, line 563 | SpecBench visible/hidden gap grows with task length | **Verified as that study's result** | September 9, 2026 v2 describes 30 systems tasks and an approximately 28-point larger visible/holdout gap per tenfold code size. It does not quantify ordinary-user test tampering frequency. [SpecBench](https://arxiv.org/abs/2605.21384). |
| §5.10, line 567 | Agents add mocks more often than non-agents | **Verified association, not misconduct evidence** | The MSR-accepted study covers 1.2M commits and reports the cited 36%/26% and 23%/13% contrasts. More mocking does not itself establish test bypass or inferior correctness. [Over-mocked tests study, Jan 30, 2026](https://arxiv.org/abs/2602.00409). |
| §5.10, lines 568-572 | 49.6% include tests; 22.5% improve Python diff coverage | **Verified with wording correction and sampling limits** | 49.6% means test changes among 4,387 PRs changing code, not new tests in all 4,882 PRs. Coverage was executable for 213 Java and 1,664 Python PRs; paired test-gain analysis uses 64/605. The 74.8% and 2.6x figures are conditional subsets. [PR coverage study, July 20, 2026](https://arxiv.org/html/2607.18057v1). |
| §5.10, line 573 | Agent weak assertions 14.3% versus human 11.9% | **Verified table values, limited construct validity** | The paper's full-agent table has 14.30/11.92; prose uses a random matched cohort at 14.63. AST categories mark bare asserts unknown and use heuristic strength/edge-value classifications. Agents also show more enumerated edge cases; the paper does not support a blanket bad-tests conclusion. [Beyond Test Presence, July 2026](https://arxiv.org/html/2607.12068v1). |
| §5.10, line 581 | Gauntlet can protect verification; corpus 35/35 tamper detections and 7/21 benign alarms | **Verified author-reported evaluation; independent qualification absent** | Current README reports the same corpus counts but rc.35 and 296 commits, versus source rc.14/84 commits. Its README explicitly says research cited there does not evaluate Gauntlet. No execution was done here. [Gauntlet](https://github.com/matthewjones372/gauntlet). |
| §5.11, lines 651-660 | Routing/multiple models improves work | **Mixed; fixed task-brand rule speculative** | LLMRouterBench reports up to 4% average accuracy gain and 31.7% cost savings at matched quality for some routers; others lose. Its pool mismatch also qualifies the reported OpenRouter -24.7%. This is not a test of Opus-for-features/GPT-for-tests/Gemini-for-debugging in current CLI workflows. [LLMRouterBench, Jan 12, 2026](https://arxiv.org/html/2601.07206v1). |

The document fairly separates owner statements, features, unresolved questions and unverified items. It records competing tools, negative experiences, vendor-native alternatives, research that weakens simplistic routing claims, and uncertainty around its own statistics. Those are substantial strengths. [Source §4](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:98), [source §5.11](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:651), [source §10](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:936).

Methodological weaknesses visible in the document are:

1. The 308 Markdown citations appear in the end bibliography; there are no inline hyperlinks before §11. A reader has to reconstruct which source supports each claim. Versioned snapshots, run receipts and repository SHAs would strengthen reproducibility.
2. The capability matrix defines N as both absent and not documented, merging two different evidence states. Y means documented, not tested. A platform-level sandbox Y hides OS-specific differences. [Source §7](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:734).
3. Benchmarks, observational repository studies, author demos, issue anecdotes and market reactions have different external validity. Large-looking percentages need their actual denominators and task conditions. More mocks or weak assertion categories do not prove intentional gaming.
4. Issues and stars are selected, overlapping samples, not independent votes for fifteen bundled features. Personal pain is a strong starting hypothesis, but there is no comparative usability test or measured reduction in setup/recovery work. [Owner problems](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:37), [issue inventory](/Users/chaoticqubit/Projects/CrazyAgent/docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md:841).
5. The source documents many moving upstream interfaces yet promises quick delivery and any-harness support without an adapter lifecycle or maintenance budget. Even the material claims sampled here show release drift within the research window.

### Inferences

For test integrity, distinguish four results: the test harness was not tampered with; production code was exercised; seeded defects were caught; and requirements were satisfied. They require different evidence. A read-only test file or second LLM can help, but neither alone establishes all four. Coverage cannot establish assertion quality; mutation testing cannot establish missing requirements; passing hidden tests cannot cover unknown requirements. The verification layer must say which claim it supports.

The smallest meaningful validation program is five bounded comparisons, proposed here as research acceptance criteria rather than implementation authorization:

1. **Handoff quality:** freeze real partial tasks, compare native resume/import, ordinary handoff notes and the proposed checkpoint under the same repository state; score missing decisions, repeated exploration, valid completion, steps, elapsed time and input tokens. Include a no-handoff baseline and report per-task failures.
2. **Interruption recovery:** interrupt before, during and after a file write, test process and synthetic external side effect; verify no lost dirty work, falsely completed check or duplicate effect. Include reset-time changes, sleep, login change and killed child processes.
3. **Configuration and authority:** install a finite test pack with supported and unsupported skills/hooks/MCP/settings, then inspect actual target behavior. Require explicit failure/loss reporting and demonstrate no broader effective permissions after migration.
4. **Verification integrity:** independently seed deleted tests, weakened assertions, skipped suites, altered runner configuration, mocked production boundaries, stale result files and ordinary legitimate test rewrites. Report false positives and false negatives, not a single score. Existing Gauntlet corpus is a useful baseline to reproduce, not a certification.
5. **Usefulness and maintenance:** have several real multi-harness users recover the same kind of work with their current setup and the experiment. Measure successful recovery without manual repair, minutes spent, repeated usage and adapter breakage across a supported upstream version change.

Predeclare success thresholds and a stop condition with the owner before running paid or lengthy experiments. A candidate threshold could be reliable preservation of declared artifacts, zero silent authority widening or data loss in the corpus, and a material reduction in recovery effort versus native workflows. These are suggested decision rules, not measured results.

The open-source direction is justified if a small, reusable handoff/recovery or verifier component solves the owner's own recurring problem and other users can demonstrate the same benefit. Abandon the universal scope now. Abandon the narrow experiment only if existing native flows already solve it adequately, permission restrictions remove its benefit, or the supported adapters cost more to maintain than users gain. Technical feasibility of a modest tool does not prove adoption; this audit does not claim novelty or commercial success.

### Gaps

No controlled user comparison, maintenance experiment, secure cross-device design, independent tamper-corpus reproduction or prototype exists in the provided evidence. The alleged absence of a complete independent spec-to-black-box-test product remains unverified; the document itself lists adjacent QA tools as insufficiently researched. Model rankings, prices and task-routing preferences were not refreshed exhaustively, and no current benchmark validates the owner's specific fixed routing rule. The 308 cited pages were not all fetched or archived. The complete research should remain labeled exploratory until the chosen outcome has direct evidence.
