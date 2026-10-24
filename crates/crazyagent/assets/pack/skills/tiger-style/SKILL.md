---
name: tiger-style
description: >-
  TigerBeetle discipline (assertions, bounds, shape, naming) for Go/TypeScript/Python.
  Safety, then performance, then developer experience. Third leg of gate 4, after
  software-practices and ponytail, on every build/change.
---

Design priority order, in this order, a lower one never wins a tradeoff against a higher one:
**safety, performance, developer experience.**

## Assertions

Assertions detect programmer errors. The only correct way to handle corrupt code is to crash -
an assertion downgrades a catastrophic correctness bug into a liveness bug, caught at the moment
the wrong belief was acted on instead of an arbitrary number of steps later.

- Assert all function arguments and return values, preconditions, postconditions, and invariants.
- Assert the **positive space** you do expect, and the **negative space** you do not expect. The
  negative space is where the bug lives - the state everyone assumed was impossible.
- **Pair assertions.** For every property worth enforcing, find at least two different code paths
  where it can be checked - assert before a write and again after the matching read, at the
  caller and at the callee. A pair is an airlock: one side alone lets a bug through, both sides
  together don't.
- Split compound assertions (`assert a && b` hides which one failed). Assert compile/build-time
  constants too, not just runtime values. `maybe(cond)` marks a state that's deliberately
  possible, so it reads differently from a state that would be a bug.
- Assertion density: **average at least two assertions per function.** This is a floor, not a
  ceiling - trivial glue (a one-line wrapper, a pure getter) doesn't need to hit it, but real
  logic should clear it comfortably.
- **An assertion is a written-down belief, not a fallback.** No `else`, no recovery path, no
  catch that logs and continues. If the code could do something sensible on failure, that's
  error handling and the normal rules apply - assertions are for what should be structurally
  impossible, not for conditions a caller can legitimately trigger.
- Assertions stay **enabled in production**, in every language here: Go `panic`, never a build
  tag that strips it; Python `raise`, never a bare `assert` (`-O` strips those); TypeScript a
  throwing `invariant()` helper, never a comment. An assertion that only runs in dev isn't a
  belief, it's a hope.
- Assertions are a safety net, not a substitute for understanding the code. Build the mental
  model first: what must be true here, and why. The assertion writes that belief down; it
  doesn't create it.

## Bounds

- **Put a limit on everything.** Every loop, every queue, every retry, every page size, every
  buffer gets a fixed upper bound. Unbounded retries, unbounded pagination, and unbounded queues
  are among the most common real production failures in web services - this is the rule that
  catches them at design time instead of at 3am.
- Only simple, explicit control flow. Recursion isn't banned outright here (unlike the original,
  which forbids it absolutely) - bound the depth explicitly instead, since Go/TS/Python don't
  give you the stack guarantees Zig's authors were relying on.
- All errors are handled - not logged-and-ignored, not swallowed. Roughly 92% of catastrophic
  distributed-systems failures trace back to a mishandled *non-fatal* error (OSDI '14), not an
  exotic one.
- Explicitly-sized/typed values where the language supports it; don't let an implicit widening or
  truncation be the reason a bound doesn't actually bound anything.

## Shape

- Hard limit: **70 lines per function.** 100 columns per line.
- Push `if`s up and `for`s down - hoist branching toward the top of a function, push loops toward
  the leaves.
- Keep leaf functions pure where practical.
- Smallest possible scope for every variable.
- Compute near use (POCPOU) - a value gets computed right before the code that needs it, not
  hoisted to the top "for tidiness."

## Naming

- Units and qualifiers last, descending significance: `latency_ms_max`, not `max_latency_ms`.
- Give paired/derived names equal length (`source`/`target`, not `src`/`dest`) so they visually
  line up wherever they appear together.

## Performance

- The best time to solve a performance problem is at design time, which is exactly when nothing
  can be measured or profiled yet - sketch the back-of-envelope cost before writing the code, not
  after.
- Batch where the workload is a writer/queue (DB writes, event publishing); don't manufacture
  batching for a CLI or a one-shot script where there's nothing to batch.
- Don't react directly, inline, to every external event - a burst of events shouldn't turn into a
  burst of synchronous work if it can be coalesced.

## Policies

- **Zero technical debt.** Do it right the first time rather than shipping a known-wrong version
  and a ledger entry to fix it later. This is a deliberate override of ponytail's usual
  ship-the-lazy-version-and-mark-it move - see Boundaries below.
- Explicitly pass options to library/API calls at the call site instead of relying on a default -
  a default can change out from under the call; an explicit value can't.
- Always motivate a non-obvious decision - say why, not just what, in the code or the commit that
  introduces it.

## Not adopted here (documented, not applied)

The original is written for a single-binary Zig database and some of it has no meaning in this
stack:

- **Static allocation at startup, no allocation after init** - no lever in a garbage-collected
  language; don't chase this in Go/TS/Python.
- **Zero dependencies** - a from-scratch-database policy, not a web-service one. Ponytail's
  "installed dependency before new code" already covers the sane version of this for us.
- Zig-specific mechanics: `zig fmt`, snake_case, out-pointer initialization, `*const` - not
  portable, ignore.
- VOPR/deterministic-simulation testing is referenced in the source doc but not specified there
  either; not something to build a harness for on the strength of a one-line mention.

## Boundaries

This is the third leg of gate 4, run after software-practices and ponytail on every build/change -
see `CLAUDE.md`. Where this skill and ponytail give opposite instructions, **this skill wins**,
decided explicitly by the user rather than left as a default:

- The two-assertions-per-function floor stands, even where ponytail would call it ceremony on
  something small.
- Zero technical debt stands: don't ship a `ponytail:`-marked shortcut and defer it, get it right
  now.

Everywhere else the two skills agree (an assertion refuses to handle what it caught, and ponytail
already prefers no fallback branch over five defensive ones) or don't overlap at all (ponytail's
YAGNI/dependency ladder, this skill's assert/bound/shape discipline) - no override needed, both
apply as written.
