# Installed Codex transport contract inspection

Date: October 18, 2026. Scope: read-only installed CLI help and local protocol generation. This is source-contract evidence, not an admitted native session, account, recovery or child-control result.

## Frozen input and generated evidence

Installed binary: /opt/homebrew/Caskroom/codex/0.163.0/bin/codex, SHA-256 8813367285524ac798dd96d1b071aea30d58708c7226fd236cd275cf1cfdee31. The prior inventory observed codex-cli 0.163.0; this inspection uses that exact installation path rather than assuming the current PATH wrapper is equivalent.

Executed app-server --help, app-server daemon --help and app-server generate-json-schema --help with a cleared environment and explicit PATH/LANG. Then executed app-server generate-json-schema --out /Users/chaoticqubit/Projects/CrazyAgent/.local-state/codex-protocol-0.163.0/stable. All exited zero. The CLI warned that PATH aliases could not be created because the coding sandbox denied that write. No escalation, daemon start, bootstrap, restart, update, remote-control change, account method or provider/model turn was executed. Do not rerun these commands with broader authority merely to remove that warning.

The default, nonexperimental generation produced 318 JSON files. No generated file was hand-edited. The sorted tree digest is 89fe2000fd0efb67f607e6c5f619eea15e510fcace39ae247947b8343d3fff09, computed by hashing each relative path, a null byte, its lowercase SHA-256 and a newline, in sorted path order. The v2 aggregate schema SHA-256 is d204b96d8560d56600127423b89df9db471b3e22338e8809bb91d5a952998978. ClientRequest.json SHA-256 is 6b10888f163f569922d42c41e94a98b4261258abc7581c14eca867c3b125dc6f. Retained generator output is .local-state/codex-protocol-generation.log.

## Actual advertised contract

| Need | Local contract | Qualification limit |
|---|---|---|
| Structured local transport | app-server supports stdio://, unix://, unix://PATH, ws://IP:PORT and off | No server was started; authentication, attachment and permitted endpoint paths remain untested |
| Reattachment candidate | daemon management and a stdio-to-control-socket proxy are advertised | No existing daemon was inspected or changed; installed management may touch user-global state and is not automatically suitable for managed project runners |
| Thread identity and observation | thread/start, resume, fork, read, loaded/list, turns/list and items/list occur in the generated request union | An API name does not prove original live identity survives controller death |
| Turn control | turn/start, steer and interrupt; interrupt requires threadId and turnId | A request acknowledgement cannot be equated with observed child stop or complete cleanup |
| Native child relationships | SubAgentSource.thread_spawn carries parent_thread_id and depth; CollabAgentState carries status; tool values include spawnAgent, sendInput, resumeAgent, closeAgent, interruptAgent and listAgents | These are schema data/tool representations, not proof that every native child is independently controllable through every account/profile |

CollabAgentStatus includes pendingInit, running, interrupted, completed, errored, shutdown and notFound. Preserve these distinctions rather than flattening all terminal or missing states into success. ThreadLoadedListParams permits an omitted limit with no limit by default, so a managed adapter must always provide and enforce its own finite page size and total bound. Generated account/usage/rate-limit methods describe potential capabilities; no such method was invoked and no credential or billing route was inspected.

## Reuse direction and remaining gates

Prefer qualifying the installed structured app-server transport before building a custom terminal parser or a replacement model/tool loop. A private explicit Unix endpoint launched inside the surviving managed runner is the narrower candidate than automatically invoking global daemon bootstrap or remote-control management. This is an implementation direction under IMP-081, not permission to launch it or adopt a runtime.

Actual proof must still cover protected account/destination selection, bounded transport/capture, explicit resource limits, original harness/native connection continuity across engine death, controller epochs and operation deduplication, permission requests while the engine is down, per-child observation/control and cancellation races. The existing admission boundary remains closed until those real checks and the complete runtime profile pass. RCLI leader hosting and Claude transport require their own evidence; Codex schema support cannot satisfy those adapters.
