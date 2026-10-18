# Innovation and build verdict

Assessment date: 2026-10-16. Advisory research only. No implementation, install, account handling, runtime qualification, or provider permission assessment was performed in this lane.

## Takeaway

Research: **Good**. Idea: **Promising**. Original fifteen-feature project shape: **Overextended**. Recommendation: **Reshape**.

Do not abandon the underlying need. Do abandon the plan to create a new universal platform around F1-F15 before testing existing tools. The user's real outcome is credible: install once, preserve work when changing tools, and know whether the resulting work is trustworthy. That outcome can justify an open-source integration even without a business model or a novel algorithm. This research does not yet establish that another independent runtime is the best route.

My preferred next move is an opinionated personal integration or upstream contribution, evaluated against actual interrupted work. A new repository earns its existence only if a repeatable, important gap remains after that trial. A public compatibility test corpus could justify an independent repository more readily than another dashboard because competing hosts can consume it without adopting a competing runtime. Whether maintainers would use such a corpus remains unvalidated.

## Owner intent and constraints

The supplied document records recurring personal pain across context, configuration, quota interruption, worktrees, cross-machine setup, usage analysis, native UI feedback, and weak agent-written tests. The owner explicitly wants an open-source project, not a company, and identifies unified installation as the thesis. Its F15 composition claim deserves evaluation on setup effort and reliability rather than startup market size. Its F2/F3/F6/F9 promises also require distinguishing transferable task facts from hidden model state, running processes, permissions, and external effects. These are interpretations of the supplied primary document, not additional requirements imposed on the owner. [Original research](../../docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md)

The original is strong as an inventory: feature IDs, alternatives, negative evidence, unanswered decisions, and explicit unverified items. It is weaker as validation: issue reactions do not measure how frequently one person experiences a problem, how much recovery costs, or whether a single integrated install changes behavior. A capability matrix cannot establish semantic equivalence between vendors. An impossible-task cheating result cannot supply the real-world frequency of untrustworthy tests. Those are inference limits, not accusations of poor research.

## Disconfirming prior art found in this lane

All project capabilities below are documented by their authors and were retrieved on 2026-10-16. They were not installed or exercised. Repository existence and documentation are evidence of prior art, not proof of reliability or demand.

