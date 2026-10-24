# Code Review Checklists

## Author pre-flight (before requesting review)

- [ ] Change is **small** (~200 LOC) and covers **one** concern.
- [ ] Not a mix of refactor + behavior change + bug fix.
- [ ] Presubmit passes: formatter, linter, fast tests all green.
- [ ] Tests added/updated for the changed behavior (regression test for a bug fix).
- [ ] Description: first line = concise summary of the *type* of change; body = **what and why**.
- [ ] No unrelated drive-by edits; no leftover debug code or dead flags.
- [ ] If the change is unavoidably large, it is split into a stack of small, independently reviewable pieces (plumbing disabled first).
- [ ] Reviewer set is minimal (the right owner + one competent reviewer).

## Reviewer checklist

Correctness & design
- [ ] Does it do what the description says? Any missing edge cases?
- [ ] Error/failure handling present and sensible?
- [ ] Any behavior that would surprise a caller (Hyrum's Law risk)?
- [ ] Does it duplicate something that already exists? (Reuse over rewrite.)

Tests
- [ ] New/changed behavior is covered by tests.
- [ ] Tests check behavior via public API, not implementation details.
- [ ] Bug fix includes a regression test.

Readability & consistency
- [ ] Names, structure, and idioms match the codebase and language style guide.
- [ ] Reader can reason locally without chasing the whole system.
- [ ] Comments explain *why* where the code can't.

Scope & process
- [ ] Change is small and single-purpose; if not, request a split.
- [ ] Description explains *why*, not just *what*.
- [ ] Formatting/style nits deferred to tooling or marked "nit:" (non-blocking).

## Comment etiquette

- Ask about the code, not the author: "Can this loop be simplified?" not "you overcomplicated this."
- Separate **blocking** from **optional**: prefix optional preferences with `nit:`.
- Give a concrete suggested fix, not just an objection.
- Respond / re-review within one working day. Use `PTAL` after addressing comments.
- Approve changes that improve the codebase; don't hold out for perfection.

## Change description template

```
component: short summary of the type of change

What: <what this change does>
Why: <the problem it solves / bug it fixes / reason it's needed>
Notes: <rollout flag, follow-ups, anything a future reader needs>
```
