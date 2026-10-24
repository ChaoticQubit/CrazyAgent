---
name: code-review
description: >
  Google code review practice (SWE at Google ch. 9): making a change reviewable, how big a PR/CL should be, writing the change description or commit message, reviewing someone else's, review policy (approvals, OWNERS, readability), giving and receiving feedback.
---

# Code Review (Google-style)

At Google, **every change is reviewed before it lands**, and every engineer both authors and reviews. The goal of review is not gatekeeping perfection — it is a second engineer confirming the change is correct and comprehensible, plus keeping the codebase consistent and knowledge shared. Optimize the process to stay **lightweight and fast** so it scales.

## The approval model

A change needs three "bits" of approval before commit. One person can supply all three; by default a single reviewer is enough.

1. **LGTM ("looks good to me")** — another engineer confirms correctness and that they understood the change. **Only one LGTM is required by default.** More reviewers hit diminishing returns fast.
2. **Owner approval** — someone in the `OWNERS` file for the affected directory approves. OWNERS files are hierarchical and additive per directory.
3. **Readability** — the change conforms to the language's style and best practices (often certified via a "readability" program). Language consistency across the whole codebase, taught through review.

## Rules for authors

1. **Write small changes.** Target **~200 lines of code**, one focused, self-contained issue per change. Small changes review faster, bisect bugs more easily, and roll back cleaner. Most changes should be reviewable within about a day; ~35% of Google changes touch a single file. If a change is big, split it (land plumbing disabled first, then wire it up — see `trunk-based-development` and `feature-flags`).
2. **One change = one concern.** Don't mix a refactor with a behavior change with a bug fix. Keep bug fixes atomic (the fix + a regression test) so they roll back independently.
3. **Write a good change description.** The first line is a short summary of *the type of change*; the body explains **what changed and why**. "Bug fix" is not a useful description — say which bug and why this fixes it. The description is read for the life of the code.
4. **Respond to review comments within 24 working hours.** Don't leave a reviewer waiting; don't answer piecemeal across days. Treat each comment as a TODO to resolve; use "PTAL" (please take another look) when you've addressed them.
5. **Reuse before you write.** "Code is a liability, not an asset." Prefer existing solutions over new code; avoid duplication. But a code review is not the place to re-litigate a settled design decision.
6. **Automate the mechanical stuff.** Run linters, formatters, and the fast test suite as **presubmit** checks so human review time is spent on logic and design, not style nits a tool can catch.

## Rules for reviewers

1. **Be polite, professional, and timely.** Respond within one working day. Frame comments constructively — ask about the code ("I'm confused by this control flow — can it be simplified?") rather than attacking the author ("you got this wrong"). "You are not your code."
2. **Defer to the author on approach.** Suggest an alternative only when it genuinely improves comprehension or functionality — not personal preference. Approve a change that **improves the codebase** even if it isn't perfect; don't hold out for an idealized version.
3. **Comprehension questions are the reviewer's right and the author's signal.** If a reviewer had to ask, the code (or a comment) probably needs to be clearer — "the customer is always right" about confusion.
4. **Keep the number of reviewers minimal.** The first LGTM delivers most of the value.
5. **Don't block on nits.** Mark trivial style preferences as optional ("nit:"). Rely on formatters/linters for formatting; don't debate it by hand.

## Review by change type

- **Greenfield / new code:** expect a design review first, full unit tests, an OWNERS file, docs, and CI wired up.
- **Behavioral change:** tests must be updated to reflect the new behavior.
- **Bug fix:** fix *only* the bug and add a regression test; keep it atomic so it can be rolled back cleanly.
- **Refactoring:** should be behavior-preserving and usually machine-generated; existing tests should not need to change (if they do, it wasn't a pure refactor).
- **Large-Scale Change (LSC):** machine-generated, reviewed in shards. As a reviewer of a shard touching *your* code, limit comments to concerns specific to your code — don't veto the overall LSC. (See `engineering-principles`.)

## When acting as the reviewer for the user's change

- Check: correctness, edge cases, error handling, test coverage of the changed behavior, clarity/naming, and whether it belongs in one change or should be split.
- Verify the change is small and single-purpose; if not, recommend how to split it.
- Confirm the description explains **why**, not just what.
- Prefer concrete, actionable comments with a suggested fix over vague objections.
- Distinguish blocking issues from optional "nit:" suggestions.

See `references/review-checklist.md` for an author pre-flight checklist and a reviewer checklist.
