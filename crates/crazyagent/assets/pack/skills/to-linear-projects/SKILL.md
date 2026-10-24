---
name: to-linear-projects
description: >-
  Split a product scope into one markdown per Linear project (auth, payments, frontend,
  infra): title, description, goals, milestones, owner, color, dependencies. Use on
  "set up Linear projects for this".
---
 
# To Linear Projects
 
Take a product scope and produce a set of Linear project briefs — one per natural area of work. In this skill's vocabulary, a "project" is a categorical area (Authentication, Payments, Frontend, Data Platform), not a strict 1–3 week chunk. Within each project, milestones capture the smaller time-bounded chunks that match Linear Method's project-sizing guidance.
 
This is orthogonal to `to-releases`: projects describe **what areas of work exist**, releases describe **when things ship**. A single project (e.g., Authentication) typically has milestones spread across multiple releases.
 
This skill applies the Linear Method to project organization. The principles that matter most here:
 
- **Meaningful direction.** Every project should clearly state why it matters and what success looks like.
- **One named owner.** Every project has one person responsible for the brief and delivery, even if many contribute.
- **Specs force scoping.** Writing the project brief surfaces what belongs and what doesn't.
- **Plain language.** Use the project's domain vocabulary. Don't invent jargon.
- **Reserve room for unplanned work.** Project timelines should leave space for things that come up.
If the `linear-method` skill is installed, see it for fuller context.
 
## Process
 
### 1. Read the scope
 
Work from whatever is already in the conversation context (uploaded file, pasted text). If the user passes a path, read the file.
 
### 2. Identify the natural project boundaries
 
A good project:
- Covers one coherent area of capability
- Has a single team or owner who can be accountable
- Can accumulate milestones over time as the product evolves
- Doesn't overlap heavily with other projects (some boundary blur is fine)
Common axes for software products:
- **Capability areas** (Authentication, Payments, Notifications, Search)
- **Surface areas** (Mobile App, Web App, Public API)
- **Platform layers** (Infrastructure, Observability, CI/CD)
- **Domain features** (e.g. for a finance app: Expense Tracking, Splitting, Places & Profile)
- **Cross-cutting concerns** (Design System, Testing, Security & Compliance)
Aim for 6–12 projects total for a sizable product scope. Fewer than five usually means projects are too broad; more than fifteen means likely overlap or projects too granular.
 
### 3. Draft each project
 
Use the template below. Save each to `./output/projects/<project-slug>.md` (e.g. `auth-and-authorization.md`).
 
<project-template>
# <Project Title>
 
**Lead:** TBD
**Status:** Backlog
**Color:** <suggest one — pick a varied palette across projects>
 
## Description
 
Two to four sentences. What this project covers, why it exists, and how it fits into the broader product. Use the project's domain vocabulary.
 
## Goals
 
What success looks like for this project. Measurable where possible. Outcomes, not a feature list.
 
- ...
- ...
## Milestones
 
Time-bounded chunks of work within this project. Each milestone should be roughly 1–3 weeks of work for 1–3 people, in line with Linear Method's project-sizing guidance. Milestones are the unit at which this project ships value, even though the project itself is long-running.
 
| # | Milestone | Rough size | Maps to release |
|---|---|---|---|
| 1 | ... | ~2 weeks | v1 |
| 2 | ... | ~1 week | v1 |
| 3 | ... | ~3 weeks | v2 |
 
## Dependencies
 
Other projects this depends on, and projects that depend on this one. Or "None — can run independently".
 
- **Depends on:** ...
- **Blocks:** ...
## Out of scope
 
What does NOT belong in this project even though someone might think it does. Make the boundary explicit.
 
## Open questions
 
Things to decide as this project gets going.
 
</project-template>
**Color palette suggestion** — when assigning colors, pick from a varied palette so projects are visually distinct in Linear. Reasonable options: indigo, emerald, amber, rose, sky, violet, lime, orange, teal, fuchsia.
 
### 4. Show the project list and quiz the user
 
Before saving, present a summary table:
 
| Project | Lead | # of milestones | Mapped releases |
|---|---|---|---|
| Authentication & Authorization | TBD | 4 | v1, v2, v5 |
| Frontend (Flutter) | TBD | 6 | v1, v2, v3, v4 |
 
Then ask:
- Are the project boundaries right, or are any overlapping?
- Are there areas of the scope that don't fit cleanly into any project? (Indicates a missing project.)
- Any projects too small to justify being top-level? (Could be milestones inside another project.)
- Any too broad? (Could be split into 2–3 projects.)
Iterate until the user approves, then save the markdown files.
 
### 5. Offer to publish to Linear
 
After saving, ask: "Want me to publish these as Linear projects?"
 
If yes, recap one line per project and confirm once more before calling MCP tools. Then for each project:
 
- Call `save_project` with title, description, status (`backlog`), and color
- For each milestone in the project's milestones table, call `save_milestone` with the project ID
- If the user has a specific team for these projects, set the team ID; otherwise ask once which team to put them under
If the user says no, confirm the markdown files are in `./output/projects/` and stop.
 
## Anti-patterns to avoid
 
- **One mega-project.** "Backend" or "Frontend" alone is usually too broad. Split by capability area.
- **Projects without an owner-shape.** If you can't picture one person being accountable, the boundary is wrong.
- **Projects that are really features.** "Add Apple Pay support" is a feature/issue, not a project. A project should accommodate ongoing work over months.
- **Inventing jargon.** Use words from the user's scope document. Don't rename "Expenses" to "Transactions" because it sounds more enterprisey.
- **Skipping milestones.** A project with no milestones has no scope; it's just a folder. Always include a milestone breakdown.
 