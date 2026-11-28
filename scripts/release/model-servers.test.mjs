import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readdir, readFile } from "node:fs/promises";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { WORKFLOWS, jobsOf, workflow } from "./workflow-text.mjs";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const ACTION = new URL("../../.github/actions/model-servers/action.yml", import.meta.url);
const CACHE = "55cc8345863c7cc4c66a329aec7e433d2d1c52a9";
const WITH = ["      with:", "        path: ~/model-servers-cache", "        key: model-servers-${{ hashFiles('.github/actions/model-servers/action.yml') }}"];

/**
 * The model-server row's name, read from the one farm job that runs it. Input: none.
 * Output: the row name.
 */
async function modelServerRow() {
  const job = jobsOf(await workflow("tests.yml")).find(({ name }) => name === "rust-model-servers");
  assert(job, "the farm has a rust-model-servers job");
  const names = [...job.lines.join("\n").matchAll(/test\(=([a-z0-9_]+)\)/g)].map((match) => match[1]);
  assert.equal(names.length, 1, "the job runs one row by its exact name");
  return names[0];
}

test("make test and test-rust-selected leave out the model-server row by its exact name, since it needs servers only the farm starts", async () => {
  const row = await modelServerRow();
  const env = { ...process.env, MAKEFLAGS: "", ENGINE_TESTS_EXCLUDE: "" };
  const shown = spawnSync("make", ["-n", "-s", "MAKE=true", "test", "test-rust-selected"], { cwd: ROOT, env, encoding: "utf8" });
  assert.equal(shown.status, 0, shown.stderr);
  const runs = shown.stdout.split("\n").filter((line) => /\bcargo test (--workspace|-p crazyagent )/.test(line));
  assert.equal(runs.length, 2, shown.stdout);
  for (const line of runs) assert.match(line, new RegExp(` -- --exact( --skip \\S+)* --skip ${row}( |;|$)`), line);
});

test("the model-servers action restores its one cache on every run and saves it only on master, keyed by its own text, and only the macos-15 rust-model-servers job uses it", async () => {
  const text = await readFile(ACTION, "utf8");
  const steps = text.split("\n    - ").slice(1).map((step) => `    - ${step}`.replace(/\n+$/, "").split("\n"));
  const restore = steps.filter((step) => step.some((line) => line.includes("actions/cache/restore@")));
  const save = steps.filter((step) => step.some((line) => line.includes("actions/cache/save@")));
  assert.deepEqual(restore, [["    - name: Restore the downloads", "      id: cache", `      uses: actions/cache/restore@${CACHE}`, ...WITH]]);
  assert.deepEqual(save, [["    - name: Keep the downloads for later runs, from master only", "      if: ${{ github.ref == 'refs/heads/master' }}", `      uses: actions/cache/save@${CACHE}`, ...WITH]]);
  assert(steps.indexOf(save[0]) > steps.findIndex((step) => step[0].includes("Download, verify")), "it saves only what it verified");
  const users = [];
  for (const name of (await readdir(WORKFLOWS)).filter((file) => /\.ya?ml$/.test(file))) {
    for (const job of jobsOf(await workflow(name))) if (job.lines.some((line) => line.includes("uses: ./.github/actions/model-servers"))) users.push(`${name}:${job.name}:${job.lines.find((line) => line.startsWith("    runs-on: "))?.trim()}`);
  }
  assert.deepEqual(users, ["tests.yml:rust-model-servers:runs-on: macos-15"]);
});
