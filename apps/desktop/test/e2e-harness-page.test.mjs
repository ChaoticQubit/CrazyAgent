import assert from "node:assert/strict";
import test from "node:test";
import { owner } from "./native/e2e-harness.mjs";

test("waiting for what the owner sees right after a navigation keeps polling while the new document has no body yet", async () => {
  const bodies = [null, null, { innerText: "Team graph" }];
  let reads = 0;
  const evaluate = async (expression) => {
    const document = { body: bodies[Math.min(reads, bodies.length - 1)] };
    reads += 1;
    return new Function("document", `return ${expression};`)(document);
  };
  const me = owner({ evaluate, send: async () => ({}) }, "/nonexistent-evidence");
  const text = await me.waitFor("the Agents tab", (seen) => seen.includes("Team graph"), 5_000);
  assert.equal(text, "Team graph");
  assert.equal(reads, bodies.length, "the page is read until its body is there");
});
