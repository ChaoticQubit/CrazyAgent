# RCLI host readiness

Date: October 17, 2026. Status: pinned source inspected; dependency execution and native leadership unqualified. This document refines Task 3 preparation without changing the accepted RCLI reuse decision.

## Immutable input and available tools

Backboard-io/Backboard-R-CLI revision `51bee17e02344de5fffd89523ce2b73d5042f216`, manifest 3.0.4, MIT metadata. IMP-019 records archive integrity and validation. The source is retained under ignored `.local-state/tools/rcli-source`; it has not been built or run. Installed Bun is 1.4.3. Its install help exposes frozen-lockfile, production, no-save, cache-dir and ignore-scripts controls. No Bun package install was performed for this source.

The upstream 83,540-byte `bun.lock` has 355 SHA-512 entries and no explicit HTTP/Git URL lines in the inspected text. This is an inventory, not a complete verified production closure. The package's `prepare` script changes Git hooks and must remain disabled. `scripts/build.ts` spawns `bun build --compile` for the CLI and can embed BACKBOARD_OAUTH_CLIENT_ID when supplied. No such environment value was accessed or supplied. `bunfig.toml` only configures exact-version installation behavior and the upstream test root.

## Observed transport and reuse points

At this revision, `src/entrypoints/cli.tsx:724` defines `runHeadless`: it awaits one `controller.submit(prompt)` and returns. The stdin helper at line 788 reads until EOF. `--format json` supplies output events; these observations do not establish a persistent bidirectional input/permission/cancellation protocol.

`src/core/session/JsonEventStream.ts` attaches to the actual EventBus and writes timestamp, session ID, numeric sequence, event type and payload as JSON lines. It has activation and detach operations. The sequence is local to this stream instance; it is not the engine's durable event cursor or an exactly-once dispatch ledger. Events cover what the provider/harness exposes and do not reveal hidden reasoning.

The CLI constructs the actual AgentController with configuration, EventBus, SessionStore, ToolRegistry, AgentClient, skill/hook controllers, language services, checkpoints, background supervision, durable-session callbacks and permissions. These are source-level reuse points; they are not a qualified standalone library interface. The package declares no public exports map.

`src/providers/createAgentClient.ts` constructs Backboard and BYOK clients and routes according to configured model, provider registry and available credentials. This establishes those source paths, not universal support for Claude Code or Codex subscription credentials. Native account compatibility must be tested separately through the relevant adapter/profile; no credential translation is assumed.

## Smallest qualification path

Retain the accepted RCLI controller/tool/context implementation. Investigate one dedicated persistent host entrypoint rather than treating repeated one-shot CLI launches as live-worker recovery or rewriting its agent loop in Rust. Expose only typed bounded input, observable events, explicit permission decisions, status and cancellation. Rust retains policy, routing, worktree, memory and verification authority; default shell/filesystem tools cannot bypass those boundaries.

Before source execution, inspect and freeze the actual production dependency closure using project-local isolated configuration and disabled lifecycle scripts. Trace the referenced constructors and startup hooks before choosing the host assembly. Then test two real turns, a real permission round trip, engine disconnect/reconnect with the original host identity, cancellation and logging failure against an admitted profile and an explicitly approved account/budget. No mock backend or transcript restoration can count as that native integration.

Remaining decisions: permanent execution runtime after DEC-069's experiment, and bounded account/profile authority. Remaining evidence: actual persistent transport, original live identity, provider auth, individual child control, permission behavior and capture coverage. These are not supplied by source inspection, a lockfile, a help response or a successful build.

## Primary source references

[Pinned CLI entrypoint](https://github.com/Backboard-io/Backboard-R-CLI/blob/51bee17e02344de5fffd89523ce2b73d5042f216/src/entrypoints/cli.tsx), [pinned JSON event stream](https://github.com/Backboard-io/Backboard-R-CLI/blob/51bee17e02344de5fffd89523ce2b73d5042f216/src/core/session/JsonEventStream.ts) and [pinned provider factory](https://github.com/Backboard-io/Backboard-R-CLI/blob/51bee17e02344de5fffd89523ce2b73d5042f216/src/providers/createAgentClient.ts) support the source observations. The proposed persistent host entrypoint is an implementation inference requiring real qualification.
