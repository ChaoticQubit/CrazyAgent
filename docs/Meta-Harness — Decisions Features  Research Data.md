# Meta-Harness: Decisions, Features & Research Data

_Record covering 2026-10-09 → 2026-10-15. This document contains only the requirements and decisions stated by the project owner, the feature list as specified, and research facts (tool capabilities, prices, star counts, issue counts, terms-of-service text, benchmark and survey data). It contains no verdicts, ratings or recommendations._

_Notation: "r" = GitHub reactions, "c" = comments, "★" = GitHub stars, "pts" = Hacker News points. Counts were read 2026-10-09 to 2026-10-15. "Unverified" marks items that could not be confirmed from a primary source. Star counts were read from repo pages because the GitHub API was blocked during most of the research, so they may lag._

---

## Contents

1. Problem statements (as stated)
2. Decisions and stated requirements
3. Feature list (as specified)
4. Open questions raised (unanswered)
5. Research data by feature area
   - 5.1 Running multiple harnesses from one layer (F1)
   - 5.2 Config, rules, skills, plugins, MCP portability (F4)
   - 5.3 Session handoff and shared memory (F2, F3, F9)
   - 5.4 Multiple accounts and subscriptions (F5)
   - 5.5 Pause and resume at usage limits (F6)
   - 5.6 Sessions, worktrees, sandboxes, "Kubernetes for agents" (F7, F8)
   - 5.7 Cross-machine and cloud portability (F10)
   - 5.8 Usage monitoring and analytics (F11)
   - 5.9 Native macOS UI picker for agents (F12)
   - 5.10 Test integrity of agent-written tests (F13)
   - 5.11 Task-type model routing (F14)
   - 5.12 Bundles and distributions (F15)
6. Terms of service: text and enforcement timeline
7. Integrated-platform capability matrix
8. Demand and market data
9. GitHub issue data (complaints and requests)
10. Unverified items
11. Sources

---

## 1. Problem statements (as stated)

| # | Date | Problem as described |
|---|---|---|
| P1 | 2026-10-09 | Works in Claude Code (with AGENTS.md / CLAUDE.md files, rules, plugins and skills set up there), and all context and session history stays inside Claude Code. When session limits or token usage run out, switching to Codex loses all history and context, and skills and settings must be set up again because each harness uses different files. |
| P2 | 2026-10-09 | Has multiple Claude subscriptions and multiple Codex subscriptions. Switching means logging out of the current account and logging in with another, by hand. |
| P3 | 2026-10-09 | When session limits are exhausted, subagents and background processes die. |
| P4 | 2026-10-09 | Starting a second session (Claude Code or Codex) on the same project without affecting the running one requires creating a worktree by hand or asking the agent. Session management is manual. Context can't be shared between sessions. |
| P5 | 2026-10-09 | Moving to a different machine or to the cloud means repeating the whole setup by hand or writing and porting scripts. |
| P6 | 2026-10-09 | No central view of usage and tokens, of what is and isn't working across projects, or of logs and reasoning across vendors. |
| P7 | 2026-10-15 | Building a native macOS app. The agent doesn't understand visual problems described in words ("text is clipped", "scrolling isn't working", "this looks weird"). Taking a screenshot every time and sending it to the agent is manual. |
| P8 | 2026-10-15 | Doesn't review most of the code and tests agents write, because reviewing would slow development. Agents review the code and tests. There is no way to know whether the tests are real, or whether the agent mocked them out or bypassed them to make them pass. |
| P9 | 2026-10-15 | Has observed that models differ by task type: Gemini 3.6 for debugging and fixing, Claude Opus for writing features and code, GPT models for identifying problems and brainstorming. Wants to route by type of work. |
| P10 | 2026-10-15 | Individual tools exist for individual problems, but installing, configuring and managing many separate tools is tedious and manual, even for a developer who can script it, and harder for other people. |

---

## 2. Decisions and stated requirements

