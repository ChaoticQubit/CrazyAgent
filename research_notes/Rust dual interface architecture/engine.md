# Rust engine, local session ownership and protected verification

Research date: October 16, 2026. Scope: a primary TUI plus desktop application, one Rust engine, Linux/macOS/Windows architectural targets, macOS-first release qualification. This is research and a recommendation for the forthcoming grilling session. No framework, persistent service, platform parity or implementation has been approved by this note. The numbered project decision register and previously approved release sequence were consulted after querying the project graph.

## 1. What should own state and native sessions across both clients?

### Takeaway

Recommend one user-local Rust engine process owning sessions, projects, shared configuration and verification coordination, with two clients using the same application operations. Keep reusable domain code in Rust libraries inside that product, but do not make each UI a separate execution owner. Closing a client should detach from its sessions; surviving an engine crash or reboot is a different requirement requiring native session recovery.

### Cited Findings

- Tokio already provides Unix-domain streams and Windows named pipes. Windows pipe options include remote-client rejection, first-instance creation and explicit security attributes. Microsoft documents that the default pipe descriptor grants read access to Everyone and anonymous accounts, so a default pipe is not an acceptable sensitive control channel. [Tokio UnixStream](https://docs.rs/tokio/latest/tokio/net/struct.UnixStream.html), [Tokio pipe options](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/struct.ServerOptions.html), [Microsoft named pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights).
- The interprocess crate offers a shared local-socket API over Unix sockets and Windows named pipes, supports Tokio and lists Windows/Linux/macOS as explicitly tested targets. Its license is 0BSD OR Apache-2.0. This is a possible dependency when its abstractions remove real platform code, not a mandatory networking layer. [interprocess documentation](https://docs.rs/interprocess/latest/interprocess/), [local socket API](https://docs.rs/interprocess/latest/interprocess/local_socket/index.html).
- Tokio broadcast channels have finite retention and report lag when old values are overwritten. They do not provide durable replay automatically. [Tokio broadcast](https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html).
- Tokio process commands inherit environment and working directory by default; an executable without an absolute path is resolved using platform search behavior. Its API provides explicit environment control, process groups and child termination behavior. [Tokio Command](https://docs.rs/tokio/latest/tokio/process/struct.Command.html).
- Serde and serde_json already provide Rust data serialization, including typed JSON representations; both are offered under MIT OR Apache-2.0. [Serde](https://docs.rs/serde/latest/serde/), [serde_json](https://docs.rs/serde_json/latest/serde_json/).
- Rusqlite is a synchronous SQLite binding under MIT; its bundled feature includes SQLite itself, which is public domain, and avoids requiring users to supply a particular SQLite installation. SQLx is MIT OR Apache-2.0 and supports asynchronous SQLite by using a dedicated background thread communicating through channels. Async SQLx does not make SQLite itself an async storage engine. [Rusqlite repository](https://github.com/rusqlite/rusqlite), [SQLx repository](https://github.com/transact-rs/sqlx), [SQLx SQLite connection](https://docs.rs/sqlx/latest/sqlx/sqlite/struct.SqliteConnection.html).
- SQLite WAL enables concurrent readers with a writer, but still has one writer at a time and requires processes to be on the same host. It is not a database file to put on a network filesystem for cross-machine sharing. [SQLite WAL](https://sqlite.org/wal.html).
- Git linked worktrees share repository metadata outside their per-worktree directories. Git also supports executable hooks and external filters through configuration. Consequently, copying a worktree or calling ordinary Git from an untrusted repository is not itself an isolation boundary. [Git worktree](https://git-scm.com/docs/git-worktree), [Git configuration](https://git-scm.com/docs/git-config).
- Gitoxide documents incomplete high-level Git feature coverage, including planned merge, rebase and push support; git2-rs wraps libgit2 and currently documents a bundled/static or system-library build choice. Neither eliminates the need to qualify the exact operations the application uses. [Gitoxide feature status](https://github.com/GitoxideLabs/gitoxide), [git2-rs](https://github.com/rust-lang/git2-rs).
- Current Keyring 4.2 documentation advises applications that need to select platform credential stores to use keyring-core and specific store crates rather than relying on the all-in-one keyring crate. Store options include Apple native, Windows native, Linux keyutils and Secret Service implementations. Keyring is MIT OR Apache-2.0. [Keyring current documentation](https://docs.rs/keyring/latest/keyring/), [keyring-core](https://docs.rs/keyring-core/latest/keyring_core/).

### Inferences

#### Proposed minimum topology

One Rust engine is justified by the newly required two-client product: it prevents duplicate account refresh, racing session ownership, conflicting database writes and two independent verification controllers. It can start lazily when the first client attaches. It need not be a root service, a TCP server, a multi-user deployment, an always-installed OS launch agent or a distributed system.

The primary TUI and the desktop's Rust backend both connect to this owner. The webview never receives engine authentication material or provider secrets. Both clients call the same project/session/configuration/verification behavior; neither owns native agent children. The owner launches them once and holds their transport handles. Multiple observers can see a session, while one explicit input owner supplies terminal input and resize authority at a time. This avoids duplicate keystrokes and competing dimensions without inventing collaborative terminal editing.

Recommend a small shared Rust domain library plus an engine executable and client protocol types, rather than exposing one giant backend interface to every component. Keep vendor-specific paths and account handles explicit. No broker, NATS, Kafka, universal event bus, generic workflow scheduler or microservice fleet is justified.

#### Minimal local protocol

Use a filesystem Unix socket in an owner-only runtime directory on macOS/Linux and a Windows named pipe with an owner-restricted DACL, remote-client rejection and first-instance protection. Validate peer identity and use a private per-engine bootstrap capability where needed. Do not put a reusable token in command arguments, project files, shared memory, logs, agent environment variables or a world-readable runtime file. IPC endpoint access checks are necessary but do not confine other processes running as the owner.

Start with typed, framed JSON requests/replies and explicit bounded event messages, not an HTTP server for a local application. Include protocol version, request identifier, project identifier and typed operation. Bound frame length, request counts, pending messages and subscription buffers. Return structured errors. A request asking to start a session must not be replayed as a fresh start after reconnect; persist its relevant operation identifier and outcome.

The engine retains current application state and monotonic event positions. A reconnecting client requests a state snapshot and subsequent events; a gap results in explicit resynchronization. Critical state changes are persisted transactionally. High-volume terminal bytes are a separate bounded stream with a documented scrollback/retention policy, not unlimited SQLite event rows. A slow client must not block native agent output or all other clients. Tokio broadcast is suitable for transient notification only if lag is handled; the SQLite state remains authoritative.

Loopback TCP is a fallback only if a concrete platform limitation wins the comparison. It adds port ownership, local-web-origin attacks, authentication and transport exposure considerations. Binding localhost is not authentication. Conversely, local sockets are not a complete security boundary against same-UID malicious tools.

#### Storage and execution recommendation

Use rusqlite with bundled SQLite on one dedicated blocking database worker owned by the engine. Keep transactions short, apply migrations serially and use WAL only with understood checkpoint/backup behavior. This matches a small local product with one authoritative writer. SQLx is a sound alternative if its async API and existing migration/compile-check workflow save demonstrated work; choosing it is not wrong, but adding both libraries is unnecessary. Avoid an ORM, external database or independent database copy per UI.

Persist project/session metadata, effective configuration versions, message delivery state and verification receipt metadata. Keep mutable application metadata distinct from the protected verifier authority and accepted receipt store. Do not store native OAuth tokens in SQLite or synchronized project memory. Provider-native login/profile directories remain provider-owned; an application keyring dependency is only needed for application-owned secrets or explicitly supported API-key flows. A secure store can be unavailable on headless Linux; show that state and require an explicit supported alternative rather than silently downgrading to plaintext.

Use the installed Git CLI for initial worktree operations and exact behavior users already expect, with argument arrays and protected operation choices. Agent-controlled repository hooks, filters, local config and environment can execute code; trusted snapshot creation must disable unapproved execution paths or occur inside confinement. Do not execute a repository's build helper during snapshot capture. gix can later accelerate measured read-only status/diff work. git2 is a legitimate alternative when it demonstrably simplifies a required operation, but adding a second complete Git stack before a bottleneck exists increases qualification work.

#### Build versus reuse matrix

| Product need | Reuse candidate | Proposed choice and limit |
| --- | --- | --- |
| Async IO, process tasks and bounded channels | Tokio, MIT | Reuse directly; explicit process lifecycle, no custom executor. [License](https://github.com/tokio-rs/tokio/blob/master/LICENSE) |
| Typed wire data | Serde + serde_json, MIT OR Apache-2.0 | Reuse; application defines a small versioned protocol. [Serde](https://docs.rs/serde/latest/serde/) |
| Local transports | Tokio UDS/named pipes; interprocess 0BSD OR Apache-2.0 | Prefer native Tokio APIs; add interprocess only if it removes qualified portability code. [Transport documentation](https://docs.rs/interprocess/latest/interprocess/) |
| Local metadata | rusqlite MIT, bundled SQLite public domain; SQLx MIT OR Apache-2.0 alternative | Prefer one synchronous SQLite worker. No per-client database. [Rusqlite](https://github.com/rusqlite/rusqlite), [SQLx](https://github.com/transact-rs/sqlx) |
| Native credentials | Provider-owned login; keyring-core plus platform stores if needed | Do not build a token vault or read consumer refresh tokens into common state. [Keyring](https://docs.rs/keyring/latest/keyring/) |
| Git behavior | Installed Git CLI; gix and git2 alternatives | Reuse Git initially, keep repository-controlled execution outside trusted host operations. [Git worktree](https://git-scm.com/docs/git-worktree) |

### Gaps

No prototype, dependency audit, startup/idle-memory measurement, SQLite load benchmark, packaging build, reconnect test or credential flow was executed. Cargo documentation retrieved on this date includes versions that can differ between latest-page crawls; select and record actual resolved versions and licenses during qualification. Current documentation and existing releases show maintained interfaces, not a support SLA. No engine crash guarantee follows from choosing Rust. Background startup and orphan/process-tree handling differ by OS and remain qualification work.

## 2. How can the verifier remain independent of coding agents and candidate tests?

### Takeaway

The trusted controller and rules must sit outside every coding harness and every candidate build/test process's write and control authority. A worktree, a second AI, another folder, client access controls or a containerized verifier alone cannot establish this. Recommend qualifying an existing whole-process sandbox first and declining the independence claim on any unsupported route.

### Cited Findings

- Anthropic sandbox-runtime can wrap arbitrary processes using macOS Seatbelt and Linux bubblewrap/network isolation; Windows support is now alpha using a dedicated local account, filesystem ACEs and a Windows Filtering Platform fence with one-time elevated setup. Its default reads are broad, and powerful Unix sockets or enabling Apple Events can expose host escape paths. It is Apache-2.0. [Runtime README](https://github.com/anthropics/sandbox-runtime/blob/main/README.md), [license](https://github.com/anthropics/sandbox-runtime/blob/main/LICENSE).
- Current upstream reports demonstrate why platform execution needs real tests: issue 623 reports missing child stdin on Windows, while issue 642 reports missing terminal resize propagation under Linux. These are reported behaviors for stated environments, not proof every current build remains broken. [Windows stdin report](https://github.com/anthropics/sandbox-runtime/issues/623), [Linux resize report](https://github.com/anthropics/sandbox-runtime/issues/642).
- Linux Landlock is unprivileged kernel access control with feature/ABI differences across kernels. Bubblewrap explicitly leaves policy construction to its caller. gVisor interposes a userspace application kernel and requires qualifying application compatibility. None is a portable drop-in Windows/macOS sandbox. [Landlock](https://docs.kernel.org/userspace-api/landlock.html), [Bubblewrap](https://github.com/containers/bubblewrap), [gVisor](https://gvisor.dev/docs/).
- OpenAI describes its native Windows sandbox in terms of dedicated security principals, restricted access and network controls. This is useful design evidence, not proof the same mechanism can be transplanted into CrazyAgent or applied to Claude without additional work. [OpenAI Windows sandbox engineering](https://openai.com/index/building-codex-windows-sandbox/).
- Docker Sandboxes now provides standalone sbx: local requirements are macOS 14+ Apple Silicon, Windows 11 x64 with Windows Hypervisor Platform, or supported Ubuntu 24.04+ with KVM and appropriate permissions. Docker Desktop is not required. Local use requires Docker sign-in. [Install requirements](https://docs.docker.com/ai/sandboxes/install/).
- Docker's security documentation describes a separate Linux microVM/kernel, a private Docker engine, explicit workspace sharing and a host credential proxy. Direct workspace mode is read/write; clone mode and mountless mode change that exposure. Shared skills and host-side MCP servers are explicit trust-boundary exceptions. [Docker isolation](https://docs.docker.com/ai/sandboxes/security/isolation/), [Docker security model](https://docs.docker.com/ai/sandboxes/security/).
- Docker local credentials can use host-managed OAuth and injected API headers, with optional passthrough weakening token isolation. SSH-agent forwarding exposes signature operations, even while private keys remain on the host. Its credential boundary is distinct from the application login and provider permissions. [Docker credentials](https://docs.docker.com/ai/sandboxes/configuration/credentials/).
- The Docker Claude guide supports a Claude subscription through native /login when no API key is set. Its Codex guide supports host OAuth, does not automatically import host user-level Codex configuration and documents a default command bypassing Codex's own approvals/internal sandbox. Consequently sbx is not behaviorally identical to hosting the user's current native CLI session. [Claude guide](https://docs.docker.com/ai/sandboxes/agents/claude-code/), [Codex guide](https://docs.docker.com/ai/sandboxes/agents/codex/).
- Docker documents free local compute/CLI, separate cloud charges and required sign-in. Those statements do not establish that all sbx implementation and redistribution rights are open source; this research did not locate a source repository/license establishing that. Keep it an optional external runtime pending license review. [Docker FAQ](https://docs.docker.com/ai/sandboxes/faq/).
- Gauntlet is Apache-2.0 and offers test-integrity checks, approved baselines and a selftest applying ten tampering categories. Its published evidence reports false positives as well as caught tampering. That makes it a relevant detector and corpus to qualify, not an independent OS isolation boundary or mathematical proof. [Gauntlet README](https://github.com/matthewjones372/gauntlet), [license](https://github.com/matthewjones372/gauntlet/blob/main/LICENSE), [published evidence](https://github.com/matthewjones372/gauntlet/blob/main/docs/evidence.md).

### Inferences

#### Threat and data flow

The trusted owner controls approvals in the TUI or desktop. The user-local engine owns policy versions, approved baseline material and accepted receipts. A coding worker can edit only its candidate project/configuration scope and send bounded project messages. A verification worker receives an immutable candidate snapshot, the approved checks and a restricted execution environment. It can write scratch output, but it cannot replace the baseline or write accepted evidence. The engine starts and records the run, validates its association with the snapshot and policy, and exposes status to both clients.

Treat the complete native CLI, its shell children, filesystem tools, project hooks, plugins and MCP subprocesses as coding-side execution. Confining only Bash leaves other write tools free to reach the controller. Likewise, sandbox candidate compilation, dependency install hooks, test setup, test discovery and cleanup, not merely the final test command. No Docker socket, host session token, application control socket, SSH agent, broad home mount, writable executable/configuration path or policy approval channel should cross into those workers by accident.

Host owner and host OS are trusted in this bounded model. No local application can promise protection against an administrator or compromised kernel. An unsandboxed same-UID coding process is already inside the owner's authority and can edit a user-writable verifier executable, change a policy file, read an IPC token or impersonate a client. Unix permissions and an extra folder cannot solve that. Stronger IPC authentication does not repair a missing worker execution boundary.

#### Concrete v1 route to qualify

First try an external maintained runtime around the entire native CLI and every tool it can launch on macOS. Anthropic sandbox-runtime is the shortest reusable candidate because it can wrap arbitrary commands, rather than reimplementing Seatbelt. Supply a fixed trusted profile: restrict writes to the assigned worktree and designated native profile state; deny controller, trusted baseline, receipt store and application executable/configuration paths; remove control tokens and dangerous inherited handles; deny application IPC, unnecessary local sockets and network access to the engine. Deny Apple Events and host-launch escape paths. A profile existing on disk is not qualification: prove its actual effects with hostile worker commands.

Native login, browser opening, Keychain access, plugin behavior, native interactive input and resize inside this outer boundary remain unproven. The engine can mediate an owner-approved external URL rather than giving every worker arbitrary host application launch privileges. Avoid making application-control permissions a workaround for authentication. If native auth cannot operate under a boundary without exposing protected authority, this route fails and must not silently run unsandboxed.

The preferred qualification path is therefore the native whole-process boundary first, not a mandatory VM product dependency. The runtime itself is distributed through the Node/npm ecosystem; the documented native path has platform tools such as ripgrep on macOS and bubblewrap/network/seccomp prerequisites on Linux. This is additional runtime and packaging work despite a Rust engine. An existing dependency is not cost-free just because its source is reusable. [Runtime setup](https://github.com/anthropics/sandbox-runtime/blob/main/README.md).

The stronger optional fallback is to host the CLI inside a qualified sbx microVM using its supported login flow, plus a separate disposable verification environment with only approved inputs. Import selected supported global/project configuration explicitly; do not mount the whole home directory or share writable skills across trust domains. Confirm changed approval semantics, account identity, subscription authorization and provider-specific profile concurrency. Do not choose sbx by assuming the native app-server path, exact PTY UX or multiple identities are already supported through its wrapper. It also excludes Intel Macs and unsupported Linux distributions for its documented local path.

The fallback adds a Docker account, external sbx installation, Linux guest/toolchain images, hardware virtualization and additional guest memory/disk/startup work. Multiple simultaneous workers multiply resource pressure; reuse a stopped or reset qualified worker only when its isolation and cleanliness are preserved. This is an architectural cost inference from the documented VM model, not a measured overhead number. No source here establishes that sbx meets the desired idle-memory or startup budget. Measure engine-only, TUI-plus-engine, desktop-plus-engine and each sandbox workload separately before calling the Rust product lightweight. Security takes precedence over those measurements; failure of the preferred secure path moves the date or supported scope instead of silently weakening protection.

Use a qualified existing sandbox/container or microVM for candidate verification; no credentials or engine control access are needed there. Ordinary containers can reduce filesystem exposure if configured correctly, but are not equivalent to microVM kernel separation. Run the chosen pilot language/toolchain genuinely inside the worker. Native macOS Accessibility/UI tests and native Windows tests require separate native qualification and cannot be represented as passed by Linux-based workers.

If neither whole-coder boundary preserves the required native workflow, the honest outcomes are extending the qualification window or explicitly shipping an unprotected experimental mode with verification independence unavailable. The latter does not satisfy the already stated integrated v1 acceptance criterion and therefore changes the release scope; it is not an implementation shortcut to treat as approved.

#### Bounded code and test-integrity semantics

An owner approves the baseline and commands outside coder-writable storage. Baseline changes, disabled checks, lower thresholds and test exclusions are proposals until the owner explicitly accepts them. Shared project memory and peer messages cannot authorize a policy change. Test maintenance is allowed through this flow; legitimate product/API changes can require a new baseline and should show a pending decision rather than a false failure or silent blessing.

Snapshot inputs include actual uncommitted code and relevant untracked files, tests, configuration and lockfiles, with an explicit manifest of included paths. Validate path traversal and symlink behavior; do not mount the live worktree or its shared .git metadata into the verification worker. The snapshot's content identity, policy/baseline identity, runner/toolchain identity, commands, test discovery, outcomes and evidence hashes are bound in a receipt written by the controller. The worker cannot create an authoritative pass by planting a file with a convenient name. A normal content hash is not an authenticity guarantee if its writer is untrusted.

The minimum integrity checks reject missing/deleted protected tests, changed protected rules, reduced approved thresholds, unexpected zero-test discovery, planted result files and a known-bad candidate with vacuous always-pass tests. Use a small qualified detector such as Gauntlet where it genuinely saves work. Run one or more known-bad changes or negative controls appropriate to the pilot toolchain, and report sensitivity for those cases. Mutation scores and negative controls do not establish universal test correctness.

Candidate code can be actively malicious within a test runtime, including tampering with in-process observations or forging report output. A read-only baseline does not remove that possibility. The trusted controller should own the invocation, observation channel and receipt creation, not trust a repository JUnit file or exit zero alone. Black-box holdouts across a separate process boundary strengthen checks for chosen observable behaviors. The product should say which checks actually executed and what attack classes were exercised, not claim an unforgeable semantic proof that arbitrary code and tests are correct.

Any candidate edit after capture makes the previous result stale for the working state. Watcher events are hints; compare snapshot identity again before displaying a current pass or performing an action dependent on that pass. Failures, unsupported checks, pending baseline approval and incomplete infrastructure remain distinct visible statuses in both clients.

#### Platform qualification matrix

| Target | Reusable boundary to evaluate | Product claim before hands-on qualification |
| --- | --- | --- |
| macOS Apple Silicon | Whole-process sandbox-runtime/Seatbelt; sbx microVM optional | Primary v1 qualification target. Native Keychain/login, PTY lifecycle and protected authority still blocking evidence. |
| macOS Intel | Whole-process sandbox-runtime/Seatbelt | Rust/UI support is separate from verification support. sbx local docs do not cover Intel Mac. |
| Linux | sandbox-runtime with bubblewrap/seccomp/network prerequisites; Landlock as OS-specific alternative; sbx on supported Ubuntu/KVM | Kernel, distro, user namespaces, PTY resize and credential store vary. No generic all-Linux parity claim. |
| Windows | sandbox-runtime native account/WFP alpha; provider-specific native sandbox; sbx on documented x64 target | ConPTY/stdin, profile access, setup privileges, pipe ACLs and job/process lifecycle need real tests. Provider sandbox is not automatically a common sandbox. |

### Gaps

No runtime was installed, downloaded code executed, sandbox escape attempted, native account authenticated or model charged. There is no demonstrated implementation satisfying both native auth and verifier authority yet. The controller's own frontend attack surface, updater and packaging trust need ordinary implementation/security review later; research is not a completed audit. Gauntlet's exact API fit and detector precision for the chosen project remain unqualified. Proprietary runtime dependencies, redistribution terms, provider-auth terms and multiple-profile behavior need explicit decisions before adoption.

## 3. Which decisions and roadmap changes should the grilling session resolve?

### Takeaway

Keep the approved integrated product, but replace the old 22-hour desktop-oriented qualification assumption with a dual-client qualification budget. The added interface is real work even with a shared engine. Recommend protecting the integrated workflow and both clients before allocating November time to a third harness or native UI picker.

### Cited Findings

- Native process launch, transport security and buffering are already supplied by the cited operating-system/Rust libraries, while sandbox-runtime's current interactive reports demonstrate that assembling them still needs real integration testing. These support reuse plus qualification, not a claim that both clients are free. [Tokio process](https://docs.rs/tokio/latest/tokio/process/index.html), [Windows stdin report](https://github.com/anthropics/sandbox-runtime/issues/623), [Linux resize report](https://github.com/anthropics/sandbox-runtime/issues/642).
- Sandbox product platform prerequisites and provider authentication/configuration paths differ. Cross-platform Rust compilation therefore cannot establish equivalent workflow qualification. [Docker prerequisites](https://docs.docker.com/ai/sandboxes/install/), [Codex sandbox configuration](https://docs.docker.com/ai/sandboxes/agents/codex/), [native Windows sandbox design](https://openai.com/index/building-codex-windows-sandbox/).

### Inferences

#### Proposed numbered architecture decisions, all awaiting user agreement

| Candidate | Recommended decision | Alternative and consequence |
| --- | --- | --- |
| ADR-E01 | One user-local Rust owner process, reusable Rust domain code and two clients; sessions detach on client close | Separate library instance in each UI is simpler to start but creates duplicate live ownership or requires a different no-concurrent-clients product promise. |
| ADR-E02 | Same project/session/configuration/verification semantics in both clients; TUI is primary daily driver | Full pixel-equivalent parity wastes time; desktop-only required behavior would violate the user's primary TUI workflow. |
| ADR-E03 | Local native IPC, bounded versioned operations, reconnect snapshot and one terminal input owner | Loopback HTTP may simplify tools later but adds an unnecessary network interface now. |
| ADR-E04 | One rusqlite worker and local metadata; native credential ownership | SQLx is acceptable if it saves demonstrated work. Per-client databases and custom token synchronization add coordination risk. |
| ADR-E05 | Whole-coder confinement plus separate candidate verification worker, trusted host policy/receipts, bounded integrity claims | Merely placing verifier files elsewhere cannot satisfy the stated protection. A same-UID unsandboxed agent forces a changed security claim. |

#### Revised qualification proposal

The following is a planning estimate, not measured implementation effort. Timeboxes are decision limits and retained prototype work, not a promise every spike will succeed.

| Qualification slice | Owner hours | Required result |
| --- | --- | --- |
| Both clients attach to one owner | 5-7 | TUI and desktop see one project/session; closing either leaves session owned; no duplicate start/input. |
| Native interactive routes and profiles | 6-10 | Claude/Codex login, native input/approval/resize, account identity and explicit exit behavior; supported profile concurrency evidence. |
| Shared context and project messages | 4-6 | One scoped memory/configuration update and two-way bounded message visible in both clients and harnesses. |
| Protected code and test integrity | 10-14 | Full coder/test confinement, protected baseline/receipt write authority, negative control and planted-result/tamper rejection. |
| Client recovery and packaging feasibility | 4-7 | Attach/reconnect, lag recovery, initial native packaging and dependency inventory with no false platform claim. |

Total: 29-44 owner hours of qualification, replacing the prior 22-hour assumption. At the established 30-40 gross hours/week and 20 percent reserve, feature capacity is 24-32 hours/week. These spikes alone consume about 0.9-1.8 feature weeks. They should end with an evidence-based forecast; reaching the high end leaves too little room to promise the original October 30 fully integrated alpha.

Recommend October 30 as the first integrated demonstrator target with both clients, not an unconditional public-quality commitment. November 6 is a more defensible alpha fallback if the full boundary and session routes qualify. November 13 remains the real-use and verification gate. Preserve the November 30 goal for a qualified macOS beta with both clients and two harnesses, while treating the optional third harness/UI picker as first scope to drop. This is a proposed adjustment to the approved dates, not an accepted schedule change.

Do not duplicate all UI polish by launch. Both clients need the same core flow: create/select project, start and inspect a harness session, use shared state/messages, approve native permission requests, run verification and see current/stale/failed evidence. Optional desktop visual affordances can differ from TUI navigation. Platform target design should be established now, but full Linux/Windows qualification needs a separate bounded release commitment and real machines.

#### First grilling question recommendation

Ask the owner whether sessions should continue when all client windows/terminals close, and whether the TUI and desktop should be able to attach to the same live project simultaneously. Recommend yes to both, with explicit stop-session/stop-engine actions and one terminal input owner. This is a genuine product decision that determines whether the proposed local engine owner is necessary. The next branch is the protected runtime and native authentication tradeoff, followed by the exact parity boundary and pilot project/toolchain. Record each answered decision immediately with a stable identifier, rationale, rejected alternatives, risks and acceptance evidence.

### Gaps

No updated dates, service topology, lifecycle promise or platform release ordering has been approved. The 29-44-hour range must be revised against actual spikes and the user's availability. The parent UI/harness researchers are covering Tauri/Ratatui and adapter specifics; this note does not select those frameworks or invent performance measurements for them.
