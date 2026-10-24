---
name: diagram-codebase
description: >
  System-specific mermaid diagrams for a codebase or one subsystem, without the full
  documentation: context, container, component/class, sequence, data flow, state, ER,
  DAG, deployment, CI/CD, each with a one-line caption. Use on "diagram this codebase",
  "draw the architecture", "visualize how this system works".
metadata:
  version: "0.1.0"
---

# Diagram Codebase

Produce technical mermaid diagrams that accurately depict a real system. This is the focused, diagram-only companion to the `document-product` skill; it reuses the same discovery method and diagram cookbook so the visuals match what full documentation would produce.

## Principles

- **Accuracy over decoration.** Every node and edge must correspond to something real in the code — a real module, service, call, state, or table. No generic filler boxes.
- **Right diagram for the question.** Structure → container/component/class. Behavior over time → sequence. Lifecycle → state. Data model → ER. Dependencies/pipeline → DAG/flowchart. Deployment → infra diagram. Choose deliberately using the cookbook's decision guide.
- **Legibility.** Keep each diagram under ~20 nodes; split a large picture into a high-level diagram plus per-area detail diagrams. Use subgraphs for layers, consistent direction, and short but specific labels.
- **Always validate.** No diagram ships until it renders.

## Workflow

1. **Scope.** Determine whether the user wants a whole-system set or a single subsystem/flow, and which diagram types. If unspecified, default to a whole-system set: system context, high-level container, core component/class, primary sequence, end-to-end data flow, and (if applicable) a DAG and a deployment diagram.

2. **Discover just enough.** Use `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/discovery-playbook.md` to identify the components, boundaries, main flow, key lifecycles, and data model relevant to the requested diagrams. If a code graph (e.g. a Graphify export) is available, ingest it — it maps directly onto structure and dependency diagrams.

3. **Author diagrams.** Build each one from the templates in `${CLAUDE_PLUGIN_ROOT}/skills/document-product/references/mermaid-cookbook.md`, substituting real names and steps from the code. Give each diagram a title and a one-sentence caption stating what it shows and where it lives in the code.

4. **Validate.** Render every diagram with the Mermaid MCP if connected, else a local `mmdc`, else check against the cookbook's syntax-gotchas list. Fix until all render.

5. **Deliver.** Return the diagrams as fenced ```mermaid blocks. If the user asked to place them in a file (README, wiki page, docs), write them there with their captions; otherwise present them inline and offer to save them.

## Output

A titled, captioned set of validated mermaid diagrams. When multiple diagrams form a set, order them from most abstract (context) to most concrete (class/ER), so a reader can zoom in progressively.
