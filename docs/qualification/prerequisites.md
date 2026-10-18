# Runtime prerequisites inventory

Observed October 17, 2026, on branch `docs/runtime-readiness`, starting at `8e9293f006d2a9b4ac35faaf89744b753a8e5256`. This is an offline metadata inventory supporting Tasks 1-3, not runtime qualification, dependency resolution, a build result or a security review.

Authority: [DEC-059 through DEC-062](../agent-decisions.md), [adopted architecture](../superpowers/specs/2026-10-17-crazyagent-architecture-design.md), [implementation plan](../superpowers/plans/2026-10-17-crazyagent-implementation-plan.md) and the assigned runtime-readiness brief. The design and execution method are approved. Code and security reviews are deferred under DEC-061, never passed. The leader owns decision records, integration and shared graph refreshes; the qualification worker owns Rust builds.

## Result and boundaries

Task 1 can select locally cached serialization and SHA-256 crates without requesting downloads. Their default dependency paths were inspected as metadata; Cargo resolution, compilation and real admission tests remain the qualification worker's responsibility. The starting workspace has only the local `crazyagent 0.1.0` package in Cargo.lock and no external dependencies in its manifests.

RCLI and the standalone Anthropic sandbox-runtime entry points were absent from PATH and the inspected global package locations. No qualifying runtime/profile was established. Native isolation, original-worker recovery, authentication and individual native child controls are all **UNQUALIFIED**. PATH presence, a version string, a help option or cached source never admits these capabilities.

No install, download, build, service start, provider call, auth command, account connection, credential inspection, scanner, review, push or external PR was performed. No shared graph update was performed. Existing untracked `.serena/` state was left untouched. Only this document is owned by this worker.

## Installed tool metadata

`shutil.which(name)` supplied PATH locations. Version commands below were run directly with captured output and an eight-second timeout, except Docker's five-second timeout. Successful commands exited zero. No package-manager execution wrapper such as npx or bunx was used.

| Read-only command or lookup | Observed result | Evidence limit |
| --- | --- | --- |
| `sw_vers` | macOS 26.6.2, build 25G83 | Host metadata only |
| Python `platform.system(), platform.release(), platform.machine()` | Darwin, 25.6.0, arm64 | No resource/load qualification |
| `rustc --version` | `rustc 1.99.1 (58cf8f9c3 2026-10-13)` | `/opt/homebrew/opt/rustup/bin/rustc`; no compile run |
| `cargo --version` | `cargo 1.99.1 (5f94df478 2026-08-27)` | Same rustup bin directory; no dependency resolution |
| `rustfmt --version` | `rustfmt 1.10.0-stable (58cf8f9c35 2026-10-13)` | Formatter installed, not run on source |
| `cargo-clippy --version` | `clippy 0.1.99 (58cf8f9c35 2026-10-13)` | Lint not run |
| `make --version` | GNU Make 3.81, `/usr/bin/make` | No make target run |
| `bun --version` | 1.4.3, `/opt/homebrew/bin/bun` | No package install or execution |
| `node --version` | v26.12.0, `/opt/homebrew/bin/node` | No application run |
| `npm --version` | 11.20.0, `/opt/homebrew/bin/npm` | No install or remote query |
| `codex --version` | `codex-cli 0.163.0` | Resolves to `/opt/homebrew/Caskroom/codex/0.163.0/bin/codex`; auth uninspected |
| `codex --help` | Advertises app-server, sandbox, resume, fork and agents commands | Help is not a tested adapter/control protocol |
| `codex app-server --help` | Experimental; advertises stdio and Unix socket transports | Only help executed; no server launched |
| `claude --version` | Timed out after eight seconds, no version result | `/opt/homebrew/bin/claude` resolves to `/opt/homebrew/Caskroom/claude-code@latest/2.1.301/claude`; 2.1.301 is installation-path metadata only |
| `docker --version` | `Docker version 29.8.2, build 7fc2dff` | `/usr/local/bin/docker` resolves into Docker.app; daemon availability untested |
| `sqlite3 --version` | 3.51.0, build date 2025-06-12, 64-bit | `/usr/bin/sqlite3`; CLI existence does not prove Rust linkage |
| `rg --version` | ripgrep 15.2.0 | Installed Codex-bundled rg |
| `xcode-select -p` | `/Applications/Xcode.app/Contents/Developer` | Selected developer path only |
| `xcrun --find clang` | `/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang` | Compiler location only; no compilation |

PATH lookups found `/usr/bin/sandbox-exec`. Lookups found no `rcli`, `RCLI`, `srt`, `sandbox-runtime`, `socat`, `bwrap`, `podman` or `limactl`. This is bounded PATH evidence, not a machine-wide absence claim. No sandbox-exec probe or container operation ran; neither is an admitted replacement for the selected runtime candidate.

Claude's timeout cause is unknown. No repeated startup, diagnostic, config inspection or auth repair was attempted. A bounded local startup investigation is needed before relying on that executable.

