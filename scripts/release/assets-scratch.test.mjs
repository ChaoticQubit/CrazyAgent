import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

test("the release asset tests leave nothing in the temp folder, throwaway signing keys included (RV-DS241-36)", () => {
  const temp = mkdtempSync(join(tmpdir(), "release-assets-tmp-"));
  try {
    const env = { ...process.env, TMPDIR: temp };
    delete env.NODE_TEST_CONTEXT;
    const run = spawnSync(process.execPath, ["--test", fileURLToPath(new URL("./assets.test.mjs", import.meta.url))], { encoding: "utf8", env, timeout: 300000 });
    assert.deepEqual(readdirSync(temp), [], "every scratch folder is removed");
    assert.equal(run.status, 0, `${run.stdout}${run.stderr}`);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});
