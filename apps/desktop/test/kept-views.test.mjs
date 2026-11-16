import assert from "node:assert/strict";
import { createServer } from "node:http";
import test from "node:test";
import "./load-source.mjs";
import { bundle, withPage } from "./browser.mjs";

const coord = await import("../src/screens/team/coordination.tsx");
const harnessPanel = await bundle(new URL("./fixtures/terminal-harnesses.mjs", import.meta.url).pathname);

test("DEC-187: a switch back shows the kept terminal with the output it got while hidden and attaches nothing, and an ended one attaches again on show", async (t) => {
  const harness = coord.COORD_HARNESSES[0];
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(harnessPanel);
    await evaluate(`Fixture.open("master", ${JSON.stringify(harness)})`);
    assert.equal(await evaluate("Fixture.until(() => Fixture.view().live)"), true, "the terminal went live");
    await evaluate('Fixture.output("before the switch\\r\\n")');
    assert.equal(await evaluate('Fixture.until(() => Fixture.view().text.includes("before the switch"))'), true);
    await evaluate("Fixture.away()");
    const parked = await evaluate('({ panel: document.querySelector(".tg-terminal") !== null, host: document.querySelector(".tg-terminal-parking > .tg-terminal-host") !== null, focus: Boolean(document.activeElement?.closest(".xterm")) })');
    await evaluate('Fixture.output("while hidden\\r\\n")');
    const showMs = await evaluate(`Fixture.back("master", ${JSON.stringify(harness)})`);
    const shown = await evaluate('Fixture.until(() => Fixture.view().text.includes("before the switch") && Fixture.view().text.includes("while hidden"))');
    const live = await evaluate("Fixture.view().live");
    const attachesAfterSwitch = await evaluate("Fixture.attaches()");
    await evaluate("Fixture.close()");
    assert.equal(await evaluate('Fixture.until(() => document.querySelector(".tg-terminal-state[role=alert]") !== null)'), true, "the closed terminal says it ended");
    await evaluate("Fixture.away()");
    await evaluate(`Fixture.back("master", ${JSON.stringify(harness)})`);
    const again = await evaluate("Fixture.until(() => Fixture.view().live)");
    return { parked, showMs, shown, live, attachesAfterSwitch, again, attachesAfterEnded: await evaluate("Fixture.attaches()") };
  });
  t.diagnostic(`Chrome mount of a kept terminal, render and show: ${seen.showMs.toFixed(2)} ms`);
  assert.deepEqual(seen.parked, { panel: false, host: true, focus: false }, "away keeps the view parked, unfocused");
  assert.equal(seen.shown, true, "the switch back shows the kept scrollback and the output that came while hidden");
  assert.equal(seen.live, true);
  assert.equal(seen.attachesAfterSwitch, 1, "a switch back attaches nothing");
  assert.equal(seen.again, true, "an ended terminal shown again is live again");
  assert.equal(seen.attachesAfterEnded, 2, "an ended terminal attaches once more when shown again");
});

/** Moves the page to a local http origin, where the page's storage works. Inputs: the page's
 * evaluate and send. Output: the server, for the caller to close. */
async function httpPage(evaluate, send) {
  const server = createServer((_, reply) => reply.end("<!doctype html><title>terminal</title>"));
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  const origin = `http://127.0.0.1:${server.address().port}`;
  await send("Page.navigate", { url: `${origin}/` });
  for (let waited = 0; !(await evaluate("location.origin + document.readyState")).endsWith("complete") || await evaluate("location.origin") !== origin; waited += 20) {
    assert.ok(waited < 5000, "the page never loaded");
    await new Promise((done) => setTimeout(done, 20));
  }
  return server;
}

test("DEC-187: app start loads the remembered terminal hidden and unfocused after the first frame, a switch to it attaches nothing more, the last project's master loads when none is remembered, a closed project lets go of its view, and a gone or unlisted terminal loads nothing", async () => {
  const harness = coord.COORD_HARNESSES[0];
  const master = { id: "team-1", kind: "master", parent: null, label: "Master", harness, status: "working", status_label: "Working", account: null, terminal: "team-1", agent: 1 };
  const remembered = (patch = {}) => ({ project: "p1", terminal: "team-1", pane: null, width: 600, height: 300, ...patch });
  let server = null;
  const seen = await withPage(async ({ evaluate, send }) => {
    server = await httpPage(evaluate, send);
    await evaluate(harnessPanel);
    const start = async (setup, graphs) => {
      const atMount = await evaluate(`Fixture.preload(${JSON.stringify(setup)})`);
      await evaluate(`Fixture.until(() => Fixture.graphs().length >= ${graphs})`);
      await evaluate("new Promise((done) => setTimeout(done, 400))");
      return { atMount, attaches: await evaluate("Fixture.attaches()"), graphs: await evaluate("Fixture.graphs()"), placed: await evaluate("Fixture.placed()") };
    };
    const saved = await start({ saved: [remembered()], last: null, nodes: [master], projects: ["p1"] }, 1);
    await evaluate('Fixture.output("loaded before the owner looked\\r\\n")');
    await evaluate(`Fixture.back("master", ${JSON.stringify(harness)})`);
    const switched = {
      text: await evaluate('Fixture.until(() => Fixture.view().text.includes("loaded before the owner looked"))'),
      attaches: await evaluate("Fixture.attaches()"),
      placed: await evaluate("Fixture.placed()"),
    };
    const fallback = await start({ saved: [], last: "p1", nodes: [master], projects: ["p1"] }, 1);
    await evaluate(`Fixture.back("master", ${JSON.stringify(harness)})`);
    await evaluate("Fixture.away()");
    await evaluate("Fixture.list([])");
    const closed = { released: await evaluate("Fixture.released()"), placed: await evaluate("Fixture.placed()") };
    const gone = await start({ saved: [remembered({ terminal: "team-9" })], last: "p1", nodes: [master], projects: ["p1"] }, 1);
    const unlisted = await start({ saved: [remembered({ project: "p2" })], last: "p2", nodes: [master], projects: ["p1"] }, 0);
    return { saved, switched, fallback, closed, gone, unlisted };
  }).finally(() => server?.close());
  assert.deepEqual(seen.saved, { atMount: 0, attaches: 1, graphs: ["p1"], placed: { parked: 1, shown: 0, focus: false } }, "the remembered terminal loads hidden and unfocused, after the mount returned");
  assert.deepEqual(seen.switched, { text: true, attaches: 1, placed: { parked: 0, shown: 1, focus: true } }, "showing the preloaded terminal attaches nothing more and shows what came while hidden");
  assert.deepEqual(seen.fallback, { atMount: 0, attaches: 1, graphs: ["p1"], placed: { parked: 1, shown: 0, focus: false } }, "with none remembered the last project's master loads");
  assert.deepEqual(seen.closed, { released: { detaches: 1, saved: [] }, placed: { parked: 0, shown: 0, focus: false } }, "a project gone from the complete list lets its kept view go with a detach and is forgotten");
  assert.deepEqual(seen.gone, { atMount: 0, attaches: 0, graphs: ["p1"], placed: { parked: 0, shown: 0, focus: false } }, "a terminal the team no longer lists loads nothing");
  assert.deepEqual(seen.unlisted, { atMount: 0, attaches: 0, graphs: [], placed: { parked: 0, shown: 0, focus: false } }, "a project not listed is never read or loaded");
});
