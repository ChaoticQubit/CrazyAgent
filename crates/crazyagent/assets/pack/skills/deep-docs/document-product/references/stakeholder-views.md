# Stakeholder perspectives

The same system reads differently to different people. Write one subsection per persona that actually applies to this product. Each subsection is a *tailored on-ramp*: it tells that reader where to start, what they most need to know, and links into the deeper sections rather than repeating them. Keep each to a few tight paragraphs plus a short "start here" list.

For each persona, answer their driving question, list their top tasks, and point to the relevant sections and diagrams.

## End user

- **Driving question:** "What can this do for me and how do I use it?"
- **Cover:** what the product does in their terms, the core workflow (configure → run → get output), how to read the output/report, limits and expected behavior, and where things can go wrong from their side.
- **Point to:** overview (§1), the config/DSL authoring tutorial (§9) if users author configs, the FAQ (§16), and a user-journey diagram.
- **Tone:** minimal jargon; concrete examples.

## Developer / integrator

The reader who will **embed this product into their own systems and services**, extend it, or build against its APIs. Often the most important persona for a handoff.

- **Driving question:** "How do I build on, extend, or integrate this — safely?"
- **Cover:** the architecture (§2–3), the public interfaces/APIs and their contracts, the extension points (write a plugin, add a node/handler, add a service — §14), the data model (§4), local setup (§10), how to run tests (§11), and the coding conventions/patterns to follow.
- **Point to:** subsystem deep-dives (§5), sequence and dependency diagrams, the "Extending the product" walk-throughs.
- **Include:** a short "integrate in 15 minutes" path — the smallest real example of calling/embedding the system.

## Tester / QA

- **Driving question:** "How do I verify this works and catch regressions?"
- **Cover:** the test strategy and layers (§11), how to run each suite, what is and isn't covered, how to write a new test, the critical paths that must never break, known flaky areas, and how to reproduce/verify a bug.
- **Point to:** testing section, the end-to-end flow (§8) as the map of what to exercise, failure modes (§12), and the status table (§15) for coverage gaps.
- **Include:** a checklist of the highest-value scenarios to regression-test.

## Ops / SRE / DevOps

- **Driving question:** "How do I run this in production and keep it healthy?"
- **Cover:** deployment topology (§10), configuration and secrets, scaling characteristics and bottlenecks, health checks and observability (logs/metrics/traces), the failure runbooks and troubleshooting trees (§12), backup/restore and data retention, and rollback procedure.
- **Point to:** deployment + CI/CD diagrams, the debugging section, the state diagrams for lifecycles.
- **Include:** an at-a-glance "operational summary": ports, endpoints, dependencies, resource needs, key dashboards/alerts.

## Business / client stakeholder

- **Driving question:** "What is it, what's its status, and what does it cost/enable?"
- **Cover:** the product overview in plain language (§1), the capability status — done vs pending (§15), the roadmap, key risks/limitations, and the value delivered. Avoid implementation detail.
- **Point to:** overview, status/roadmap, and a single high-level architecture picture (for credibility, not depth).
- **Tone:** outcomes and status, not code.

## Security / compliance **(include if relevant)**

- **Driving question:** "What is the attack surface and how is data handled?"
- **Cover:** trust boundaries (from the deployment diagram), authn/authz, secrets management, data classification and flow (what data, where it is stored, how long, encryption in transit/at rest), external dependencies and their access, input validation, and audit/logging.
- **Point to:** the data-flow diagram (annotated with trust zones), the deployment diagram, and the config section for secrets handling.

## Product / PM **(optional)**

- **Driving question:** "What's built, what's next, and where are the seams to change?"
- **Cover:** capability map, status/roadmap (§15), extension points as "where new features plug in", and the open questions surfaced during discovery.

---

## Persona → section map (include as a small table in §0 and §13)

| Persona | Start with | Then read |
| --- | --- | --- |
| End user | §1 Overview, §16 FAQ | §9 Config authoring |
| Developer / integrator | §2–3 Architecture, §14 Extending | §5 Deep-dives, §10 Setup, §11 Testing |
| Tester / QA | §11 Testing, §8 Data flow | §12 Debugging, §15 Status |
| Ops / SRE | §10 Setup/Deploy, §12 Debugging | §7 Stack, state diagrams |
| Business / client | §1 Overview, §15 Status | §2 Architecture (skim) |
| Security | §8 Data flow, §10 Topology | §9 Secrets, §7 Stack |
