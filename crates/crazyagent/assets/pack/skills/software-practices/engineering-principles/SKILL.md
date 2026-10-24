---
name: engineering-principles
description: >
  Google engineering judgement (SWE at Google ch. 1-3, 8, 15, 18, 23), the models under trunk-based dev, flags, review and testing. Use for design/architecture tradeoffs, tech debt, style rules and coding standards, deprecation and migration, build/CI, maintainability, Hyrum's law, rewrite-or-not, engineering culture - and as the default advisor on an engineering decision.
---

# Google Engineering Principles

These are the throughlines behind Google's specific practices. Use them to reason about engineering decisions the way *Software Engineering at Google* does. When advising, name the relevant principle and apply it concretely.

## Foundational mental models

- **"Software engineering is programming integrated over time."** Programming produces code; software engineering is developing + maintaining it over its whole life span. Treat expected code lifetime (minutes to decades) as a first-class variable: short-lived code can be clever/hacky; long-lived code must be maintainable. *"It's programming if 'clever' is a compliment, but it's software engineering if 'clever' is an accusation."*
- **Optimize for sustainability**, not just current correctness: be *able* to react to any valuable change (technical or business) for the code's expected lifetime.
- **Hyrum's Law** (verbatim): *"With a sufficient number of users of an API, it does not matter what you promise in the contract: all observable behaviors of your system will be depended on by somebody."* Any observable behavior — timing, ordering, error text — will eventually be relied on. Design and deprecate with this in mind; don't depend on unspecified behavior yourself (e.g., hash iteration order).
- **The Beyoncé Rule:** *"If you liked it, you should have put a [CI] test on it."* If it isn't protected by a CI test, breaking it is not the infrastructure change's fault.
- **Code is a liability, not an asset.** Value comes from *functionality*, not lines of code. Maximize functionality per line; deleting obsolete code is progress. *"If you're writing it from scratch, you're doing it wrong"* — prefer reuse.
- **Shift left:** catch problems as early in the workflow as possible (design → review → test → commit → canary → prod). Each step right is exponentially more expensive. Use defense in depth.
- **Scale sublinearly:** any task done repeatedly must not grow linearly in human effort. Stress-test a policy by imagining the org 10-100× bigger. Push migration effort onto expert teams and tooling, not every consumer (the Churn Rule).
- **Decide on evidence, never "because I said so."** Weigh all costs — financial, resource, personnel, transaction, opportunity, societal. Revisit decisions as data changes; leaders who admit mistakes earn more trust.

## Team & knowledge culture

- Software is a team endeavor built on **Humility, Respect, Trust**; reject the Genius Myth. **Don't hide your work** — early sharing raises the bus factor and tightens feedback.
- **"You are not your code."** Give and take criticism about the code, not the person. *"Fail early, fail fast, fail often"* and run **blameless post-mortems** (summary, timeline, root cause, impact, action items with owners, lessons — no finger-pointing).
- Build **psychological safety**; always be learning and asking questions. **Chesterton's Fence:** understand why something exists before removing it; prefer understanding legacy code to rewriting it.
- Avoid **haunted graveyards** (code people fear to touch), **information islands**, single points of expertise, and **parroting** (copying without understanding). Scale knowledge: write it down, keep canonical sources of truth, improve docs the first time you learn something.

## Style guides & rules

- Rules are **laws** (mandatory); guidance is "shoulds." Ask *"what goal does this advance?"* not "what rules should we have." Rules must:
  - **Pull their weight** — don't codify the self-evident or one-off mistakes.
  - **Optimize for the reader**, not the writer — code is read far more than written.
  - **Be consistent** — enables chunking, tooling, and mobility across the codebase; prefer external/community norms at the boundary.
  - **Avoid error-prone/surprising constructs**; concede to real practicalities (performance, interop).
- Leave **explicit evidence of intent** in code and aim for **local reasoning**. **Automate enforcement** — formatters (gofmt/clang-format) and linters (clang-tidy, Error Prone) settle style; don't debate formatting by hand. Back every rule with documented reasoning so it can be revisited.

