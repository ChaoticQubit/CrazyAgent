# Vercel Sandbox for CrazyAgent

Research date: October 17, 2026. Research only. No installation, account connection, provisioning, credentials, provider calls, paid execution or repository upload occurred. Official documentation and public source were read; the service was not runtime-qualified. No adoption decision is made here.

## What does Sandbox provide, and what are its security and integration boundaries?

### Takeaway

Vercel Sandbox is a managed Linux microVM execution service with programmatic commands, interactive terminal access, persistent filesystems and network controls. It can supply a remote execution boundary, but it does not supply CrazyAgent's policy authority, immutable verification contract or local-first operation. [Overview](https://vercel.com/docs/sandbox)

### Cited Findings

1. **Isolation and environment.** Vercel documents one Firecracker microVM with its own filesystem/network per sandbox, including support for privileged workloads such as Docker and FUSE. Managed Linux images and custom OCI images are available; custom images use Vercel Container Registry. The default universal image includes Node.js, Python and coding tools. This is documented architecture, not an escape-resistance test performed here. [Overview](https://vercel.com/docs/sandbox); [Images](https://vercel.com/docs/sandbox/concepts/images)

2. **Commands and human interaction.** The JS SDK supports argument-vector commands, working directories, environment variables, detached execution, streamed output, command lookup and signals. Commands can receive a readable stdin stream, or `stdin: true` with detached execution followed by `writeStdin()`. Missing stdin means an empty input stream. The CLI supports interactive PTY execution with `sandbox exec --interactive --tty NAME -- bash`; it also manages sandbox lifecycle and copies files in either direction. `sandbox copy` transfers one file per invocation, not a directory synchronization service. Public sandbox URLs expose declared development-server ports. [JS SDK reference](https://vercel.com/docs/sandbox/sdk-reference); [CLI reference](https://vercel.com/docs/sandbox/cli-reference)

3. **Network and secrets.** Policies support unrestricted, blocked or custom egress, and live policy changes. Hostname filtering uses TLS SNI; plain HTTP needs IP rules. Ordinary allowed TLS is forwarded without HTTP inspection, so an SNI allowlist alone does not pin the encrypted HTTP Host header. Rules can transform headers or forward requests to a chosen proxy; these rules require TLS termination with a per-sandbox CA. Header brokering can keep provider secrets outside the VM. Clients using custom CA bundles need configuration. A broad allowed domain remains a possible exfiltration destination. These are controls requiring an explicit engine-owned policy, not a claim that a default sandbox has restricted egress. [Firewall](https://vercel.com/docs/sandbox/concepts/firewall)

4. **State and workspaces.** Persistent sandboxes are now the default: stopping or timing out saves the filesystem, and resuming boots another session from it. Non-persistent mode discards its filesystem on stop. Explicit snapshots can seed other sandboxes; snapshots can outlive a deleted sandbox and continue to incur storage charges. Snapshot retention is configurable. Persistent drives are another independent storage surface. Git sources, file APIs and CLI copy move code into/out of the VM; none is a documented transparent mount of a local Git worktree. [Persistence](https://vercel.com/docs/sandbox/concepts/persistent-sandboxes); [Snapshots](https://vercel.com/docs/sandbox/concepts/snapshots); [Drives](https://vercel.com/docs/sandbox/concepts/drives); [CLI reference](https://vercel.com/docs/sandbox/cli-reference)

5. **Control surfaces and authentication.** Supported first-party integrations are JS/TS `@vercel/sandbox`, Python `vercel.sandbox`, the CLI and REST endpoints. The REST reference exposes create/get, command execution/logs/kill, file operations, timeout and network policy operations, so Rust can use HTTP without replacing its engine with Node. SDKs remain the recommended integration path. Sandbox authentication is Vercel project OIDC or an access token. Local OIDC tokens obtained through the Vercel workflow expire after 12 hours. This authenticates Sandbox infrastructure, separately from the model account used by an agent. [Overview](https://vercel.com/docs/sandbox); [Python SDK](https://vercel.com/docs/sandbox/python-sdk-reference); [REST API](https://vercel.com/docs/rest-api); [Authentication](https://vercel.com/docs/sandbox/concepts/authentication)

### Inferences

Persistent state means tokens written into files, account sessions, fixtures, cloned customer code and agent histories may survive stop. A deliberate non-persistent configuration, minimal exported artifacts and explicit snapshot/drive retention would therefore be required for disposable work. Brokering avoids placing the secret in the guest, but still delegates handling to Vercel and permits authenticated actions on the allowed destination. [Persistence basis](https://vercel.com/docs/sandbox/concepts/persistent-sandboxes); [Firewall basis](https://vercel.com/docs/sandbox/concepts/firewall)

Filesystem recovery should be treated separately from live-worker recovery: the documented mechanism boots a new session from saved files. CrazyAgent must restore harness continuation and process supervision explicitly rather than promising that a stopped PTY resumes its old process. [Persistence basis](https://vercel.com/docs/sandbox/concepts/persistent-sandboxes)

A Rust HTTP adapter is plausible, but its reconnect, streaming, PTY resize, cancellation, retry and error semantics still need qualification. A JS SDK sidecar reduces protocol implementation work while adding another runtime and a process boundary. Neither option is selected by this research. [REST basis](https://vercel.com/docs/rest-api); [SDK basis](https://vercel.com/docs/sandbox/sdk-reference)

### Gaps

No official Rust Sandbox SDK or complete self-hosted Sandbox server distribution was identified in the reviewed official interfaces. CLI PTY support is documented; this research did not prove equivalent interactive control entirely through the public REST surface. Native macOS/Windows GUI or Tauri desktop qualification inside these Linux VMs was not demonstrated. [Published interfaces](https://vercel.com/docs/sandbox); [Public repository](https://github.com/vercel/sandbox)

## What are the current costs, quotas and resource budgets?

### Takeaway

Use the dedicated pricing reference dated September 10, 2026, rather than older official comparisons. CPU billing excludes I/O waiting, while allocated memory remains billable throughout a session. [Pricing and quotas](https://vercel.com/docs/sandbox/pricing)

### Cited Findings

Default-region `iad1` figures, USD. Regional differences apply. [Pricing and quotas](https://vercel.com/docs/sandbox/pricing)

| Metric | Hobby allowance | Pro | Enterprise |
| --- | --- | --- | --- |
| Active CPU | 5 h/month | $0.128/CPU-h | Same listed rate |
| Allocated RAM | 420 GB-h/month | $0.0212/GB-h | Same listed rate |
| Creations | 5,000/month | $0.60/million | Same listed rate |
| Transfer | 20 GB/month | Flat Rate CDN inclusion | $0.15/GB |
| Snapshot storage | 15 GB | $0.08/GB-month | Same listed rate |
| Drive storage | 15 GB | $0.05/GB-month | Same listed rate |
| Drive reads/writes | 30/30 GB/month | $0.0015/$0.004 per GB | Same listed rates |
| Concurrent sandboxes | 10 | 10,000 | 10,000 |
| Maximum vCPU/RAM | 4/8 GB | 8/16 GB | 32/64 GB |
| Session maximum | 45 min | 24 h | 24 h |

Default allocation: 2 vCPU, 2 GB RAM/vCPU; 1 or even CPU counts; five-minute timeout; 15 ports; SDK 3+ has 64 GB disk. RAM has a one-minute billing minimum. Internet downloads are free; outbound and exposed-port traffic count. Initial/max vCPU allocation per minute: Hobby 20/40, paid 150/5,000. Control requests/minute: 1,000/10,000/100,000. [Pricing and quotas](https://vercel.com/docs/sandbox/pricing)

Pro starts at $20/month and includes $20 shared monthly usage credit; Sandbox consumes that credit. It is not an extra Sandbox-specific $20 allocation. Enterprise commercial terms are custom. [Plan pricing](https://vercel.com/pricing); [Credit model announcement, September 9, 2025](https://vercel.com/changelog/included-pro-usage-is-now-credit-based); [Sandbox billing](https://vercel.com/docs/sandbox/pricing#billing-information)

### Inferences

**Transparent ten-worker example, calculation rather than observed invoice:** ten Pro sandboxes, each 2 vCPU/4 GB, each running one hour in `iad1`, averaging 10% utilization across its two CPUs:

| Component | Calculation | Estimated usage charge |
| --- | --- | --- |
| CPU | 10 × 2 × 1 h × 10% × $0.128 | $0.256 |
| RAM | 10 × 4 GB × 1 h × $0.0212 | $0.848 |
| Creation | 10 × $0.60 / 1,000,000 | $0.000006 |
| Combined | Sum above | $1.104006 |

At sustained 100% utilization, the same allocation costs about $3.408006. The 10% assumption is illustrative, not measured CrazyAgent behavior. Formula uses the documented resource rates, independently calculated here. [Rate inputs](https://vercel.com/docs/sandbox/pricing)

These totals exclude snapshots, drives, transfer beyond applicable inclusion, image-registry charges, model tokens or subscriptions, taxes and the plan minimum. Existing unused Pro credit might absorb the usage; a new customer still has a subscription baseline. Ten concurrent workers fit Hobby's concurrency count, but a continuous one-hour session exceeds Hobby's duration limit. [Quota basis](https://vercel.com/docs/sandbox/pricing); [Plan baseline](https://vercel.com/pricing)

### Gaps

**Official-source conflicts preserved:** the older Vercel comparison reports 2,000 paid concurrent sandboxes, whereas the dedicated September 10 pricing page reports 10,000. The public README inspected October 17 says Hobby permits 8 vCPUs, whereas the dedicated pricing page says 4. Do not combine these numbers. Use dedicated current pricing for planning, and verify account-specific entitlements before any purchase. [Older comparison](https://vercel.com/kb/guide/vercel-sandbox-vs-e2b); [README](https://github.com/vercel/sandbox); [Dedicated pricing](https://vercel.com/docs/sandbox/pricing)

No actual utilization, snapshot size, command-request rate, provider-token volume, account entitlements or invoice was measured. The word "included" for Pro transfer is not interpreted here as an unconditional unlimited contractual allowance. [Pricing basis](https://vercel.com/docs/sandbox/pricing)

## Should CrazyAgent depend on it, and how does it compare with local execution?

### Takeaway

**Recommendation: consider an optional remote backend, never a mandatory dependency for the local-first product.** Retain the Rust policy engine, local storage/memory authority and independent verifier contract. Any backend selection remains a later owner decision after live qualification and authorization. [Service boundary](https://vercel.com/docs/sandbox); [CrazyAgent adopted architecture](../../docs/superpowers/specs/2026-10-17-crazyagent-architecture-design.md)

### Cited Findings

1. **Open source is the client layer.** The public repository includes the SDK, CLI and managed-image definitions under Apache-2.0. That license covers the published source; the README describes a Vercel-operated execution service. This is not evidence that the entire hosted control plane can be deployed locally. [Repository and license](https://github.com/vercel/sandbox); [License text](https://github.com/vercel/sandbox/blob/main/LICENSE)

2. **Model choice is separate.** Sandbox supports general command execution and documents integrations with several model/agent ecosystems. Its infrastructure authentication does not establish that a user's Claude, Codex or other CLI subscription can be transferred to a cloud worker. Agent/provider authentication and billing must be checked separately for each harness. [Ecosystem](https://vercel.com/docs/sandbox/ecosystem); [Authentication](https://vercel.com/docs/sandbox/concepts/authentication)

3. **Multi-agent users are a convenience boundary.** Vercel documents separate Linux users/private homes and group-based file sharing inside one sandbox. That is different from one VM per worker. Commands also have privileged execution support, so a verifier placed in the same guest is not automatically immutable against a worker granted root. [Multi-agent documentation, updated August 19, 2026](https://vercel.com/docs/sandbox/concepts/multi-agent); [SDK](https://vercel.com/docs/sandbox/sdk-reference)

4. **Local native option.** Anthropic sandbox-runtime documents native filesystem/network restrictions using macOS `sandbox-exec` and Linux isolation/proxies, with explicit limitations around network/domain policy and privileged sockets. Its public source is an option for local execution, not evidence that CrazyAgent's requested aggregate CPU/RAM/process budgets have passed. The project-side filesystem/network proof and outstanding resource-confinement boundary supplied in this assignment remain distinct. [sandbox-runtime source](https://github.com/anthropics/sandbox-runtime); [CrazyAgent authority](../../docs/agent-decisions.md)

5. **Project acceptance remains local.** CrazyAgent's adopted architecture places execution policy, work ownership, verification and approval-sensitive memory promotion in the engine, with TUI and desktop control surfaces. The current phase explicitly defers normal/security reviews while preserving real testing. This research neither changes that authority nor counts as a qualification/review pass. [Architecture](../../docs/superpowers/specs/2026-10-17-crazyagent-architecture-design.md); [Decisions DEC-060 through DEC-062](../../docs/agent-decisions.md)

### Inferences

| Option | Useful advantage | Cost or limit for CrazyAgent |
| --- | --- | --- |
| Existing bounded local Docker prototype | Local data, known container controls, no per-hour cloud meter, natural repository access | Host/Docker-daemon ownership and mounted paths remain critical; current proof does not transfer automatically to every harness |
| Local sandbox-runtime | Native agent experience, filesystem/network boundary already exercised locally | Full aggregate resource confinement remains unadmitted; do not relabel this as fully qualified |
| Vercel Sandbox | Separate microVMs, remote Linux capacity, explicit network policies, interactive CLI, recoverable filesystems | Cloud upload/trust, network dependency, recurring resource/storage costs, Linux-only guest and worktree/result synchronization |

The local prototype statements are the assignment's current baseline, not fresh tests run during this research. Vercel advantages are documented capabilities, not comparative performance or security measurements. [Vercel basis](https://vercel.com/docs/sandbox); [Local-native basis](https://github.com/anthropics/sandbox-runtime)

A future optional adapter should keep Vercel control credentials in the Rust engine, outside coding workers; give each worker its own sandbox where strong worker separation is needed; run the verifier from engine-owned immutable inputs in a distinct environment; and export only approved patches/artifacts keyed to the intended base revision. Neither Linux users nor a snapshot alone enforce CrazyAgent's immutable test/specification contract. This is a proposed fit, not an approved implementation plan. [Isolation/control basis](https://vercel.com/docs/sandbox); [Current contract](../../docs/superpowers/specs/2026-10-17-crazyagent-architecture-design.md)

Keep configurable global/project memory on the existing local authority and send only task-scoped material after explicit upload authorization. A cloud backend can run Rust builds and browser-facing development servers, but it cannot by itself prove local native TUI/desktop behavior or continuous recovery of each actual harness process. [Linux environment basis](https://vercel.com/docs/sandbox); [Filesystem persistence basis](https://vercel.com/docs/sandbox/concepts/persistent-sandboxes)

### Gaps

No service execution, adversarial boundary test, latency benchmark, offline operation, self-hosted deployment, provider login or CLI-subscription portability test occurred. No direct first-party relationship between Sandbox and fx was established in the reviewed Sandbox material; the separate fx research should resolve that without assuming they are the same product. [Published Sandbox scope](https://github.com/vercel/sandbox)

The next bounded research-to-implementation step would be an owner decision about whether to authorize a separate optional-backend qualification proposal. It would need a declared account/budget, upload scope and real acceptance evidence before any cloud execution. No such authorization is inferred here.
