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
  assert.deepEqual(release.split("\n").filter((line) => /^\s+if: .*(always|cancelled|failure)\(\)/.test(line)).map((line) => line.trim()), ["if: always() && matrix.os == 'macos'"], "only the keychain cleanup step runs after a failure");
});

const GUARD = "github.event_name == 'push'";
const RUNNERS = Object.freeze({
  linux: "ubuntu-22.04",
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

test("the update manifests cover exactly the targets the release matrix builds, and no Windows target ships until its sandbox holds", async () => {
  const release = await workflow("release.yml");
  const matrix = block(block(release, 2, "build").join("\n"), 6, "matrix").join("\n");
  const built = [...matrix.matchAll(/^ {12}target: (\S+)$/gm)].map((match) => match[1]);
  const manifest = steps(release, "publish").find((step) => step.includes("- name: Write update manifests and checksums"));
  assert.deepEqual(manifest.match(/ --targets (\S+)/)[1].split(","), built);
  assert.deepEqual(built, ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu"]);
});

test("the Windows CI job runs on every event and shows its failures, but cannot fail the CI workflow until its sandbox holds", async () => {
  const jobs = block(await workflow("ci.yml"), 0, "jobs").join("\n");
  const windows = block(jobs, 2, "windows");
  assert(windows.includes("    name: Windows lint and tests (informational until v2.2.1)"));
  assert(windows.includes("    continue-on-error: true"));
  assert(!windows.some((line) => /^ {4}if:/.test(line)), "the job always runs");
  assert.equal(windows.filter((line) => line.includes("continue-on-error")).length, 1, "no Windows step hides its own failure");
  for (const job of ["linux", "macos"]) assert(!block(jobs, 2, job).some((line) => line.includes("continue-on-error")), job);
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
  for (const [name, job, title] of [["ci.yml", "linux", "Install Claude Code"], ["ci.yml", "macos", "Install Claude Code"], ["ci.yml", "macos", "Install Codex"],
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
  assert.match(rust, /\n {8}if: runner\.environment != 'github-hosted'\n/);
  assert.doesNotMatch(rust, /\brustup\b|\.rustup|RUSTUP_/, "no rustup, toolchain or proxy left on the runner");
  for (const pin of ["RUSTC_DARWIN_ARM64_SHA256", "CARGO_DARWIN_ARM64_SHA256", "RUST_STD_DARWIN_ARM64_SHA256", "RUST_STD_DARWIN_X64_SHA256"]) assert(rust.includes(`$${pin}`), pin);
  assert.match(rust, /\n\s+if ! echo "\$sha  \$work\/\$archive" \| shasum -a 256 -s -c - 2>\/dev\/null; then\n\s+curl [^\n]+\n\s+echo "\$sha  \$work\/\$archive" \| shasum -a 256 -c -\n/, "every Rust archive, cached or downloaded, matches its pin");
  assert.match(rust, /\n\s+cp "\$work\/\$archive" "\$prefix\/\$archive\.\$\$"\n\s+mv "\$prefix\/\$archive\.\$\$" "\$prefix\/\$archive"\n/);
  assert.match(rust, /\n\s+tar -xJf "\$work\/\$archive" -C "\$work"\n/, "it unpacks only the copy it just checked");
  assert.match(rust, /dir="\$\(dirname "\$GITHUB_WORKSPACE"\)"\n\s+while :; do\n\s+for config in "\$dir\/\.cargo\/config\.toml" "\$dir\/\.cargo\/config"; do\n\s+if \[ -e "\$config" \]; then\n[^\n]+::error::[^\n]+\n\s+exit 1\n/, "Cargo settings above the checkout stop the release");
  assert.match(rust, /test "\$\(command -v rustc\)" = "\$work\/bin\/rustc"\n\s+test "\$\(command -v cargo\)" = "\$work\/bin\/cargo"\n/);
  assert.match(rust, /\n\s+echo "\$work\/bin" >> "\$GITHUB_PATH"\n?$/);
  const node = at(/name: Install pinned Node on the owner's runner/);
  const bun = at(/id: bun-arm64\n/);
  for (const index of [node, bun]) assert.match(all[index], /\n {8}if: runner\.environment != 'github-hosted'\n/);
  assert.match(all[node], /node-v\$NODE_VERSION-darwin-arm64\.tar\.gz/);
  assert.match(all[bun], /bun-darwin-aarch64\.zip/);
  assert(node < at(/\bnode scripts\//) && node < at(/\bnpm ci\b/), "no node or npm run comes before the pinned Node");
  assert(at(/name: Install pinned Rust/) < at(/\bcargo build\b/), "no cargo run comes before the pinned Rust");
  assert.match(all[at(/name: Use a clean Rust build folder/)], /echo "BUN_INSTALL_CACHE_DIR=\$RUNNER_TEMP\/crazyagent-release\/bun-cache" >> "\$GITHUB_ENV"/);
  assert(at(/name: Use a clean Rust build folder/) < at(/install --frozen-lockfile/), "bun installs into a fresh cache");
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

test("the Intel macOS release runs a pinned, checksum-verified x86_64 bun", async () => {
  const release = await workflow("release.yml");
  assert.match(release, /^ {2}BUN_DARWIN_X64_SHA256: [0-9a-f]{64}$/m);
  const all = steps(release, "build");
  const bun = all.find((step) => step.includes("id: bun-x64"));
  assert.match(bun, /if: matrix\.target == 'x86_64-apple-darwin'/);
  assert.match(bun, /echo "\$BUN_DARWIN_X64_SHA256  \$work\/bun\.zip" \| shasum -a 256 -c -/);
  assert.match(bun, /"\$RUNNER_TOOL_CACHE\/bun\/\$BUN_VERSION\/x64"/);
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
