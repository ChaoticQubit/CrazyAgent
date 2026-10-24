---
name: testing
description: >
  Google testing practice (SWE at Google ch. 11-14): what and how to test, unit vs integration vs end-to-end, the test pyramid, test doubles (mock, stub, fake), flaky tests, coverage, hermetic tests, test strategy.
---

# Testing (Google-style)

Automated testing is the foundation that makes change safe — without it you get "haunted graveyards" of code nobody dares touch. Guide the user toward **many small, fast, hermetic tests of behavior**, and away from brittle, over-mocked, or top-heavy end-to-end suites.

## Governing rules

1. **The Beyoncé Rule** (verbatim): *"If you liked it, then you shoulda put a test on it."* Test everything you don't want to break — including failure handling (simulate exceptions, RPC errors, latency). Only failures caught by tests in CI "count."
2. **Prefer the smallest test that gives confidence.** Smaller = faster + more deterministic + easier to diagnose.
3. **All tests should be hermetic** — self-contained setup/execute/teardown, no dependence on run order, no shared mutable database, no external services.
4. **Write, run, react — and fix a broken test within minutes.** A perpetually-red or ignored suite is worse than no suite. Never let failing tests pile up.

## Test size (about the runtime environment)

Classify every test by **size** and keep the mix skewed small:

- **Small** — single process, often single thread. **No sleeping, no I/O, no blocking calls, no network, no disk.** Use test doubles for heavy deps. Fast and deterministic. The overwhelming majority of tests.
- **Medium** — single machine, multiple processes/threads; may talk to `localhost`. No network to other machines.
- **Large** — multiple machines / full system; reserved for true end-to-end and legacy scenarios. Slow, may be nondeterministic — **isolate from the small/medium run** and give them explicit owners or they rot.

## Test scope & the pyramid

Scope = how much code a test exercises: narrow (unit), medium (integration), broad (end-to-end). Target roughly:

- **~80% unit / ~15% integration / ~5% end-to-end.**
- Avoid the **ice cream cone** (too many E2E, too few unit) and the **hourglass** (too few integration).

## Writing good unit tests

1. **Strive for unchanging tests.** Pure refactorings, new features, and bug fixes should **not** require editing existing tests. Only a genuine **behavior change** should. If a refactor breaks tests, they were testing the wrong thing.
2. **Test through the public API ("use the front door").** Don't reach into private implementation. A failing test should imply a real user would break.
3. **Test state, not interactions.** Verify the resulting state, not which methods were called. Interaction tests are brittle "change-detector" tests.
4. **Test behaviors, not methods.** One test per behavior. Structure as **given / when / then** (arrange / act / assert), one "when" and one "then" each.
5. **Name the test after the behavior**, e.g. `shouldNotAllowWithdrawalWhenBalanceIsEmpty`. The name is the first thing seen on failure. If you need "and" in the name, you're testing two behaviors — split it.
6. **No logic in tests.** No loops, conditionals, or operators — prefer straight-line code even if repetitive. Logic hides bugs, and there are no tests for your tests.
7. **DAMP over DRY.** Prefer **Descriptive And Meaningful Phrases** over Don't-Repeat-Yourself. Some duplication is fine if it makes each test **complete** (all relevant info in the test body) and **concise** (nothing irrelevant). Don't hide test-relevant values in shared setup; use builders/helpers with sensible defaults for the irrelevant ones.
8. **Write clear failure messages** — expected vs. actual with context. Use expressive assertion libraries.

## Test doubles (mocks / stubs / fakes)

1. **Prefer real implementations** over doubles (classical, not mockist, testing). Use the real thing when it's **fast, deterministic, and has simple dependencies** (e.g., value objects). Real objects catch real bugs and survive refactors.
2. **When a real implementation won't work, prefer a fake.** A **fake** is a lightweight working implementation (e.g., in-memory database). Fakes should be **written and maintained by the team that owns the real thing**, must stay faithful to the real API contract, fail fast on unsupported paths, and **have their own tests** (ideally contract tests run against both the real and fake).
3. **Stub sparingly.** Hardcoding return values duplicates the contract with no fidelity guarantee and makes tests brittle and unclear. Each stub should tie directly to an assertion; don't stub broadly to force behavior.
4. **Avoid interaction testing** — verifying a function *was called* creates change-detector tests. Only verify interactions for **state-changing** calls (`sendEmail`, `chargeCard`), never for reads. Use argument matchers (`any()`, `eq()`) to avoid over-specification; fall back to interaction tests only when state testing is impossible, and supplement with a larger state-based test.
5. Overusing a mocking framework pollutes the codebase; steer callers to real implementations or fakes.

## Larger tests (when unit tests aren't enough)

Larger tests close **fidelity** gaps unit tests can't: stale doubles, **configuration bugs** (Google's #1 cause of major outages — version config with the code), behavior under load, unanticipated inputs, and emergent/Hyrum's-Law behavior. Structure them as **System-Under-Test + Data + Actions + Verification**:

- Prefer **hermetic SUTs** (sandboxed/cloud-isolated) over shared staging for stability; shrink the SUT at API boundaries; **don't hit real third-party APIs.**
- **Poll for state transitions instead of `sleep()`/`setTimeout()`** to de-flake; lower internal timeouts for tests; make failures traceable (clear message, request IDs, an owner to contact).
- Kinds: functional, browser/device, performance/load/stress, config smoke tests, **A/B diff regression**, probers & canary analysis (read-only/deterministic actions in prod), disaster-recovery/chaos, and user evaluation (dogfood/experiment).

## Coverage — handle with care

- **Code coverage is a weak metric** — it measures lines *executed*, not behavior *verified*. Don't treat a coverage number as a goal or a ceiling. Think in terms of behaviors tested. Measure coverage from small tests only.

## When writing tests for the user

- Default to **small, hermetic unit tests**; reach for integration/E2E only for what units can't cover.
- Use **real objects → fakes → stubs → mocks** in that order of preference.
- One behavior per test, `given/when/then`, behavior-named, no logic, clear failure message.
- Cover the failure/error paths, not just the happy path (Beyoncé Rule).
- If existing tests would need editing for a pure refactor, treat that as a smell and flag it.

See `references/testing-patterns.md` for test-structure templates and a fake vs. mock decision guide.
