# Native harnesses, shared context and communication for one Rust engine

Research date: 2026-10-16. These are architecture recommendations and primary-source findings, not a qualified implementation. The user requires a primary TUI and a desktop application backed by the same Rust engine. macOS is the first release platform; Linux and Windows remain product targets.

## Which native integration paths should the shared Rust engine use?

### Takeaway

For a native-first primary TUI plus desktop on the current schedule, prefer one engine-owned native PTY process per Claude or Codex session. Qualify Codex app-server as the strongest structured integration alternative, with an explicit owner choice about reduced native UI breadth. Keep both harnesses behind one CrazyAgent project owner. Adopt ACP as an expansion option when a specific adapter passes capability and authentication qualification, rather than replacing the native harnesses with a new agent loop.

### Cited Findings

- The local installed `codex --help` and `codex app-server --help` advertise `stdio://`, `unix://`, explicit Unix socket paths and WebSocket listeners, plus a native TUI `--remote` option. This is a read-only local observation, not a live connection test or proof of stability. The official command reference independently documents those transports and explains that Unix sockets carry WebSocket handshakes. It also distinguishes managed remote control from a local app-server client. [Codex command reference](https://developers.openai.com/codex/cli/reference)
- App-server is the documented rich-client integration surface. It has initialization, thread creation/resumption, history pagination, turn start/steer/interrupt, native approvals, model selection and account login/status/logout. Local stdio is its default. Its WebSocket transport is explicitly experimental and unsupported; non-loopback listeners allow unauthenticated connections by default during rollout unless configured otherwise. The documented auth choices include capability-token files and signed bearer tokens. Overload uses bounded queues and retryable `-32001` responses. These facts support a narrow stdio adapter first, not exposure of an unauthenticated network endpoint. [Codex App Server](https://developers.openai.com/codex/app-server/)
- Official Codex source includes a Rust app-server test client, but its manifest depends on several workspace crates including `codex-core`, protocol and telemetry. The protocol crate also has many workspace dependencies. These are useful reference implementations and types, not evidence of a small independently supported Rust client library. The repository is Apache-2.0. [Test client manifest](https://github.com/openai/codex/blob/main/codex-rs/app-server-test-client/Cargo.toml), [protocol manifest](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/Cargo.toml), [license](https://github.com/openai/codex/blob/main/LICENSE)
- Claude's current product-hosting conditions require an unmodified published binary, preserving its authentication methods, and direct end-user authentication/billing. The same legal page prohibits developers collecting or intermediating Claude.ai tokens and offering Claude.ai login in their own apps, while expressly allowing end users to sign into hosted unmodified Claude Code. Accepting the relevant commercial terms is part of that published hosting route. This supports native terminal hosting but does not establish that every SDK wrapper has identical rights. [Claude legal and compliance](https://code.claude.com/docs/en/legal-and-compliance)
- A newer official help article says Claude Agent SDK, `claude -p` and third-party app usage continue drawing from subscription limits while a previously announced SDK change is paused. This must be read alongside the product and credential restrictions above. It contradicts a blanket assertion that all programmatic subscription use is forbidden, but does not itself authorize an arbitrary distributed application's login architecture. [Use the Claude Agent SDK with your Claude plan](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan)
- Claude's documented `-p` mode supports JSON and streaming JSON, session continuation and resumption. Bare mode skips discovery of hooks, skills, plugins, MCP, memory and `CLAUDE.md`, with a limited additional-directory skill exception. It also explicitly skips subscription login, OAuth credentials and the system Keychain, using an API key/helper or a configured cloud provider instead. Without bare mode, print sessions can load repository hooks and MCP configuration without the normal interactive trust dialogue. SIGTERM leaves an unfinished turn and differs from interrupting it first. The page documents partially different bare runtime behavior before v2.1.286 and says bare mode will become the default for `-p` in a future release. [Claude programmatic mode, bare-mode section](https://code.claude.com/docs/en/headless#start-faster-with-bare-mode)
- The official ACP Rust SDK supports clients, agents and proxies. Its stable-v1 entry points are distinct from opt-in draft-v2 APIs; native MCP-over-ACP is also opt-in. Current main has `agent-client-protocol` 3.4.0, Rust 1.88 MSRV and Apache-2.0 licensing. Main is not automatically the package version to ship. [ACP Rust SDK README](https://github.com/agentclientprotocol/rust-sdk), [workspace manifest](https://github.com/agentclientprotocol/rust-sdk/blob/main/Cargo.toml)
- The old Zed Codex adapter points new users to `agentclientprotocol/codex-acp`. The new adapter starts Codex app-server and maps requests/events to ACP; it supports ChatGPT, API-key and custom gateway auth, permissions, history and usage. Its package is TypeScript, Apache-2.0, and has Bun binary-build targets for macOS, Linux and Windows. It is a sidecar option, not a Rust library. Do not carry the superseded adapter's paid-only or remote-project restrictions forward without checking the new implementation. [New Codex ACP adapter](https://github.com/agentclientprotocol/codex-acp), [package and build targets](https://github.com/agentclientprotocol/codex-acp/blob/main/package.json), [old adapter migration notice](https://github.com/zed-industries/codex-acp)
- Claude ACP now lives at `agentclientprotocol/claude-agent-acp`. It wraps the Claude Agent SDK and exposes permission requests, messages, session operations and optional protocol extensions. It is Apache-2.0 TypeScript with Node >=22, not a native Rust implementation. Draft ACP v2 support is explicitly experimental. Its own capability list is not a provider entitlement grant. [Claude ACP adapter](https://github.com/agentclientprotocol/claude-agent-acp), [package manifest](https://github.com/agentclientprotocol/claude-agent-acp/blob/main/package.json)

### Inferences

Recommended integration boundary, distinguishing native-first v1 from the structured alternative:

| Component | Decision | Why and boundary |
| --- | --- | --- |
| Claude Code | Consume native binary as a managed PTY child | Preserves native prompts, login and harness behavior. The engine owns process/session metadata and terminal stream, not Claude's hidden reasoning. |
| Codex native-first v1 | Consume native interactive binary as an engine-owned PTY child | Preserves the provider UI and permission flow; qualify attach/detach and screen recovery. No structured tool-card parity is promised. |
| Codex structured alternative | Consume native app-server over private child stdio | Gives structured approvals, streaming, cancellation and history. Requires bounded custom views in both clients, a Rust JSON-RPC adapter and explicit unsupported-feature behavior. |
| Codex Rust workspace code | Reference or narrowly adapt with Apache notices | Reusing the entire internal workspace couples the application to a large moving dependency graph. Validate any chosen extraction before making it a dependency. |
| ACP | Defer mandatory dependency; qualify as the third-harness route | Stable-v1 Rust support is useful. Current Claude and Codex adapters add a TypeScript process, another protocol mapping and capability differences. |
| API-backed Claude SDK | Optional explicit mode after qualification | Can support a structured UI, but must visibly identify billing/auth and preserve user choice. Do not silently convert subscription sessions to API billing. |

The same engine events feed TUI and desktop. Their input components differ, but approval state, provider session IDs, project context and message delivery belong to the engine. Tauri should not become the owner of those rules. Closing either view should not terminate a run unless the owner explicitly chooses that lifecycle.

Using Codex's own Unix listener can simplify only the Codex transport. It cannot own Claude sessions, global/project context, app-level message identity or independent verification. A separate CrazyAgent session owner remains necessary. Nor does the existence of `codex --remote` mean that embedding its native TUI offers the same API stability as private stdio.

Avoid `--bare` in the normal subscription-backed shared-context Claude path because it suppresses required context and explicitly uses API/cloud authentication instead of subscription login. A clean verifier may need a deliberately different execution mode, but the coding session is not that verifier. The earlier research's bare-specific OAuth/Keychain statement is supported by the current documentation. Do not generalize this restriction to all non-bare `-p`, SDK or third-party usage. The exact pinned binary still needs qualification because the docs describe version-dependent behavior and a planned future default change.

### Gaps

- No account was accessed and no live inference was run. Subscription behavior, branding terms and distributed-app eligibility require exact-mode qualification. A successful personal experiment would not resolve every distribution question.
- No small official published Rust Codex client with an independently stable compatibility promise was established. The protocol is documented; the source crates are internal workspace components.
- PTY hosting leaves desktop structured tool-card parity incomplete for Claude. Scraping terminal text should not become authoritative approval or verification state. Detailed terminal emulator and TUI attach choices belong to the UI research and grilling.
- Exact minimum supported versions must be pinned from released binaries and artifacts before implementation. Repository main versions above are research snapshots, not tested dependency recommendations.

### Native UI scope clarification for the owner decision

The earlier direct-app-server recommendation optimizes structured integration, but it does not include a finished Codex interface. For the revised requirement of a primary TUI plus desktop on the existing release dates, rank a native Codex PTY first if preserving the native experience and delivery speed matter more than uniform tool cards. This is a UI and integration recommendation to grill with the owner, not a reversal of app-server's suitability as the eventual structured adapter.

| Choice | V1 benefit | V1 cost and limitation | Rank |
| --- | --- | --- | --- |
| One native Codex interactive CLI process in an engine-owned PTY | Native rendering, slash commands and permission dialogue remain in the provider UI. Both harnesses use the same transport shape. The focused TUI can forward native terminal I/O, while desktop uses its terminal renderer. | Programmatic tool state, exact approval status and transcript cards are not reliably available from terminal text. Attach, detach, resize and screen recovery still require real terminal qualification. Structured usage and native history browsing initially remain native UI functions where available. | First for schedule-constrained native-first v1, subject to PTY spike. |
| One Codex app-server child with bounded custom views in both clients | Structured session state, requests, output and permissions can be rendered consistently. The engine controls a documented protocol surface. | We must implement and qualify two presentation layers. This does not preserve all Codex TUI features, shortcuts, slash commands or terminal interaction. Native UI parity must not be implied. | Second unless the owner prioritizes structured controls over native UI breadth. |

The native CLI and app-server distinction follows the documented command surfaces; the ranking and delivery estimates above are architecture inferences. [Codex command reference](https://developers.openai.com/codex/cli/reference), [Codex App Server](https://developers.openai.com/codex/app-server/)

Minimum integrated v1 under the native PTY choice: create/open the shared project, select an explicitly configured native profile, launch and focus a harness, observe its terminal, send native input, forward interruption, detach/reattach, and recover through supported native resume. Project memory, effective configuration, visible mail and independent verification remain CrazyAgent panels/actions in both clients. Native approval stays in the native terminal; the app must not infer approval from scraped text. Opening desktop while the TUI is attached observes the same PTY and project state. One client holds the write/resize lease; the other observes until explicit transfer. Attaching a view never launches a second inference process.

Minimum bounded v1 under the structured app-server choice: select a model/profile, start or resume a thread, submit text, render streamed assistant output and supported tool events, show pending native approval requests and send supported decisions, interrupt the active turn, inspect a bounded paginated history, and surface unsupported requests without granting permission or losing the request. File/image inputs, custom elicitation forms, specialized plugins, browser/computer-use interfaces and full CLI command parity need an explicit compatibility list. A required unsupported permission path blocks the action rather than silently approving it.

Under the structured choice, do not open a second interactive `codex` process as a visual representation of the app-server conversation. That creates a second session unless a specific native remote-client attachment is qualified against the same server and thread. Current remote TUI flags suggest a possible future route, but they do not prove simultaneous same-thread UI interoperability or erase experimental transport caveats. Private app-server stdio cannot simply be attached to a second native TUI. Treat any hybrid native/structured view of one run as a separate spike, not included v1 behavior.

This choice must be an early owner decision because it changes the everyday experience and implementation scope. Recommended grilling question: should v1 preserve each native harness UI inside the integrated project, or replace Codex's UI with a smaller structured interface? Recommend native UI first on the current schedule, retain the app-server option for the next qualified adapter release, and keep all project integration outside provider UI so the transition does not split state ownership.

## How should identities, platforms and two-client recovery work?

### Takeaway

Use native identity profiles and a single engine-owned session registry. A UI reconnect restores an engine snapshot and subsequent events; it does not spawn a duplicate harness or replay a prompt blindly. Platform support must qualify each harness and execution mode, not merely compile the Rust UI on three operating systems.

### Cited Findings

- Claude explicitly documents simultaneous work/personal identities through separate `CLAUDE_CONFIG_DIR` directories, each with its own settings, history and Claude.ai login or API key. It warns that two Console sign-ins without API keys are not isolated by this directory because that credential is stored elsewhere. [Claude authentication](https://code.claude.com/docs/en/authentication)
- Codex uses local auth storage in `CODEX_HOME` or an OS credential store. Its configurable stores include file, keyring, auto and ephemeral; file mode uses `CODEX_HOME/auth.json`. Managed requirements can enforce the storage choice. A separate home alone does not prove every keyring namespace or concurrent logout case is isolated. [Codex authentication](https://developers.openai.com/codex/auth), [configuration reference](https://developers.openai.com/codex/config-reference)
- Claude supports native Windows and WSL. Git for Windows is now optional for native operation: without it, shell commands use PowerShell; with it, Git Bash is available. Its documented sandbox matrix says native Windows has no Claude sandbox, WSL2 supports sandboxing and WSL1 does not. This is a meaningful execution-security distinction even when the terminal UI runs. [Claude setup](https://code.claude.com/docs/en/setup)
- Current Codex Windows documentation supports native Windows operation and native sandbox implementations, with machine capability and policy prerequisites. It recommends native operation by default and WSL when Linux tooling or a suitable native sandbox is unavailable. The current page describes MXC plus legacy elevated/unelevated fallbacks; copying an older elevated-only description would be stale. [Codex Windows sandbox](https://developers.openai.com/codex/windows)
- Codex documents live thread state separately from persisted history, including paginated history APIs and resumption. Its account identity and model lists are provider-owned; resuming history is different from restarting a killed process or continuing every background child. [Codex App Server](https://developers.openai.com/codex/app-server/)

### Inferences

The engine should store a profile label and native configuration-root handle, selected harness executable/version, native session identifier, project/worktree identifier and observed lifecycle state. Never store consumer tokens in shared memory, project exports or a generic account-rotation database. Native login, refresh and logout stay with the provider binary. Selecting a work identity for a work project is distinct from automatically cycling accounts to evade limits.

Use one shared engine process as session owner with two local clients. A private local IPC endpoint is sufficient for the first release. On Unix, use a Unix-domain socket with explicit ownership/permissions; Windows requires a named-pipe or another qualified local transport. A loopback HTTP/WebSocket service needs origin validation, authentication and endpoint lifecycle handling; localhost alone is not authorization. Those choices require one short transport spike, not a remote cloud control plane.

Minimal reconnect contract, proposed for grilling:

1. A client requests the current authoritative project/session snapshot, including revision and last durable event sequence.
2. It subscribes from that sequence, accepting a clear resync response when history was compacted or missed.
3. Command IDs deduplicate mutating requests. A disconnected submitter can discover the accepted request before retrying.
4. Approval requests have one owner-visible identity and exactly one winning response. A second client sees the outcome or conflict instead of approving stale work.
5. Terminal input has one active writer lease per session. Other views can observe; resize and input authority transfer explicitly.

Bound event buffers and terminal history separately. A slow desktop must not block a coding subprocess indefinitely, lose the durable result or exhaust memory. Persist high-value state transitions before notifying clients, while using bounded streaming for transient output. Recovering a UI is an event/snapshot operation; recovering a dead harness is an explicit native resume operation with an honest interruption marker.

For v1, both interfaces need create/open project, choose native profile, start/observe/input/interrupt/resume a supported session, inspect memory/messages, answer native approvals where surfaced, request verification and see current/stale results. Nice tool-card rendering may differ. No interface may bypass the same engine authorization or start its own independent project owner.

### Gaps

Future runnable qualification checks, all unrun:

| Check | Required observation |
| --- | --- |
| Two native Claude profiles | Two owner-controlled identities operate concurrently; logout/refresh in one does not alter the other; history and permissions are correctly scoped. |
| Two native Codex homes | File/keyring choice and managed policy are recorded; concurrent login/logout/refresh isolation is demonstrated for each advertised mode. |
| Two frontends, one run | Attach TUI and desktop, disconnect/reconnect each, submit the same request ID twice, answer one approval twice, and observe one effect plus clear conflicts. |
| Process cancellation | Interrupt foreground work, a permission wait and a background command. Record exactly what stopped, remained alive and could be natively resumed. |
| Native Windows versus WSL2 | Verify executable path, shell, cwd/path translation, ConPTY behavior, named-pipe access, sandbox availability and permission prompts for each advertised combination. |

Cross-platform compilation is not execution qualification. In particular, a native Windows Claude session cannot inherit a nonexistent Claude sandbox. The independent verifier's real protection and the coding process's allowed filesystem/IPC reach must be designed and tested separately.

## What shared configuration, memory and messaging can be reused?

### Takeaway

Reuse Agent Skills for portable content, official Rust MCP for scoped tools, and Rulesync for a qualified import/export boundary. Build the small canonical project-state and mailbox policy in Rust because no inspected library makes the application's identity, authority, conflict and delivery decisions for it. Do not adopt another full Python or Electron host as a second engine underneath the required Rust engine.

### Cited Findings

- Agent Skills defines a directory containing `SKILL.md`, optional executable scripts, references and assets, with metadata including compatibility and per-skill licensing. It recommends progressive disclosure and supplies a reference validator. The format makes content portable; it does not guarantee an agent will honor every instruction or that bundled scripts are safe. [Agent Skills specification](https://agentskills.io/specification)
- The official MCP Rust SDK is `rmcp`, with Tokio-based client and server support, stdio and Streamable HTTP transports and negotiated protocol versions. Its current main workspace is Apache-2.0, edition 2024, MSRV 1.88 and version 3.5.1. MCP provides a tool protocol, not automatic cross-harness identity or trusted message routing. [Official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk), [workspace manifest](https://github.com/modelcontextprotocol/rust-sdk/blob/main/Cargo.toml)
- Rulesync is an MIT utility with a Node programmatic API and CLI, plus standalone distributions. Its current main package is 36.0.2. It imports and generates rules, skills, MCP configuration, commands, hooks and permissions across tools. This is a process/tool dependency or a licensed source adaptation, not a Rust crate. [Rulesync README](https://github.com/dyoshikawa/rulesync), [package manifest](https://github.com/dyoshikawa/rulesync/blob/main/package.json)
- Rulesync's detailed matrix separates project, global and simulated support. For example, Codex commands are global-only while Claude commands support both scopes. Claude and Codex rules, MCP and skills have both project/global entries, but this does not imply every native permission or plugin field is equivalent. [Rulesync supported tools](https://rulesync.dyoshikawa.com/reference/supported-tools)
- Omnigent offers a multi-client server with native harness sessions, terminal/web/desktop views and policy concepts under Apache-2.0. Its main package requires Python >=3.12 and includes a Python runtime/client stack. It provides concrete patterns to inspect, but adopting the runtime would add another engine language and authority owner. [Omnigent README](https://github.com/omnigent-ai/omnigent), [Python package manifest](https://github.com/omnigent-ai/omnigent/blob/main/pyproject.toml)
- Emdash is an Apache-2.0 agent development environment; its current package layout uses Electron/Node components, including `node-pty` and SQLite dependencies. Its project/worktree and terminal UX can inform implementation, but the complete application is not a Rust core library. Superset's license is Elastic License 2.0, so copying it as a permissive open-source foundation would require separate license analysis. [Emdash repository](https://github.com/generalaction/emdash), [Emdash package](https://github.com/generalaction/emdash/blob/main/package.json), [Superset license](https://github.com/superset-sh/superset/blob/main/LICENSE.md)

### Inferences

Build/reuse/defer matrix:

| Need | Reuse path | V1 recommendation and limits |
| --- | --- | --- |
| Portable skills | Agent Skills directories/specification | Reuse the format and existing user content. Preserve each skill's license and compatibility requirements. Do not invent a competing skill DSL. |
| MCP context and mail tools | Apache-2.0 `rmcp` crate | Build a small Rust tool surface on the official SDK; pin a released version and enable only needed transports/features. |
| Config import/export | MIT Rulesync CLI | Run a bounded spike on temporary fixtures and two native profiles. Keep it if it preserves unsupported fields and produces auditable scoped diffs. Avoid global in-place generation without owner review. |
| Runtime canonical state | Small Rust state module backed by selected local database | Own project/global facts, selected bundles, effective configuration revisions, session associations and conflict rules. This is product logic, not a new agent framework. |
| Native config adapters | Existing Rulesync transformations where qualified; otherwise two narrow native mappings | Do not port its complete target catalogue to Rust or promise lossless equivalence across all harnesses. |
| Cross-harness messages | Engine mailbox plus MCP tools | Durable bounded records with server-derived sender/project identity, recipient, delivery state and idempotency. Both UIs inspect the same records. |
| Existing full host applications | Architecture/UX references | Reuse isolated compatible modules only after dependency/license inspection. Do not run several complete hosts as the product spine. |
| Draft ACP v2, general conductor/proxy chains | Available upstream, not required | Defer unless one concrete accepted use case needs them. No automatic swarm or dynamic workflow approval is implied. |

Canonical global/project state should compile into a preview of what each harness actually receives. Keep scope, provenance, revision and target capability beside every supported value. Project overrides must not silently widen permissions inherited from trusted policy. Existing provider files and manual edits require a visible conflict result; generation should not overwrite unrelated sections. Windows profile directories need materialized copies or another qualified strategy rather than assuming Unix symlinks work identically.

Start memory with explicit versioned facts and decisions, searchable text and source links. Record proposals separately from accepted facts and retain supersession history. Build provider-specific read views from this state. Do not merge hidden chain-of-thought, entire transcripts and secrets into an undifferentiated global memory. Semantic embedding or a separate memory service is optional until plain search fails on real project tasks.

Start messaging with send/list/acknowledge operations. Sender and project scope come from the engine's session binding, not agent-supplied JSON. An agent can propose a message or fact, not authorize verifier rules or promote its own output to a trusted verdict. Include message IDs, bounded size/retention, recipient state and a visible transcript. Cross-project delivery and impersonation must reject.

Mailbox persistence does not itself guarantee model attention. MCP polling makes messages available when a harness checks them. A documented push/channel or native steering mechanism can notify a running harness only after testing that adapter's semantics and permission boundary. UI delivery, tool availability and agent acknowledgement should be separate visible states. Never claim immediate bidirectional inter-agent communication merely because both processes can read a shared file.

### Gaps

- Rulesync has not been run on this machine or against the owner's configuration. Import/export round-trips, environment/profile-root handling, permissions and unknown-field preservation remain acceptance checks, not assumed correctness.
- No inspected component provides a universal shared plugin binary/API across native harnesses. Common skills and MCP tool definitions are a realistic subset; provider plugins and native hidden state need explicit compatibility boundaries.
- No upstream mailbox or memory package was established as simpler than a few application-owned records. That is an architecture inference for v1, not a claim no such project exists.
- Before adopting source, record the exact revision, license/NOTICE obligations and transitive dependencies. Source licensing does not override provider service terms or make a compiled dependency secure.
