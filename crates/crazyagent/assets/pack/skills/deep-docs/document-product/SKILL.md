---
name: document-product
description: >
  Comprehensive documentation for a whole codebase or product, as one navigable markdown
  deliverable with mermaid diagrams: architecture high and low, every subsystem, stack
  rationale, data flows, custom config/DSL, local and CI/CD setup, testing, debugging
  runbooks, per-stakeholder guides, FAQs, done-vs-pending. Use on "document this
  product/codebase", "write architecture docs", "technical handoff or onboarding doc",
  "refresh the product docs".
metadata:
  version: "0.1.0"
---

# Document Product

Produce a deep, client-grade documentation set for a finished (or in-progress) codebase. The reader — a new engineer, an integrating developer, a QA lead, an operator, or a business stakeholder — should be able to open the result and understand *what the system is, how every part works, why it was built that way, how to run and extend it, and what is done versus pending*, without reading the source.

This skill is **general-purpose**. Do not assume the project's language, framework, or shape. Discover what is actually there, name things using the repo's own vocabulary, and include only the sections and diagrams that apply. The subsystem "archetypes" referenced below (orchestrators, DAG schedulers, agent runtimes, plugin systems, config/DSL engines, dedicated services, reporting, discovery) are a *menu to recognize*, never a required checklist.

## Operating principles

- **Ground every claim in the code.** Read the source. When you state how something works, it must be traceable to specific files/functions. Cite `path:line` or `path/module` inline. Never invent behavior.
- **Explanation-first prose.** Write in full, explanatory sentences ("The scheduler resolves dependencies by topologically sorting the node graph, so that…"), not terse bullet fragments. This is documentation a client will read, not a checklist.
- **Diagram almost everything.** Every architectural layer, every major flow, every lifecycle, and the data model get a mermaid diagram. Diagrams must be *technical and specific to this system* — real component names, real steps — not generic boxes.
- **Adapt to reality.** If the project has no DSL, skip the DSL tutorial. If it has three services, write three service deep-dives. If it uses vocabulary like "collectors" instead of "agents", use *their* word.
- **Be honest about status and unknowns.** Mark anything you could not verify as `⚠ unverified`. Distinguish shipped behavior from stubs, TODOs, and roadmap.
- **Progressive detail.** Lead each section with the big picture, then drill down. A busy reader gets value from the first paragraph; a deep reader gets the internals.

## Inputs to gather first

Before writing, locate and skim:

1. **The source tree** — the primary target. Get its root path (a folder, a cloned repo, or a connected project directory).
2. **Existing docs** — `README*`, `CLAUDE.md`, `docs/`, ADRs, design notes, wikis. Treat these as *claims to verify*, not ground truth.
3. **Build & CI config** — package manifests, lockfiles, `Dockerfile`, compose/k8s/terraform, `.github/workflows`, `.gitlab-ci.yml`, `Jenkinsfile`, `Makefile`, task runners.
4. **A code graph, if one exists** — for example an export from Graphify or any tool that emits nodes (symbols/files/modules) and edges (calls/imports/deps). If present, ingest it to drive the architecture and dependency diagrams. See the discovery playbook for how.

If the target, audience, or depth is ambiguous **and the user is present**, ask a couple of focused questions (primary audience? handoff vs onboarding vs client delivery? single file vs a linked docs set?). If working **unattended**, choose the most reasonable interpretation, state it at the top of the document, and proceed.

## Workflow

Work the phases in order. Load each reference file (below) at the phase that needs it — do not preload everything.

### Phase 1 — Discover the codebase

Follow `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/discovery-playbook.md`.

Establish ground truth: languages and their proportions, entry points, module/package boundaries, dependencies, configuration surface, tests, infrastructure, and external services. Ingest the code graph if one was provided. Produce an internal **system map**: a list of every subsystem with its responsibility, its key files, and its relationships to the others. Reconcile existing-doc claims against the code and note discrepancies.

### Phase 2 — Classify the subsystems

