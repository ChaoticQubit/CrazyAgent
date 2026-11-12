import assert from "node:assert/strict";
import { test } from "node:test";
import { TRIAL3_STEPS } from "./trial3-steps.mjs";
import { hooksVerdict, memoryVerdict, rememberPrompt } from "./trial4-context.mjs";
import { checks as hooks } from "./trial4/n-d22-hooks-list.mjs";
import { checks as memory } from "./trial4/n-d24-project-memory.mjs";
import { checks as pool } from "./trial4/n-d28-pool-fit.mjs";

test("each context check module holds one check of the trial 4 shape, run after a trial 3 step", () => {
  const slugs = new Set(TRIAL3_STEPS.map((step) => step.slug));
  const found = [hooks, memory, pool].map((checks) => {
    assert.equal(checks.length, 1);
    const [check] = checks;
    assert.deepEqual(Object.keys(check).sort(), ["after", "id", "op", "run", "title", "why"]);
    assert.ok(check.op.length > 0 && check.op.every((op) => /^OP-\d+$/u.test(op)), check.id);
    assert.ok(check.why.startsWith("owner: "), check.id);
    assert.ok(slugs.has(check.after), `${check.id} runs after ${check.after}`);
    assert.equal(typeof check.run, "function");
    return [check.id, check.op.join(" "), check.after];
  });
  assert.deepEqual(found, [["N-D22", "OP-13 OP-23", "process-instructions"], ["N-D24", "OP-22", "changes"], ["N-D28", "OP-41", "add-project"]]);
});

test("the Hooks list passes with the owner's caveman and ADHD hooks and says how it loaded", () => {
  const plugins = ["caveman", "caveman", "i-have-adhd", "superpowers"];
  assert.equal(hooksVerdict({ plugins, failedFirst: false, failedAfterRetry: false }),
    "4 hooks listed from caveman, i-have-adhd, superpowers; the owner's caveman and i-have-adhd hooks are there; it loaded on the first open, so Try again was not needed");
  assert.match(hooksVerdict({ plugins: ["caveman"], failedFirst: true, failedAfterRetry: false }), /the first load failed and Try again loaded it$/u);
});

test("the Hooks list fails in the owner's words when it never loads, is empty or misses the owner's plugins", () => {
  assert.throws(() => hooksVerdict({ plugins: [], failedFirst: true, failedAfterRetry: true }), /^Error: OP-23: the Hooks list still says Couldn't load hooks\. after Try again$/u);
  assert.throws(() => hooksVerdict({ plugins: [], failedFirst: false, failedAfterRetry: false }), /^Error: OP-13: the Hooks list says No hooks yet$/u);
  assert.throws(() => hooksVerdict({ plugins: ["session-start", "pre-tool", "stop"], failedFirst: false, failedAfterRetry: false }),
    /^Error: OP-13: no hook of caveman or the ADHD style is listed; it lists session-start, pre-tool, stop$/u);
  assert.throws(() => hooksVerdict({ plugins: ["caveman"], failedFirst: false, failedAfterRetry: true }), /Try again is pressed only after a failed load/u);
});

test("the remember request names the token once and refuses a token that could break the message", () => {
  const message = rememberPrompt("T4-0a1b2c3d");
  assert.equal(message.split("T4-0a1b2c3d").length, 2);
  assert.match(message, /^Use your remember tool now, with scope project,/u);
  assert.throws(() => rememberPrompt("T4 \"quoted\""), /the token is plain/u);
});

test("the project memory check passes on a master's fact saved at once or approved after Ask me first", () => {
  const shown = "Project memory\nFacts agents keep for this project.\nYour notes\nT4-0a1b2c3d is the trial 4 project memory fact.\nSaved";
  const waiting = "Project memory\nMemory settings\n1 memory waits for your approval.\nYour notes\nT4-0a1b2c3d is the trial 4 project memory fact.\nNeeds approval\nApprove";
  assert.equal(memoryVerdict({ token: "T4-0a1b2c3d", writes: "save", source: "ca-trial-4 only, saved by Master", approved: false, waiting: shown, shown }),
    "Agent memory writes: Save directly; the master saved T4-0a1b2c3d with remember (ca-trial-4 only, saved by Master); it was saved at once; Context, Project memory shows it");
  assert.match(memoryVerdict({ token: "T4-0a1b2c3d", writes: "ask", source: "Waiting for your approval. ca-trial-4 only, asked by Master", approved: true, waiting, shown }),
    /; it waited for approval under Ask me first and was approved in Context, Project memory; Context, Project memory shows it$/u);
});

test("the project memory check fails when a waiting fact has no count in the project or the project points at Settings, Memory", () => {
  const source = "Waiting for your approval. ca-trial-4 only, asked by Master";
  const shown = "Your notes\nT4-0a1b2c3d is the trial 4 project memory fact.\nSaved";
  assert.throws(() => memoryVerdict({ token: "T4-0a1b2c3d", writes: "ask", source, approved: true, waiting: "Your notes\nT4-0a1b2c3d", shown }),
    /does not say how many memories wait for approval/u);
  assert.throws(() => memoryVerdict({ token: "T4-0a1b2c3d", writes: "ask", source, approved: true,
    waiting: "2 memories wait for your approval.\nApprove it in Settings, Memory.", shown }), /still sends the owner to Settings, Memory/u);
  assert.throws(() => memoryVerdict({ token: "T4-0a1b2c3d", writes: "save", source: "ca-trial-4 only, saved by Master", approved: false,
    waiting: shown, shown: `${shown}\nShowing your newest 1 notes. Settings, Memory lists them all.` }), /still sends the owner to Settings, Memory/u);
});

test("the project memory check fails when the fact is missing, not the master's or left waiting", () => {
  const empty = "Project memory\nNo memories yet";
  assert.throws(() => memoryVerdict({ token: "T4-0a1b2c3d", writes: "save", source: "ca-trial-4 only, saved by Master", approved: false, waiting: empty, shown: empty }),
    /^Error: OP-22: Context, Project memory does not show the master's fact T4-0a1b2c3d$/u);
  assert.throws(() => memoryVerdict({ token: "T4-0a1b2c3d", writes: "save", source: "ca-trial-4 only", approved: false, waiting: "", shown: "T4-0a1b2c3d" }),
    /is not the master's: ca-trial-4 only$/u);
  assert.throws(() => memoryVerdict({ token: "T4-0a1b2c3d", writes: "ask", source: "Waiting for your approval. ca-trial-4 only, asked by Master", approved: false, waiting: "", shown: "T4-0a1b2c3d" }),
    /still waits for approval/u);
  assert.throws(() => memoryVerdict({ token: "T4-0a1b2c3d", writes: "never", source: "x, saved by Master", approved: false, waiting: "", shown: "T4-0a1b2c3d" }), /save or ask/u);
});
