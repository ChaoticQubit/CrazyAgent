# Codebase discovery playbook

Goal: build an accurate **system map** of an unfamiliar codebase — its layers, subsystems, boundaries, flows, and data model — grounded in the actual source. Language-agnostic. Adjust commands to whatever tools are available (`rg`/ripgrep preferred; fall back to `grep -r`).

Work top-down (shape) then follow-the-flow (behavior). Record findings as you go into the system map format at the end.

## Step 1 — Orient

Establish size, languages, and top-level shape.

```bash
# Top-level layout (2 levels), ignoring noise
find . -maxdepth 2 -type d -not -path '*/.git/*' -not -path '*/node_modules/*' | sort
# Language mix and LOC (if cloc present); else count by extension
cloc . 2>/dev/null || rg --files | sed 's/.*\.//' | sort | uniq -c | sort -rn | head -30
# Repo history signal: most-churned files (often the core)
git log --pretty=format: --name-only 2>/dev/null | grep . | sort | uniq -c | sort -rn | head -30
```

Note: monorepo vs single package, primary language(s), and where the bulk of the code lives.

## Step 2 — Find the entry points

Execution has to start somewhere. Locate every start:

- **Services/servers**: `main`, `app`, `server`, `wsgi/asgi`, `index`, framework bootstraps, `if __name__ == "__main__"`.
- **CLIs**: argument parsers, `bin/`, console-script entries in manifests.
- **HTTP/RPC handlers**: route/controller registration, `@app.route`, `router.`, gRPC service impls.
- **Event/queue consumers**: subscribers, workers, `on_message`, cron/schedulers.
- **UI**: root component / page entry.

```bash
rg -n --hidden -g '!.git' -e '\bdef main\b' -e 'if __name__ == .__main__.' \
  -e '\bfunc main\(' -e 'app\.listen' -e 'createServer' -e '@(app|router)\.(get|post|put|delete)' \
  -e 'addEventListener|on_message|@task|@cron|schedule\('
```

For each entry point, record: what triggers it, and the first few calls it makes (this seeds the flow trace).

## Step 3 — Map modules & boundaries

Turn directories into subsystems.

- Read the top-level dirs; infer responsibility from names and contents.
- Identify **process/network boundaries**: separate services, separate deployables, anything with its own manifest/Dockerfile, anything talking over HTTP/queue/socket.
- Identify **core abstractions**: base classes, interfaces, protocols, ABCs, traits — these reveal the extension seams.

```bash
# Interfaces / abstractions (tune per language)
rg -n -e 'class .*\(ABC\)|abstractmethod' -e 'interface [A-Z]' -e 'type .* interface' \
  -e 'trait [A-Z]' -e 'Protocol\)' -e '@runtime_checkable'
```

## Step 4 — Dependencies & build

- Read every manifest/lockfile: `package.json`, `pyproject.toml`/`requirements*.txt`, `go.mod`, `Cargo.toml`, `pom.xml`/`build.gradle`, `Gemfile`, etc. These name the tech stack.
- Read `Makefile`, `Taskfile`, `justfile`, and `scripts/` — they encode the real build/run/test commands.

## Step 5 — Configuration surface

- Find config files and loaders: `*.yml/yaml`, `*.toml`, `*.ini`, `*.env*`, `config/`, `settings*`.
- Find every place config/env is *read*: `os.environ`, `process.env`, `viper`, `Settings(`, config schema classes. This tells you the real knobs.
- Detect a **custom DSL / DAG language**: a bespoke YAML/JSON schema with its own parser, or a hand-written grammar (look for `lark`, `antlr`, `pyparsing`, `peg`, `.g4`, custom tokenizers). If present, flag §9 and use the DSL guide.

```bash
rg -n -e 'os\.environ|getenv|process\.env|Deno\.env' -e 'load.*(yaml|yml|toml|json).*config' \
  -e 'antlr|lark|pyparsing|peg\.|\.g4\b'
```

## Step 6 — Data & external integrations

- **Data stores**: ORM models, migrations, `CREATE TABLE`, connection strings, client libs (postgres, mysql, mongo, redis, sqlite, s3).
- **Messaging**: kafka, rabbitmq, sqs, nats, redis streams, pub/sub.
- **External services/APIs**: outbound HTTP clients, SDKs, webhook handlers, auth providers.

```bash
rg -n -e 'CREATE TABLE|migrat' -e 'psycopg|sqlalchemy|mongoose|redis|boto3|s3' \
  -e 'kafka|rabbit|sqs|nats|pubsub' -e 'requests\.(get|post)|axios|httpx|fetch\('
```

## Step 7 — Tests & CI