Global package.json metadata was read under `/opt/homebrew/lib/node_modules`, including scoped package directories. No RCLI or `@anthropic-ai/sandbox-runtime` package appeared there. `~/.bun/install/global/node_modules` and `~/.local/share/claude/versions` were absent. Relevant installed package metadata: `@tauri-apps/cli 2.12.3`, license `Apache-2.0 OR MIT`, at `/opt/homebrew/lib/node_modules/@tauri-apps/cli/package.json`. This does not establish desktop build or packaging readiness. Native Claude/Codex binary licenses were not established from the inspected local installation metadata.

## Offline Rust candidates for Task 1

Registry identifier: `index.crates.io-1949cf8c6b5b557f`. Inspected local roots:

| Evidence | Location under `~/.cargo/registry/` |
| --- | --- |
| Extracted package manifests and source | `src/index.crates.io-1949cf8c6b5b557f/<name>-<version>/` |
| Downloaded package archives | `cache/index.crates.io-1949cf8c6b5b557f/<name>-<version>.crate` |
| Sparse index records | `index/index.crates.io-1949cf8c6b5b557f/.cache/` |

Candidate selection uses existing crates for report serialization and evidence hashing; stdlib supplies filesystem/path handling and CLI arguments. No extra async, persistence, error, hex or test framework dependency is necessary merely to implement Task 1 admission. The implementer must still preserve source/profile binding, intact artifact verification and evidence-root confinement.

| Candidate selection | Purpose | Local license and source metadata | Local Rust minimum |
| --- | --- | --- | --- |
| `serde = { version = "=1.0.229", features = ["derive"] }` | Typed report serialization | MIT OR Apache-2.0; `https://github.com/serde-rs/serde` | serde 1.56; derive 1.71 |
| `serde_json = "=1.0.154"` | JSON report read/write | MIT OR Apache-2.0; `https://github.com/serde-rs/json` | 1.71 |
| `sha2 = "=0.10.9"` | SHA-256 artifact digests | MIT OR Apache-2.0; `https://github.com/RustCrypto/hashes` | No rust-version field in inspected package manifest |

These are inventory candidates, not changes to Cargo manifests. Each selected direct crate has extracted source, a cached archive and a matching local sparse-index record. Archive SHA-256 matched that record's checksum and each record reported `yanked: false`. Matching locally cached records proves local consistency, not independent upstream authenticity or a vulnerability assessment.

The same checksum/non-yanked checks passed for these cached supporting versions:

| Dependency path | Cached supporting versions inspected |
| --- | --- |
| Serde core and derive | serde_core 1.0.229, serde_derive 1.0.229 |
| Derive macros | proc-macro2 1.0.107, quote 1.0.47, syn 3.0.8, unicode-ident 1.0.26 |
| JSON defaults | itoa 1.0.18, memchr 2.8.3, zmij 1.0.23 |
| SHA-256 default digest path | cfg-if 1.0.5, digest 0.10.7, crypto-common 0.1.7, block-buffer 0.10.4 |
| Digest support and Apple arm64 detection | generic-array 0.14.7, typenum 1.20.1, version_check 0.9.5, cpufeatures 0.2.17, libc 0.2.190 |

The inspected serde_derive 1.0.229 explicitly requires syn major 3. Cached syn 2.0.119 was also checksum-checked but is not the derive dependency for this candidate. Do not substitute an assumed major version from older Serde releases. Cached manifests declare unicode-ident's license as `(MIT OR Apache-2.0) AND Unicode-3.0`, memchr's as `Unlicense OR MIT`, zmij's as MIT and generic-array's as MIT. Preserve transitive notices when distribution begins.

Cargo's actual feature/target selection and lockfile are not resolved here. Offline Cargo errors remain blockers for the implementing worker to report; source/cache presence is not a green build. No cargo metadata or lockfile generation was run in this worker's checkout.

## Later-task dependency inventory

These versions have extracted source and `.crate` archives; they were not compiled or checksum-validated as a complete dependency closure in this slice.

| Package | Cached versions | Local license/source metadata | Qualification limit |
| --- | --- | --- | --- |
| tokio | 1.53.1, 1.53.2 | MIT; `https://github.com/tokio-rs/tokio` | Features and transitive closure not resolved |
| toml | 0.8.2 | MIT OR Apache-2.0; `https://github.com/toml-rs/toml` | Parser integration untested |
| rusqlite | 0.40.2 | MIT; `https://github.com/rusqlite/rusqlite` | Default feature `cache` requires hashlink 0.12; no extracted hashlink source found |
| libsqlite3-sys | 0.38.2 | MIT; same rusqlite repository | sqlite3/sqlite3.c is present; C compilation/linkage untested |
| tempfile | 3.27.0 | MIT OR Apache-2.0; `https://github.com/Stebalien/tempfile` | Not required merely to create real stdlib temporary fixtures |
| thiserror | 1.0.69, 2.0.21 | MIT OR Apache-2.0; `https://github.com/dtolnay/thiserror` | Optional convenience, not a Task 1 prerequisite |
| hex | 0.4.3 | MIT OR Apache-2.0; `https://github.com/KokaKiwi/rust-hex` | Optional convenience, not a Task 1 prerequisite |
| tauri | 2.12.0, 2.12.1 | Apache-2.0 OR MIT; `https://github.com/tauri-apps/tauri` | Declared Rust minimum 1.90; desktop closure/build/signing untested |
| ratatui, crossterm | No extracted source found in the inspected registry | Version/license not resolved here | No complete offline TUI dependency set demonstrated |

