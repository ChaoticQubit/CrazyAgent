# CrazyAgent

CrazyAgent is an open-source development environment for coordinating native coding harnesses through one central conversation, shared project knowledge and independent verification.

The accepted direction is a Rust engine, a Ratatui/Crossterm TUI and a Tauri 2 desktop client with React/TypeScript. Initial native qualification targets macOS with Claude Code, Codex and an RCLI leader foundation.

## Development status

The repository foundation and first qualification/admission CLI slice are implemented. Admission checks trusted source/profile identity, required outcomes, bounded real evidence files and SHA-256 integrity. Both interfaces, the shared engine and native runtime workflows remain unimplemented or unqualified.

[Accepted decisions](docs/agent-decisions.md), [architecture](docs/superpowers/specs/2026-10-17-crazyagent-architecture-design.md), [implementation plan](docs/superpowers/plans/2026-10-17-crazyagent-implementation-plan.md) and [execution status](docs/development/execution-status.md) are the project context.

## Local commands

Run `make check` to check formatting and lint, run tests, build and run tests again. The integrated qualification slice passes 11 unit tests and 3 actual-binary integration tests, with none ignored. Cargo commands are offline and require the locked dependencies already available in your local cache; a fresh machine may need an explicitly authorized dependency acquisition step.

Run `make help` for individual commands. The tested toolchain is Rust/Cargo 1.99.1; other versions have not been qualified. The initial worker shell resolved legacy Cargo 1.71.2; selecting the already-installed Rust/Cargo 1.99.1 through process-local PATH resolved that environment mismatch without an installation.

Print an honest report without provider calls:

```sh
cargo run --offline -- qualification report --source-revision local --profile-id macos-local-unqualified
```

The report marks all native capabilities unavailable until real qualification evidence exists. The [qualification contract](docs/qualification/capabilities.md) documents admission arguments and evidence limits. Unit and CLI tests establish admission behavior; they do not qualify sandboxing, native authentication, live recovery or child controls.

## Contributions

Read [AGENTS.md](AGENTS.md) before changing source. Work on isolated task branches and preserve the real-test requirements. Code and security reviews are deferred by explicit owner instruction for the current development phase.

## License

CrazyAgent's original code uses [Apache-2.0](LICENSE). The canonical license text comes from the [Apache Software Foundation](https://www.apache.org/licenses/LICENSE-2.0.txt). Third-party code and research retain their own terms.
