# Testing Patterns

## given / when / then structure (language-agnostic)

```
test "should not allow withdrawal when balance is empty":
    # given (arrange) — only the state relevant to THIS behavior
    account = Account(balance=0)

    # when (act) — exactly one action
    result = account.withdraw(10)

    # then (assert) — one behavior, clear expected vs actual
    assert result.failed
    assert result.reason == "insufficient funds"
    assert account.balance == 0
```

Rules applied: one behavior, behavior-named, no loops/conditionals, complete (balance shown in the body, not hidden in setup), concise (no irrelevant fields), clear assertion.

## Test double decision guide

Ask in order; stop at the first "yes":

1. **Can I use the real implementation?** Is it fast, deterministic, and simple-dependency?
   → Use the real object. (Preferred.)
2. **Is there a fake maintained by the owning team** (in-memory DB, in-memory queue)?
   → Use the fake. Verify it has contract tests against the real thing.
3. **Do I need to force a specific state I can't otherwise reach?**
   → Stub *narrowly* — one stub per assertion, nothing broad.
4. **Is the thing under test a state-changing side effect** (`sendEmail`, `chargeCard`) with no observable state to assert?
   → Interaction-test it (verify called), using argument matchers to avoid over-specifying. Supplement with a larger state test if possible.

Never: mock a value object; stub broadly to reshape behavior; verify calls on read-only methods.

## Fake fidelity checklist

- [ ] Written/owned by the team that owns the real implementation.
- [ ] Obeys the same API contract and error semantics.
- [ ] Fails fast (throws) on unsupported operations rather than silently diverging.
- [ ] Has its own tests; ideally a shared contract-test suite runs against both real and fake.

## De-flaking larger tests

- Replace `sleep(2s)` with **polling** for the awaited condition (with a timeout):
  ```
  wait_until(lambda: job.status == "done", timeout=30s, poll=100ms)
  ```
- Use event/callback notifications instead of fixed waits where available.
- Lower internal service timeouts in the test config so failures surface fast.
- Make failures diagnosable: assertion messages, correlation/request IDs, and a named owner/contact for the test.
- Isolate large tests from the fast unit run so their flakiness never blocks presubmit.

## Coverage guidance

- Use coverage to find **untested behavior**, not as a target number.
- Measure from small tests only.
- A green coverage bar over interaction-heavy, implementation-coupled tests is worse than lower coverage over behavior tests.
