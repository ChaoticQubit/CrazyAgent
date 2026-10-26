import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
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

test("CI runs for pull requests and as a reusable workflow, with a concurrency group that needs no pull request", async () => {
  const ci = await workflow("ci.yml");
  const on = block(ci, 0, "on");
  assert(on.includes("  pull_request:"));
  assert(on.includes("  workflow_call:"));
  assert.match(ci, /^ {2}group: ci-\$\{\{ github\.event\.pull_request\.number \|\| github\.sha \}\}$/m);
  assert.doesNotMatch(ci, /secrets\./);
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
  assert.deepEqual(release.split("\n").filter((line) => /^\s+if: .*(always|cancelled|failure)\(\)/.test(line)).map((line) => line.trim()), ["if: always() && matrix.os == 'macos'"], "only the keychain cleanup step runs after a failure");
});

const GUARD = "github.event_name == 'push'";
const RUNNERS = Object.freeze({
  linux: `\${{ fromJSON(${GUARD} && '["self-hosted","Linux","X64","oracle"]' || '"ubuntu-22.04"') }}`,
  macos: `\${{ fromJSON(${GUARD} && '["self-hosted","macOS","badal"]' || '"macos-15"') }}`,
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

test("CI runs on the owner's runners only for the release workflow's pushes, and every pull request, fork or same-repository, stays on GitHub", async () => {
  const ci = await workflow("ci.yml");
  const jobs = block(ci, 0, "jobs").join("\n");
  for (const [job, runner] of Object.entries(RUNNERS)) assert(block(jobs, 2, job).includes(`    runs-on: ${runner}`), job);
  for (const name of ["ci.yml", "release.yml"]) {
    const text = await workflow(name);
    assert.doesNotMatch(text, /pull_request_target/, name);
    assert.doesNotMatch(text, /head\.repo/, `${name}: no pull request reaches a self-hosted runner by its head repository`);
    for (const line of text.split("\n").filter((line) => /^\s+runs-on:/.test(line) && line.includes("self-hosted"))) {
      assert(line.includes(`fromJSON(${GUARD} && `), `${name}: ${line.trim()}`);
      assert(/\|\| '"(ubuntu|macos|windows)-[0-9.]+"'\) \}\}$/.test(line), `${name}: pull requests fall back to a GitHub-hosted runner`);
    }
  }
});

test("release builds macOS on badal only for pushes, and every other release job stays on GitHub", async () => {
  const release = await workflow("release.yml");
  assert.deepEqual(block(release, 0, "on").filter((line) => /^ {2}\S/.test(line)), ["  push:"], "no pull request can start a release");
  const matrix = block(block(release, 2, "build").join("\n"), 6, "matrix").join("\n");
  const entries = matrix.split(/\n(?= {10}- )/).slice(1);
  const macos = entries.filter((entry) => entry.includes("os: macos"));
  assert.deepEqual(macos.map((entry) => entry.match(/target: (\S+)/)[1]).sort(), ["aarch64-apple-darwin", "x86_64-apple-darwin"]);
  for (const entry of entries) {
    assert.equal(entry.includes("runner: [self-hosted, macOS, badal]"), entry.includes("os: macos"), entry);
  }
  assert.equal(release.split("\n").filter((line) => line.includes("self-hosted")).length, 2);
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
  for (const job of ["linux", "macos"]) {
    const text = steps(ci, job).join("\n");
    assert.match(text, /"\$RUNNER_TOOL_CACHE\/crazyagent-ci\/target"/, job);
    assert.match(text, /\(\( size_kib > 20 \* 1024 \* 1024 \)\)/, job);
  }
  for (const [name, job] of [["ci.yml", "linux"], ["ci.yml", "macos"], ["release.yml", "build"]]) {
    const all = steps(name === "ci.yml" ? ci : release, job);
    const text = all.join("\n");
    assert.match(text, /- if: runner\.environment == 'github-hosted'\n\s+uses: actions\/cache@/, job);
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
  for (const [name, job, title] of [["ci.yml", "linux", "Install Claude Code"], ["ci.yml", "macos", "Install Claude Code"], ["release.yml", "build", "Install x86_64 bun for the Intel rcli host"]]) {
    const step = steps(await workflow(name), job).find((text) => text.includes(`name: ${title}`));
    assert(step, `${job}: ${title}`);
    assert.doesNotMatch(step, /\.complete/, `${job}: ${title} keeps no install marker`);
    assert.match(step, /\n\s+if ! echo "\$[A-Z0-9_]+_SHA256  \$work\/[a-z.]+" \| (sha256sum -c --status -|shasum -a 256 -s -c -) 2>\/dev\/null; then\n/, `${job}: ${title} checks the cached archive every time`);
    assert.match(step, /\n\s+cp "\$work\/([a-z.]+)" "\$prefix\/\1\.\$\$"\n\s+mv "\$prefix\/\1\.\$\$" "\$prefix\/\1"\n/, `${job}: ${title} replaces the cached archive in one rename`);
    assert.match(step, /"\$work\/bin" >> "\$GITHUB_PATH"|bun-path=\$work\/bin\/bun/, `${job}: ${title} runs the copy it just checked`);
  }
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

test("the Intel macOS release runs a pinned, checksum-verified x86_64 bun", async () => {
  const release = await workflow("release.yml");
  assert.match(release, /^ {2}BUN_DARWIN_X64_SHA256: [0-9a-f]{64}$/m);
  const all = steps(release, "build");
  const bun = all.find((step) => step.includes("id: bun-x64"));
  assert.match(bun, /if: matrix\.target == 'x86_64-apple-darwin'/);
  assert.match(bun, /echo "\$BUN_DARWIN_X64_SHA256  \$work\/bun\.zip" \| shasum -a 256 -c -/);
  assert.match(bun, /"\$RUNNER_TOOL_CACHE\/bun\/\$BUN_VERSION\/x64"/);
  for (const name of ["Install rcli source dependencies", "Build rcli host"]) {
    assert.match(all.find((step) => step.includes(`name: ${name}`)), /BUN_PATH: \$\{\{ steps\.bun-x64\.outputs\.bun-path \|\| steps\.bun\.outputs\.bun-path \}\}/, name);
  }
});
