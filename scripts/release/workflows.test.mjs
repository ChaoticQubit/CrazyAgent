import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { readdir, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

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

/**
 * Lists the jobs under `jobs:` of a workflow, failing closed: every line of the block that is
 * not blank or a comment belongs to a job whose key sits alone at 2-space indent, so a job the
 * guards cannot read throws instead of being skipped. Input: the workflow text. Output: each
 * job's name with its lines, at least one.
 */
function jobsOf(text) {
  const lines = text.split("\n");
  const start = lines.indexOf("jobs:");
  assert(start >= 0, "no jobs: block at indent 0, so no job can be read");
  const jobs = [];
  for (const line of lines.slice(start + 1)) {
    if (line.trim() === "" || line.trimStart().startsWith("#")) continue;
    if (/^\S/.test(line)) break;
    const name = line.match(/^ {2}([A-Za-z0-9_-]+):\s*$/)?.[1];
    if (name) {
      jobs.push({ name, lines: [line] });
      continue;
    }
    assert(jobs.length > 0 && line.length - line.trimStart().length > 2, `every job key sits alone at 2-space indent under jobs:, not ${JSON.stringify(line)}`);
    jobs.at(-1).lines.push(line);
  }
  assert(jobs.length > 0, "no job key at 2-space indent under jobs:");
  return jobs;
}

/**
 * The lines of a workflow outside its `jobs:` block, such as a workflow-level `env:` every job
 * inherits; the block ends where jobsOf stops reading. Input: the workflow text. Output: the lines.
 */
function sharedLines(text) {
  const lines = text.split("\n");
  const start = lines.indexOf("jobs:");
  const end = lines.findIndex((line, index) => index > start && /^[^\s#]/.test(line));
  return [...lines.slice(0, start), ...(end < 0 ? [] : lines.slice(end))];
}

/**
 * Names the jobs of a workflow that read a secret, hand secrets to a called workflow or bind an
 * environment, whatever the file or job; a secret outside `jobs:` reaches every job. Input: the
 * workflow text. Output: the job names in file order.
 */
function exposedJobs(text) {
  const jobs = jobsOf(text);
  const shared = sharedLines(text).some((line) => line.includes("secrets."));
  return jobs.filter(({ lines }) => shared || lines.some((line) => line.includes("secrets.") || /^ {4}(environment|secrets):/.test(line))).map(({ name }) => name);
}

/**
 * Lists every runner of a workflow outside the allowed GitHub-hosted images, reading each
 * matrix runner value when a job takes `${{ matrix.runner }}`; a `runner:` key, in block or
 * flow form, whose value is not one plain name is a problem. A job that calls a reusable workflow has no runner of its own.
 * Input: the workflow text. Output: "job: runner" entries.
 */
function runnerProblems(text) {
  const allowed = new Set(Object.values(RUNNERS));
  const problems = [];
  for (const { name, lines } of jobsOf(text)) {
    const runsOn = lines.find((line) => /^ {4}runs-on:/.test(line))?.replace(/^ {4}runs-on:\s*/, "");
    if (runsOn === undefined) {
      if (!lines.some((line) => /^ {4}uses: \.\/\.github\/workflows\//.test(line))) problems.push(`${name}: no runner`);
      continue;
    }
    if (runsOn !== "${{ matrix.runner }}") {
      if (!allowed.has(runsOn)) problems.push(`${name}: ${runsOn}`);
      continue;
    }
    const named = lines.filter((line) => /(?:^\s+(?:- )?|[{,]\s*)runner:/.test(line));
    if (named.length === 0) problems.push(`${name}: no matrix runner`);
    for (const line of named) {
      const runner = line.match(/^\s+(?:- )?runner: ([A-Za-z0-9._-]+)$/)?.[1];
      if (runner === undefined) problems.push(`${name}: runner line ${line.trim()}`);
      else if (!allowed.has(runner)) problems.push(`${name}: ${runner}`);
    }
  }
  return problems;
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
  assert.deepEqual(exposedJobs(release), ["build"], "every other job reads no secret and needs no environment");
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
  for (const name of (await readdir(WORKFLOWS)).filter((name) => /\.ya?ml$/.test(name))) {
    const text = await workflow(name);
    assert.doesNotMatch(text, /pull_request_target|head\.repo/, name);
    assert.deepEqual(runnerProblems(text), [], name);
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

test("a release builds the bundled tmux from its pinned script before staging sidecars and the Tauri bundle, on macOS and Linux only, and checks it is signed on macOS (DEC-157)", async () => {
  const release = await workflow("release.yml");
  const build = steps(release, "build");
  const index = (name) => build.findIndex((step) => new RegExp(`^\\s*(- )?name: ${name}$`, "m").test(step));
  const tmux = index("Build bundled tmux");
  const stage = index("Stage desktop sidecars");
  const bundle = index("Build desktop bundle");
  const verify = index("Check the bundled tmux signature");
  assert(tmux >= 0 && stage >= 0 && bundle >= 0 && verify >= 0, "all four steps exist");
  assert(tmux < stage && stage < bundle && bundle < verify, "tmux is built, then staged, then bundled, then checked");
  assert.match(build[tmux], /^ {8}if: matrix\.os != 'windows'$/m);
  assert.match(build[tmux], /scripts\/release\/build-tmux\.sh --target "\$TARGET" --work "\$work\/build" --out "\$work\/crazyagent-tmux"/);
  assert.match(build[tmux], /TMUX_SIDECAR=\$work\/crazyagent-tmux" >> "\$GITHUB_ENV"/);
  assert(!/continue-on-error/.test(build[tmux]), "a failed tmux check, such as a weak import, fails the release");
  const rosetta = index("Install Rosetta for the x86_64 tmux start check");
  assert(rosetta >= 0 && rosetta < tmux, "Rosetta is in place before the x86_64 tmux is built and started");
  assert.match(build[rosetta], /^ {8}if: matrix\.target == 'x86_64-apple-darwin'$/m);
  const install = build[rosetta].indexOf("sudo softwareupdate --install-rosetta --agree-to-license");
  const ownRunner = build[rosetta].indexOf('if [ "$RUNNER_ENVIRONMENT" != github-hosted ]; then');
  assert(install > 0 && ownRunner > 0 && ownRunner < install, "Rosetta is installed only on a GitHub-hosted runner");
  assert.match(build[rosetta].slice(ownRunner, install), /::error::[^\n]*install Rosetta[^\n]*\n\s+exit 1\n/, "on the owner's runner CI installs nothing: it fails and says to install Rosetta");
  assert.match(build[rosetta], /^ {10}if \[ "\$\(uname -m\)" = arm64 \] && ! \/usr\/bin\/arch -x86_64 \/usr\/bin\/true 2> \/dev\/null; then$/m, "only an Apple silicon runner without Rosetta needs it");
  assert.match(build[rosetta], /^ {10}\/usr\/bin\/arch -x86_64 \/usr\/bin\/true$/m, "the step fails when x86_64 still cannot run");
  assert(!/continue-on-error/.test(build[rosetta]), "a failed Rosetta install fails the release, never skips the start check");
  assert.match(build[stage], /--tmux "\$TMUX_SIDECAR"/);
  assert.match(build[index("Install Linux build packages")], /--no-install-recommends [^\n]*\bbison\b[^\n]*\bmusl-tools\b/);
  assert.match(build[verify], /^ {8}if: matrix\.os == 'macos'$/m);
  assert.match(build[verify], /Contents\/MacOS\/crazyagent-tmux/);
  assert.match(build[verify], /codesign --verify --strict/);
  assert.match(build[verify], /flags=.*runtime/);
  const ci = await workflow("ci.yml");
  assert(!/TMUX_(VERSION|SHA256)/.test(ci), "lint-only CI builds no tmux, so it pins none (DEC-143)");
});

test("no release step pipes a tool into grep -q, since under pipefail an early grep exit fails a good check or passes a bad one (DEC-157)", async () => {
  const release = await workflow("release.yml");
  assert.deepEqual(release.split("\n").filter((line) => /\|\s*grep\b[^|]*\s-[A-Za-z]*q/.test(line)), [], "each check captures its tool's output, then greps it");
  const build = steps(release, "build");
  const stage = build.find((step) => /^\s*(- )?name: Stage desktop sidecars$/m.test(step));
  assert.match(stage, /\$\{tmux\[@\]\+"\$\{tmux\[@\]\}"\}/, "an empty tmux list expands to nothing under set -u on any bash");
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
  const linux = steps(ci, "linux").join("\n");
  assert.match(linux, /dpkg -s "\$package"/);
  assert.match(linux, /sudo -n true/);
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
  for (const [name, job, title] of [["ci.yml", "macos", "Install pinned Node on the owner's runner"], ["ci.yml", "macos", "Install pinned arm64 bun on the owner's runner"],
    ["release.yml", "build", "Install x86_64 bun for the Intel rcli host"], ["release.yml", "build", "Install pinned Node on the owner's runner"],
    ["release.yml", "build", "Install pinned arm64 bun for macOS"], ["release.yml", "build", "Install pinned x64 bun for Linux"]]) {
    const step = steps(await workflow(name), job).find((text) => text.includes(`name: ${title}`));
    assert(step, `${job}: ${title}`);
    assert.doesNotMatch(step, /\.complete/, `${job}: ${title} keeps no install marker`);
    assert.match(step, /\n\s+if ! echo "\$[A-Z0-9_]+_SHA256  \$work\/[a-z.]+" \| (sha256sum -c --status -|shasum -a 256 -s -c -) 2>\/dev\/null; then\n/, `${job}: ${title} checks the cached archive every time`);
    assert.match(step, /\n\s+cp "\$work\/([a-z.]+)" "\$prefix\/\1\.\$\$"\n\s+mv "\$prefix\/\1\.\$\$" "\$prefix\/\1"\n/, `${job}: ${title} replaces the cached archive in one rename`);
    assert.match(step, /"\$work\/bin" >> "\$GITHUB_PATH"|bun-path=\$work\/bin\/bun/, `${job}: ${title} runs the copy it just checked`);
  }
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

test("a release signs with pinned, checksum-checked Node and Rust on the owner's runner and embeds a checksum-checked bun on every runner, never the tool cache, rustup or Cargo settings CI left on the runner", async () => {
  const release = await workflow("release.yml");
  for (const pin of ["BUN_DARWIN_ARM64_SHA256", "BUN_LINUX_X64_SHA256", "NODE_DARWIN_ARM64_SHA256", "RUSTC_DARWIN_ARM64_SHA256", "CARGO_DARWIN_ARM64_SHA256", "RUST_STD_DARWIN_ARM64_SHA256", "RUST_STD_DARWIN_X64_SHA256"]) {
    assert.match(release, new RegExp(`^ {2}${pin}: [0-9a-f]{64}$`, "m"), pin);
  }
  const all = steps(release, "build");
  const at = (pattern) => all.findIndex((step) => pattern.test(step));
  for (const shared of [/uses: actions\/setup-node@/, /rustup toolchain install/]) {
    const step = all[at(shared)];
    assert(step, String(shared));
    assert.match(step, /^ {6}- (?:id: \S+\n {8})?(?:name: [^\n]+\n {8})?if: runner\.environment == 'github-hosted'\n/, `${shared} runs only on GitHub's fresh runners`);
  }
  assert.doesNotMatch(release, /uses: oven-sh\/setup-bun@/, "no release runner builds with setup-bun's unchecked bun");
  const linux = all[at(/id: bun\n/)];
  assert.match(linux, /^ {6}- id: bun\n {8}name: Install pinned x64 bun for Linux\n {8}if: matrix\.os == 'linux'\n/, "every Linux runner, GitHub-hosted included, runs the pinned x64 bun");
  assert.match(linux, /\/bun-v\$BUN_VERSION\/bun-linux-x64\.zip"/);
  assert.match(linux, /\n\s+echo "\$BUN_LINUX_X64_SHA256  \$work\/bun\.zip" \| sha256sum -c -\n/, "a downloaded Linux bun that does not match its pin fails the job");
  assert.match(linux, /\n\s+install -m 0755 "\$work\/bun-linux-x64\/bun" "\$work\/bin\/bun"\n/);
  const rust = all[at(/name: Install pinned Rust on the owner's runner/)];
  assert(rust, "the release on the owner's runner installs its own Rust");
  assertPinnedRust(rust, "release");
  assert(rust.includes("$RUST_STD_DARWIN_X64_SHA256"), "RUST_STD_DARWIN_X64_SHA256");
  const node = at(/name: Install pinned Node on the owner's runner/);
  const bun = at(/id: bun-arm64\n/);
  assert.match(all[node], /\n {8}if: runner\.environment != 'github-hosted'\n/);
  assert.match(all[bun], /^ {6}- id: bun-arm64\n {8}name: [^\n]+\n {8}if: matrix\.os == 'macos'\n/, "every macOS runner, GitHub-hosted included, runs the pinned arm64 bun");
  assert.match(all[node], /node-v\$NODE_VERSION-darwin-arm64\.tar\.gz/);
  assert.match(all[bun], /bun-darwin-aarch64\.zip/);
  assert.match(all[bun], /\n\s+echo "\$BUN_DARWIN_ARM64_SHA256  \$work\/bun\.zip" \| shasum -a 256 -c -\n/, "a downloaded arm64 bun that does not match its pin fails the job");
  for (const name of ["Install rcli source dependencies", "Build rcli host"]) {
    const index = at(new RegExp(`name: ${name}\n`));
    assert.equal(all[index].match(/\n\s+BUN_PATH: (.+)\n/)[1], "${{ steps.bun-x64.outputs.bun-path || steps.bun-arm64.outputs.bun-path || steps.bun.outputs.bun-path }}", name);
    assert(bun < index && at(/id: bun-x64\n/) < index && at(/id: bun\n/) < index, `${name}: an rcli gets the checksum-checked bun of its runner`);
    for (const id of all[index].match(/\bsteps\.([a-z0-9-]+)\.outputs\.bun-path\b/g).map((ref) => ref.split(".")[1])) {
      const source = all[at(new RegExp(`^ {6}- id: ${id}\\n`))];
      assert(source, `${name}: ${id} is a step of this job`);
      assert.doesNotMatch(source, /\n {8}uses: /, `${name}: ${id} is a step this workflow writes, not an action`);
      assert.match(source, /\n\s+echo "\$BUN_[A-Z0-9_]+_SHA256  \$work\/bun\.zip" \| (?:sha256sum|shasum -a 256) -c -\n/, `${name}: ${id} checks its bun against a pinned SHA-256`);
    }
  }
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
  assert(node < at(/\bnpm ci\b/), "no npm run comes before the pinned Node");
  assert(bun < at(/\bbun install\b/), "no bun run comes before the pinned bun");
  assert(at(/name: Install pinned Rust/) < at(/\bcargo fetch\b/) && at(/name: Install pinned Rust/) < at(/\bmake lint\b/), "no cargo run comes before the pinned Rust");
  const bunPaths = all.join("\n").split("\n").filter((line) => line.includes("outputs.bun-path"));
  assert.equal(bunPaths.length, 0, "lint-only CI runs no Node or Bun suite, so no step names a bun");
});

test("nothing macOS CI runs on the owner's runner installs globally or reads or writes the runner user's home", async () => {
  const all = steps(await workflow("ci.yml"), "macos");
  const index = all.findIndex((step) => /^ {6}- name: Keep CI state out of the runner user's home\n/.test(step));
  const home = all[index];
  assert.match(home, /^ {6}- name: Keep CI state out of the runner user's home\n {8}if: runner\.environment != 'github-hosted'\n/, "only on the owner's runner");
  assert(index > 0 && index < all.findIndex((step) => step.includes("uses: actions/checkout@")), "it moves HOME and TMPDIR before the checkout or any tool runs");
  assert.match(home, /\n\s+state="\$RUNNER_TOOL_CACHE\/crazyagent-ci"\n/);
  assert.match(home, /\n\s+home="\$RUNNER_TEMP\/home"\n/);
  assert.match(home, /\n\s+rm -rf "\$state"\n\s+mkdir -p "\$state"\n/, "nothing an earlier job left is kept");
  for (const line of ['echo "HOME=$home"', 'echo "TMPDIR=$RUNNER_TEMP/tmp"', 'echo "CARGO_HOME=$state/cargo-home"', 'echo "CARGO_TARGET_DIR=$target"',
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
  const home = all.find((step) => /^ {6}- name: Keep CI state out of the runner user's home\n/.test(step));
  assert(home, "the owner's runner sets up its own state");
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
  const lint = all.findIndex((step) => /- name: Lint workspace\n/.test(step));
  assert(lint > 0 && lint === all.length - 2, "the cleanup is the last step, right after the lint");
  const last = all.at(-1);
  assert.equal((last.match(/df -Pk "\$RUNNER_TOOL_CACHE"/g) ?? []).length, 1, "one free-space reader");
  assert.equal((last.match(/\$\(free_gib\) GiB free/g) ?? []).length, 2, "free space after the lint and after the cleanup");
  assert.match(last, /\n\s+target="\$RUNNER_TOOL_CACHE\/crazyagent-ci\/target-line-tables"\n\s+if \[ -d "\$target" \]; then\n\s+du -sk "\$target" \| awk '\{ printf "Rust target: %\.1f GiB\\n", \$1 \/ 1048576 \}'\n\s+fi\n/, "the target size shows, so the floor can follow the real need");
  assert(last.indexOf('du -sk "$target"') < last.indexOf("rm -rf"), "the size is read before anything goes");
  assertDeletesEverything(all, "CI");
});

test("a release on the owner's runner deletes every file it made after the last upload, even after a failure or cancel", async () => {
  const all = steps(await workflow("release.yml"), "build");
  assert.match(all.at(-2), /^ {6}- uses: actions\/upload-artifact@[0-9a-f]{40}\n {8}with:\n {10}name: signing-/, "the cleanup comes after the last upload");
  assertDeletesEverything(all, "release");
});

test("Linux CI frees the hosted runner's unused toolsets first, builds with line tables only and shows the free disk and target size after the lint", async () => {
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
  assert.match(all.at(-2), /^ {6}- name: Lint workspace\n/, "the disk report comes right after the lint");
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

const SCRIPT_MS = 10_000;
const HDIUTIL_RETRY = fileURLToPath(new URL("./hdiutil-retry/", import.meta.url));
const FAKE_SLEEP = String.raw`printf '%s\n' "$1" >> "$FAKE_STATE/sleeps"
`;
const FAKE_GH = String.raw`state="$FAKE_STATE"
printf '%s\n' "$*" >> "$state/calls"
stale="$(cat "$state/stale")"
if [ "$1 $2" = "release view" ] || [ "$1 $2" = "release delete" ]; then exit 0; fi
if [ "$1 $2" = "release create" ]; then
  if [ "$stale" -gt 0 ]; then echo "HTTP 422: Validation Failed: tag_name already exists" >&2; exit 1; fi
  exit 0
fi
if [ "$1" = api ] && [ "$2" = -X ] && [ "$3" = DELETE ]; then echo "gh: Reference does not exist (HTTP 422)" >&2; exit 1; fi
if [ "$1" = api ] && [ -e "$state/read-fails" ]; then echo "gh: Server Error (HTTP 502)" >&2; exit 1; fi
gone='[{"ref":"refs/tags/dev-old"}]'
refs="$gone"
if [ "$1" = api ] && [ "$stale" -gt 0 ]; then
  echo "$((stale - 1))" > "$state/stale"
  refs='[{"ref":"refs/tags/dev"},{"ref":"refs/tags/dev-old"}]'
fi
case "$1 $2 $3" in
  "api repos/example/fake-repo/git/ref/tags/dev ")
    if [ "$refs" = "$gone" ]; then echo "gh: Not Found (HTTP 404)" >&2; exit 1; fi
    echo '{"ref":"refs/tags/dev"}'
    exit 0 ;;
  "api repos/example/fake-repo/git/matching-refs/tags/dev --jq")
    if [ -e "$state/answer" ]; then cat "$state/answer"; exit 0; fi
    printf '%s' "$refs" | jq "$4"
    exit $? ;;
esac
echo "fake gh: unexpected call: $*" >&2
exit 2
`;
const FAKE_HDIUTIL = String.raw`for image; do :; done
if [ -e "$image" ]; then seen=present; else seen=absent; fi
printf '%s %s\n' "$1" "$seen" >> "$FAKE_STATE/calls"
[ -e "$image" ] || : > "$image"
fails="$(cat "$FAKE_STATE/fails")"
if [ "$fails" -gt 0 ]; then
  echo "$((fails - 1))" > "$FAKE_STATE/fails"
  echo "hdiutil: $1 failed - Resource busy" >&2
  exit "$(cat "$FAKE_STATE/status")"
fi
exit 0
`;

/**
 * Returns the shell a workflow step runs, as GitHub hands it to bash.
 * Input: one step from `steps`, whose last key is `run: |`. Output: the run body with the step's
 * indentation removed.
 */
function runBody(step) {
  const lines = step.split("\n");
  const start = lines.indexOf("        run: |");
  assert(start >= 0, "the step has a run block");
  const body = lines.slice(start + 1);
  assert(body.every((line) => line === "" || line.startsWith("          ")), "run is the step's last key");
  return `${body.map((line) => line.slice(10)).join("\n")}\n`;
}

/**
 * Writes executable bash stand-ins into a folder. Inputs: the folder and a map from file name to
 * script body.
 */
function standIns(folder, scripts) {
  mkdirSync(folder, { recursive: true });
  for (const [name, body] of Object.entries(scripts)) {
    writeFileSync(join(folder, name), `#!/bin/bash\n${body}`);
    chmodSync(join(folder, name), 0o755);
  }
}

/** The lines a stand-in appended to one state file. Inputs: the state folder and the file name. Output: the lines, or none. */
function logged(state, name) {
  const file = join(state, name);
  return existsSync(file) ? readFileSync(file, "utf8").split("\n").filter(Boolean) : [];
}

/**
 * Runs the dev publish step's shell against a stand-in `gh` that holds no token and can reach no
 * repository: `stale` tag reads still show the dev tag after the release delete, the ref DELETE
 * always answers 422 "Reference does not exist", `readFails` makes every tag read error, and
 * `answer`, when given, is printed as every tag read's output instead of the filtered refs.
 * Inputs: the step's shell and the stand-in state. Output: the exit status, output, `gh` calls and sleeps.
 */
function publishDev(body, { stale, readFails = false, answer = null }) {
  const root = mkdtempSync(join(tmpdir(), "ca-relwf-"));
  try {
    const state = join(root, "state");
    const work = join(root, "work");
    mkdirSync(state);
    mkdirSync(join(work, "dist"), { recursive: true });
    mkdirSync(join(root, "home"));
    writeFileSync(join(state, "stale"), `${stale}\n`);
    if (readFails) writeFileSync(join(state, "read-fails"), "");
    if (answer !== null) writeFileSync(join(state, "answer"), `${answer}\n`);
    writeFileSync(join(work, "notes.txt"), "Dev build\n");
    writeFileSync(join(work, "dist", "CrazyAgent.dmg"), "");
    writeFileSync(join(root, "step.sh"), body);
    standIns(join(root, "bin"), { gh: FAKE_GH, sleep: FAKE_SLEEP });
    const run = spawnSync("/bin/bash", ["--noprofile", "--norc", "-eo", "pipefail", join(root, "step.sh")], {
      cwd: work,
      encoding: "utf8",
      timeout: SCRIPT_MS,
      env: { PATH: `${join(root, "bin")}:/usr/bin:/bin`, HOME: join(root, "home"), FAKE_STATE: state, GITHUB_REPOSITORY: "example/fake-repo", GITHUB_SHA: "0123456789abcdef0123456789abcdef01234567", VERSION: "0.0.0-dev.1" },
    });
    assert.equal(run.error, undefined, `the step ends within ${SCRIPT_MS} ms`);
    return { status: run.status, stdout: run.stdout, stderr: run.stderr, calls: logged(state, "calls"), sleeps: logged(state, "sleeps").map(Number) };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

/**
 * Runs scripts/release/hdiutil-retry/hdiutil in front of a stand-in hdiutil whose first `fails`
 * calls leave a half-made image and fail with "Resource busy" and `status`. Inputs: the arguments,
 * with IMAGE for the image path, and the stand-in state; `existing` makes the image before the run.
 * Output: the exit status, stderr, the stand-in's calls, the sleeps and the image's text.
 */
function hdiutil(args, { fails, status = 1, existing = false }) {
  const root = mkdtempSync(join(tmpdir(), "ca-relwf-"));
  try {
    const state = join(root, "state");
    const image = join(root, "rw.1.CrazyAgent.dmg");
    mkdirSync(state);
    writeFileSync(join(state, "fails"), `${fails}\n`);
    writeFileSync(join(state, "status"), `${status}\n`);
    if (existing) writeFileSync(image, "kept");
    standIns(join(root, "bin"), { sleep: FAKE_SLEEP });
    standIns(join(root, "real"), { hdiutil: FAKE_HDIUTIL });
    const run = spawnSync(join(HDIUTIL_RETRY, "hdiutil"), args.map((arg) => (arg === "IMAGE" ? image : arg)), {
      encoding: "utf8",
      timeout: SCRIPT_MS,
      env: { PATH: `${join(root, "bin")}:/usr/bin:/bin`, HOME: root, FAKE_STATE: state, CRAZYAGENT_REAL_HDIUTIL: join(root, "real", "hdiutil") },
    });
    assert.equal(run.error, undefined, `the hdiutil retry ends within ${SCRIPT_MS} ms`);
    return { status: run.status, stderr: run.stderr, calls: logged(state, "calls"), sleeps: logged(state, "sleeps").map(Number), kept: existsSync(image) ? readFileSync(image, "utf8") : null };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

/** The dev publish step's shell from release.yml. Output: the run body of "Publish dev prerelease". */
async function devPublishBody() {
  return runBody(steps(await workflow("release.yml"), "publish").find((step) => step.includes("- name: Publish dev prerelease")));
}

/** How many dev tag reads a dev publish run made. Input: a `publishDev` result. Output: the count. */
function reads(run) {
  return run.calls.filter((call) => call.startsWith("api repos/example/fake-repo/git/matching-refs/tags/dev ")).length;
}

/** How many times a dev publish run created the dev release. Input: a `publishDev` result. Output: the count. */
function created(run) {
  return run.calls.filter((call) => call.startsWith("release create dev ")).length;
}

test("the dev publish tolerates a tag delete that finds the tag already gone and waits, bounded, until the tag reads gone before it creates the dev release", async () => {
  const jq = spawnSync("/bin/bash", ["-c", "command -v jq"], { encoding: "utf8", env: { PATH: "/usr/bin:/bin" } });
  assert.equal(jq.status, 0, "the stand-in gh runs the step's --jq filter with jq, which must be on /usr/bin:/bin");
  const body = await devPublishBody();

  const stale = publishDev(body, { stale: 2 });
  assert.equal(stale.status, 0, `${stale.stdout}${stale.stderr}`);
  assert.equal(reads(stale), 3, "two reads still show the tag, the third reads it gone");
  assert.equal(created(stale), 1);
  assert.match(stale.calls.at(-1), /^release create dev /, "the release is created only after the tag reads gone");
  assert.deepEqual(stale.sleeps, [2, 4]);

  const never = publishDev(body, { stale: 99 });
  assert.notEqual(never.status, 0, "a tag that never reads gone fails the publish");
  assert.match(never.stdout, /::error::the dev tag still reads present after 5 checks/);
  assert.equal(reads(never), 5);
  assert.equal(created(never), 0, "nothing is published while the tag reads present");
  assert(never.sleeps.reduce((sum, seconds) => sum + seconds, 0) <= 30, "the wait is bounded");

  const broken = publishDev(body, { stale: 0, readFails: true });
  assert.notEqual(broken.status, 0, "a tag read that errors fails the publish and never counts as gone");
  assert.equal(created(broken), 0);
});

test("the dev publish fails at once with an error, deleting and publishing nothing, when a dev tag read gives anything but 0 or 1", async () => {
  const body = await devPublishBody();
  for (const answer of ["2", "not-a-count"]) {
    const run = publishDev(body, { stale: 0, answer });
    assert.notEqual(run.status, 0, `a read of '${answer}' fails the publish`);
    assert(run.stdout.split("\n").includes(`::error::reading the dev tag gave '${answer}', not 0 or 1`), `${run.stdout}${run.stderr}`);
    assert.equal(reads(run), 1, "the first bad read stops the step");
    assert(!run.calls.some((call) => call.startsWith("api -X DELETE")), "no tag is deleted on a bad read");
    assert.equal(created(run), 0, "nothing is published on a bad read");
    assert.deepEqual(run.sleeps, []);
  }
});

test("on macOS the release build puts the hdiutil retry first on PATH before Tauri builds, so a busy hdiutil create is retried inside the dmg step without signing or notarizing the app again", async () => {
  const build = steps(await workflow("release.yml"), "build").find((step) => step.includes("id: desktop"));
  const macos = [
    '          if [ "$PLATFORM" = macos ]; then',
    '            export TMPDIR="$RUNNER_TEMP/macos-tmp"',
    '            mkdir -p "$TMPDIR"',
    '            export PATH="$GITHUB_WORKSPACE/scripts/release/hdiutil-retry:$PATH"',
    "          fi",
  ].join("\n");
  assert(build.includes(macos), "the macOS block exports the hdiutil retry folder ahead of PATH");
  assert(build.indexOf(macos) < build.indexOf("tauri.js build"), "PATH is set before Tauri runs bundle_dmg.sh");
  assert.equal((await workflow("release.yml")).split("\n").filter((line) => line.includes("hdiutil-retry")).length, 1, "only the macOS desktop build uses it");
  assert.deepEqual(readdirSync(HDIUTIL_RETRY), ["hdiutil"], "the folder that sits ahead of /usr/bin while codesign and notarytool run holds nothing but hdiutil");
  assert.notEqual(statSync(join(HDIUTIL_RETRY, "hdiutil")).mode & 0o111, 0, "the hdiutil retry is executable");
});

test("the hdiutil retry tries a busy hdiutil create three times, removing the half-made image between tries, and passes the last failure through", () => {
  const create = ["create", "-srcfolder", "src", "-volname", "CrazyAgent", "-format", "UDRW", "IMAGE"];
  const busy = hdiutil(create, { fails: 2 });
  assert.equal(busy.status, 0, busy.stderr);
  assert.deepEqual(busy.calls, ["create absent", "create absent", "create absent"], "each try starts with no half-made image");
  assert.deepEqual(busy.sleeps, [5, 10]);
  assert.match(busy.stderr, /hdiutil-retry: hdiutil create, try 3 of 3/);

  const broken = hdiutil(create, { fails: 9, status: 35 });
  assert.equal(broken.status, 35, "the last try's status");
  assert.equal(broken.calls.length, 3);
  assert.equal(broken.stderr.match(/hdiutil: create failed - Resource busy/g)?.length, 3, "every try's error is shown, the last one included");
  assert.deepEqual(broken.sleeps, [5, 10]);
});

test("the hdiutil retry runs every other hdiutil command, and a create over an existing image, exactly once", () => {
  const attach = hdiutil(["attach", "-readwrite", "-noverify", "IMAGE"], { fails: 1 });
  assert.equal(attach.status, 1);
  assert.deepEqual(attach.calls, ["attach absent"]);
  assert.deepEqual(attach.sleeps, []);

  const over = hdiutil(["create", "-srcfolder", "src", "IMAGE"], { fails: 1, existing: true });
  assert.equal(over.status, 1);
  assert.deepEqual(over.calls, ["create present"]);
  assert.equal(over.kept, "kept", "an image that was there before is never deleted");
  assert.deepEqual(over.sleeps, []);
});

test("the release job marks every engine it builds for a target that ships crazyagent-tmux, at job level so no later cargo step rebuilds it unmarked, and the engine reads that exact mark (leader 20:15)", async () => {
  const text = await workflow("release.yml");
  const env = block(block(text, 2, "build").join("\n"), 4, "env");
  assert(env.includes("      CRAZYAGENT_BUNDLES_TMUX: ${{ matrix.os != 'windows' && '1' || '0' }}"), env.join("\n"));
  assert.equal(text.split("CRAZYAGENT_BUNDLES_TMUX").length - 1, 1, "no step sets the mark again");
  const tools = readFileSync(fileURLToPath(new URL("../../crates/crazyagent/src/tools.rs", import.meta.url)), "utf8");
  assert.match(tools, /pub const SHIPS_BUNDLED_TMUX: bool = match option_env!\("CRAZYAGENT_BUNDLES_TMUX"\) \{\n {4}Some\(mark\) => matches!\(mark\.as_bytes\(\), \[b'1'\]\),\n {4}None => false,\n\};/);
});

test("the secret and runner guards read every job of every workflow file, matrix runner values included (RV-DS241-38)", async () => {
  const names = (await readdir(WORKFLOWS)).filter((name) => /\.ya?ml$/.test(name)).sort();
  assert(names.includes("ci.yml") && names.includes("release.yml"), names.join(" "));
  const exposed = [];
  for (const name of names) {
    const text = await workflow(name);
    exposed.push(...exposedJobs(text).map((job) => `${name}:${job}`));
    assert.deepEqual(runnerProblems(text), [], name);
  }
  assert.deepEqual(exposed, ["release.yml:build"], "only the release build reads a secret or binds an environment");
  const added = [
    "jobs:",
    "  lint:",
    "    runs-on: ubuntu-22.04",
    "    steps:",
    "      - run: echo ${{ secrets.TOKEN }}",
    "  deploy:",
    "    runs-on: ubuntu-latest",
    "    environment: production",
    "  odd:",
    "    runs-on: ${{ matrix.runner }}",
    "    strategy:",
    "      matrix:",
    "        include:",
    "          - runner: macos-15",
    "          - runner: self-hosted",
    "",
  ].join("\n");
  assert.deepEqual(exposedJobs(added), ["lint", "deploy"]);
  assert.deepEqual(runnerProblems(added), ["deploy: ubuntu-latest", "odd: self-hosted"]);
});

test("the secret guard also catches a called workflow's secrets key and a workflow-level secret every job inherits, both guards refuse a workflow whose jobs do not parse, and a matrix runner that is not one plain value is a problem (RV-DS241-38 r2)", () => {
  const call = [
    "jobs:",
    "  ci:",
    "    uses: ./.github/workflows/ci.yml",
    "    secrets: inherit",
    "  plain:",
    "    runs-on: ubuntu-22.04",
    "",
  ].join("\n");
  assert.deepEqual(exposedJobs(call), ["ci"]);
  const inherited = [
    "env:",
    "  TOKEN: ${{ secrets.TOKEN }}",
    "jobs:",
    "  lint:",
    "    runs-on: ubuntu-22.04",
    "  test:",
    "    runs-on: ubuntu-22.04",
    "",
  ].join("\n");
  assert.deepEqual(exposedJobs(inherited), ["lint", "test"]);
  for (const jobs of [
    ["jobs:", "    build:", "      runs-on: self-hosted"],
    ["jobs:", '  "build":', "    runs-on: self-hosted"],
    ["jobs:", "  lint:", "    runs-on: ubuntu-22.04", "  deploy: {runs-on: self-hosted}"],
    ["jobs:", "  lint:", "    runs-on: ubuntu-22.04", "  deploy: # prod", "    runs-on: self-hosted"],
    ["jobs:", "      runs-on: self-hosted", "  lint:", "    runs-on: ubuntu-22.04"],
  ]) {
    const text = [...jobs, ""].join("\n");
    assert.throws(() => exposedJobs(text), /job/, text);
    assert.throws(() => runnerProblems(text), /job/, text);
  }
  const runners = [
    "jobs:",
    "  odd:",
    "    runs-on: ${{ matrix.runner }}",
    "    strategy:",
    "      matrix:",
    "        include:",
    "          - runner: macos-15",
    "          - runner: [ubuntu-22.04, self-hosted]",
    "  tail:",
    "    runs-on: ${{ matrix.runner }}",
    "    strategy:",
    "      matrix:",
    "        runner: self-hosted # pinned",
    "",
  ].join("\n");
  assert.deepEqual(runnerProblems(runners), ["odd: runner line - runner: [ubuntu-22.04, self-hosted]", "tail: runner line runner: self-hosted # pinned"]);
});

test("a run block line that starts with # is script text GitHub still expands, so a secret there exposes its job; only a comment at job-key indent is passed over (RV-DS241-38 r2b)", () => {
  const scripted = [
    "jobs:",
    "  # the jobs",
    "  build:",
    "    runs-on: ubuntu-22.04",
    "  publish:",
    "    runs-on: ubuntu-22.04",
    "    steps:",
    "      - run: |",
    "          # token ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}",
    "          echo done",
    "",
  ].join("\n");
  assert.deepEqual(exposedJobs(scripted), ["publish"]);
  assert.deepEqual(runnerProblems(scripted), []);
});
