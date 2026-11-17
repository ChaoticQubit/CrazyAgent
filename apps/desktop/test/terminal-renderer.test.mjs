import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";
import { bundle, withPage } from "./browser.mjs";

const coord = await import("../src/screens/team/coordination.tsx");
const harnessPanel = await bundle(new URL("./fixtures/terminal-harnesses.mjs", import.meta.url).pathname);
const harness = JSON.stringify(coord.COORD_HARNESSES[0]);

/** Opens the real panel on a page that offers WebGL2 or not, waits for it to go live and
 * prints a line into it. Inputs: `evaluate` and whether WebGL2 is offered. Output: none. */
async function live(evaluate, offered) {
  await evaluate(harnessPanel);
  await evaluate(`Fixture.offerGpu(${offered})`);
  await evaluate(`Fixture.open("master", ${harness})`);
  assert.equal(await evaluate("Fixture.until(() => Fixture.view().live)"), true, "the terminal went live");
  await evaluate('Fixture.output("drawn line\\r\\n")');
}

test("V243-TERMINAL-SLOW: a terminal on a page with WebGL2 draws on the GPU, not with DOM rows", async () => {
  const drawn = await withPage(async ({ evaluate }) => {
    await live(evaluate, true);
    return await evaluate('Fixture.until(() => Fixture.renderer() === "webgl").then(() => Fixture.renderer())');
  });
  assert.equal(drawn, "webgl");
});

test("V243-TERMINAL-SLOW: a terminal on a page without WebGL2 keeps the DOM renderer and shows its output", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, false);
    const shown = await evaluate('Fixture.until(() => Fixture.view().text.includes("drawn line"))');
    return { shown, renderer: await evaluate("Fixture.renderer()") };
  });
  assert.deepEqual(seen, { shown: true, renderer: "dom" });
});

test("V243-TERMINAL-SLOW: a terminal whose WebGL context is lost goes back to DOM rows that show its output", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true);
    const gpu = await evaluate('Fixture.until(() => Fixture.renderer() === "webgl")');
    const lost = await evaluate("Fixture.loseGpu()");
    const back = await evaluate('Fixture.until(() => Fixture.renderer() === "dom", 10000)');
    await evaluate('Fixture.output("after the loss\\r\\n")');
    const shown = await evaluate('Fixture.until(() => Fixture.view().text.includes("drawn line") && Fixture.view().text.includes("after the loss"))');
    return { gpu, lost, back, shown };
  });
  assert.deepEqual(seen, { gpu: true, lost: true, back: true, shown: true });
});
