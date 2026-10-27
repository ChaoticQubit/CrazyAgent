import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const extension = fileURLToPath(new URL("..", import.meta.url));
const tsc = fileURLToPath(new URL("../../desktop/node_modules/typescript/bin/tsc", import.meta.url));

function files(folder) {
  return readdirSync(folder, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => join(entry.parentPath, entry.name).slice(folder.length + 1))
    .sort();
}

test("the committed out folder is exactly what tsc makes of src", () => {
  const fresh = mkdtempSync(join(tmpdir(), "ca-ide-out-"));
  try {
    execFileSync(process.execPath, [tsc, "-p", join(extension, "tsconfig.json"), "--outDir", fresh], {
      stdio: "pipe",
    });
    const committed = join(extension, "out");
    assert.deepEqual(files(committed), files(fresh));
    assert.ok(files(fresh).includes("extension.js"));
    for (const name of files(fresh)) {
      assert.equal(readFileSync(join(committed, name), "utf8"), readFileSync(join(fresh, name), "utf8"), name);
    }
  } finally {
    rmSync(fresh, { recursive: true, force: true });
  }
});
