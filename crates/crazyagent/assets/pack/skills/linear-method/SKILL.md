---
name: linear-method
description: >-
  Linear Method for issues, specs, project briefs, cycle planning, backlog and
  prioritization, roadmaps, team workflow, product direction.
---
 
# SKILL.md — The Linear Method: Project Tracking & Product Building
 
> **Purpose:** Apply the Linear Method's principles and practices when planning projects, writing issues, managing cycles, setting product direction, or advising on software team workflows. This skill encodes the philosophy from [linear.app/method](https://linear.app/method).
 
---
 
## When to Use This Skill
 
Use this skill when the user is:
- Setting up or improving their project tracking system
- Planning product initiatives, roadmaps, or goals
- Writing issues, specs, or project briefs
- Deciding how to structure cycles/sprints
- Managing backlogs or prioritizing work
- Designing team workflows for cross-functional collaboration
- Thinking about how to launch or ship products
- Asking about best practices for product development
---
 
## Core Principles
 
1. **Build for the creators.** Optimize tools and processes for the people doing the work, not for generating reports.
2. **Purpose-built over flexible.** Opinionated workflows beat infinite customization. Flexibility creates chaos at scale.
3. **Momentum over sprints.** Maintain a healthy, sustainable cadence. Don't rush toward deadlines — build a rhythm.
4. **Meaningful direction.** Connect every task to larger goals. Everyone should know *why* their work matters.
5. **Clarity in language.** Use plain terms. Projects are projects. Don't invent jargon.
6. **No busy work.** Automate or eliminate "work around work." Tools serve you, not the other way around.
7. **Simple first, powerful later.** Start simple and add complexity only as the team scales.
8. **Decide and move on.** Avoid analysis paralysis. An imperfect decision now beats a perfect decision never.
---
 
## Product Direction
 
### Setting Initiatives
- Define a clear set of product initiatives that articulate your vision and execution path.
- Initiatives should be ambitious — slightly out of reach — to push people to do their best work.
- Anyone in the company should be able to look at initiatives and understand what matters most, why, and how it's progressing.
- Reserve space in timelines for unplanned work. Allow plans to change.
### Setting Goals
- Even without enough data, set a goal that propels you forward in a measurable way.
- Work backward from goals: path to 10 users starts with 1 user, which starts with having a findable, usable product.
- Start small, figure it out, then scale.
### Prioritization: Enablers vs. Blockers
- **Enablers:** New functionality that makes the product more valuable.
- **Blockers:** Gaps or friction preventing users from using the product.
- Always ask: Is this truly preventing someone from using the product, or is it nice-to-have?
- Prioritize what moves the needle *this week or month*, not someday.
- Consider compounding effects and added complexity before building.
- It's okay to defer non-critical features (e.g., launch with Google Login only, add email login later).
### Scoping Projects
- Target: **1–3 weeks, 1–3 people** per project.
- Smaller fixes: hours or a day.
- If a project can't be scoped down, break it into stages.
- Small scope forces prioritization of the most important feature set and creates fast feedback loops.
- Early in product building, you can't predict impact well — avoid massive bets.
---
 
## Building & Execution
 
### Generate Momentum
- Take swift action daily. Do it today instead of tomorrow.
- When unsure, trust your intuition and act. Talk to users. Clarity comes with feedback.
- Correct or revert decisions — that's cheaper than standing still.
- **Startups die from moving too slow or giving up, not from making too much progress.**
### Writing Issues (Not User Stories)
User stories are considered an anti-pattern. Write short, simple issues in plain language instead.
 
