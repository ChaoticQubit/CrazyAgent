import assert from "node:assert/strict";
import { test } from "node:test";
import { checks, rcliProblem } from "./trial4/n-d30-rcli-harness.mjs";

test("N-D30 is one trial 4 check for OP-51 that runs at the end", () => {
  assert.deepEqual(checks.map((check) => [check.id, check.op, check.after]), [["N-D30", ["OP-51"], "end"]]);
});

test("N-D30 takes RCLI listed as Ready or as a master-only harness under Limited control", () => {
  assert.equal(rcliProblem({ group: "Ready", status: "Ready" }), null);
  assert.equal(rcliProblem({ group: "Limited control", status: "Limited control: runs only as a CrazyAgent master" }), null);
});

test("N-D30 names any other place Settings, Harnesses lists RCLI", () => {
  assert.equal(rcliProblem({ group: "Limited control", status: "Limited control: installed, but CrazyAgent cannot start it yet" }),
    "RCLI is listed under Limited control (Limited control: installed, but CrazyAgent cannot start it yet), not Ready or as master only");
  assert.equal(rcliProblem({ group: "Not installed", status: "Not installed" }), "RCLI is listed under Not installed (Not installed), not Ready or as master only");
});
