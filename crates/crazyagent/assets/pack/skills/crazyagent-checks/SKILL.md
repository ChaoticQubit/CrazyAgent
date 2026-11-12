---
name: crazyagent-checks
description: Run the project's checks on your own changes and read their results through CrazyAgent's checks_run and checks_results tools. Use after a change, before you call work done, and before you ask for another run, reading the failing results first.
---

# CrazyAgent checks

The owner approves a check profile for the project: the commands that test it, and holdout checks you never see. The engine runs them on your own change set, the changes your turns committed, in a sandboxed snapshot, and records each run. Only the owner approves a profile, and only the engine records results.

## Run checks

- Call the `checks_run` tool of the `crazyagent-team` MCP server. Leave `checks` out to run every check the profile runs on request, or name check ids from the profile.
- It runs on your own change set only. You cannot name another agent's change set.
- Each call counts against the hourly limit the owner sets, 6 requests per hour unless the owner changed it. When the tool says you used them, stop asking and work from the results you have.
- The engine runs the profile's turn-end checks after your turns by itself. You do not need to ask for those.

## Read results

- Call the `checks_results` tool once the runs finish. It shows the newest run of each check, the tests that failed and their messages.
- Read every failing result before you ask for another run. Fix what failed first: asking again without a change runs the same thing and spends your limit.
- A protected test's message is withheld, and holdout checks show their outcome only. Do not try to find out what a holdout tests. Make the behavior right instead.

## Never

- Never edit, skip or delete a test to make a check pass, never add skip markers, and never change a path the profile protects. The engine notes added skips and removed tests, and the owner's gate can refuse the change set.
- Never say checks passed unless `checks_results` shows they passed on your current head.
