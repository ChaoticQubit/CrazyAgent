import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";
import { bundle, withPage } from "./browser.mjs";

const coord = await import("../src/screens/team/coordination.tsx");
const harnessPanel = await bundle(new URL("./fixtures/terminal-harnesses.mjs", import.meta.url).pathname);
const harness = JSON.stringify(coord.COORD_HARNESSES[0]);

/** Opens the real panel on a page that offers WebGL2 or not, under a saved Terminal renderer
 * choice (GPU unless named) or, with `platform`, under that platform's default with nothing
 * saved, waits for it to go live and prints a line into it. Inputs: `evaluate`, whether
 * WebGL2 is offered, the renderer the GPU reports as a page expression, the choice and the
 * platform. Output: none. */
async function live(evaluate, offered, { renderer = "Fixture.HARDWARE", choice = "gpu", platform = null } = {}) {
  await evaluate(harnessPanel);
  await evaluate(`Fixture.offerGpu(${offered}, ${renderer})`);
  if (platform) await evaluate(`Fixture.startOn(${JSON.stringify(platform)})`);
  else await evaluate(`Fixture.choose(${JSON.stringify(choice)})`);
  await evaluate(`Fixture.open("master", ${harness})`);
  assert.equal(await evaluate("Fixture.until(() => Fixture.view().live)"), true, "the terminal went live");
  await evaluate('Fixture.output("drawn line\\r\\n")');
}

/** Waits until the page's terminal draws with one renderer. Inputs: `evaluate` and the
 * renderer's name. Output: whether it did within the fixture's wait. */
function drawnWith(evaluate, name) {
  return evaluate(`Fixture.until(() => Fixture.renderer() === ${JSON.stringify(name)})`);
}

/** Waits until the page's terminal shows a line. Inputs: `evaluate` and the text. Output:
 * whether it did within the fixture's wait. */
function shows(evaluate, text) {
  return evaluate(`Fixture.until(() => Fixture.view().text.includes(${JSON.stringify(text)}))`);
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
    const shown = await shows(evaluate, "drawn line");
    return { shown, renderer: await evaluate("Fixture.renderer()") };
  });
  assert.deepEqual(seen, { shown: true, renderer: "dom" });
});

test("V243-TERMINAL-SLOW: a terminal whose WebGL context is lost goes back to DOM rows that show its output", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true);
    const gpu = await drawnWith(evaluate, "webgl");
    const lost = await evaluate("Fixture.loseGpu()");
    const back = await evaluate('Fixture.until(() => Fixture.renderer() === "dom", 10000)');
    await evaluate('Fixture.output("after the loss\\r\\n")');
    const shown = await evaluate('Fixture.until(() => Fixture.view().text.includes("drawn line") && Fixture.view().text.includes("after the loss"))');
    return { gpu, lost, back, shown };
  });
  assert.deepEqual(seen, { gpu: true, lost: true, back: true, shown: true });
});

for (const [platform, drawn] of [["MacIntel", "webgl"], ["Win32", "webgl"], ["Linux x86_64", "dom"]]) {
  test(`#358 r2 (NOTE-1215): with nothing chosen, a GPU machine on ${platform} draws with ${drawn}`, async () => {
    const seen = await withPage(async ({ evaluate }) => {
      await live(evaluate, true, { platform });
      return { drawn: await drawnWith(evaluate, drawn), shown: await shows(evaluate, "drawn line"), renderer: await evaluate("Fixture.renderer()") };
    });
    assert.deepEqual(seen, { drawn: true, shown: true, renderer: drawn });
  });
}

test("#358 r2: under GPU, a machine whose WebGL is a software renderer keeps DOM rows", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true, { renderer: "Fixture.SOFTWARE" });
    return { shown: await shows(evaluate, "drawn line"), renderer: await evaluate("Fixture.renderer()") };
  });
  assert.deepEqual(seen, { shown: true, renderer: "dom" });
});

test("#358 r2: Standard keeps DOM rows on a machine with a GPU", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true, { choice: "standard" });
    return { shown: await shows(evaluate, "drawn line"), renderer: await evaluate("Fixture.renderer()") };
  });
  assert.deepEqual(seen, { shown: true, renderer: "dom" });
});

test("#358 r2: Automatic keeps DOM rows on a software renderer and draws on a GPU", async () => {
  const software = await withPage(async ({ evaluate }) => {
    await live(evaluate, true, { renderer: "Fixture.SOFTWARE", choice: "auto" });
    return { shown: await shows(evaluate, "drawn line"), renderer: await evaluate("Fixture.renderer()") };
  });
  assert.deepEqual(software, { shown: true, renderer: "dom" });
  const hardware = await withPage(async ({ evaluate }) => {
    await live(evaluate, true, { choice: "auto" });
    return await drawnWith(evaluate, "webgl");
  });
  assert.equal(hardware, true);
});

