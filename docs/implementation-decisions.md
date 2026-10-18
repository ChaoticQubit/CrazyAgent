# Implementation decisions

This log records architecture and function-level implementation rationale. The owner/product authority register is docs/agent-decisions.md. Stable IMP IDs survive revisions; superseded decisions name their replacements. Each record distinguishes observed evidence from retrospective inference.

## Record contract

Each meaningful decision records date/status and task, affected files/symbols, the problem and constraints, the chosen approach and rationale, alternatives and their pros/cons/costs, verification evidence, risks and revisit conditions. A symbol coverage index maps every production function to relevant decisions. One decision may cover several functions when they share the same rationale.

Workers record new choices as they make them in their own .local-state/implementation-decisions.md. The leader consolidates those records here before integrating each task. The documentation worker initially owns this file for Task 1; the leader resumes ownership when that worker finishes.

## IMP-001: Separate product authority from implementation rationale

Date: October 17, 2026. Status: accepted implementation workflow under DEC-067. Task: continued V1 implementation. Scope: docs/agent-decisions.md, this file and worker briefs.

Problem: Product approvals and detailed function design have different evidence and change frequencies. A single undifferentiated chat history loses both.

Decision and rationale: Retain numbered DEC records for owner/product authority and maintain IMP records here for technical choices. Preserve cross-links, stable identifiers and a production-symbol coverage index. Label reconstructed Task 1 rationale as retrospective wherever contemporaneous intent was not recorded.

Alternatives and tradeoffs: One combined register has fewer files but makes detailed implementation changes obscure product authority. Code comments place rationale near statements but violate the owner's no-comment rule and distribute context across files. Unstructured chat is cheap initially but expensive to recover and cannot prove a decision's current status. The chosen split costs maintenance of links and the coverage index.

Verification and revisit: The leader checks the record and symbol map before task integration. Revisit layout if the log becomes difficult to navigate, preserving IDs and history rather than deleting records.

## IMP-002: Keep unqualified native workflows unavailable while building independent code

Date: October 17, 2026. Status: implementation boundary under DEC-059, DEC-062 and DEC-066. Task: native qualification. Scope: qualification admission, execution boundary and subsequent independent engine work.

Decision and rationale: Continue real source work and tests using existing tools while resolving missing runtime acquisition and account authority. Do not turn missing dependencies, synthetic capability claims or skipped native tests into passes. Run actual native probes after their prerequisites and authority exist.

Alternatives and tradeoffs: Stopping all source work wastes independent development capacity. Treating a mocked runtime as qualified gives faster green output but loses the accepted security and recovery guarantee. The chosen approach requires explicit separation of implemented behavior, native evidence and remaining gaps, and may expose a schedule change when qualification fails.

Verification and revisit: Existing Task 1 tests establish admission behavior only; native capability outcomes remain unavailable. Revisit the runtime choice on concrete failed boundary evidence rather than weakening launch requirements silently.
