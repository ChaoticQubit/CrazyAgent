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
