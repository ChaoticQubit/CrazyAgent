# RCLI host readiness

Date: October 18, 2026. Status: frozen production dependencies acquired; source bundle and isolated help path pass. Native leadership, provider authentication, persistent control and live recovery remain unqualified. This document refines Task 3 preparation without changing the accepted RCLI reuse decision.

## Current acquisition and build evidence

DEC-068 covers required V1 dependency acquisition. Existing Bun 1.4.3 ran the actual frozen production install with explicit registry.npmjs.org, project-local cache/configuration, --ignore-scripts and --no-save under the installed runtime. The environment was cleared and only explicit tool/configuration values supplied. The source package.json, bun.lock and bunfig.toml remained byte-identical. No upstream prepare/build lifecycle script ran. Bun reported 172 installed packages; all thirteen direct production roots are present. The filesystem inventory records 176 installed package instances, including nested versions, whose name/version keys all occur in the frozen lock. These counts describe different tool/filesystem views and are not treated as interchangeable. The dry-run printed development entries too, so it is not used as production-only installation proof.

The unchanged bun.lock SHA-256 is `f09a277fc534604caf344cf04117fa8306a549d76576c714219f7312e8e97281`. The private installation inventory retains package-manifest hashes and corresponding lock integrities at `.local-state/rcli-acquisition-20261018/inventory.json`, SHA-256 `d9a050f62a58af281846ce91901e8241070884d9ce3b1b2fad8be67c87f56e57`. Bun's lock/integrity enforcement is tool evidence; the inventory does not claim an independent revalidation of registry tarballs.

A direct Bun build of the pinned CLI entrypoint, with all network denied, bundled 1,347 modules into the 4.31 MB Bun-target JavaScript entrypoint. The build returned zero and preserved frozen inputs. Bundle `.local-state/rcli-acquisition-20261018/build/rcli.js` SHA-256 is `6ab5214c4f251566db49304219c2a2115b605df0ec786b975e4f249b0f235d5b`. Its built --help path then returned zero in 0.45 seconds under a separate no-network profile denying home reads except the isolated source/tool/configuration directories. Bun environment-file loading and auto-install were explicitly disabled. This is only a help probe; it does not exercise a model turn or prove full credential confinement.

Raw dry-run, install, build and help outputs are retained under `.local-state/rcli-acquisition-20261018/logs/`. The install, build and help log hashes are `10437fd11590e9dd18f012a0bfa54f7486a68d2e89f969d968e8ca143470bfdd`, `20665a9f0945cc4e13fe6d51028ebf6b37ef2da77be4d4ee8db0f236b205564f`, and `0b00e5a590d9ce1fcba171707801a13b8af9dbead7d48fc59705da86b019b190`. No global installation/configuration, actual provider call, login, account connection or real credential read was authorized or used by these commands.

Bun documents [configuration precedence](https://bun.com/docs/pm/npmrc) and [lifecycle-script controls](https://bun.com/docs/pm/lifecycle). The actual commands additionally used the local sandbox policy; these references do not replace measured application/runtime qualification.

## Original source inspection checkpoint

The October 17 source-only observations below are retained as historical preparation. Their statements that installation or execution had not yet happened describe that earlier checkpoint, superseded only by the bounded acquisition/build/help observations above.

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
