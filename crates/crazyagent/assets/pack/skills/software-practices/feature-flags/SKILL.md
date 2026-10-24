---
name: feature-flags
description: >
  Flag-guarded release (SWE at Google ch. 23-24): ship incomplete work to main behind a toggle, feature flag or kill switch, dark launch, canary or staged rollout, decouple deploy from release, roll back by config, retire stale flags.
---

# Feature Flags & Flag-Guarded Releases (Google-style)

Feature flags are what make trunk-based development and continuous delivery safe: they let unfinished or risky code live on trunk and in the production binary while staying **off** until it is ready, and they let you turn a feature off via a config change without rebuilding or redeploying.

## Core principle

> "A key to reliable continuous releases is to make sure engineers 'flag guard' all changes."

New feature code ships in the binary **alongside the existing code path**, guarded by a flag. If the new path works, delete the old one. If it breaks, **flip the flag via a dynamic config update — independent of the binary — with no re-release.** This makes the flag your fastest rollback and decouples *deploying* code from *releasing* behavior.

## Rules

1. **Flag-guard all non-trivial changes.** Incomplete work lands on trunk disabled behind a flag instead of hiding on a branch. A disabled flag also lets build tooling strip the feature from release builds.

2. **Decouple deploy from release.** "Deploy" = the binary is running in prod. "Release" = users can see the behavior. The flag is the seam between them. This is what lets you continuously integrate at head without exposing half-done features.

3. **Never flip a flag to 100% at once.** Use a config service that does safe, **staged** rollouts (e.g., 1% → 5% → 25% → 50% → 100%), watching health metrics at each step. Ramp down instantly if metrics regress.

4. **Prefer dynamic config over static.** Where possible, drive behavior with experiments/flags read at runtime so you can change it without a deploy. For static config, keep it in version control and promote it as part of the release candidate — a large share of production outages are configuration bugs, so config must be versioned, reviewed, and tested like code.

5. **Roll back by config, not by rebuild.** The point of the flag is that mitigation is a config edit, not an emergency binary push. Keep every guarded feature independently switchable.

6. **Flags are not secrecy.** Guarded code can still be scraped/inspected from the shipped binary. Don't use flags to hide truly confidential features; use them for rollout control and safety.

## Rollout patterns

- **Dark launch:** turn the feature *on* server-side (it runs, exercises load, writes shadow data) but keep its output hidden from users. Validates performance and correctness under real traffic before exposure.
- **Canary:** enable for a small percentage / one region / one cell first; compare canary metrics against the baseline before widening. Watch for **version skew** — old and new versions running simultaneously must interoperate.
- **Staged / percentage rollout:** ramp the flag through increasing user percentages with automated metric gates.
- **A/B experiment:** flag defines the experiment arm; make ship/no-ship decisions on statistically significant quality signals rather than intuition.
- **Change-neutral release:** if your userbase is too small for a meaningful deployment A/B test, flag-guard every new feature so that during a rollout the *only* variable is deployment stability itself.
- **Kill switch:** every risky subsystem gets a flag that disables it fast without a deploy.

## Build/dev vs. release expression

- Express flags differently for **release** vs. **development** builds: stable, shipped features enabled in both; in-development features enabled in dev only and stripped/disabled in release builds.
- This lets developers work against the real trunk binary while users never see unfinished behavior.

## The release train

- Automate release-candidate creation from **green head** (the latest CI-verified commit). An RC is code **plus config**, promoted unchanged across environments — don't recompile as it moves; use containers/orchestration for consistency.
- Set firm feature-submission deadlines: "If you're late for the release train, it will leave without you." A missed feature catches the next train in hours, not days — this protects work-life balance and discourages risky last-minute cramming.
- "No binary is perfect" — launch against clear KPI/error-budget thresholds rather than waiting for perfection.
- Decouple **how often a viable release is built** from **how often a user receives it.** Ship only code that brings a given client value (matters most for native/mobile clients paying storage/data cost).
- **Simply having the machinery for continuous deployment captures most of the value — even if you don't push every build to users.** It requires: production-configurable binaries, config managed as code, dry-run verification, reliable rollback/rollforward, and real-time health/satisfaction metrics.

## Flag hygiene (avoid flag debt)

- **Every flag needs an owner and a planned removal.** A flag is a temporary seam, not permanent architecture.
- Once a feature is fully rolled out and stable, **delete the old code path and then delete the flag.** Leftover flags become dead conditionals, combinatorial test burden, and confusion.
- Track flags like tasks: creation ticket, rollout state, cleanup ticket. Don't let a codebase accumulate hundreds of stale toggles.

## When implementing a flag (language-agnostic checklist)

1. Read the flag from a **runtime** config source (config service / env-backed config), not a compile-time constant, so it can change without a deploy.
2. Default to **off / old behavior**; new path only runs when explicitly enabled.
3. Keep the flag check at a single, well-named decision point; avoid scattering the same condition across the code.
4. Make the new and old paths coexist safely (data written by one must be readable by the other during rollout).
5. Add a metric/log on each branch so canary comparison and debugging are possible.
6. File the cleanup ticket at the same time you add the flag.

See `references/flag-patterns.md` for pseudocode and a rollout runbook.
