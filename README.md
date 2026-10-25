<picture>
  <source media="(prefers-color-scheme: dark)" srcset="apps/desktop/src/assets/brand/lockup-dark.svg">
  <img src="apps/desktop/src/assets/brand/lockup.svg" alt="CrazyAgent" width="360">
</picture>

# CrazyAgent

CrazyAgent runs one master agent that plans your work and hands it to sub-agents. Each agent can use any coding harness you choose, runs in a sandbox for its project, and works with your own accounts and pools.

A Rust engine owns all state and policy. You drive it from a terminal UI or a desktop app.

## Features

- **Accounts and pools.** Sign in with your own provider accounts and group them into pools that agents draw from.
- **Projects with research agents.** Add a project folder and let research agents study the code and your documents to build project context.
- **Team.** The master agent splits work into tasks and runs sub-agents on them, each in its own Git worktree, on harnesses such as Claude Code, Codex, Gemini CLI, OpenCode, Cursor CLI or Aider.
- **Plugins, skills and MCP.** Add skills (folders with a `SKILL.md`) and MCP servers for every project or for one project only.
- **Memory approval.** Nothing enters a project's long-term memory until you approve it.
- **Desktop app and TUI.** Two complete clients for the same engine. Use either one, or both at once.

## Install

Download from GitHub Releases:

- Stable: [latest release](https://github.com/ChaoticQubit/CrazyAgent/releases/latest)
- Dev, rebuilt from every merge to `master`: [dev prerelease](https://github.com/ChaoticQubit/CrazyAgent/releases/tag/dev)

Each release lists a `SHA256SUMS` file. Check a download against it before you open it:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing
```

### macOS

1. Download `CrazyAgent_<version>_aarch64.dmg` for Apple silicon, or `CrazyAgent_<version>_x64.dmg` for an Intel Mac.
2. Open the disk image and drag CrazyAgent into Applications.
3. Open CrazyAgent from Applications. On first launch it installs the engine as a background service and offers to install the command line tool.

Stable releases are signed and notarized. A dev release whose notes say "Not code signed" is blocked by macOS on first open: open it once, then choose Open Anyway in System Settings > Privacy & Security.

### Windows

1. Download `CrazyAgent_<version>_x64-setup.exe`.
2. Run it and follow the installer.
3. Open CrazyAgent from the Start menu. On first launch it installs the engine as a background service and offers to add the command line tool to your `PATH`.

A dev release whose notes say "Not code signed" shows a SmartScreen warning: choose More info, then Run anyway.

### Linux

Download `CrazyAgent_<version>_amd64.AppImage` and make it executable, or install the Debian package:

```sh
sudo apt install ./CrazyAgent_<version>_amd64.deb
```

### Command line only

1. Download `crazyagent-<version>-<target>.tar.gz` for your platform (`.zip` on Windows). The target is `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` or `x86_64-pc-windows-msvc`.
2. Extract it into a folder on your `PATH`, such as `~/.local/bin`. Keep `crazyagent`, `crazyagent-rcli` and the helper binaries together in that folder.
3. Install and start the engine service for your user account:

```sh
crazyagent service install
```

The service is a launchd agent on macOS, a systemd user unit on Linux and a logon task on Windows. `crazyagent service status` shows whether it runs, and `crazyagent service uninstall` removes it.

## Build requirements

- macOS
- Rust toolchain with edition 2024 support (tested with Rust 1.99.1)
- Node.js and npm, for the desktop app
- The harnesses you plan to use, installed and on your `PATH`

## Build

```sh
cargo fetch --locked
make build
```

The Makefile runs Cargo offline, so fetch the locked dependencies once first. The binary is `target/debug/crazyagent`.

## Run the engine

Pick a private folder for the engine, write a minimal settings file, then start the engine:

```sh
mkdir -m 700 -p "$HOME/.crazyagent"
printf 'version = 1\ntimeout_ms = 5000\nallowed_destinations = []\n' > "$HOME/.crazyagent/global.toml"
chmod 600 "$HOME/.crazyagent/global.toml"
target/debug/crazyagent engine "$HOME/.crazyagent" "$HOME/.crazyagent/global.toml" "$HOME/.crazyagent/project.toml" engine.sock --writable-settings
```

The engine prints `ready` and listens on `$HOME/.crazyagent/engine.sock`. Stop it with Ctrl-C.

## Run the TUI

```sh
target/debug/crazyagent tui "$HOME/.crazyagent/engine.sock"
```

## Run the desktop app

```sh
cd apps/desktop
npm ci
CRAZYAGENT_DESKTOP_SOCKET="$HOME/.crazyagent/engine.sock" npm run tauri dev
```

## Tests

```sh
make check
```

This checks formatting and lint, runs the Rust and Node test suites, builds, and runs the tests again. Run `make help` for the narrower commands.

## Security model

- The engine is the only authority: it owns policy, stored state, credentials and worktrees.
- The TUI and the desktop app never hold secrets. They pass a secret to the engine once and from then on see only a reference to it.
- Harnesses run inside a macOS Seatbelt sandbox scoped to their project.
- An MCP server stays off until you approve its exact command or URL, and any change to it needs a new approval.

## License

CrazyAgent is licensed under [Apache-2.0](LICENSE). Third-party code keeps its own license.
