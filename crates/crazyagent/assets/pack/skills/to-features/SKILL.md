---
name: to-features
description: >-
  Expand one release markdown into per-feature/epic markdowns: problem, solution, user
  stories, acceptance criteria, out of scope, dependencies, open questions. Use on
  "break this release into features/epics".
---
 
# To Features
 
Take a release plan and break it into the discrete features (or epics, in Linear's vocabulary) that make up that release. Each feature is a coherent piece of user-visible functionality that can be specced and assigned to a team.
 
Features are NOT vertical slices yet — that's `to-issues`'s job. Features are the bridge between "v2 is about personal finance tracking" and "implement the dashboard chart component".
 
This skill applies the Linear Method to feature-level specs. The principles that matter most here:
 
- **Solve problems, not features.** Even when writing a "feature", anchor it in the user problem it solves.
- **Specs force scoping.** Writing the spec surfaces gaps and forces priorities to be clear.
- **Brief is better.** Short specs get read. Communicate why, what, and how — not every implementation detail.
- **One owner per feature.** Even if multiple people contribute, one person is responsible.
- **User stories are useful at the spec level**, not the issue level. Use them here to articulate user value, but resist treating them as task descriptions.
If the `linear-method` skill is installed, see it for fuller context.
 
## Process
 
### 1. Read the release file
 
Take the release markdown the user references (path in their message, file in conversation context, or the most recent output of `to-releases`). Read the whole thing — theme, in scope, out of scope, dependencies. The "Out of scope" section matters as much as "In scope": features that try to pull in deferred items break the release plan.
 
### 2. Identify the features
 
A feature is a coherent piece of user-visible functionality that:
- Maps to one or more user stories
- Has clear acceptance criteria
- Could be assigned to one person or a small group as a unit of work
- Typically takes 1–3 weeks to ship end-to-end (in line with Linear Method's project sizing)
If a "feature" feels like 6+ weeks, split it. If two "features" can't be tested independently, they might be one feature with sub-parts.
 
For each candidate feature, ask:
- What user problem does this solve?
- What does "done" look like from a user's perspective?
- What other features in this release does it depend on?
- What's deliberately NOT part of this feature?
### 3. Draft each feature
 
Use the template below. Save to `./output/features/<release-slug>/<feature-slug>.md` (e.g. `./output/features/v1-split-and-settle/expense-splitting.md`).
 
<feature-template>
# <Feature Title>
 
**Release:** v<N> — <Release Codename>
**Status:** Draft
**Owner:** TBD
 
## Problem
 
The user-side problem this feature solves. One or two sentences. Anchor in the user, not the implementation.
 
## Solution overview
 
What this feature does, from the user's perspective. Two or three sentences.
 
## User stories
 
A numbered list. Format: "As a <user>, I want <capability>, so that <benefit>."
 
These describe user value, not tasks. They're a checklist for "have we covered the relevant cases", not work units.
 
1. As a ..., I want ..., so that ...
2. As a ..., I want ..., so that ...
## Acceptance criteria
 
Checklist of observable behaviors that must be true for this feature to be "done". Behavior, not implementation.
 
- [ ] ...
- [ ] ...
## Out of scope
 
What is deliberately NOT in this feature. Things that would feel related but are deferred, or belong in a different feature.
 
## Dependencies
 
Other features in this release (or earlier releases) that must be done first. Or "None — can start immediately."
 
## Technical considerations
 
Architectural choices, third-party integrations, data model decisions, or known constraints the implementing team needs to know. Keep this brief — it's a flag, not a design doc.
 
## Open questions
 
Things not yet decided that should be resolved before implementation starts.
 
</feature-template>
### 4. Show the feature list and quiz the user
 
Before saving, present a summary table:
 
| # | Feature | Rough size | Depends on |
|---|---|---|---|
| 1 | ... | ~1 week | None |
| 2 | ... | ~2 weeks | #1 |
 
Then ask:
- Are any features sized too big? (>3 weeks suggests it should be split)
- Are any features overlapping or genuinely the same thing?
- Are there gaps — anything in the release's "In scope" not covered?
- Any feature pulling in deferred (out-of-scope) items? Those need to either drop or move to a later release.
Iterate until the user approves, then write the markdown files.
 
### 5. Offer to publish to Linear
 
After saving, ask: "Want me to publish these as Linear issues (epics) under the release?"
 
If yes, recap one line per feature and confirm once more before calling MCP tools. Then:
 
- For each feature, call `save_issue` with the title and a body composed from the markdown
- Apply an "epic" label if the workspace uses one (check via `list_issue_labels` first)
- If the user has a project for this release, set the project ID on each issue
- Otherwise create the issues at the workspace level and tell the user where they landed
If the user says no, confirm the markdown files are in `./output/features/<release-slug>/` and stop.
 
## Anti-patterns to avoid
 
- **Re-introducing deferred scope.** A feature in v2 that pulls in something the v2 release doc said was out of scope is a sign of weak release boundaries. Push back instead of papering over it.
- **Features that aren't user-visible.** "Refactor the auth module" isn't a feature — it's a tech-debt issue. Features deliver user-visible outcomes.
- **Acceptance criteria written as tasks.** "Add a button to the toolbar" is a task. "User can save an expense from the dashboard in one click" is acceptance criteria.
- **User stories as tasks.** Don't write "As a developer, I want to write tests..." — that's not a user story. Save tasks for `to-issues`.
- **Massive features.** If a feature feels like a small project on its own, split it into 2–3 features that ship independently.
 