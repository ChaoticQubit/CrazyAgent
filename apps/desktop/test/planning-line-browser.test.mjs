import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { withPage } from "./browser.mjs";

const planning = await import("../src/screens/project/planning.tsx");

const APP_CSS = [
  readFileSync(new URL("../src/ui/tokens.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/style.css", import.meta.url), "utf8").replace(/@import[^;]*;/g, ""),
  readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8"),
].join("\n");
const job = (status, report_path = null) => ({ id: "plan-1", kind: "planning", status, report_path, error: null });
const VIEWS = [
  ["not planned", planning.planningView({ kind: "idle" })],
  ["done", planning.planningView({ kind: "done", job: job("done", "docs/roadmap.md") })],
  ["failed", planning.planningView({ kind: "failed", job: job("failed") })],
];
const noop = () => {};
const PLACES = {
  overview: (row) => `<div class="shell"><nav class="sidebar"></nav><main class="main"><div class="page"><div class="pj"><div class="pj-overview"><div class="pj-stack">${row}</div><aside class="pj-panel pj-facts"></aside></div></div></div></main></div>`,
  settings: (row) => `<div class="shell"><nav class="sidebar"></nav><main class="main"><div class="page"><div class="pj"><div class="pj-settings"><nav class="pj-subnav"><button type="button">General</button></nav><div class="pj-settings-body">${row}</div></div></div></div></main></div>`,
};

/** Draws one Roadmap line in a place of the app at a window width with the app's CSS and
 * measures it. Inputs: the place, the width and the view. Output: the parts that run past the
 * row's right edge, whether the actions start right of the line's words and the sideways scroll. */
async function drawn(place, width, view) {
  const row = renderToStaticMarkup(createElement(planning.PlanningLineView, { view, busy: false, problem: "", onAction: noop, onOpen: noop }));
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width, height: 800, deviceScaleFactor: 1, mobile: false });
    return evaluate(`(() => {
      const style = document.createElement("style");
      style.textContent = ${JSON.stringify(APP_CSS)};
      document.head.append(style);
      document.body.innerHTML = ${JSON.stringify(PLACES[place](row))};
      const line = document.querySelector(".pj-research-row");
      const edge = line.getBoundingClientRect().right;
      const words = line.querySelector(".pj-secondary").getBoundingClientRect();
      const buttons = [...line.querySelectorAll("button")].map((button) => button.getBoundingClientRect());
      return {
        past: [...line.querySelectorAll("*")].filter((part) => part.getBoundingClientRect().right > edge + 0.5).map((part) => part.textContent.trim() || part.className),
        beside: buttons.every((box) => box.left >= words.right - 0.5 || box.top >= words.bottom - 0.5),
        buttons: buttons.length,
        sideways: document.querySelector(".main").scrollWidth - document.querySelector(".main").clientWidth,
      };
    })()`);
  });
}

for (const place of Object.keys(PLACES)) {
  for (const width of [1024, 1280]) {
    test(`in ${place} at a ${width} px window the Roadmap line keeps its actions inside the row, after its words`, async () => {
      for (const [name, view] of VIEWS) {
        const seen = await drawn(place, width, view);
        assert.ok(seen.buttons > 0, `${name} offers an action`);
        assert.deepEqual(seen.past, [], `${name}: nothing runs past the row: ${JSON.stringify(seen)}`);
        assert.ok(seen.beside, `${name}: the actions sit after the line's words: ${JSON.stringify(seen)}`);
        assert.equal(seen.sideways, 0, `${name}: the page does not scroll sideways`);
      }
    });
  }
}
