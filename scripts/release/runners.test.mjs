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

const ROSETTA = "Refuse to run under Rosetta";

/**
 * Returns the steps of `job` in `workflow` text as one string per step.
 * Inputs: the workflow text and the job name. Output: each `- ` step with its indented lines.
 */
function steps(workflowText, job) {
  return block(block(workflowText, 0, "jobs").join("\n"), 2, job).join("\n").split(/\n(?= {6}- )/).slice(1);
}

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

test("every test farm Linux tmux install first takes group and other write off the runner's world-writable /usr/local/bin and checks it, so the engine's tool resolver accepts the farm tmux there instead of passing on to Ubuntu's older one (DEC-191)", async () => {
  const farm = await workflow("tests.yml");
  const shard = await readFile(new URL("../../.github/actions/rust-shard/action.yml", import.meta.url), "utf8");
  const sites = [...steps(farm, "node"), ...shard.split(/\n(?= {4}- )/)].filter((step) => /^ +- name: Install tmux on Linux\n/.test(step));
  assert.equal(sites.length, 2, "the node part and the Rust shard action");
  for (const site of sites) {
    const owned = site.indexOf("sudo chmod go-w /usr/local/bin\n");
    assert(owned >= 0 && owned < site.indexOf('test -z "$(find /usr/local/bin -maxdepth 0 -perm /022)"\n'), "the folder loses group and other write, then the step checks it");
    assert(site.indexOf("sudo install ") > owned, "before tmux goes in");
  }
});

test("both test farm cargo builds turn on the engine's owner-trace feature only where the built tree's engine defines it, so a failed row's captured stderr holds one line per owner exchange and a tree from any other base still builds (leader 02:58, #297 F1; clean_mac_release.rs keeps it out of every release build)", async () => {
  const archive = await readFile(new URL("../../.github/actions/rust-archive/action.yml", import.meta.url), "utf8");
  const parts = archive.split(/\n(?= {4}- )/);
  const check = parts.findIndex((step) => step.startsWith("    - name: Turn on the engine's owner trace where this tree defines it\n"));
  assert(check >= 0 && parts[check].includes("if grep -Eq '^owner-trace[[:space:]]*=' crates/crazyagent/Cargo.toml; then\n          echo \"FARM_ENGINE_FEATURES=crazyagent/owner-trace\" >> \"$GITHUB_ENV\"\n"), "one check reads the built tree's engine manifest");
  const builds = parts.flatMap((step, at) => step.split("\n").filter((line) => /^ +cargo (build|nextest archive) /.test(line)).map((line) => ({ at, line })));
  assert.equal(builds.length, 2, "the engine binaries and the test archive");
  for (const { at, line } of builds) {
    assert(at > check, "after the check");
    assert(line.includes(' ${FARM_ENGINE_FEATURES:+--features "$FARM_ENGINE_FEATURES"} --locked --offline'), line);
  }
  assert.equal(archive.match(/owner-trace/g).length, 2, "named only by the check");
});