- Locate test dirs and the runner (`pytest`, `jest`, `go test`, `cargo test`, `rspec`).
- Read CI config (`.github/workflows/*`, `.gitlab-ci.yml`, `Jenkinsfile`, `circleci`) — it reveals build, test, and deploy stages authoritatively.

## Step 8 — Infrastructure & deployment

- `Dockerfile`, `docker-compose*`, `k8s`/`helm`, `terraform`, `serverless`, `Procfile`, cloud config. These give the runtime topology and the deployment story.

## Step 9 — Ingest a code graph (e.g. a Graphify export)

If the user has a code graph, use it — it is the fastest route to accurate structure and dependency diagrams.

1. **Locate & detect format.** Common: JSON (nodes/edges arrays), GraphML/XML, DOT/Graphviz, CSV edge list, or a graph-DB export. Inspect the first lines to detect it.
2. **Load it.** Parse into nodes and edges. Nodes typically = files/modules/symbols (functions, classes); edges = calls, imports, or contains. Preserve any `type`/`kind`/`layer` attributes.
3. **Extract signal:**
   - **Hubs** (high in/out degree) → the architecturally central components; feature them in the high-level diagram.
   - **Layers/clusters** → subsystem boundaries; map clusters to the modules from Step 3.
   - **Cycles** → coupling hotspots worth calling out.
   - **Entry→leaf paths** → confirm the end-to-end flow found in Step 2.
   - **Orphan/dead nodes** → possible pending/unused code (feeds status §15).
4. **Drive diagrams.** Convert the relevant subgraph directly into a mermaid dependency/component diagram (collapse to module level so it stays legible — do not dump hundreds of symbol nodes).

```bash
head -c 2000 <graph-file>            # detect format
# JSON quick shape:
rg -o '"(nodes|edges|source|target|type|label)"' <graph-file> | sort | uniq -c
```

Treat the graph as *evidence*, not gospel — reconcile it with what the source actually shows.

## Step 10 — Reconcile existing docs

Read `README`, `CLAUDE.md`, `docs/`, ADRs, design notes. Extract their claims and **verify each against code**. Where docs and code disagree, trust the code and note the drift (this is valuable to the client). Salvage accurate rationale (the "why") — it is hard to recover from code alone.

## Step 11 — Interview for ambiguities (if a human is available)

When intent is unrecoverable from code (why a design choice was made, which flows matter most, what is intentionally unfinished), ask the user a short, specific list. If unattended, proceed and mark inferences `⚠ inferred`.

## Step 12 — Detect done-vs-pending signals (feeds §15)

```bash
rg -n -e 'TODO|FIXME|HACK|XXX' -e 'NotImplemented|not implemented|unimplemented|raise NotImplementedError' \
  -e '@skip|xfail|it\.skip|t\.Skip|@Ignore' -e 'feature.?flag|if .*ENABLED|toggle'
```

Stubs, skipped tests, feature flags, empty modules, and unwired interfaces all indicate pending work.

## Output: the system map

Record discovery as a structured map before writing prose:

```
SYSTEM MAP
- Product: <one-line>
- Languages/runtime: <...>
- Deployables/services: [ {name, path, responsibility, boundary-type} ]
- Subsystems: [ {name, archetype, key_files, responsibility, depends_on, consumed_by} ]
- Primary flow: <entrypoint> -> ... -> <output>   (numbered hops)
- Data model: [ entities + relationships ]
- Data stores / queues / external services: [ ... ]
- Config/DSL: {present?, loader, schema, example files}
- Tests: {frameworks, dirs, how-to-run}
- CI/CD: {system, stages}
- Infra: {docker/k8s/terraform/...}
- Code graph: {present?, format, key hubs, clusters}
- Status signals: {TODOs, stubs, skipped tests, flags}
- Doc/code drift: [ ... ]
- Open questions: [ ... ]
```

Everything written later must trace back to this map.

## Language-specific quick hints

| Language | Entry points | Manifests | Interfaces | Tests |
| --- | --- | --- | --- | --- |
| Python | `__main__`, ASGI/WSGI app, Click/argparse | `pyproject.toml`, `requirements.txt` | `ABC`, `Protocol` | `pytest`, `tests/` |
| JS/TS | `index`, `main`, framework entry, `bin` | `package.json` | `interface`, `type` | `jest`/`vitest`, `*.test.*` |
| Go | `func main`, HTTP mux, cobra | `go.mod` | `interface` | `*_test.go` |
| Rust | `fn main`, `lib.rs` | `Cargo.toml` | `trait` | `#[test]`, `tests/` |
| Java/Kotlin | `main`, Spring `@SpringBootApplication` | `pom.xml`/`gradle` | `interface`, `abstract` | JUnit, `src/test` |
| Ruby | `bin/`, Rack, Rails | `Gemfile` | modules/duck | RSpec, `spec/` |
