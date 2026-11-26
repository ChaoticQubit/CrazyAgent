import assert from "node:assert/strict";
import test from "node:test";
import { SECRET, block, exposedJobs, workflow } from "./workflow-text.mjs";

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