test("#358 r2: under Automatic one failed start keeps the terminal on DOM rows until the setting changes; GPU tries again", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true, { choice: "auto" });
    const first = await drawnWith(evaluate, "webgl");
    await evaluate("Fixture.failGpu(1)");
    await evaluate("Fixture.away()");
    await evaluate(`Fixture.back("master", ${harness})`);
    const failed = await drawnWith(evaluate, "dom");
    await evaluate("Fixture.away()");
    await evaluate(`Fixture.back("master", ${harness})`);
    const held = await drawnWith(evaluate, "dom");
    const heldText = await shows(evaluate, "drawn line");
    await evaluate('Fixture.choose("gpu")');
    const gpu = await drawnWith(evaluate, "webgl");
    await evaluate('Fixture.choose("auto")');
    const again = await drawnWith(evaluate, "webgl");
    return { first, failed, held, heldText, gpu, again, renderer: await evaluate("Fixture.renderer()") };
  });
  assert.deepEqual(seen, { first: true, failed: true, held: true, heldText: true, gpu: true, again: true, renderer: "webgl" });
});

test("#358 r2: a new Terminal renderer choice applies to an open terminal at once, with its output kept", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true);
    const gpu = await drawnWith(evaluate, "webgl");
    await evaluate('Fixture.choose("standard")');
    const standard = await drawnWith(evaluate, "dom");
    const standardText = await shows(evaluate, "drawn line");
    await evaluate('Fixture.choose("gpu")');
    const back = await drawnWith(evaluate, "webgl");
    await evaluate('Fixture.output("after the switch\\r\\n")');
    const text = await evaluate('Fixture.until(() => Fixture.view().text.includes("drawn line") && Fixture.view().text.includes("after the switch"))');
    return { gpu, standard, standardText, back, text, contexts: await evaluate("Fixture.liveGpu()") };
  });
  assert.deepEqual(seen, { gpu: true, standard: true, standardText: true, back: true, text: true, contexts: 1 });
});

test("#358 r2: a hidden terminal holds no WebGL context and draws on the GPU again, with what it printed meanwhile, once shown", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true);
    const gpu = await drawnWith(evaluate, "webgl");
    const shownContexts = await evaluate("Fixture.liveGpu()");
    await evaluate("Fixture.away()");
    const hiddenContexts = await evaluate("Fixture.liveGpu()");
    const hidden = await evaluate("Fixture.renderer()");
    await evaluate('Fixture.output("while hidden\\r\\n")');
    await evaluate(`Fixture.back("master", ${harness})`);
    const again = await drawnWith(evaluate, "webgl");
    const text = await evaluate('Fixture.until(() => Fixture.view().text.includes("drawn line") && Fixture.view().text.includes("while hidden"))');
    return { gpu, shownContexts, hiddenContexts, hidden, again, text, contexts: await evaluate("Fixture.liveGpu()") };
  });
  assert.deepEqual(seen, { gpu: true, shownContexts: 1, hiddenContexts: 0, hidden: "dom", again: true, text: true, contexts: 1 });
});

test("#358 r2: hiding and showing a terminal on the GPU never changes the size it sends the engine", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true);
    const gpu = await drawnWith(evaluate, "webgl");
    const sized = await evaluate("Fixture.until(() => Fixture.resizes().length > 0)");
    await evaluate("Fixture.until(() => false, 600)");
    const before = await evaluate("Fixture.resizes()");
    for (let round = 0; round < 3; round += 1) {
      await evaluate("Fixture.away()");
      await evaluate(`Fixture.back("master", ${harness})`);
      await drawnWith(evaluate, "webgl");
      await evaluate("Fixture.until(() => false, 300)");
    }
    return { gpu, sized, before, after: await evaluate("Fixture.resizes()") };
  });
  assert.deepEqual([seen.gpu, seen.sized], [true, true], JSON.stringify(seen));
  const first = seen.before.at(-1);
  assert.deepEqual(seen.after.slice(seen.before.length).filter((size) => size.cols !== first.cols || size.rows !== first.rows), [], JSON.stringify(seen));
});

test("#358 r2: letting a terminal go loses its WebGL context", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true);
    const gpu = await drawnWith(evaluate, "webgl");
    const before = await evaluate("Fixture.liveGpu()");
    await evaluate("Fixture.drop()");
    return { gpu, before, after: await evaluate("Fixture.liveGpu()") };
  });
  assert.deepEqual(seen, { gpu: true, before: 1, after: 0 });
});

test("#358 r1 F1: the terminal's text reads from its buffer, so a reader sees it under WebGL too", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await live(evaluate, true);
    return { gpu: await drawnWith(evaluate, "webgl"), shown: await shows(evaluate, "drawn line") };
  });
  assert.deepEqual(seen, { gpu: true, shown: true });
});