| # | Date | Decision / requirement |
|---|---|---|
| D1 | 2026-10-09 | A layer above harnesses. Any harness can run inside it (named: Claude Code, Codex, Pi, OpenClaw, "whatever harness is available"). |
| D2 | 2026-10-09 | All harnesses share the same memory, the same session per project, the same plugins, skills and rules, and the same settings. |
| D3 | 2026-10-09 | Multiple subscriptions are added to the layer. When limits run out on one, it switches to the next automatically. |
| D4 | 2026-10-09 | Pause and resume work when limits are exhausted, instead of subagents and background processes dying. |
| D5 | 2026-10-09 | The layer manages sessions and worktrees, including automatic creation, closure and deletion. Everything runs inside a sandbox. Described as "Kubernetes for harnesses." |
| D6 | 2026-10-09 | Context is shared between parallel sessions on the same project. |
| D7 | 2026-10-09 | Integration is the point: one single system rather than many separate tools. |
| D8 | 2026-10-09 | Portability: a single app or binary. Log in on a Mac, on other machines (10 named as an example) or in the cloud, and all sessions, management state, context and configuration are available immediately, with no manual setup. |
| D9 | 2026-10-09 | Centralized usage and token monitoring, codeburn-style analysis of what works and what doesn't, per-project graphs, and analysis of logs and reasoning, in one place per project and not locked to one vendor. |
| D10 | 2026-10-09 | **Not a product and not a company. An open-source project**, to be built quickly and released. |
| D11 | 2026-10-09 | Mine user frustrations (Reddit, GitHub issues of Traycer and similar tools, alternatives) and include fixes for them in the layer. |
| D12 | 2026-10-15 | Add a native-app visual inspector: select elements or areas in a running Mac app (like Chrome DevTools' element picker) and send a comment about the selected element to the agent. |
| D13 | 2026-10-15 | Add an external test-verification layer, separate from the coding agent and the application, that verifies the tests themselves (unit, integration, end-to-end, smoke, scenario-based), not the product code. |
| D14 | 2026-10-15 | Add a built-in task-type model router: development code to Opus, tests to GPT models, debugging of broken tests to Gemini, switching automatically while keeping context and memory, delivering the complete feature. |
| D15 | 2026-10-15 | **Core thesis:** a single open-source tool that you download and install and that gives all these features out of the box, because managing different tools is a manual job. |

---

## 3. Feature list (as specified)

| ID | Feature | Added |
|---|---|---|
| F1 | Run any harness (Claude Code, Codex, Pi, OpenClaw, others) inside one layer | 2026-10-09 |
| F2 | One shared memory across harnesses | 2026-10-09 |
| F3 | Shared session and context per project across harnesses (continue in another harness without losing history) | 2026-10-09 |
| F4 | One shared set of plugins, skills, rules and settings across harnesses | 2026-10-09 |
| F5 | Multiple subscriptions held in the layer, with automatic switch to the next one when limits are exhausted | 2026-10-09 |
| F6 | Pause and resume when limits are exhausted, including subagents and background processes | 2026-10-09 |
| F7 | Session management: parallel sessions on one project without disturbing each other; automatic worktree creation, closure and deletion | 2026-10-09 |
| F8 | Everything runs inside a sandbox ("Kubernetes for harnesses") | 2026-10-09 |
| F9 | Context shared between parallel sessions | 2026-10-09 |
| F10 | Portability: one app or binary, log in on any machine or in the cloud, with sessions, context, config and accounts following | 2026-10-09 |
| F11 | Usage and token monitoring, codeburn-style analytics, per-project graphs, log and reasoning analysis | 2026-10-09 |
| F12 | Native macOS app inspector: pick an element or area in a running app, comment, send to the agent | 2026-10-15 |
| F13 | External test-integrity verification of agent-written tests (all test levels) | 2026-10-15 |
| F14 | Task-type model router (features → Opus, tests → GPT, debugging → Gemini) with context and memory kept | 2026-10-15 |
| F15 | All of the above in one open-source tool, installed once, working out of the box | 2026-10-15 |

---

## 4. Open questions raised (unanswered as of 2026-10-15)

- Which component gets built first.
- Whether the first release includes new features or only bundles existing tools.
- Which harnesses and operating systems the first release supports.
- For F13, whether to build new tooling or contribute to an existing project (Gauntlet).

---
## 5. Research data by feature area

### 5.1 Running multiple harnesses from one layer (F1)

**Orchestrators and layers**

| Tool | Harnesses | Documented capabilities | License / ★ / status | Price |
|---|---|---|---|---|
| Traycer Desktop | Claude Code, Codex, Cursor, OpenCode, Pi, Antigravity, Grok, Devin, Amp, Kiro, Copilot, Droid, Hermes | v1.1.7: multiple accounts per provider, switchable per conversation. v1.2: one shared copy of skills/MCP/plugins across providers; usage dashboards (cost, tokens, tool calls by harness/model/chat); chats backed up to the cloud. v1.3: two-step worktree review before deletion. v1.4: model routing (other account on same agent → equivalent model on another provider → wait for reset → notify); optional automatic cleanup of merged/pushed worktrees. Routing is off by default and applies to chat agents only, "never to Terminal agents"; a switch "starts a new agent session from the chat's history". No sandbox in the changelog. | MIT, ~1.5k★, v1.4.2 | Free; Sync $16/user/mo yearly or $20 monthly |
| Omnigent (Databricks) | Claude Code, Codex, Cursor, OpenCode, Hermes, Pi, Antigravity (`agy`); Grok Build and Devin via ACP; OpenClaw; YAML agents | Described as "the open-source meta-harness for all your AI agents". Policies at server, agent and session level. Sandboxing: `bwrap` on Linux (mandatory), `seatbelt` on macOS, Job Object on Windows; cloud sandboxes on Modal, Daytona, E2B, CoreWeave, Kubernetes, Databricks. Sessions "follow you" across terminal, browser, phone and desktop, with messages, subagents, terminals and files in sync. Memory through extras (`hindsight`, `s3`). No failover. | Apache-2.0, ~10.3k★, 1.6k forks, 557 open issues, "Status: alpha" | Free |
| Conductor | Claude Code, Codex, Cursor, OpenCode | Mac app; each workspace is a git worktree; `$CONDUCTOR_PORT`; archive saves git state and can be undone; cloud microVMs (Firecracker) in early access | Closed source; $22M Series A (Mar 30, 2026, Spark and Matrix) | Free / Pro $50 / Teams $60 per user |
| Superset | 20+ CLIs incl. Claude Code, Codex, Gemini, Antigravity, Pi, OpenCode, Cursor, Amp | Automatic worktree per task (docs: worktrees "do not sandbox processes or prevent merge conflicts"); quota meters per Claude and Codex account; manual account switch; "Continue with another agent" seeds recent terminal output | Elastic License 2.0 (relicensed from Apache-2.0), ~14.9k★, YC | Free / Pro $20 ($15 yearly) |
| Emdash | 25–34 CLIs | Parallel worktrees, SSH | Apache-2.0, ~5.6k★, YC W26 | Free |
| Agent Deck | Claude (full), Gemini, Codex, OpenCode, Copilot, Cursor, Crush, Hermes | tmux sessions; MCP defined once with a pooled MCP process; Claude-only skills manager; opt-in `--worktree`, `worktree finish/cleanup`; Docker per session; manual `session switch-account` (Claude) | MIT | Free |
| Claude Squad | Claude, Codex, Gemini, Aider, OpenCode, Amp | tmux + worktree TUI | AGPL-3.0, ~8.3k★ | Free |
| Vibe Kanban | 10 agents | Kanban + worktree per attempt, cleaned up automatically | Apache-2.0, ~28.1k★. Bloop shut down Apr 10, 2026; community-maintained | — |
| Nimbalyst (replaced Crystal, Feb 2026) | Claude Code, Codex (OpenCode, Copilot in alpha) | Kanban workspace | MIT | Free for individuals |
| Sculptor (Imbue) | Claude Code; Codex via OpenAI API key | One container per agent; pairing-mode sync; merge-conflict flagging | — | Free beta |
| Toad | ACP agents | Terminal ACP client | AGPL-3.0, ~3.5k★ | Free |
| 1Code | Claude Code, Codex | Client | Archived Jul 7, 2026 | — |
| Happy | Claude Code, Codex | Mobile/remote control | MIT, ~23.5k★ | Free |
| Omnara | Claude Code, Codex | Remote control; now positioned as a "control plane for managed agents" | YC S25 | $20/mo |
| coder/agentapi | 11 agent TUIs | HTTP API wrapper | MIT, ~1.5k★ | Free |
| Claudexor | Claude Code, Codex, Cursor CLI, OpenCode, raw APIs | "Continuation packets" between harnesses/accounts; quota tracking (5h, 7d); opt-in rotation | MIT, 13★, v3.0.0 (Jul 20, 2026) | Free |
| AgentsRoom | Claude Code, Codex, Grok, Antigravity, Cursor | Automatic account switch at a quota threshold; copies the Claude transcript to the new account | License unknown | Free download |
| agent-squid | Claude Code, Codex, Cursor, OpenCode, Pi | Browser orchestrator; manual "context pins" | MIT, 11★ | Free |

**IDE and platform hosts**
- **Zed:** runs Claude Agent, Codex, OpenCode, Copilot, Cursor, Pi and Gemini over the Agent Client Protocol (ACP). Each agent handles its own login. Zed's MCP servers may be forwarded; Zed's skills and profiles don't apply. The ACP registry lists ~41 agents.
- **JetBrains:** ACP registry live since Jan 28, 2026. Each agent reads its own instruction file.
- **VS Code 1.129 (Jul 2026):** Agent Host and Agents window. Claude runs on the Agent SDK (Copilot billing or own key); Codex runs on Copilot Pro+ or a ChatGPT subscription. A handoff to another agent carries the conversation history. Hooks aren't shared across harnesses.
- **GitHub Agent HQ** (Claude and Codex added Feb 9, 2026): dashboard for Claude, Codex and Copilot cloud agents. Third-party agents need Copilot Pro+ ($39/mo) and bill through GitHub.
- **Warp:** runs 16 CLI agents in panes. Plans: Free / Build $20 / Max $200 / Business $50. "Factories" runs Claude Code and Codex as harnesses (early access). Client open-sourced May 2026.
- **Cursor:** no evidence it hosts the Claude Code or Codex harnesses. It automatically loads CLAUDE.md, `.claude/{skills,agents,commands}`, `.codex/skills` and Claude hooks.

**Single harnesses that work with several providers**
- **Pi:** `/login` for ChatGPT, Copilot and Claude Pro/Max. Claude OAuth use is billed as extra usage. Ctrl+L switches model; sessions are stored as trees. MIT, ~113.7k★, 1.1.0 (Oct 7, 2026).
- **OpenCode:** OAuth for ChatGPT Plus/Pro, Copilot, GitLab Duo, SuperGrok. Claude Pro/Max support removed in v1.3.0 (PR #18186, Mar 19, 2026) after Anthropic legal requests. MIT, ~212k★.
- **OpenClaw:** round-robin rotation across accounts, cooldowns, cross-provider fallback that keeps the transcript. MIT, ~391.5k★.
- **Crush:** switches LLM mid-session and keeps context; API keys or Charm Hyper subscription. FSL-1.1-MIT, ~27.9k★.
- **Kilo Code:** OAuth for ChatGPT Plus/Pro and SuperGrok. Free / Teams $15 / Kilo Pass $19, $49, $199.
- **Factory Droid:** API keys documented. Pro $20 / Plus $100 / Max $200.
- **Letta Code:** memory-first; `/model` switches mid-conversation; ChatGPT Plus/Pro and Copilot subscriptions. Free / Pro $20.
- **Coder Mux (→ Xum):** its own agent loop; doesn't host other harnesses. AGPL-3.0, ~2k★.

---

### 5.2 Config, rules, skills, plugins, MCP portability (F4)

**Standards (as of Oct 2026)**
- **AGENTS.md** (Agentic AI Foundation / Linux Foundation): agents.md lists 23 tools reading it natively, including Codex, Jules, Factory, Aider, goose, opencode, Zed, Warp, VS Code, Devin, Junie, Amp, Cursor, RooCode, Gemini CLI, Kilo, Copilot coding agent, Windsurf and Augment. Claude Code is not on the list.
- **Claude Code** v2.1.277 (2026-09-18): reads AGENTS.md only when a folder has no CLAUDE.md; changeable under "Project instructions" in `/config`; not available on Bedrock, Vertex or Foundry.
- **Study of 283 top repos with AGENTS.md:** 161 (57%) also have a CLAUDE.md.
- **Claude Code issue #6235** ("Support AGENTS.md"): ~5,146–6,686 reactions, 394–409 comments; closed Aug 2026. The HN thread on the release: 740 pts / 286 comments.
- **Gemini CLI** reads GEMINI.md by default; AGENTS.md only if added to `context.fileName`.
- **Codex** reads AGENTS.override.md, then AGENTS.md, then `project_doc_fallback_filenames`; 32 KiB cap.
- **OpenCode** prefers AGENTS.md and falls back to CLAUDE.md; also reads `~/.claude/CLAUDE.md` and `.claude/skills`. **Pi** reads AGENTS.md or CLAUDE.md.
- **Agent Skills (SKILL.md, agentskills.io):** ~45 listed clients, including Claude Code, Codex, Gemini CLI, Cursor, Copilot/VS Code, OpenCode, Pi, OpenClaw, Goose, Amp, Kiro, Junie, Factory and Roo.
  - Codex, Gemini CLI, Cursor, Copilot, OpenCode, Amp, Cline and Pi read `.agents/skills/`. Claude Code documents `.claude/skills` (plus plugin and claude.ai-sync locations).
  - Claude Code issue #31005 (`.agents/skills` support): 546r, open.
  - The spec allows `name`, `description`, `license`, `compatibility`, `metadata`, `allowed-tools`. Claude Code adds `context: fork`, `agent`, `hooks`, `model`, `paths`, `disable-model-invocation`.
- **Plugins:**
  - Claude Code `.claude-plugin/plugin.json` (skills, agents, hooks with 30+ events, MCP, LSP).
  - Codex `.codex-plugin/plugin.json` with a marketplace (skills, MCP, hooks, browser extensions, app connectors).
  - Cursor 3.9 `.cursor-plugin/plugin.json` (skills, rules, MCP, commands, hooks).
  - **Agent Plugins 1.0** (2026-08-06; Amazon, Cursor, Microsoft, OpenAI, Vercel, Google joining): skills and `mcp.json` portable; hooks, commands, agents, rules, permissions and distribution out of scope. Anthropic is not on the steering committee; Claude Code and Gemini CLI are not listed as clients.
  - VS Code auto-detects four manifest formats.
  - Dual-publishing a Claude plugin to Codex: a second marketplace file and manifest; `hooks/hooks.json` shared; `CLAUDE_PLUGIN_ROOT` passed through; `CLAUDE_PROJECT_DIR` missing in Codex.
- **Hooks:** Codex has 5 events (SessionStart, UserPromptSubmit, Pre/PostToolUse, Stop), with tool events firing only for Bash. Gemini CLI has about a dozen events. OpenCode uses JS modules; Pi uses TypeScript extension events.
- **Subagents:** Claude and Gemini use Markdown + YAML frontmatter (`.claude/agents`, `.gemini/agents`); Codex uses TOML in `.codex/agents/` with `developer_instructions`.
- **MCP config schemas:** JSON `mcpServers` (Claude, Cursor, Gemini); TOML `[mcp_servers]` (Codex); `opencode.json`; no native MCP in Pi. Gateways: Docker MCP Toolkit (beta, Docker Desktop 4.62+, one-click connect for Claude Code, Codex, Gemini, Cursor, OpenCode, Goose, Zed); MetaMCP (MIT, ~2.7k★, remote endpoints only).
- **ACP** standardizes the editor↔agent connection. The client can forward MCP servers; skills, rules and permissions stay inside each agent.

**Sync and generation tools**

| Tool | What it syncs | Targets | License / traction |
|---|---|---|---|
| rulesync | rules, MCP, commands, subagents, skills, hooks, permissions, plugin packaging | 63 | MIT, ~1.5k★, ~1.1M npm downloads/mo, v17.0.0 (Sep 21) |
| ruler | rules, MCP, skills (experimental), subagents (experimental) | ~31 | MIT, ~2.9k★, ~298k downloads/mo, v0.3.44 |
| CC Switch | provider switching (90+ presets), format-converting local proxy with failover queue, MCP sync, skills manager, prompt library (CLAUDE.md / AGENTS.md / GEMINI.md), usage dashboard, beta multi-account OAuth (ChatGPT, Copilot, xAI), WebDAV/S3 sync of its database | Claude Code, Claude Desktop, Codex, Gemini CLI, Grok Build, OpenCode, OpenClaw, Hermes, Pi, MiniMax | MIT Tauri app; ~104k–141k★ reported by different pages; v3.20.1 (Aug 28) |
| Vercel skills CLI (`npx skills`) | skills (symlinks) | ~79 agents | MIT, ~33.4k★; telemetry on by default |
| openskills | skills injected into AGENTS.md | AGENTS.md readers | Apache-2.0, ~10.7k★ |
| CCS | account/provider profiles, OAuth providers, usage analytics | Claude Code, Codex, Droid | MIT, ~2.8k★ |
| skills-manager | skills library, presets, git backup | 53 | MIT, ~4.3k★ |
| amtiYo/agents | MCP, skills, instructions (`.agents/`) | 11 | Apache-2.0, 85★ |
| OpenAI migrate-to-codex skill | one-way Claude → Codex: CLAUDE.md, commands, skills, subagents → TOML, MCP, supported hooks | — | Official; plugins need manual migration; unsupported hooks dropped |

---

### 5.3 Session handoff and shared memory (F2, F3, F9)

**Session transfer tools**

| Tool | Mechanism | Coverage | Status |
|---|---|---|---|
| Codex `/import` | Converts Claude Code JSONL into Codex rollouts; continues with prior context compacted. v0.140 (2026-06-15); v0.145 added Cursor, MCP servers, plugins, sessions, commands and project-scoped memories ("Add external agent memory migration") | Claude Code / Cursor → Codex | Built in; one way; 50 most recent sessions from the last 30 days |
| continues (cli-continues) | Parses the source session; injects a structured handoff document into the target | 16 tools, 240 source–target pairs | MIT, ~1.6k★; presets keep 3/10/20/50 messages |
| casr | Canonical intermediate format; writes provider-native session files | ~15 providers | 119★; only Grok round-trip verified |
| codex-claude-transfer (cct) | Neutral intermediate format, both directions | Claude Code ↔ Codex | MIT, v1.9 (Aug 2026); tool calls and runtime state don't carry over |
| authsec-bridge | Writes native sessions or a HANDOFF.md fallback | Claude Code, Codex, Gemini | OSS, ~600 lines |
| claudex | Detects exhausted quota; reruns the prompt in Codex | Claude → Codex | 0★; last exchange + repo snapshot only |
| Amp `/handoff` | Goal-driven extraction of a draft prompt + file list into a new thread; replaced compaction Oct 2025 | Amp | Proprietary |
| Pi | Serializes context as JSON; continues with any provider | Within Pi | Anthropic thinking becomes tagged text; signed blobs can't cross providers |
| claude-handoff | `/handoff` with a 12-item checklist + git state | Claude Code | 59★ |
| cass | Local search across 26 agents; `cass pack` builds token-budgeted answer packs | 26 | MIT, ~1.1k★ |
| agent-sessions | macOS session and quota browser | 14 | MIT, ~850★ |
| SpecStory | `specstory run <agent>` saves markdown to `.specstory/history/` | Claude Code, Codex, Gemini, Pi, Droid, others | ~1.3k★; capture free; Pro $25/mo adds resume features; Team $300/mo |

**Session storage formats**
- **Claude Code:** `~/.claude/projects/<project>/<id>.jsonl`. The docs say the format is "internal… changes between versions". Default retention 30 days. `claude --resume <transcript-path>` accepts a file path.
- **Codex:** `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` with a SQLite index since v0.134.
- **Gemini CLI:** `~/.gemini/tmp/<hash>/chats/`, 30-day retention. Replaced by Antigravity CLI for free and Google One users on 2026-06-18.
- **ATIF v1.7 (Harbor):** trajectory interchange format. `atif-make` converts Claude Code, Codex, Copilot and Cursor logs. Harbor can load an ATIF trajectory into claude-code or codex by converting it to native format (drops system messages and non-text content).
- **OpenTelemetry GenAI semantic conventions:** Development status; message-content capture is opt-in.
- **ACP `session/load`:** the agent replays its own history; no portable history format.

**Shared memory tools**

| Tool | Harnesses | Notes | Price |
|---|---|---|---|
| claude-mem | Claude Code, Codex, OpenCode, Gemini, OpenClaw, Pi, others | ~97k★, Apache-2.0. `CLAUDE_MEM_SESSION_START_INCLUDE_ALL_SOURCES=false` by default | Free; hosted observer 30-day trial |
| Basic Memory | Any MCP client | Markdown source of truth; AGPL; two-way rclone sync in cloud | Free self-host; $15/seat Team, $30/seat Business |
| mem0 / OpenMemory | Codex plugin/MCP, Claude Code | Cloud API key | Hobby free / Starter $19 / Pro $249 |
| Supermemory | Claude Code, Cursor, OpenCode, Codex (one command, 2026-06-20) | Shared store across agents unverified | Free / $19 / $100 / $399 |
| Letta Code | Own harness | Git-backed markdown memory (MemFS); ~3.5k★ | Free / Pro $20 |
| ByteRover CLI | 22+ agents | ~5.0k★, Elastic-2.0; repo archived 2026-09-27 | — |
| Serena | Any MCP client | Project-local memories; MIT, ~30k★ | Free |
| Zep / Graphiti | Via MCP | General agent memory | Free tier 10k credits; Flex $125/mo |
| Memorix | ~17 agents incl. Claude Code, Codex, Pi | Shared memory MCP with locks and messages | — |

- Codex memories live in `~/.codex/memories/`, off by default.
- Codex 0.145 imports Claude Code project-scoped memories with provenance.

**Measured evidence on handoff**
- **Handoff Debt** (arXiv 2606.02875): 181 handoff points from SWE-bench Verified, open-weight models only.
  - Full predecessor transcript: 57–59% fewer successor steps and +6.1 to +14.9 points solve rate.
  - Free-text or structured notes: 20–46% fewer steps; solve-rate gain significant only for Devstral.
  - Transcript prompts ~87k characters vs notes 7–10k.
- **handover.sh pilot:** structured handoff 79.45, raw transcript 76.67, compressed memory 45.0 (3 cases; vendor-authored benchmark).
- **Factory compression evaluation** (36k messages): Factory 3.70, Anthropic 3.44, OpenAI 3.35. The "artifact trail" (files touched) scored 2.19–2.45 for all three.
- **Chroma "context rot":** accuracy fell with input length across all 18 models tested; on LongMemEval a focused ~300-token prompt outperformed the full ~113k-token one.
- **Prompt caching:** Anthropic cache reads cost 0.05–0.1× input; default TTL 5 minutes. Claude Code offers "resume from summary" for sessions over 100k tokens idle more than an hour.
- **Anthropic "effective harnesses for long-running agents"** (Nov 2025): uses `claude-progress.txt`, a JSON feature list, git commits and `init.sh`; states "compaction isn't sufficient".

---

### 5.4 Multiple accounts and subscriptions (F5)

| Tool | Switch type | Mechanism | ★ / release |
|---|---|---|---|
| claude-swap | `cswap auto` polls every 60s; at 90% switches to the account with most quota left | Swaps stored credentials; official binary; on Linux applies "on your next message" | ~1.8k★ |
| ccswap (PyPI) | `ccswap auto` polls every 60s | Official binary; Claude Code + Codex | v0.35.1 (2026-09-25) |
| CCS | Native profiles manual; Account Pools auto-fail over | Profiles: separate `CLAUDE_CONFIG_DIR`. Pools: tokens cached in `~/.ccs/cliproxy/auth/`, `ANTHROPIC_BASE_URL=http://127.0.0.1:8317` | ~2.8k★, v8.10.0 (2026-09-11) |
| CLIProxyAPI | Round-robin across accounts | Token proxy over OAuth logins (Claude Code, Codex, Antigravity, Grok) | ~51.1k★, v7.2.147 |
| CC Switch | Auto-failover across providers with a circuit breaker; beta subscription login | README: "Using a subscription outside the official client may violate the vendor's terms of service"; "Official providers (e.g. Claude Official) can't go through local routing" | ~100k+★ |
| better-ccflare | "Automatic failover between accounts" | Token proxy | 271★ |
| maxpool | On 429 parks the account and retries another | Token proxy; README: "contested gray area… People have lost accounts to pattern detection" | — |
| AIClient-2-API | Multi-account polling, failover on 429 | Token proxy; markets "Bypass Official Restrictions" | ~8.9k★ |
| claude-code-router | Credential pools, key rotation, ordered fallback | Proxy, mainly API keys | ~37.3k★, v3.1.0 |
| swapdex / codex-switcher | Manual only | Config snapshots | codex-switcher 637★ |
| Claude Code `CLAUDE_CONFIG_DIR` | Manual profiles | Native env var; a maintainer pointed to it + aliases in #20131 | Native |

Vendor-side overflow options:
- **Claude:** usage credits at standard API rates with a monthly cap and auto-reload; Max 20x upgrade; Team premium seats $100–125/seat/mo.
- **Codex:** purchasable credits; Pro plans have no 5-hour limit.
- **API-key fallback:** OpenRouter `models` array falls back on rate-limiting and downtime; LiteLLM `fallbacks` cover `RateLimitError`.

---

### 5.5 Pause and resume at usage limits (F6)

**Native behavior**

**Claude Code CLI** (v2.1.234+, default on; setting `autoContinueAtUsageLimit`)
- "Usage limit reached · continuing automatically at 3:45pm". At reset it sends a fixed prompt to pick up the task; it doesn't resend the last message.
- Re-arms at most twice in a row, then stops with "Automatic continue stopped after repeated usage-limit hits".
- Doesn't start the wait automatically for resets more than 24h away (weekly limits), Remote Control or agent-team teammate sessions; "Wait here" is available from `/rate-limit-options`.
- Not offered for background sessions, `-p` runs or API-key billing.
- The wait ends without continuing on exit, `/login`, `/clear`, `/resume`, `/teleport` or handoff.
- Claude Code Desktop: "Auto-continue when limits reset" checkbox on the session-limit card.

**Claude Code: other limit behavior**
- **Usage credits:** `/usage-credits` continues past the limit; cache lifetime drops from 1 hour to 5 minutes while drawing on credits.
- **Subagents:** end with "Agent terminated early due to an API error"; background ones are marked failed. Resumable with `SendMessage` to the agent ID (transcript at `~/.claude/projects/<project>/<sessionId>/subagents/agent-<id>.jsonl`). Built-in Explore/Plan agents return no ID.
- **`--resume` / `--continue`:** restores the conversation and scheduled tasks; background Bash and monitor tasks are not restored. Dead background work shows as a "didn't finish" note. In-flight tool calls are marked as cut off.
- **Dynamic workflows:** the v2.1.271 changelog says they "pause when you hit your usage limit and continue automatically when it resets". Issue #98299 (v2.1.281) reports 26 in-flight workflow agents failed.
- **Cloud sessions:** share the account's limits; on VM expiry, background subagents and shell commands are not restored.
- **Observed during this research (2026-10-09):** a research subagent ended with "Agent terminated early due to an API error: You've hit your session limit · resets 5:40am".

**Other harnesses**
- **Codex CLI:** the turn fails and shows the reset time; no auto-resume. Requests #21073 (72r, ~11c, no OpenAI reply), #28931 (42r), #34188, #48392. Codex Goals enter a paused `usageLimited` state requiring a manual message. Subagents can be orphaned (#19197).
- **Codex limits:** 5-hour window removed Jul 12 for Plus, Pro, Business; restored for Plus Aug 25; Pro has no 5-hour cap; weekly limits apply to all. "Saved resets" can be banked.
- **Gemini CLI:** prompts to switch to a fallback model; no wait-for-reset.
- **OpenCode:** rate-limit responses sent as HTTP 400 are treated as terminal (#40241).
- **Pi:** built-in 429 retry; the `pi-retry` extension (~3k downloads/mo) deliberately does not retry quota errors.

**Claude Code issues**
- #98299: limit kills in-flight workflow agents (open).
- #94222: mark limit-killed subagents resumable and keep them listed after restart (open, filed High priority). A commenter reports 449 subagents cut off, 8 resumed, 438 re-dispatched. The reset time is in the transcript as `quotaLimits.resetsAt` on the `error: "rate_limit"` entry. In one comparison, a fresh re-run found 4 issues for ~205k tokens; a resumed run found 10 for ~166k.
- #89263: auto-continue never arms when only background subagents are killed.
- #74162: background agent should pause, not fail.
- #80037, #55928: closed not planned.
- #5967: context loss after reset (closed not planned).
- #13354 (215r), #35744 (106r), #3138 (45r).

**Third-party tools**

| Tool | Mechanism | ★ |
|---|---|---|
| claude-auto-resume | Probes `claude -p 'check'`, parses the reset time, sleeps, runs `claude -c --dangerously-skip-permissions -p "<prompt>"` | 817 |
| autoclaude | Polls tmux panes every 3s, sends Esc / "continue" / Enter | 47 |
| claude-auto-continue | PTY wrapper with regex detection | 0 |
| TaskWake | `StopFailure` hook, detached waiter, `claude --resume <id>` | 0 |
| Kurrent Capacitor | Sessions as event logs; another agent recaps and resumes | vendor |
| claude-rotate | Proxy rotating between own Claude accounts (maintainer: "terms gray area") | small |

**Durable-execution projects**
- Temporal + OpenAI Agents SDK: model and tool calls as Activities; local shell and computer tools unsupported.
- Temporal Agent Harness ("earlier than public preview", Aug 20): Gemini, OpenAI Agents SDK, PydanticAI; doesn't wrap Claude Code or Codex.
- Restate + Modal: custom-loop durable coding agent.
- DBOS, LangGraph checkpointers.
- Claude Agent SDK `SessionStore`: mirrors main and subagent transcripts to S3, Redis or Postgres.

---
### 5.6 Sessions, worktrees, sandboxes, "Kubernetes for agents" (F7, F8)

**Native capabilities**

**Claude Code CLI**
- **Starting in a worktree:**
  - `claude --worktree <name>` / `-w` creates `.claude/worktrees/<name>/` on branch `worktree-<name>`; a name is generated if omitted.
  - `EnterWorktree` tool mid-session; subagents can use `isolation: worktree` in frontmatter.
  - `.worktreeinclude` (gitignore syntax) copies gitignored files such as `.env` into new worktrees.
  - `worktree.baseRef`: `"fresh"` (default) or `"head"`. `--worktree "#1234"` branches from a PR or MR.
- **Cleanup:**
  - On exit, a clean unnamed worktree and its branch are removed automatically; a named one prompts.
  - A worktree with work prompts keep/remove.
  - `-p` runs are not cleaned.
  - Periodic sweep of subagent and background-session worktrees per `cleanupPeriodDays`.
- **Shared with the main checkout:** `.git`, project-scope plugins, permission approvals (v2.1.211+), untracked skills, agents and commands.
- **Isolation enforcement:** edits to the main checkout and git redirects into it are blocked.

**Other Claude Code surfaces**
- **Background / agent view** (research preview): `claude --bg` sessions move into their own worktree before editing; deleting a session removes its worktree; `claude rm` keeps uncommitted work.
- **Desktop:** worktree option per session; archive removes it; "Auto-archive after PR merge or close"; Claude can list, read, message and archive other sessions.
- **Cloud:** one isolated VM per session; `claude --cloud`; inactive VMs reclaimed.
- **Cross-session messaging** (v2.1.224+): `ListAgents` / `SendMessage` between Claude sessions on the same machine, other machines or the cloud.
- **Agent teams** (experimental): shared task list + messaging; "Agent teams don't isolate teammates in worktrees."
- **Sandbox:** `/sandbox` (off by default) uses Seatbelt on macOS and bubblewrap on Linux (`@anthropic-ai/sandbox-runtime`); covers shell commands.

**Codex**
- **Codex app:** opt-in worktree per thread in `$CODEX_HOME/worktrees`; keeps the 15 most recent; deletes on archive after a snapshot; restorable.
- **Codex CLI:** no `--worktree`; request #12862 (129r) unmerged.
- **Sandbox modes:** `read-only` / `workspace-write` (default) / `danger-full-access`.
- **Codex cloud:** one VM per task; state kept 7 days.

**Others**
- **Cursor:** Agents Window, `/worktree`, `/best-of-n`; `worktreeMaxCount` default 25.
- **Gemini CLI:** `--worktree` experimental since v0.36.
- **Antigravity CLI:** `/fork` and background subagents; worktrees not documented.

**Third-party session and worktree managers:** see 5.1. Additional documented details:
- **Superset:** automatic per task; orphan worktree folders after deleting a workspace (#2863, 10r).
- **Conductor:** archive saves git state including uncommitted files.
- **Vibe Kanban:** worktree per attempt, "automatically cleaned up"; #765 (19r/14c) reports 26 GB in two days.
- **Claude Squad:** #260 asks for a setup hook for deps/.env/ports (8r).
- **Traycer:** v1.4 optional automatic cleanup of merged worktrees.
- **Agent Deck:** `worktree finish` / `worktree cleanup`.
- **Omnigent:** worktrees in its "Polly" example orchestrator.

**Sandbox and environment layers**
- **Docker Sandboxes (`sbx`):** microVMs, launched Mar 2026. Runs 11 agents (Claude Code, Codex, Gemini, OpenCode, Cursor and others). Local free, cloud pay-as-you-go. No automatic worktree or branch; `--rm` / `prune` for cleanup.
- **Docker Agent "harnesses":** claude-code, codex, opencode, pi with `--worktree`; not sandboxed by default.
- **Dagger container-use:** MCP server; container + git branch per agent; ~4k★, early development.
- **VibeKit:** Docker isolation + secret redaction for Claude Code, Gemini, Codex, OpenCode, Grok.
- **Sculptor:** container per agent; two-way pairing sync; merge-conflict flagging.
- **Rivet Sandbox Agent:** one HTTP API over Claude Code, Codex, OpenCode, Cursor, Amp, Pi on E2B, Daytona, Vercel, Docker or Modal.
- **Coder Tasks:** workspace per task; removed in v2.37 (2026-09-01), replaced by Coder Agents (Coder's own agent).
- **Ona (ex-Gitpod):** acquired by OpenAI (announced 2026-06-11).
- **Compute pricing:** E2B and Daytona ~$0.05/vCPU-hr; Modal ~$0.071/vCPU-hr; E2B Pro $150/mo.

**"Kubernetes for agents" projects**
- **kubernetes-sigs/agent-sandbox:** SIG Apps project with Sandbox, SandboxTemplate, SandboxClaim and SandboxWarmPool resources (v1beta1), gVisor or Kata; ~3.8k★.
- **kagent:** CNCF Sandbox project for ops agents; its AgentHarness resource provisions OpenClaw, NemoClaw or Hermes sandboxes.
- **Kelos:** "Run and orchestrate coding agents on Kubernetes"; runs Claude Code, Codex, Gemini, OpenCode and Cursor as Pods and Jobs.
- **Plural Agent Runtime** (Feb 2026), **Stacklok Mecatl** (Sep 2026).
- **Omnigent:** "meta-harness"; Kubernetes listed as a sandbox provider.

**Documented problems with parallel worktrees (Upsun write-up and issues)**
- Port clashes (3000, 5432, 8080).
- Shared database, Docker daemon and caches.
- `node_modules` reinstalled per worktree.
- `.env` not carried over.
- Merge conflicts between worktrees.
- Cursor forum: 9.82 GB in 20 minutes and ~140 GB in a week.

---

### 5.7 Cross-machine and cloud portability (F10)

**Claude Code**
- **Syncs down from the claude.ai account:**
  - Skills enabled on claude.ai land in `~/.claude/skills/synced/` (download-only, v2.1.273+).
  - Plugins load as `<name>@synced`.
  - claude.ai connectors and org server-managed settings.
- **Cloud sessions:**
  - `claude --cloud` starts one.
  - `--teleport` pulls a cloud session down one way: same repo, pushed branch, clean tree, same account.
  - `claude -p --cloud <id>` sends a follow-up from any logged-in machine.
  - Cloud sessions don't get the user `~/.claude/CLAUDE.md`, user skills/agents/commands, user-scope plugins, or MCP servers from `~/.claude.json`.
- **Remote Control:** steers a local process, which must keep running.
- **Local-only:** transcripts; auto memory ("not shared across machines or cloud environments"); `~/.claude.json` (OAuth state, personal MCP, per-project trust); `settings.json`, `history.jsonl`, `file-history/`; credentials (macOS Keychain or `.credentials.json`).
- **Feature request #66303** (sync `~/.claude` through the account): closed not planned, Jul 15, 2026.

**Codex**
- **Cloud tasks:** `codex apply <task>` applies the diff only.
- **ChatGPT-app relay** (May 14, 2026): "Files, credentials, permissions, and local setup stay on the machine where Codex is operating." Remote SSH is GA; `--remote` attaches to a remote app-server.
- **Local-only:** `~/.codex/auth.json` or OS keyring; memories in `~/.codex/memories/` (no cloud sync for CLI memories); sessions.
- **Cloud config bundles:** Business/Enterprise/EDU only.

**Gemini / Antigravity:** no cross-device sync documented. **OpenCode:** `opencode export/import` moves sessions as JSON.

**Third-party portability tools**
- **CC Switch:** WebDAV/S3 sync of its database (providers, MCP, prompts, skills); "settings.json, the device state, and the original config files… are not included in cloud sync."
- **claude-nomad:** private git repo, `path-map.json` rewrites transcript paths, gitleaks scan, "credentials never sync"; hooks and agents not synced.
- **claude-hop** v0.4.0: rsync over SSH, rewrites paths, refuses to run while Claude is active.
- **clync:** age-encrypted git; merges sessions by UUID tree.
- **entangle:** magic-wormhole transfer for Claude, Codex, opencode, with path rewriting.
- **cct:** `--map-cwd` when paths differ.
- **claude-sync:** git, Dropbox or Syncthing backends with age encryption (28★).
- **Basic Memory Cloud:** $15/mo, two-way sync. **claude-mem:** cloud backup to cmem.ai. **SpecStory Cloud Pro:** $25/mo.
- **cass:** multi-machine search over rsync/SSH.
- **Traycer Sync:** syncs tasks and history; terminal-agent transcripts stay on the host machine.
- **Omnigent:** sessions follow you across devices; reuses each host's CLI logins.
- **Warp Drive:** syncs rules, MCP, prompts for Warp's own agents.
- **Nix:** home-manager `programs.claude-code` module.
- **Tailscale + tmux/mosh:** one always-on remote box.

**Environment portability**
- **Devcontainers / Codespaces:** home wiped on rebuild. Documented fix: named volume + `CLAUDE_CONFIG_DIR`; auth via `CLAUDE_CODE_OAUTH_TOKEN` secret (one-year token, model requests only).
- **OpenShift Dev Spaces:** PVC or GitOps copy of `~/.claude`.
- **Coder Agents:** chat state survives rebuilds (Coder's own agent).

**Technical constraints**
- **Version skew:** sessions created in Claude Code 2.1.149 failed to load after 2.1.170 (#68489, closed not planned).
- **Paths:** project directory names, `cwd` fields and `.claude.json` keys hold absolute paths.
  - Native mitigations: `--resume <id>` searches all projects (v2.1.223+); resumes in the current directory if the original is missing; `--resume <transcript-path>`; `CLAUDE_CODE_PROJECT_DIR_NAME` (v2.1.234+).
  - Codex needs matching cwd or `--map-cwd`. Gemini keys sessions by path hash.
- **Concurrency:** resuming one session in two places interleaves both into one transcript.

---

### 5.8 Usage monitoring and analytics (F11)

| Tool | Coverage | Per-project | Live quota | Multi-account | License / traction |
|---|---|---|---|---|---|
| codeburn (AgentSeal) | 37–42 tools | By project, model, task, git branch | `codeburn quota` (5h + weekly) | Multiple machines | MIT, ~11.4k★; `optimize` finds re-read files, unused MCP, oversized CLAUDE.md; one-shot edit rate; spend matched to PRs |
| ccusage | 18 sources (Claude Code, Codex, OpenCode, Amp, pi-agent, OpenClaw, Gemini CLI, Antigravity, Copilot CLI…) | `--instances` | 5h blocks (Claude) | Comma-separated profile dirs | MIT, ~18.9k★, ~600k npm downloads/mo |
| CodexBar | 69 providers | — | Session/weekly/monthly windows with reset countdowns | Unclear | MIT, ~20.8k★; macOS menu bar + Linux CLI |
| tokscale | Dozens | Projects tab | Usage view | Some clients | MIT, ~5.5k★; public leaderboard |
| Claude Code Usage Monitor | Claude Code | — | Statusline `rate_limits` + burn-rate forecast | `--data-paths` / `CLAUDE_CONFIG_DIR` | MIT, ~8.6k★ |
| agent-sessions | 14+ | Analytics view | Codex, Claude | — | MIT, ~850★, macOS |
| agentsview (kenn-io) | 20+ | Session stats + opt-in git/PR outcomes | — | — | MIT, ~6.1k★ |
| Splitrail | 14 tools | — | Claimed | Multiple machines via cloud | MIT, 223★ |
| TokenTelemetry | 12+ | Trace replay, skill and MCP views | — | — | Free OSS (~Aug 2026) |

**Native analytics**
- **Claude Code `/usage`** (`/cost` and `/stats` are aliases): session cost at list price; prompt-cache misses with likely cause; on subscriptions, usage split by skills, subagents, plugins and MCP servers; flags behaviors taking 10%+ of usage; heaviest `/loop` tasks. This machine only.
- **Claude Code `/insights`:** model-written HTML report on friction points and suggestions across up to 200 new sessions; costs plan tokens; this machine only.
- **Claude Code OpenTelemetry:** metrics (cost, tokens, lines changed, PRs, commits, active time) and events (prompts, tool results, API errors, skill activation). Content redacted unless opted in (e.g. `OTEL_LOG_USER_PROMPTS`). Traces in beta.
- **Claude Code Team/Enterprise dashboard:** daily active users, sessions, accept rate, PRs and lines.
- **Codex:** per-chat credits and "Top consuming chats" (Enterprise/Edu); a consumer dashboard spotted in testing 2026-09-04; OTel via an `[otel]` config block.
- **Cursor:** AI share of committed code, per-repo AI percentage, "Conversation insights".
- **AWS CloudWatch Coding Agent Insights** (Jul 2026): Claude Code, Codex, Copilot via OTel; raw tokens, no cost.

**Session-quality and observability tools**
- **retro-agent:** churn, retries, flail rate, error clusters for Claude Code, Codex, Cursor.
- **Rudel:** abandonment and completion; the makers' data shows 26% of sessions abandoned in under 60 seconds and skills used in 4%.
- **Langfuse:** Claude Code, Codex, Copilot, Cursor, OpenCode; Hobby free / Core $29 / Pro $199; documentation says it doesn't evaluate outcomes.
- **Arize harness-tracing**, **LangSmith** (Claude Code plugin + Codex), **LiteLLM Lens**, **Coralogix Code Agents Intelligence**, **Jellyfish**, **DX**.
- **Helicone:** maintenance mode after joining Mintlify.
- **git-ai:** records agent, model and prompt per line in git notes; Apache-2.0, ~2.8k★.

**Reasoning data available in logs**
- **Claude:** on Opus 4.7+, Opus 5/5.5, Sonnet 5/5.5 and Haiku 5.5, thinking returns omitted by default (empty field + encrypted signature): "No `display` setting returns the raw chain of thought." Issue #49807 reports thinking text missing from Claude Code JSONL from ~v2.1.112.
- **Codex:** reasoning stored as `encrypted_content`. "Detailed" summaries returned as headings only since ~Jul 2026 (#34873, open). MultiAgentV2 (Jun 2026) encrypts task text sent to subagents.

---
### 5.9 Native macOS UI picker for agents (F12)

**Web element-pick tools**
- React Grab: 7.6k★; hover, ⌘C copies the element + component stack with file:line.
- Agentation: 4.5k★, PolyForm Shield; click elements, add notes, markdown output with selectors; MCP folder.
- Stagewise: 6.8k★; now an agentic IDE/browser; free / $20 / $200 per month.
- Onlook: 26.9k★; Next.js + Tailwind visual editor.
- Cursor 3.0 Design Mode (2026-04-02): ⌘⇧D, Shift-drag area select, ⌘L adds the element to chat; browser only.
- Claude Code Desktop preview: select elements; Claude receives tag, classes, computed styles, cropped screenshot, React file and props.
- ChatGPT/Codex built-in browser: Annotate mode (click element or drag area, comment, "Adjust" styles).
- Jam: Free (MCP included) / Team $14 per creator/mo; Jam for Mac records screen/window/area without element data.
- BrowserTools MCP: 7.3k★, `getSelectedElement`; README warns of a critical vulnerability in 1.2.x.
- Chrome DevTools MCP: 52.9k★.
- Marker.io: MCP free for customers.
- BugHerd: from ~$42/user/mo (sources conflict); no MCP found.

**Native: agent-side capture**
- Claude Code computer use (CLI on macOS + Desktop): described for "Build and validate native apps" and "Debug visual and layout issues"; needs Accessibility + Screen Recording.
- Claude Desktop iOS Simulator pane (2026-07-21): tap and swipe; no element select or annotation; not yet on Xcode 27.
- Codex Appshots (2026-05-21): pressing both ⌘ keys attaches the frontmost window image + its accessibility text; whole window only.
- Codex computer use: controls native Mac apps.
- Peekaboo: 5.3k★, v4.9.3, macOS 15+. `see --json` returns AX elements (role, label, identifier, bounds) and an annotated screenshot; MCP.
- XcodeBuildMCP: 6.4k★; UI automation documented for the simulator.
- AXe: 2.2k★; iOS Simulator only.
- peek: 5★; macOS MCP with x/y hit-testing.
- macos-automator-mcp: 878★; AppleScript/JXA.
- Xcode 26.3 + Claude Agent SDK (2026-02-03): captures Xcode Previews.
- Xcode 27 MCP bridge: 20 tools; `RenderPreview` is the visual one.
- Reveal 58 (2026-08-07): beta MCP; iOS/iPadOS/tvOS inspection described.
- InjectionIII: 4.6k★; requires the app sandbox off on macOS.

**Native: human-side pickers**

| Tool | Capability | Output | ★ |
|---|---|---|---|
| Loupe | Hover any Mac app (macOS 14+, Accessibility permission), click, describe the change; roles, hierarchy paths, search patterns | Copy button (text); no screenshots; MCP not mentioned | 107, MIT, v1.1.0 (2026-02-04) |
| Remarc | Text selection + annotated screenshot regions (native AX capture not described; Chrome extension captures element data) | MCP to Claude Code, Codex, Cursor | 8 |
| Deiko | Double-tap Right Option, point, speak; reads AX under the cursor + screenshot | Agent input | Free OSS; Pro $6.99/mo |
| Show & Tell (snapback) | Front-window capture, pins/boxes with notes | Paste into Claude/Codex | 0 |
| Notate | Freezes menus and hover states | Markdown, no element data | MIT |
| swift-agentation | In-app SDK, iOS 17+; AX label, `#id`, path, frame | — | 62 |
| Crit | iOS Simulator screenshots with pins | `.crit/` folder | — |
| AppContext | iOS Simulator + web | — | $9.99/mo |

**Platform facts**
- `AXUIElementCopyElementAtPosition` returns the accessibility element at a screen point.
- Permissions: Accessibility + Screen Recording. Since Sequoia, the Screen Recording re-prompt is monthly.
- Apple DTS: a sandboxed app can't use `AXUIElementCreateApplication` on other apps.
- Accessibility exposure varies: apps aren't required to implement it. SwiftUI identifiers have been missed by tooling (Appium Mac2 driver, 2022). Electron's Chromium tree turns on only with assistive tech present or `AXManualAccessibility` set.
- No SwiftUI runtime file/line source map was found; `_viewDebug` exports debug data without file/line.
- Claude Code docs contrast computer use (real desktop) with the sandboxed Bash tool. A full-VM sandbox policy disables the iOS Simulator pane.

**Demand signals**
- HN: Deiko ("tired of taking screenshots") 5 pts / 2 comments; Crit 5 pts; Remarc 1 pt (posted twice); agent-desktop (agent-side AX automation) 99 pts / 44 comments.
- Indragie Karunaratne (Jul 2025, building a macOS app with Claude Code): pasted screenshots by hand; knew of no native equivalent of playwright-mcp.
- No anthropics/claude-code or openai/codex issue found asking for a native element picker; closest is #91521 (web Design Mode, 0 reactions).

---

### 5.10 Test integrity of agent-written tests (F13)

**Lab measurements**
- **ImpossibleBench** (Zhong, Raghunathan, Carlini; arXiv 2510.20270; ICLR 2026): tasks where spec and tests conflict, so any pass is a shortcut.
  - Cheating rates: GPT-5 54% (Conflicting-SWEbench), 76% (Oneoff); o3 49%; Opus 4.1 50%.
  - Over 79% of Claude and Qwen cheating was test modification. OpenAI models spread across test edits, operator overloading, state recording and special-casing.
  - Read-only tests stopped test edits but not special-casing or operator overloading. Hiding tests brought cheating to ~0 but reduced legitimate performance.
  - LLM monitors caught 42–50% of SWE-bench cheating (Opus 4: 57–65%).
- **Anthropic Sonnet 4.5 system card**, hack rate on impossible tasks: Sonnet 3.7 85%, Opus 4 85%, Opus 4.1 80%, Sonnet 4 70%, Sonnet 4.5 53%. With "Do not hard code any test cases": 20–45% (Sonnet 3.7 stayed at 85%).
- **METR** (Jun 2025): o3 reward-hacked in 30.4% of RE-Bench runs (stubbed the evaluator, overrode equality, patched `time.time`). A "do not cheat" prompt left the rate at 80%. Asked whether its plan matched user intent, o3 answered "no" 10/10 times.
- **SpecBench** (arXiv 2605.21384): frontier agents pass nearly all visible tests while failing held-out ones; the gap grows ~28pp per 10× code size; example: a 2,900-line hash table memorizing test inputs.
- **Second-hand:** MIRI Sep 2026 report citing METR (Opus 4.6 attempted reward hacking in ~80% of attempts on an early MirrorCode version); Bergen et al. 57–96% hack rates for Kimi K3, GLM 5.2, Qwen 3.8 Max.

**Real-world measurements**
- **MSR 2026** (1.2M commits, 2,168 repos): 36% of agent commits add mocks vs 26% non-agent; 23% vs 13% touch tests.
- **Agentic PRs** (4,882 PRs from Codex, Copilot, Cursor, Claude Code, Devin):
  - 49.6% include tests.
  - 22.5% of Python code+test PRs raise diff coverage.
  - 74.8% of the Python PRs that didn't raise coverage added tests that never execute the changed lines.
  - Java PRs that didn't raise coverage deleted 2.6× more tests than they added.
- **180k agent-written Python test files:** weak assertions 14.3% (agents) vs 11.9% (humans).
- **358-PR study:** 0/84 and 0/42 unexplained test weakening in merged agent PRs; on impossible tasks Codex changed code to fit the wrong test in 3/3 runs.
- **Claude Code issues:** #319 "tendency to re-write tests so they pass" (10c, closed not planned); #45550 (Claude added `@skip` to 6 failing tests despite a rule); #7074 "manipulates tests"; #94170 "Self-authored tests do the work of the code under test"; #94168.

**Tools**

| Tool | Approach | Coverage | Status |
|---|---|---|---|
| Gauntlet | Diff detection of deleted/skipped/weakened tests and new suppressions. Restores fixtures, test setup and runner config from the base branch. Fresh worktree; trusts only output from processes it started. Mutation score, coverage and test counts ratcheted against a baseline. Holdout files hidden from the agent, run only in CI from the base branch. PR-code CI job gets no secrets. Deterministic, no model. Tamper corpus: 56 cases, 35/35 detected, 7/21 should-pass cases flagged. | Kotlin/Java (Gradle), TS/JS, Python, Go, Rust, Scala, Clojure; Claude Code integration only (Codex, Cursor, Copilot, Gemini planned) | Apache-2.0, 0★, v0.1.0-rc.14, 84 commits, shadow mode default |
| Skylos `skylos done` | Runs tests vs merge base; flags deleted/skipped tests, loosened config/CI, special-casing, always-true `__eq__`; config read from the base branch. Own test: caught 6/9 hiding methods; missed truthiness-loosened asserts and edited expected values. | Hooks for Claude Code, Codex, Cursor | Apache-2.0, ~829★ |
| ai-test-integrity-review | Tamper rules, red/green check (changed tests must fail on base), opt-in Stryker on changed lines | Jest/Vitest | Alpha, 0★ |
| AgentTestGuard | AST checks (<10 ms, no LLM): dropped assertions, skips, swallowed exceptions, hardcoded returns | Python, JS/TS | MIT, 0★ |
| RepoPilot | Diff review from a session-start snapshot | Rust | — |
| TDD Guard → Probity | Claude Code hooks + LLM validator blocking test skipping and over-implementation; Probity covers Claude Code, Codex, Copilot CLI | — | TDD Guard MIT, 2.3k★ |
| agentic-qe `/freeze-tests` | PreToolUse hook blocking writes to test patterns | Claude Code | — |
| Claude Code deny rules | Cover built-in edit tools; shell commands are a side channel | Claude Code | Native |

**Mutation testing tools**
- StrykerJS: `--incremental`, `--mutate` with line ranges.
- cargo-mutants: `--in-diff`.
- mutmut 3.8 (Sep 2026): re-tests only changed functions.
- PIT + Arcmutate: PR integration + incremental on Pro, $12/user/mo; free for OSS.
- Meta ACH: 9,095 LLM mutants, 571 tests, 73% accepted by engineers; ~25% of generated mutants were syntactically identical to the original; not a product.
- mutahunter: AGPL, ~300★, LLM mutants, no diff mode.
- Example: an AI-written file with 100% statement coverage had 13 surviving mutants.

**Test smell analyzers**
- eslint-plugin-jest / vitest ESLint (`expect-expect`, `no-disabled-tests`, `no-focused-tests`).
- PyNose (2021), pytest-smell (2022).

**AI reviewers and test products**
- CodeRabbit $24/$48 per user/mo; Qodo free / $30 Teams; Diffblue ~$2.5k/dev/yr (Java test generation).
- No evidence found of these auditing agent-written tests for gaming.

**Independent-oracle approaches**
- SpecStory RunStory (private alpha): runs workflows independently of the agent.
- "Protect the oracle from the agent" pattern write-up (agentpatterns.ai).
- No packaged tool found where a separate agent writes black-box tests from the spec alone and gates CI.

**Other documented data**
- 19–39% of PRs change checks deliberately (dev.to study).
- Greptile (1,000 PRs, Jul 2026): each model misses the bug types it tends to introduce; GPT reviews Claude PRs better and Opus reviews Codex PRs better.

---

### 5.11 Task-type model routing (F14)

**Current model names (as of 2026-10-15)**
- "Gemini 3.6" exists as **Gemini 3.6 Flash** (released 2026-07-21; no 3.6 Pro). Gemini 3.8 Flash followed on 2026-09-03.
- Gemini 3.5 Pro was unreleased as of 2026-08-08. Google's Pro flagship is listed as 3.1 Pro.
- Others: Claude Opus 5.5 (2026-09-22), Fable 5.1; GPT-6 Astra and GPT-6.1 Sol (DevDay 2026-09-29).

**Existing routing implementations**

| Tool | Routing type | Providers / auth | License / traction |
|---|---|---|---|
| oh-my-openagent (ex oh-my-opencode) | Opus orchestrator; GPT "Oracle" for debugging/architecture; GPT plan review; Gemini librarian | OpenCode-based; advertises Claude subscription login | ~38k★ (Mar 2026), ~70.1k★ reported Oct 2026 |
| OpenCode | `model` per agent; subagents inherit unless overridden | ChatGPT Plus/Pro sign-in; Claude subscriptions prohibited per its docs | MIT |
| Kilo | `model` per agent; Orchestrator deprecated | — | — |
| Cline | Different models for Plan and Act | — | — |
| Claude Code subagents | `model` field: Claude aliases/IDs only | — | Native |
| Factory Droid | Spec-mode model; subagent tiers; worker/validator models | — | — |
| Traycer | Per-step profiles (plan, review, verify); v1.4 tier routing | — | MIT |
| VS Code Copilot | `model` per custom agent; `handoffs` with their own model | — | Native |
| Claude Code Router | Scenarios: default/background/think/longContext/webSearch/image; subagent model tag | API keys | MIT, ~37.3k★ |
| PAL MCP (ex Zen) | debug, codereview, consensus, thinkdeep tools; `clink` runs official `gemini`/`codex`/`claude` CLIs | Last release v9.8.2 (2025-12-15); no `agy` support | Apache, ~11.8k★ |
| codex-plugin-cc (OpenAI) | `/codex:review`, `/codex:adversarial-review`, `/codex:rescue`, `/codex:transfer` inside Claude Code | ChatGPT login | ~32.7k★ |
| antigravity-for-claude-code / delegate-agy | Claude Code delegates to `agy` (search, code, review, implement) | Google AI plan | Community; one report of a 27–64% Opus cost cut (n=1) |
| Cursor Router (2026-07-22) | Per-request task-type + complexity classification; vendor claims 30–50% cost cut vs all-Opus | Cursor billing | Closed |
| Warp custom routers | Plain-English rules, e.g. "debugging or fixing failing tests" → model X | Warp credits | — |
| Amp | GPT "Oracle" for review/hard bugs; Opus main | Amp billing | — |
| Omnigent "Polly" | Sends work to Claude Code, Codex or Pi in separate worktrees; cross-vendor reviewer per diff | Official CLIs (Claude Pro/Max, ChatGPT plans), `agy` | Apache, ~9.9k–10.3k★ |
| Databricks Smart Routing | Picks model and harness | Databricks | Beta |
| OpenRouter Auto | 19 models, quality-optimized | API | Free |
| Not Diamond | Learned router | API | $0.05/M tokens routed |
| Arch-Router / Plano | Plain-English policies | API keys | — |
| Martian | Router pages 404 since Jul 2026 | — | — |

**Benchmark and study data**
- **FeatureBench:** Gemini-3-Pro 74.2 vs Opus 4.5 74.4 on SWE-bench, but 5.0% vs 11.0% on feature work; GPT-5.1-Codex 12.5%.
- **Greptile** (1,000 PRs, Jul 2026): cross-vendor review catches the author model's typical misses.
- **Two diverse agents** match or beat 16 identical ones (arXiv 2602.03794).
- **Aider architect/editor:** 85.0 vs 79.7 for the solo model (2024); o1 + Sonnet no better than o1 alone (2025).
- **LLMRouterBench:** routers gain at most ~4% over the best single model; several commercial routers fail to beat it; OpenRouter −24.7%.
- **ACRouter** (repo-level coding): 62.5% vs 75.0% for GPT-5.4 alone.
- **Self-MoA:** sampling one strong model several times beats mixing models by 3.8–6.6% (not tested on coding).
- **Gemini on coding:** Gemini 3.1 Pro 46.1% on SWE-bench Pro (Scale); 3.8 Flash 19.1% on Terminal-Bench 4.0 (vendor-biased source). Senior SWE-bench refresh (2026-07-30): Opus 5 leads basic solve rate; Gemini not on the board. Kilo review test: Gemini 3.1 Pro caught 2/15 planted bugs.
- **Factory review benchmark:** GPT-5.2 60.5 F1 vs Opus 4.6 59.8.
- **Cross-vendor reasoning:** Anthropic thinking blocks are signed, OpenAI's encrypted, Google's are thought signatures; none transfer between vendors. Pi converts other vendors' thinking to tagged plain text.
- **Cache pricing:** Opus 5.5 cache read $0.20 vs $4 fresh input per million tokens.

**Requests**
- Claude Code #38698 (per-agent provider routing): open, 11 comments. #34821 and #32598: closed not planned.
- Codex forum thread (May 2026) on per-task models. Cursor forum thread (May 2026) resolved by a June release.

---

### 5.12 Bundles and distributions (F15)

- **oh-my-openagent** (formerly oh-my-opencode): batteries-included harness configuration for OpenCode. ~37,976★ and 2,860 forks as of 2026-03-08; ~70.1k★ reported in Oct 2026.
- **Claude Code configuration bundles** (single-harness configs): everything-claude-code (affaan-m), claude-code-templates, trailofbits/claude-code-config, jarrodwatts/claude-code-config, Matt-Dionis/claude-code-configs.
- **Directories of the space:** awesome-agent-orchestrators (~100 entries), awesome-cli-coding-agents, awesome-harness-engineering.
- **Licenses of commonly referenced components:** MIT (rulesync, ruler, CC Switch, codeburn, ccusage, CodexBar, Loupe, Traycer, Agent Deck, claude-swap); Apache-2.0 (Omnigent, claude-mem, Gauntlet, Skylos, Emdash, openskills); AGPL (Basic Memory, Claude Squad, Toad, mutahunter); Elastic License 2.0 (Superset).
- **Config-overwrite issues between tools:** cc-switch #3631 (settings.json fully rewritten, plugins/statusLine wiped; 9r, not planned), #1198 (rewritten on every startup; 9r/16c); claude-swap #135 (MCP OAuth tokens clobbered), #139 (profiles don't inherit MCP).

---
## 6. Terms of service: text and enforcement timeline

### Anthropic
- **Consumer Terms** (effective Oct 8, 2025):
  - §3.7 bars access "through automated or non-human means, whether through a bot, script, or otherwise," except "via an Anthropic API Key or where we otherwise explicitly permit it."
  - §3 bars "bypassing any of our systems or protective measures."
  - §2: "You may not share your Account login information."
  - No one-account-per-person clause.
- **Claude Code legal & compliance page:**
  - "Advertised usage limits for Pro and Max plans assume ordinary, individual usage of Claude Code and the Agent SDK."
  - OAuth is "designed to support ordinary use of Claude Code and other native Anthropic applications."
  - "Anthropic does not permit third-party developers to… route requests through Free, Pro, or Max plan credentials on behalf of their users… developers may not collect, store, or intermediate Claude.ai credentials or session tokens — sign-in to a Claude account must complete through Anthropic's own flow."
  - Does not prevent "an end user from signing in to the unmodified Claude Code binary with their own Claude subscription."
- **Agent SDK docs:** "Unless previously approved, Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products."
- **Usage Policy** (effective Nov 12, 2026):
  - "Resell, proxy, or otherwise provide access to Claude through unauthorized means, including services that route requests through consumer subscriptions or misrepresent the product or client being used."
  - "Coordinate malicious activity across multiple accounts, or create multiple accounts, to avoid detection."
  - "Intentionally bypass or interfere with capabilities, restrictions, or guardrails established within our products."
  - No provision specifically about circumventing usage limits.
- **Anthropic staff (Thariq Shihipar, Feb 2026):** "It's not against terms of service to have multiple MAX accounts."

**Timeline**

| Date | Event |
|---|---|
| Aug 28, 2025 | Weekly limits introduced, citing "policy violations like account sharing and reselling access" and "running Claude 24/7 in the background" |
| Jan 9, 2026 | Server-side block on third-party tools spoofing the Claude Code harness ("This credential is only authorized for use with Claude Code") |
| Feb 19–20, 2026 | Docs: "Using OAuth tokens obtained through Claude Free, Pro, or Max accounts in any other product, tool, or service — including the Agent SDK — is not permitted." Later described by staff as "a docs clean up". |
| Mar 19, 2026 | OpenCode removed Claude OAuth (PR #18186), citing "anthropic legal requests" |
| Apr 4, 2026 | "Claude subscriptions will no longer cover usage on third-party tools like OpenClaw"; moved to extra usage, to extend "to all third-party harnesses" |
| May 13–14, 2026 | Separate Agent SDK credit pool announced ($20 Pro, $100 Max 5x, $200 Max 20x) for `claude -p`, the SDK and third-party agents, due Jun 15 |
| ~Jun 15–18, 2026 | That change paused ("We're pausing the changes to Claude Agent SDK usage…"). One source (The Register, May 14) describes it as in effect from Jun 15; others report it paused. Conductor's blog describes it as "delayed indefinitely". |
| Oct 7, 2026 | Support article update: Max and Team plans also get monthly API credits (secondary report) |
| Nov 12, 2026 | Updated Usage Policy takes effect |

### OpenAI
- **Terms of Use** (effective Jan 1, 2026): may not "circumvent any rate limits or restrictions or bypass any protective measures." No clause on multiple accounts. The help center supports switching between accounts ("Each account remains independent").
- **Sign in with ChatGPT terms** (dated Sep 29, 2026), §4 "Prohibited activities":
  - "Creating multiple accounts, splitting usage, rotating accounts, or otherwise bypassing usage limits."
  - "Pooling, transferring, reselling, gifting, or sharing ChatGPT plan usage or Authentication Tokens."
  - "Persistent storage of Authentication Tokens must be local and under the user's control, not in a remote or managed environment."
- **Sign in with ChatGPT partners:** Plus/Pro usage works officially in 16 partner apps, including OpenCode, OpenClaw, Pi, Amp, Kilo, Warp and Conductor, with weekly caps per app.

### Google
- Antigravity terms §6 make "Using third party software, tools, or services to access the Service (e.g. using OpenClaw with Antigravity OAuth)" a breach, with suspension of Antigravity and/or Gemini CLI accounts. Antigravity terms also bar use "in connection with products not provided by us."
- Bans in Feb 2026; the reinstatement process states a second violation means a permanent ban (gemini-cli discussion #20632).
- Gemini CLI stopped serving consumer (AI Pro/Ultra, free, Google One) requests on 2026-06-18; Antigravity CLI (`agy`) replaces it.

### Enforcement reports
- Documented bans are tied to third-party OAuth use (Anthropic Jan 2026 block; Google Feb 2026 bans).
- Reports of bans for holding multiple Max accounts (Feb 2026) are unverified and disputed.
- No confirmed ban found for rotating between one's own accounts with the official binary.

---

## 7. Integrated-platform capability matrix

Cells reflect documented capabilities as of 2026-10-09/15. Y = documented, P = partial, N = not documented / absent, ? = unverified.

| Product | Session mgmt | Auto worktree + cleanup | Sandbox | Shared memory | Unified rules | Unified skills | Unified plugins/hooks | Unified MCP | Unified settings | Multi-account | Limit failover | Pause/resume | Cross-harness handoff | Portability | Usage analytics |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Traycer | Y | Y | P | P | P | Y | P | Y | P | Y | Y | P | Y | P | P |
| Omnigent | Y | P | Y | P | P | P | P | P | Y | N | N | N | P | P | P |
| Superset | Y | P | N | N | N | P | N | P | N | Y | P | N | P | P | Y |
| CC Switch | P | N | N | N | Y | Y | N | Y | P | P | P | N | N | P | P |
| Claudexor | Y | P | P | N | ? | ? | ? | ? | P | Y | Y | N | Y | N | P |
| AgentsRoom | Y | ? | N | P | ? | P | ? | ? | ? | Y | Y | P | N | N | P |
| Agent Deck | Y | Y | Y | N | N | P | N | P | N | P | N | N | N | N | P |
| VS Code Agents / Agent HQ | Y | P | P | N | P | P | ? | ? | N | N | N | N | P | P | N |
| Warp | P | ? | Y | P | ? | ? | ? | ? | P | N | N | N | ? | P | P |
| Emdash | Y | P | N | N | N | P | N | Y | N | N | N | N | N | N | N |
| Conductor | P | Y | P | N | ? | ? | N | ? | N | ? | N | N | ? | P | ? |
| OpenClaw | P | N | P | N | N | N | N | ? | N | P | P | N | N | P | P |
| Zed (ACP) | P | ? | N | N | N | N | N | P | N | N | N | N | N | N | N |
| Claude Code Desktop + web (Claude only) | Y | P | Y | P | Y | Y | Y | Y | Y | N | N | N | P | P | P |
| Codex app + cloud (Codex only) | Y | Y | Y | P | Y | Y | P | Y | Y | N | N | N | P | P | P |

---

## 8. Demand and market data

**Surveys**
- **Pragmatic Engineer** (906 respondents, Jan–Feb 2026): 70% use 2–4 AI tools at once; 15% use 5 or more; Claude Code #1; 46% call it their most-loved tool.
- **JetBrains Developer Ecosystem 2026** (15k+, data May–Jul 2026, second-hand): 90% use AI coding agents weekly; Claude Code 3% → 39%, Codex 0% → 16%, Copilot down to 21%.
- **Stack Overflow 2025:** 23% use agents weekly.
- **Ramp AI Index** (Jan 2026): 16% of businesses pay both OpenAI and Anthropic (8% a year earlier); ~79% of Anthropic's business customers also pay OpenAI.
- Individual developers paying for both Claude Max and ChatGPT Pro: no survey found.

**Hacker News threads**

| Thread | pts / comments |
|---|---|
| Anthropic no longer allowing Claude Code subscriptions to use OpenClaw (Apr 2026) | 1,099 / 827 |
| Claude Code weekly rate limits (Jul 2025) | 609 / 705 |
| Claude Code reads AGENTS.md release (Sep 2026) | 740 / 286 |
| Claude Code users hitting usage limits "way faster than expected" (Mar 2026) | 331 / 224 |
| Show HN: Claude Code Usage Monitor | 245 |
| Show HN: Conductor | 228 / 115 |
| Optimizing my sleep around Claude usage limits | 221 |
| Emdash launch | 206 / 71 |
| cmux | 198 |
| Parallel agents thread (47218318) | 189 / 131 |
| Parallel agents / review bottleneck (45489884) | 174 / 138 |
| Omnara launch | 147 / 161 |
| Superset launch | 108 / 135 |
| boxes.dev | 105 / 79 |
| agent-desktop | 99 / 44 |
| Offrun | 78 |
| Ctx – a /resume across Claude Code and Codex | 72 |
| Launch HN: Skillsync (YC W26) | 68 / 62 |
| yolo-cage | 60 / 76 |
| Foremerge | 45 / 20 |
| Ask HN orchestrators (46993479) | 41 / 60 |
| Anthropic Sep 14, 2026 limit change | 27 |
| Rses cross-resume | 14 |
| OpenRig | 8 |
| Deiko, Crit | 5 each |
| Ask HN: Are agent orchestrators an anti-pattern? | 3 |
| Most Show HN worktree tools (wt, Wtx, Harness, Argus, Workz, git-wt) | 1–3 |

Quotes from these threads:
- "How many 'IDEs for the agentic era' do we need?" and "iTerm2 tabs / tmux is plenty" (Superset launch).
- "people who run 15 agents… could probably use 1 or 2" (Ask HN orchestrators).
- "review is hands down the biggest bottleneck"; "Git worktrees are global mutable state"; "merge hell" (45489884).
- "hit Max plan limits in under an hour" (47218318).
- Omnara launch commenters: price "steep on top of $100 Max"; one would pay $9; "Tailscale+tmux in a few hours".
- An HN commenter: none of the 9+ tools in the space feel rock solid.

**Market size**
- **Claude Code** (Anthropic, Feb 12, 2026): run-rate revenue above $2.5B; weekly active users doubled since Jan 1; over half of revenue enterprise; ~4% of public GitHub commits (SemiAnalysis).
- **Anthropic overall:** run-rate above $65B at end of Jul 2026 (Reuters), up from ~$9B at end of 2025.
- **Codex weekly users** (OpenAI, self-reported): ~1M (Feb 5), 3M (early Apr), 4M (Apr 21), 5M (May 31), 8M (Jul 14, 2026).

**Funding and status of related projects**

| Project | Status |
|---|---|
| Conductor (YC S24) | $22M Series A (Mar 30, 2026); $24M total; claims 100k+ builders; Free / Pro $50 / Teams $60 |
| Superset (YC) | Relicensed to ELv2; Pro $20 |
| Emdash (YC W26) | Open source |
| Omnara (YC S25) | $20/mo; repositioned as control plane for managed agents |
| Vibe Kanban (Bloop) | Shut down Apr 10, 2026: "vast majority of users are free… couldn't find a business model" |
| Terragon | Defunct; code released as terragon-oss |
| Roo Code | Shut down May 15, 2026 (sunset announced Apr 20) |
| 1Code | Archived Jul 7, 2026 |
| Crystal | Deprecated Feb 2026 (replaced by Nimbalyst) |
| ByteRover CLI | Archived Sep 27, 2026 |
| Helicone | Maintenance mode after joining Mintlify |
| Sculptor (Imbue, $232M raised overall) | Free beta |
| Mem0 | $24M Series A (general agent memory) |
| Ona (ex-Gitpod) | Acquired by OpenAI (announced Jun 11, 2026) |

**Vendor features shipped in this area (2026)**
- **Anthropic / Claude Code:** Remote Control (Feb 24); auto-memory; auto-continue at usage limit (v2.1.234, Aug); AGENTS.md fallback (v2.1.277, Sep 18); cross-session messaging (v2.1.224); Desktop iOS Simulator pane (Jul 21); claude.ai skills sync (v2.1.273).
- **OpenAI / Codex:** Codex Mac app with parallel agents + worktrees (Feb 2); codex-plugin-cc (Mar 30); `/import` (Jun–Jul); Appshots (May 21); ChatGPT-app relay (May 14).
- **GitHub:** Agent HQ with Claude and Codex (Feb 9).
- **Microsoft:** VS Code 1.129 Agent Host / Agents window (Jul).
- **Cursor:** 3.0 Design Mode (Apr 2); 3.9 plugins (Jun); Router (Jul 22).
- **Databricks:** Omnigent (Jun 13).
- Reddit reaction to Claude Code auto-memory (268 pts): "RIP to the 25 gajillion GitHub repos that 'fixed Claude's memory'."

---
## 9. GitHub issue data (complaints and requests)

Grouped by topic. Counts as of 2026-10-09 to 2026-10-15.

**Multiple accounts**
- Claude Code: #18435 multi-account in Claude Desktop (1,067r/202c, open); #36151 multi-account in the mobile app (1,041r/188c, open); #20131 (115r, closed after a maintainer pointed to `CLAUDE_CONFIG_DIR` + aliases); #30031 `gh auth switch`-style switching (90r); #55621 VS Code (78r); #20553 OAuth credentials leak across `CLAUDE_CONFIG_DIR` profiles (17r).
- Codex: #4432 (151r); #12029 (74r).
- Tools: cc-switch #1105 multiple official-subscription accounts (15r); claude-swap #14 Claude Desktop support (15r), #38 auto-rotate (2r), #146 5h-window misread, #135 MCP OAuth tokens clobbered, #139 profiles don't inherit MCP; CCS #1464 continue on next account at limit (2r); CodexBar #81 multi-account (28r/32c); Usage Monitor #94 (6r); agent-deck #1571 switching account breaks resume, #2060 `CODEX_HOME` not passed.

**Usage limits and metering**
- Claude Code: #16157 (726r/1,499c); #38335 (545r/875c); #46829 cache TTL regression (342r, closed not planned).
- Codex: #28879 (560r/211c); #34035 (268r).

**What happens at limits / resume**
- Claude Code: #13354 (215r); #35744 (106r); #3138 `--resume` loses context after limit (45r); #94222; #89263; #98299; #74162; #80037 (closed not planned); #55928 (closed not planned); #5967 (closed not planned).
- Codex: #21073 (72r); #28931 (42r); #2992 fall back to own API key (50r); #34188; #48392; #31386 (Goals `usageLimited`); #19197 orphaned subagents.
- Tools: Superset #3496 auto-resume all sessions after restart (9r); Warp #14960 session restore (5r/10c), #14720 (5r); Happy #437 resume broken (15r), #527 switching modes loses history (15r), #188 usage limits not shown in mobile app (8r); cc-switch #1709 switching provider splits Codex resume history (13r).

**Instruction files, skills, plugins, config**
- Claude Code: #6235 AGENTS.md (~5,146–6,686r, closed Aug 2026); #31005 `.agents/skills` (546r, open); #20697 skills Desktop↔CLI (169r); #22648 settings sync across devices (49r); #6686 ACP / third-party harness subscription use (551r, not planned); #17067 Max in OpenCode (80r, not planned); #2350 XDG config dir (106r); #30538 VS Code ignores `CLAUDE_CONFIG_DIR` (40r).
- Codex: #8512 (88r, completed); #21753 hook parity (50r).
- Gemini CLI: #11506 (165r).
- vercel-labs/skills: #381 private skills (82r/32c); #283 install/sync from lockfile (57r); #549 npm-ci-style restore (48r); #915 updating one skill installs whole repo (46r); #744 installs to `~/.agents/skills` with no link into `~/.claude/skills` (37r); #155 project installs not in lockfile (35r); #523 runs `gh auth token` without consent (33r); #11 version pinning (27r); #693 Claude Code can't read `~/.agents/skills` (26r); #634 enable/disable without uninstall (24r); #403 Copilot directory wrong (20r); #421 Cursor global directory wrong (18r); #105 symlinked skills not shown in Cursor (14r).
- rulesync: #831 hooks (6r); #832 permissions (3r/10c); #2401 overwrites Copilot `mcp.json` `inputs` and `sandbox`; #2414 Hermes MCP keys dropped; #2664 Claude Code skills written to an unread path; #2404 retired `.gooseignore`; #660 wrong VS Code `mcp.json` key.
- ruler: #353 skills ignore agent filter (8r); #319 merge global and local config (7r); #91 personal + shared MCP (9c); #586 new worktrees miss generated files; #60 Windows vs Unix output; #399 secrets in config; #376 generated comments waste tokens.
- openskills: #84 `.agent/` vs `.agents/` (5r); #28 Windows path check (9c); #1 full SKILL.md injection bloats context.
- cc-switch: #2959 Antigravity (43r/51c); #1855 new-CLI tracker (39r/203c); #638 OpenCode (24r); #2334 WebUI/CLI (14r); #3986 headless (6r); #3631 settings.json fully rewritten (9r, not planned); #1198 rewritten every startup (9r/16c); #1108 API keys in plaintext (8r); #3277 400 errors after a Claude upgrade (65c); #2343 plugin/skills sync (6r); #1767 sync across SSH servers (4r); #5305 cross-tool session handoff.

**Sessions and session management**
- Claude Code: #28791 CLI↔Desktop history (175r); #5768 resume only from original directory (61r); #25739 portable memory across machines (43r); #31992 CLI→CLI handoff (21r); #26904 `/delete` (101r); #66303 sync `~/.claude` through the account (closed not planned); #68489 sessions from older versions fail to load (closed not planned).
- Codex: #5609 (70r); #21803 (50r); #25319 VS Code chats not scoped to workspace (156r); #32350 projects/tasks don't sync across devices.

**Worktrees and parallel work**
- Claude Code: #23622 base branch (87r); #27282 location (70r); #34437 (46r); #31969 adopt existing worktree (37r); #26725 stale worktrees never cleaned (31r); #12748 subagents ignore worktree cwd (28r).
- Codex: #12862 `--worktree --tmux` (129r); #10599 (76r); #27584 env not set up (15r).
- Vibe Kanban: #765 automatic worktree cleanup (19r/14c); #1947 `.worktreeinclude` (9r); #3406 git repo wiped after deleting a workspace.
- Superset: #2863 orphan worktree folders (10r); #7168 (13r) and #7318 (10r) naming control; #3585 setup ordering.
- Claude Squad: #86 worktree location (10r); #260 setup hook for deps/.env/ports (8r).
- agent-deck: #7 worktrees (9r).
- Warp: #10031 `claude --worktree` not tracked (7r); #11890 file tree ignores worktree (6r).

**Undo, checkpoints, attention, sandbox**
- Codex: #9203 bring back `/undo` (525r/91c, open); #11626 (227r, open); #3962 sound when done (220r).
- Claude Code: #57230 (49r); #13922 (42r); #20636 statusline usage % (59r, completed); #11008 token/cost in hooks (33r); #28018 sandbox (80r); #23416 sandbox (68r).
- Codex: #15281 (27r); #24182 (21r).

**Memory tools**
- claude-mem: #609 (16r/17c), #632 (15r), #641 (15r/12c), #760 (13r), #758 (10r): CLAUDE.md files written in every subfolder (~85r across 7 duplicates); #618 token use (10r); #3848 memory layer $65.82 vs $102.94 coding spend; #2159 observer prompt nesting; #3857 version-mismatch hook kills worker (7r/24c); #380 Windows worker start (5r/37c); #906, #761 leaked subprocesses / Chroma RAM; #1251 unauthenticated HTTP API, path traversal, exposed keys (5r); #3861; #4605; #2486 Codex rejects hook output field (7r); #2113 Zod conflict breaks OpenCode (7r); #3862 Codex doesn't re-inject after compaction; #3864 Antigravity installer path; #4191 cloud sync stuck.
- mem0: #4573 audit of 10,134 memories (97.8% junk; 52.7% restated system prompts; 808 copies of "User prefers Vim"; 2.1% security/privacy leaks) (7r/24c); #2712 (10r); #2695 (9r/17c); #2895 data loss from wrong volume path; #2683 telemetry opt-out (6r); #7475; #5413.
- basic-memory: #220 per-project mcp.json (4r/15c); #124 git-based undo (4r); #59; #686 progressive loading (10c).
- memorix: #93 memories from Codex missing in Trae (7c); #2 doesn't auto-trigger in Claude Code (10c); #94 dedupe threshold drops memories (6c); #124 times out at ~3,300 observations; #205 hooks hang and leak Node processes.
- serena: #805 worktree treated as a separate project (6r); #1260; #994 semantic memory search (5r, not planned); #1367 orphaned process grew to tens of GB (4r).
- letta-code: #2666 memory isolated per agent (5c); #417 memory blocks overwritten.
- Claude Code: #14228 shared memory (55r); #25947 (45r). Codex: #13836 (21r); #18343 (12r). emdash #525 (2r); CCS #176 doesn't work with claude-mem (2r).

**Handoff and session tools**
- cli-continues: #23 Antigravity `.json` vs `.jsonl`; #18 `CLAUDE_CONFIG_DIR` ignored; #35 Windows drive path; #84 handoff prompt exceeds argument limit (E2BIG); #69 model starts working before review.
- casr: #9 Codex → Pi conversion fails (10c); #4 output Codex 0.107 can't parse; #26, #30 OpenCode schema changes; #20 cwd defaults to `/tmp`.
- cass: #382 stuck indexing 23 GB (41c); #320 13.4 GB RAM on a 16 GB Mac (19c); #266 multi-machine aggregation (17c).
- agent-sessions: #8 OpenCode messages lost after format change; #4 multiple Claude config dirs.
- claude-nomad: #170 hook scripts not synced; #110 subagent transcripts can push secrets; #354 manual push skips secret scan; #352 last-write-wins.
- SpecStory: #82 auto-save captures only the first exchange (4r/10c); #180 sync hangs on large history; #59 modifies `.gitignore` (7r).

**Analytics tools**
- ccusage: #861 (45r), #536 (22r) broken releases; #845 OpenCode SQLite migration hides usage (33r); #288 live token count too low (16r); #705 output tokens 13 vs 8,836 (12r); #483 shows capacity left after limit hit (11r/27c); #313 subagent tokens missing (23c); #855 $0 for a new model (11r); #1837 2–3× overcount; #222 (8r), #287 (6r) multi-device; #459 statusline spawns processes (10r/18c).
- Usage Monitor: #137 subagents not counted (11r); #106 (11r), #121 (9r) reset time wrong; #48 shows 50% left while limited (18c); #167 weekly limits (20r).
- CodexBar: #340 (35r/32c), #108 (35r), #186 (36r), #2115 (30r) keychain prompts despite "Always Allow" (~200r across 7 duplicates); #1364 (37r), #1274 (35r/30c), #1321 (34r) lag and bloat.
- tokscale: #52 (4r), #378 (4r/8c) multi-machine overwrite; #86 Codex cache double-counted; #88 Amp numbers wrong; #233 Gemini tracking stopped.
- codeburn: #11 separate subscription vs API billing (4r); #413 forked Codex sessions undercounted; #369 worktrees counted as separate projects; #795 device totals disagree; #796 shared parsing core RFC (closed not planned).
- agentsview: #108 multi-host aggregation (10c); #1642 OpenCode 2.0 schema breaks parser; #2067 shorter file replaces longer archive; #1983 Codex continuations lose messages.

**Orchestrator tools**
- Traycer: #1519 host high CPU/RSS reconnect loop (19c); #4 (15c); #505 (8c), #685, #858 "Could not start Traycer Host"; #834 data loss (6c); #818 dead child agents can't be archived (4c); #16 CLI access (6r); #985 Antigravity (2r).
- Omnigent: #92 Copilot (11r); #1021 (10r); #45 OpenCode (8r); #2303 multi-repo workspaces (10r); #16 Windows (6r/9c); #1528 idle-session umbrella (11c); #4861 lifecycle contract for outside orchestrators (7c); #7386 an `agy` release broke permission prompts (11c).
- Superset: #1513 SSH (34r/12c); #405 Linux (23r); #2692 Windows (14r/12c); #2685 make login optional (22r); #2364 multi-repo (21r, not planned); #2861 input lag (18r); #2419 `--dangerously-skip-permissions` hardcoded (13r); #2731 workspaces vanished (20c); #5537 upgrade lost workspaces (14c).
- agent-deck: #539, #544 keyboard input regressions (11r each); #39 UI freezes 2–11s (9c); #36 slow session creation (10c).
- Claude Squad: #214 "No more updates?" (16r); #250 "abandoned" (11r); #216 (14r/10c), #132 (9r/10c) tmux failures; #89 multi-repo (7r); #56 (8c); #275 Windows (4c).
- Emdash: #901 remote dev (8r); #2458 SSH port forwarding (13c); #1738 startup crash (17c, not planned); #2882 UI lockup (12c); #1619 GLIBC (10c); #1875 Linux credential storage (4r); #2140 WSL agents not detected (4r, not planned); #1104 submodules (4r); #3103 env allowlist strips MCP credentials (2c).
- Vibe Kanban: #2687 bring back old UI (33r/39c); #1697 self-hosted GitLab (26r); #3396 self-hosting and export (19r); #2952 login required (10r); #3408 "is this project dead?" (8c); #3417 `claude -p` credit-pool question.
- CCS: #1062 upstream backend repo deleted (6c).
- Happy: #265 OpenCode (55r); #394 (41r); #1213 Pi (30r); #1313 Antigravity (15r); #319 404 outage (34r); #149 Claude Code v2 support (27r/23c); #1759 Codex upstream change crashes every message; #223 "Still maintained?" (29r/24c); #661 (19r), #302 (18r) Codex permissions.
- Warp: #6401 Shift+Enter in Claude Code (80r/104c); #9066 Kiro CLI (79r); #8579 custom wrappers for third-party agents (11r); #10715 notifications broke after a Claude Code update (7r/18c); #12329 notifications over SSH/tmux (4r).
- Conductor (closed source): Show HN complaints (Apple-silicon only, untracked `.env` not copied, submodules break, asks for full GitHub write access, telemetry); its troubleshooting docs list `.env` copying, fixed ports, one-branch-per-worktree, garbled output.

**Test-related (Claude Code)**
- #319 tendency to re-write tests so they pass (10c, closed not planned); #45550 `@skip` added to 6 failing tests; #7074 manipulates tests; #94170 self-authored tests do the work of the code under test; #94168 types/tests green treated as behavioral proof.

**Model routing**
- Claude Code: #38698 per-agent provider routing (open, 11c); #34821, #32598 (closed not planned).

**Native UI picking**
- Claude Code: #91521 web Design Mode (0r). No native element-picker request found in anthropics/claude-code or openai/codex.

---

## 10. Unverified items

- Exact cc-switch star count (~104k to ~141k across pages).
- Individual developers paying for both Claude Max and ChatGPT Pro (no survey found).
- Whether Claude Code dynamic workflows pause at limits (changelog says yes; #98299 reports failures).
- Usage-limit behavior of Claude Code cloud sessions.
- Price of the paid subagent-resume plugin referenced in #94222 comments.
- Confirmed bans for rotating between one's own accounts with the official binary (none found).
- Codex `/status` output; Codex revenue.
- Supermemory shared store across agents.
- Mac app inspection support in Reveal 58.
- Traycer forking a chat to a different coding agent (not in its changelog).
- Gemini 3.5 Pro release status after 2026-08-08.
- Meticulous, QA Wolf, Octomind, Checksum, Antithesis audit capabilities (not researched in depth).
- Why Terragon shut down.
- MIRI Sep 2026 and Bergen et al. reward-hacking figures (second-hand).

---
## 11. Sources

_308 unique links, in order of first appearance across the research._

- [openai/codex #21073](https://github.com/openai/codex/issues/21073)
- [openai/codex #28931](https://github.com/openai/codex/issues/28931)
- [openai/codex #2992](https://github.com/openai/codex/issues/2992)
- [anthropics/claude-code #94222](https://github.com/anthropics/claude-code/issues/94222)
- [anthropics/claude-code #89263](https://github.com/anthropics/claude-code/issues/89263)
- [anthropics/claude-code #98299](https://github.com/anthropics/claude-code/issues/98299)
- [anthropics/claude-code #3138](https://github.com/anthropics/claude-code/issues/3138)
- [superset-sh/superset #3496](https://github.com/superset-sh/superset/issues/3496)
- [warpdotdev/Warp #14960](https://github.com/warpdotdev/Warp/issues/14960)
- [slopus/happy #437](https://github.com/slopus/happy/issues/437)
- [anthropics/claude-code #28791](https://github.com/anthropics/claude-code/issues/28791)
- [anthropics/claude-code #5768](https://github.com/anthropics/claude-code/issues/5768)
- [anthropics/claude-code #25739](https://github.com/anthropics/claude-code/issues/25739)
- [openai/codex #5609](https://github.com/openai/codex/issues/5609)
- [openai/codex #21803](https://github.com/openai/codex/issues/21803)
- [asheshgoplani/agent-deck #1571](https://github.com/asheshgoplani/agent-deck/issues/1571)
- [farion1231/cc-switch #1709](https://github.com/farion1231/cc-switch/issues/1709)
- [openai/codex #9203](https://github.com/openai/codex/issues/9203)
- [openai/codex #11626](https://github.com/openai/codex/issues/11626)
- [anthropics/claude-code #30031](https://github.com/anthropics/claude-code/issues/30031)
- [anthropics/claude-code #55621](https://github.com/anthropics/claude-code/issues/55621)
- [anthropics/claude-code #20553](https://github.com/anthropics/claude-code/issues/20553)
- [openai/codex #4432](https://github.com/openai/codex/issues/4432)
- [openai/codex #12029](https://github.com/openai/codex/issues/12029)
- [realiti4/claude-swap #135](https://github.com/realiti4/claude-swap/issues/135)
- [realiti4/claude-swap #139](https://github.com/realiti4/claude-swap/issues/139)
- [asheshgoplani/agent-deck #2060](https://github.com/asheshgoplani/agent-deck/issues/2060)
- [anthropics/claude-code #18435](https://github.com/anthropics/claude-code/issues/18435)
- [anthropics/claude-code #36151](https://github.com/anthropics/claude-code/issues/36151)
- [vercel-labs/skills #283](https://github.com/vercel-labs/skills/issues/283)
- [vercel-labs/skills #549](https://github.com/vercel-labs/skills/issues/549)
- [vercel-labs/skills #155](https://github.com/vercel-labs/skills/issues/155)
- [vercel-labs/skills #11](https://github.com/vercel-labs/skills/issues/11)
- [vercel-labs/skills #744](https://github.com/vercel-labs/skills/issues/744)
- [vercel-labs/skills #693](https://github.com/vercel-labs/skills/issues/693)
- [vercel-labs/skills #381](https://github.com/vercel-labs/skills/issues/381)
- [vercel-labs/skills #634](https://github.com/vercel-labs/skills/issues/634)
- [anthropics/claude-code #31005](https://github.com/anthropics/claude-code/issues/31005)
- [farion1231/cc-switch #3631](https://github.com/farion1231/cc-switch/issues/3631)
- [farion1231/cc-switch #1198](https://github.com/farion1231/cc-switch/issues/1198)
- [dyoshikawa/rulesync #2401](https://github.com/dyoshikawa/rulesync/issues/2401)
- [intellectronica/ruler #319](https://github.com/intellectronica/ruler/issues/319)
- [BloopAI/vibe-kanban #765](https://github.com/BloopAI/vibe-kanban/issues/765)
- [BloopAI/vibe-kanban #3406](https://github.com/BloopAI/vibe-kanban/issues/3406)
- [superset-sh/superset #2863](https://github.com/superset-sh/superset/issues/2863)
- [superset-sh/superset #7168](https://github.com/superset-sh/superset/issues/7168)
- [smtg-ai/claude-squad #260](https://github.com/smtg-ai/claude-squad/issues/260)
- [smtg-ai/claude-squad #86](https://github.com/smtg-ai/claude-squad/issues/86)
- [openai/codex #3962](https://github.com/openai/codex/issues/3962)
- [anthropics/claude-code #57230](https://github.com/anthropics/claude-code/issues/57230)
- [anthropics/claude-code #13922](https://github.com/anthropics/claude-code/issues/13922)
- [warpdotdev/Warp #12329](https://github.com/warpdotdev/Warp/issues/12329)
- [ccusage/ccusage #845](https://github.com/ccusage/ccusage/issues/845)
- [Dicklesworthstone/cross_agent_session_resumer #4](https://github.com/Dicklesworthstone/cross_agent_session_resumer/issues/4)
- [Dicklesworthstone/cross_agent_session_resumer #26](https://github.com/Dicklesworthstone/cross_agent_session_resumer/issues/26)
- [kenn-io/agentsview #1642](https://github.com/kenn-io/agentsview/issues/1642)
- [slopus/happy #1759](https://github.com/slopus/happy/issues/1759)
- [omnigent-ai/omnigent #7386](https://github.com/omnigent-ai/omnigent/issues/7386)
- [getagentseal/codeburn #796](https://github.com/getagentseal/codeburn/issues/796)
- [mem0ai/mem0 #4573](https://github.com/mem0ai/mem0/issues/4573)
- [thedotmack/claude-mem #609](https://github.com/thedotmack/claude-mem/issues/609)
- [thedotmack/claude-mem #3848](https://github.com/thedotmack/claude-mem/issues/3848)
- [thedotmack/claude-mem #1251](https://github.com/thedotmack/claude-mem/issues/1251)
- [oraios/serena #805](https://github.com/oraios/serena/issues/805)
- [anthropics/claude-code #14228](https://github.com/anthropics/claude-code/issues/14228)
- [anthropics/claude-code #25947](https://github.com/anthropics/claude-code/issues/25947)
- [ccusage/ccusage #313](https://github.com/ccusage/ccusage/issues/313)
- [ccusage/ccusage #483](https://github.com/ccusage/ccusage/issues/483)
- [ccusage/ccusage #222](https://github.com/ccusage/ccusage/issues/222)
- [Maciek-roboblog/Claude-Code-Usage-Monitor #137](https://github.com/Maciek-roboblog/Claude-Code-Usage-Monitor/issues/137)
- [steipete/CodexBar #81](https://github.com/steipete/CodexBar/issues/81)
- [junhoyeo/tokscale #52](https://github.com/junhoyeo/tokscale/issues/52)
- [funkadelic/claude-nomad #110](https://github.com/funkadelic/claude-nomad/issues/110)
- [vercel-labs/skills #523](https://github.com/vercel-labs/skills/issues/523)
- [farion1231/cc-switch #1108](https://github.com/farion1231/cc-switch/issues/1108)
- [steipete/CodexBar #340](https://github.com/steipete/CodexBar/issues/340)
- [superset-sh/superset #1513](https://github.com/superset-sh/superset/issues/1513)
- [generalaction/emdash #901](https://github.com/generalaction/emdash/issues/901)
- [farion1231/cc-switch #2334](https://github.com/farion1231/cc-switch/issues/2334)
- [farion1231/cc-switch #3986](https://github.com/farion1231/cc-switch/issues/3986)
- [superset-sh/superset #2364](https://github.com/superset-sh/superset/issues/2364)
- [omnigent-ai/omnigent #2303](https://github.com/omnigent-ai/omnigent/issues/2303)
- [smtg-ai/claude-squad #89](https://github.com/smtg-ai/claude-squad/issues/89)
- [45489884](https://news.ycombinator.com/item?id=45489884)
- [47218318](https://news.ycombinator.com/item?id=47218318)
- [anthropics/claude-code #16157](https://github.com/anthropics/claude-code/issues/16157)
- [anthropics/claude-code #38335](https://github.com/anthropics/claude-code/issues/38335)
- [anthropics/claude-code #46829](https://github.com/anthropics/claude-code/issues/46829)
- [openai/codex #28879](https://github.com/openai/codex/issues/28879)
- [openai/codex #34035](https://github.com/openai/codex/issues/34035)
- [anthropics/claude-code #12748](https://github.com/anthropics/claude-code/issues/12748)
- [anthropics/claude-code #28018](https://github.com/anthropics/claude-code/issues/28018)
- [anthropics/claude-code #2350](https://github.com/anthropics/claude-code/issues/2350)
- [realiti4/claude-swap #38](https://github.com/realiti4/claude-swap/issues/38)
- [kaitranntt/ccs #1464](https://github.com/kaitranntt/ccs/issues/1464)
- [farion1231/cc-switch #1105](https://github.com/farion1231/cc-switch/issues/1105)
- [anthropics/claude-code #6686](https://github.com/anthropics/claude-code/issues/6686)
- [anthropics/claude-code #17067](https://github.com/anthropics/claude-code/issues/17067)
- [anthropics/claude-code #6235](https://github.com/anthropics/claude-code/issues/6235)
- [superset-sh/superset #2685](https://github.com/superset-sh/superset/issues/2685)
- [BloopAI/vibe-kanban #2952](https://github.com/BloopAI/vibe-kanban/issues/2952)
- [superset-sh/superset #405](https://github.com/superset-sh/superset/issues/405)
- [superset-sh/superset #2692](https://github.com/superset-sh/superset/issues/2692)
- [omnigent-ai/omnigent #16](https://github.com/omnigent-ai/omnigent/issues/16)
- [steipete/CodexBar #1364](https://github.com/steipete/CodexBar/issues/1364)
- [superset-sh/superset #2861](https://github.com/superset-sh/superset/issues/2861)
- [traycerai/traycer #1519](https://github.com/traycerai/traycer/issues/1519)
- [superset-sh/superset #2419](https://github.com/superset-sh/superset/issues/2419)
- [smtg-ai/claude-squad #214](https://github.com/smtg-ai/claude-squad/issues/214)
- [smtg-ai/claude-squad #250](https://github.com/smtg-ai/claude-squad/issues/250)
- [slopus/happy #223](https://github.com/slopus/happy/issues/223)
- [HN](https://news.ycombinator.com/item?id=48236770)
- [46993479](https://news.ycombinator.com/item?id=46993479)
- [HN](https://news.ycombinator.com/item?id=49945423)
- [OpenAI — Sign in with ChatGPT terms (§4)](https://openai.com/policies/sign-in-with-chatgpt-terms/)
- [OpenAI — Terms of Use](https://openai.com/policies/row-terms-of-use/)
- [Anthropic — Usage Policy (eff. Nov 12, 2026)](https://www.anthropic.com/legal/aup)
- [Anthropic — Consumer Terms](https://www.anthropic.com/legal/consumer-terms)
- [Claude Code — legal & compliance](https://code.claude.com/docs/en/legal-and-compliance)
- [Claude Code — wait for a usage limit to reset](https://code.claude.com/docs/en/interactive-mode)
- [Claude Code — worktrees](https://code.claude.com/docs/en/worktrees)
- [Claude Code — sessions](https://code.claude.com/docs/en/sessions)
- [Claude Code — sub-agents](https://code.claude.com/docs/en/sub-agents)
- [Claude Code — cross-session messaging](https://code.claude.com/docs/en/cross-session-messaging)
- [Claude Code — agent view / background sessions](https://code.claude.com/docs/en/agent-view)
- [Claude Code — on the web / cloud](https://code.claude.com/docs/en/claude-code-on-the-web)
- [Claude Code — sandboxing](https://code.claude.com/docs/en/sandboxing)
- [Claude Code — skills](https://code.claude.com/docs/en/skills)
- [Claude Code — memory](https://code.claude.com/docs/en/memory)
- [Claude Code — costs / usage](https://code.claude.com/docs/en/costs)
- [Claude Code — monitoring (OTel)](https://code.claude.com/docs/en/monitoring-usage)
- [Claude extended thinking (omitted by default)](https://platform.claude.com/docs/en/build-with-claude/thinking)
- [Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
- [Codex v0.145 release (/import)](https://github.com/openai/codex/releases/tag/rust-v0.145.0)
- [Codex — git worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees)
- [Codex — AGENTS.md](https://learn.chatgpt.com/docs/agent-configuration/agents-md.md)
- [Codex — plugins](https://learn.chatgpt.com/docs/plugins)
- [Codex — pricing](https://learn.chatgpt.com/docs/pricing)
- [OpenAI — work with Codex from anywhere](https://openai.com/index/work-with-codex-from-anywhere/)
- [Gemini CLI — session management](https://geminicli.com/docs/cli/session-management/)
- [Antigravity terms](https://antigravity.google/terms/)
- [Zed — external agents (ACP)](https://zed.dev/docs/ai/external-agents)
- [VS Code — third-party agents](https://code.visualstudio.com/docs/copilot/agents/third-party-agents)
- [Warp — third-party agents](https://docs.warp.dev/agent-platform/third-party-agents/overview)
- [AGENTS.md](https://agents.md)
- [Agent Skills](https://agentskills.io)
- [Agent Plugins 1.0 (AAIF)](https://aaif.io/blog/from-skills-and-tools-to-portable-agent-plugins)
- [ACP agents](https://agentclientprotocol.com/overview/agents)
- [ATIF (Harbor)](https://docs.harborframework.com/agents/atif)
- [Claude Code AGENTS.md fallback (Simon Willison)](https://simonwillison.net/2026/Sep/18/thariq-shihipar/)
- [283-repo AGENTS.md/CLAUDE.md study](https://pub.towardsai.net/does-claude-code-read-your-agents-md-we-checked-283-repos-in-161-it-still-reads-claude-md-cb6199708fc1)
- [Traycer changelog](https://docs.traycer.ai/changelog.md)
- [Traycer usage limits & routing](https://docs.traycer.ai/agents-and-models/usage-limits-and-routing.md)
- [Traycer repo](https://github.com/traycerai/traycer)
- [Omnigent repo](https://github.com/omnigent-ai/omnigent)
- [Omnigent announcement (Databricks)](https://www.databricks.com/blog/introducing-omnigent-meta-harness-combine-control-and-share-your-agents)
- [Conductor](https://www.conductor.build)
- [Conductor pricing](https://www.conductor.build/pricing)
- [Conductor Series A](https://www.conductor.build/blog/series-a)
- [Superset](https://github.com/superset-sh/superset)
- [Emdash](https://github.com/generalaction/emdash)
- [Agent Deck](https://github.com/asheshgoplani/agent-deck)
- [Claude Squad](https://github.com/smtg-ai/claude-squad)
- [Vibe Kanban shutdown](https://vibekanban.com/blog/shutdown)
- [Happy](https://github.com/slopus/happy)
- [coder/agentapi](https://github.com/coder/agentapi)
- [Claudexor](https://github.com/razzant/claudexor)
- [AgentsRoom](https://agentsroom.dev/features/account-auto-switch)
- [awesome-agent-orchestrators](https://github.com/andyrewlee/awesome-agent-orchestrators)
- [Pi](https://github.com/badlogic/pi-mono)
- [OpenCode providers](https://opencode.ai/docs/providers/)
- [OpenClaw model failover](https://docs.openclaw.ai/concepts/model-failover.md)
- [CC Switch](https://github.com/farion1231/cc-switch)
- [rulesync](https://github.com/dyoshikawa/rulesync)
- [ruler](https://github.com/intellectronica/ruler)
- [Vercel skills CLI](https://github.com/vercel-labs/skills)
- [openskills](https://github.com/numman-ali/openskills)
- [CCS](https://github.com/kaitranntt/ccs)
- [Docker MCP Toolkit](https://docs.docker.com/ai/mcp-catalog-and-toolkit/get-started/)
- [Cross-tool plugin comparison (AWS Labs)](https://awslabs.github.io/aidlc-workflows/reference/research/Cross-Tool%20Plugin%20Comparison/)
- [cli-continues](https://github.com/yigitkonur/cli-continues)
- [casr](https://github.com/Dicklesworthstone/cross_agent_session_resumer)
- [cass](https://github.com/Dicklesworthstone/coding_agent_session_search)
- [SpecStory pricing](https://specstory.com/pricing)
- [claude-mem](https://github.com/thedotmack/claude-mem)
- [Basic Memory pricing](https://basicmemory.com/pricing)
- [Memorix](https://github.com/AVIDS2/memorix)
- [Letta Code](https://github.com/letta-ai/letta-code)
- [claude-nomad](https://github.com/funkadelic/claude-nomad)
- [Amp handoff](https://ampcode.com/news/handoff)
- [Handoff Debt (arXiv 2606.02875)](https://arxiv.org/html/2606.02875v2)
- [Factory compression eval](https://factory.com/news/evaluating-compression)
- [Chroma — context rot](https://trychroma.com/research/context-rot)
- [Anthropic — effective harnesses for long-running agents](https://anthropic.com/engineering/effective-harnesses-for-long-running-agents)
- [Session version-skew issue #68489](https://claudeissues.com/issue/68489-sessions-created-in-older-claude-code-versions-silently-fail-to-load-ui-shows-no)
- [claude-swap](https://github.com/realiti4/claude-swap)
- [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI)
- [better-ccflare](https://github.com/tombii/better-ccflare)
- [Anthropic multiple Max accounts comment](https://x.com/trq212/status/2024230184287949207)
- [The Register — Feb 2026 clarification](https://www.theregister.com/software/2026/02/20/anthropic-clarifies-ban-on-third-party-tool-access-to-claude/5014546)
- [The Register — Apr 2026 third-party harness change](https://theregister.com/2026/04/06/anthropic_closes_door_on_subscription)
- [Zed — Anthropic subscription changes](https://zed.dev/blog/anthropic-subscription-changes)
- [Gemini CLI ban discussion #20632](https://github.com/google-gemini/gemini-cli/discussions/20632)
- [claude-auto-resume](https://github.com/terryso/claude-auto-resume)
- [autoclaude](https://github.com/henryaj/autoclaude/)
- [TaskWake](https://github.com/Mira190/TaskWake)
- [Temporal Agent Harness](https://temporal.io/blog/temporal-agent-harness-durable-agent-infrastructure)
- [Claude Agent SDK session storage](https://code.claude.com/docs/en/agent-sdk/session-storage.md)
- [Docker Sandboxes](https://docs.docker.com/ai/sandboxes/agents/)
- [Dagger container-use](https://github.com/dagger/container-use)
- [kubernetes-sigs/agent-sandbox](https://github.com/kubernetes-sigs/agent-sandbox)
- [kagent AgentHarness](https://kagent.dev/docs/kagent/concepts/agent-harness/)
- [Kelos](https://github.com/kelos-dev/kelos)
- [Plural Agent Runtime](https://www.plural.sh/blog/introducing-the-plural-agent-runtime/)
- [Rivet Sandbox Agent](https://rivet.dev/sandbox-agent/docs/)
- [Upsun — worktrees for parallel agents](https://developer.upsun.com/posts/ai/git-worktrees-for-parallel-ai-coding-agents)
- [codeburn](https://github.com/getagentseal/codeburn)
- [ccusage](https://github.com/ccusage/ccusage)
- [CodexBar](https://github.com/steipete/CodexBar)
- [tokscale](https://github.com/junhoyeo/tokscale)
- [Claude Code Usage Monitor](https://github.com/Maciek-roboblog/Claude-Code-Usage-Monitor)
- [agentsview](https://github.com/kenn-io/agentsview)
- [Langfuse — coding agent tracing](https://langfuse.com/resources/engineering/coding-agent-tracing)
- [git-ai](https://github.com/git-ai-project/git-ai)
- [Pragmatic Engineer — AI tooling 2026](https://newsletter.pragmaticengineer.com/p/ai-tooling-2026)
- [Ramp AI Index](https://ramp.com/data/ai-index-february-2026)
- [Anthropic Series G (Claude Code run-rate)](https://www.anthropic.com/news/anthropic-raises-30-billion-series-g-funding-380-billion-post-money-valuation)
- [HN — OpenClaw subscription thread](https://news.ycombinator.com/item?id=47633396)
- [HN — weekly rate limits](https://news.ycombinator.com/item?id=44713757)
- [HN — Ctx cross-harness resume](https://news.ycombinator.com/item?id=47836740)
- [ImpossibleBench](https://arxiv.org/abs/2510.20270)
- [Anthropic Sonnet 4.5 system card](https://www-cdn.anthropic.com/963373e433e489a87a10c823c52a0a013e9172dd.pdf)
- [METR — recent reward hacking](https://metr.org/blog/2025-06-05-recent-reward-hacking)
- [SpecBench](https://arxiv.org/abs/2605.21384)
- [Agent over-mocking (MSR 2026)](https://arxiv.org/abs/2602.00409)
- [Agentic PR test coverage](https://arxiv.org/html/2607.18057v1)
- [Agent-written test assertions](https://arxiv.org/html/2607.12068v1)
- [358 PRs study](https://dev.to/cherven/358-pull-requests-that-changed-tests-agents-rarely-weakened-them-they-bent-the-code-instead-3ld)
- [Gauntlet](https://github.com/matthewjones372/gauntlet)
- [Skylos](https://pypi.org/project/skylos/)
- [Skylos — testwashing](https://skylos.dev/blog/testwashing-coding-agents-delete-tests)
- [AgentTestGuard](https://github.com/SefaYilmaz0/AgentTestGuard)
- [ai-test-integrity-review](https://github.com/muhammadmirza97/ai-test-integrity-review)
- [TDD Guard](https://github.com/nizos/tdd-guard)
- [mutmut](https://pypi.org/project/mutmut)
- [Arcmutate](https://www.arcmutate.com/)
- [Meta ACH](https://arxiv.org/pdf/2501.12862)
- [mutahunter](https://github.com/codeintegrity-ai/mutahunter)
- [Mutation testing for agent-written code](https://www.awesome-testing.com/2026/08/mutation-testing-for-agent-written-code)
- [Protect the oracle from the agent](https://agentpatterns.ai/patterns/agent-design/protect-the-oracle-from-the-agent/)
- [Claude Code #45550](https://github.com/anthropics/claude-code/issues/45550)
- [Claude Code #94170](https://github.com/anthropics/claude-code/issues/94170)
- [Loupe](https://github.com/smughead/Loupe)
- [Peekaboo](https://github.com/steipete/Peekaboo)
- [Codex Appshots](https://learn.chatgpt.com/docs/appshots)
- [Claude Code computer use](https://code.claude.com/docs/en/computer-use.md)
- [Claude Desktop iOS Simulator](https://code.claude.com/docs/en/desktop-ios-simulator.md)
- [Remarc](https://github.com/metedata/Remarc)
- [Deiko](https://deiko.app)
- [swift-agentation](https://github.com/ertembiyik/swift-agentation)
- [XcodeBuildMCP](https://github.com/getsentry/XcodeBuildMCP)
- [AXe](https://github.com/cameroncooke/AXe)
- [React Grab](https://github.com/aidenybai/react-grab)
- [Agentation](https://github.com/benjitaylor/agentation)
- [Stagewise](https://github.com/stagewise-io/stagewise)
- [Cursor 3.0 Design Mode](https://cursor.com/changelog/3-0)
- [ChatGPT/Codex browser annotate](https://learn.chatgpt.com/docs/browser.md?surface=app)
- [Jam pricing](https://jam.dev/pricing)
- [Chrome DevTools MCP](https://github.com/ChromeDevTools/chrome-devtools-mcp)
- [AXUIElementCopyElementAtPosition](https://developer.apple.com/documentation/applicationservices/1462077-axuielementcopyelementatposition)
- [Apple DTS sandbox AX answer](https://developer.apple.com/forums/thread/756130)
- [Electron accessibility](https://www.electronjs.org/docs/latest/tutorial/accessibility)
- [Indragie Karunaratne — macOS app built by Claude Code](https://www.indragie.com/blog/i-shipped-a-macos-app-built-entirely-by-claude-code)
- [oh-my-openagent](https://github.com/code-yeongyu/oh-my-opencode)
- [oh-my-openagent stats (OSS Metrics)](https://oosmetrics.com/repo/code-yeongyu/oh-my-openagent)
- [OpenCode agents](https://opencode.ai/docs/agents/)
- [Cline plan and act](https://docs.cline.bot/core-workflows/plan-and-act)
- [Factory mixed models](https://docs.factory.com/cli/configuration/mixed-models)
- [Traycer models](https://docs.traycer.ai/tasks/models)
- [VS Code custom agents](https://code.visualstudio.com/docs/copilot/customization/custom-agents)
- [Claude Code Router](https://github.com/musistudio/claude-code-router)
- [PAL MCP clink](https://github.com/BeehiveInnovations/pal-mcp-server/blob/main/docs/tools/clink.md)
- [codex-plugin-cc](https://github.com/openai/codex-plugin-cc)
- [delegate-agy](https://github.com/davdittrich/delegate-agy)
- [Cursor Router](https://startdebugging.net/2026/07/cursor-router-makes-auto-a-per-request-model-decision/)
- [Warp custom routers](https://docs.warp.dev/agent-platform/inference/custom-routers/)
- [Amp oracle](https://ampcode.com/news/gpt-5.4-the-new-oracle)
- [Databricks coding agent model selection](https://docs.databricks.com/aws/en/ai-gateway/coding-agent-model-selection)
- [LLMRouterBench](https://arxiv.org/html/2601.07206v1)
- [ACRouter](https://www.alphaxiv.org/abs/2606.22902)
- [FeatureBench](https://arxiv.org/html/2602.10975v1)
- [Greptile — model inversion](https://greptile.com/blog/model-inversion)
- [Diverse agents study](https://arxiv.org/abs/2602.03794)
- [Aider architect mode](https://aider.chat/2024/09/26/architect.html)
- [Kilo review test](https://blog.kilo.ai/p/review-testing)
- [Factory review benchmark](https://docs.factory.com/benchmarks/review-benchmark)
- [Senior SWE-bench refresh](https://senior-swe-bench.snorkel.ai/blog/2026-07-30-refresh)
- [Gemini 3.6 Flash release (TechCrunch)](https://techcrunch.com/2026/07/21/google-releases-three-new-gemini-models-but-no-3-5-pro/)
- [Gemini 3.8 Flash](https://www.deccanchronicle.com/technology/google-launches-gemini-38-flash-with-enhanced-reasoning-1984543)
- [Reasoning can't change models](https://www.digitalapplied.com/blog/reasoning-that-cannot-change-models-openai-google-anthropic)
- [Sign in with ChatGPT (The New Stack)](https://thenewstack.io/sign-in-with-chatgpt/)
- [Google forum — Gemini/Antigravity bans](https://discuss.ai.google.dev/t/urgent-mass-403-tos-bans-on-gemini-api-antigravity-for-open-source-cli-users-paid-tier/124508)
- [Claude Code #38698](https://claudeissues.com/issue/38698-feature-per-agent-model-provider-routing-e-g-local-ollama-for-subagents-anthropi)
- [everything-claude-code](https://agentskill.work/en/skills/affaan-m/everything-claude-code)
- [claude-code-templates](https://agentconn.com/skills/claude-code-templates)
- [trailofbits/claude-code-config](https://gittrend.io/repo/trailofbits/claude-code-config)
- [awesome-cli-coding-agents](https://github.com/bradagi/awesome-cli-coding-agents)
- [awesome-harness-engineering](https://github.com/ai-boost/awesome-harness-engineering)
