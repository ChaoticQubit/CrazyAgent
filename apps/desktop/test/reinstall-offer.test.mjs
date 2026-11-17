import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-intake.ts");
const { REINSTALL_SIGN_IN, ReinstallView, reinstallLine } = await import("../src/screens/project/reinstall.tsx");
const { seededText } = await import("../src/screens/project/console-model.ts");
const intake = await import("../src/screens/intake/intake.ts");

const noop = () => {};
const offer = (folders, more = 0) => ({ folders, more, draft: "Please reinstall." });
const CAME = "left out when this project came into your projects folder";

test("slice 3: the reinstall offer has a client call to read it and one to dismiss it, by project id", () => {
  for (const name of ["loadReinstallOffer", "dismissReinstallOffer"]) assert.equal(typeof api[name], "function", name);
  const client = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  assert.match(client, /return invoke<ReinstallOffer \| null>\("desktop_reinstall_offer", \{ projectId \}\)/);
  assert.match(client, /return invoke<boolean>\("desktop_reinstall_dismiss", \{ projectId \}\)/);
});

test("slice 3: the notice names one folder, two, and three with the rest counted", () => {
  assert.equal(reinstallLine(offer(["node_modules"])), `node_modules was ${CAME}, as it can be rebuilt.`);
  assert.equal(reinstallLine(offer(["node_modules", "api/.venv"])), `node_modules and api/.venv were ${CAME}, as they can be rebuilt.`);
  assert.equal(reinstallLine(offer(["a/node_modules", "b/node_modules", "c/dist"], 4)),
    `a/node_modules, b/node_modules, c/dist and 4 more were ${CAME}, as they can be rebuilt.`);
});

test("slice 3: with an account the notice asks the master; with none it says to add one and opens accounts, with no Ask", () => {
  const html = (signedIn) => renderToStaticMarkup(createElement(ReinstallView, {
    offer: offer(["node_modules"]), signedIn, problem: "", busy: false, onAsk: noop, onDismiss: noop, onAccounts: noop,
  }));
  const asked = html(true);
  assert.match(asked, />Ask the master</);
  assert.match(asked, />Dismiss</);
  assert.doesNotMatch(asked, /Open accounts/);
  assert.ok(!asked.includes(REINSTALL_SIGN_IN), asked);
  const none = html(false);
  assert.doesNotMatch(none, /Ask the master/);
  assert.match(none, />Open accounts</);
  assert.match(none, />Dismiss</);
  assert.ok(none.includes(REINSTALL_SIGN_IN), none);
});

test("slice 3: Ask puts the draft alone in an empty input and after the owner's own text in one that has some", () => {
  assert.equal(seededText("", "Reinstall."), "Reinstall.");
  assert.equal(seededText("  \n", "Reinstall."), "Reinstall.");
  assert.equal(seededText("Hello", "Reinstall."), "Hello\n\nReinstall.");
});

test("slice 3: Home offers Reinstall in the project only for a done project move that left folders", () => {
  const job = (extra) => ({
    id: "move-1", kind: "move_project", project_id: "p-1", source: "/Users/o/Library/Mobile Documents/p", mode: "move", overrides: [],
    stopping: false, state: { state: "done" }, left: [{ path: "node_modules", files: 3 }], ...extra,
  });
  assert.deepEqual(intake.jobActions(job({})), ["reinstall", "dismiss"]);
  assert.equal(intake.reinstallRoute(job({})), "/projects/p-1/conversation");
  assert.deepEqual(intake.jobActions(job({ left: [], left_more: { folders: 2, files: null } })), ["reinstall", "dismiss"]);
  assert.deepEqual(intake.jobActions(job({ left: [] })), ["dismiss"]);
  assert.deepEqual(intake.jobActions(job({ kind: "add", project_id: null })), ["dismiss"]);
});
