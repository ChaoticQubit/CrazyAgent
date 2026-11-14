# Changelog

What went into every CrazyAgent version and build, newest first, in Added, Changed, Fixed and Security lines. Times are UTC and come from release, tag, merge and install records; a version with no recorded time shows its date only.

Status of each version:

- Released: a stable release with a version tag.
- Dev build: a prerelease built from master, with no stable tag yet.
- Internal build: built and used for testing, never released on its own. Its changes reached users in the release it names.
- In progress: work not on master yet.

## [Unreleased]

## [2.4.1] - 2026-11-14 02:36 UTC - In progress (not on master)

- Work merged into the 2.4.1 line from 2026-11-13 04:34 UTC to 2026-11-14 02:36 UTC, last merge d7011e65b. Built on [2.4.0].
- Not built or released yet.
- Still in review, not yet merged into this line:
  - The default master is a harness the pool can run (an Anthropic API key pool leads with Claude Code), and the no-account line names the kind of account to add.
  - A saved master harness switch shows in the master view at once, and a live master terminal no longer stays on "connecting to the terminal".
  - A tool that was found but refused (such as graphify whose Python other users can change) reads "Found, not used" with its reason and a Fix permissions action.
  - A setting for when a model provider can't be reached: Stop at once, Retry briefly or Harness default.
  - Read-only runs fail over past an unreachable local model account.
  - A Claude free reset count reads "couldn't read" instead of "No free resets" when the reply gives no reason about the account.
  - Agents in kept terminals are frozen while the engine is closed and continue once their connection is back.
  - Add folder and New Project answers that arrive late are checked, not read as a lost engine.

### Added

- CrazyAgent ships its own tmux 3.8 on macOS and Linux. The engine uses it first and falls back to the system tmux with a note, and setup shows it as "Bundled with CrazyAgent".
- Node.js is a setup row: when no Node is found, the app can install a pinned Node LTS runtime, SHA-256 checked, from the Node row's Install.
- Node from nvm, Volta, asdf, mise or fnm is found and used.
- First-run Harnesses step after tools, in the app and the TUI, RCLI included. Picked harnesses install in the engine's queue.
- Try again on the tools step after a setup run with failures.
- Settings, Harnesses reads harnesses the same way first run does. A found harness that cannot run says what it needs ("<name> can't run on this Mac.") and offers Node.js through setup.
- A team question names its asker and says Urgent, and "The agent is paused until you answer." while that holds.

### Changed

- Tool lookup order: your own folders first, including those your login shell adds, and the app's own setup and harness folders last.
- Setup checks again as soon as the login-shell lookup finds new tools, and a tool not found yet reads Checking.
- The Overview writing run, the roadmap status check and Edit with AI fail at once with one plain line when a local model provider refuses connections.
- The setup review says what the mapping step could not work out and what to do about it.
- iCloud download progress moves per file.
- A goal typed into the demo's live master terminal ticks its learning step.

### Fixed

- The project Overview showed raw JSON and unclear warnings after setup. A mapping answer or overview that leaves a code fence open is no longer shown, and only a real wait for an account reads as a wait.
- A research job kept only the last part of a long Claude answer. It now keeps the whole answer, a Claude answer cut at its output cap goes on in the same turn, and one long tool line no longer voids a finished report.
- A running engine refused to start masters after a harness file's change time alone moved, such as after a hard link.
- A slow bundled tmux version check no longer holds every terminal start.
- A native lead's teammates' terminal server now runs under a keeper, and its servers end at the quit time limit.
- The urgent question toast's Open puts the keyboard only on the urgent question that is waiting.

### Security

- Harness and tool lookups set temp PATH folders aside, refuse a script whose interpreter really lives in a temp folder, and count an agent's own temp folder as one agents can write.
- On Linux the process's own group is trusted for group-writable folders only when it is your private group.
- The engine's sign-in profiles folder is an engine secret that no agent sandbox can cover.
- Read-only research runs guard the engine folder like every other sandbox.
- The Node archive is kept only when its SHA-256 matches and every link inside it stays under its top folder.
- The old engine tool folder is removed only when it is a real folder you own with private permissions.
- A lead's tmux grant names the tmux file for reading and starting, never its folder.

## [V3] - 2026-11-13 16:59 UTC - In progress (not on master)

- Work from 2026-11-03 to 2026-11-13, last merge 2ec813c80 (2026-11-13 16:59 UTC), which brought in the final [2.4.0].
- Not built or released yet.

### Added

- Tickets: a ticket tracker in every project, with a Tickets tab, a Roadmap tab, board moves that close or reopen a ticket, and ticket rules in Settings, Tickets. The TUI has the same views.
- Masters and agents work tickets through ticket tools. The roadmap can be imported into tickets and tickets exported as Markdown.
- Each ticket can carry acceptance criteria, evidence and a close policy.
- The tracker also runs on its own: a command line tool with JSON output and an MCP server any harness can use.
- Tests: test cases, sets, plans, runs, results and verdicts, read from JUnit, Allure and Rust test output, with coverage and test evidence linked to tickets. A Tests tab in the app and Tests pages in the TUI.
- Checks: check profiles a project runs in a confined runner on a change set snapshot. The change set shows its checks, and an Accept gate can hold a change set until its checks pass.
- Settings, Checks: check rules per app and project, and the whole profile is shown before you approve it.
- Go/no-go scorecard at the end of the app Metrics page, in the app and the TUI.
- Harness settings pages: each harness has its own settings page in Settings and Project Settings, and the settings apply at launch and to master turns.
- Models page per harness, for the app and each project: shipped Claude Code, Codex and RCLI model catalogs, each harness's live model list, your own model ids and model rules.
- The master and agents start on the models you choose and fall back with one notice. You can pick the model of a task you hand out, and task views show the model a start picked and why.
- RCLI master model: an RCLI model id is added with the provider it runs on, tested once on a pool account, and runs only on accounts of that provider.
- Native sub-agent model setting per app and project.
- Notification center: a bell with unread and pending badges on every page, and a panel with filters, a project picker, folded rows and bulk actions. The TUI opens the list with n from any view and shows the counts in its header.
- Settings, Notifications: delivery per kind (action kinds can never be turned off) and macOS notifications Off, Action items only or All.
- The engine reports restarts, a live master turned off or moved, account limits, harness updates and found app updates as notifications.
- App plugins: install plugins into a user folder, with trust records, enable, remove and secret bindings, in Settings, App plugins in the app and the TUI.
- Search: past sessions and memories are indexed for full text search, with an optional local or HTTP embedding plugin for hybrid ranking. Settings, Search picks the plugin and confirms a model download size.
- Recall of saved memories uses the same hybrid ranking and says so.
- Untrusted repository level: a project cloned from a repository you have not reviewed starts at Sandboxed access, and only you can propose its check profile.

### Changed

- Project Overview's Needs you lists the project's pending items and pool account notices from the notification center.
- TUI: Home's New project moves to key p so n can open notifications.
- Removing a project clears its model choices, notifications, search index and team model records.

### Fixed

- A Codex turn that ends in an error now reads a usage or rate limit from the error details as well as the message.
- A full agent slot frees an idle agent on another model, and keep warm starts on the agent default.
- An account hold that cannot be read counts as held for that pick, so a failed read never hands out a held account.

### Security

- Plugin keys reuse the account key store and are a sandbox secret. HTTP embedding calls pass the key only on stdin, follow no redirects and check the reply.
- Plugin manifests are read with a strict YAML subset and validated, and plugin request bodies stay in a guarded engine folder.
- Notification text is redacted when stored and again when served, and macOS banners stop after 256 in one run with one notice.
- Indexed search text is redacted and stored once per message; a missing full text search engine leaves the index empty and unused.
- Untrusted tools-on agents and jobs reach the model only through the engine's proxy, and lowering a repository's trust stops its open IDE.
- Agents read holdout check runs as outcome only, and checks fail closed on strings left open at a line end.

## [2.4.0] - 2026-11-13 19:41 UTC - Dev build

