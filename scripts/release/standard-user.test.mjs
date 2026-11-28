import assert from "node:assert/strict";
import test from "node:test";
import { jobsOf, workflow } from "./workflow-text.mjs";

/**
 * The steps of the farm's Windows standard-user job, one string per step. Input: none.
 * Output: the job's header lines and its steps.
 */
async function standardUserJob() {
  const job = jobsOf(await workflow("tests.yml")).find(({ name }) => name === "windows-standard-user");
  assert(job, "the farm has a windows-standard-user job");
  const text = job.lines.join("\n");
  return { text, steps: text.split(/\n(?= {6}- )/).slice(1) };
}

test("the Windows standard-user job builds exactly the two ignored rows and runs them as a created standard user, failing unless the elevated control fails with its refusal", async () => {
  const { text, steps } = await standardUserJob();
  assert.match(text, /\n {4}runs-on: windows-2022\n/);
  assert.match(text, /\n {4}timeout-minutes: 150\n/);
  const build = steps.find((step) => step.startsWith("      - name: Build the rows into a folder the gate user can read\n"));
  assert(build, "a step builds the rows");
  assert(build.includes(`[ "$listed" = 2 ] || { echo "::error::expected 2 ignored rows, listed $listed"; exit 1; }`), "the build lists exactly the two ignored rows");
  const run = steps.find((step) => step.startsWith("      - name: Run the rows as the gate standard user, with an elevated control\n"));
  assert(run, "a step runs the rows as the gate user");
  assert.match(run, /\n {8}shell: pwsh\n {8}env:\n {10}GATE_STAGE: run\n/);
  assert(run.includes("foreach ($row in @($env:ROW_ONE, $env:ROW_TWO)) {"), "both rows run as the gate user");
  assert(run.includes("-Credential $cred"), "the rows start under the gate user's credential");
  assert(run.includes("& C:\\gate\\rows.exe --ignored --exact $env:ROW_ONE --test-threads=1 *> C:\\gate\\control.log"), "the control runs row one as the elevated runner");
  assert(run.includes("$refused = Select-String -Path C:\\gate\\control.log -Pattern 'this row runs only as a standard user' -Quiet"), "the control reads its refusal");
  assert(run.includes("if ($control -eq 0 -or -not $refused) { Write-Host '::error::the elevated control did not fail with its refusal, so the gate cannot read red'; $failed = $true }"), "a passing control fails the job");
  assert.match(run, /\n {10}if \(\$failed\) \{ exit 1 \}\n {10}exit 0$/, "the step ends on the gate verdict, not on the control's exit code");
});
