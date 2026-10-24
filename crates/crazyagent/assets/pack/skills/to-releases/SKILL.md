---
name: to-releases
description: >-
  Slice a product scope into shippable releases (v1, v2, ...) with theme, users, scope
  in/out, timeline, metrics, dependencies. Use on "what should v1 be", "release plan",
  "roadmap", "MVP cut".
---
 
# To Releases
 
Slice a product scope into a sequence of releases. Each release ships something valuable on its own — not a horizontal layer of the stack, but a thin vertical cut that gets a real user a real outcome.
 
This skill applies the Linear Method to release planning. The principles that matter most here:
 
- **Launch multiple times.** Each launch compounds interest and brings users who help shape future launches.
- **Launch early.** v1 doesn't need to be fit for everyone — it needs to be fit for someone.
- **Solve problems, not features.** Each release should solve a real user problem end-to-end, even if narrowly.
- **Deferral is a feature, not a failure.** Launching with one auth method, one currency, one platform is fine.
- **Small scope forces prioritization.** A release attempting everything ships nothing.
If the `linear-method` skill is installed, see it for fuller context.
 
## Process
 
### 1. Read the scope
 
Work from whatever is already in the conversation context (uploaded file, pasted text, prior discussion). If the user provides a path, read the file. Don't interview — synthesize what's already there.
 
### 2. Identify v1 — the smallest valuable launch
 
Ask: what is the thinnest possible cut of this product that gets a real user a real outcome? Most things in the scope don't belong in v1.
 
Heuristics for what to cut from v1:
 
- **Features that depend on having an audience.** Public profiles, viral mechanics, social feeds — none of these matter until users exist.
- **Features that depend on accumulated data.** Dashboards, charts, recommendation engines need a population of data first.
- **Features with free workarounds.** A user can paste a venmo link in a comment — no need for a public profile field in v1.
- **Polish that doesn't change what's possible.** Theming, animations, multi-currency are usually v2+.
- **Optional integrations.** Third-party auth, fancy maps integrations, etc. — start with the basics.
### 3. Sequence subsequent releases
 
Each release after v1 should:
- Have one coherent theme — one new thing the product can now do
- Build on what's already shipped — no orphaned features
- Have a target user, even if the same as v1
Order by:
- What unblocks adoption first (often the core promise of the product)
- What compounds with what's already shipped
- What can be deferred without hurting earlier users
### 4. Draft each release
 
Use the template below. Save to `./output/releases/v<N>-<short-slug>.md`. The slug is a kebab-case codename (e.g., `v1-split-and-settle.md`).
 
<release-template>
# v<N> — <Codename or Theme>
 
## Theme
 
One sentence describing what this release lets users do that they couldn't before.
 
## Target users
 
Who specifically benefits. Narrow > broad. "Everyone" is a smell.
 
## Value proposition
 
Why a target user would care. What problem this solves, in their words if possible.
 
## In scope
 
Bulleted list of what's included. Keep this short — if it's growing past five or six items, the release is probably too big.
 
## Out of scope
 
What is explicitly NOT in this release, especially the things people will ask "wait, isn't that part of this?" Make the deferral explicit so it doesn't get re-litigated later.
 
## Success metrics
 
Measurable indicators that this release worked. ("50 users splitting at least one expense in week one" beats "users love it".)
 
## Rough timeline
 
Weeks of work, not calendar dates. Reserve ~20% for unplanned work and quality.
 
## Dependencies
 
Which prior releases must ship first. Or "None — this is v1".
 
## Constraints & risks
 
What could derail this. Technical risks, scope creep risks, dependency risks.
 
## Open questions
 
Genuine ambiguities that need to be resolved before this can start. Don't paper over things you don't actually know.
 
</release-template>
### 5. Show the sequence and quiz the user
 
Before saving, present a summary table:
 
| Version | Theme | Rough timeline | Key cuts |
|---|---|---|---|
| v1 | ... | ~4 weeks | Public profile, charts, theming |
| v2 | ... | ~3 weeks | ... |
 
Then ask:
- Does the v1 cut feel small enough to actually ship?
- Are the deferrals defensible — would a v1 user be upset by what's missing?
- Is the sequence right? Anything that should move earlier or later?
- Any release without a single clear theme (sign of being too broad)?
Iterate until the user approves, then write the markdown files.
 
### 6. Offer to publish to Linear
 
After saving, ask: "Want me to publish these as Linear documents so the team can see them?"
 
If yes, recap one line per release ("v1 — Split & Settle, ~4 weeks") and confirm once more before calling MCP tools. Then for each release:
 
- Call `save_document` (Linear MCP) with the release title and the markdown body
- If the workspace has an obvious "Releases" or "Roadmap" doc collection, use it; otherwise leave at the workspace root and tell the user where it landed
If the user says no, confirm the markdown files are in `./output/releases/` and stop.
 
## Anti-patterns to avoid
 
- **"Everything in v1".** If v1 has more than five or six in-scope items, it isn't v1.
- **Horizontal slicing.** "v1 = backend, v2 = frontend" is wrong — each release needs a vertical cut delivering user value.
- **Releases without a user.** If shipping a release doesn't change what a user can do, it's an internal milestone, not a release.
- **Calendar-dated promises.** Estimate in weeks of work. Calendar dates invite missed deadlines.
- **Skipping v1 because "obviously the MVP is X".** Write the cut down anyway. Forcing yourself to articulate it surfaces the assumptions.
 