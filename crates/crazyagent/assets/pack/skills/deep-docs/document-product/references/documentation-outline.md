# Canonical documentation outline

This is the master structure for the deliverable. Adapt it to the target system: rename sections to the project's vocabulary, drop sections that do not apply, and repeat the subsystem template once per real subsystem. Sections marked **(conditional)** are included only when the corresponding thing exists.

Each section below lists: its **purpose**, **what to include**, **which diagram(s)**, and **where to mine the facts**. Write in explanatory prose. Prefer a strong opening paragraph per section, then detail.

---

## 0. Front matter

- **Title & one-line definition** of the product.
- **Document metadata**: version, date, source commit/branch, who/what generated it, and the audience.
- **How to read this document**: the audience map — which sections matter to which reader (link to the stakeholder section).
- **Table of contents** with anchor links.

Diagram: none. Optionally a single "big picture" context diagram teaser.

## 1. Executive / product overview

- **Purpose**: let anyone understand what the system does and why it exists in 3–5 minutes.
- **Include**: the problem it solves, who uses it, the core capabilities, the top-level shape (a few sentences naming the major pieces), and the primary end-to-end journey in one paragraph. Keep it non-jargony; this is the one section a business stakeholder reads fully.
- **Diagram**: system-context diagram (external actors and systems around the product).
- **Mine from**: README, product/marketing docs, top-level entry points, the main flow.

## 2. High-level architecture

- **Purpose**: the layered mental model of the whole system.
- **Include**: the major layers/services and how requests/data move between them; the responsibility of each; the key boundaries (process, network, trust); synchronous vs asynchronous paths; where state lives. Explain *why* the system is decomposed this way.
- **Diagram**: container/component diagram with subgraphs per layer/service; optionally a second diagram showing runtime topology (processes/hosts).
- **Mine from**: directory structure, service manifests, inter-service calls, deployment config, the code graph's high-degree hubs.

## 3. Low-level architecture

- **Purpose**: the internal structure of the core — the classes/modules and their relationships.
- **Include**: the principal modules of the most important service(s), their public interfaces, key abstractions (base classes, interfaces, protocols), and the dependency direction between them. Explain the design patterns in use (e.g. strategy, registry, pipeline, actor) and why.
- **Diagram**: component or class diagram; a module dependency diagram derived from imports/the code graph.
- **Mine from**: source of the core service, interfaces/abstract types, import graph.

## 4. Core concepts & domain model

- **Purpose**: define the nouns and verbs of the system before the deep-dives use them.
- **Include**: each core concept (what it is, its lifecycle, its relationships), and the ubiquitous language. This section seeds the glossary.
- **Diagram**: ER or class diagram for the domain entities; state diagram for any concept with a lifecycle.
- **Mine from**: domain models, schemas, ORM entities, core types.

## 5. Subsystem deep-dives — *repeat the template below once per subsystem*

Order subsystems along the primary data flow (entry → processing → output). For each, use this **repeatable template**:

### 5.x `<Subsystem real name>` (`<archetype>`)

- **Responsibility** — what it owns, in one paragraph. What it is *not* responsible for.
- **Where it lives** — key files/directories/packages, with `path` references and the main entry symbols.
- **How it works** — the step-by-step mechanics, in prose. Walk an actual operation from start to finish. Name the functions/classes involved. This is the core of the deep-dive; be specific and thorough.
- **Key data structures** — the important types/records this subsystem produces or consumes, with field-level notes for the non-obvious ones.
- **Interfaces & contracts** — how other subsystems call it (APIs, events, function signatures, queues), and what it calls out to.
- **Configuration** — what knobs affect it (link to the config/DSL section).
- **Concurrency & state** — threading/async model, shared state, ordering/idempotency guarantees, back-pressure.
- **Failure modes & handling** — what can go wrong, how it degrades, retries/timeouts/circuit-breakers (link to the debugging section).
- **Extension points** — how to add to it (a new handler, plugin, node type, strategy).
- **Diagram(s)** — pick per archetype:
  - orchestrator / pipeline engine → sequence + data-flow
  - task/DAG scheduler → DAG/dependency graph + state diagram for job lifecycle
  - agent/worker runtime → state diagram (agent lifecycle) + sequence (dispatch→result)
  - plugin/extension system → component diagram (host ↔ plugin boundary) + sequence (load/register/invoke)
  - config/DSL engine → flow (parse→validate→bind) + class diagram of the schema
  - dedicated service (device/hardware/integration) → its own container diagram + sequence to/from the core (give this its own top-level section too — see §6)
  - reporting/analytics/rendering → data-flow (raw → aggregated → rendered)
  - discovery/registry → sequence (register→lookup→resolve) + state (healthy/stale/evicted)

> **Archetype recognition hints** — orchestrator: a component that sequences other components and owns the main loop. Scheduler/DAG: builds a graph of tasks with dependencies and executes respecting order. Agent/worker: units that receive work, act (often with tools/handlers), and return results, frequently with their own lifecycle. Plugin system: a host that discovers, loads, and invokes external/late-bound units against a defined contract. Config/DSL engine: code that parses a bespoke config or language into an internal model that drives behavior. Dedicated service: a separately-deployable or clearly-bounded module (e.g. a device/edge/hardware layer) with its own interface to the core. Reporting: turns accumulated data into human- or machine-facing output. Discovery: how components find each other or find work/resources at runtime.

## 6. Dedicated service deep-dive **(conditional)** — *one top-level section per major standalone service*

