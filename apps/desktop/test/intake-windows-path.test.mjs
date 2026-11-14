import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const api = await import("../src/api-intake.ts");
const intake = await import("../src/screens/intake/intake.ts");

const UNREACHABLE = { kind: "error", code: "engine_unreachable", message: "CrazyAgent's engine isn't running." };

test("RV-DS241-1: Move or Copy of a picked Windows drive or network share folder reaches the native bridge, which decides by the host's own rule", async () => {
  for (const path of ["C:\\Users\\me\\site", "\\\\server\\share\\site"]) {
    assert.equal(intake.pickedFolderProblem(path, null), null, `the picker takes ${path}`);
    for (const mode of ["move", "copy"]) {
      assert.deepEqual(await api.startIntake(path, mode, []), UNREACHABLE, `${mode} ${path} is sent, not refused in the window`);
    }
  }
});

test("the window never refuses an intake path on its own; the native bridge's path check answers for every platform", async () => {
  for (const path of ["/Users/me/site", "relative/site", ""]) {
    assert.deepEqual(await api.startIntake(path, "move", []), UNREACHABLE, path);
  }
});
