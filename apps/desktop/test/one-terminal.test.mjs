import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/one-terminal.mjs", import.meta.url).pathname);

/** Mounts the Agents tab in a 1200x800 page with the engine answering attaches after
 * `delayMs`, runs each step in turn (`["card", name]` clicks a graph card, `["waiting", name]`
 * clicks Open terminal on a "Waiting on you" row), waiting for that agent's terminal to go live
 * when `settle` is set, then waits for the page to settle. Inputs: the steps, the delay and
 * whether to wait. Output: what the page and the engine show. */
async function run(steps, delayMs, settle) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${delayMs})`);
    assert.equal(await evaluate("Fixture.until(() => document.querySelectorAll('.tg-card-open').length === 4 && Fixture.openWaiting !== undefined && document.querySelector('[aria-label=\"Waiting on you in their terminal\"]') !== null)"), true, "the graph and the inbox drew");
    for (const [kind, name] of steps) {
      const step = kind === "card" ? `Fixture.click(${JSON.stringify(name)})` : `Fixture.openWaiting(${JSON.stringify(name)})`;
      assert.equal(await evaluate(step), true, `${kind} ${name}`);
      if (settle) {
        assert.equal(await evaluate(`Fixture.until(() => Fixture.shown().panels.includes(${JSON.stringify(`${name} terminal`)}) && Fixture.shown().live >= 1)`), true, `${name} went live`);
      }
    }
    const last = `${steps.at(-1)[1]} terminal`;
    await evaluate(`Fixture.until(() => Fixture.shown().attaches.length === ${steps.length} && Fixture.shown().panels.includes(${JSON.stringify(last)}) && Fixture.shown().live > 0)`);
    await evaluate(`new Promise((done) => setTimeout(done, ${delayMs + 300}))`);
    return evaluate("Fixture.shown()");
  });
}

test("clicking three agents in the graph leaves one terminal panel, for the last one, and one attached terminal client", async () => {
  const seen = await run([["card", "Ada"], ["card", "Brook"], ["card", "Cove"]], 0, true);
  assert.deepEqual(seen.panels, ["Cove terminal"], JSON.stringify(seen));
  assert.equal(seen.xterms, 1, JSON.stringify(seen));
  assert.deepEqual(seen.clients, ["team-3"], JSON.stringify(seen));
  assert.deepEqual(seen.detaches, ["team-1", "team-2"], JSON.stringify(seen));
});

test("clicking three agents faster than the engine attaches still leaves one panel and one attached client", async () => {
  const seen = await run([["card", "Ada"], ["card", "Brook"], ["card", "Cove"]], 400, false);
  assert.deepEqual(seen.panels, ["Cove terminal"], JSON.stringify(seen));
  assert.equal(seen.xterms, 1, JSON.stringify(seen));
  assert.deepEqual(seen.clients, ["team-3"], JSON.stringify(seen));
});

test("a terminal opened from Waiting on you is the same one panel the graph swaps, so clicking two agents after it leaves one", async () => {
  const seen = await run([["waiting", "Ada"], ["card", "Brook"], ["card", "Cove"]], 0, true);
  assert.deepEqual(seen.panels, ["Cove terminal"], JSON.stringify(seen));
  assert.equal(seen.xterms, 1, JSON.stringify(seen));
  assert.deepEqual(seen.clients, ["team-3"], JSON.stringify(seen));
  assert.deepEqual(seen.detaches, ["team-1", "team-2"], JSON.stringify(seen));
});

test("an agent waiting on you that the graph does not draw opens in the same one panel, and a graph click replaces it", async () => {
  const seen = await run([["waiting", "Dune"], ["card", "Ada"]], 0, true);
  assert.deepEqual(seen.panels, ["Ada terminal"], JSON.stringify(seen));
  assert.equal(seen.xterms, 1, JSON.stringify(seen));
  assert.deepEqual(seen.clients, ["team-1"], JSON.stringify(seen));
  assert.deepEqual(seen.detaches, ["team-5"], JSON.stringify(seen));
});

test("Open terminal in Waiting on you opens the one panel while the team graph is loading, failed, not in the build or empty", async () => {
  for (const graph of ["pending", "error", "not_ready", "empty"]) {
    const seen = await withPage(async ({ evaluate, send }) => {
      await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
      await evaluate(fixture);
      await evaluate(`Fixture.mount(0, ${JSON.stringify(graph)})`);
      assert.equal(await evaluate("Fixture.until(() => document.querySelector('[aria-label=\"Waiting on you in their terminal\"]') !== null)"), true, `${graph}: the inbox drew`);
      assert.equal(await evaluate("Fixture.openWaiting('Ada')"), true, `${graph}: Open terminal on Ada`);
      await evaluate("Fixture.until(() => Fixture.shown().panels.includes('Ada terminal') && Fixture.shown().live > 0)");
      return evaluate("Fixture.shown()");
    });
    assert.deepEqual(seen.panels, ["Ada terminal"], `${graph}: ${JSON.stringify(seen)}`);
    assert.equal(seen.xterms, 1, `${graph}: ${JSON.stringify(seen)}`);
    assert.deepEqual(seen.clients, ["team-1"], `${graph}: ${JSON.stringify(seen)}`);
  }
});