| Source | What is already documented | Consequence for CrazyAgent |
|---|---|---|
| [Agent Engineering Toolkit](https://github.com/AdvancingTitans/agent-engineering-toolkit) | A local evidence plane with source-bound checks, stale-proof detection, explicit unknown states, and portable evidence bundles. | A proof-bearing handoff is not an unclaimed invention. Evaluate or integrate this before building another evidence format. |
| [oneharness](https://github.com/nickderobertis/oneharness) | A shared CLI and Rust/Python/TypeScript SDKs, configuration synchronization, capability mapping, explicit unsupported behavior, and documented live checks. Permission rules without safe translations are reported rather than widened. | Even semantic portability auditing has direct prior art. A wrapper must improve a measured workflow, not merely expose unified fields. |
| [Gauntlet](https://github.com/matthewjones372/gauntlet) | Base-controlled checking, fresh-process evidence, changed-line mutation testing, human-authored scenario bindings, explicit owner decisions, and holdouts. Its README says repository holdouts are hidden from the agent rather than secret. | F13 is a natural contribution or adapter target. Calling an independent new verifier novel would overclaim. |
| [Portable Agent Memory](https://arxiv.org/abs/2605.11032) | A May 10, 2026 preprint proposing provenance-linked memory, scoped access, and rehydration across heterogeneous agents. | Provenance and scoped memory transfer already have research prior art. Cryptographic integrity does not establish truth or sufficient context. |
| [UseAgent shared packages](https://useagent.org/docs/architecture/shared-packages/) | Provider-neutral session/capability contracts and deterministic contract tests. Its documentation distinguishes these from environment-gated live provider parity. | A portability corpus must test real supported workflows and version changes to add value beyond interface shape checks. |
| [Harbor ATIF](https://docs.harborframework.com/agents/atif) | An agent trajectory interchange format. | Reuse interchange formats where possible. A new serialized transcript alone is a weak project thesis. |

New direct overlaps weaken a novelty claim, but they do not settle usability. A collection of technically capable tools may still fail the owner's install-once experience. No end-to-end comparison of the above was performed here.

## Four recommended hypotheses, ranked

Scores are my ordinal judgments, not measured research results. A 5 indicates high pain or differentiation; for risk it means high risk; for feasibility it means relatively feasible. Differentiation is assessed as a practical outcome, not a claim of patent-level novelty.

| Rank | Hypothesis | Pain | Differentiation | Provider/dependency risk | Feasibility | Preferred project form |
|---|---|---:|---:|---:|---:|---|
| 1 | Tested portability profile and compatibility doctor | 4 | 3 | 2 | 4 | Public corpus plus adapters to existing tools |
| 2 | Resume-readiness receipt that exposes unfinished effects | 5 | 2 | 3 | 3 | Extension to an existing host/evidence toolkit |
| 3 | Cross-harness test-integrity adapter | 5 | 2 | 2 | 3 | Contribute to Gauntlet or integrate it |
| 4 | Native repro packet with a verifiable before/after journey | 3 | 3 | 3 | 3 | Standalone optional tool or plugin |

### 1. Tested portability profile and compatibility doctor

**Scenario:** A developer moves from Claude Code to Codex. Before the successor edits anything, the tool says which instructions were discovered, which permission rules retained their meaning, which hooks do not apply, which MCP connections need local authentication, and which checks have actually passed on the installed versions. It refuses to label unsupported protection as equivalent.

**Proposed differentiator:** An independently reusable set of real workflow probes: interruption during a tool call, permission denial, conflicting memory, dirty checkout, stale test report, and resumed work in another host. The output is a specific compatibility result for a supported version pair, not a universal compatibility promise. The useful application is a preflight users can act on before risking unfinished work.

**Prior art:** oneharness already maps capabilities and refuses unsafe permission translations; UseAgent already distinguishes contract tests from live provider parity. This hypothesis survives only if a public, maintained workflow corpus catches consequential failures those projects miss, or substantially reduces setup/support work. [oneharness](https://github.com/nickderobertis/oneharness), [UseAgent](https://useagent.org/docs/architecture/shared-packages/)

**Falsification:** Select ten concrete owner failures. If an existing tool can already reproduce and prevent at least nine with an acceptable setup, contribute the missing case instead of launching a new runtime. Even the corpus should be contributed upstream if maintainers prefer that arrangement.

### 2. Resume-readiness receipt that exposes unfinished effects

**Scenario:** A session stops after editing code, while a test is running and a migration command may have reached the database. The next harness sees source revision, dirty work, last independently observed checks, open processes, the migration's uncertain outcome, and explicit steps required before continuing. It does not infer that a transcript saying a command was started means the operation completed.

**Proposed differentiator:** Reduce human recovery decisions after interruption. The central unit is unfinished work with known, failed, and unknown effects, rather than a claim to preserve a model's internal session. The resumer first reconciles the current environment with the receipt. No generic claim of exactly-once execution, transparent subprocess revival, or safe automatic replay follows from this idea.

**Prior art:** AET already provides portable, source-bound evidence. Handover documents structured continuity records. Durable execution systems demonstrate that resumability depends on controlled execution and side-effect semantics; merely wrapping a third-party CLI does not acquire that control. This is an integration hypothesis rather than an original checkpoint protocol. [AET](https://github.com/AdvancingTitans/agent-engineering-toolkit), [Handover guides](https://handover.sh/guides), [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence)

**Falsification:** Compare interrupted tasks with native resume plus a normal handoff file. If the receipt does not reduce median manual recovery time by at least half without increasing silent errors, the extra layer has not justified itself. The threshold is proposed, not an established industry benchmark.

### 3. Cross-harness test-integrity adapter

**Scenario:** Claude implements a feature and Codex continues it. An independent checker reports that the selected behavior was exercised against this exact source, the relevant test can fail, and altered verification machinery requires an owner's decision. Merely observing green terminal output is insufficient.

**Proposed differentiator:** Make the same protected verification policy survive a host change, and present one understandable acceptance receipt. Integrate existing mutation, scenario, baseline, and CI mechanisms rather than inventing another test generator. Tests still need a credible behavioral oracle; mutation strength alone does not tell whether the requested feature is correct.

**Prior art:** Gauntlet is directly in this territory. Its current README is materially broader than the supplied document's snapshot. I recommend contributing a supported cross-host adapter or evidence consumer, not competing on generic test-tamper detection. ImpossibleBench establishes a failure mechanism under constructed conflicting tasks; it does not estimate the owner's production defect rate. [Gauntlet](https://github.com/matthewjones372/gauntlet), [ImpossibleBench](https://arxiv.org/abs/2510.20270)

**Falsification:** Test a blinded set of genuine behavior changes, deliberate weakening cases, and legitimate test edits. Require low false alarms as well as detection. If owners routinely override it or disable it, more checks have made the workflow worse.

### 4. Native repro packet with a verifiable before/after journey

**Scenario:** A Mac app's dialog clips a button only after resizing and opening a menu. The user marks the button once. A packet captures the build identity, selected accessibility element if exposed, screenshot, relevant window geometry, and the short action sequence. A later run recreates the same situation and records whether the expected behavior changed.

**Proposed differentiator:** Preserve a reproducible user-observed failure, not just send an annotated screenshot. This can close the loop between human feedback and independent acceptance of native UI changes. A failed or unavailable accessibility lookup remains explicit. A source file and line cannot be promised from accessibility data alone.

**Prior art and uncertainty:** The supplied document names Loupe, Deiko, Peekaboo, Appshots, and annotation tools. Peekaboo's primary repository documents native accessibility and screenshot tooling, so element capture itself is existing capability. I did not establish that no existing tool combines reproducible native journeys with source-bound before/after evidence. Demand is thinner than the portability/test problems, and macOS-only permissions and automation add support cost. [Peekaboo](https://github.com/steipete/Peekaboo), [Original research](../../docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md)

**Falsification:** Give five native app builders ten real visual defects each. Retain the idea only if at least four prefer it over their current screenshot/computer-use workflow and it cuts median time to a usable reproduction by at least half. These sample sizes and thresholds are proposed discovery gates, not statistical proof.

## Four additional ideas considered

These are creative compositions, not claims of unprecedented invention. They are lower priority until the core failure is measured.

| Idea | Concrete application | Prior art and reason to defer |
|---|---|---|
| Graceful quota landing | Stop at a useful artifact: a reviewable patch, completed evidence, and explicitly parked work before a limit interrupts the session. | Checkpoint and handoff systems already exist. Quota prediction and subprocess control may be unreliable. First test a manual checkpoint policy rather than background automation. [Handover](https://handover.sh/guides) |
| Branch-scoped memory conflict inbox | Two agents disagree about a decision. Preserve both claims, their branches, sources, and expiry instead of silently merging them into shared truth. | Memory provenance and access controls already have prior art. False conflict alerts and prompt-injection exposure can outweigh the benefit. [Portable Agent Memory](https://arxiv.org/abs/2605.11032) |
| Human attention queue | Rank unfinished work by the decision only a person can make: changed acceptance criteria, uncertain side effects, or missing independent evidence. Show the exact unresolved decision. | AET already separates evidence states and advice; generic approval inboxes are common. Differentiation would be fewer owner minutes per accepted change, which is unmeasured. [AET](https://github.com/AdvancingTitans/agent-engineering-toolkit) |
| Personal model experiment notebook | For one developer's repeated tasks, compare a strong single model with a handoff or cross-model review using correctness, elapsed time, and review burden. | The supplied routing studies already challenge universal role assignments. This should establish whether routing helps before a router is built, and avoid generalizing one person's sample into global policy. [Original research](../../docs/Meta-Harness%20%E2%80%94%20Decisions%20Features%20%20Research%20Data.md) |

## Anti-features and the smallest useful open-source core

Avoid making automatic account rotation the defining advantage, silent permission translation, automatic replay of uncertain side effects, or whole-transcript synchronization by default. Avoid claiming that an evidence hash proves correctness: an attestation can identify provenance while the underlying code remains wrong. Avoid a universal all-provider support matrix maintained by one person. [GitHub artifact-attestation concepts](https://docs.github.com/en/actions/concepts/security/artifact-attestations)

The smallest useful contribution is one repeatable continuity workflow, two explicitly supported hosts, one platform, local inspectable artifacts, and a compatibility result users can reproduce. Its first artifact could be only the portability failure corpus and a documented integration profile. A GUI, credential vault, cloud account, model scheduler, new sandbox, memory database, and a new plugin standard are unnecessary to test this value. This is an advisory scope boundary, not an implementation plan.

There is a legitimate alternative: publish an opinionated distribution of existing components. That satisfies the owner's integration thesis if it reduces setup and support burden. Pinning versions, recording supported behavior, and declining unsafe combinations would be its actual work. It should not promise all F1-F15 on day one or hide which pieces are experimental. Existing components' source licenses, update mechanics, security boundaries, and actual runtime compatibility still need assessment before distribution.

## First users and proposed stop/go evidence

The first users should be owner-like developers who already switch between two native coding harnesses on real repositories, experience unfinished-work recovery repeatedly, and will share local failure receipts. Large enterprises, beginners needing every provider, and people merely interested in agents are poor first validation targets. This is a recruitment hypothesis.

All numbers below are proposed decision thresholds, not facts from the literature:

1. Record ten real interruptions or tool changes, including recovery steps and elapsed minutes. If the pain occurs less than weekly or consumes under thirty minutes per week, do not build a new platform for it.
2. Run those cases through the best existing host plus simple handoff instructions. If at least nine of ten outcomes are acceptable, contribute the remaining failure or publish a recipe.
3. If a gap remains, observe forty interrupted/resumed tasks with the smallest integration. Continue only if median recovery time falls at least 50%, no loss or unauthorized replay is observed, and at least eight of ten target users can complete setup within ten minutes. Zero observed bad events does not prove safety.
4. After two weeks, continue public maintenance only if at least six of ten target users use it on three or more real sessions and prefer it to their existing setup. Interest, GitHub stars, and installation counts do not replace repeat use.
5. Cap ongoing maintenance at two hours per week for the supported pair. If vendor changes repeatedly exceed this or every useful behavior requires private transcript parsing, narrow further, contribute upstream, or stop. This cap reflects a small open-source project's constraints, not a universal engineering standard.

## Strongest argument against my recommendation

The owner explicitly says integration is the point. Recommending upstream contributions and a corpus risks solving the maintainer's elegance problem while leaving the user managing a pile of tools. A cohesive, well-supported distribution can be valuable without novel features, and existing READMEs may hide a poor real installation experience. If a hands-on comparison shows that all available hosts remain cumbersome or unsafe for this exact workflow, building a narrowly supported integrated app is defensible.

The strongest argument against building it is symmetrical: each additional wrapper becomes one more stateful tool to debug, and the investigation found direct prior art even for the apparent best differentiators. The burden of proof is therefore a concrete improvement over existing setups, not a larger feature list.

## Remaining uncertainty

No current integrated stack was installed, no end-to-end portability trial was run, and no prospective users were interviewed. Evidence was drawn from primary documents and project documentation, not from a security qualification of those projects. README capability claims may exceed production behavior. Searches cannot prove novelty or absence of competitors. The overall original research should be refreshed with the newly found overlaps before it is treated as a decision-ready validation document.

**Verdict:** Do not build the proposed universal platform now. Keep the problem, test existing integrations against the owner's real failures, and prefer a focused contribution or small distribution. Start a new runtime only after a measured gap survives those comparisons.
