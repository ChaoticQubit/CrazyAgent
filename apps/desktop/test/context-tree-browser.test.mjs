import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/context-tree.mjs", import.meta.url).pathname);
const CSS = [
  readFileSync(new URL("../src/ui/tokens.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/style.css", import.meta.url), "utf8").replace(/@import[^;]*;/g, ""),
  readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8"),
].join("\n");
const doc = (path) => ({ path: `repo/${path}`, kind: "document", bytes: 8, linked: true });
const CONTEXT = {
  project_id: "p-1",
  folder: "/Users/mira/CrazyAgent/harbor-notes/.crazyagent",
  items: [
    { path: "context/brief.md", kind: "brief", bytes: 8 },
    ...["ROADMAP.md", "docs/architecture/overview.md", "docs/architecture/adr/0001-store.md", "docs/architecture/adr/0002-sync.md", "docs/roadmap/README.md"].map(doc),
  ],
  unlisted_docs: 6,
};
const WORDS = ["Project memory", "From your repo", "ROADMAP.md", "docs/architecture/", "overview.md", "docs/architecture/adr/", "0001-store.md", "docs/roadmap/", "README.md", "6 more docs not listed"];

/** Clicks an element with the real mouse. Inputs: the page, its selector and its words.
 * Output: nothing. */
async function clickOn(page, selector, words) {
  const box = await page.evaluate(`Fixture.boxOf(${JSON.stringify(selector)}, ${JSON.stringify(words)})`);
  assert.ok(box, `nothing to click at ${selector} ${words}`);
  for (const type of ["mousePressed", "mouseReleased"]) {
    await page.send("Input.dispatchMouseEvent", { type, x: box.x, y: box.y, button: "left", clickCount: 1 });
  }
  await page.evaluate("Fixture.settle(120)");
}

for (const theme of ["light", "dark"]) {
  test(`the Context tree draws nested docs folder by folder and the left-out count, readable in ${theme}, and a nested doc opens (DEC-152)`, async () => {
    await withPage(async (page) => {
      await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 860, deviceScaleFactor: 1, mobile: false });
      await page.evaluate(fixture);
      await page.evaluate(`document.documentElement.setAttribute("data-theme", ${JSON.stringify(theme)})`);
      await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, ${JSON.stringify(CONTEXT)})`);
      for (const fact of await page.evaluate(`Fixture.facts(${JSON.stringify(WORDS)})`)) {
        assert.ok(fact.shown, `${fact.words} is drawn`);
        assert.ok(fact.contrast >= 4.5, `${fact.words} reads at ${fact.contrast.toFixed(2)}:1 in ${theme}`);
      }
      assert.equal(await page.evaluate("Fixture.treeOverflows()"), false, "folder by folder, the tree needs no sideways scroll");
      await clickOn(page, "button.pj-titem", "0001-store.md");
      assert.equal(await page.evaluate("Fixture.heading()"), "docs/architecture/adr/0001-store.md");
      const reads = await page.evaluate(`Fixture.sentTo("desktop_project_context_item").map((args) => args.path)`);
      assert.equal(reads.at(-1), "repo/docs/architecture/adr/0001-store.md");
    });
  });
}
