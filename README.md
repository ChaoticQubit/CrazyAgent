# CrazyAgent

CrazyAgent is an open-source development environment for coordinating native coding harnesses through one central conversation, shared project knowledge and independent verification.

The accepted direction is a Rust engine, a Ratatui/Crossterm TUI and a Tauri 2 desktop client with React/TypeScript. Initial native qualification targets macOS with Claude Code, Codex and an RCLI leader foundation.

## Development status

Repository foundation only. The Rust workspace compiles; application behavior and native runtime capabilities are not implemented or qualified yet. The first implementation slice is evidence-based qualification admission.

[Accepted decisions](docs/agent-decisions.md), [architecture](docs/superpowers/specs/2026-10-17-crazyagent-architecture-design.md), [implementation plan](docs/superpowers/plans/2026-10-17-crazyagent-implementation-plan.md) and [execution status](docs/development/execution-status.md) are the project context.

## Local commands

Run `make check` to check formatting and lint, run tests, build and run tests again. The Rust foundation uses the installed toolchain and offline Cargo commands. No dependency installation is required for this initial empty library.

Run `make help` for individual commands. A zero-test foundation run verifies the scaffold only; it does not prove any application behavior.

## Contributions

Read [AGENTS.md](AGENTS.md) before changing source. Work on isolated task branches and preserve the real-test requirements. Code and security reviews are deferred by explicit owner instruction for the current development phase.

## License

CrazyAgent's original code uses [Apache-2.0](LICENSE). The canonical license text comes from the [Apache Software Foundation](https://www.apache.org/licenses/LICENSE-2.0.txt). Third-party code and research retain their own terms.