When a subsystem is a first-class, separately-bounded service (for example a device service, an edge/hardware layer, an ingestion service, a gateway), give it its **own top-level section** with the full treatment: its purpose, internal architecture, its own data flow, its interface/contract with the rest of the system, its lifecycle and health model, its config, its failure modes, and how to run/test it in isolation.

- **Diagram**: a dedicated container diagram for the service, a sequence diagram of its interaction with the core, and a state diagram of its lifecycle (e.g. connect → provision → active → draining → disconnected).

## 7. Technology stack & rationale

- **Purpose**: what the system is built from and *why*, plus what each piece does *in this codebase specifically*.
- **Include**: a table, then prose for the non-obvious choices explaining trade-offs and alternatives considered.

Table format:

| Layer / concern | Technology | Why it was chosen here | Role in this codebase | Notable alternatives / trade-offs |
| --- | --- | --- | --- | --- |
| Language(s) | … | … | … | … |
| Runtime / framework | … | … | … | … |
| Data store(s) | … | … | … | … |
| Messaging / queue | … | … | … | … |
| Orchestration / scheduling | … | … | … | … |
| Config / DSL | … | … | … | … |
| Testing | … | … | … | … |
| Build / CI-CD | … | … | … | … |
| Deployment / infra | … | … | … | … |
| Observability | … | … | … | … |

- **Mine from**: manifests/lockfiles, imports, Dockerfiles, CI config, infra-as-code. State versions where they matter. If a rationale is not documented anywhere, infer it cautiously and mark it `⚠ inferred`.

## 8. End-to-end data flow

- **Purpose**: trace the system's primary journey across all subsystems — for many products this is *input/prompt → processing → output/report*.
- **Include**: a numbered narrative of the whole path, naming each subsystem, the data at each hop (its shape and any transformation), where it is persisted, and where it can branch or fail. Then per-service flows where they differ, and any secondary flows (e.g. async callbacks, retries, streaming).
- **Diagram**: an end-to-end sequence diagram *and* a data-flow flowchart (with data stores). If orchestrator and a dedicated service have distinct internal flows, draw one per side and show the boundary crossing.
- **Mine from**: the entry point outward; follow the call chain; use the code graph to confirm the path.

## 9. Configuration & custom DSL **(conditional)**

- **Purpose**: teach a reader to configure the system and to author its config/DSL/DAG files from scratch.
- **Include**: everything in `dsl-documentation-guide.md` — the structure/grammar, a key-by-key reference, worked examples from minimal to advanced, validation/errors, and gotchas.
- **Diagram**: parse→validate→bind flow; a schema class diagram; for pipeline/DAG YAML, an example DAG rendered from a sample file.
- **Mine from**: the config loader/parser, schema/validation code, example config files, and every place a config value is read.

## 10. Setup & installation

- **Purpose**: get the system running locally and understand how it ships to production.
- **Include**: everything in `setup-and-cicd.md` — prerequisites, local setup step-by-step, environment/secrets, running, verifying, and the production/CI-CD pipeline.
- **Diagram**: local run diagram; CI/CD pipeline diagram; deployment/topology diagram.
- **Mine from**: README, Makefile/scripts, Dockerfiles, CI workflows, infra-as-code, env templates.

## 11. Testing

- **Purpose**: explain how quality is assured and how to add tests.
- **Include**: everything in the testing half of `testing-and-debugging.md` — the test layers present, how they are organized, how to run them, coverage, fixtures/mocks, and how to write a new test.
- **Diagram**: the test pyramid/layers for this project; optionally a CI test-stage flow.
- **Mine from**: test directories, test runner config, CI test steps.

## 12. Operations, observability & debugging

- **Purpose**: run it in production and fix it when it breaks.
- **Include**: everything in the debugging half of `testing-and-debugging.md` — logging/metrics/tracing, health checks, common failure modes with symptom → cause → fix, and troubleshooting decision trees.
- **Diagram**: a troubleshooting decision-tree flowchart for the top failures; an observability data-flow.
- **Mine from**: logging/telemetry code, health endpoints, error handling, alert config.

## 13. Stakeholder perspectives

- **Purpose**: a tailored on-ramp per audience.
- **Include**: one subsection per relevant persona (end user, developer/integrator, tester/QA, ops/SRE, business/client, security), each following `stakeholder-views.md`.
- **Diagram**: optionally a persona→section map.

## 14. Extending the product

- **Purpose**: the concrete "how do I add X" guide.
- **Include**: worked walk-throughs for the real extension points found — e.g. write a new plugin, add a DAG node type, register a new agent/handler, add a config option, integrate a new device/service. Show the minimal diff/steps.
- **Diagram**: sequence showing where the extension hooks in.

## 15. Status: done vs pending

- **Purpose**: an honest snapshot of maturity.
- **Include**: everything in `status-and-gaps.md` — a capability status table, known limitations, and a roadmap. Mark unverified items.
- **Diagram**: a roadmap timeline/gantt; optionally a coverage/status heat table.

## 16. FAQ

- 8–20 real questions a reader of *this* system would ask, with direct answers and links to the relevant section. Draw from ambiguities you hit during discovery.

## 17. Glossary

- Every domain and system term, defined in one or two sentences, alphabetized.

## 18. Appendices

- Directory map (annotated tree), full config key index, API/endpoint index, environment-variable index, and a "documentation confidence" note listing what was verified vs `⚠ unverified`.

---

## Section applicability quick rule

Include §6 only if there is a standalone service. Include §9 only if there is a config surface or DSL. Include §14 only if there are real extension points. Everything else is near-universal. When in doubt, include the section but keep it short rather than omit — a reader prefers "this system has no plugin system" stated explicitly over silence.
