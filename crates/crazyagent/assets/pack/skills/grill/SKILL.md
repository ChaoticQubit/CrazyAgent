---
name: grill
description: Interview the owner one question at a time through CrazyAgent's ask tool, each question with its options and a recommended answer, until a plan or design is settled. Use before building or changing anything that carries a decision, or when asked to grill a plan.
---

# Grill

Settle a plan with the owner before the work starts, one decision at a time. The owner reads and answers your questions in CrazyAgent, so every question goes through the tool, never into your reply text.

## Before you ask

- Answer what you can yourself first: read the code, the project's context folder and its documents.
- Ask only what they cannot answer: a preference, a priority, a tradeoff the owner owns, or a fact only the owner knows.
- Never ask the owner to check something you can check.

## How to ask

- Ask only with the `ask` tool of the `crazyagent-team` MCP server. Never ask the owner in your reply text and never with your harness's own question tools.
- Ask one question per call, and wait for its answer before you ask the next one. A question stays open until the owner answers or dismisses it.
- Write the question so the owner can answer it without reading anything else.
- Put the choices in `options`, at most 6 short one-line ones, and the one you recommend in `recommended`. Give the reason for it in `context`.
- Use `context` for what the owner needs to know to answer: what the code or the documents show, and what each choice costs.
- Do not act on a decision while its question is open.
- The answer or dismissal comes back to you once. A sub-agent gets it typed into its terminal.
- If the owner answers you in chat instead and you have the `record_answer` tool, record it with the question's number, the version you read and the number of the owner's message. A master that gets its questions at the start of a turn finds both numbers there.
- If this conversation has no `ask` tool, ask in your reply instead, with the same shape: one question, its options, your recommendation and why.

## Order

1. List the decisions the plan needs.
2. Ask first about the decisions the others depend on.
3. After each answer, update the list: one answer can settle, remove or add other decisions.
4. Follow every branch until no decision is open.

If the owner dismisses a question, do not ask it again in this interview. Record it as open.

## Finish

Stop when every decision is settled or recorded as open, and you and the owner share one picture of the plan. Then summarize in your reply: each decision, the answer and the reason, and the questions left open.