**Rules for good issues:**
- **Concrete tasks with clear outcomes.** If it's not a task, it doesn't belong in the issue tracker.
- **Short, scannable titles.** Directly state the task. Descriptions are optional.
- **Write your own issues.** The person doing the work writes the issue. This forces deep thinking about the problem.
- **Quote user feedback directly** instead of summarizing. Link to the customer conversation.
- **For issues written for others** (e.g., bug reports): frame as an ask or describe the problem. Let the assignee devise the solution.
- **Keep UX discussions at the product/spec level**, not the task level. The team should understand user needs from project specs, not individual issue descriptions.
**The Linear process:** Discuss features deeply → project owner writes specs and gathers feedback → iterate until the approach feels right → then straight into execution. Individuals write their own issues.
 
### Managing Design Projects
1. **Verify the problem.** Don't take feature requests at face value. Investigate to find the root cause.
2. **Explore freely.** Create an "Explore designs" placeholder issue. Don't judge feasibility yet. Bad ideas clarify thinking.
3. **Get early feedback.** Share work-in-progress. Ask "why" behind feedback. Alternate between overall direction review and detail-level input.
4. **Choose a direction.** Involve engineers early — they spot technical limitations and suggest alternatives.
5. **Collaborate, don't hand off.** Designers and engineers work together throughout. Use sub-issues to split design/engineering tasks.
### Building with Users
- **Balance vision and feedback.** Too vision-driven misses market needs. Too reactive creates Frankenstein products.
- **Solve problems, not features.** When users request a feature, ask: What's the problem? What's the use case? Pivot from solutions to pain points.
- **Build for your target users.** If building for startups, don't let enterprise feedback set your direction.
- **Let feedback refine, not dictate.** Strategic initiatives balance user needs with company needs.
### Launching
- **Launch multiple times.** Don't wait for a single perfect moment. Each launch compounds interest.
- **Launch early.** Start getting users and momentum. Your product doesn't need to be fit for everyone.
- **Each launch builds following** that helps future launches reach more people.
### Building in Public
- **Publish a changelog** even with few users. It encourages constant shipping, shows progress to users and investors, and boosts team morale.
- Showing what you're building can discourage competition or force them to play catch-up.
---
 
## Operational Patterns
 
### Cycles
- Use **2-week cycles** (most common). Short enough to stay focused, long enough to build meaningful features.
- Cycles should feel reasonable — don't overload them.
- Let unfinished items auto-move to the next cycle.
### Backlog
- Keep it manageable. Don't save every request indefinitely.
- Important items resurface. Low-priority items never get done.
- A focused backlog makes cycle planning faster and ensures work gets done.
### Quality
- Mix feature work and quality work (bugs, fixes, tooling) in every cycle.
- Tooling investment is a force multiplier.
### Ownership
- Every project has **one named owner** responsible for the brief and delivery.
- Every issue has **one owner**, even if others contribute.
### Specs
- Keep them brief. Short specs get read.
- Communicate the **why**, **what**, and **how**.
- Specs force scoping so priorities are clear and teams avoid building the wrong thing.
### Progress Measurement
- Measure with actual work output (diffs in code or design files), not status updates.
- Small scope = small changes = easier review.
- Avoid massive PRs or design changes.
### Cross-Functional Teams
- Designers and engineers on the same project team.
- Designers push thinking and explore ideas. Engineers challenge implementation and bring ideas to reality.
---
 
## Quick Reference: Anti-Patterns to Avoid
 
| Anti-Pattern | Better Approach |
|---|---|
| Writing user stories | Write plain-language issues with clear tasks |
| One massive launch | Launch multiple times, compounding interest |
| Overloaded backlogs | Prune ruthlessly; important items resurface |
| Overloaded cycles | Keep cycles reasonable; auto-roll unfinished work |
| Massive projects (months-long) | Scope to 1–3 weeks, 1–3 people |
| Hard design-to-engineering handoff | Collaborate throughout; use sub-issues |
| Building requested features literally | Investigate the root problem; solve that instead |
| Waiting for perfect data to set goals | Set a goal that propels forward in any measurable way |
| Analysis paralysis | Decide and move on; correct course later |
| Jargon and invented terminology | Use plain, clear language everyone understands |
 