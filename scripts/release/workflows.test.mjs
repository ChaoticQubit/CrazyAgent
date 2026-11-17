import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { readdir, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

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
 * Lists the jobs under `jobs:` of a workflow, failing closed: every non-blank line of the block
 * deeper than 2-space indent belongs to a job whose key sits alone at 2-space indent, `#`-led
 * run block lines included, since GitHub expands them; only a `#` line at indent 2 or less is a
 * YAML comment passed over. A job the guards cannot read throws instead of being skipped.
 * Input: the workflow text. Output: each job's name with its lines, at least one.
 */
function jobsOf(text) {
  const lines = text.split("\n");
  const start = lines.indexOf("jobs:");
  assert(start >= 0, "no jobs: block at indent 0, so no job can be read");
  const jobs = [];
  for (const line of lines.slice(start + 1)) {
    if (line.trim() === "") continue;
    if (line.trimStart().startsWith("#") && line.length - line.trimStart().length <= 2) continue;
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
 * Matches workflow text that reads a secret, in any form GitHub expands and in any case, as the
 * runner names contexts without case: `secrets.NAME`, `SECRETS['NAME']`, a whole-context call
 * such as `toJSON(secrets)`, or the bare context.
 */
const SECRET = /secrets\.|\bsecrets\b/i;

/**
 * Whether a YAML line leaves a quoted scalar open, so it runs on into the next lines: a quote
 * opens a scalar only where a node starts, after the indent, `- `, `: `, `? `, `[`, `{` or `,`
 * and any anchor (`&a`) or tag (`!!str`) in front of it, and a `#` after a space outside one
 * starts a comment. Input: one line outside a block scalar. Output: the answer.
 */
function leftOpen(line) {
  let quote = "";
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (quote !== "") {
      if (char !== quote) continue;
      if (quote === "'" && line[index + 1] === "'") index += 1;
      else quote = "";
    } else if (char === "#" && /(?:^|\s)$/.test(line.slice(0, index))) {
      return false;
    } else if ((char === "'" || char === '"') && /(?:^\s*|[-:?]\s+|[[{,]\s*)(?:[&!]\S*\s+)*$/.test(line.slice(0, index))) {
      quote = char;
    }
  }
  return quote !== "";
}

/**
 * Lists the lines of a workflow, outside block scalars (`|`, `>`), that YAML reads as more than
 * their text: an alias, which hands a job another's keys without their words; a backslash, which
 * a double-quoted scalar decodes; and a quoted scalar left open, which runs on into lines a block
 * indicator would hide (B17 F1). Input: the workflow text. Output: the lines.
 */
function unreadLines(text) {
  const unread = [];
  let block = -1;
  for (const line of text.split("\n")) {
    const indent = line.length - line.trimStart().length;
    if (block >= 0 && (line.trim() === "" || indent > block)) continue;
    const opener = line.match(/^(\s*(?:- +)*)(?:[^\s#'"][^#'"]*:\s+)?[|>][-+0-9]*\s*(?:#.*)?$/);
    block = opener ? opener[1].length : -1;
    if (line.includes("\\") || /(?:^\s*|[-:?]\s+|[[{,]\s*)\*\S/.test(line) || leftOpen(line)) unread.push(line);
  }
  return unread;
}

/**
 * Names the jobs of a workflow that read a secret, hand secrets to a called workflow or bind an
 * environment, whatever the file or job; a secret outside `jobs:` reaches every job. A workflow
 * whose YAML says more than its lines ([unreadLines]) is refused, as is one whose jobs do not
 * parse. Input: the workflow text. Output: the job names in file order.
 */
function exposedJobs(text) {
  const jobs = jobsOf(text);
  const unread = unreadLines(text);
  assert(unread.length === 0, `the secret guard reads YAML line by line, so it refuses an alias, a backslash or a quoted scalar left open outside a block scalar: ${JSON.stringify(unread)}`);
  const shared = sharedLines(text).some((line) => SECRET.test(line));
  return jobs.filter(({ lines }) => shared || lines.some((line) => SECRET.test(line) || /^ {4}(environment|secrets):/.test(line))).map(({ name }) => name);
}

/**
 * Lists the lines of a local composite action the secret guard refuses, by the workflow rules:
 * a line YAML reads as more than its text ([unreadLines]), and a line that reads a secret or
 * binds an environment, since every job that uses the action runs it. Input: the action text.
 * Output: the lines.
 */
function actionSecretLines(text) {
  return [...unreadLines(text), ...text.split("\n").filter((line) => SECRET.test(line) || /^\s*(environment|secrets):/.test(line))];
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
  assert.doesNotMatch(ci, SECRET);
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
  assert(release.split("\n").filter((line) => SECRET.test(line)).length > 0);
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

/**
 * Returns the steps of `job` in `workflow` text as one string per step.
 * Inputs: the workflow text and the job name. Output: each `- ` step with its indented lines.
 */
function steps(workflowText, job) {
  return block(block(workflowText, 0, "jobs").join("\n"), 2, job).join("\n").split(/\n(?= {6}- )/).slice(1);
}

const ACTIONS = new URL("../../.github/actions/", import.meta.url);

/**
 * Reads every local composite action under .github/actions, one folder each. Input: none.
 * Output: { name, text } per action, in name order.
 */
async function localActions() {
  const names = (await readdir(ACTIONS, { withFileTypes: true })).filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort();
  const actions = [];
  for (const name of names) {
    const text = await readFile(new URL(`${name}/action.yml`, ACTIONS), "utf8");
    assert(!text.includes("\r"), `${name} has LF line endings`);
    actions.push({ name, text });
  }
  return actions;
}

/**
 * Reads one local composite action. Input: its folder name under .github/actions. Output: its
 * text.
 */
async function farmAction(name) {
  const found = (await localActions()).find((action) => action.name === name);
  assert(found, `.github/actions/${name} exists`);
  return found.text;
}

/**
 * Returns the steps of a composite action's text as one string per step. Input: the action
 * text. Output: each `- ` step under `runs: steps:` with its indented lines.
 */
function actionSteps(text) {
  const lines = text.split("\n");
  const start = lines.indexOf("  steps:");
  assert(start >= 0 && lines.includes("  using: composite"), "a composite action lists its steps under runs:");
  return lines.slice(start + 1).join("\n").replace(/\s+$/, "").split(/\n(?= {4}- )/);
}

/**
 * Puts one workflow or composite step in a form two places can compare: its own indentation
 * removed, without the lines that only say where it runs, an OS condition or the bash shell a
 * composite step must name. Input: the step text. Output: the compared text.
 */
function stepBody(step) {
  const indent = step.length - step.trimStart().length;
  return step.replace(/\s+$/, "").split("\n").map((line) => line.slice(indent))
    .filter((line) => !/^ {2}(if: (matrix|inputs)\.os [!=]= '[a-z]+'|shell: bash)$/.test(line)).join("\n");
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

test("every GitHub-hosted CI Rust job keeps its build cache with the pinned rust-cache action, saved only from master, even when the job fails, so a pull request or batch branch restores master's entry and adds none to the free 10 GB; the release build, which holds the signing keys, runs no third-party cache action; and no job caches the whole target folder", async () => {
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
      "          save-if: ${{ github.ref == 'refs/heads/master' }}",
    ].join("\n"), job);
    assert(all.findIndex((step) => step.includes("- name: Install Rust toolchain")) < at, `${job} keys its cache on the installed toolchain`);
  }
  assert.doesNotMatch(release, /rust-cache/, "no third-party action runs beside the release signing keys");
  for (const [text, jobs] of [[ci, ["linux", "macos", "windows"]], [release, ["prepare", "build", "publish", "prune-dev-builds"]]]) {
    assert.match(text, /^ {2}CARGO_INCREMENTAL: "0"$/m);
    for (const job of jobs) {
      for (const step of steps(text, job).filter((step) => step.includes("uses: actions/cache@"))) {
        assert.doesNotMatch(step, /\btarget\b|\.cargo\//, `${job}: actions/cache keeps no Rust build output or Cargo registry`);
      }
    }
  }
});

test("a Linux release builds the sandbox helper static in its own folder after the other binaries and puts it where staging reads them, so no dynamic loader runs in its AppArmor profile (DEC-179 SBX-3 fold, rv-ds241 F1)", async () => {
  const release = await workflow("release.yml");
  const step = steps(release, "build").find((text) => /^\s*(- )?name: Build command line binaries$/m.test(text));
  assert(step, "the command line build step exists");
  const all = step.indexOf('cargo build --release --locked --target "$TARGET" -p crazyagent --bins');
  const linux = step.indexOf('if [ "$PLATFORM" = linux ]; then');
  const helper = step.indexOf('RUSTFLAGS="-C target-feature=+crt-static" cargo build --release --locked --target "$TARGET" -p crazyagent --bin crazyagent-landlock --target-dir "${CARGO_TARGET_DIR:-target}/crt-static"');
  const kept = step.indexOf('install -m 0755 "${CARGO_TARGET_DIR:-target}/crt-static/$TARGET/release/crazyagent-landlock" "${CARGO_TARGET_DIR:-target}/$TARGET/release/crazyagent-landlock"');
  assert(all >= 0 && linux > all && helper > linux && kept > helper, "the static helper replaces the one the full build made");
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

/**
 * Returns the value of one workflow-level `env:` entry.
 * Inputs: the workflow text and the variable name. Output: the text after `NAME: ` on its one line.
 */
function envValue(workflowText, name) {
  const lines = block(workflowText, 0, "env").filter((line) => line.startsWith(`  ${name}: `));
  assert.equal(lines.length, 1, `one ${name} in the workflow env`);
  return lines[0].slice(`  ${name}: `.length);
}

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
  assert.doesNotMatch(cleanup, SECRET);
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

test("every Rust test target that needs the staged RCLI host is left out of CI and the test farm, which stage none", async () => {
  const ci = await workflow("ci.yml");
  const farm = await workflow("tests.yml");
  const root = new URL("../../", import.meta.url);
  let needing = 0;
  for (const [folder, text, list] of [
    ["crates/crazyagent/tests/", ci, "ENGINE_TESTS_EXCLUDE"], ["apps/desktop/src-tauri/tests/", ci, "DESKTOP_TESTS_EXCLUDE"],
    ["crates/crazyagent/tests/", farm, "FARM_ENGINE_EXCLUDE"], ["apps/desktop/src-tauri/tests/", farm, "FARM_DESKTOP_EXCLUDE"],
  ]) {
    const skipped = excluded(text, list);
    for (const name of (await readdir(new URL(folder, root))).filter((name) => name.endsWith(".rs"))) {
      if (!(await readFile(new URL(folder + name, root), "utf8")).includes("CRAZYAGENT_RCLI")) continue;
      needing += 1;
      assert(skipped.has(name.slice(0, -3)), `${folder}${name} reads CRAZYAGENT_RCLI but is not in ${list}`);
    }
  }
  assert(needing > 1, "the check found the tests that need the RCLI host");
});

test("every engine and desktop row or target CI or the test farm leaves out, for all jobs or one, names a real test, so no exclusion goes stale", async () => {
  const ci = await workflow("ci.yml");
  const farm = await workflow("tests.yml");
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
    ["crates/crazyagent/", ["ENGINE_TESTS_EXCLUDE", ci], ["PLATFORM_TESTS_EXCLUDE", job("macos")], ["PLATFORM_TESTS_EXCLUDE", job("linux")], ["FARM_ENGINE_EXCLUDE", farm]],
    ["apps/desktop/src-tauri/", ["DESKTOP_TESTS_EXCLUDE", ci], ["FARM_DESKTOP_EXCLUDE", farm]],
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

test("every live Rust test target, named *_live, is left out of CI and the test farm, which have no live accounts, harnesses or local models", async () => {
  const folder = new URL("../../crates/crazyagent/tests/", import.meta.url);
  const live = (await readdir(folder)).filter((name) => name.endsWith("_live.rs")).map((name) => name.slice(0, -3));
  assert(live.includes("codex_bridge_live"), "the check found the live targets");
  for (const [file, list] of [["ci.yml", "ENGINE_TESTS_EXCLUDE"], ["tests.yml", "FARM_ENGINE_EXCLUDE"]]) {
    const skipped = excluded(await workflow(file), list);
    for (const name of live) {
      assert(skipped.has(name), `crates/crazyagent/tests/${name}.rs is a live target but is not in ${file} ${list}`);
    }
  }
});

/**
 * Lists the matrix entries of one job, read from its `include:` list in block form: an entry
 * starts with `- key: value` at 10-space indent and goes on with `key: value` at 12.
 * Inputs: the workflow text and the job name. Output: one object of strings per entry.
 */
function matrixEntries(workflowText, job) {
  const lines = block(block(workflowText, 0, "jobs").join("\n"), 2, job);
  const include = lines.findIndex((line) => line === "        include:");
  assert(include >= 0, `${job} has a matrix include`);
  const entries = [];
  for (const line of lines.slice(include + 1)) {
    const start = line.match(/^ {10}- ([a-z0-9_]+): (\S+)$/);
    const more = line.match(/^ {12}([a-z0-9_]+): (\S+)$/);
    if (start) entries.push({ [start[1]]: start[2] });
    else if (more && entries.length > 0) entries.at(-1)[more[1]] = more[2];
    else break;
  }
  assert(entries.length > 0, `${job} lists matrix entries`);
  return entries;
}

const FARM_OS = Object.freeze(["linux", "macos", "windows"]);

/**
 * Lists the matrix entries of one test farm job kind across its per-OS jobs, `<kind>-<os>`.
 * Inputs: the workflow text and the kind, build or rust. Output: the entries, Linux first.
 */
function farmEntries(workflowText, kind) {
  return FARM_OS.flatMap((os) => matrixEntries(workflowText, `${kind}-${os}`));
}

test("the test farm runs on a pushed integration batch or on demand, never for a pull request, one run per ref with a newer push cancelling the older, with a read-only token, and every job refuses a self-hosted runner first (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  assert.deepEqual(block(farm, 0, "on").filter(Boolean), ["on:", "  push:", "    branches:", '      - "chore/v242-batch*"', '      - "chore/v25*"', '      - "test-farm/**"', "  workflow_dispatch:"]);
  assert.doesNotMatch(farm, /pull_request|workflow_call/);
  assert.deepEqual(block(farm, 0, "concurrency").filter(Boolean), ["concurrency:", "  group: tests-${{ github.ref }}", "  cancel-in-progress: true"]);
  assert.deepEqual(block(farm, 0, "permissions").filter(Boolean), ["permissions:", "  contents: read"]);
  assert.doesNotMatch(farm, /^ {4}permissions:/m, "no job widens the token");
  const jobs = jobsOf(farm).map(({ name }) => name);
  assert.deepEqual(jobs, [...FARM_OS.map((os) => `build-${os}`), ...FARM_OS.map((os) => `rust-${os}`), "node", "typecheck", "report"]);
  for (const job of jobs) {
    const [first] = steps(farm, job);
    assert(first.startsWith("      - name: Refuse a runner that is not GitHub-hosted\n"), job);
    assert(first.includes('[ "$RUNNER_ENVIRONMENT" = github-hosted ] || { echo "::error::'), job);
    assert.match(first, /exit 1; \}$/, job);
  }
});

test("the test farm keeps one Rust cache per OS and lockfile, saved only from master, and no other cache, so it fits the free 10 GB beside ci.yml (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const texts = [farm, ...(await localActions()).map(({ text }) => text)];
  assert.deepEqual(texts.join("\n").split("\n").filter((line) => /uses: (Swatinem\/rust-cache|actions\/cache)@|^\s+cache(-dependency-path)?:/.test(line)).map((line) => line.trim()), ["- uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0"]);
  const archive = actionSteps(await farmAction("rust-archive"));
  const cache = archive.find((step) => step.includes("Swatinem/rust-cache@"));
  assert.deepEqual(cache.split("\n").slice(1).map((line) => line.trim()), ["with:", "prefix-key: farm", "shared-key: ${{ inputs.os }}", "add-job-id-key: false", "add-rust-environment-hash-key: false", "cache-bin: false", "save-if: ${{ github.ref == 'refs/heads/master' }}"]);
  for (const step of archive.filter((text) => text.includes("actions/upload-artifact@"))) assert(step.includes("        retention-days: 1\n"), "a test archive lives one day");
});

test("every test farm part is one matrix entry that uploads one JUnit report, even when it crashed, and a run holds at most 5 macOS jobs and 20 jobs at once (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const jobs = block(farm, 0, "jobs").join("\n");
  const builds = farmEntries(farm, "build");
  const rust = farmEntries(farm, "rust");
  const node = matrixEntries(farm, "node");
  for (const entry of [...builds, ...rust, ...node]) assert.equal(entry.runner, RUNNERS[entry.os], JSON.stringify(entry));
  assert.deepEqual(builds.map((entry) => entry.os), ["linux", "macos", "windows"]);
  assert.deepEqual([...new Set(rust.map((entry) => entry.os))], ["linux", "macos", "windows"]);
  for (const entries of [rust, node]) {
    for (const os of new Set(entries.map((entry) => entry.os))) {
      const mine = entries.filter((entry) => entry.os === os);
      assert.deepEqual(mine.map((entry) => entry.part), mine.map((_, index) => String(index + 1)), `${os} parts are 1..N`);
      assert(mine.every((entry) => entry.parts === String(mine.length)), `${os} parts name N = ${mine.length}`);
    }
  }
  const parts = [...rust.map((entry) => `${entry.os}-rust-${entry.part}`), ...node.map((entry) => `${entry.os}-node-${entry.part}`), "linux-bun-1"];
  assert.deepEqual([...excluded(farm, "FARM_PARTS")].sort(), parts.sort(), "the report expects exactly the parts the matrices run");
  const report = (all, label, name) => {
    const keep = all.findIndex((step) => /^ *- name: Keep a JUnit report for this part\n *if: always\(\)\n/.test(step));
    assert(keep >= 0 && all[keep].includes('if [ ! -s "$FARM_JUNIT" ]; then'), `${label} writes a failing report when none was written`);
    assert(all[keep + 1].includes("actions/upload-artifact@") && /\n *if: always\(\)\n/.test(all[keep + 1]), `${label} uploads its report even after a failure`);
    assert(all[keep + 1].includes(`name: farm-junit-${name}\n`), `${label} names its report after its part`);
    return all[keep + 1];
  };
  for (const [job, part] of [["node", "${{ matrix.os }}-node-${{ matrix.part }}"], ["typecheck", "linux-bun-1"]]) {
    assert(block(jobs, 2, job).includes(`      FARM_PART: ${part}`), job);
    report(steps(farm, job), job, "${{ env.FARM_PART }}");
  }
  const upload = report(actionSteps(await farmAction("rust-shard")), "rust-shard", "${{ inputs.os }}-rust-${{ inputs.part }}");
  assert(upload.includes("        path: target/nextest/farm/junit.xml\n"), "the shard uploads the JUnit file nextest's farm profile writes");
  for (const os of FARM_OS) {
    const job = block(jobs, 2, `rust-${os}`);
    assert(job.includes("      FARM_PART: ${{ matrix.os }}-rust-${{ matrix.part }}") && job.includes("      FARM_JUNIT: target/nextest/farm/junit.xml"), `rust-${os} names the part and file the shard reports`);
  }
  const count = (os) => [builds, rust, node].flat().filter((entry) => entry.os === os).length;
  assert(count("macos") <= 5, "at most 5 macOS jobs, even if a build overlapped its shards");
  assert(builds.length + rust.length + node.length + 2 <= 20, "at most 20 jobs at once, typecheck and report included");
});

test("the test farm report needs every other job and fails on any that did not succeed, so a red typecheck or build never reads green in the summary (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const others = jobsOf(farm).map(({ name }) => name).filter((name) => name !== "report");
  assert(block(block(farm, 0, "jobs").join("\n"), 2, "report").includes(`    needs: [${others.join(", ")}]`));
  const summary = steps(farm, "report").find((step) => step.includes("scripts/ci/junit-summary.mjs"));
  assert(summary.includes("        env:\n          FARM_NEEDS: ${{ toJSON(needs) }}\n"), "the summary reads every needed job's result");
});

test("each OS's Rust shards wait only on that OS's build, so a slow macOS build holds no Linux or Windows shard, and every OS runs one shared build action and one shared shard action, its own steps limited to what only it needs (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const jobs = block(farm, 0, "jobs").join("\n");
  const head = (step) => step.split("\n")[0].replace(/^ *- (name: |uses: )?/, "");
  const own = {
    build: { linux: ["Free disk on the Linux runner", "checkout", "Install Linux build packages"], macos: ["Free disk on the macOS runner", "checkout"], windows: ["checkout", "Install SQLite for Windows"] },
    rust: { linux: ["checkout", "Install Linux packages"], macos: ["checkout", "Install tmux on macOS", "Select Xcode 26.3, whose git the engine runs"], windows: ["checkout", "Install SQLite for Windows"] },
  };
  const uses = {
    build: "      - uses: ./.github/actions/rust-archive\n        with:\n          os: ${{ matrix.os }}\n          node-version: ${{ env.NODE_VERSION }}",
    rust: "      - uses: ./.github/actions/rust-shard\n        with:\n          os: ${{ matrix.os }}\n          part: ${{ matrix.part }}\n          parts: ${{ matrix.parts }}\n          node-version: ${{ env.NODE_VERSION }}",
  };
  for (const os of FARM_OS) {
    for (const kind of ["build", "rust"]) {
      assert.deepEqual([...new Set(matrixEntries(farm, `${kind}-${os}`).map((entry) => entry.os))], [os], `${kind}-${os} runs ${os} only`);
      const all = steps(farm, `${kind}-${os}`);
      assert.deepEqual(all.slice(1, -1).map(head).map((name) => (name.startsWith("actions/checkout@") ? "checkout" : name)), own[kind][os], `${kind}-${os} runs only its own OS steps before the shared action`);
      assert.equal(all.at(-1).replace(/\s+$/, ""), uses[kind], `${kind}-${os} ends with the shared action, fed only matrix and workflow values`);
    }
    assert(!block(jobs, 2, `build-${os}`).some((line) => line.startsWith("    needs:")), `build-${os} waits on nothing`);
    const rust = block(jobs, 2, `rust-${os}`);
    assert.deepEqual(rust.filter((line) => line.startsWith("    needs:")), [`    needs: build-${os}`], `rust-${os} waits on its own build only`);
    assert(rust.includes("    if: ${{ !cancelled() }}"), `rust-${os} reports even when its build failed`);
  }
  for (const { name, text } of await localActions()) {
    assert(text.includes("\nruns:\n  using: composite\n  steps:\n"), `${name} is a composite action`);
    for (const step of actionSteps(text)) {
      if (/\n {6}run: /.test(step)) assert(/\n {6}shell: bash\n/.test(step), `${name}: every run step names bash`);
      const used = step.match(/^ {4}- uses: (\S+)$/m)?.[1];
      if (used) assert.match(used, /^[a-z-]+\/[a-z-]+@[0-9a-f]{40}$/i, `${name}: every action it uses is pinned by commit`);
    }
    for (const [expression] of text.matchAll(/\$\{\{[^}]*\}\}/g)) {
      assert.match(expression, /^\$\{\{ (inputs\.(os|part|parts|node-version)|runner\.temp|github\.ref == 'refs\/heads\/master'|steps\.ide\.outputs\.(version|url|sha256|bytes)) \}\}$/, `${name} expands only its inputs and fixed runner values: ${expression}`);
    }
  }
});

test("every test farm Rust shard installs the build job's toolchain, Windows SQLite and debug settings and fetches the locked crates before it runs, so rows that spawn the cargo that built them build offline (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const build = actionSteps(await farmAction("rust-archive"));
  const rust = actionSteps(await farmAction("rust-shard"));
  const named = (all, name) => all.findIndex((step) => step.startsWith(`    - name: ${name}\n`));
  const shard = rust.findIndex((step) => step.includes("bash scripts/ci/farm-rust-shard.sh "));
  assert(shard >= 0, "the shard step exists");
  for (const name of ["Install Rust toolchain", "Set debug info for the platform's disk", "Fetch Rust dependencies"]) {
    const at = named(rust, name);
    assert(at >= 0 && at < shard, `${name} runs before the shard`);
    assert.equal(rust[at], build[named(build, name)], `${name} is the build action's own step`);
  }
  assert(rust[named(rust, "Fetch Rust dependencies")].endsWith("run: cargo fetch --locked"));
  assert(named(rust, "Install Rust toolchain") < named(rust, "Fetch Rust dependencies"), "crates are fetched with the pinned cargo");
  const sqlite = (job) => steps(farm, job).find((step) => step.startsWith("      - name: Install SQLite for Windows\n"));
  assert(sqlite("rust-windows") && sqlite("rust-windows") === sqlite("build-windows"), "Windows shards install the build's SQLite before the shard action");
});

test("the test farm installs tmux only from pinned, checked sources: the Linux tarball by sha256, the macOS build by the release's pinned script, never from Homebrew (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const shard = await farmAction("rust-shard");
  assert.doesNotMatch(farm + shard, /brew install/);
  const linux = actionSteps(shard).find((step) => step.startsWith("    - name: Install tmux on Linux\n"));
  assert(linux.includes("      if: inputs.os == 'linux'\n") && linux.includes('echo "$TMUX_SHA256  $work/tmux.tar.gz" | sha256sum -c -'));
  const macos = steps(farm, "rust-macos").find((step) => step.startsWith("      - name: Install tmux on macOS\n"));
  assert(macos.includes("scripts/release/build-tmux.sh --target aarch64-apple-darwin "));
  assert(macos.includes('test "$(tmux -V)" = "tmux $TMUX_VERSION"'));
  const pinned = (await readFile(new URL("./build-tmux.sh", import.meta.url), "utf8")).match(/^TMUX_VERSION=(\S+)$/m)[1];
  assert(farm.includes(`  TMUX_VERSION: "${pinned}"\n`), "the Linux build and the release script pin one tmux version");
});

test("the macOS Rust shards select Xcode 26.3 after tmux is built and check that the git the engine finds, through xcrun with a cleared environment, meets the landing floor in landing/git.rs, since the runner's default Xcode 16.4 git is older (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const all = steps(farm, "rust-macos");
  const at = all.findIndex((step) => step.startsWith("      - name: Select Xcode 26.3, whose git the engine runs\n"));
  assert(at > all.findIndex((step) => step.startsWith("      - name: Install tmux on macOS\n")) && at === all.length - 2, "after tmux, right before the shard action");
  const select = all[at];
  assert(select.includes("          sudo xcode-select -s /Applications/Xcode_26.3.app\n"));
  assert(select.includes('          git="$(env -i PATH=/usr/bin:/bin /usr/bin/xcrun --find git)"\n'), "found the way setup_git finds it");
  const source = await readFile(new URL("../../crates/crazyagent/src/landing/git.rs", import.meta.url), "utf8");
  const [, major, minor] = source.match(/^const VERSION_MIN: \(u32, u32\) = \((\d+), (\d+)\);$/m);
  assert(select.includes(`          (( major > ${major} || (major == ${major} && minor >= ${minor}) )) || {`), "the check uses the engine's own floor");
  for (const os of ["linux", "windows"]) assert(!steps(farm, `rust-${os}`).some((step) => step.includes("xcode-select")), `${os} selects no Xcode`);
});

test("the test farm macOS build gets the hosted job's 330-minute budget and two cargo jobs, logging memory and swap, and Windows leaves only the desktop crate out of its archive, whose test exe cannot load without the app manifest (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  for (const os of FARM_OS) assert(block(block(farm, 0, "jobs").join("\n"), 2, `build-${os}`).includes(`    timeout-minutes: ${os === "macos" ? 330 : 150}`), os);
  const action = await farmAction("rust-archive");
  const archive = actionSteps(action).find((step) => step.startsWith("    - name: Build and archive every workspace test\n"));
  assert(archive.includes('if [ "$RUNNER_OS" = Windows ]; then exclude=(--exclude crazyagent-desktop); fi'));
  assert(archive.includes("export CARGO_BUILD_JOBS=2"));
  assert(archive.includes("sysctl -n vm.swapusage"));
  assert(archive.includes('cargo nextest archive --workspace ${exclude[@]+"${exclude[@]}"} --features crazyagent/owner-trace --locked --offline'));
  assert.equal((farm + action).match(/--exclude /g).length, 1, "no other crate is left out of any archive");
  const size = actionSteps(action).find((step) => step.startsWith("    - name: Show the free disk, the target and the archive size\n"));
  assert(size.includes("hw.memsize") && size.includes("free -g"), "every build prints its CPUs and memory");
});

test("every test farm node part installs what its rows run, tmux, the locked crates and the PDF Chrome, from the same pinned steps as the Rust jobs, and stops before the job limit so its JUnit is kept (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const build = actionSteps(await farmAction("rust-archive"));
  const shard = actionSteps(await farmAction("rust-shard"));
  const node = steps(farm, "node");
  const named = (all, name) => all.findIndex((step) => /^ *- name: (.+)\n/.exec(step)?.[1] === name);
  const run = named(node, "Run this share of the node suites");
  assert(run >= 0, "the node run step exists");
  const sources = [[build, "Install Rust toolchain"], [build, "Fetch Rust dependencies"], [steps(farm, "rust-linux"), "Install Linux packages"], [shard, "Install tmux on Linux"], [steps(farm, "rust-macos"), "Install tmux on macOS"]];
  for (const [source, name] of sources) {
    const at = named(node, name);
    assert(at >= 0 && at < run, `${name} runs before the node suites`);
    assert.equal(stepBody(node[at]), stepBody(source[named(source, name)]), `${name} is the same step as in the Rust jobs`);
  }
  const shell = node[named(node, "Install the pinned Chrome headless shell for the PDF rows")];
  assert(shell.includes('echo "$HEADLESS_SHA256  $work/headless.zip" | shasum -a 256 -c -'));
  assert(shell.includes('echo "CRAZYAGENT_PDF_CHROME=$shell" >> "$GITHUB_ENV"'));
  for (const entry of matrixEntries(farm, "node")) assert.match(entry.headless_sha256, /^[0-9a-f]{64}$/, JSON.stringify(entry));
  const job = Number(block(block(farm, 0, "jobs").join("\n"), 2, "node").find((line) => line.startsWith("    timeout-minutes: ")).split(": ")[1]);
  const step = Number(node[run].match(/^ {8}timeout-minutes: (\d+)$/m)[1]);
  assert(step + 10 <= job, `the node run stops at ${step} min, leaving the JUnit steps time before the ${job}-min job limit`);
});

test("the hosted macOS node part scales the Makefile's 15 s per-test limit by one workflow value passed after it, and every other part keeps 15 s (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const value = block(farm, 0, "env").find((line) => line.startsWith("  NODE_TEST_TIMEOUT_MACOS_MS: "));
  assert(value, "the workflow env names the macOS node test limit");
  const ms = Number(JSON.parse(value.split(": ")[1]));
  assert(Number.isInteger(ms) && ms > 15000 && ms <= 120000, `${ms} ms scales the 15 s limit and stays bounded`);
  const run = steps(farm, "node").find((step) => step.startsWith("      - name: Run this share of the node suites\n"));
  assert(run.includes('if [ "$RUNNER_OS" = macOS ]; then limit="--test-timeout=$NODE_TEST_TIMEOUT_MACOS_MS"; fi'));
  assert.match(run, /make test-node BUN="\$BUN" NODE_TEST_ARGS="[^"\n]*\$limit[^"\n]*"$/m, "the macOS limit rides in NODE_TEST_ARGS, which the Makefile passes after its own, so it wins");
  assert.equal(farm.match(/NODE_TEST_TIMEOUT_MACOS_MS/g).length, 2, "one value, read only by the node run step");
  assert.equal(farm.match(/--test-timeout/g).length, 1, "no other part or row sets its own limit");
  const makefile = await readFile(new URL("../../Makefile", import.meta.url), "utf8");
  assert.match(makefile, /^\t.*node --test --test-timeout=15000 \$\(NODE_TEST_ARGS\) \$\(NODE_TESTS\)$/m, "the Makefile's limit comes before NODE_TEST_ARGS");
});

test("CI runs the price snapshot script's check on Linux, so a bad refresh fails a pull request", async () => {
  const linux = steps(await workflow("ci.yml"), "linux");
  assert(
    linux.some((step) => /^ {8}run: make test-price-script$/m.test(step)),
    "the linux job runs make test-price-script",
  );
});

const SCRIPT_MS = 10_000;
const STEP_MS = 30_000;
const FLOW = Object.freeze({ timeout: 120_000 });
const HDIUTIL_RETRY = fileURLToPath(new URL("./hdiutil-retry/", import.meta.url));
const FAKE_GH = fileURLToPath(new URL("./fake-gh.mjs", import.meta.url));
const DEV_BUILDS = fileURLToPath(new URL("./dev-builds.mjs", import.meta.url));
const REPO_ROOT = fileURLToPath(new URL("../../", import.meta.url));
const FAKE_SLEEP = String.raw`printf '%s\n' "$1" >> "$FAKE_STATE/sleeps"
`;
const FAKE_CURL = String.raw`printf 'curl %s\n' "$*" >> "$FAKE_STATE/calls"
fails="$(cat "$FAKE_STATE/curl-fails")"
if [ "$fails" -gt 0 ]; then
  echo "$((fails - 1))" > "$FAKE_STATE/curl-fails"
  echo "curl: (22) The requested URL returned error: 404" >&2
  exit 22
fi
exit 0
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
const COMMIT = "0123456789abcdef0123456789abcdef01234567";
const VERSION = "0.0.0-dev.202611131207";
const BUILD = `build-${VERSION}`;
const BUILD_TAG = /^build-(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-dev\.\d{12}$/;
const DOWNLOADS = "https://github.com/example/fake-repo/releases/download";

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

/**
 * Makes, once per test process, the stand-ins every release step run puts first on PATH: gh (the
 * fake, run by node with no shell in between), node, curl and sleep. Fresh executables wait on
 * macOS's first-run check, so the runs share one folder. Output: the folder, removed at exit.
 */
function fakeBin() {
  const root = mkdtempSync(join(tmpdir(), "ca-relwf-bin-"));
  process.on("exit", () => rmSync(root, { recursive: true, force: true }));
  standIns(root, { curl: FAKE_CURL, sleep: FAKE_SLEEP });
  writeFileSync(join(root, "gh"), `#!${process.execPath}\nimport(${JSON.stringify(pathToFileURL(FAKE_GH).href)});\n`);
  chmodSync(join(root, "gh"), 0o755);
  symlinkSync(process.execPath, join(root, "node"));
  return root;
}

let sharedBin = null;

/** The shared stand-in folder, made on first use. Output: its path. */
function stepBin() {
  sharedBin ??= fakeBin();
  return sharedBin;
}

/** The lines a stand-in appended to one state file. Inputs: the state folder and the file name. Output: the lines, or none. */
function logged(state, name) {
  const file = join(state, name);
  return existsSync(file) ? readFileSync(file, "utf8").split("\n").filter(Boolean) : [];
}

/** The `name=value` lines a step wrote to GITHUB_OUTPUT. Input: the file. Output: name -> value. */
function stepOutputs(file) {
  return Object.fromEntries(readFileSync(file, "utf8").split("\n").filter(Boolean).map((line) => [line.slice(0, line.indexOf("=")), line.slice(line.indexOf("=") + 1)]));
}

/**
 * The one step of `job` in release.yml named `name`. Inputs: the job and the step name.
 * Output: the step text; it fails the test unless exactly one step has that name.
 */
async function releaseStep(job, name) {
  const found = steps(await workflow("release.yml"), job).filter((step) => step.split("\n").some((line) => line.trim() === `- name: ${name}` || line.trim() === `name: ${name}`));
  assert.equal(found.length, 1, `one ${job} step named ${name}`);
  return found[0];
}

/** The sha256 of a text as hex. Input: the text. Output: the hex digest. */
function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

/**
 * The files the build jobs leave in dist for VERSION, with manifests whose URLs start at `base`.
 * Inputs: the base URL and the manifests' pub_date. Output: name -> text.
 */
function distFiles(base = `${DOWNLOADS}/${BUILD}`, pubDate = "2026-11-13T12:07:00Z") {
  const files = { "app.tar.gz": "desktop bytes", "app.tar.gz.sig": "desktop signature", "cli.tar.gz": "cli bytes", "cli.tar.gz.sig": "cli signature" };
  files["latest.json"] = JSON.stringify({ version: VERSION, notes: "Dev build", pub_date: pubDate, platforms: { "darwin-aarch64": { signature: files["app.tar.gz.sig"], url: `${base}/app.tar.gz` } } });
  files["cli-latest.json"] = JSON.stringify({ version: VERSION, channel: "dev", commit: COMMIT, pub_date: pubDate, platforms: { "aarch64-apple-darwin": { url: `${base}/cli.tar.gz`, sha256: sha256(files["cli.tar.gz"]), signature: files["cli.tar.gz.sig"] } } });
  files.SHA256SUMS = Object.keys(files).sort().map((name) => `${sha256(files[name])}  ${name}\n`).join("");
  return files;
}

/** The three files the dev release copies from a build. Input: the build's files. Output: name -> text. */
function pointerFiles(files) {
  return { "latest.json": files["latest.json"], "cli-latest.json": files["cli-latest.json"], SHA256SUMS: files.SHA256SUMS };
}

/**
 * A release the stand-in gh already holds, published as a prerelease of COMMIT unless `extra`
 * says otherwise. Inputs: its id, tag, files (name -> text) and overrides. Output: { release, files }.
 */
function stored(id, tagName, files, extra = {}) {
  const assets = Object.entries(files).map(([name, text]) => ({ name, size: Buffer.byteLength(text), state: "uploaded", digest: `sha256:${sha256(text)}` }));
  return { release: { id, tag_name: tagName, name: tagName, draft: false, prerelease: true, target_commitish: COMMIT, assets, ...extra }, files };
}

/**
 * Runs release.yml step bodies in order, as one job runs its steps, against stand-in gh, curl and
 * sleep that hold no token and reach no network, and stops at the first step that fails.
 * Inputs: the bodies; the releases and tag names gh holds; dist; the stand-in config; stale dev
 * tag reads; failing curl tries; the prune selection file. Output: the last status, all output,
 * the calls, sleeps, step outputs, the releases left, and the files and notes dev got.
 */
function runSteps(bodies, { builds = [], refs = [], dist = distFiles(), config = {}, stale = 0, curlFails = 0, selected = null } = {}) {
  const root = mkdtempSync(join(tmpdir(), "ca-relwf-"));
  try {
    const state = join(root, "state");
    const work = join(root, "work");
    const temp = join(root, "temp");
    for (const folder of [state, join(work, "dist"), join(work, "scripts", "release"), temp, join(root, "home")]) mkdirSync(folder, { recursive: true });
    copyFileSync(DEV_BUILDS, join(work, "scripts", "release", "dev-builds.mjs"));
    for (const [name, text] of Object.entries(dist)) writeFileSync(join(work, "dist", name), text);
    writeFileSync(join(work, "notes.txt"), "Dev build\n");
    writeFileSync(join(state, "releases.json"), JSON.stringify(builds.map((build) => build.release)));
    for (const { release, files } of builds) {
      mkdirSync(join(state, "assets", String(release.id)), { recursive: true });
      for (const [name, text] of Object.entries(files)) writeFileSync(join(state, "assets", String(release.id), name), text);
    }
    writeFileSync(join(state, "refs.json"), JSON.stringify(refs.map((name) => ({ ref: `refs/tags/${name}` }))));
    writeFileSync(join(state, "config.json"), JSON.stringify(config));
    writeFileSync(join(state, "stale"), `${stale}\n`);
    writeFileSync(join(state, "curl-fails"), `${curlFails}\n`);
    if (selected !== null) writeFileSync(join(temp, "selected.txt"), selected);
    const output = join(root, "output");
    writeFileSync(output, "");
    let status = 0;
    let stdout = "";
    let stderr = "";
    for (const body of bodies) {
      writeFileSync(join(root, "step.sh"), body);
      const run = spawnSync("/bin/bash", ["--noprofile", "--norc", "-eo", "pipefail", join(root, "step.sh")], {
        cwd: work,
        encoding: "utf8",
        timeout: STEP_MS,
        env: {
          PATH: `${stepBin()}:/usr/bin:/bin`, HOME: join(root, "home"), FAKE_STATE: state, GITHUB_REPOSITORY: "example/fake-repo", GITHUB_SHA: COMMIT,
          GITHUB_OUTPUT: output, RUNNER_TEMP: temp, VERSION, ASSET_TAG: BUILD, DEV_BUILDS_KEPT: "15",
        },
      });
      assert.equal(run.error, undefined, `the step ends within ${STEP_MS} ms`);
      stdout += run.stdout;
      stderr += run.stderr;
      status = run.status;
      if (status !== 0) break;
    }
    const devFolder = join(state, "dev");
    const dev = existsSync(devFolder) ? Object.fromEntries(readdirSync(devFolder).map((name) => [name, readFileSync(join(devFolder, name), "utf8")])) : {};
    const devNotes = existsSync(join(state, "dev-notes.txt")) ? readFileSync(join(state, "dev-notes.txt"), "utf8") : null;
    return {
      status, stdout, stderr, calls: logged(state, "calls"), sleeps: logged(state, "sleeps").map(Number), outputs: stepOutputs(output),
      releases: JSON.parse(readFileSync(join(state, "releases.json"), "utf8")), dev, devNotes,
    };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

/**
 * Runs prepare's version step for a push to master with the real version script and the shared
 * stand-ins on PATH. Input: the step's shell. Output: the exit status, output and step outputs.
 */
function prepareDev(body) {
  const root = mkdtempSync(join(tmpdir(), "ca-relwf-"));
  try {
    writeFileSync(join(root, "step.sh"), body);
    writeFileSync(join(root, "output"), "");
    const run = spawnSync("/bin/bash", ["--noprofile", "--norc", "-eo", "pipefail", join(root, "step.sh")], {
      cwd: REPO_ROOT,
      encoding: "utf8",
      timeout: STEP_MS,
      env: { PATH: `${stepBin()}:/usr/bin:/bin`, HOME: root, REF_TYPE: "branch", REF_NAME: "master", GITHUB_SHA: COMMIT, GITHUB_OUTPUT: join(root, "output") },
    });
    assert.equal(run.error, undefined, `the step ends within ${STEP_MS} ms`);
    return { status: run.status, stdout: run.stdout, stderr: run.stderr, outputs: stepOutputs(join(root, "output")) };
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

/** The two dev steps of the publish job, in order. Output: the run bodies of "Publish dev build" and "Point dev at the new build". */
async function devBodies() {
  return [runBody(await releaseStep("publish", "Publish dev build")), runBody(await releaseStep("publish", "Point dev at the new build"))];
}

/** The dev pointer step's shell from release.yml. Output: the run body of "Point dev at the new build". */
async function pointerBody() {
  return runBody(await releaseStep("publish", "Point dev at the new build"));
}

/** The prune job's three steps, in order. Output: the run bodies of its read, select and delete steps. */
async function pruneBodies() {
  const names = ["Read dev builds", "Select old dev builds", "Delete old dev builds"];
  return Promise.all(names.map(async (name) => runBody(await releaseStep("prune-dev-builds", name))));
}

/**
 * Runs the pointer step alone with BUILD published and newest: `stale` tag reads still show the
 * dev tag after the release delete, the ref DELETE always answers 422 "Reference does not exist",
 * `readFails` makes every dev tag read error, and `answer`, when given, is every tag read's output.
 * Inputs: the step's shell and the stand-in state. Output: a `runSteps` result.
 */
function pointDev(body, { stale, readFails = false, answer = null }) {
  return runSteps([body], { builds: [stored(1, BUILD, distFiles())], stale, config: answer === null ? { readFails } : { readFails, answer } });
}

/** How many dev tag reads a run made. Input: a `runSteps` result. Output: the count. */
function reads(run) {
  return run.calls.filter((call) => call.startsWith("api repos/example/fake-repo/git/matching-refs/tags/dev ")).length;
}

/** How many times a run created the dev release. Input: a `runSteps` result. Output: the count. */
function created(run) {
  return run.calls.filter((call) => call.startsWith("release create dev ")).length;
}

/** The calls that read or change the dev release or its tag. Input: a `runSteps` result. Output: the calls. */
function devCalls(run) {
  return run.calls.filter((call) => /^release (view|delete|create) dev /.test(call) || call.includes("/tags/dev"));
}

/** The calls that delete a release by its id. Input: a `runSteps` result. Output: the calls. */
function releaseDeletes(run) {
  return run.calls.filter((call) => call.startsWith("api -X DELETE repos/example/fake-repo/releases/"));
}

/** The calls of a run that start with `prefix`. Inputs: a `runSteps` result and the prefix. Output: the calls. */
function callsOf(run, prefix) {
  return run.calls.filter((call) => call.startsWith(prefix));
}

test("the dev publish tolerates a tag delete that finds the tag already gone and waits, bounded, until the tag reads gone before it creates the dev release", FLOW, async () => {
  const jq = spawnSync("/bin/bash", ["-c", "command -v jq"], { encoding: "utf8", env: { PATH: "/usr/bin:/bin" } });
  assert.equal(jq.status, 0, "the stand-in gh runs the step's --jq filter with jq, which must be on /usr/bin:/bin");
  const body = await pointerBody();

  const stale = pointDev(body, { stale: 2 });
  assert.equal(stale.status, 0, `${stale.stdout}${stale.stderr}`);
  assert.equal(reads(stale), 3, "two reads still show the tag, the third reads it gone");
  assert.equal(created(stale), 1);
  assert.match(stale.calls.at(-1), /^release create dev /, "the release is created only after the tag reads gone");
  assert.deepEqual(stale.sleeps, [2, 4]);

  const never = pointDev(body, { stale: 99 });
  assert.notEqual(never.status, 0, "a tag that never reads gone fails the publish");
  assert.match(never.stdout, /::error::the dev tag still reads present after 5 checks/);
  assert.equal(reads(never), 5);
  assert.equal(created(never), 0, "nothing is published while the tag reads present");
  assert(never.sleeps.reduce((sum, seconds) => sum + seconds, 0) <= 30, "the wait is bounded");

  const broken = pointDev(body, { stale: 0, readFails: true });
  assert.notEqual(broken.status, 0, "a tag read that errors fails the publish and never counts as gone");
  assert.equal(created(broken), 0);
});

test("the dev publish fails at once with an error, deleting and publishing nothing, when a dev tag read gives anything but 0 or 1", FLOW, async () => {
  const body = await pointerBody();
  for (const answer of ["2", "not-a-count"]) {
    const run = pointDev(body, { stale: 0, answer });
    assert.notEqual(run.status, 0, `a read of '${answer}' fails the publish`);
    assert(run.stdout.split("\n").includes(`::error::reading the dev tag gave '${answer}', not 0 or 1`), `${run.stdout}${run.stderr}`);
    assert.equal(reads(run), 1, "the first bad read stops the step");
    assert(!run.calls.some((call) => call.startsWith("api -X DELETE")), "no tag is deleted on a bad read");
    assert.equal(created(run), 0, "nothing is published on a bad read");
    assert.deepEqual(run.sleeps, []);
  }
});

test("a dev build's own tag is build-<version>, which no v* trigger, stable version check or dev tag read can take for theirs", FLOW, async () => {
  const release = await workflow("release.yml");
  assert.deepEqual(block(release, 0, "on").filter((line) => line.trim().startsWith("tags:")), ['    tags: ["v*"]']);
  assert(block(release, 2, "prepare").includes("      asset_tag: ${{ steps.version.outputs.asset_tag }}"), "prepare hands the build tag on");
  const version = steps(release, "prepare").find((step) => step.includes("- id: version"));
  const tagBranch = version.slice(version.indexOf('if [ "$REF_TYPE" = tag ]; then'), version.indexOf("          else"));
  assert.match(tagBranch, /\n {12}asset_tag="\$REF_NAME"\n/, "a stable release keeps its files under its own v* tag");
  assert.match(version, /\n {12}\[\[ "\$asset_tag" =~ \^build-[^\n]+\$ \]\] \|\| \{ echo "::error::[^"\n]+"; exit 1; \}\n/, "the dev build tag is checked against its pattern");
  const run = prepareDev(runBody(version));
  assert.equal(run.status, 0, `${run.stdout}${run.stderr}`);
  assert.match(run.outputs.version, /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-dev\.\d{12}$/);
  assert.equal(run.outputs.channel, "dev");
  assert.equal(run.outputs.tag, "dev");
  assert.equal(run.outputs.asset_tag, `build-${run.outputs.version}`);
  assert.match(run.outputs.asset_tag, BUILD_TAG);
  assert(!run.outputs.asset_tag.startsWith("v") && !run.outputs.asset_tag.startsWith("dev"), run.outputs.asset_tag);
});

test("the update manifests name each build's own release: build-<version> on dev, the v* tag on stable", async () => {
  const manifest = await releaseStep("publish", "Write update manifests and checksums");
  assert(manifest.includes('--base-url "https://github.com/$GITHUB_REPOSITORY/releases/download/$ASSET_TAG"'), manifest);
  assert(block(await workflow("release.yml"), 2, "publish").includes("      ASSET_TAG: ${{ needs.prepare.outputs.asset_tag }}"));
});

test("a dev build is published as its own prerelease, never Latest, and only then does dev move to a copy of its three manifest files", FLOW, async () => {
  const run = runSteps(await devBodies(), { builds: [stored(1, "build-0.0.0-dev.202611131100", pointerFiles(distFiles(`${DOWNLOADS}/build-0.0.0-dev.202611131100`)))] });
  assert.equal(run.status, 0, `${run.stdout}${run.stderr}`);
  const made = run.releases.find((item) => item.tag_name === BUILD);
  assert(made, "the build has its own release");
  assert.deepEqual([made.draft, made.prerelease, made.make_latest, made.target_commitish], [false, true, "false", COMMIT]);
  assert.deepEqual(made.assets.map((asset) => asset.name).sort(), Object.keys(distFiles()).sort());
  const order = [
    `api -X POST repos/example/fake-repo/releases -f tag_name=${BUILD} -f target_commitish=${COMMIT} -f name=Dev build ${VERSION} -F body=@notes.txt -F draft=true -F prerelease=true -f make_latest=false --jq .id`,
    `release upload ${BUILD} --repo example/fake-repo --clobber ${Object.keys(distFiles()).sort().map((name) => `dist/${name}`).join(" ")}`,
    `api -X PATCH repos/example/fake-repo/releases/${made.id} -F draft=false -F prerelease=true -f make_latest=false --jq .draft`,
    `curl -fsSL -r 0-0 -o /dev/null ${DOWNLOADS}/${BUILD}/latest.json`,
    `release download ${BUILD} --repo example/fake-repo --dir `,
    "release create dev ",
  ].map((prefix) => run.calls.findIndex((call) => call.startsWith(prefix)));
  assert(order.every((index, at) => index >= 0 && (at === 0 || index > order[at - 1])), `draft, upload, publish, wait, then dev: ${run.calls.join("\n")}`);
  assert.match(callsOf(run, "release create dev ")[0], new RegExp(`^release create dev --repo example/fake-repo --prerelease --latest=false --target ${COMMIT} --title Dev build ${VERSION} --notes-file \\S+/pointer-notes\\.txt \\S+/pointer/latest\\.json \\S+/pointer/cli-latest\\.json \\S+/pointer/SHA256SUMS$`));
  assert.deepEqual(run.dev, pointerFiles(distFiles()), "dev holds exactly the build's three manifest files");
  assert(run.devNotes.includes(`https://github.com/example/fake-repo/releases/tag/${BUILD}\n`), run.devNotes);
  assert.equal(run.outputs.pointer, "moved");
  assert.deepEqual(releaseDeletes(run), []);
});

test("a failed upload deletes, by its id, the draft this step made, and never touches dev", FLOW, async () => {
  const run = runSteps(await devBodies(), { config: { uploadFails: true } });
  assert.notEqual(run.status, 0);
  assert(run.calls.includes("api repos/example/fake-repo/releases/1001"), "the draft is read again before it is deleted");
  assert.deepEqual(releaseDeletes(run), ["api -X DELETE repos/example/fake-repo/releases/1001"]);
  assert.deepEqual(run.releases, []);
  assert.deepEqual(callsOf(run, "api -X PATCH"), []);
  assert.deepEqual(devCalls(run), []);
});

test("the draft clean-up deletes nothing when its re-read fails or shows a published or renamed release", FLOW, async () => {
  const bodies = await devBodies();
  for (const reread of ["fails", "published", "retagged"]) {
    const run = runSteps(bodies, { config: { uploadFails: true, reread } });
    assert.notEqual(run.status, 0, reread);
    assert.deepEqual(releaseDeletes(run), [], reread);
    assert.equal(run.releases.length, 1, `${reread}: the draft is left`);
    assert.match(run.stdout, /::warning::left the draft /, reread);
    assert.deepEqual(devCalls(run), [], reread);
  }
});

test("a dev build that fails after it is published, in the wait or the check, is never deleted, and dev keeps naming the build before it", FLOW, async () => {
  const bodies = await devBodies();
  const waited = runSteps(bodies, { curlFails: 99 });
  assert.notEqual(waited.status, 0);
  assert(waited.stdout.includes(`::error::${DOWNLOADS}/${BUILD}/latest.json did not answer after 5 tries`), waited.stdout);
  assert.deepEqual(waited.sleeps, [2, 4, 6, 8]);
  const checked = runSteps(bodies, { config: { digestless: true } });
  assert.notEqual(checked.status, 0);
  assert.match(checked.stderr, /dev_build_digest_missing:/);
  for (const run of [waited, checked]) {
    assert.equal(callsOf(run, "api -X PATCH").length, 1, "the build was published");
    assert.deepEqual(releaseDeletes(run), []);
    assert.equal(run.releases.find((item) => item.tag_name === BUILD)?.draft, false);
    assert.deepEqual(devCalls(run), []);
  }
});

test("a re-run reuses a published build of this commit with the same file digests, and dev gets the build's own manifests, not this attempt's", FLOW, async () => {
  const first = distFiles(`${DOWNLOADS}/${BUILD}`, "2026-11-13T12:07:00Z");
  const again = distFiles(`${DOWNLOADS}/${BUILD}`, "2026-11-13T12:31:00Z");
  const run = runSteps(await devBodies(), { builds: [stored(7, BUILD, first)], dist: again });
  assert.equal(run.status, 0, `${run.stdout}${run.stderr}`);
  for (const prefix of ["api -X POST", "release upload", "api -X PATCH"]) assert.deepEqual(callsOf(run, prefix), [], prefix);
  assert.deepEqual(run.dev, pointerFiles(first));
  assert.notEqual(run.dev["latest.json"], again["latest.json"]);
  assert.equal(run.outputs.pointer, "moved");
  assert.deepEqual(releaseDeletes(run), []);
});

test("a re-run refuses a published build of another commit or with other file bytes, uploading, deleting and moving nothing", FLOW, async () => {
  const bodies = await devBodies();
  const files = distFiles();
  for (const [label, build, error] of [
    ["other bytes of the same size", stored(7, BUILD, { ...files, "app.tar.gz": "desktop bytez" }), /dev_build_asset_rejected:app\.tar\.gz/],
    ["another commit", stored(7, BUILD, files, { target_commitish: "fedcba9876543210fedcba9876543210fedcba98" }), /dev_build_commit_rejected/],
  ]) {
    const run = runSteps(bodies, { builds: [build] });
    assert.notEqual(run.status, 0, label);
    assert.match(run.stderr, error, label);
    for (const prefix of ["api -X POST", "release upload", "api -X PATCH"]) assert.deepEqual(callsOf(run, prefix), [], `${label}: ${prefix}`);
    assert.deepEqual(releaseDeletes(run), [], label);
    assert.deepEqual(devCalls(run), [], label);
  }
});

test("a re-run that reuses a published build and then fails deletes nothing", FLOW, async () => {
  const run = runSteps(await devBodies(), { builds: [stored(7, BUILD, distFiles())], curlFails: 99 });
  assert.notEqual(run.status, 0);
  assert.deepEqual(releaseDeletes(run), []);
  assert.equal(run.releases.length, 1);
  assert.deepEqual(devCalls(run), []);
});

test("a re-run finishes this run's leftover draft found by its tag, and never deletes a draft this step did not make", FLOW, async () => {
  const bodies = await devBodies();
  const left = stored(7, BUILD, {}, { draft: true });
  const done = runSteps(bodies, { builds: [left] });
  assert.equal(done.status, 0, `${done.stdout}${done.stderr}`);
  assert.deepEqual(callsOf(done, "api -X POST"), []);
  assert.equal(callsOf(done, `release upload ${BUILD} --repo example/fake-repo --clobber `).length, 1);
  assert.equal(callsOf(done, "api -X PATCH repos/example/fake-repo/releases/7 ").length, 1);
  assert.equal(done.outputs.pointer, "moved");
  const failed = runSteps(bodies, { builds: [left], config: { uploadFails: true } });
  assert.notEqual(failed.status, 0);
  assert.deepEqual(releaseDeletes(failed), []);
  assert.equal(failed.releases.length, 1);
});

test("a re-run of an older run publishes its build but never moves dev back to it, and leaves pruning to a newer run", FLOW, async () => {
  const newer = "build-0.0.0-dev.202611131300";
  const run = runSteps(await devBodies(), { builds: [stored(9, newer, pointerFiles(distFiles(`${DOWNLOADS}/${newer}`)))] });
  assert.equal(run.status, 0, `${run.stdout}${run.stderr}`);
  assert.equal(run.releases.find((item) => item.tag_name === BUILD)?.draft, false);
  assert.deepEqual(devCalls(run), []);
  assert.equal(run.outputs.pointer, "kept");
  assert(run.stdout.includes(`::notice::${BUILD} is published, but ${newer} is newer`), run.stdout);
  const release = await workflow("release.yml");
  assert(block(release, 2, "publish").includes("      pointer: ${{ steps.pointer.outputs.pointer }}"));
  assert(block(release, 2, "prune-dev-builds").includes("    if: needs.prepare.outputs.channel == 'dev' && needs.publish.outputs.pointer == 'moved'"));
});

test("dev never moves to manifests whose URLs name the dev release instead of the build's own", FLOW, async () => {
  const run = runSteps(await devBodies(), { dist: distFiles(`${DOWNLOADS}/dev`) });
  assert.notEqual(run.status, 0);
  assert.match(run.stderr, /dev_build_pointer_url_rejected:https:\/\/github\.com\/example\/fake-repo\/releases\/download\/dev\//);
  assert.deepEqual(devCalls(run), []);
});

/**
 * Releases and tags for the prune rows: dev, a stable release, 19 old builds, the newest build
 * BUILD and one old orphan tag. Output: { builds, refs, old, orphan }.
 */
function pruneState() {
  const old = Array.from({ length: 19 }, (_, index) => stored(10 + index, `build-0.0.0-dev.${202611120000 + index}`, {}));
  const builds = [stored(2, "dev", {}), stored(3, "v2.3.2", {}, { prerelease: false }), ...old, stored(5, BUILD, {})];
  const orphan = "build-0.0.0-dev.202611110000";
  return { builds, refs: [...builds.map((build) => build.release.tag_name), orphan], old, orphan };
}

/** The calls of a prune run that delete a release or a tag. Input: a `runSteps` result. Output: the calls. */
function pruneDeletes(run) {
  return run.calls.filter((call) => call.startsWith("release delete ") || call.startsWith("api -X DELETE "));
}

test("prune reads every page, then deletes the oldest builds and orphan tags past the newest 15, never dev or a v* release", FLOW, async () => {
  const { builds, refs, old, orphan } = pruneState();
  const run = runSteps(await pruneBodies(), { builds, refs, config: { pageSize: 7 } });
  assert.equal(run.status, 0, `${run.stdout}${run.stderr}`);
  assert.deepEqual(pruneDeletes(run), [
    `api -X DELETE repos/example/fake-repo/git/refs/tags/${orphan}`,
    ...old.slice(0, 5).map((build) => `release delete ${build.release.tag_name} --repo example/fake-repo --yes --cleanup-tag`),
  ]);
  assert.deepEqual(run.releases.map((item) => item.tag_name), ["dev", "v2.3.2", ...old.slice(5).map((build) => build.release.tag_name), BUILD]);
});

test("prune with no more builds than it keeps passes and deletes nothing", FLOW, async () => {
  const builds = [stored(2, "dev", {}), ...Array.from({ length: 8 }, (_, index) => stored(10 + index, `build-0.0.0-dev.${202611120000 + index}`, {})), stored(5, BUILD, {})];
  const run = runSteps(await pruneBodies(), { builds, refs: builds.map((build) => build.release.tag_name) });
  assert.equal(run.status, 0, `${run.stdout}${run.stderr}`);
  assert.deepEqual(pruneDeletes(run), []);
  assert.equal(run.releases.length, builds.length);
});

test("prune deletes nothing when a list read fails, and stops at its first failed delete", FLOW, async () => {
  const bodies = await pruneBodies();
  const { builds, refs, old, orphan } = pruneState();
  for (const config of [{ listFails: true }, { refsFails: true }]) {
    const run = runSteps(bodies, { builds, refs, config });
    assert.notEqual(run.status, 0, JSON.stringify(config));
    assert.deepEqual(pruneDeletes(run), [], JSON.stringify(config));
  }
  const stopped = runSteps(bodies, { builds, refs, config: { deleteFails: [old[1].release.tag_name] } });
  assert.notEqual(stopped.status, 0);
  assert.deepEqual(pruneDeletes(stopped), [
    `api -X DELETE repos/example/fake-repo/git/refs/tags/${orphan}`,
    ...old.slice(0, 2).map((build) => `release delete ${build.release.tag_name} --repo example/fake-repo --yes --cleanup-tag`),
  ]);
});

test("the prune delete step refuses, before any gh call, a selected line that is not one well-formed old build", FLOW, async () => {
  const [, , remove] = await pruneBodies();
  for (const line of ["release v2.3.2", "release dev", "tag build-2.4.0", `release ${BUILD}`, "release build-0.0.0-dev.202611120000 extra", "drop build-0.0.0-dev.202611120000", "release ../build-0.0.0-dev.202611120000", " "]) {
    const run = runSteps([remove], { selected: `${line}\n` });
    assert.notEqual(run.status, 0, line);
    assert.deepEqual(run.calls, [], line);
    assert.match(run.stdout, /::error::refusing to delete /, line);
  }
});

test("the prune job runs only after dev moved, holds contents: write alone and no secret, and keeps the token away from the selection", async () => {
  const release = await workflow("release.yml");
  const job = block(release, 2, "prune-dev-builds");
  assert(job.includes("    needs: [prepare, publish]"));
  assert(job.includes("    runs-on: ubuntu-22.04"));
  assert.deepEqual(block(job.join("\n"), 4, "permissions").map((line) => line.trim()), ["permissions:", "contents: write"]);
  assert.equal(envValue(release, "DEV_BUILDS_KEPT"), '"15"');
  assert.match(steps(release, "prune-dev-builds")[0], /^ {6}- uses: actions\/checkout@[0-9a-f]{40}\n {8}with:\n {10}persist-credentials: false$/);
  for (const [name, token] of [["Read dev builds", true], ["Select old dev builds", false], ["Delete old dev builds", true]]) {
    assert.equal((await releaseStep("prune-dev-builds", name)).includes("GH_TOKEN: ${{ github.token }}"), token, name);
  }
  for (const name of ["Publish dev build", "Point dev at the new build"]) {
    assert.match(await releaseStep("publish", name), /\n {8}if: needs\.prepare\.outputs\.channel == 'dev'\n/, name);
  }
  const stable = await releaseStep("publish", "Publish stable release");
  assert.match(stable, /\n {8}if: needs\.prepare\.outputs\.channel == 'stable'\n/);
  assert.doesNotMatch(stable, /build-|dev-builds/, "a stable release makes no build-* or prune call");
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

test("the secret and runner guards read every job of every workflow file, matrix runner values included, and the secret guard reads every local composite action by the same line rules (RV-DS241-38)", async () => {
  const names = (await readdir(WORKFLOWS)).filter((name) => /\.ya?ml$/.test(name)).sort();
  assert(names.includes("ci.yml") && names.includes("release.yml"), names.join(" "));
  const exposed = [];
  for (const name of names) {
    const text = await workflow(name);
    exposed.push(...exposedJobs(text).map((job) => `${name}:${job}`));
    assert.deepEqual(runnerProblems(text), [], name);
  }
  assert.deepEqual(exposed, ["release.yml:build"], "only the release build reads a secret or binds an environment");
  const actions = await localActions();
  assert.deepEqual(actions.map(({ name }) => name), ["rust-archive", "rust-shard"], "the guard reads every local action");
  for (const { name, text } of actions) assert.deepEqual(actionSecretLines(text), [], `.github/actions/${name} reads no secret and nothing YAML hides`);
  const composite = (line) => ["name: x", "description: x", "runs:", "  using: composite", "  steps:", line, "      shell: bash", ""].join("\n");
  assert.deepEqual(actionSecretLines(composite("    - run: echo ok")), []);
  for (const line of ["    - run: echo ${{ secrets.TOKEN }}", "    - run: echo ${{ SECRETS['TOKEN'] }}", "    - run: *shared", '    - run: "echo \\x41"', "    - run: 'echo open"]) {
    assert.notDeepEqual(actionSecretLines(composite(line)), [], line);
  }
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

test("the secret guard reads every form of the secrets context, an index, a whole-context call and the bare context, in a job or workflow-wide (B17)", () => {
  for (const line of ["${{ secrets.TOKEN }}", "${{ secrets['TOKEN'] }}", '${{ secrets["TOKEN"] }}', "${{ toJSON(secrets) }}", "${{ secrets }}", "${{ format('{0}', secrets) }}"]) {
    assert.match(line, SECRET, line);
  }
  const indexed = [
    "jobs:",
    "  lint:",
    "    runs-on: ubuntu-22.04",
    "    steps:",
    "      - run: echo ${{ secrets['TOKEN'] }}",
    "  dump:",
    "    runs-on: ubuntu-22.04",
    "    steps:",
    "      - run: echo '${{ toJSON(secrets) }}'",
    "  plain:",
    "    runs-on: ubuntu-22.04",
    "",
  ].join("\n");
  assert.deepEqual(exposedJobs(indexed), ["lint", "dump"]);
  const inherited = [
    "env:",
    "  ALL: ${{ toJSON(secrets) }}",
    "jobs:",
    "  lint:",
    "    runs-on: ubuntu-22.04",
    "  test:",
    "    runs-on: ubuntu-22.04",
    "",
  ].join("\n");
  assert.deepEqual(exposedJobs(inherited), ["lint", "test"]);
});

test("the secret guard reads the secrets context in any case, as the runner does, and refuses a YAML alias, a backslash escape or a quoted scalar left open outside a block scalar, each of which hands a job secrets its lines never spell (B17 F1)", () => {
  for (const line of ["${{ SECRETS.KEY }}", "${{ Secrets['KEY'] }}", "${{ toJSON(SECRETS) }}"]) {
    assert.match(line, SECRET, line);
  }
  const shouted = [
    "jobs:",
    "  lint:",
    "    runs-on: ubuntu-22.04",
    "    steps:",
    "      - run: echo ${{ SECRETS.KEY }}",
    "  plain:",
    "    runs-on: ubuntu-22.04",
    "",
  ].join("\n");
  assert.deepEqual(exposedJobs(shouted), ["lint"]);
  const head = ["jobs:", "  build:", "    runs-on: ubuntu-22.04", "    env: &signing", "      KEY: ${{ secrets.KEY }}", "  publish:", "    runs-on: ubuntu-22.04"];
  for (const tail of [
    ["    env: *signing"],
    ["    env: *signing # shared"],
    ["    env:", "      <<: *signing"],
    ["    env: { <<: *signing }"],
    ["    steps:", "      - *step"],
    ["    env:", '      KEY: "${{ \\x73ecrets.KEY }}"'],
    ["    env:", '      KEY: "${{ \\u0073ecrets.KEY }}"'],
    ["    env:", '      KEY: "${{ secr\\', '        ets.KEY }}"'],
    ["    steps:", '      - run: "echo ${{ \\u0053ECRETS.KEY }}"'],
    ["    steps:", "      - run: |", "          echo hi", "        env:", '          KEY: "${{ \\u0073ecrets.KEY }}"'],
    ["    env:", '      KEY: "${{ toJSON(', "        x: |", '          \\u0073ecrets) }}"'],
    ["    env:", '      KEY: &a "${{ toJSON(', "        x: |", '          \\u0073ecrets) }}"'],
    ["    env:", '      KEY: !!str "${{ toJSON(', "        x: |", '          \\u0073ecrets) }}"'],
  ]) {
    const text = [...head, ...tail, ""].join("\n");
    assert.throws(() => exposedJobs(text), /alias, a backslash/, text);
  }
  const scripted = [
    "jobs:",
    "  build:",
    "    runs-on: ubuntu-22.04",
    "    steps:",
    "      - run: |",
    "          printf '%s\\n' \"$NAME\" | tee *.txt",
    "          echo \"it's",
    "        env:",
    "          NAME: plain",
    "",
  ].join("\n");
  assert.deepEqual(exposedJobs(scripted), []);
});

const NOTES_SCRIPT = fileURLToPath(new URL("./notes.mjs", import.meta.url));
const NOTES_CHANGELOG = "# Changelog\n\n## [0.0.0] - 2026-11-13 12:07 UTC - Dev build\n\n### Added\n\n- A `quoted` line with $HOME and 100%.\n\n## [0.0.0-older] - 2026-11-01 - Released\n\n- Old.\n";

/**
 * Runs publish's "Write release notes" step in a temporary checkout. Inputs: the channel, the version,
 * the changelog text (null for none) and the signing marks by target. Output: the step's status, its
 * stderr, notes.txt (the release body) and update-notes.txt (the updater manifest's notes).
 */
async function writeNotes({ channel, version, changelog, signing }) {
  const body = runBody(await releaseStep("publish", "Write release notes"));
  const root = mkdtempSync(join(tmpdir(), "ca-relnotes-"));
  try {
    mkdirSync(join(root, "scripts", "release"), { recursive: true });
    mkdirSync(join(root, "signing"));
    copyFileSync(NOTES_SCRIPT, join(root, "scripts", "release", "notes.mjs"));
    if (changelog !== null) writeFileSync(join(root, "CHANGELOG.md"), changelog);
    for (const [target, mark] of Object.entries(signing)) writeFileSync(join(root, "signing", `${target}.txt`), mark);
    writeFileSync(join(root, "step.sh"), body);
    const run = spawnSync("/bin/bash", ["--noprofile", "--norc", "-eo", "pipefail", join(root, "step.sh")], {
      cwd: root, encoding: "utf8", timeout: STEP_MS,
      env: { PATH: `${stepBin()}:/usr/bin:/bin`, HOME: root, GITHUB_SHA: COMMIT, CHANNEL: channel, VERSION: version },
    });
    assert.equal(run.error, undefined, `the step ends within ${STEP_MS} ms`);
    const read = (name) => (existsSync(join(root, name)) ? readFileSync(join(root, name), "utf8") : null);
    return { status: run.status, stderr: run.stderr, notes: read("notes.txt"), update: read("update-notes.txt") };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("release notes carry the version's CHANGELOG section after the title and signing lines; the updater keeps the short notes", async () => {
  const dev = await writeNotes({ channel: "dev", version: "0.0.0-dev.202611131207", changelog: NOTES_CHANGELOG, signing: { "aarch64-apple-darwin": "signed", "x86_64-unknown-linux-gnu": "unsigned" } });
  assert.equal(dev.status, 0, dev.stderr);
  const short = "Dev build 0.0.0-dev.202611131207 of 0123456.\nNot code signed: x86_64-unknown-linux-gnu\n";
  assert.equal(dev.update, short);
  assert.equal(dev.notes, `${short}\n### Added\n\n- A \`quoted\` line with $HOME and 100%.\n`);

  const stable = await writeNotes({ channel: "stable", version: "0.0.0", changelog: NOTES_CHANGELOG, signing: { "aarch64-apple-darwin": "signed" } });
  assert.equal(stable.status, 0, stable.stderr);
  assert.equal(stable.update, "CrazyAgent 0.0.0\n");
  assert.equal(stable.notes, "CrazyAgent 0.0.0\n\n### Added\n\n- A `quoted` line with $HOME and 100%.\n");
});

test("release notes keep today's one line when the CHANGELOG has no section for the version or is missing", async () => {
  for (const changelog of [null, "# Changelog\n\n## [9.9.9] - 2026-11-13 - Released\n\n- Other.\n"]) {
    const run = await writeNotes({ channel: "stable", version: "0.0.0", changelog, signing: { "x86_64-apple-darwin": "unsigned" } });
    assert.equal(run.status, 0, run.stderr);
    assert.equal(run.notes, "CrazyAgent 0.0.0\nNot code signed: x86_64-apple-darwin\n");
    assert.equal(run.update, run.notes);
  }
});

test("the updater manifest reads the short notes, and the release body and dev pointer read the full notes", async () => {
  const manifest = await releaseStep("publish", "Write update manifests and checksums");
  assert.match(manifest, / --notes "\$\(cat update-notes\.txt\)" /);
  assert(!manifest.includes("cat notes.txt"), "the 4096-character manifest bound never meets a CHANGELOG section");
  assert.match(await releaseStep("publish", "Publish dev build"), /-F body=@notes\.txt /);
});