For native macOS, rusqlite 0.40.2 directly requires libsqlite3-sys 0.38.2. A possible later offline choice is `default-features = false` with only necessary features, avoiding optional statement caching; that is an implementer decision requiring real resolution and tests, not an admitted configuration. Enabling `bundled` uses cached C source and needs a functioning C toolchain. Cached build-dependency candidates include cc 1.5.1/1.6.0, pkg-config 0.3.34 and vcpkg 0.2.15; the full build closure remains unverified. Do not install hashlink or alter feature semantics silently.

The npm cache index at `~/.npm/_cacache/index-v5` and Bun cache directory `~/.bun/install/cache` exist. A bounded metadata scan found zero npm index keys matching `@anthropic-ai/sandbox-runtime`, `backboard-r-cli` or `/rcli`, and zero Bun directory names matching sandbox-runtime, r-cli, rcli, react@, vite@ or typescript@. This does not prove every possible cache alias or nested project dependency absent, and does not establish a complete frontend or runtime installation set.

## Runtime blockers and authorization requirements

| Requirement | Current evidence | Concrete prerequisite before qualification |
| --- | --- | --- |
| RCLI leader | No inspected executable/global package/cache match | Select and authorize acquisition of a pinned sidecar/source and its dependencies; record artifact checksum and license notices before execution |
| Anthropic sandbox-runtime candidate | No inspected `srt`/sandbox-runtime executable or global package; no matching cache metadata | Authorize exact package/version/source acquisition after resolving metadata; then qualify macOS backend, helper dependencies and the declared profile |
| Native Claude/Codex authentication | Not inspected or exercised; Claude version timed out | Leader records approved nonsecret profile references, auth modes, model destinations and bounded call budget; no credential portability assumption |
| Execution isolation and resource enforcement | sandbox-exec and Docker client exist; no admitted runtime/profile | Real positive/negative filesystem, traversal/symlink, child inheritance, credential-path and network probes; record effective CPU/memory/process limits and unsupported controls |
| Live recovery and child controls | Codex advertises control-related commands; no live runner tested | Prove original runner/harness identity, retained native connection/capture, unchanged worktree, zero duplicate dispatch and truthful per-child cancellation under engine-only crash |

The existing local [RCLI reuse assessment](../../reports/RCLI%20leader%20reuse%20assessment.md) records upstream `Backboard-io/Backboard-R-CLI`, revision `51bee17e02344de5fffd89523ce2b73d5042f216`, manifest version 3.0.4 and MIT licensing. These are retained October 16 research observations, not newly fetched upstream metadata or evidence of a local installed artifact. They give a concrete acquisition candidate, not installation authorization. The assessment explicitly leaves subscription-credential reuse and persistent bidirectional control unproved.

For sandbox-runtime, DEC-059 selects Anthropic's candidate for evaluation. No installed version/license was available in the inspected locations; an exact acquisition version and source must be resolved before an installation request. No install command is recommended from unverified remote metadata.

After prerequisites are admitted, QF-01 requires original live-session recovery; QF-02 requires protected verifier authority; QF-03 requires independent complete TUI and desktop proofs; QF-04 requires shared project behavior; QF-05 requires load/shipping evidence. This inventory passes none of those gates. Engine-control endpoint denial remains a later Task 5 check, not evidence available now.

## Reproduction and evidence limits

Read-only graph queries ran from this worktree before relevant source inspection:

```text
graphify query 'Task 1 typed contracts persistence SQLite offline dependencies runtime qualification sandbox RCLI prerequisites'
graphify query 'Cargo.toml qualification.rs typed contract Capability QualificationReport artifact hashing serialization'
graphify query 'RCLI backboard-io backboard-r-cli repository license installation sandbox-runtime Anthropic npm'
```

All succeeded. Scoped graph results named the architecture/plan, Cargo dependency concepts and RCLI assessment documents used above. The shared cache was queried only.

The tool/version table records exact executable arguments and results. Package inventory used Python pathlib glob/read_text over package.json and cached Cargo.toml files, extracting only version, license, repository, rust-version, dependency and feature metadata. Sparse-index verification parsed NUL-delimited JSON records and compared `hashlib.sha256(archive.read_bytes()).hexdigest()` with the matching version's `cksum`. Crate names of length three use index path `3/<first-letter>/<name>`; longer names use `<first-two>/<next-two>/<name>`.

Two inventory-script errors were corrected without installation: the system Python is 3.9 and lacks tomllib, so scalar manifest metadata was extracted from the package section without that import; the first checksum traversal used the wrong three-letter index path and stopped at syn, then the corrected path completed the remaining checks. Neither error was a product-test failure or passing behavior evidence. No raw secret/config files were read, and no raw native-session artifacts were captured.

Next owner action: pass the Task 1 candidate versions to the qualification worker for real offline resolution and tests; keep Tasks 2-3 unavailable until the leader resolves the explicit runtime/profile prerequisites.
