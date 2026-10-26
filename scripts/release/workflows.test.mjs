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
  assert(!/^\s+if: .*(always|cancelled|failure)\(\)/m.test(release), "no job runs after a failed or cancelled dependency");
});

const ORACLE = `\${{ fromJSON((github.event_name == 'push' || github.event.pull_request.head.repo.full_name == github.repository) && '["self-hosted","Linux","X64","oracle"]' || '"ubuntu-22.04"') }}`;

test("Linux CI runs on the Oracle runners for pushes and same-repository pull requests, and fork pull requests stay on GitHub", async () => {
  const ci = await workflow("ci.yml");
  assert(block(block(ci, 0, "jobs").join("\n"), 2, "linux").includes(`    runs-on: ${ORACLE}`));
  for (const name of ["ci.yml", "release.yml"]) {
    const text = await workflow(name);
    assert.doesNotMatch(text, /pull_request_target/, name);
    for (const line of text.split("\n").filter((line) => /^\s+runs-on:/.test(line) && line.includes("self-hosted"))) {
      assert(line.includes("github.event.pull_request.head.repo.full_name == github.repository"), `${name}: ${line.trim()}`);
      assert(line.includes("github.event_name == 'push'"), `${name}: ${line.trim()}`);
      assert(/\|\| '"(ubuntu|macos|windows)-[0-9.]+"'\) \}\}$/.test(line), `${name}: fork pull requests fall back to a GitHub-hosted runner`);
    }
  }
  assert.doesNotMatch(await workflow("release.yml"), /self-hosted/, "release builds stay on GitHub-hosted runners");
});

test("on a self-hosted runner CI installs nothing globally and keeps its build cache bounded", async () => {
  const ci = await workflow("ci.yml");
  const jobs = block(ci, 0, "jobs").join("\n");
  for (const job of ["linux", "macos"]) {
    const steps = block(jobs, 2, job).join("\n");
    assert.match(steps, /"\$RUNNER_TOOL_CACHE\/claude-code\/\$CLAUDE_CODE_VERSION\/(x64|arm64)"/, job);
    assert.match(steps, />> "\$GITHUB_PATH"/, job);
    assert.doesNotMatch(steps, /\/usr\/local\/bin\/claude/, job);
    assert.match(steps, /- if: runner\.environment == 'github-hosted'\n\s+uses: actions\/cache@/, job);
    assert.match(steps, /"\$RUNNER_TOOL_CACHE\/crazyagent-ci\/target"/, job);
    assert.match(steps, /\(\( size_kib > 20 \* 1024 \* 1024 \)\)/, job);
    assert.match(steps, /command -v rustup/, job);
    for (const install of steps.matchAll(/^.*(\bsudo (?!-n true|apt-get)\S+|brew install).*$/gm)) {
      const before = steps.slice(0, install.index);
      assert(before.lastIndexOf('[ "$RUNNER_ENVIRONMENT" != github-hosted ]') > before.lastIndexOf("- name:"),
        `${job}: "${install[0].trim()}" runs only after the step refuses self-hosted runners`);
    }
  }
  const linux = block(jobs, 2, "linux").join("\n");
  assert.match(linux, /dpkg -s "\$package"/);
  assert.match(linux, /sudo -n true/);
  assert.match(linux, /"\$RUNNER_TOOL_CACHE\/tmux\/\$TMUX_VERSION\/x64"/);
});
