import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import test from "node:test";

const WORKFLOWS = new URL("../../.github/workflows/", import.meta.url);

async function workflow(name) {
  const text = await readFile(new URL(name, WORKFLOWS), "utf8");
  assert(!text.includes("\r"), `${name} has LF line endings`);
  return text;
}

/**
 * Returns the lines of one top-level section (`on:`, `jobs:`) or of one job under `jobs:`.
 * Inputs: the workflow text, the indentation of the key and its name. Output: the key's line
 * and every following line indented deeper, so a test can read one block without a YAML parser.
 */
function block(text, indent, name) {
  const lines = text.split("\n");
  const start = lines.indexOf(`${" ".repeat(indent)}${name}:`);
  assert(start >= 0, `no ${name} block at indent ${indent}`);
  const end = lines.findIndex((line, index) => index > start && line.trim() !== "" && line.length - line.trimStart().length <= indent);
  return lines.slice(start, end < 0 ? lines.length : end);
}

test("CI runs for pull requests and as a reusable workflow; a pull request cancels its own older run, and a release's CI never shares a group with another ref or gets cancelled", async () => {
  const ci = await workflow("ci.yml");
  const on = block(ci, 0, "on");
  assert(on.includes("  pull_request:"));
  assert(on.includes("  workflow_call:"));
  assert.deepEqual(block(ci, 0, "concurrency").filter(Boolean), [
    "concurrency:",
    "  group: ${{ github.event_name == 'pull_request' && format('ci-pr-{0}', github.event.pull_request.number) || format('ci-{0}-{1}', github.ref, github.sha) }}",
    "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
  ]);
  assert.doesNotMatch(ci, /secrets\./);
});

