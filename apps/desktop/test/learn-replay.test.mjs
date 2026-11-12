import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { LearningReplay, replay } = await import("../src/screens/dashboard/learn.tsx");
const { Welcome } = await import("../src/screens/first-run/index.tsx");

/** A replay's steps that record what happened, with the mark answering `answer`. */
function steps(answer) {
  const done = [];
  return {
    done,
    mark: async (step, on) => { done.push(["mark", step, on]); return answer; },
    close: () => done.push(["close"]),
    go: (route) => done.push(["go", route]),
  };
}

test("both Profile replay entries clear the skipped mark, close the dialog and open Home or first run", async () => {
  for (const [kind, route] of [["guide", { name: "home" }], ["first_run", { name: "welcome" }]]) {
    const run = steps({ kind: "ok", value: undefined });
    assert.equal(await replay(kind, run), null, kind);
    assert.deepEqual(run.done, [["mark", "skipped", false], ["close"], ["go", route]], kind);
  }
});

test("a refused clear stays on the dialog with its reason, and an engine without marks still runs first run", async () => {
  const refused = { kind: "error", code: "owner_busy", message: "The engine is busy." };
  for (const [kind, said] of [["guide", "Couldn't show the guide again. The engine is busy."],
    ["first_run", "Couldn't run first-run setup again. The engine is busy."]]) {
    const run = steps(refused);
    assert.equal(await replay(kind, run), said);
    assert.deepEqual(run.done, [["mark", "skipped", false]], "nothing moves on");
  }
  const guide = steps({ kind: "not_ready" });
  assert.equal(await replay("guide", guide), "The learning guide isn't ready in this build.");
  assert.deepEqual(guide.done, [["mark", "skipped", false]]);
  const firstRun = steps({ kind: "not_ready" });
  assert.equal(await replay("first_run", firstRun), null);
  assert.deepEqual(firstRun.done, [["mark", "skipped", false], ["close"], ["go", { name: "welcome" }]]);
});

test("the Profile dialog's Learn CrazyAgent section offers both entries", () => {
  const html = renderToStaticMarkup(createElement(LearningReplay));
  assert.match(html, /Learn CrazyAgent/);
  assert.match(html, /Show the learning guide again/);
  assert.match(html, /Run first-run setup again/);
});

test("first run started again from Profile starts its form with the saved profile", () => {
  const profile = { name: "Mira Okafor", role: "Founder", bio: "Builds tools.", preferred_stack: ["Rust"],
    working_style: { detail: "terse", initiative: "ask_first" } };
  const again = renderToStaticMarkup(createElement(Welcome, { profile, onSaved: () => undefined }));
  assert.match(again, /value="Mira Okafor"/);
  const fresh = renderToStaticMarkup(createElement(Welcome, { onSaved: () => undefined }));
  assert.doesNotMatch(fresh, /Mira Okafor/);
});