- Version set to 2.4.0 on 2026-11-13 10:00 UTC (dd70a3af2).
- PR [#3](https://github.com/ChaoticQubit/CrazyAgent/pull/3) merged 2026-11-13 16:57 UTC, commit 7ace0bbc9.
- Dev build 2.4.0-dev.202611131658 published 2026-11-13 18:59 UTC, commit 7ace0bbc9. Its macOS notarizing failed first and passed on a rerun. Published as the rolling dev release (since replaced).
- PR [#4](https://github.com/ChaoticQubit/CrazyAgent/pull/4) merged 2026-11-13 19:02 UTC, commit 1b791c409.
- Dev build 2.4.0-dev.202611131902 published 2026-11-13 19:41 UTC, commit 1b791c409, as its own prerelease `build-2.4.0-dev.202611131902`.
- No stable tag yet. Latest stable stays [2.3.2].

### Added

- First run offers a "Learn CrazyAgent" checklist on Home that follows the first steps, groups them by topic, shows N of M done and can be hidden.
- An optional demo project, seeded with a small web page, runs on an account you pick, with a note on what it costs. It is offered on the last first-run page and can be replayed from the Profile dialog.
- Profile offers Show the learning guide again and Run first-run setup again, in the app and the TUI.
- The TUI first run gains an Accounts step after Setup, with Skip for now.
- Free limit resets and credits show for every provider on Pools, Metrics and the TUI usage screen. Each Pools row offers "Use a free reset" when its account holds a fresh one, and a new free reset is announced once.
- Spend confirmation setting: Confirm each (default), One click, or Off. Off never uses a free reset.
- When every account in a pool is at its limit: Notice (default) names the accounts that hold a free reset, No hint shows nothing, and Spend on its own uses one free reset at most once an hour per pool and once per limit window per account.
- User menu: pointing at, focusing or clicking the profile name in the sidebar opens Profile, Updates, GitHub, Projects folder and Restart engine, each as a dialog. Arrow keys move through it and Esc closes it.
- Context is a tab beside the IDE again, and its tree reaches the project's nested docs and memory files.
- Team size can be set to No limit.
- Settings for leftover processes and what the master may accept.
- Landing setting: Squash or Merge. After a landing, agents move onto the landed tip.
- A wake tool gives a finished sub-agent its next task, and the master's status lists retired sub-agents.
- Heavy lane: builds, tests and other heavy jobs run in one lane per project, queued first come first served across the engine, with named shared resources. Rows in Agent settings, the TUI and the Team view.
- Shared project folders: projects can share local folders with agents, read-only by default.
- Narrow access grants: agents can be granted sockets and ports, and can ask for more with a request you answer. A setting decides when access changes apply, and Agent access shows the grant log.
- Toolchains are recorded when a project is made, with wider markers and the Xcode SDK.

### Changed

- The master works only through tools or strict task blocks; its prose is no longer parsed.
- One tool resolver finds tmux, Node, harnesses and pack tools the same way for setup, the engine, launches and the sandbox, including Linuxbrew and NixOS profiles. A file it passes over is reported as refused, with why.
- Engine updates carry the bundled tmux, and a restart waits until the old engine has stopped.
- Start and Try again clear the project's automatic start count, and switching a live master on looks for tools again.
- The wait for the live master ends when its terminal opens, and a master stopped before its terminal opened says why. Keys typed into the TUI master terminal stay terminal keys.
- Get ready, a pool save, Add account, a sign-in cancel and a sign-out run one at a time, so none changes the account another is setting up.

### Fixed

- Overview, roadmap and research checks with no account wait for one under a plain status line, instead of failing for good or showing an alert.
- A local model provider that refuses connections fails at once with one plain line naming the way out, instead of three four-minute tries.
- A Claude account check on a cold start gets its full time bound, so a signed-in account is no longer read as signed out.
- Keyed RCLI accounts fail over on a rate limit, a spent quota or a rejected key. Keyed accounts saved before this fix are repaired at master start; when a repair fails, the turn names Get ready.
- A harness the resolver refuses to start ends as a plain launch or sign-in failure and frees the account.
- A sign-in started while the engine's first tool lookup is still running waits for it instead of failing.
- TUI at 80x24: settings hint bars wrap instead of dropping keys, Team settings scroll with PgUp and PgDn, the plan line stays in view, and the Process page keeps its problem line and editor in view after a refused save.
- The TUI Context tree scrolls to keep the cursor in view and names the docs the engine left out.
- The team drawer with no open question points to the Questions page, as the TUI does.
- Full access and unkept terminals follow their agent's work again, each on a thread of its own.
- If learning progress cannot be saved, both apps say "Your learning progress was not saved." with the plain reason.

### Security

- On macOS each harness agent's private home gets its own keychain, so agent sign-ins never touch or prompt for your login keychain.
- Shared local folders are read-only by default, and secret files are left out of them.
- Access grants are explicit: sockets and ports are granted per agent, a request for more access waits for you, and a grant that could let agents act as you shows a warning. Terminal and editor control sockets, Docker and debugger ports are refused.
- Free resets are used only as the spend settings allow, and the engine refuses one you did not confirm while Confirm each is on. A retried Codex free reset never spends twice.
- The demo runs Sandboxed, inside the projects folder only, and never pushes.
- Sandbox profiles keep each rule under the system's size limits, and a profile past the 64 KiB cap is refused at install instead of crashing the engine.
- A master's sandbox also denies the engine's own secrets (its database, GitHub sign-in, account keys and sockets).
- A data store that does not switch to write-ahead logging is refused, not used silently.
- The engine service is replaced only by an installed app's engine, never by a build output or by an older engine over a newer one.
- The engine skips PATH folders inside a temp folder, and refuses a harness found in any project folder, agent worktree or agent home. A folder an untrusted group can write counts as changeable.
- Every commit a Merge landing makes reachable is checked: its tree, identity and whole object.
- Report Chrome runs in its own process group and is ended as a whole group, so no helper outlives a print.

## [2.3.2] - 2026-11-12 16:18 UTC - Released

- Test install 2026-11-12 11:50 UTC, commit dfd32ccb7.
- PR [#2](https://github.com/ChaoticQubit/CrazyAgent/pull/2) merged 2026-11-12 15:42 UTC, commit 13cb98a9e.
- Stable tag v2.3.2 published 2026-11-12 16:18 UTC, commit 13cb98a9e.
- Dev build 2.3.2-dev.202611121542 published 2026-11-12 16:20 UTC, commit 13cb98a9e, as the rolling dev release (since replaced).
- Version set to 2.3.2 on 2026-11-12 15:24 UTC (c93cb04e2).
- Carries the [2.3.0] and [2.3.1] lines below, which were never released on their own.

### Added

- Master limits: sub-agent rounds per message and a master run time setting. Your answers and team actions renew the run time, and a run time stop offers Start again.
- Master frame size limit setting for Claude Code and Codex masters, with a plain stop line that says what to shorten.
- End a turn after done, in Agent settings and the master's settings: a sub-agent's change set is posted when it reports done or commits.
- Master accepts change sets: Itself, or only when you click or say accept all.
- The master can discard a finished harness's change set.
- One question list: team questions join the questions log. Masters can withdraw a question, relay an answer from a terminal and ask a text question again as choices.
- Dismiss all waiting questions at once, and see who closed each one.
- Why each queued task waits is stored and shown.
- Move an app-wide agent memory into one project.

### Changed

- The default team names no harness and no size cap, whether research is on or off.
- Agent memories stay in their project by default, and memories of projects that are gone are forgotten.
- Cancel reasons are recorded per path, and your team actions are audited.
- A change set shows on the done report and the agent's commit, and is named by the agent's summary.
- You are told once per change set, not at every turn that grows it, and master notices are batched into one turn.
- File overlap claims work at file level, not by shared folder.
- Newer Codex versions run with one notice; older and known-bad ones are still refused.
- The planning agent writes its open questions in plain words.
- TUI: Master run time moves to key N, so key D reaches End a turn after done.

### Fixed

- A short team view in the TUI drops the focused agent's Tools and Working copy rows before the plan line, so every setting shows at 80x24.
- A linked question the team inbox already answered is not told to anyone again, and counts once on the project flag and app badge.
- An answer to a team question whose message already ended still reaches the asker.
- An urgent team question's toast opens the Questions tab where it waits.
- A stopped engine's refused connection reads as one plain line in every client.
- Stale sockets and locators are removed once a start proves them dead.
- Accept works past your own edits outside the change set, committing the ones in the way first, and never commits untracked files.
- A worker's merge refusal names Update on its change set as the way its lead's work arrives.
- An orphan worker's change set lands on the Land on branch.
- Session search keeps harness-injected instructions and summaries out of your words.
- A refused pack hook shows as a warning, and every shipped hook folder's plugin name is pinned.
- Every master failure cause gets a plain reason and action.

### Security

- An open backups folder is made private again, and a failed backup is recorded for the apps.
- A discard reason with hidden characters is refused.
- A question answer never grants accept all.

## [2.3.1] - 2026-11-12 08:28 UTC - Internal build

- Test build of commit cea59a451 started 2026-11-11 20:55 UTC.
- Test install 2026-11-12 08:28 UTC, commit 40bfb82a2.
- Work from 2026-11-11 to 2026-11-12, on top of the combined 2.2.0. Never released on its own. Shipped to users in [2.3.2].

### Added

- Processes an agent left running after a stop are listed for you, with End and Keep. A per-project setting picks End them (default), Keep running and release the account, or Ask me.
- A live master whose terminal ends on its own starts again in its own conversation, instead of turning Live terminal off.
- The Harnesses page lists where CrazyAgent looked for a missing Codex or Claude Code, lets you choose the file by path, and Check again picks up a late install.
- Memory a master wants to save is approved on the project's own Memory screen (Context, Project memory), not only in Settings, in the app and the TUI.

### Changed

- Same features on every harness: sign-in, edit protections, step limits, background commands, and key, URL and local accounts act the same on Claude Code, Codex and RCLI.
- Key, URL and local accounts run on the harness you pick.
- The RCLI master gets the same instructions, failover and stop reasons as the other masters.
- Native sub-agent mode on a Codex lead uses Codex's own sub-agents, on the lead's model, and the app says plainly what it shows of them.
- A turn that ends at an engine bound is named as such, not as ended by itself.

### Fixed

- "Codex is not installed" showed while Codex was installed. The engine now finds harnesses the way your terminal does, and the app and TUI give one plain sentence when one is missing.
- The live master's terminal ended on long output. No output or event count ends an agent session, the live master's 24 hour launch deadline is gone, and an over-long record line is skipped with one note.
- A master step that writes takes the write lock before it reads.
- Older notes in Project memory restart when a newer note moves the first page.
- Release publishing tolerates a removed dev tag and retries a busy disk image.

### Security

- On macOS each Claude agent home gets its own keychain, so macOS no longer prompts for a missing keychain and agents never touch your login keychain.
- A harness file found or chosen inside any project folder, agent worktree or agent home is refused, and a group-writable or empty harness file never passes.
- A Node script harness reads only its own Node folder and package, never your npmrc.
- On Linux an agent on a limited network cannot listen at all, and launches stay on ports Linux picks.
- Codex hook trust is pinned per hook, and the Codex edit guard refuses unplaced or oversize patches.
- Leftover process checks read process identity from fixed fields and check again before each signal.
- A plugin server check the sandbox refuses shows the sandbox's reason.

## [2.3.0] - 2026-11-11 17:25 UTC - Internal build

- Version set to 2.3.0 on 2026-11-11 14:39 UTC (98008f483).
- Test build of commit 6a3ca5084 started 2026-11-11 17:25 UTC and was installed later that day (install time not recorded). It was replaced by the combined 2.2.0 build at 19:52 UTC.
- Work from 2026-11-11. Never released on its own. Shipped to users in [2.3.2].

### Added

- Project Overview with tabs: each project opens on an Overview with the README and generated tabs, kept per project. Needs you sits above the tab strip, and activity moved to its own Activity tab.
- Overview tabs are written after the first read, after each landed change set and on Refresh. You can edit a tab, and accept or reject a refresh as a diff.
- Competitors tab with Run research, and commands with Copy and Run.
- Overview settings in Project settings.
- TUI Overview tab row with the picked tab's text; Shift+R asks for a new writing run.
- Harness updates: Check for updates and Update for each installed harness, and Install for every harness in the catalog, in the app and on the TUI Harnesses page.
- The engine picks up a harness that it installed or updated after it started, with no restart.
- Run in project terminal: run one confirmed command in the project's own terminal from the app or the IDE. The confirm names the project's access level.

### Fixed

- A running agent renews its sign-in through the harness entry, not the file it started with, so a harness update no longer breaks it.

### Security

- Overview Accept writes only the proposal you reviewed, and a replaced proposal is shown before it can be accepted.
- Overview failure lines mask anything that looks like a credential, and prompt data cannot close its own fence.
- An Overview tab file replaced by a link or a folder is blocked and named, and only one writer runs per tab at a time.
- Run in project terminal accepts a single command of plain words only, checked in the engine, the app and the IDE.
- A harness tool folder that the macOS staff group can write is refused, a failed update pins nothing, and update output is masked.

## [2.2.0] - 2026-11-12 01:21 UTC - Released

- Test install of commit fc9600c57 2026-11-11 19:52 UTC.
- PR [#1](https://github.com/ChaoticQubit/CrazyAgent/pull/1) merged 2026-11-11 23:30 UTC, commit bb7691145.
- Dev build 2.2.0-dev.202611112331 published 2026-11-12 00:20 UTC, commit bb7691145, as the rolling dev release (since replaced).
- Stable tag v2.2.0 published 2026-11-12 01:21 UTC, commit bb7691145.
- Version set to 2.2.0 on 2026-11-11 04:31 UTC (b8d654849).
- First stable release. It carries the [V2], [V2.1] and [2.2.1] lines below, plus the V2.2 work listed here.
- Platforms: macOS (Apple silicon and Intel) and Linux x86_64. No Windows build in this release.
- Known limit on Linux: a master with tools edits existing files but cannot create a new file or folder at the top of its worktree.

### Added

- The IDE can move into a maximized window of its own. It keeps its server until that window, the project or the app ends, and the app menu follows the focused window.
- The IDE runs at the project's access level (Sandboxed, Own branch or Full access) and restarts when that level or agent access changes. Each Save says it restarts the IDE.
- The IDE terminal menu offers each access level, with levels above the project's greyed out.
- IDE agent folders setting: Allow all changes (default), No delete or rename, or Read only, at Sandboxed and Own branch.
- Workspace trust is on only at Full access, and Full access says what it means for the IDE.
- Pools and account choice are one screen, in the ranking's order. Pin, schedule and order apply on the row; add, remove, New pool, rename and delete happen on the Pools screen.
- Finished agents setting: the worktrees of finished agents are removed on the schedule you pick (1 day by default), and their logs stay. Clean up removes only worktrees and lists the agents a check keeps.
- Every agent worktree gets its own copy of the project's published graph.

### Changed

- Release builds are made for macOS (Apple silicon and Intel) and Linux x86_64: CLI archives, a DMG and app archive for macOS, an AppImage and a deb for Linux, each signed, with updater manifests and SHA-256 sums.
- One Terminal display for every harness, so a Codex terminal can be copied from.
- TUI Settings questions are drawn whole at any window size.

### Fixed

- Your answer now reaches an idle live master without a false "you are typing" hold.
- A project move by copy now clears its old source against the copy it landed as, and compares bytes before deleting a source file changed moments earlier.
- On Linux, usage counting reused old progress for a session record recreated under the same file id.
- On Linux, a cloned repository's credential helper lost the desktop session bus.
- A stop no longer waits on a stuck process listing.

### Security

- A Sandboxed IDE terminal has no internet and cannot stage or commit. A rebase from Sandboxed or Own branch stops with a plain message, while Source Control push, pull and merge still work.
- Revoking a project's IDE closes its terminals at once.
- The IDE writes nothing under its agents' git admin folders, and a gc from the IDE keeps every reflog.
- The IDE keeps your local model ports free, and a settings file that does not parse stops its start.
- On Linux the agent git wrapper is now written, so the merge guard holds there too.
- Windows builds are held back: the Windows sandbox did not hold its deny rules, so an agent could have written protected git folders.

## [2.2.1] - 2026-11-11 - Internal build

- Never built or released on its own. The whole line merged into the 2.2.0 release on 2026-11-11 19:09 UTC (184c15299) and shipped to users in [2.2.0]. Work from 2026-11-11.

### Added

- Questions can be single choice, multiple choice or free text, each with an optional comment, in the app and the TUI.
- Waiting questions show as a pop-up above the project tab you are on, answerable in place and usable from the keyboard.
- Roadmap status: after each landed change set (once the project has been quiet for 10 minutes) and on Refresh, a status run marks roadmap items done. The app and the TUI show it with a Refresh action and a setting.
- Setup finds a roadmap and research the project already has, by their headings, including in hidden folders and ignored Markdown, and links them instead of researching again.
- The mapping step names the roadmap and raises open setup points as questions.
- The Context tab lists and reads linked repository files, and Open in default app opens a linked file.
- Edit with AI for instruction files: it proposes a cleaned file for you to review and accept, in the app and project editors.
- The joined instructions leave out project lines the app text already has, with a setting to keep them.
- Move into CrazyAgent folder: move a project kept in place into the projects folder.
- Live master account setting in Settings, Pools: an idle live master moves to its pool's top account, at most once per 15 minutes and never over a line you have not sent.
- Master time limit and master step limit settings, with a plain reason when a turn runs out of time or output.
- A running master turn shows what it last did, in the app and the TUI.
- Every master conversation is kept as one stored conversation, whatever the harness or master mode. Switching mode or harness carries it, and the Sessions list shows each session's message count and opens its messages.
- Restart engine from the menu on the bottom-left profile block, with a confirm, always reachable.
- The app Metrics page can hide account-wide panels while one project is picked.
- Adding a folder asks to download iCloud-only files before it moves or copies them.
- `/usage` in the master console opens the project's Metrics tab.

### Changed

- Accounts go by name everywhere. An id is added only when two accounts share a name, and no screen shows a sign-in reference.
- New Project preselects and offers only the access levels this computer can run.
- Agents ask with the kind of question that fits: single, multiple or text.

### Fixed

- A master turn no longer ends on a hidden count (24 tool steps, messages, events, threads) or a hidden wall clock on Claude Code, Codex or RCLI. Large tool output no longer ends it either.
- A master that worked for minutes showed no sign of life.
- A dropped live master turn left its working status on screen. It now clears on any screen, counts once in Sessions, and a turn is taken over as idle only when the idle prompt shows.
- A master switch lost the conversation, and words read back from the harness record could show as words you typed.
- A worktree lease check failed when Git removed a `.git` entry between listing and reading it; a quick read failure is now tried once more.
- After a harness update the engine kept launching the old path until restarted. It now launches the harness its PATH entry leads to and re-pins it after an update.
- The graph build failed on a graphify whose Python sits behind uv's version link.
- Repositories with a large `.git` folder (up to 65,536 entries), a config over 64 KiB or many branches were refused or got no agent clones.
- Landing actions now answer at once while a project move holds the repository.
- Adding a folder deletes only source entries it verified, and refuses iCloud placeholders.

### Security

- On Linux an agent with open network cannot listen on your login callback, local model or proxy ports (Landlock network rules), and no kept port lies where the kernel picks ports.
- The engine never waits on a FIFO planted where it reads a file an agent can write.
- Secret files stay unreadable to the graph build, which no longer fails because of them.
- Linked repository files are read as text through no link, never by path.
- Roadmap status runs treat stored rows as data, and app-wide Edit with AI Off keeps the app text out of a project's proposal prompt.

## [V2.1] - 2026-11-11 03:37 UTC - Internal build

- First V2.1 test install 2026-11-09 18:01 UTC, commit b8dfe7f79.
- Last V2.1 test install 2026-11-11 03:37 UTC, commit 022a19dcd (last merge 2026-11-11 03:26 UTC). It also held the V2.2 work listed under [2.2.0].
- Work from 2026-11-09 to 2026-11-11. Never released on its own. Shipped to users in [2.2.0].

### Added

- New sidebar: projects on top, the app below, a live dot and agent count beside each project, and the alert badge read aloud as alerts.
- Settings lists its sections in groups beside the open one. Every section uses the same row layout, with a changed mark where a value differs from the default.
- App Metrics page leads with Tokens beside an over-time chart with a readout and its own table. Settings, Usage is gone and its link opens Metrics.
- Metrics panels: By project, By agent, By pool, By model, By activity, Lines and By day (newest seven days).
- Account limits and Check limits now are one panel on Metrics. Limits and a refused check stay when other reads fail.
- Main number and price source are settings. Dollars come from a bundled, dated price snapshot; with prices Off no dollar figure shows, and every dollar figure carries its label and credit.
- First-try rate, activity types and tool calls per hour show on the app Metrics page and the project Metrics tab.
- Counting history again is staged: you choose Use new or Keep old once, with a 30 day copy and a notice while old counting shows.
- Live terminal switch per project, On by default. Off shows the conversation view and ends a live master at its next stop, never counted as a crash.
- Stop the master when idle after: Off, 15 min or 1 h per project. A master stopped for being idle gets one Start again button on the view you have open.
- Reuse idle agents per project, Off by default. At a full limit an idle agent may be stopped to make room.
- Terminal input is one setting for every terminal, Terminal only by default.
- Tool cache per project: Shared by agents with the same access (default) or One per agent. Sandboxed agents' uv and pnpm installs reuse a cache CrazyAgent prepares.
- Quitting setting: when CrazyAgent quits, Keep team agents running (default) or Stop team agents. Agents left running end after 1 to 168 hours (default 24) or never.
- The account ranking says why each account stands where it does.
- A screen that crashes while drawing shows a plain message and a Reload button instead of a blank window.
- Pause all is one action from the Agents button and the project menu.
- TUI: key I sets Stop the master when idle after, key V turns Live terminal on and off, and key q on App setup sets Quitting.

### Changed

- The Conversation tab is a fixed screen: the master fills the middle and the Team bar scrolls on its own.
- A pick you just saved stays on screen until a read made after the save. Another window putting the old value back then shows. All 40 controls work this way.
- A harness row waits for the read after its save, No pool asks first, and Esc closes only the question.
- Changes: an Accept closes only the change sets whose work reached the landed files, and the master's own land must hold nothing but conflict fixes.
- Changes: your Accept row says where you pressed Accept, and an Undone row names its change set the same way.
- Changes: you are told when a lockfile was rebuilt with the project's own tools, or when a lockfile clash went back to its agent. A uv.lock clash is rebuilt on a fresh cache made for that one run.
- A worker's open change set is never closed by a lead's or master's merge; only your own press discards it.
- Catch up with main Off is described as a guide for the git the app writes, not a lock.
- The master and its session brief list open change sets the way you see them, one line each.
- The master's search of past conversations gives each agent a fair share, so one busy agent no longer hides the others.
- Claude messages and Codex forks, resumes and spawned runs count once in Metrics, and a very long master turn is counted to its end (up to 16 reads) or marked partial.
- By agent lists an agent that worked but used no tokens, and an agent used in two projects shows one row per project.

### Fixed

- Delete everything keeps its dialog still with many folders, counts them, and shows the whole project name.
- A project being deleted refuses to open its IDE, and the delete removes only the engine files it recorded.
- Open all terminals counts its windows from a fresh read and opens nothing on a removed project. Wall windows close when their project is gone.
- The Overview's activity refreshes as soon as the window shows again.
- A narrow team tree keeps each agent's status word.
- The questions drawer never takes the keyboard off the field or button you are on.
- The plan line stays when there is no master yet, and "No master yet" no longer reads as "no live terminal".
- Opening the master on an engine of another version says restart, not pick a pool. An older app or TUI against a newer engine says restart the engine.
- A sender is told when its message waits for a stopped agent, and a woken agent whose message was already delivered is told so.
- A healthy sign-in recheck leaves the notice you were reading, and a busy engine keeps a running sign-in with "CrazyAgent is busy with other requests. Try again in a moment."
- Editing global.toml outside the app and restarting the engine keeps the master conversation, as long as the edit leaves the master's own settings alone.
- A failed Metrics read still shows the usage-only panels.
- Clean up removes a worktree even when its agent locked files inside it, and says which file it had to leave.
- Tool caches go with their project on Delete everything and Remove from dashboard, and an agent's own cache goes with its worktree on Clean up.
- A reset puts each file back from the agent's latest commit, so a commit the agent made during the reset is never undone.
- A Discard the store refuses leaves the set open, with no false Discarded row in the activity.
- A lead cannot Complete while a teammate's edits are not saved on that teammate's own branch. A teammate that stopped mid turn gets its edits saved and landed on the lead's branch, with a note that they may be partial.
- When a lead stops an agent the master already stopped, a task still queued for it fails and names the lead.
- Codex's folder access screen is answered with one Enter, so Codex agents start straight away.
- Codex skills an earlier run left are moved away before a start, or the start is refused with a plain sentence.
- A letter counts as read only when the session really got it as input.
- A rate limit an agent's transcript reports holds the account for that project only.
- The engine's own update waits while a team is paused, and one repository runs at most two lockfile rebuilds at once.

### Security

- Sandboxed agents and terminals cannot write git state that runs a program when you continue a cherry-pick or rebase, cannot plant worktree links, and cannot read other processes' arguments.
- A repository's own `.codex` settings apply to no launch.
- The IDE's git refuses worktree prune and remove, network commands after leading options, an index entry with no stored object, and a project whose `.git` points elsewhere through commondir.
- A teammate may write only its own lead's teammate branches, and a teammates server with a missing or wrong pin record is refused.
- Recall replies, control frames and kept message ids are bounded.
- Removing an account deletes its sign-in or generated home through the file trash, unless a connection still names it.
- A model endpoint that resolves to this Mac's own address is refused like loopback; localhost still works.
- Full access runs are checked every second, so a process left running is named at the stop and the account stays held until it ends.
- Each agent's model connection uses a port from 41300 to 41427, which agents on the Open network cannot listen on, and never takes your local model ports.
- With Quitting on Stop, quitting leaves no terminal server behind; with Keep, servers left running end once the time limit passes.
- A native team lead's own terminal server runs under a keeper, so stopping the lead ends everything it started, and the account is freed only once those processes are gone.

## [V2] - 2026-11-09 06:20 UTC - Internal build

- Test install 2026-11-09 06:20 UTC, commit f79ff9b02 (last merge 2026-11-09 05:08 UTC). Built on V1 build 19. Work from 2026-10-27 to 2026-11-09.
- Never released on its own. Shipped to users in [2.2.0].

### Added

- IDE tab in every project: an embedded VSCodium editor with CrazyAgent dark and light themes. It replaces the Context and Changes tabs.
- IDE Explorer has one section for the project folder, one for the master and one per agent. The newest 12 agents show; "Show all agents" lists the rest.
- IDE Source Control shows each agent's change set as a group with diffs. Accept, Discard and Restore go through the engine.
- A comment on a diff in the IDE reaches the agent that made that change set as a message.
- CrazyAgent sidebar inside the IDE for memories, agents and change sets.
- Push, pull and fetch from the IDE use your own git credentials and run only from the project folder. Agent worktrees cannot push.
- IDE terminal choice: Sandboxed (default, your shell under the agent sandbox with no agent credentials) or IDE shell (inside the IDE's own sandbox, no network).
- A running agent's files open read-only. Edit anyway adds your edit to that agent's change set, marked as yours, and the agent is told.
- IDE extensions install only from Open VSX, one at a time, after you approve each in a CrazyAgent dialog. Installs are pinned by hash and never auto-update.
- The IDE build (pinned version, SHA-256 checked) downloads in the background on first use, and the IDE tab shows its download state.
- TUI: "open in IDE" brings the desktop IDE tab forward, and "open in $EDITOR" opens the selected file in your own editor.
- Questions: agents, masters and engine steps (mapping, research, landing) ask you through one ask tool, one question at a time, with a recommended answer.
- Questions tab in the app and a Questions page in the TUI log every question. Each project also gets a read-only `.crazyagent/questions.md`.
- Pending questions show as a badge in the app, in the TUI footer, and as a macOS notification while the app is in the background.
- Each answer reaches the asker exactly once. Questions a live master asks in its own terminal are logged too.
- Roadmap planning: after a new project's research, a planning job writes `roadmap.md` and `release-process.md` in `.crazyagent/context/`. Open points arrive as questions.
- "Plan a roadmap after research" setting (On by default) for New Project, import and clone, and a "Plan roadmap now" action in project settings, Overview and the TUI. It asks before it replaces a plan you edited.
- Crash recovery: after a crash or restart, each interrupted agent resumes in its own harness conversation with an "interrupted at" note. Paused teams stay paused.
- Recovery banner on the project page and in the TUI lists what was interrupted, what resumed and what failed, with Retry.
- Engine actions (worktrees, agent starts, message deliveries) are recorded before they run, so none is ever repeated. An action whose outcome is unknown waits for you to check and dismiss it.
- Versioned shared state: project setup, read paths, team rules and settings, roles, skills, MCP, pack toggles, schedules, questions, memories, agent instruction settings and agent access carry a version. A stale write is refused and must be redone ("This question changed. Review it again."), and your edits win over agents'.
- Usage counts tokens from Claude Code and Codex session records and from RCLI masters. A harness with no usage reader shows "Usage not reported by <harness>", never zero.
- Project Metrics tab with tiles, a tokens chart and a breakdown, and an app Metrics page across all projects with a project filter.
- Metrics count merges, merges into main and undone merges, as a tile and as a By project column, and show each agent's turns in the range.
- Project activity: the engine records every project event it sees (merges, accepts, rejects, reviews, tests, tasks started and finished), and agents log their work phases and test runs.
- Overview shows that activity By goal or as a Timeline, with filter chips and goals that close.
- Undo a merge into main from Activity. The confirm names the branch, and the undo adds one commit that reverts the merge.
- Catch up with main setting (On by default): agents may merge the project's main into their own branch.
- Before a change set reaches you, the engine replays it onto current main, and the owning agent fixes real clashes first.
- Lockfile-only clashes with main are regenerated with the project's own tool in a confined temporary worktree.
- After its change set lands, an agent in its own clone is told to restart from main, and the landed note says to use `git reset --keep`, which keeps uncommitted edits.
- The turn-end catch-up with main never undoes your Stop, and leaves to you what it cannot merge.
- Overlapping edits setting: file claims hold until the change set lands, and a second agent can be made to wait until the file lands.
- Under Full access, a merge setting: the master resolves conflicts and merges into main itself (default), or resolves them and waits for your Accept.
- Change sets are numbered per project ("Change set #16 (Wren: checkout UI, 7 files)") in the app, the TUI and the master's merge rows.
- Agents page toolbar on one row: working line, the pool's account count, Running and All counts, Pause all and Open all terminals. Team parts fold under the graph, and questions open in a drawer.
- Team graph lines show each relationship (leads, delegated to, teammate of) and the live action (handing off, messaging, waiting on, reviewing).
- Compact four-line graph cards, and each card shows how long its running task has run.
- One terminal panel on the Agents page follows the agent you click.
- Open all terminals opens terminal wall windows of 4, 6 or 9 tiles, set by Terminals per window, each tile with Message and Stop.
- Project menu: Stop all beside Pause all, and Start all again while the team is stopped.
- Project head names the folder, branch, access and pool, with working and question chips, and keeps a compact form while you scroll.
- Agent settings regrouped with a group list, Find a setting and a Master settings drawer. Harnesses, agent access and the master's account are picked in place.
- Process page grouped into instructions, who follows them and delivery, and shows the exact text agents get.
- Delete everything moves a project to the Trash after stopping its agents, only inside the projects folder. Remove from dashboard keeps the files. Usage stays under Removed projects.

### Changed

- Headline token counts are new input plus cache writes plus output. Cache re-reads show on their own line, and budgets and limits use the same headline count.
- A Fullscreen Claude Code terminal always starts Claude Code's fullscreen renderer, whatever the account's own choice.
- A held agent shows as working, only typing reads its held turn, and held turns end after a 30 minute grace.
- The merge words, the land note and the Process page say exactly what lands by itself: only the master's conflict fixes.
- Metrics opens on the last 7 days.

### Fixed

- A master at Full access could not merge into main because the agent git wrapper refused it.
- Token totals were inflated because cache re-reads counted one to one.
- Two agents editing the same file ended in merge conflicts at landing.
- Overview Recent activity showed only research and setup events.
- An unreadable agent policy no longer fails the whole team status.
- A native lead on a signed-in account keeps its terminal row, so its calls are named and its teammates register.
- An engine reset moves an agent worktree only where the agent changed nothing, and starts only from the commit its reset began at.
- A growing session record is read only to the length it had when opened, and a usage step that fails shows a health line and backs off.
- The TUI keeps running when Ctrl-C reaches a waiting editor.
- Home in the TUI keeps your focus when the setup check lands late, and the Agent access box fits the window with Save on screen.
- A settings save the engine cannot confirm reads the settings again, and a save refused before anything was written says nothing was saved.
- Change set numbers stay with their sets when a project is removed.
- A sign-in shows as stuck only when its database or its own account really moved, and a recheck marks it unconfirmed only on the engine's conflict.

### Security

- The IDE server runs under a macOS Seatbelt sandbox, on loopback with a per-launch token. It cannot read your keys, the engine database or an agent's `.vscode` settings.
- The engine keeps AI locked off in the IDE: no chat, no Copilot, no Accounts or Settings Sync, no Remote, no telemetry.
- IDE git never walks into agent worktrees or repositories planted below one, never prunes or garbage-collects agent worktrees, and stops at the agents folder.
- An IDE pull only fetches in the engine; IDE git calls are capped, and their PATH holds only your own folders.
- The IDE page cannot drive or wedge the app: native commands are gated by page, and terminal wall windows reach only their own commands.
- IDE servers left by a crashed engine end when the engine starts.
- Answers typed in chat count only from turns of yours the engine recorded, and question notifications are bounded and escaped.
- Planning treats research text as data, and your answers sit in an engine section agent text cannot fake.
- Agent worktree remotes refuse each built-in git transport by name.
- Accept after a change set moved waits until the diff's last line has been drawn on screen, in the app and the TUI, so an old view never approves new code.
- IDE config and git file names are compared the way the macOS file system folds case, so a differently cased name cannot slip past a lock.
- Landing notes cut local paths anywhere in a word, file links and Windows drive paths.
- Activity rows are scrubbed of local paths, and agent-logged rows are capped at 30 an hour per caller.

## [V1 build 19] - 2026-11-07 08:36 UTC - Internal build

Installed 2026-11-07 08:36 UTC. Commit `c1bfeaf6`, changes since build 18.15.2 (`1d7ea29c..c1bfeaf6`). The last V1 build. An early preview of this line (`593b3056`, build 19 work on top of 18.2) was installed on 2026-10-28 at 20:03 UTC and rolled back to 18.2 the same day, because the live master could no longer start.

### Added

- Messages timeline for each agent, on the Agents tab, the open graph card and the TUI team views. It is built from tasks, events and the master's and your kept words; engine notices typed into a harness show as the engine's.
- Messages settings in project Settings (desktop and TUI); every choice is saved and read back.
- Linux limited-network sockets setting, on the desktop Settings Sandbox tab and the TUI Setup page, with a check of the system loader.
- Adding a folder offers Move anyway and Copy anyway, per add.
- Every agent launch carries firm tool rules, and a master's turn from you carries the Task-start reminder.

### Changed

- Narrowing a project's saved agent access, or turning a toolchain off, stops the agents, research jobs and master turns that ran with it. Their work returns to the queue, each stop says what happened in its own sentence, and held tasks move to an allowed agent.
- A task goes to the next idle agent when the first one is past its session.
- Removing an account forgets its limits; the usage report lists only accounts that exist.
- Resume continues when another project joins the session's pool.

### Fixed

- Full access stops name what they left running, hold until it can be checked, and you can release a hold by signing out; the process listing is bounded in time and size.
- Stopping a pane agent ends every process it started, including its own sessions; leftover panes are cleaned up and an unconfirmed cleanup is reported.
- A sandboxed agent's finished card opens on how it ended, because its last screen is kept before the pane closes.
- A native lead starts in its own tmux server with its kept command.
- Codex agents: read receipts come from the agent's own records, and a message in an aborted turn is put back correctly.
- A projects home move is refused while agent work or a research job sits inside it, and launches wait for the whole move.
- Pools: a new pool sent twice is saved once, and a pool's projects are found by its id, not its display name.
- Project stack entries that hold commas stay whole.
- Usage readings that skipped a changed record are marked partial.
- Desktop: terminal views are freed when the window reloads, the terminal menu closes on reconnect, the agent terminal head shows what its harness reported, and a saved team setting stays shown until the engine confirms it.
- TUI: a request refused as busy is said so and asked again; the Finished box scrolls to the picked agent; waiting change sets are counted per project; s opens team settings; tabs show as four spaces; long lines of marked instruction files wrap.
- The agent Instructions view says why Claude loads no file.
- Context files open from the folder the engine lists them in.
- Sign-in: never ended by a settings conflict while the engine still runs it; only a stuck sign-in is cleared; a pasted Claude code reaches its own sign-in.
- Research: a stale job fails at its attempt limit instead of requeueing forever, and a run stays claimed while it waits for repository setup.
- A reused project id clears the old project's skill folder, so skills no longer leak between projects.
- Agents run the Command Line Tools directly, so xcrun never writes its cache, and each agent gets its own getconf so the context tools run.
- The Windows build compiles again; native lead and graph steps refuse plainly on Windows.

### Security

- Agent credential copies: a Claude copy is refused when its scopes go beyond inference and profile, or include organization scopes; a Codex copy carries no usable id_token signature.
- A harness key file is read through one bounded no-follow open, and a busy home is refused before the key file is written.
- Change diffs show hidden characters as escapes and mark instruction file changes in any letter case, including Codex overrides, harness folders and files an instruction file imports or links to.
- Accept refuses links through links, links into .git, names with trailing dots or non-ASCII characters, and names the volume folds together.
- Every writable sandbox root is pinned against move, replace and permission change; no agent environment may carry variables that the helper keeping its terminal open would obey.
- A Linux limited-network agent cannot open datagram or other non-TCP sockets; a Linux harness script is judged by its interpreter.
- An MCP secret shorter than 8 bytes is refused, and a refused launch names the server and secret it would have exposed.
- Messages text and results are drawn in the agent's named frame as plain text, so they cannot pass as another row.
- Changes reads every diff page at the commit you review, and a moved diff is read again before Accept or Discard.
- Project ids that Windows would rewrite to another folder name are refused; on Windows the sandbox anchors the folders above a hidden path.

## [V1 build 18.15.2] - 2026-11-06 07:38 UTC - Internal build

Installed 2026-11-06 07:38 UTC. Commit `1d7ea29c` (`579eebcb..1d7ea29c`).

### Added

- Task-start reminder setting, On by default: each sub-agent's first task turn and each native teammate's first prompt start with a reminder of the tools to use.
- Team settings can switch the master harness (desktop Apply, and the TUI says when the change applies); the master head names the running harness, and a saved switch reaches the live master once it is idle.
- TUI: a Shift-arrows setting, and Shift-Enter in plain terminals through the kitty keyboard protocol; the Conversation tab of a live master opens its terminal.

### Changed

- A kept conversation resumes on whichever pool account the schedule picks, but only between pool subscriptions.
- Your notices to agents are held on their own and steered into a working Codex turn; an awake lead is only typed into with a waking message.
- The ChatGPT device sign-in waits out its full 15 minutes and says when CrazyAgent stops waiting.

### Fixed

- A sign-in panel failure now offers Clear stuck sign-in; a sign-in refused because another one still runs names that account; a sign-in the engine ended while no panel read it blocks nothing.
- A live master waiting for its account's profile says why; a line you send to a live master waits until its app reads keys; the app says once when it reads no keys.
- An action you take on a worker never wakes its lead.
- Under Native only, a tools-off Codex sub-agent that cannot receive messages says so on its row and graph card.
- Right-click keeps a drag selection, so the terminal menu's Copy copies it.
- The Sessions bar re-reads its count on each master turn change; a working Sessions read clears an earlier refused one.
- TUI: a second n while a New session starts says it is already starting; the project page re-reads its setup when shown.
- The uncommitted instruction import banner names the checked-out branch and escapes hidden characters.
- Usage: a window the provider leaves out is cleared and worded per provider.
- A stop kills what its panes leave in their process groups; a gone group is never signalled again.

### Security

- Revoking a client's access takes effect even when storage answers late or fails; a revoked client can no longer keep its access until the next restart.
- The device sign-in code is masked wherever it appears in logs.

## [V1 build 18.15.1] - 2026-11-05 17:32 UTC - Internal build

Installed 2026-11-05 17:32 UTC. Commit `579eebcb` (`4368da8f..579eebcb`).

### Added

- Claude Code terminal display setting, Classic by default.
- Agent worktrees show the project's code graph, and agents are told when to use the agent tools.
- Console master turns with tools keep memory through remember and recall.
- Your notes: a note waiting for approval reads "Needs approval", and each note names the agent that wrote it.

### Changed

- Adding a folder: Move is offered only when the folder can move without breaking linked git worktrees, and Copy is not offered for a folder that is itself a linked worktree.
- Sessions: turns run in the live terminal count as the session's messages, and a terminal-only session shows its newest reply in the Sessions list.

### Fixed

- Every sign-in failure has its own code and a true, plain sentence in the desktop app and the TUI, including a Claude or Codex sign-in that fails, is cancelled or ends before it starts. A stuck sign-in names the engine restart that clears it.
- TUI: the master view is the master's terminal from its first key; a New session reads the current settings, and one that ends unsent says why.
- Usage: a window with no reading after a failed read shows as not read and points at the cause; API-key accounts keep the provider's wording; Codex usage reads get their access token.
- Sessions count no phantom terminal turn.
- Agents use the Command Line Tools git, with a plain reason when it is missing.

### Security

- Onboarding mapping and the code graph keep secret-like and git-ignored file names out of the prompt, the stored answer and the overview; nested repositories, old graphs and hidden folders stay out.

## [V1 build 18.15] - 2026-11-05 07:36 UTC - Internal build

Installed 2026-11-05 07:36 UTC. Commit `4368da8f` (`afd69922..4368da8f`).

### Added

- RCLI can be picked as the project master, end to end.
- Onboarding mapping reads the project's documents and shows what they cover.
- A finished worker's card opens its kept last screen, read-only.

### Fixed

- Live master: the app opens your default master session before the live switch turns on; a refused Open, a master that opened no session, and a failed open or read are each named.
- A GitHub device sign-in no longer races with a status read.
- Tools-off Claude launches run under a no-tools system prompt; print runs take one turn.
- A failed sign-in names its cause, and the sign-in helper never crashes on a closed error stream.
- A failed or slow onboarding mapping try starts a fresh try; the review and the TUI setup screen show the running try and why the earlier one failed.
- A native lead's own turn uses your current rules, is held while its background commands run, and is bounded by the session time left.
- Each Codex sign-in refusal has its own code and a plain sentence on the Usage card.

### Security

- The onboarding mapping never reads or lists git-ignored, secret-named or hard-linked documents, or other mounts.
- The TUI setup screen shows hidden and direction-changing characters in engine reasons as escapes.

## [V1 build 18.14] - 2026-11-04 19:16 UTC - Internal build

Installed 2026-11-04 19:16 UTC. Commit `afd69922` (`00efe55c..afd69922`).

### Added

- Network None: agents reach only their model's own hosts, through a per-agent proxy. The live master and research jobs use the same proxy.
- A native lead and its teammates get Claude Code's task list tools.
- Owner questions show whom each question is for.
- Agents started without hooks are counted and flagged like any other agent.
- Session history search covers tool inputs and outputs and native teammates' records.
- The engine makes each native teammate's own worktree on its own branch, commits a resting teammate's turns and lands them on the idle lead. A lead finishes only once its teammates' work has landed.
- A master without its live terminal gets the remember and recall tools.

### Changed

- Narrowing the network choice stops running agents and refuses kept panes with wider access.
- The desktop team editor and master area read harnesses through the harness catalog; Get set up waits for the first tools check once an account and a pool are ready.

### Fixed

- A resumed Claude app runs with the instructions its relaunch installed.
- The engine says why it could not make a native teammate's worktree.
- The Windows build compiles again.

### Security

- Agents cannot write the instruction, skill, hook, sub-agent, command, output style, rule and settings files their harness loads from its home, in any letter case. The Codex .agents folder and config.toml are denied, and Codex system skills are removed before each launch.
- Agents cannot run an app bundle's main executable.
- The model proxy refuses connections that hide their destination (encrypted TLS greetings), never connects back to this machine and logs no request secrets.
- A native lead's teammates cannot create a .git in shared team folders, follow case-variant worktree links, or link to each other's folders; the lead gets its own sealed temp folder.
- Session history masks more credential shapes on every read.
- Native teammate rows and the teammates' tmux server are pinned to the process that first answered.

## [V1 build 18.13] - 2026-11-03 17:43 UTC - Internal build

Installed 2026-11-03 17:43 UTC. Commit `00efe55c` (`ccb39fb7..00efe55c`).

### Added

- Land on branch: one project setting for where merges and Accept land and what change sets are measured against. The Accept dialog lands on it by default (desktop, and L in the TUI).
- Hidden folders: a project Sandbox setting that hides folders from agents, the master, research and the code graph. Git-tracked folders are refused.
- Project network setting for tools-on agents and the plugin check; None limits agents to their model's own port.
- Background commands at task end: a project setting to wait, wait with a grace time, or stop them (desktop, and Shift-B / Shift-G in the TUI).
- Re-ask a sub-agent once when its task ends with no reply text; a team setting, on by default.
- A setting for how instruction edits reach running agents; the master follows it.
- Choose skills per role.
- A first-edit hook reminder as a nudge option.

### Fixed

- Setup shows why a pack's hooks were refused; a hooks pack without plugin.json is refused with a plain reason.
- A new URL account that cannot be reached says so instead of "Connected".
- A grace time out of range says the allowed range on Save.
- Instruction edits reach only agents that are ready; a large edit reaches the live master once, and one edit restarts an agent only once.
- A finished agent is listed by number when a running agent has its name.
- A bare Claude agent's tool calls are counted from its transcript.
- The engine finishes its background change set checks before it stops.
- TUI and desktop layout fixes at 80x24 for the team settings, plugins and Sessions bar.

### Security

- A native lead's teammates run in their own tmux server and config home, and cannot move, replace or write the lead's home, launch folders, shell snapshots or session scripts.
- Team tool callers are identified by their kernel peer process, not by the pane they write from; the caller socket's folder is pinned.
- The hidden-folder git check fails closed.

## [V1 build 18.12] - 2026-11-02 02:55 UTC - Internal build

Installed 2026-11-02 02:55 UTC. Commit `ccb39fb7` (`f0f0f760..ccb39fb7`).

### Added

- Claude Code and Codex agents get a skill index with one-line descriptions.
- Workflow nudges setting, Off by default.

### Changed

- context-mode runs from one engine install instead of one npx install per agent.
- A lead worker's change set is measured against its lead's branch.
- Open change sets are checked with one git run per repository.

### Fixed

- Changes: agent branch names stay flat, a branch-list failure stays in view, and a landed commit that git later pruned counts as not landed.
- A task stays open while its agent's background command runs; background holds end when the agent is stopped or you interrupt.
- A coordination message and its wake are stored together; a refused wake says "Not sent".
- Only your Wake or restart of a lead clears its workers' stops; Update restarts an agent you stopped.
- A tmux server ended from outside shows no live agents instead of a closed terminal; the terminal reconnects without starting a new server.
- A native lead's window is made current, so its splits land beside it.
- TUI: a new session turns the live switch on at once and the master view keeps its state; the Delete question and team settings fit at 80 columns.
- Engine start makes no setup folder before you ran setup.
- Memory import refusals use plain sentences.

### Security

- A tools-off Codex agent runs without the shared app server and cannot ask for user input.
- The context-mode package is pinned and checked before any package script runs; its shared install is bounded.
- A hook folder past its size bounds is refused.

## [V1 build 18.11] - 2026-10-29 23:54 UTC - Internal build

Installed 2026-10-29 23:54 UTC. Commit `f0f0f760` (`0f9bc944..f0f0f760`).

### Fixed

- Changes shows an action's outcome only under the change set it was about.

## [V1 build 18.10] - 2026-10-29 23:39 UTC - Internal build

Installed 2026-10-29 23:39 UTC. Commit `0f9bc944` (`5400d569..0f9bc944`).

### Fixed

- After an engine restart, the master waits for the account sign-in re-checks to finish instead of failing its first turn.

## [V1 build 18.9] - 2026-10-29 22:15 UTC - Internal build

Built 2026-10-29 22:15 UTC; no install recorded (build 18.10 followed the same evening). Commit `5400d569` (`80687d07..5400d569`).

### Changed

- A change set is measured against your branch, so it holds only what the agent added. Junk folders are refused and orphaned worker change sets are closed.
- Workers never merge other branches into their own.

### Fixed

- When the team is stopped, the master view says so instead of waiting for the live master to start.

## [V1 build 18.8] - 2026-10-29 10:41 UTC - Internal build

Installed 2026-10-29 10:41 UTC. Commit `80687d07` (`95058b75..80687d07`).

### Added

- Commit the move: an instruction import whose file move is not committed yet can be committed from the app.

### Fixed

- Idle native teammates, and their tasks, keep their agent's name.
- A native lead's own turns are recorded; it is stopped as idle only when nothing waits on it, and an idle lead's kept session is woken with a new turn.
- A lead's worker change set closes once the lead holds its work, and leaves your Changes list.
- The master's merge refusal names an uncommitted instruction import.

## [V1 build 18.7] - 2026-10-29 03:50 UTC - Internal build

Installed 2026-10-29 03:50 UTC. Commit `95058b75` (`3a7cc9be..95058b75`).

### Added

- Instruction set: an instruction mode setting, project instructions created by the app, import from a file, import during onboarding, and a prompt for a pending import.
- Receipts and skill adoption read "unavailable" for agents started without hooks; modes that need hooks are refused for those agents.

### Fixed

- The RCLI master gets your instruction set.
- TUI: Ctrl-T opens Team while typing on the Conversation tab; the live terminal view stays in step with tmux.
- Agents run the real git instead of the macOS command-line-tools shim, which failed inside the sandbox.
- Idle agents stay in view, and each task names the lead that handed it out.
- A native lead on a signed-in account keeps one session.
- Idle per-turn sub-agents wake for their coordination messages; agents are told a note needs no reply and whom to write to.
- A lead's worker change set goes to the lead to merge, not to you.
- Skill adoption counts every enabled skill by name, including ones never used.
- An onboarded project gets the same agent access as a new project.

## [V1 build 18.6] - 2026-10-29 01:39 UTC - Internal build

Installed 2026-10-29 01:39 UTC. Commit `3a7cc9be` (`1c4e2cf0..3a7cc9be`). The coordination layer: agents talk to each other and leads run their own workers.

### Added

- Agent coordination: an engine coordination store, with work verbs and role tools on the team servers, so agents can message each other and hand work around.
- Messages reach Claude agents through a relay plugin (hooks, a wake monitor and read receipts) and Codex agents through the Codex app server or typing when idle.
- Native teams: a harness's own team lead runs in its own confined tmux server; its teammates' panes are adopted by name, shown once as their own graph node, and open from the graph.
- Leads: a lead's sub-agents work on their own branches and merge into the lead's copy; the lead merges a finished sub-agent, and finishing waits for unmerged sub-agents.
- Harness task branches, teammate worktrees and a master merge.
- The team graph is drawn as a tree of who leads whom, with a Message, Stop and Merge menu.
- Owner questions inbox, with severity alerts and Waiting-on-you links.
- Stop all, stop a harness or stop one teammate; a Max teammates per harness cap, a token budget and an account spread setting.
- Tool switches page and coordination settings in the desktop app and the TUI; the TUI opens team settings from the Team screen.
- The default pack ships a code graph reminder hook.

### Fixed

- A lead's sub-agent mode is fixed when it starts, never changed by the live setting.
- A native teammate's team call is answered as that teammate; teammates never enter the agent register.
- A stopped teammate row stays stopped while its stop is pending.
- The team keeps going when a message fails to deliver, and a master hand-off is committed all at once.

## [V1 build 18.5] - 2026-10-28 23:02 UTC - Internal build

Installed 2026-10-28 23:02 UTC. Commit `1c4e2cf0` (`e9ed09e9..1c4e2cf0`).

### Added

- Sub-agents can lead workers of their own.
- Named agent branches, and Accept into a branch you choose.

## [V1 build 18.4.2] - 2026-10-28 22:10 UTC - Internal build

Installed 2026-10-28 22:10 UTC. Commit `e9ed09e9` (`427db6cb..e9ed09e9`).

### Fixed

- The Hooks list was empty for packs installed earlier: the engine now fills in pack hooks from cached checkouts at start.

## [V1 build 18.4.1] - 2026-10-28 21:58 UTC - Internal build

Installed 2026-10-28 21:58 UTC. Commit `427db6cb` (`f2e0b5bc..427db6cb`).

### Fixed

- Hooks and Import memory failed to load in the desktop app ("Try again" did nothing): the main window now allows their commands.

## [V1 build 18.4] - 2026-10-28 21:47 UTC - Internal build

Installed 2026-10-28 21:47 UTC. Commit `f2e0b5bc` (`2401f591..f2e0b5bc`).

### Added

- Hooks follow your selection in the plugin catalog.
- Agent memory: remember and recall tools, with an Agent memory writes setting.
- Import memory: bring a folder of markdown memory files into a project.
- Agents can search past conversations with session tools.

### Changed

- Identical instructions are given to an agent once; long parts are indexed instead of pasted.

## [V1 build 18.3] - 2026-10-28 20:34 UTC - Internal build

Installed 2026-10-28 20:34 UTC. Commit `2401f591` (`70436db0..2401f591`).

### Changed

- Opening the master view starts the live master; the separate switch button is gone.

### Fixed

- New session no longer leaves a blank session behind, and deleting a session removes its transcript.
- After a session switch, the chosen session opens in the live terminal.
- Pages fill the window width, including in a full-screen window.

## [V1 build 18.2] - 2026-10-28 19:58 UTC - Internal build

Installed 2026-10-28 19:58 UTC. Commit `70436db0` (`52a5ba08..70436db0`).

### Added

- Changes notices change sets you landed or threw away outside the app, for example in your own terminal or IDE.
- New session and Sessions on the live master view.

### Changed

- The live master starts quietly, with its instructions given as system context.

### Fixed

- After a Mac restart the engine starts again: a stale locator or dead socket is set aside, and recorded file identities are read on the disk's new device number.
- After a Mac restart, worktrees, settings and asset files, configured RCLI profiles and masters, and usage records keep working.
- A run's leftover control socket from before a restart is removed.
- The harness catalog finds the RCLI host the engine launches.

### Security

- The sandbox stops agents from starting macOS apps, which crashed and popped up crash reports.

## [V1 build 18.1] - 2026-10-28 19:13 UTC - Internal build

Installed 2026-10-28 19:13 UTC. Commit `52a5ba08` (`b068d29f..52a5ba08`).

### Fixed

- Signing in failed after a Mac restart (the disk got a new device number): the engine accepts the new number when every file is otherwise unchanged.

## [V1 build 18] - 2026-10-28 18:04 UTC - Internal build

Installed 2026-10-28 18:04 UTC. Commit `b068d29f` (`7c65b712..b068d29f`).

### Added

- Per-project agent access level and toolchain switches, chosen in the desktop app. Lowering the level says beside it that agents running above it stop, and applies at each agent's next start.
- Own branch git layout: an agent can work in its own clone on its own branch, chosen at launch, and lands through a checked fetch. Masters and agents are told how their work lands.
- Agent time limits and the idle stop are team settings you own. The session limit is one wall clock per agent, it can stand without a task limit, and the idle-stop setting says when an idle master stops.
- Finished agents: a Finished fold-out (desktop) and Finished box (TUI) with Wake, and a Finished agents setting. Finished agents move out of the team graph behind Hide/Show.
- Wake a finished sub-agent by name or team id, in its own worktree; a signed-in agent's conversation resumes across turns and wakes; a master that is not live can wake a finished sub-agent, and its status read lists finished sub-agents.
- One conversation is kept across a master harness switch, and the live session shows where the harness switched.
- The live master shows as one console with a calm terminal palette, and you choose how to type to it.
- Copy from agent terminals with the copy key, a right-click menu or Copy output; Ctrl+C interrupts right after a copy; the mouse wheel reaches a full-screen agent.
- The desktop app tells you when change sets wait in Changes.
- The app shows the instruction files and skills each agent's harness reported; the project's instruction files reach every agent; headless Claude transcripts are kept.
- Suggested memories show beside new team notifications on a TUI Home card and are counted on Overview.
- Add an MCP server and every secret it needs in one request.
- Clear stuck sign-in, scoped to its own account.
- You can stop a team whose status outgrew one reply.

### Changed

- Stable releases ship only from master and are signed in the release environment.
- Saving Process instructions warns when a Claude master would refuse them; a Claude master's Process text is cut to the host limit instead of failing the turn.
- Usage By day labels follow fixed-offset days across daylight saving changes.

### Fixed

- Master turn inputs survive conversation pruning; a long turn is cut to fit a harness switch hand-off instead of being dropped.
- Terminal scrollback replays outside the attach deadline; a reopened terminal starts without a stale Jump to latest; an oversized tmux reply fails only its own request.
- The engine servers stay up through accept failures and client crashes; Cleanup, Usage and skill imports are served past the request deadline.
- Desktop: a failed setup read keeps polling with backoff; Restart engine is held off while an update installs; unsaved Process instructions are kept across saves; the terminal right-click menu stays inside its panel and works from the keyboard.
- TUI: a pool delete is refused when its projects changed since the confirm; the new project wizard fits the smallest frame; a many-line paste goes into the Process editor in one pass; keys in a closed terminal wait until Ctrl-]; a refused wake or setting reads as refused; an unconfirmed cleanup says so; the Changes selection never jumps to a neighbour.
- GitHub: a repository the engine cannot clone is listed instead of failing the page.
- Projects home: nested projects are re-pointed on a move, declared repositories stay out of Delete folder and Move, and a home folder holding a set-up project is never trashed.
- Removing a project clears its plugins, skill folder and kept conversations.
- Cleanup finishes a worktree git removed only part way, and removes a finished Own branch clone without running git in it.
- Import my skills lists every skill past 1024 entries; the app says why agents no longer get a stored skill and shows every SKILL.md header line.
- Your settings are kept across a new pack pin.
- An agent you stopped is never woken by the master, and your stop is written before it is applied.
- A refused live master stop names its project; a per-message master starts only once the stopping live master has ended.
- Here-documents work in agent shells.

### Security

- .git is denied in every agent-writable root, embedded repositories are refused at turn end, and a bare-repository layout is refused at turn end and at Accept.
- Own branch clones: agents cannot write the clone's git folder entry, repoint its borrowed store, add fallback repositories or pack indexes, or move folders into its object store. The engine fetches only from the clone's own git folder, with a time bound.
- Submodule stores are pre-created and protected.
- A landed link pointing outside the project or into the agents' folder is refused.
- A tools-off master is confined to its own working copy, with the repository read-only.
- Changes, cleanup and plugin actions check your client's access again before they act, including before projects-home disk work.
- Sub-agent text reaching the master is quoted line by line, with control characters dropped, so it cannot pose as another speaker.
- Hidden characters are escaped or refused in proposed commit messages, conflict paths, skill descriptions and memory text.
- The desktop app asks before opening a link whose text names another host, including localhost and IP addresses.
- MCP secrets: a launch whose command would hold a secret is refused; skill reads send no secret frames; an adopted pane's secret files are deleted when it stops; removed secrets are deleted once storage commits, and leftovers from a crash are swept.
- No GitHub token is saved after sign-out.
- Skill folders, agent temp files, research reports, agent git pointers and instruction files are read without following links or waiting on a pipe.
- The record of each agent's instruction files and installed skills is kept in engine state, not in the agent's writable home.
- The engine service never stores or copies an engine from a folder other users can change, and the Linux service path drops shared folders.
- Account profile endpoints are checked with the account URL rule.
- Your configured local model ports and tool ports stay free of agent listeners.
- Sign-in, refresh and logout hold the renewal lock.
- Identities must be one plain path component, and a worktree HEAD that names a ref of its own folder is refused.
- A revoked terminal viewer's next key is refused.
- Onboarding never reads or writes a draft through a link, and reuses only drafts this engine saved.

## [V1 build 17] - 2026-10-27 20:56 UTC - Internal build

Installed 2026-10-27 20:56 UTC as `dd6c4765`, then updated in place the same evening: `d438eff0` at 21:37 UTC and `7c65b712` later (time not recorded). Range `cd93c9e4..7c65b712`. Merged into master as `f78ccb0c`.

### Added

- Live master: the master area shows the master's real terminal. The header starts and stops it and says why it stopped; the earlier conversation stays readable.
- Master sessions: park and resume sessions, a Sessions list, and switch sessions from the master view. A resumed session continues its own harness conversation.
- The master conversation is a terminal-style console with / commands and @ tags; @ completes project files, and master replies render as Markdown.
- Team panel and activity strip beside the master conversation; the live master can message, answer and check the status of named sub-agents.
- Sub-agent events, wake rules and notification controls, in the TUI and the desktop app. Escalations reach Home's Needs you and open the conversation.
- Process page, in Settings and in each project, for app and project instructions; every master and sub-agent gets your instructions, and you can see what an agent received at its last launch.
- Instructions panel on the open agent card (desktop) and Instructions box on the Team screen (TUI).
- Import my skills from your Claude and Codex skill folders, checked in place before import; skill tags open the skill page.
- Account choice on Settings, Pools and a project's own order, with reset-aware ranking and a reason for each account.
- Usage counts onboarding, read-only and master runs, and piped Claude team agents, with a per-agent Usage card.
- Clean up finished agents and their worktrees from the Agents tab and the Team screen, after you confirm.
- An empty team plan means any runnable harness up to the team maximum; a team at the 64-agent maximum takes no more and says what to remove.
- Queued questions are counted on team graph nodes.

### Changed

- The harness catalog is served from a probe cache and desktop reads run side by side, so pages load faster.
- The project list shows up to 200 projects; Home no longer flashes Get set up.
- Long names and badges wrap instead of overlapping.

### Fixed

- A conversation started with New session reaches its first turn.
- The team status and graph fit in one reply.
- A failed read never starts the live master; a live master that cannot keep running turns its switch off; a live master on a signed-in account keeps its pane across an engine restart.
- The first message on a fresh project is sent once its conversation opens.
- Onboarding: a same-name leftover of a vanished folder is replaced, leftover agent folders never collide, and a step that cannot start shows its cause.
- A paused onboarding mapping asks for its account again every 10 seconds.
- Agent terminals take pastes without control characters and answer no color query.
- TUI: an open box gets every key before the plugins and Changes panels, and the Changes selection stays on its change set.
- A stuck change set is repaired on your next action, and background change set checks keep going past one bad record.
- Setup git under the sandbox no longer fails under load.
- GitHub sign-in stops polling once the device code expires.
- Usage record reads never block and token sums are bounded.

### Security

- Desktop and TUI strip direction-changing characters from what they show; project names with hidden characters are refused, and the Home card shows them as escapes.
- Links show their target on hover, and the app asks before a link whose text names another host.
- Accept takes only the change set head you reviewed; a second open change set for a worktree is refused.
- Sandboxed agents are denied nested .git metadata, the common index and the submodule store, and engine git does not recurse into submodules.
- The engine owns agent worktree HEAD moves, follows no symbolic ref or reflog link, and never waits on a pipe planted in an agent's git folder.
- Confined agents are kept out of the shared object store.
- Agent commits and staged files are bounded like edits, and added files are judged by your ignore rules.
- The relay token is kept out of every agent's environment and arguments.
- A tools-off Codex research agent is offered no tools and no web search.
- Hidden-character text is kept out of kept and global memory.
- Each folder chooser has its own fixed prompt.

## [V1 build 16] - 2026-10-27 06:53 UTC - Internal build

Installed 2026-10-27 06:53 UTC. Commit `cd93c9e4` (`e9f53b05..cd93c9e4`).

### Added

- Agents can build and test with your installed toolchains (read-only), without access to your credentials.
- Enter sends a message, Shift+Enter adds a new line.
- Accept proposes a commit message from the agent's own commit subjects, and you can edit it before landing.
- A change set now includes commits the agent made itself; agent commits carry your git identity with no extra trailer.
- Usage: a Settings Usage page in the desktop app and a Usage summary in the TUI, with totals, account limits, tokens by group and lines changed. The engine counts run tokens and polls account limits.
- Skill catalog with one-line descriptions; open a skill to read its full SKILL.md and files (desktop and TUI).
- Persona and role names for team agents; rename an agent's handle in the desktop app.
- Readable team graph: fixed cards, harness frames, pan, zoom and fit.
- TUI team tree rows lead with a status glyph and cut long text with an ellipsis.

### Fixed

- The team terminal input box always takes typing, and a team terminal scrolls through the session's history.
- The window stays fixed; only its panels scroll.
- A pending change the engine never recorded can be dropped instead of blocking every later change.
- Update on a master change set starts the resolve turn at once.
- Agents tab: the header counts working agents, the graph names the agent's account (not an id), keeps the newest stopped sub-agents, shows an idle or stopped agent's last task, and uses one name per agent everywhere.
- A master reply longer than 8 KiB no longer fails the turn.

### Security

- A tools-off Codex agent gets no web search.
- Research setup runs real git directly inside the sandbox, not through the macOS command-line-tools shim.

## [V1 build 15] - 2026-10-27 01:18 UTC - Internal build

Built 2026-10-27 01:18 UTC. Commit `e9f53b05` (`4530b411..e9f53b05`).

### Added

- Changes tab in the desktop app and the TUI: review each agent's change set with a plain-text diff, then Accept, Discard, Restore or Ask agent to update.
- The engine commits each agent's work at the end of its turn, one change set per agent. Accept lands it as one squash commit authored by you on the checked-out branch. Discarded work can be restored for 7 days.
- Tools-on master turns and team agent turns land in change sets.
- The master shows a note when a turn's edits are waiting in Changes.
- Change sets are reconciled and pruned when the engine starts.

### Changed

- On Linux the code graph build, and on Windows the onboarding mapping, stop with a plain "not available" line.

### Fixed

- A Claude master turn reads tool results within its allowance; one large Claude or Codex event no longer fails the turn.
- A conflicting Accept is reported once, in the change set's own note.
- Changes names a moved or altered agent working copy in plain words.

## [V1 build 14] - 2026-10-26 22:49 UTC - Internal build

Built 2026-10-26 22:49 UTC. Commit `4530b411` (`1edd9d54..4530b411`).

### Changed

- On Linux, team agents and research stop with a plain "not available in this version" line; on Windows, team agents do the same, and so does onboarding mapping on Linux.

### Fixed

- An idle agent's terminal opens its last run's screen.
- A very large harness output line no longer fails the turn.
- A stop whose sandbox watcher process was killed reports cleanup as uncertain instead of clean.

## [V1 build 13] - 2026-10-26 21:46 UTC - Internal build

Built 2026-10-26 21:46 UTC. Commit `1edd9d54` (`49dfe95e..1edd9d54`).

### Fixed

- A master turn stopped by a failed sign-in renewal says so in plain words; a failed renewal holds the account for a minute and fails over instead of marking it rejected.
- Replacing an account key lifts a rejected sign-in, and a failed confirm after Replace key keeps the new key.
- Sign-in renewals never stall the engine, and a check during a renewal leaves nothing behind.
- A paused onboarding mapping no longer blocks onboarding of other projects.
- Moving the projects home points each project's repositories at the moved folder.
- An agent's runtime folder is removed when it ends; stale ones are pruned at start.
- A busy database never loses a master's reply.
- Setup narrows the previous build's uv tool folder and updates it; the default pack never replaces your own item with the same name.
- The tmux holder runs under /bin/sh whatever your login shell is.
- The desktop banner names every kind of kept change.
- The engine updates and starts where no system service is installed.

### Security

- A Windows agent can add files beside read-only paths without gaining write access to them.
- On Linux, git metadata that git follows is not granted to agents.
- A tools-off Codex master runs from a private copy of the sign-in that is never refreshed in place.
- Stream diagnostics log frame kinds and sizes, never content.
- A checked MCP server, and research and onboarding agents, are stopped by their run's sandbox watcher like every other agent.

## [V1 build 12] - 2026-10-26 18:17 UTC - Internal build

Built 2026-10-26 18:17 UTC. Commit `49dfe95e` (`5279353b..49dfe95e`).

### Fixed

- A sub-agent that Codex starts on its own is interrupted and the turn continues.
- A revoked profile sign-in fails the turn as invalid authentication.
- A failed Codex master turn leaves a redacted stream diagnostic.

### Security

- A Codex approval request that arrives together with its turn's end is still declined.

## [V1 build 11] - 2026-10-26 17:43 UTC - Internal build

Built 2026-10-26 17:43 UTC. Commit `5279353b` (`4eb7e0b5..5279353b`).

### Fixed

- A lapsing managed Claude sign-in is renewed with a real turn; the lock Claude Code takes beside a managed home is accepted.
- A Claude team agent survives output lines after its result.
- A task from a conversation master reads as from the master.

### Security

- Tools-on Claude and Codex team agents cannot start their harness's own sub-agents.
- A team master's delegate server is the engine that started it.
- When a run ends, the sandbox stops whatever its shells left running in the background; the sandbox watcher leaves the harness's process group before the run is stopped, and its report survives its exit.

## [V1 build 10] - 2026-10-26 14:40 UTC - Internal build

Built 2026-10-26 14:40 UTC. Commit `4eb7e0b5` (`afd715b8..4eb7e0b5`).

### Changed

- A tools-on master starts sub-agents only through CrazyAgent's delegate; Claude Code's and Codex's own sub-agents are off.

### Fixed

- A Claude master turn ends at its result and keeps a stream diagnostic.
- The projects home watch starts off the storage thread, so no request waits on it.
- The engine database lock is released whenever its hold is dropped.
- Linux and Windows builds compile clean.

### Security

- A tools-on master's delegate server must be the registered engine.

## [V1 build 9] - 2026-10-26 12:56 UTC - Internal build

Built 2026-10-26 12:56 UTC. Commit `afd715b8` (`6507640f..afd715b8`).

### Added

- The desktop app shows the tasks a conversation handed off and their outcome; the outcome comes back into the conversation.
- A Codex conversation master follows the project's master tools switch, and its tools run without prompts.

## [V1 build 8] - 2026-10-26 12:25 UTC - Internal build

Built 2026-10-26 12:25 UTC. Commit `6507640f` (`5b932baf..6507640f`).

### Added

- A conversation master can hand tasks to the project's team.

### Changed

- A tools-on Claude master runs with the team tool flags and only the engine's delegate server.
- A rejected sign-in lists as "Sign-in rejected" and offers Sign in again.

### Fixed

- A conversation turn follows the project's master tools switch.
- Team agents and research pass over an account whose sign-in was rejected.
- A credential the engine cannot read holds the account for a check instead of rejecting it.
- A harness with no sign-in ends the turn without blaming the provider.

## [V1 build 7] - 2026-10-26 11:31 UTC - Internal build

Built 2026-10-26 11:31 UTC. Commit `5b932baf` (`51ede04a..5b932baf`).

### Fixed

- The onboarding mapping agent skips paused pool accounts, waits while all are paused, and shows when the pause ends.
- A rate-limited research run pauses its pool account and fails over; research waiting on a limit reads "Rate limited" until it ends.
- Checking an existing sign-in runs beside the account's running agents.
- The shared limit check understands Claude Code's own limit lines.

## [V1 build 6] - 2026-10-26 10:15 UTC - Internal build

Built 2026-10-26 10:15 UTC. Commit `51ede04a` (`fdc3c481..51ede04a`; a first pass at `c1dbd80f` was rebuilt five minutes later). The first build where a live master reply came back end to end in the installed app.

### Fixed

- A startup sign-in re-check that finds the profile still in use checks again within its time bound.
- A master turn runs again when another engine connection held the database write lock.
- A conversation's first worktree is checked out off the storage thread, so the app keeps answering.
- A change the engine answered with an unknown outcome is checked once, and Home replies before its watch starts.

## [V1 build 5] - 2026-10-26 09:45 UTC - Internal build

Built 2026-10-26 09:45 UTC. Commit `fdc3c481` (`03530fe3..fdc3c481`).

### Fixed

- After an engine restart, failover re-checks every pool account and waits out a busy account-check host.
- A sandboxed agent on macOS trusts the system root certificates instead of the keychain it cannot read.
- Every waiter on a native sign-in gets the same answer.

### Security

- A managed Codex research agent runs without the account's ChatGPT apps and plugins.

## [V1 build 4] - 2026-10-26 08:55 UTC - Internal build

Built 2026-10-26 08:55 UTC. Commit `03530fe3` (`1277620e..03530fe3`).

### Added

- A lapsing managed sign-in is renewed before and during an agent turn, and before the master gets its credential copy.

### Fixed

- A native sign-in shows as finished only once the engine has released it.
- A headless Claude agent keeps its temp folder in its own home, not in a shared system temp folder.
- After an engine restart a conversation continues in the same session, and its worktree returns to Ready.

## [V1 build 3] - 2026-10-26 08:15 UTC - Internal build

Built 2026-10-26 08:15 UTC. Commit `1277620e` (`443b5578..1277620e`).

### Fixed

- A rejected sign-in holds the account until you sign in again, instead of pausing it.

### Security

- A Claude master turn signs in from a per-turn credential copy, not from the keychain.

## [V1 build 2] - 2026-10-26 07:35 UTC - Internal build

Built 2026-10-26 07:35 UTC. Commit `443b5578` (`8769dc88..443b5578`).

### Fixed

- TUI: press e on the Conversation tab to end an open master conversation, so the project can be removed afterwards.
- An open master conversation stays open when only settings outside it change.
- Signing an account out names and ends the master turns and research jobs still using it.
- A turn that no pool account can run records why.
- TUI: the typing keys show while the master input has them; a set-up folder gets the pool its setup picked; the code graph result stays on the setup review.

## [V1 build 1] - 2026-10-26 07:15 UTC - Internal build

Built 2026-10-26 07:15 UTC. Commit `8769dc88` (changes since 0.2.0 on master `772b699b`). The first local macOS app build.

### Added

- The command line starts the default engine, auto-starts the TUI, manages the engine service and prints --version.
- The engine runs as a per-user login service (launchd on macOS, user services elsewhere) and starts automatically; each data root gets its own service.
- The desktop app shows engine and app versions, offers Restart engine on a version mismatch, and Check for updates on the engine-needs-a-restart screen; Settings and Updates open while the engine is down.
- Accounts page: delete accounts, cancel sign-ins, sign in again and sign out from the account drawer (naming the agents that still use it); TUI deletes selected accounts too.
- Add account offers one OpenAI and one Anthropic entry; Needs you shows one row per account to sign in.
- The master area shows the master's account and why each blocked account cannot run.
- Any number of agents can share one signed-in account.
- Project Settings offers No pool, and delete confirmations name what changes.
- Add an existing folder where it is, or copy a repository standalone; moves that would break linked worktrees are refused.
- The code graph step can be switched off and says when Graphify is not installed.
- Stopping a team agent is answered; Remove project ends the open master conversation in the same action.

### Changed

- Each master turn runs on a ready account of its pool; removing a pool or account takes it off whatever used it.
- A full conversation history drops its oldest message instead of refusing the next one.
- Failures without their own sentence show their code; the master's stop cause is stored and shown with a plain sentence.
- Update failures read plainly, with a "not published" message per channel.

### Fixed

- The next message replaces a turn the harness left uncertain, and a message refused for a stale conversation starts a new one.
- A send to a conversation opened before an engine restart is refused plainly.
- A refused change frees the controller for the next one; an unconfirmed change settles at the next job instead of blocking until restart.
- Onboarding finishes when you save the setup, shows the mapping agent's own failure with Retry, and gives the chosen pool to the project's master.
- The project folder is recorded as its repository, and older projects are repaired on start.
- Desktop bridge calls queue behind a bounded wait instead of failing busy, and long engine calls no longer hold the bridge.
- Pool changes are saved onto the pool as it is now.
- A terminal view no longer answers terminal queries into the agent, and a newly attached view learns the agent's paste and cursor-key modes.
- The tmux server ends on engine stop unless a team agent lives on.
- A socket path that is too long is reached through a short private address.
- Stored sign-ins are checked again on each new engine run; a logged-out account reads as not signed in.

### Security

- A saved account key is never sent over plain http to another host.
- Codex sign-in pages are shown only on the OpenAI sign-in origin.
- Managed agents run in their own home with a credential copy that is never refreshed in place.
- MCP servers are checked inside the agent boundary.
- Linux: Landlock is accepted only when every requested right is enforced; agents' signals and abstract sockets are scoped to their own domain; every agent starts in its own session without the host's terminals.
- macOS: agents get only the device nodes they need, and git redirect files are refused.
- Each worker can write only inside its own git worktree; repositories are accepted only after checking what their git config would run, and adding a worktree runs no repository-configured command.
- Windows: AppContainer grants a narrower profile drops are taken back.
- The model relay refuses percent-encoded dot segments and stops in-flight requests when it stops.
- A Claude profile keychain behind a planted link is refused; agent-planted skill links and pre-existing Codex transcripts are never followed or reused.
- Git config, Claude settings, onboarding drafts and the master's brief are read through bounded no-follow opens.
- The TUI shows hidden and direction-changing characters as escapes, and shows a whole suggested memory before it is saved.
- Terminal captures are framed by row count so pane text stays data, and the master gets only explicit prompts, never typed keys.

## [0.2.0] - 2026-10-25 23:34 UTC - Dev build

Published 2026-10-25 23:34 UTC as dev build `0.2.0-dev.202610252305` from master `772b699b`. Version set to 0.2.0 at 20:59 UTC (`caea6194`). No stable release.

### Added

- Release pipeline: per-platform and per-channel signing decisions, signed Windows binaries, packaged and signed CLI archives, desktop bundles, and `latest.json`, `cli-latest.json` and `SHA256SUMS` written only after every signature is verified.
- Signed updates: `crazyagent update` with signed downloads and a safe swap, and Settings > Updates in the desktop app with channel, check, install and restart.
- Numbered engine database migrations, with a backup, restore, and a refusal to open data from a newer version.

### Changed

- Replacing an API key updates the account in place instead of adding a new one.
- Team history keeps a recent window instead of lifetime caps.
- Harnesses that cannot run team agents are not offered for them.
- Desktop shortcuts use Ctrl on Windows and Linux.

### Fixed

- The desktop app finds the default engine when opened from the Dock.
- Each project has its own page, so switching never shows the old project's data or seeds its settings.
- Confirm dialogs stay open while their step runs; menus stay open when focus moves; Cmd+Enter follows the Send button's rules.
- The master conversation reads every page and keys messages by turn.
- A stopped or cancelled Claude master and a Codex master end their whole process group cleanly.
- A crashing request no longer takes the engine server down; GitHub and account network work run outside the request deadline.
- A projects home move is recorded as pending and finished from disk, and a half-deleted source is made whole on rollback.
- Add existing folder accepts Windows folders.
- Each research job is claimed by one run attempt at a time, and a cancelled job stays cancelled.
- An interrupted settings save is staged again on restart, and a save overtaken by a newer one is recorded as rejected.
- Model-access reads work for every account the config allows.
- A finished sign-in releases its hold once its helper process is gone, so the next sign-in can start.
- The engine refuses clients past its connection caps with a plain "busy" instead of stalling.
- RCLI host messages are bounded by their real wire size, and an oversized turn fails cleanly instead of stopping the host.
- The TUI keeps a background New project run alive when you leave its page, and an unchanged MCP server line saves nothing.

### Security

- Python tools install only from bundled hash locks.
- Tools-on agents on macOS get IP network access only, and agents can open only the terminals their own sandbox created.
- Linux Landlock agents cannot use Unix-domain sockets.
- The model relay checks its token before reading the request body.
- Hidden and reordering characters are refused in memory text and shown as escapes in the desktop app.
- Markdown is parsed in linear time.
- Engine git calls run with fsmonitor and the pager off, and the git metadata your own git reads stays in place for sandboxed agents.
- Random IDs come from the platform's secure source.

## [0.1.0] - 2026-10-18 01:41 UTC - Internal build

Project start 2026-10-18 01:41 UTC (`2a4bbd2f`, `8e9293f0`). Every change up to the 0.2.0 version bump. Never released. Everything below was new in this version, so fixes made along the way are folded into the lines they fixed.

### Added

- A local engine that keeps all state in one database and survives restarts.
- An authenticated private connection between the engine and its clients (desktop app and TUI), with every client bound to one engine.
- A durable runner service that carries agent work, with authenticated worker runners.
- An account registry with a provider catalog and no limit on the number of accounts.
- Managed sign-in for Claude and Codex subscriptions; an existing Codex sign-in is reused without a new device code.
- Saved API key and URL accounts, set up for agents when they join a pool.
- Add account dialog, account drawer and Accounts page in the desktop app; Accounts list, details and Add account flow in the TUI.
- Named pools of up to 15 accounts, with a pool editor in both clients; pools are kept from deletion while a project uses them.
- New pool from selected accounts, pool readiness on pool cards, and the most ready pool preselected in New Project.
- Pool scope is enforced: agents use only accounts from their project's pool.
- Provider quota policy with kept history, in the desktop app and the TUI.
- Harness catalog served by the engine, with a Harnesses page (details and Check again) in the TUI.
- Projects home: one watched folder for projects, with Add, Existing folder (move or copy) and moving the whole home; deleted folders go to the Trash.
- New Project wizard: details, documents, team plan, pool and research jobs, with validation and smart team defaults. The TUI keeps an unfinished draft and lists it on Home.
- Projects with declared repositories; edit, remove and research controls per project.
- Onboarding of an imported folder: a code graph step and a mapping agent that drafts a project overview for your review.
- GitHub: sign in by device flow, token or the gh tool, list your repositories and clone one.
- The engine answers git credential requests from its own store.
- Project master: start the project's master and talk to it from the desktop app and the TUI Conversation tab.
- The master runs on a signed-in Claude subscription, a signed-in Codex subscription, or the RCLI host.
- The master gets the project's context, memories and team plugins, and your profile as a preface.
- Master turns are sent to the harness automatically once you send them.
- Stop next to Send while the master replies, Send again on a stopped turn, "Couldn't send" on a failed send, and a seconds counter while it thinks.
- Edit on the project brief opens the project details it is written from.
- Team plan: the engine starts the master and sub-agents on signed-in accounts, each in its own terminal pane on the engine's private tmux server.
- A master with tools on gets the delegate tool; a fresh master gets the recent team conversation.
- The master's plan steps are shown, and you can hand a step or a task to a sub-agent.
- Pause and resume a project's whole team; keep a project's master running after quit.
- What you type into a sub-agent is noted to its master.
- Agent register: every team agent with its role, harness, account, status, task and where it works.
- Stop, Interrupt and Restart per agent, recovered after an engine restart.
- Agent panes left running by an earlier engine are taken over on restart; orphans are ended.
- Team panel in the desktop app and a Team view in the TUI, with the live team graph and the selected agent's terminal beside it.
- Agent terminals stream into the desktop app and the TUI, with keys passed through to a focused agent.
- Follow new agent log lines with Jump to latest after scrolling up; View log on agents.
- Tasks waiting for a pool account and accounts that need to sign in show under Needs you.
- Research jobs queued at project creation run in the project sandbox, on Claude Code or Codex, with a pool account the harness can drive.
- Stop, cancel and retry research; Try again on failed research; each research agent's latest output and activity.
- Research reports render as a safe Markdown subset; keep a report as an approved memory with one click.
- Project and global memory, with one-click promotion of a project memory to every project.
- Suggested memories under Needs you; edit a suggestion before saving it.
- Save selected text from a project file, or a context selection, as a memory.
- Delete project memory for good; view and delete memories in Settings.
- Add documents to a project, and delete context files or documents.
- Skills and approved MCP servers, globally or per project, with per-project switches.
- Edit what an MCP server runs, and save, replace or remove its secrets.
- Agent launches get the project's approved plugins, context and memories, with secret values filled in only at launch.
- Setup: a Setup place for first run and in Settings, with tools, default pack ticks, MCP approval, home folder and GitHub account.
- Setup installs pinned uv, Graphify and tmux into the app data folder on macOS, and uv and Graphify on Linux and Windows.
- A pinned default pack fetched at its pinned commit, with nine skills written for CrazyAgent under Apache-2.0; switch, install or remove pack entries.
- The team defaults to the pool's best signed-in harness.
- Desktop app shell, theme and UI kit; Home with Needs you and eight Recent activity rows; project cards showing live agents and research.
- Desktop Settings: accounts, pools, memory, plugins and skills, pack, projects folder and GitHub; Project Settings for details, research, pool and team.
- Go to anything, keyboard shortcuts and support details in the desktop app.
- macOS folder and file choosers for skill folders and documents.
- Open links, project folders and context files from the window, with plain messages when opening is refused.
- The CrazyAgent brand mark, favicon and app icons.
- Linux and Windows desktop bundle targets.
- TUI: a rebuilt shell, theme and frame with the same places as the desktop app, a vt100 terminal pane and terminal key encoding.
- TUI: Home lists missing tools in Get set up, the Project screen covers research, context and memory review, and quitting warns before an unsent message or memory edit is lost.
- Terminal viewers have bounded buffers and capped scrollback, and leave nothing behind after quit; a Windows ConPTY terminal host.
- Open, reveal and copy on macOS, Linux and Windows.
- A release channel tool for dev and stable builds.

### Security

- Every harness launch runs inside a per-project sandbox: Seatbelt on macOS, Landlock with a bubblewrap fallback on Linux, and an AppContainer boundary with a kill-on-close job on Windows. A launch with no project sandbox profile is refused.
- An agent may write only in its own engine worktree, and a master's network is limited to its own model endpoint.
- Secrets live in the system credential store (macOS Keychain, Linux and Windows secret stores) under the engine's own namespace and are kept away from agent processes.
- Agents never see real model keys: a loopback relay swaps a placeholder token for the real key.
- Native sign-in: one helper process at a time holds each signed-in profile, tied to your engine and checked by its executable's identity.
- Clients, worker runners and verification receipts are authenticated.