test("a stable release ships only from a v* tag on a commit that master already holds", async () => {
  const release = await workflow("release.yml");
  const prepare = steps(release, "prepare");
  const checkout = prepare.find((step) => step.includes("uses: actions/checkout@"));
  assert.match(checkout, /\n {10}fetch-depth: 0(\n|$)/, "prepare sees master's history");
  const version = prepare.find((step) => step.includes("- id: version"));
  const tagBranch = version.slice(version.indexOf('if [ "$REF_TYPE" = tag ]; then'), version.indexOf("          else"));
  assert.match(tagBranch, /git merge-base --is-ancestor "\$GITHUB_SHA" refs\/remotes\/origin\/master \|\| \{ echo "::error::[^"]+"; exit 1; \}/);
  assert(tagBranch.indexOf("git merge-base") < tagBranch.indexOf("version.mjs stable"), "the commit is checked before a stable version is made");
  assert(block(release, 2, "publish").includes("    needs: [prepare, build]"), "nothing publishes without prepare");
});

test("signing secrets reach only the build job, which is bound to the release environment", async () => {
  const release = await workflow("release.yml");
  const jobs = block(release, 0, "jobs").join("\n");
  const build = block(jobs, 2, "build");
  assert(build.includes("    environment: release"), "build runs in the release environment");
  for (const job of ["prepare", "ci", "publish"]) {
    const lines = block(jobs, 2, job);
    assert(!lines.some((line) => line.includes("secrets.")), `${job} reads no secret`);
    assert(!lines.some((line) => /^ {4}environment:/.test(line)), `${job} needs no environment`);
  }
  assert(release.split("\n").filter((line) => line.includes("secrets.")).length > 0);
});

test("every release, dev or stable, builds only after the whole CI workflow passes, and CI gets no secrets", async () => {
  const release = await workflow("release.yml");
  const on = block(release, 0, "on");
  assert(on.includes("    branches: [master]"));
  assert(on.includes('    tags: ["v*"]'));
  const ci = block(release, 2, "ci");
  assert(ci.includes("    uses: ./.github/workflows/ci.yml"));
  assert.deepEqual(block(ci.join("\n"), 4, "permissions").map((line) => line.trim()), ["permissions:", "contents: read"]);
  assert(!ci.some((line) => /^\s+(secrets|with|if):/.test(line)), "the CI call takes no secrets, inputs or condition");
  assert(block(release, 2, "build").includes("    needs: [prepare, ci]"));
  assert(block(release, 2, "publish").includes("    needs: [prepare, build]"));
  assert(!/^ {4}if: .*(always|cancelled|failure)\(\)/m.test(release), "no job runs after a failed or cancelled dependency");
  assert.deepEqual(release.split("\n").filter((line) => /^\s+if: .*(always|cancelled|failure)\(\)/.test(line)).map((line) => line.trim()), ["if: always() && matrix.os == 'macos'", "if: always() && runner.environment != 'github-hosted'"], "only the keychain cleanup and the owner's runner cleanup run after a failure");
});

const RUNNERS = Object.freeze({
  linux: "ubuntu-22.04",
  macos: "macos-15",
  windows: "windows-2022",
});
const ROSETTA = "Refuse to run under Rosetta";

/**
 * Returns the steps of `job` in `workflow` text as one string per step.
 * Inputs: the workflow text and the job name. Output: each `- ` step with its indented lines.
 */
function steps(workflowText, job) {
  return block(block(workflowText, 0, "jobs").join("\n"), 2, job).join("\n").split(/\n(?= {6}- )/).slice(1);
}

test("every CI job runs on a GitHub-hosted runner, macOS on macos-15, and no workflow picks its runner from a pull request", async () => {
  const ci = await workflow("ci.yml");
  const jobs = block(ci, 0, "jobs").join("\n");
  for (const [job, runner] of Object.entries(RUNNERS)) assert(block(jobs, 2, job).includes(`    runs-on: ${runner}`), job);
  for (const name of ["ci.yml", "release.yml"]) {
    const text = await workflow(name);
    assert.doesNotMatch(text, /pull_request_target|head\.repo|self-hosted/, name);
    for (const line of text.split("\n").filter((line) => /^\s+runs-on:/.test(line))) {
      assert.match(line.trim(), /^runs-on: (?:ubuntu-22\.04|macos-15|windows-2022|\$\{\{ matrix\.runner \}\})$/, `${name}: ${line.trim()}`);
    }
  }
});

test("release builds both macOS targets on GitHub's macos-15 and Linux on ubuntu-22.04, with room for a cold build on three cores", async () => {
  const release = await workflow("release.yml");
  assert.deepEqual(block(release, 0, "on").filter((line) => /^ {2}\S/.test(line)), ["  push:"], "no pull request can start a release");
  const build = block(release, 2, "build");
  const matrix = block(build.join("\n"), 6, "matrix").join("\n");
  const entries = matrix.split(/\n(?= {10}- )/).slice(1);
  const macos = entries.filter((entry) => entry.includes("os: macos"));
  assert.deepEqual(macos.map((entry) => entry.match(/target: (\S+)/)[1]).sort(), ["aarch64-apple-darwin", "x86_64-apple-darwin"]);
  for (const entry of entries) {
    assert.equal(entry.split("\n")[0], entry.includes("os: macos") ? "          - runner: macos-15" : "          - runner: ubuntu-22.04", entry);
  }
  assert(build.includes("    runs-on: ${{ matrix.runner }}"));
  assert(build.includes("    timeout-minutes: 300"));
});

test("the update manifests cover exactly the targets the release matrix builds, and no Windows target ships until its sandbox holds", async () => {
  const release = await workflow("release.yml");
  const matrix = block(block(release, 2, "build").join("\n"), 6, "matrix").join("\n");
  const built = [...matrix.matchAll(/^ {12}target: (\S+)$/gm)].map((match) => match[1]);
  const manifest = steps(release, "publish").find((step) => step.includes("- name: Write update manifests and checksums"));
  assert.deepEqual(manifest.match(/ --targets (\S+)/)[1].split(","), built);
  assert.deepEqual(built, ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu"]);
});

test("the Windows CI job is off until v2.2.1: an if that is always false skips it at no cost, its body kept to turn back on", async () => {
  const jobs = block(await workflow("ci.yml"), 0, "jobs").join("\n");
  const windows = block(jobs, 2, "windows");
  assert(windows.includes("    name: Windows lint and tests (off until v2.2.1)"));
  assert.deepEqual(windows.filter((line) => /^ {4}if:/.test(line)), ["    if: ${{ false }}"], "the job never runs");
  assert(windows.includes("    continue-on-error: true"));
  assert(windows.includes("          cargo test -p crazyagent --locked --offline --test windows_terminal"), "the job body is kept");
  assert.equal(windows.filter((line) => line.includes("continue-on-error")).length, 1, "no Windows step hides its own failure");
  for (const job of ["linux", "macos"]) assert(!block(jobs, 2, job).some((line) => line.includes("continue-on-error")), job);
});

const RUST_CACHE = "Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0";

test("every GitHub-hosted CI Rust job keeps its build cache with the pinned rust-cache action, saved even when the job fails; the release build, which holds the signing keys, runs no third-party cache action; and no job caches the whole target folder", async () => {
  const ci = await workflow("ci.yml");
  const release = await workflow("release.yml");
  for (const [text, job, prefix] of [[ci, "linux", "ci-linux"], [ci, "macos", "ci-macos"], [ci, "windows", "ci-windows"]]) {
    const all = steps(text, job);
    const at = all.findIndex((step) => step.includes("rust-cache@"));
    assert(at >= 0, `${job} restores a Rust build cache`);
    assert.equal(all.filter((step) => step.includes("rust-cache@")).length, 1, job);
    assert.equal(all[at], [
      "      - if: runner.environment == 'github-hosted'",
      `        uses: ${RUST_CACHE}`,
      "        with:",
      `          prefix-key: ${prefix}`,
      "          cache-on-failure: true",
    ].join("\n"), job);
    assert(all.findIndex((step) => step.includes("- name: Install Rust toolchain")) < at, `${job} keys its cache on the installed toolchain`);
  }
  assert.doesNotMatch(release, /rust-cache/, "no third-party action runs beside the release signing keys");
  for (const [text, jobs] of [[ci, ["linux", "macos", "windows"]], [release, ["prepare", "build", "publish"]]]) {
    assert.match(text, /^ {2}CARGO_INCREMENTAL: "0"$/m);
    for (const job of jobs) {
      for (const step of steps(text, job).filter((step) => step.includes("uses: actions/cache@"))) {
        assert.doesNotMatch(step, /\btarget\b|\.cargo\//, `${job}: actions/cache keeps no Rust build output or Cargo registry`);
      }
    }
  }
});

test("every badal job refuses to run under Rosetta before anything else", async () => {
  const [ci] = steps(await workflow("ci.yml"), "macos");
  assert.match(ci, new RegExp(`^ {6}- name: ${ROSETTA}\n {8}run: \\|`));
  const [release] = steps(await workflow("release.yml"), "build");
  assert.match(release, new RegExp(`^ {6}- name: ${ROSETTA}\n {8}if: matrix\\.os == 'macos'\n`));
  for (const step of [ci, release]) {
    assert.match(step, /if \[ "\$\(sysctl -n sysctl\.proc_translated 2>\/dev\/null \|\| true\)" = 1 \]; then\n.*::error::.*Rosetta.*\n\s+exit 1/);
  }
});

test("on a self-hosted runner CI and release install nothing globally, and CI keeps its build cache bounded", async () => {
  const ci = await workflow("ci.yml");
  const release = await workflow("release.yml");
  const linuxCache = steps(ci, "linux").join("\n");
  assert.match(linuxCache, /"\$RUNNER_TOOL_CACHE\/crazyagent-ci\/target"/);
  assert.match(linuxCache, /\(\( size_kib > 20 \* 1024 \* 1024 \)\)/);
  for (const [name, job] of [["ci.yml", "linux"], ["ci.yml", "macos"], ["release.yml", "build"]]) {
    const all = steps(name === "ci.yml" ? ci : release, job);
    const text = all.join("\n");
    if (name === "ci.yml") assert.match(text, /- if: runner\.environment == 'github-hosted'\n\s+uses: Swatinem\/rust-cache@/, job);
    assert.match(text, /command -v rustup/, job);
    for (const step of all) {
      for (const install of step.matchAll(/^(?!\s*echo ).*(\bsudo (?!-n true|apt-get)\S+|\bbrew install\b).*$/gm)) {
        assert.match(step.slice(0, install.index), /if \[ "\$RUNNER_ENVIRONMENT" != github-hosted \]; then\n(?:[^\n]*\n)*?\s+exit 1\n\s+fi\n/,
          `${job}: "${install[0].trim()}" is reached only after the step has exited on a self-hosted runner`);
      }
    }
  }
  for (const job of ["linux", "macos"]) {
    const text = steps(ci, job).join("\n");
    assert.match(text, /"\$RUNNER_TOOL_CACHE\/claude-code\/\$CLAUDE_CODE_VERSION\/(x64|arm64)"/, job);
    assert.match(text, />> "\$GITHUB_PATH"/, job);
    assert.doesNotMatch(text, /\/usr\/local\/bin\/claude/, job);
  }
  const linux = steps(ci, "linux").join("\n");
  assert.match(linux, /dpkg -s "\$package"/);
  assert.match(linux, /sudo -n true/);
  assert.match(linux, /"\$RUNNER_TOOL_CACHE\/tmux\/\$TMUX_VERSION\/x64"/);
  assert.doesNotMatch(release, /"target\/\$TARGET/, "release paths follow CARGO_TARGET_DIR");
});

test("a self-hosted release builds in a clean Rust folder and Cargo home, never in what CI left on the runner", async () => {
  const release = await workflow("release.yml");
  const all = steps(release, "build");
  const index = all.findIndex((step) => step.includes("name: Use a clean Rust build folder"));
  assert(index >= 0, "the release build has a clean-folder step");
  const clean = all[index];
  assert.match(clean, /^ {6}- name: Use a clean Rust build folder\n {8}if: runner\.environment != 'github-hosted'\n/);
  assert.match(clean, /\n\s+rm -rf "\$RUNNER_TEMP\/crazyagent-release"\n/);
  assert.match(clean, /echo "CARGO_TARGET_DIR=\$RUNNER_TEMP\/crazyagent-release\/target"/);
  assert.match(clean, /echo "CARGO_HOME=\$RUNNER_TEMP\/crazyagent-release\/cargo-home"/);
  assert(all.findIndex((step) => /\bcargo (build|metadata|update)\b|tauri\.js build|version\.mjs set/.test(step)) > index, "no cargo run comes before the clean folder");
  assert.doesNotMatch(release, /crazyagent-ci/, "release never reads the CI build cache");
});

test("tools kept in the runner tool cache are checked against their pinned SHA-256 on every use, not once per version", async () => {
  for (const [name, job, title] of [["ci.yml", "linux", "Install Claude Code"], ["ci.yml", "linux", "Install Codex"], ["ci.yml", "macos", "Install Claude Code"], ["ci.yml", "macos", "Install Codex"],
    ["ci.yml", "macos", "Install pinned Node on the owner's runner"], ["ci.yml", "macos", "Install pinned arm64 bun on the owner's runner"],
    ["release.yml", "build", "Install x86_64 bun for the Intel rcli host"], ["release.yml", "build", "Install pinned Node on the owner's runner"],
    ["release.yml", "build", "Install pinned arm64 bun on the owner's runner"]]) {
    const step = steps(await workflow(name), job).find((text) => text.includes(`name: ${title}`));
    assert(step, `${job}: ${title}`);
    assert.doesNotMatch(step, /\.complete/, `${job}: ${title} keeps no install marker`);
    assert.match(step, /\n\s+if ! echo "\$[A-Z0-9_]+_SHA256  \$work\/[a-z.]+" \| (sha256sum -c --status -|shasum -a 256 -s -c -) 2>\/dev\/null; then\n/, `${job}: ${title} checks the cached archive every time`);
    assert.match(step, /\n\s+cp "\$work\/([a-z.]+)" "\$prefix\/\1\.\$\$"\n\s+mv "\$prefix\/\1\.\$\$" "\$prefix\/\1"\n/, `${job}: ${title} replaces the cached archive in one rename`);
    assert.match(step, /"\$work\/bin" >> "\$GITHUB_PATH"|bun-path=\$work\/bin\/bun/, `${job}: ${title} runs the copy it just checked`);
  }
});

test("macOS CI names the Codex and Claude Code it installed, and a graphify installed by the pinned uv from the repo lock, to the rows that run them", async () => {
  const ci = await workflow("ci.yml");
  const macos = steps(ci, "macos");
  const step = (title) => macos.find((text) => text.includes(`name: ${title}`));
  assert.match(step("Install Codex"), /\n\s+echo "CRAZYAGENT_CODEX=\$work\/bin\/codex" >> "\$GITHUB_ENV"$/);
  assert.match(step("Install Claude Code"), /\n\s+echo "CRAZYAGENT_CLAUDE=\$work\/bin\/claude" >> "\$GITHUB_ENV"$/);
  const graphify = step("Install pinned graphify for the graph rows");
  assert(graphify, "macOS CI installs graphify");
  const engine = await readFile(new URL("../../crates/crazyagent/src/setup.rs", import.meta.url), "utf8");
  const pin = graphify.match(/\n\s+UV_SHA256: ([0-9a-f]{64})\n/);
  assert(pin && engine.includes(`"aarch64-apple-darwin",\n        "${pin[1]}",`), "CI checks uv against the engine's own uv pin");
  assert.match(graphify, /\n\s+if ! echo "\$UV_SHA256  \$work\/uv\.tar\.gz" \| shasum -a 256 -s -c - 2>\/dev\/null; then\n/);
  assert.match(graphify, / UV_PYTHON_PREFERENCE=only-managed\n/, "no system Python");
  assert.match(graphify, / venv --no-project --no-config --python 3\.12\.15 "\$venv"\n/, "an exact Python, not the newest 3.12 the uv table knows");
  assert.match(graphify, /pip install --no-config --python "\$venv\/bin\/python" --require-hashes --no-build --requirements crates\/crazyagent\/src\/setup\/locks\/graphifyy-0\.9\.9\.txt\n/);
  assert.match(graphify, /\n\s+echo "CRAZYAGENT_GRAPHIFY=\$venv\/bin\/graphify" >> "\$GITHUB_ENV"$/);
  assert.doesNotMatch(graphify, /GITHUB_PATH|\$HOME/, "graphify reaches only the rows that name it");
  assert(macos.indexOf(graphify) < macos.findIndex((text) => text.includes("name: Test engine and desktop crates")));
});

test("macOS CI installs the context-mode the default pack pins from the committed lock with no package scripts, then builds only better-sqlite3, and names it to the shared-install rows", async () => {
  const macos = steps(await workflow("ci.yml"), "macos");
  const step = macos.find((text) => text.includes("name: Install the pinned context-mode for the shared-install rows"));
  assert(step, "macOS CI installs context-mode");
  assert.match(step, /' crates\/crazyagent\/src\/setup\/pack\.toml\)\n/, "version and hash come from the default pack");
  const copy = step.indexOf("cp .github/ci/context-mode/package.json .github/ci/context-mode/package-lock.json \"$install/\"");
  const locked = step.indexOf('test "$locked" = "$version $integrity"');
  const install = step.indexOf("npm ci --prefix \"$install\" --ignore-scripts --no-audit");
  const compare = step.indexOf('test "$recorded" = "$integrity"');
  const rebuild = step.indexOf("npm rebuild better-sqlite3 --prefix \"$install\" --no-audit");
  assert(copy > 0 && copy < locked && locked < install && install < compare && compare < rebuild, "the lock is checked against the pin, installed with no scripts and checked again before the one build");
  assert.equal(step.match(/npm rebuild/g)?.length, 1, "one rebuild");
  assert.doesNotMatch(step, /npm install|npm i |npx /, "nothing resolves a version outside the lock");
  assert.match(step, /npm_config_build_from_source=true npm_config_nodedir=/, "the native module builds from source against the pinned Node's own headers");
  assert.match(step, /\n\s+install="\$RUNNER_TEMP\/context-mode"\n/);
  assert.match(step, /\n\s+echo "CRAZYAGENT_CONTEXT_MODE_INSTALL=\$install" >> "\$GITHUB_ENV"$/);
  assert.doesNotMatch(step, /GITHUB_PATH|\$HOME|npm install -g|--global/);
  assert(macos.indexOf(step) < macos.findIndex((text) => text.includes("name: Test engine and desktop crates")));
});

test("the committed context-mode lock CI installs from holds the default pack's version and hash and pins every package by hash from the npm registry", async () => {
  const root = new URL("../../", import.meta.url);
  const pack = await readFile(new URL("crates/crazyagent/src/setup/pack.toml", root), "utf8");
  const entry = pack.split("[[entries]]").find((block) => /^id = "context-mode"$/m.test(block));
  const version = entry?.match(/^install = ".*\bcontext-mode@([0-9]+\.[0-9]+\.[0-9]+) /m)?.[1];
  const integrity = entry?.match(/^integrity = "(sha512-[A-Za-z0-9+/]+=*)"$/m)?.[1];
  assert(version && integrity, "the default pack pins an exact context-mode");
  const manifest = JSON.parse(await readFile(new URL(".github/ci/context-mode/package.json", root), "utf8"));
  assert.deepEqual(manifest.dependencies, { "context-mode": version });
  assert.equal(manifest.private, true);
  const lock = JSON.parse(await readFile(new URL(".github/ci/context-mode/package-lock.json", root), "utf8"));
  assert.equal(lock.lockfileVersion, 3);
  assert.equal(lock.name, manifest.name);
  assert.deepEqual(lock.packages[""].dependencies, manifest.dependencies);
  assert.equal(lock.packages["node_modules/context-mode"].version, version);
  assert.equal(lock.packages["node_modules/context-mode"].integrity, integrity);
  const packages = Object.entries(lock.packages).filter(([path]) => path !== "");
  assert(packages.length > 1 && packages.length < 1000, `${packages.length} packages`);
  for (const [path, locked] of packages) {
    assert.match(locked.integrity ?? "", /^sha512-[A-Za-z0-9+/]+=*$/, `${path} is pinned by hash`);
    assert.equal(new URL(locked.resolved).origin, "https://registry.npmjs.org", `${path} comes from the npm registry`);
    assert.notEqual(locked.link, true, `${path} is no link`);
  }
});

test("Linux CI installs Codex from the pinned, checksum-checked release archive and names it and Claude Code to the rows that run them, before the tests", async () => {
  const linux = steps(await workflow("ci.yml"), "linux");
  const step = (title) => linux.find((text) => text.includes(`name: ${title}`));
  const codex = step("Install Codex");
  assert(codex, "Linux CI installs Codex");
  assert.match(codex, /\n\s+CODEX_PACKAGE: codex-package-x86_64-unknown-linux-musl\.tar\.gz\n\s+CODEX_SHA256: [0-9a-f]{64}\n/);
  assert.match(codex, /"https:\/\/github\.com\/openai\/codex\/releases\/download\/rust-v\$CODEX_VERSION\/\$CODEX_PACKAGE"/);
  assert.match(codex, /\n\s+test "\$\("\$work\/bin\/codex" --version\)" = "codex-cli \$CODEX_VERSION"\n/);
  assert.match(codex, /\n\s+echo "CRAZYAGENT_CODEX=\$work\/bin\/codex" >> "\$GITHUB_ENV"$/);
  const claude = step("Install Claude Code");
  assert.match(claude, /\n\s+echo "CRAZYAGENT_CLAUDE=\$work\/bin\/claude" >> "\$GITHUB_ENV"$/);
  const tests = linux.findIndex((text) => text.includes("name: Test engine and desktop crates"));
  assert(linux.indexOf(codex) < tests && linux.indexOf(claude) < tests);
});

test("macOS CI runs a Codex release in codex::VERSIONS, from the pinned, checksum-checked release archive", async () => {
  const ci = await workflow("ci.yml");
  const pinned = ci.match(/^ {2}CODEX_VERSION: "(\d+)\.(\d+)\.(\d+)"$/m);
  assert(pinned, "CI pins an exact Codex version");
  const source = await readFile(new URL("../../crates/crazyagent/src/coord/delivery/codex.rs", import.meta.url), "utf8");
  const versions = source.match(/^pub const VERSIONS: \[\(u64, u64\); \d+\] = \[(.+)\];$/m);
  assert(versions, "codex::VERSIONS is one line");
  const supported = [...versions[1].matchAll(/\((\d+), (\d+)\)/g)].map((pair) => `${pair[1]}.${pair[2]}`);
  assert(supported.length > 0, "codex::VERSIONS names a release");
  assert(supported.includes(`${pinned[1]}.${pinned[2]}`), `CI Codex ${pinned[0]} is one of codex::VERSIONS ${supported.join(", ")}`);
  const step = steps(ci, "macos").find((text) => text.includes("name: Install Codex"));
  assert(step, "macOS CI installs Codex");
  assert.match(step, /\n\s+CODEX_SHA256: [0-9a-f]{64}\n/);
  assert.match(step, /"https:\/\/github\.com\/openai\/codex\/releases\/download\/rust-v\$CODEX_VERSION\/\$CODEX_PACKAGE"/);
  assert.match(step, /\n\s+test "\$\("\$work\/bin\/codex" --version\)" = "codex-cli \$CODEX_VERSION"\n/);
  assert.doesNotMatch(ci, /codex[^\n]*latest/i, "no unpinned Codex");
});

test("a release on the owner's runner signs with pinned, checksum-checked Node, bun and Rust, never the tool cache, rustup or Cargo settings CI left on the runner", async () => {
  const release = await workflow("release.yml");
  for (const pin of ["BUN_DARWIN_ARM64_SHA256", "NODE_DARWIN_ARM64_SHA256", "RUSTC_DARWIN_ARM64_SHA256", "CARGO_DARWIN_ARM64_SHA256", "RUST_STD_DARWIN_ARM64_SHA256", "RUST_STD_DARWIN_X64_SHA256"]) {
    assert.match(release, new RegExp(`^ {2}${pin}: [0-9a-f]{64}$`, "m"), pin);
  }
  const all = steps(release, "build");
  const at = (pattern) => all.findIndex((step) => pattern.test(step));
  for (const shared of [/uses: actions\/setup-node@/, /uses: oven-sh\/setup-bun@/, /rustup toolchain install/]) {
    const step = all[at(shared)];
    assert(step, String(shared));
    assert.match(step, /^ {6}- (?:id: \S+\n {8})?(?:name: [^\n]+\n {8})?if: runner\.environment == 'github-hosted'\n/, `${shared} runs only on GitHub's fresh runners`);
  }
  const rust = all[at(/name: Install pinned Rust on the owner's runner/)];
  assert(rust, "the release on the owner's runner installs its own Rust");
  assertPinnedRust(rust, "release");
  assert(rust.includes("$RUST_STD_DARWIN_X64_SHA256"), "RUST_STD_DARWIN_X64_SHA256");
  const node = at(/name: Install pinned Node on the owner's runner/);
  const bun = at(/id: bun-arm64\n/);
  for (const index of [node, bun]) assert.match(all[index], /\n {8}if: runner\.environment != 'github-hosted'\n/);
  assert.match(all[node], /node-v\$NODE_VERSION-darwin-arm64\.tar\.gz/);
  assert.match(all[bun], /bun-darwin-aarch64\.zip/);
  assert(node < at(/\bnode scripts\//) && node < at(/\bnpm ci\b/), "no node or npm run comes before the pinned Node");
  assert(at(/name: Install pinned Rust/) < at(/\bcargo build\b/), "no cargo run comes before the pinned Rust");
  const clean = all[at(/name: Use a clean Rust build folder/)];
  for (const line of ['echo "BUN_INSTALL_CACHE_DIR=$RUNNER_TEMP/crazyagent-release/bun-cache"', 'echo "npm_config_cache=$RUNNER_TEMP/crazyagent-release/npm-cache"']) {
    assert(clean.includes(`\n            ${line}\n`), line);
  }
  assert.match(clean, /\n {10}\} >> "\$GITHUB_ENV"$/, "the clean folder settings reach every later step");
  assert(at(/name: Use a clean Rust build folder/) < at(/install --frozen-lockfile/), "bun installs into a fresh cache");
  assert(at(/name: Use a clean Rust build folder/) < at(/\bnpm ci\b/), "npm installs into a fresh cache, never the runner user's home");
});

/**
 * Asserts that `step` installs Rust on the owner's runner from pinned, checksum-checked archives, never through rustup.
 * Inputs: the step text and a label for the messages. Output: none; it throws on the first property the step lacks.
 */
function assertPinnedRust(step, label) {
  assert.match(step, /\n {8}if: runner\.environment != 'github-hosted'\n/, label);
  assert.doesNotMatch(step, /\brustup\b|\.rustup|RUSTUP_/, `${label}: no rustup, toolchain or proxy left on the runner`);
  for (const pin of ["RUSTC_DARWIN_ARM64_SHA256", "CARGO_DARWIN_ARM64_SHA256", "RUST_STD_DARWIN_ARM64_SHA256"]) assert(step.includes(`$${pin}`), `${label}: ${pin}`);
  assert.match(step, /\n\s+if ! echo "\$sha  \$work\/\$archive" \| shasum -a 256 -s -c - 2>\/dev\/null; then\n\s+curl [^\n]+\n\s+echo "\$sha  \$work\/\$archive" \| shasum -a 256 -c -\n/, `${label}: every Rust archive, cached or downloaded, matches its pin`);
  assert.match(step, /\n\s+cp "\$work\/\$archive" "\$prefix\/\$archive\.\$\$"\n\s+mv "\$prefix\/\$archive\.\$\$" "\$prefix\/\$archive"\n/, label);
  assert.match(step, /\n\s+tar -xJf "\$work\/\$archive" -C "\$work"\n/, `${label}: it unpacks only the copy it just checked`);
  assert.match(step, /dir="\$\(dirname "\$GITHUB_WORKSPACE"\)"\n\s+while :; do\n\s+for config in "\$dir\/\.cargo\/config\.toml" "\$dir\/\.cargo\/config"; do\n\s+if \[ -e "\$config" \]; then\n[^\n]+::error::[^\n]+\n\s+exit 1\n/, `${label}: Cargo settings above the checkout stop the job`);
  assert.match(step, /test "\$\(command -v rustc\)" = "\$work\/bin\/rustc"\n\s+test "\$\(command -v cargo\)" = "\$work\/bin\/cargo"\n/, label);
  assert.match(step, /\n\s+work="\$RUNNER_TEMP\/rust"\n/, label);
  assert.match(step, /\n\s+echo "\$work\/bin" >> "\$GITHUB_PATH"\n?$/, label);
}

/**
 * Returns the value of one workflow-level `env:` entry.
 * Inputs: the workflow text and the variable name. Output: the text after `NAME: ` on its one line.
 */
function envValue(workflowText, name) {
  const lines = block(workflowText, 0, "env").filter((line) => line.startsWith(`  ${name}: `));
  assert.equal(lines.length, 1, `one ${name} in the workflow env`);
  return lines[0].slice(`  ${name}: `.length);
}

const HOSTED_ONLY = /^ {6}- (?:id: \S+\n {8})?(?:name: [^\n]+\n {8})?if: runner\.environment == 'github-hosted'\n/;

test("macOS CI on the owner's runner builds with the release's pinned, checksum-checked Node, bun and Rust, plus a pinned clippy", async () => {
  const ci = await workflow("ci.yml");
  const release = await workflow("release.yml");
  for (const name of ["RUST_TOOLCHAIN", "NODE_VERSION", "BUN_VERSION", "NODE_DARWIN_ARM64_SHA256", "BUN_DARWIN_ARM64_SHA256",
    "RUSTC_DARWIN_ARM64_SHA256", "CARGO_DARWIN_ARM64_SHA256", "RUST_STD_DARWIN_ARM64_SHA256"]) {
    assert.equal(envValue(ci, name), envValue(release, name), `CI and release pin the same ${name}`);
  }
  assert.match(envValue(ci, "CLIPPY_DARWIN_ARM64_SHA256"), /^[0-9a-f]{64}$/);
  assert.doesNotMatch(ci, /crazyagent-release/, "CI never writes the release's tool cache");
  const all = steps(ci, "macos");
  const at = (pattern) => {
    const index = all.findIndex((step) => pattern.test(step));
    assert(index >= 0, String(pattern));
    return index;
  };
  for (const shared of [/uses: actions\/setup-node@/, /uses: oven-sh\/setup-bun@/, /rustup toolchain install/]) {
    assert.match(all[at(shared)], HOSTED_ONLY, `${shared} runs only on GitHub's fresh runners`);
  }
  const rust = all[at(/name: Install pinned Rust on the owner's runner/)];
  assertPinnedRust(rust, "CI");
  assert(rust.includes('"clippy aarch64-apple-darwin $CLIPPY_DARWIN_ARM64_SHA256"'), "the pinned clippy is one of the checked archives");
  assert.match(rust, /\n\s+prefix="\$RUNNER_TOOL_CACHE\/crazyagent-ci\/rust\/\$RUST_TOOLCHAIN"\n/);
  assert.match(rust, /\n\s+test "\$\(command -v cargo-clippy\)" = "\$work\/bin\/cargo-clippy"\n/);
  assert.match(rust, /\n\s+test -d "\$\(rustc --print sysroot\)\/lib\/rustlib\/aarch64-apple-darwin\/lib"\n/);
  assert.match(rust, /\n\s+for config in "\$CARGO_HOME\/config\.toml" "\$CARGO_HOME\/config"; do\n\s+\[ ! -e "\$config" \] \|\| \{ echo "::error::[^\n]+"; exit 1; \}\n\s+done\n/,
    "Cargo settings a step left in the job's Cargo home stop the job");
  assert(all.findIndex((step) => step.includes('echo "CARGO_HOME=')) < at(/name: Install pinned Rust/), "the Rust step checks the Cargo home CI set");
  const node = at(/name: Install pinned Node on the owner's runner/);
  const bun = at(/id: bun-arm64\n/);
  for (const index of [node, bun]) assert.match(all[index], /\n {8}if: runner\.environment != 'github-hosted'\n/);
  assert.match(all[node], /\n\s+prefix="\$RUNNER_TOOL_CACHE\/crazyagent-ci\/node\/\$NODE_VERSION\/arm64"\n[^]*node-v\$NODE_VERSION-darwin-arm64\.tar\.gz/);
  assert.match(all[bun], /\n\s+prefix="\$RUNNER_TOOL_CACHE\/crazyagent-ci\/bun\/\$BUN_VERSION\/arm64"\n[^]*bun-darwin-aarch64\.zip/);
  assert(node < at(/\bnpm ci\b/) && node < at(/\bnode --input-type/), "no node or npm run comes before the pinned Node");
  assert(bun < at(/\bbun install\b/), "no bun run comes before the pinned bun");
  assert(at(/name: Install pinned Rust/) < at(/\bcargo fetch\b/) && at(/name: Install pinned Rust/) < at(/\bmake lint\b/), "no cargo run comes before the pinned Rust");
  const bunPaths = all.join("\n").split("\n").filter((line) => line.includes("outputs.bun-path"));
  assert.equal(bunPaths.length, 2, "the Node and Bun suites name their bun");
  for (const line of bunPaths) assert.equal(line.trim(), "BUN: ${{ steps.bun.outputs.bun-path || steps.bun-arm64.outputs.bun-path }}");
});

test("nothing macOS CI runs on the owner's runner installs globally or reads or writes the runner user's home", async () => {
  const all = steps(await workflow("ci.yml"), "macos");
  const home = all[1];
  assert.match(home, /^ {6}- name: Keep CI state out of the runner user's home\n {8}if: runner\.environment != 'github-hosted'\n/, "the second step, right after the Rosetta check");
  assert.match(home, /\n\s+state="\$RUNNER_TOOL_CACHE\/crazyagent-ci"\n/);
  assert.match(home, /\n\s+home="\$RUNNER_TEMP\/home"\n/);
  assert.match(home, /\n\s+rm -rf "\$state"\n\s+mkdir -p "\$state"\n/, "nothing an earlier job left is kept");
  for (const line of ['echo "HOME=$home"', 'echo "CARGO_HOME=$state/cargo-home"', 'echo "CARGO_TARGET_DIR=$target"',
    'echo "npm_config_cache=$state/npm-cache"', 'echo "BUN_INSTALL_CACHE_DIR=$state/bun-cache"']) {
    assert(home.includes(`\n            ${line}\n`), line);
  }
  const owner = all.filter((step) => !HOSTED_ONLY.test(step));
  assert(owner.length >= 10, "the filter keeps the steps badal runs");
  const global = /\bsudo\b|\b(?:npm|bun|pnpm|yarn) (?:i|install|add)\b[^\n]*(?:\s-g\b|--global)|\bcargo install\b|\brustup\b|\$HOME\b|~\/|\/\.(?:claude|codex|rustup|npm|bun)\b|uses: (?:actions\/setup-node|oven-sh\/setup-bun)@/;
  for (const step of owner) {
    const title = step.split("\n")[0].trim();
    assert.doesNotMatch(step, global, `${title}: nothing global and nothing under the runner user's home`);
    for (const line of step.split("\n").filter((line) => line.includes('>> "$GITHUB_PATH"'))) {
      assert.equal(line.trim(), 'echo "$work/bin" >> "$GITHUB_PATH"', title);
      assert.match(step, /\n\s+work="\$RUNNER_TEMP\/[a-z0-9-]+"\n/, `${title}: PATH gains only a folder under RUNNER_TEMP`);
    }
  }
});

/**
 * Asserts that the last step of a job deletes, on the owner's runner and after any failure or cancel, every
 * tool cache and RUNNER_TEMP folder named by a step that can run there, then the checkout's contents.
 * Inputs: the job's steps and a label for the messages. Output: none; it throws on the first gap.
 */
function assertDeletesEverything(all, label) {
  const last = all.at(-1);
  assert.match(last, /^ {6}- name: [^\n]+\n {8}if: always\(\) && runner\.environment != 'github-hosted'\n {8}run: \|\n {10}set -uo pipefail\n/,
    `${label}: it runs after a failed or cancelled step, only on the owner's runner, and one failed delete does not stop the rest`);
  assert.match(last, /\n {10}for folder in "\$RUNNER_TOOL_CACHE" "\$RUNNER_TEMP"; do\n {12}case "\$folder" in \/\?\*\) ;; \*\) echo "::error::[^"\n]+"; exit 1 ;; esac\n {10}done\n/, `${label}: no relative runner folder`);
  assert.match(last, /\n {10}case "\$GITHUB_WORKSPACE" in "\$\(dirname "\$RUNNER_TEMP"\)"\/\?\*\) ;; \*\) echo "::error::[^"\n]+"; exit 1 ;; esac\n/, `${label}: only a checkout inside the runner's work folder is emptied`);
  const list = (name) => {
    const found = last.match(new RegExp(`\\n {10}${name}=\\(([a-z0-9 -]+)\\)\\n`));
    assert(found, `${label}: one ${name} list`);
    return found[1].split(" ");
  };
  const tools = list("tools");
  const temp = list("temp");
  const reachable = all.filter((step) => !HOSTED_ONLY.test(step) && !/\n {8}if: matrix\.os == '(?:linux|windows)'\n/.test(step));
  const named = (pattern) => new Set(reachable.flatMap((step) => [...step.matchAll(pattern)].map((match) => match[1])));
  for (const name of named(/\$RUNNER_TOOL_CACHE\/([A-Za-z0-9._-]+)/g)) assert(tools.includes(name), `${label}: the tool cache folder ${name} is deleted`);
  for (const name of named(/\$RUNNER_TEMP\/([A-Za-z0-9._-]+)/g)) assert(temp.includes(name), `${label}: the RUNNER_TEMP folder ${name} is deleted`);
  assert(last.includes([
    '          for name in "${tools[@]}"; do made+=("$RUNNER_TOOL_CACHE/$name"); done',
    '          for name in "${temp[@]}"; do made+=("$RUNNER_TEMP/$name"); done',
    "          while IFS= read -r -d '' entry; do made+=(\"$entry\"); done < <(find \"$GITHUB_WORKSPACE\" -mindepth 1 -maxdepth 1 -print0)",
  ].join("\n")), `${label}: the listed folders and the checkout's contents are what goes`);
  const removals = last.split("\n").filter((line) => /\brm\b/.test(line)).map((line) => line.trim());
  assert.deepEqual(removals, ['rm -rf "$path" 2>/dev/null || { chmod -R u+w "$path" 2>/dev/null; rm -rf "$path"; }'], `${label}: one delete, retried once a read-only folder is made writable`);
  assert.match(last, /\n {10}if \[ "\$\{#left\[@\]\}" -gt 0 \]; then echo "::error::[^"\n]+"; exit 1; fi\n?$/, `${label}: anything still on the runner fails the job`);
  assert.doesNotMatch(last, /\.\.\/|\$HOME\b|~\/|\bsudo\b/, `${label}: nothing outside the runner's own folders`);
}

test("macOS CI on the owner's runner builds with line tables only, starts with nothing kept, refuses below 10 GiB free and deletes every file it made at the end", async () => {
  const ci = await workflow("ci.yml");
  const all = steps(ci, "macos");
  const home = all[1];
  assert.match(block(ci, 0, "env").join("\n"), /\n  CARGO_INCREMENTAL: "0"\n/, "no incremental state in a CI target");
  for (const line of ['echo "CARGO_PROFILE_DEV_DEBUG=line-tables-only"', 'echo "CARGO_PROFILE_TEST_DEBUG=line-tables-only"', 'echo "CARGO_TARGET_DIR=$target"']) {
    assert(home.includes(`\n            ${line}\n`), line);
  }
  assert.match(home, /\n\s+target="\$state\/target-line-tables"\n/);
  assert.match(home, /\n\s+case "\$RUNNER_TOOL_CACHE" in \/\?\*\) ;; \*\) echo "::error::[^"\n]+"; exit 1 ;; esac\n/);
  assert(home.indexOf('rm -rf "$state"') < home.indexOf("df -Pk"), "the free space is read once what an earlier job left is gone");
  assert.doesNotMatch(home, /size_kib|find "\$state"/, "no kept state to bound");
  assert.match(home, /\n\s+free_kib="\$\(df -Pk "\$RUNNER_TOOL_CACHE" \| awk 'NR == 2 \{ print \$4 \}'\)"\n\s+echo "[^"\n]*\$\(\( free_kib \/ 1024 \/ 1024 \)\) GiB free[^"\n]*"\n/, "the free space shows before the build");
  assert.match(home, /\n\s+if \(\( free_kib < 10 \* 1024 \* 1024 \)\); then\n\s+echo "::error::badal has \$\(\( free_kib \/ 1024 \/ 1024 \)\) GiB free[^"\n]*needs 10 GiB[^"\n]*free disk on badal[^"\n]*"\n\s+exit 1\n\s+fi\n/);
  assert(home.indexOf("free_kib < 10") < home.indexOf('>> "$GITHUB_ENV"'), "the refusal comes before the job sets anything up");
  const tests = all.findIndex((step) => /- name: Test Bun suites\n/.test(step));
  assert(tests > 0 && tests === all.length - 2, "the cleanup is the last step, right after the tests");
  const last = all.at(-1);
  assert.equal((last.match(/df -Pk "\$RUNNER_TOOL_CACHE"/g) ?? []).length, 1, "one free-space reader");
  assert.equal((last.match(/\$\(free_gib\) GiB free/g) ?? []).length, 2, "free space after the tests and after the cleanup");
  assert.match(last, /\n\s+target="\$RUNNER_TOOL_CACHE\/crazyagent-ci\/target-line-tables"\n\s+if \[ -d "\$target" \]; then\n\s+du -sk "\$target" \| awk '\{ printf "Rust target: %\.1f GiB\\n", \$1 \/ 1048576 \}'\n\s+fi\n/, "the target size shows, so the floor can follow the real need");
  assert(last.indexOf('du -sk "$target"') < last.indexOf("rm -rf"), "the size is read before anything goes");
  assertDeletesEverything(all, "CI");
});

test("a release on the owner's runner deletes every file it made after the last upload, even after a failure or cancel", async () => {
  const all = steps(await workflow("release.yml"), "build");
  assert.match(all.at(-2), /^ {6}- uses: actions\/upload-artifact@[0-9a-f]{40}\n {8}with:\n {10}name: signing-/, "the cleanup comes after the last upload");
  assertDeletesEverything(all, "release");
});

test("Linux CI frees the hosted runner's unused toolsets first, builds with line tables only and shows the free disk and target size at the end", async () => {
  const ci = await workflow("ci.yml");
  const linux = block(block(ci, 0, "jobs").join("\n"), 2, "linux").join("\n");
  const env = block(linux, 4, "env");
  for (const name of ["CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG"]) {
    assert.equal(env.filter((line) => line === `      ${name}: line-tables-only`).length, 1, name);
  }
  assert.match(block(ci, 0, "env").join("\n"), /\n  CARGO_INCREMENTAL: "0"\n/, "no incremental state in a CI target");
  const all = steps(ci, "linux");
  assert.match(all[0], /^ {6}- uses: actions\/checkout@/, "checkout stays the first step");
  const free = all[1];
  assert.match(free, /^ {6}- name: [^\n]+\n {8}if: runner\.environment == 'github-hosted'\n/, "the second step, right after checkout, only on a GitHub-hosted runner");
  assert.match(free, /\n\s+df -h \/\n\s+sudo rm -rf \/usr\/share\/dotnet \/usr\/local\/lib\/android \/opt\/ghc \/usr\/local\/share\/boost \/opt\/hostedtoolcache\/CodeQL\n\s+df -h \/$/,
    "the free disk shows before and after the toolsets go");
  const removals = free.split("\n").filter((line) => /\brm\b/.test(line)).map((line) => line.trim());
  assert.deepEqual(removals, ["sudo rm -rf /usr/share/dotnet /usr/local/lib/android /opt/ghc /usr/local/share/boost /opt/hostedtoolcache/CodeQL"], "only the five literal toolset folders");
  const last = all.at(-1);
  assert.match(all.at(-2), /^ {6}- name: Test engine and desktop crates\n/, "the disk report comes right after the tests");
  assert.match(last, /^ {6}- name: [^\n]+\n {8}if: always\(\)\n/, "it runs even when a step failed");
  assert.match(last, /\n\s+df -h \/\n/);
  assert.match(last, /\n\s+target="\$\{CARGO_TARGET_DIR:-target\}"\n\s+if \[ -d "\$target" \]; then\n\s+du -sh "\$target"\n\s+fi\n/, "the target size shows, so the disk need is known");
  assert.doesNotMatch(last, /\brm\b|\bsudo\b/, "the report changes nothing");
});

test("make test and test-rust-selected give every cargo test the native-auth host, desktop tests included", async () => {
  const lines = (await readFile(new URL("../../Makefile", import.meta.url), "utf8")).split("\n");
  const exported = new Set(lines.flatMap((line) => {
    const match = /^([a-z -]+): export CRAZYAGENT_TEST_NATIVE_AUTH_HOST = \S/.exec(line);
    return match ? match[1].trim().split(/\s+/) : [];
  }));
  for (const target of ["test", "test-rust-selected"]) {
    const start = lines.findIndex((line) => line.startsWith(`${target}:`) && !line.includes(" export "));
    assert(start >= 0, target);
    const end = lines.findIndex((line, index) => index > start && !line.startsWith("\t"));
    const tests = lines.slice(start + 1, end).filter((line) => /\bcargo test\b/.test(line));
    assert(tests.length > 0, target);
    for (const line of tests) assert(exported.has(target) || line.includes("CRAZYAGENT_TEST_NATIVE_AUTH_HOST="), `${target}: ${line.trim()}`);
  }
});

test("Rust tests make their temp folders under the canonical /tmp, which also exists on Linux CI, never under /private/tmp", async () => {
  const root = new URL("../../", import.meta.url);
  const found = [];
  for (const base of ["crates", "apps/desktop/src-tauri/src", "apps/desktop/src-tauri/tests"]) {
    for (const name of await readdir(new URL(base, root), { recursive: true })) {
      if (!name.endsWith(".rs")) continue;
      const text = await readFile(new URL(`${base}/${name}`, root), "utf8");
      text.split("\n").forEach((line, index) => {
        if (/tempdir_in\(\s*"\/private\/tmp"/.test(line)) found.push(`${base}/${name}:${index + 1}`);
      });
    }
  }
  assert.deepEqual(found, []);
});

test("macOS release signing keeps the certificate in a job keychain under RUNNER_TEMP and always removes it", async () => {
  const release = await workflow("release.yml");
  const all = steps(release, "build");
  const index = all.findIndex((step) => step.includes("id: desktop"));
  const build = all[index];
  const cleanup = all[index + 1];
  assert(build.indexOf("unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD") > build.indexOf('security_line "create-keychain'), "the keychain is made before the certificate leaves the environment");
  assert(build.indexOf("unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD") < build.indexOf("tauri.js build"), "Tauri never sees APPLE_CERTIFICATE, so it makes no keychain of its own");
  assert.match(build, /keychain="\$\(cd "\$RUNNER_TEMP" && pwd -P\)\/crazyagent-signing\.keychain-db"/);
  assert.match(build, /security_line\(\) \{ printf '%s\\n' "\$1" \| security -i; \}/);
  assert.doesNotMatch(release, /^\s*security (create-keychain|unlock-keychain|import|set-key-partition-list)\b/m, "passwords reach security on stdin, never on a command line");
  assert.doesNotMatch(release, /login\.keychain|default-keychain/);
  assert.match(cleanup, /^ {6}- name: Remove the signing keychain and Apple keys\n {8}if: always\(\) && matrix\.os == 'macos'\n/);
  assert.match(cleanup, /keychain="\$\(cd "\$RUNNER_TEMP" && pwd -P\)\/crazyagent-signing\.keychain-db"/);
  assert.match(cleanup, /rm -rf "\$RUNNER_TEMP\/apple"/);
  assert.match(cleanup, /security delete-keychain "\$keychain"/);
  assert.doesNotMatch(cleanup, /secrets\./);
  const edits = /security_line "create-keychain|security delete-keychain|security list-keychains -d user -s/;
  for (const [step, expected] of [[build, ["create-keychain", "list-keychains -d user -s"]], [cleanup, ["delete-keychain", "list-keychains -d user -s"]]]) {
    assert.match(step, /keychain_lock\(\) \{\n\s+exec 9>> "\$HOME\/\.crazyagent-keychain\.lock"\n\s+perl -MFcntl=:flock -e '[^'\n]*for \(1 \.\. 120\) \{ exit 0 if flock\(\$lock, LOCK_EX \| LOCK_NB\); sleep 1 \} exit 1' \|\|/);
    let locked = false;
    const inside = [];
    for (const line of step.split("\n").map((line) => line.trim())) {
      if (line === "keychain_lock" || line === "if ! keychain_lock; then") locked = true;
      else if (line === "keychain_unlock") locked = false;
      else if (edits.test(line)) {
        assert(locked, `search-list edit outside the per-user lock: ${line}`);
        inside.push(line);
      }
    }
    for (const word of expected) assert(inside.some((line) => line.includes(word)), `${word} runs inside the lock`);
  }
  assert.equal(release.split("\n").filter((line) => edits.test(line)).length, 4, "no other step edits the keychain search list");
});

test("the Intel macOS release runs a pinned, checksum-verified x86_64 bun under Rosetta, which only a GitHub-hosted runner may install", async () => {
  const release = await workflow("release.yml");
  assert.match(release, /^ {2}BUN_DARWIN_X64_SHA256: [0-9a-f]{64}$/m);
  const all = steps(release, "build");
  const bun = all.find((step) => step.includes("id: bun-x64"));
  assert.match(bun, /if: matrix\.target == 'x86_64-apple-darwin'/);
  assert.match(bun, /echo "\$BUN_DARWIN_X64_SHA256  \$work\/bun\.zip" \| shasum -a 256 -c -/);
  assert.match(bun, /"\$RUNNER_TOOL_CACHE\/crazyagent-release\/bun\/\$BUN_VERSION\/x64"/, "the x86_64 bun stays in the release's own tool cache folder");
  assert.match(bun, /\n\s+if ! arch -x86_64 \/usr\/bin\/true 2>\/dev\/null; then\n\s+if \[ "\$RUNNER_ENVIRONMENT" != github-hosted \]; then\n[^\n]+::error::[^\n]+\n\s+exit 1\n\s+fi\n\s+sudo softwareupdate --install-rosetta --agree-to-license\n\s+arch -x86_64 \/usr\/bin\/true\n\s+fi\n/);
  for (const name of ["Install rcli source dependencies", "Build rcli host"]) {
    assert.match(all.find((step) => step.includes(`name: ${name}`)), /BUN_PATH: \$\{\{ steps\.bun-x64\.outputs\.bun-path \|\| steps\.bun-arm64\.outputs\.bun-path \|\| steps\.bun\.outputs\.bun-path \}\}/, name);
  }
});

test("macOS CI provisions the pinned VSCodium build the IDE server tests need from builds.json, checked on every use, before the Rust tests", async () => {
  const root = new URL("../../", import.meta.url);
  const manifest = JSON.parse(await readFile(new URL("crates/crazyagent/src/ide/builds.json", root), "utf8"));
  const server = await readFile(new URL("crates/crazyagent/src/ide/server/tests.rs", root), "utf8");
  assert(server.includes(`const BUILD: &str = "vscodium-${manifest.version}";`), "the server tests read the build that builds.json pins");
  const all = steps(await workflow("ci.yml"), "macos");
  const at = (found) => all.findIndex(found);
  const read = at((step) => step.includes("name: Read the pinned IDE build"));
  const cache = at((step) => step.includes("uses: actions/cache@") && step.includes("vscodium-reh-web"));
  const provision = at((step) => step.includes("name: Provision the pinned IDE build"));
  const tests = at((step) => step.includes("name: Test engine and desktop crates"));
  assert(read >= 0 && read < cache && cache < provision && provision < tests, `${read} ${cache} ${provision} ${tests}`);
  assert.match(all[read], /^ {6}- id: ide\n/);
  assert.match(all[read], /readFileSync\("crates\/crazyagent\/src\/ide\/builds\.json", "utf8"\)/);
  assert.match(all[read], /manifest\.builds\["aarch64-apple-darwin"\]/);
  assert.match(all[read], /build\?\.url === url/, "only the official release URL is fetched");
  assert.match(all[cache], /^ {6}- if: runner\.environment == 'github-hosted'\n/);
  assert.match(all[cache], /key: ci-macos-vscodium-reh-web-darwin-arm64-\$\{\{ steps\.ide\.outputs\.version \}\}\n?$/);
  const step = all[provision];
  for (const name of ["VERSION", "URL", "SHA256", "BYTES"]) {
    assert.match(step, new RegExp(`IDE_${name}: \\$\\{\\{ steps\\.ide\\.outputs\\.${name.toLowerCase()} \\}\\}`), name);
  }
  assert.match(step, /prefix="\$RUNNER_TOOL_CACHE\/vscodium-reh-web\/\$IDE_VERSION\/arm64"/);
  assert.match(step, /\n\s+if ! echo "\$IDE_SHA256  \$work\/vscodium\.tgz" \| shasum -a 256 -s -c - 2>\/dev\/null; then\n/, "the cached archive is checked on every use");
  assert.match(step, /--proto '=https' --proto-redir '=https'/);
  assert.match(step, /--max-filesize "\$IDE_BYTES"/);
  assert.match(step, /\n\s+test "\$\(stat -f %z "\$work\/vscodium\.tgz"\)" = "\$IDE_BYTES"\n/);
  assert.match(step, /\n\s+echo "\$IDE_SHA256  \$work\/vscodium\.tgz" \| shasum -a 256 -c -\n/, "a download that does not match fails the job");
  assert(step.indexOf("| shasum -a 256 -c -\n") < step.indexOf('cp "$work/vscodium.tgz" "$prefix/'), "only a checked archive reaches the tool cache");
  assert.match(step, /\n\s+cp "\$work\/vscodium\.tgz" "\$prefix\/vscodium\.tgz\.\$\$"\n\s+mv "\$prefix\/vscodium\.tgz\.\$\$" "\$prefix\/vscodium\.tgz"\n/);
  assert.match(step, /build="\.local-state\/ide-cache\/vscodium-\$IDE_VERSION"/);
  assert.match(step, /\n\s+tar -xzf "\$work\/vscodium\.tgz" -C "\$build"\n\s+test -f "\$build\/out\/server-main\.js"/);
  assert.match(step, /\n\s+test -f "\$build\/out\/server-main\.js"\n\s+find "\$RUNNER_TOOL_CACHE\/vscodium-reh-web" -mindepth 1 -maxdepth 1 ! -name "\$IDE_VERSION" -exec rm -rf \{\} \+$/, "once the pinned build is provisioned, the tool cache drops the archives of every other version");
});

/**
 * Reads one test exclusion list of the CI workflow's top-level env.
 * Inputs: the workflow text and the list's name. Output: the set of excluded target names,
 * from the key's own line and every following line indented deeper than the key.
 */
function excluded(text, name) {
  const lines = text.split("\n");
  const start = lines.findIndex((line) => line.startsWith(`  ${name}:`));
  assert(start >= 0, `no ${name} in the CI env`);
  const end = lines.findIndex((line, index) => index > start && line.length - line.trimStart().length <= 2);
  return new Set([lines[start].slice(lines[start].indexOf(":") + 1), ...lines.slice(start + 1, end)].join(" ").split(/\s+/).filter((word) => word && word !== ">-"));
}

test("every Rust test target that needs the staged RCLI host is left out of CI, which stages none", async () => {
  const ci = await workflow("ci.yml");
  const root = new URL("../../", import.meta.url);
  let needing = 0;
  for (const [folder, list] of [["crates/crazyagent/tests/", "ENGINE_TESTS_EXCLUDE"], ["apps/desktop/src-tauri/tests/", "DESKTOP_TESTS_EXCLUDE"]]) {
    const skipped = excluded(ci, list);
    for (const name of (await readdir(new URL(folder, root))).filter((name) => name.endsWith(".rs"))) {
      if (!(await readFile(new URL(folder + name, root), "utf8")).includes("CRAZYAGENT_RCLI")) continue;
      needing += 1;
      assert(skipped.has(name.slice(0, -3)), `${folder}${name} reads CRAZYAGENT_RCLI but is not in ${list}`);
    }
  }
  assert(needing > 0, "the check found the tests that need the RCLI host");
});

test("every engine and desktop row or target CI leaves out, for all jobs or one, names a real test, so no exclusion goes stale", async () => {
  const ci = await workflow("ci.yml");
  const jobs = block(ci, 0, "jobs").join("\n");
  const job = (name) => block(jobs, 2, name).join("\n").replace(/^ {4}/gm, "");
  const root = new URL("../../", import.meta.url);
  const source = async (path) => readFile(new URL(path, root), "utf8");
  const library = async (folder) => {
    const texts = [];
    for (const name of await readdir(new URL(folder, root), { recursive: true })) {
      if (name.endsWith(".rs")) texts.push(await source(folder + name));
    }
    return texts;
  };
  const crates = [
    ["crates/crazyagent/", ["ENGINE_TESTS_EXCLUDE", ci], ["PLATFORM_TESTS_EXCLUDE", job("macos")], ["PLATFORM_TESTS_EXCLUDE", job("linux")]],
    ["apps/desktop/src-tauri/", ["DESKTOP_TESTS_EXCLUDE", ci]],
  ];
  let rows = 0;
  for (const [crate, ...lists] of crates) {
    const texts = await library(`${crate}src/`);
    for (const [list, text] of lists) {
      for (const entry of excluded(text, list)) {
        if (entry.includes("::")) {
          const row = entry.split("::").at(-1);
          assert(texts.some((text) => text.includes(`fn ${row}(`)), `${list} ${entry} is no library row of ${crate}`);
          rows += 1;
          continue;
        }
        const [target, row] = entry.split("/");
        const file = await source(`${crate}tests/${target}.rs`).catch(() => assert.fail(`${list} ${entry} names no target ${crate}tests/${target}.rs`));
        assert(row === undefined || file.includes(`fn ${row}(`), `${list} ${entry} is no row of ${crate}tests/${target}.rs`);
        rows += row === undefined ? 0 : 1;
      }
    }
  }
  assert(rows > 20, "the check found the excluded rows");
});

test("every live Rust test target, named *_live, is left out of CI, which has no live accounts, harnesses or local models", async () => {
  const skipped = excluded(await workflow("ci.yml"), "ENGINE_TESTS_EXCLUDE");
  const folder = new URL("../../crates/crazyagent/tests/", import.meta.url);
  const live = (await readdir(folder)).filter((name) => name.endsWith("_live.rs")).map((name) => name.slice(0, -3));
  assert(live.includes("codex_bridge_live"), "the check found the live targets");
  for (const name of live) {
    assert(skipped.has(name), `crates/crazyagent/tests/${name}.rs is a live target but is not in ENGINE_TESTS_EXCLUDE`);
  }
});

test("CI runs the price snapshot script's check on Linux, so a bad refresh fails a pull request", async () => {
  const linux = steps(await workflow("ci.yml"), "linux");
  assert(
    linux.some((step) => /^ {8}run: make test-price-script$/m.test(step)),
    "the linux job runs make test-price-script",
  );
});