## Deprecation & migration

- Deprecate a system only when it is **demonstrably obsolete and a comparable, production-ready replacement exists** — age alone doesn't justify it. Two systems doing the same job is an ongoing tax. **Limit simultaneous deprecations.**
- **Design for deprecation up front:** how easily can consumers migrate off? Can you replace it incrementally? Prefer **incremental in-place evolution over big-bang rewrites** (full migrations are consistently underestimated).
- **Advisory deprecation** (no deadline) rarely drives migration — *"hope is not a strategy."* **Compulsory deprecation** needs a hard deadline, a **staffed expert team that does the migration work** (not an unfunded mandate), and authority to break sufficiently-warned holdouts.
- **Warnings must be actionable and scoped** (surfaced where the deprecated thing is used, with next steps). Avoid alert fatigue. Assign an owner whose primary goal is removal; set incremental, measurable milestones — not "full removal" as the only milestone.

## Large-Scale Changes (LSCs)

- An LSC is a logically-related change too big to land atomically. As the codebase grows, the largest possible atomic change *shrinks*. **Shard into small, independently committable/testable pieces**; treat shards as "cattle, not pets."
- **Centralize LSCs in an infra/expert team** with the tooling and cost ownership. Statically-typed languages are far easier to migrate automatically. Rule of thumb: if a change needs >~500 edits, invest in change-generation tooling instead of doing it by hand. Prevent backsliding with review-time checks.

## Build systems

- Optimize builds for **Fast** and **Correct** (same inputs → same outputs anywhere). Prefer **artifact-based** systems (Bazel/Buck/Pants) with declarative BUILD targets over task-based ones (Make/Ant/Maven/Gradle) — limiting engineers' power lets the system parallelize, cache, and give reliable incremental builds.
- Treat compilers/tools as versioned dependencies; sandbox actions; pin external deps by cryptographic hash and mirror them. Enable **remote caching + remote execution**. Prefer **fine-grained modules** (toward 1:1:1 package/target/BUILD) and minimize visibility (default private). **Version external deps explicitly** — never "latest"/ranges — and enforce the One-Version Rule.

## Continuous integration (ties it together)

- **CI's goal: catch problematic changes as early as possible.** *"CI is alerting"* — treat test failures like production alerts (actionable, high-fidelity; a flaky test is as harmful as a spurious page).
- Integrate at **head**; distinguish true head from **green head** (latest CI-verified) and develop against green head. Run only **fast, reliable** tests on **presubmit**; catch the rest **post-submit**. Feature flags/experiments are the feedback loops that make continuous delivery safe (see `feature-flags`).
- Keep the build green; prefer **rollback over roll-forward** ("any change can be rolled back with two clicks"). Smaller changes trigger fewer tests and submit faster — another incentive to keep changes small.

## The AI era (framing from Adam Bender's Google I/O 2026 talk, "Software engineering at the tipping point")

Use **systems thinking**: a codebase is a socio-technical ecosystem, and the developer ecosystem shapes how software evolves. As AI-driven development accelerates the *writing* of code, the durable constraints move to the parts this plugin is about — **integration over time, review, testing, and safe rollout**. When code is cheap to generate, "code is a liability" matters *more*: guard against unreviewed, untested, hard-to-delete volume. Keep humans and CI as the high-fidelity checks (Beyoncé Rule), and let flags + trunk-based flow absorb faster change safely. (The talk's full transcript was not machine-accessible; this reflects its published theme.)

## When advising on any engineering decision

1. Ask **how long this code must live** and optimize accordingly.
2. Prefer **reuse and deletion** over new code; treat volume as cost.
3. Push work **left** and toward **tooling/expert teams**, not repeated human effort.
4. Protect behavior with **CI tests**; assume **Hyrum's Law**.
5. Choose **incremental** paths over big-bang rewrites/migrations.
6. Back the recommendation with a **tradeoff and evidence**, and name the principle.