For each item in the system map, recognize its **archetype** and record the repo's real name for it. Common archetypes: request/API gateway, orchestrator or pipeline engine, task/DAG scheduler, agent/worker runtime, plugin or extension system, configuration or DSL engine, a dedicated service (device/hardware/integration/edge layer), data store, reporting/analytics/rendering, discovery/registry/service-location, background jobs, UI/client. This classification decides which deep-dive template and which diagrams each subsystem gets. Include only what exists.

### Phase 3 — Draft the document

Follow the canonical outline in `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/documentation-outline.md` and fill the skeleton at `${CLAUDE_PLUGIN_ROOT}/skills/document-product/assets/report-skeleton.md`. Draft section by section:

- **Overview, context, and high/low-level architecture** — what it does, who it's for, and the layered picture.
- **Subsystem deep-dives** — one per subsystem, using the repeatable deep-dive template (responsibility → key files → step-by-step mechanics → data structures → interfaces → failure modes → extension points).
- **Tech stack with rationale** — for each technology: what it is, why it was chosen here, its role in *this* codebase, and trade-offs. Use the table format in the outline.
- **End-to-end data flow** — trace the primary journey (e.g. input/prompt → processing → output/report) across the system, and per-service where they differ.
- **Config & custom DSL** — if the project has a config language, DAG YAML, or bespoke DSL, document it fully and teach authoring from scratch using `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/dsl-documentation-guide.md`.
- **Setup** — local dev and production/CI-CD, via `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/setup-and-cicd.md`.
- **Testing & debugging** — strategy, how to run and write tests, and failure/troubleshooting runbooks, via `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/testing-and-debugging.md`.
- **Stakeholder perspectives** — tailored sections per persona, via `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/stakeholder-views.md`.
- **Status, FAQ, glossary** — done vs pending via `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/status-and-gaps.md`, plus an FAQ and glossary.

### Phase 4 — Generate diagrams

For every section that warrants one, author a mermaid diagram using `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/mermaid-cookbook.md`. At minimum produce: a system-context diagram, a high-level container/component diagram, a low-level component/class diagram for the core, a sequence diagram for each major flow, an end-to-end data-flow diagram, a state diagram for each important lifecycle, an ER diagram for the data model, a dependency/DAG diagram if a pipeline exists, and deployment + CI/CD diagrams. Keep each diagram legible (split when it exceeds ~20 nodes).

### Phase 5 — Assemble

Combine into one navigable markdown file with a title block, a "how to read this document" note, an audience map, and a linked table of contents. Default to a **single file**; for very large systems, emit a `docs/` set of cross-linked files with an index. Deliver the file(s) to the user.

### Phase 6 — Verify

This step is mandatory. Do all of:

1. **Validate every mermaid diagram.** Use the Mermaid MCP if connected, else a local `mmdc`, else the syntax rules in the cookbook. Fix anything that will not render.
2. **Fact-check.** Spot-check a sample of `path:line`/behavior claims against the actual code. Correct or flag mismatches.
3. **Status honesty.** Confirm the done-vs-pending section reflects real signals (stubs, TODOs, skipped tests, flags) and that unverified items are marked.
4. **Completeness sweep.** Ask "what would each stakeholder still not know?" and fill the gaps.

Record verification as a short "Documentation confidence" note at the end (what was verified, what remains unverified, and why).

## Output quality bar

The deliverable is done when a competent engineer who has never seen the code could, from the document alone: explain the architecture and each subsystem, run it locally and in CI, extend it (write a config/DSL file, add a plugin, add a service), test it, and debug the common failures — and when a business stakeholder could explain what it does and its current status. Every major flow and structure has a rendering, technically accurate mermaid diagram.

## Refresh mode

If a documentation set already exists and the user wants it updated: read the current docs, re-run discovery, diff the system map against what the docs describe, and update only the changed sections and their diagrams. Preserve structure and voice. Note in the change log what moved from pending to done.
