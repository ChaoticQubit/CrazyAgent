# Logging evidence and architecture boundaries

Evidence synthesized October 17, 2026 from `/private/tmp/crazyagent-memory-logging-research/notes.md`. No live provider session, model inference, installation or benchmark was performed for this report. The source notes distinguish documented API/protocol behavior from unqualified native CLI behavior.

## Primary evidence

| Source | Supported claim | Limit |
|---|---|---|
| [OpenAI reasoning](https://developers.openai.com/api/docs/guides/reasoning) | Private reasoning tokens are unavailable through Responses; usage counts, summaries and encrypted replay state are distinct. | Encrypted state is not readable reasoning or reconstructable private tokens. |
| [Codex app-server](https://learn.chatgpt.com/docs/app-server) | Structured lifecycle, message, tool/output, collaboration and reasoning-summary events exist; reasoning-text support is model-dependent. | Parent subscriptions do not establish complete child coverage; qualify completion, cancellation, replay and reconnects. |
| [Claude thinking](https://platform.claude.com/docs/en/build-with-claude/thinking) | Thinking display is summarized; opaque signatures/redacted blocks are protocol artifacts. | No display setting exposes raw chain of thought; visible text differs from billed thinking tokens. |
| [Claude SDK streaming](https://code.claude.com/docs/en/agent-sdk/streaming-output) and [hooks](https://code.claude.com/docs/en/hooks) | Main incremental streams exclude sub-agent token deltas; complete messages support parent attribution; hooks expose transcript paths, including sub-agent paths. | Combine and qualify supported surfaces; hooks/transcripts are not automatically tamper-proof. Async hook survival is bounded by session teardown behavior. |
| [Claude usage](https://code.claude.com/docs/en/agent-sdk/cost-tracking) | Top-level usage excludes sub-agent tokens; broader per-model/cumulative estimates include them. | Deduplicate by message ID; local cost estimates need not match billing. |
| [LangGraph memory](https://docs.langchain.com/oss/python/concepts/memory) | Background memory formation and namespace/key organization are established patterns. | Delayed formation trades freshness for hot-path independence; no framework integration is required. |
| [SQLite WAL](https://sqlite.org/wal.html) | Readers and one writer can operate concurrently on a local host. | Checkpoint management and workload testing remain necessary; WAL is neither a complete job queue nor unlimited write scalability. |

## Proposed synthesis

Rust owns structured capture, capability/coverage indicators and durable job state. Project curators run cursor-based asynchronous batches or handle direct remember submissions. Separate archives, candidates, versioned project memories and global nominations. Bound inference budgets and queue resources. Preserve provenance, uncertainty, conflict/freshness handling and correction links. Apply project isolation outside model prompts; the global curator receives explicit nominations rather than access to all project logs. Filter secrets before ordinary storage and distillation; define restricted originals, encryption and retention policy separately.

## Accepted and pending authority

[DEC-020](../../docs/agent-decisions.md#dec-020-log-derived-project-and-global-memory-agents) records the owner's project/global hierarchy. The owner subsequently approved the architecture and human review inbox in [DEC-021](../../docs/agent-decisions.md#dec-021-approve-asynchronous-memory-architecture-and-human-review-inbox), then selected Q-014 A in [DEC-022](../../docs/agent-decisions.md#dec-022-immediate-provisional-project-memory-and-approved-global-promotion): curated project knowledge is immediately retrievable as provisional with human keep/edit/remove controls, while each global promotion requires prior human approval. [DEC-023](../../docs/agent-decisions.md#dec-023-approved-cloud-or-local-destinations-for-memory-processing) records Q-015 A: cloud or local memory models can process authorized excerpts within approved destinations and resource authority; an unapproved destination needs approval. [DEC-024](../../docs/agent-decisions.md#dec-024-retain-captured-activity-until-explicit-deletion-by-default) records Q-016 A: retain captured activity until explicit deletion by default, with optional configurable expiry and visible storage usage. Corrections can prevent future retrieval and notify workers, but cannot erase already consumed context. Runtime capture coverage, storage-pressure behavior, throughput and implementation mechanisms remain unqualified; the numbered decision register tracks remaining policy questions.

Native subscription-authenticated Claude Code/Codex capture, child completeness, transcript retention, latency, throughput, storage growth, model cost and memory quality remain unqualified. No universal private-reasoning capture or numeric performance claim is supported.
