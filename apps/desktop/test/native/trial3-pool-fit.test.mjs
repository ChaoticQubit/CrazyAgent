import assert from "node:assert/strict";
import { test } from "node:test";
import { poolFitVerdict } from "./trial3-pool-fit.mjs";

const OWNER = "Claude has no account Claude Code can use. Add an account it can use to the pool, or pick another harness.";
const claudes = { name: "Claude", summary: "4 Claude", readiness: "4 ready" };

test("a pool of signed-in Claude accounts fits Claude Code as master when the Team editor shows no warning", () => {
  const detail = poolFitVerdict({ pool: claudes, master: "Claude Code", warnings: [], help: "Uses an account from Claude." });
  assert.match(detail, /^Claude \(4 Claude; 4 ready\) fits Claude Code as master/u);
});

test("the owner's OP-41 warning under the master fails the check with the owner's words", () => {
  assert.throws(() => poolFitVerdict({ pool: claudes, master: "Claude Code", warnings: [OWNER], help: "Uses an account from Claude." }),
    { message: `OP-41: the Team editor says "${OWNER}" for 4 Claude` });
});

test("a pool with no Claude account cannot show OP-41, so the check reports it did not run", () => {
  assert.throws(() => poolFitVerdict({ pool: { name: "Work", summary: "2 ChatGPT, 1 API key", readiness: "3 ready" }, master: "Claude Code", warnings: [], help: "Uses an account from Work." }),
    { message: "not run: Work holds no Claude account (2 ChatGPT, 1 API key)" });
  assert.throws(() => poolFitVerdict({ pool: { name: "Mixed", summary: "1 ChatGPT, 0 Claude", readiness: "1 ready" }, master: "Claude Code", warnings: [], help: "Uses an account from Mixed." }),
    { message: "not run: Mixed holds no Claude account (1 ChatGPT, 0 Claude)" });
});

test("the check needs Claude Code as the master and the pool's own help line", () => {
  assert.throws(() => poolFitVerdict({ pool: claudes, master: "Codex", warnings: [], help: "Uses an account from Claude." }),
    { message: "the master harness reads Codex, not Claude Code" });
  assert.throws(() => poolFitVerdict({ pool: claudes, master: "Claude Code", warnings: [], help: "Uses an account from the pool you choose." }),
    { message: "the master help reads \"Uses an account from the pool you choose.\", not \"Uses an account from Claude.\"" });
});

test("a warning that is not OP-41's, such as a team error, does not fail the pool check", () => {
  const detail = poolFitVerdict({ pool: { name: "Claude", summary: "3 Claude, 1 ChatGPT", readiness: "3 ready, 1 not signed in" }, master: "Claude Code",
    warnings: ["Not ready yet: changing a project's team isn't in this build."], help: "Uses an account from Claude." });
  assert.match(detail, /^Claude \(3 Claude, 1 ChatGPT; 3 ready, 1 not signed in\) fits Claude Code/u);
});
